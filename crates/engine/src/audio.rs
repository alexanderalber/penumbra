//! Audio in the engine: the hub's streams follow the network's `audio.device_in` nodes (the cook
//! thread calls [`penumbra_ops::audio::wants`] when the network changes), the output buses the
//! `out.audio` nodes ([`penumbra_ops::out::audio::wants`]).
//!
//! Commands: `audio.devices`, `audio.outputs`, `audio.status`, `audio.config`, `audio.mute`,
//! `audio.measure_latency`, `audio.play`, `audio.file_info`, and the beat analyses of sound files:
//! `beats.analyze`, `beats.status`, `beats.grid`, `beats.cancel`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_audio::capture::{self, Config};
use penumbra_audio::measure;
use penumbra_audio::playback::{self, Settings};
use penumbra_core::NodePath;
use penumbra_ops::audio::beat_grid::{Analysis, BEAT_GRID, State};
use penumbra_ops::audio::file::{FILE, FileStatus};
use penumbra_ops::audio::instrument::{INSTRUMENT, Presses};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, require};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "audio.devices",
        "The audio inputs of this machine: name, default, rate, channels, format",
        audio_devices,
    );
    add(
        "audio.outputs",
        "The audio outputs of this machine: name, default, rate, channels, format",
        audio_outputs,
    );
    add(
        "audio.status",
        "Audio input streams (device, rate, channels, open, error, frames, glitches, peak per channel in dBFS) and          output buses (device, open, error, rate, latency_ms with fill_ms and device_ms, underruns, overruns,          glitches, limiter reduction per node and of the sum, muted)",
        audio_status,
    );
    add("audio.config", "Audio settings for this run: {enabled?}", audio_config);
    add(
        "audio.mute",
        "Mutes every audio output at once, a panic button: {mute?} (default true; false unmutes). For this run only",
        audio_mute,
    );
    add(
        "audio.play",
        "An audio.instrument plays one note at its next frame, as if channel `lane` of play had crossed threshold \
         there; wired or not:{path, lane? (default 0)}. An audio.file starts over from its start (lane is ignored). \
         While the show is paused nothing sounds and the press is dropped",
        audio_play,
    );
    add(
        "audio.file_info",
        "What an audio.file plays: {path} gives file, codec, rate, channels, duration (s), in_memory, ready, \
         error, position (s), playing, gaps (silences while decoding caught up), seeks, restarts, bad_packets",
        audio_file_info,
    );
    add(
        "beats.analyze",
        "Finds the beats and downbeats of a sound file with the Beat This! network, in the background, and keeps them \
         in a cache: {path (an audio.file or chan.beat_grid) or file (relative to the project), wait? (default true: \
         answers when done, at most 600 s), again? (default false: starts over)}. A 4 minute track takes seconds, \
         a 2 hour mix about 3 minutes. Replies as beats.status",
        beats_analyze,
    );
    add(
        "beats.status",
        "How the beat analyses stand: {path or file} for one, nothing for all held. Per file: running, progress \
         (0 to 1), beats, downbeats, final_until (s; the grid before it is final, null for none yet), complete, \
         bpm (median), error, cancelled, seconds",
        beats_status,
    );
    add(
        "beats.grid",
        "The beats found in a sound file: {path or file, from? to? (s)} gives beats, downbeats (the bar starts \
         chan.beat_grid uses, voted over 64 beats) and marked_downbeats (those the network marks one by one; \
         irregular on DJ mixes), s into the file, between from and to, and final_until and complete as \
         beats.status. Does not start an analysis",
        beats_grid,
    );
    add(
        "beats.cancel",
        "Stops a sound file's beat analysis: {path or file}. Its chan.beat_grid nodes keep the beats found so far; \
         beats.analyze starts it again",
        beats_cancel,
    );
    add(
        "audio.measure_latency",
        "Plays quiet clicks (at most -20 dBFS) on an output and finds them on an input: {output?, input?,          channels?, clicks?, level?, latency?, buffer?}; input `loopback:<output>` records what the output plays.          Reports ms from push to arrival per channel, and an output estimate. Takes some seconds",
        audio_measure_latency,
    );
}

fn audio_outputs(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = playback::output_devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn audio_mute(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        mute: Option<bool>,
    }
    let a: A = args(p)?;
    let out = e.audio_out();
    out.set_muted(a.mute.unwrap_or(true));
    Ok(json!({ "muted": out.muted() }))
}

fn audio_play(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        #[serde(default)]
        lane: usize,
    }
    let a: A = args(p)?;
    let played = play(e, &a.path, a.lane)?;
    let message = (!played).then_some(PAUSED);
    Ok(json!({"path": a.path.to_string(), "lane": a.lane, "played": played, "message": message}))
}

/// What `audio.play` says when the show is paused.
pub const PAUSED: &str = "the show is paused, so nothing is heard: the press was dropped";

/// `audio.play`: the `audio.instrument` at `path` plays one note on lane `lane` at the start
/// of its next block; an `audio.file` starts over from its `start`. False when the show is paused: then the press is dropped, so it does not
/// sound later when the show plays again.
pub fn play(e: &Engine, path: &NodePath, lane: usize) -> CmdResult<bool> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != INSTRUMENT && kind != FILE {
        return Err(CommandError(format!("{path} is a {kind}, not an {INSTRUMENT} or {FILE}")));
    }
    let asked = e.on_cook(move |st| {
        if !st.clock.is_playing() {
            return Some(false);
        }
        st.cooker.resource::<Presses>().is_some_and(|p| p.request(id, lane)).then_some(true)
    })?;
    asked.ok_or_else(|| {
        CommandError(format!("{path} has not cooked yet: wire its out into an out.audio to hear it"))
    })
}

fn audio_file_info(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
    }
    let a: A = args(p)?;
    let net = e.session().doc().snapshot();
    let id = require(&net, &a.path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != FILE {
        return Err(CommandError(format!("{} is a {kind}, not an {FILE}", a.path)));
    }
    let Some(s) = e.sound_files().get(id) else {
        return Err(CommandError(format!(
            "{} has not cooked yet: wire its out into an out.audio or look at it",
            a.path
        )));
    };
    Ok(file_info_json(&a.path, &s))
}

/// `audio.file_info`'s reply.
pub fn file_info_json(path: &NodePath, s: &FileStatus) -> Json {
    let info = s.decoder.info.as_ref();
    json!({
        "path": path.to_string(),
        "file": s.file,
        "codec": info.map(|i| i.codec.clone()),
        "rate": info.map(|i| i.rate),
        "channels": info.map(|i| i.channels),
        "duration": info.and_then(|i| i.seconds()),
        "in_memory": info.map(|i| i.in_memory),
        "ready": s.decoder.ready,
        "error": s.decoder.error,
        "position": s.position,
        "playing": s.playing,
        "gaps": s.gaps,
        "seeks": s.decoder.seeks,
        "restarts": s.restarts,
        "bad_packets": s.decoder.bad_packets,
    })
}

fn audio_measure_latency(_e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        output: Option<String>,
        input: Option<String>,
        channels: Option<Vec<usize>>,
        clicks: Option<usize>,
        level: Option<f64>,
        latency: Option<f64>,
        buffer: Option<f64>,
    }
    let a: A = args(p)?;
    let defaults = measure::Spec::default();
    let settings = Settings {
        latency_ms: a.latency.unwrap_or(Settings::default().latency_ms),
        device_buffer_ms: a.buffer.unwrap_or(0.0),
    };
    let spec = measure::Spec {
        output: a.output.unwrap_or_default(),
        input: a.input.unwrap_or_default(),
        channels: a.channels.unwrap_or_default(),
        clicks: a.clicks.unwrap_or(defaults.clicks),
        level_db: a.level.unwrap_or(defaults.level_db).min(measure::MAX_LEVEL_DB),
        settings,
    };
    let report = measure::run(&spec).map_err(CommandError)?;
    Ok(serde_json::to_value(report).map_err(|e| e.to_string())?)
}

fn audio_devices(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = capture::devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn audio_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.audio();
    let out = e.audio_out();
    Ok(json!({
        "enabled": hub.config().enabled,
        "wanted": hub.wanted(),
        "streams": hub.status(),
        "muted": out.muted(),
        "outputs": out.status(),
    }))
}

fn audio_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
    }
    let a: A = args(p)?;
    let hub = e.audio();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}

/// Which sound file a `beats.*` command means.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Which {
    path: Option<NodePath>,
    file: Option<String>,
}

/// The sound file of `w`: a file relative to the project, or the file of the `audio.file` at
/// `path` (or wired into the `chan.beat_grid` there).
fn sound_file(e: &Engine, w: &Which) -> CmdResult<PathBuf> {
    let dir = e.session().dir().to_path_buf();
    let in_project = |f: &str| {
        let f = f.trim();
        if f.is_empty() {
            return Err(CommandError("no file set".into()));
        }
        let p = Path::new(f);
        Ok(if p.is_absolute() { p.to_path_buf() } else { dir.join(p) })
    };
    match (&w.path, &w.file) {
        (Some(_), Some(_)) => Err(CommandError("give path or file, not both".into())),
        (None, None) => Err(CommandError("give path (an audio.file or chan.beat_grid) or file".into())),
        (None, Some(f)) => in_project(f),
        (Some(path), None) => {
            let net = e.session().doc().snapshot();
            let mut id = require(&net, path)?;
            let mut node = net.node(id).ok_or_else(|| CommandError(format!("no node at {path}")))?;
            if node.kind == BEAT_GRID {
                let source = node.inputs.first().copied().flatten().ok_or_else(|| {
                    CommandError(format!("{path} has nothing wired into audio: wire an audio.file's out"))
                })?;
                id = source.node;
                node = net.node(id).ok_or_else(|| CommandError(format!("{path} is wired to nothing")))?;
            }
            if node.kind != FILE {
                return Err(CommandError(format!(
                    "{path} is a {}, not an {FILE} or a {BEAT_GRID} wired from one",
                    node.kind
                )));
            }
            // What the node plays, once it has cooked; its param before.
            if let Some(s) = e.sound_files().get(id).filter(|s| !s.file.is_empty()) {
                return Ok(PathBuf::from(s.file));
            }
            let desc = crate::commands::desc_of(e, FILE)?;
            in_project(node.param(desc, "file").and_then(|v| v.as_str()).unwrap_or(""))
        }
    }
}

/// One analysis as `beats.status` reports it.
fn beats_json(file: &Path, s: &State) -> Json {
    let grid = s.grid.as_ref();
    let beats = grid.map(|g| g.beats.beats.as_slice()).unwrap_or(&[]);
    let mut gaps: Vec<f64> = beats.windows(2).filter_map(|w| Some(w.get(1)? - w.first()?)).collect();
    gaps.sort_by(f64::total_cmp);
    let bpm = gaps.get(gaps.len() / 2).filter(|g| **g > 0.0).map(|g| 60.0 / g);
    json!({
        "file": file.display().to_string(),
        "running": s.running,
        "progress": s.progress,
        "beats": beats.len(),
        "downbeats": grid.map_or(0, |g| g.bars.len()),
        "final_until": grid.map(|g| g.final_until).filter(|t| t.is_finite()),
        "complete": grid.is_some_and(|g| g.complete()),
        "bpm": bpm,
        "error": s.error,
        "cancelled": s.cancelled,
        "seconds": s.seconds,
    })
}

fn analysis_of(e: &Engine, w: &Which) -> CmdResult<(PathBuf, Arc<Analysis>)> {
    let file = sound_file(e, w)?;
    let a = e.beat_files().get(&file).ok_or_else(|| {
        CommandError(format!("{} has no beat analysis: beats.analyze starts one", file.display()))
    })?;
    Ok((file, a))
}

/// The longest `beats.analyze` waits.
const ANALYZE_WAIT: Duration = Duration::from_secs(600);

fn beats_analyze(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
        file: Option<String>,
        wait: Option<bool>,
        again: Option<bool>,
    }
    let a: A = args(p)?;
    let file = sound_file(e, &Which { path: a.path, file: a.file })?;
    if !file.is_file() {
        return Err(CommandError(format!("{} is not a file", file.display())));
    }
    let files = e.beat_files();
    // A cancelled or failed analysis, or one asked for again, starts over.
    let stale = files.get(&file).is_some_and(|h| {
        let s = h.state();
        !s.running && s.grid.as_ref().is_none_or(|g| !g.complete())
    });
    if a.again.unwrap_or(false) || stale {
        files.restart(&file);
    }
    let analysis = files.hold(&file);
    if a.wait.unwrap_or(true) {
        let deadline = Instant::now() + ANALYZE_WAIT;
        while analysis.state().running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let s = analysis.state();
    let mut reply = beats_json(&file, &s);
    if let (Some(e), Some(obj)) = (&s.error, reply.as_object_mut()) {
        obj.insert("message".into(), json!(format!("no beats: {e}")));
    }
    Ok(reply)
}

fn beats_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    let w: Which = args(p)?;
    if w.path.is_none() && w.file.is_none() {
        let all: Vec<Json> = e.beat_files().list().iter().map(|(f, s)| beats_json(f, s)).collect();
        return Ok(json!({ "analyses": all }));
    }
    let (file, a) = analysis_of(e, &w)?;
    Ok(beats_json(&file, &a.state()))
}

fn beats_grid(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
        file: Option<String>,
        from: Option<f64>,
        to: Option<f64>,
    }
    let a: A = args(p)?;
    let (file, analysis) = analysis_of(e, &Which { path: a.path, file: a.file })?;
    let s = analysis.state();
    let (from, to) = (a.from.unwrap_or(f64::NEG_INFINITY), a.to.unwrap_or(f64::INFINITY));
    let within = |v: &[f64]| -> Vec<f64> { v.iter().copied().filter(|t| *t >= from && *t <= to).collect() };
    let grid = s.grid.as_ref();
    Ok(json!({
        "file": file.display().to_string(),
        "beats": grid.map(|g| within(&g.beats.beats)).unwrap_or_default(),
        "downbeats": grid.map(|g| within(&g.bars)).unwrap_or_default(),
        "marked_downbeats": grid.map(|g| within(&g.beats.downbeats)).unwrap_or_default(),
        "final_until": grid.map(|g| g.final_until).filter(|t| t.is_finite()),
        "complete": grid.is_some_and(|g| g.complete()),
        "running": s.running,
    }))
}

fn beats_cancel(e: &Engine, p: &Json) -> CmdResult<Json> {
    let w: Which = args(p)?;
    let file = sound_file(e, &w)?;
    let stopped = e.beat_files().cancel(&file);
    Ok(json!({
        "file": file.display().to_string(),
        "cancelled": stopped,
        "message": (!stopped).then_some("no analysis of this file was running"),
    }))
}
