//! Audio in the engine: the hub's streams follow the network's `audio.device_in` nodes (the cook
//! thread calls [`penumbra_ops::audio::wants`] when the network changes), the output buses the
//! `out.audio` nodes ([`penumbra_ops::out::audio::wants`]).
//!
//! Commands: `audio.devices`, `audio.outputs`, `audio.status`, `audio.config`, `audio.mute`,
//! `audio.measure_latency`, `audio.play`, `audio.file_info`, and the beat analyses of sound files:
//! `beats.analyze`, `beats.status`, `beats.grid`, `beats.cancel`, and `beats.cues`, which writes a
//! `chan.sequence`'s steps from the sections of a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_audio::capture::{self, Config};
use penumbra_audio::measure;
use penumbra_audio::playback::{self, Settings};
use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_ops::audio::beat_grid::{Analysis, BEAT_GRID, State, letter};
use penumbra_ops::audio::cues::{self, Blend, Level, Pieces};
use penumbra_ops::audio::file::{FILE, FileStatus};
use penumbra_ops::audio::instrument::{INSTRUMENT, Presses};
use penumbra_ops::sequence::SEQUENCE;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{edit, engine_commands, outcome, require};
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
        "audio.seek",
        "An audio.file jumps to `seconds` into its file at the next frame and plays on from there, with short fades (a looping file stays in its pass): {path, seconds}. Not while its position is bound to a channel",
        |e, p| crate::video_file::seek_command(e, p, FILE),
    );
    add(
        "audio.file_info",
        "What an audio.file plays: {path} gives file, codec, rate, channels, duration (s), in_memory, ready, \
         error, position (s), playing, gaps (silences while decoding caught up), seeks, restarts, bad_packets",
        audio_file_info,
    );
    add(
        "beats.analyze",
        "Finds the beats and downbeats of a sound file with the Beat This! network, and its envelopes (level, bands, \
         onsets) as chan.beat_analysis measures them, in the background, and keeps both in a cache: {path (an audio.file or chan.beat_grid) or file (relative to the project), wait? (default true: \
         answers when done, at most 600 s), again? (default false: starts over)}. A 4 minute track takes seconds, \
         a 2 hour mix about 3 minutes. Replies as beats.status",
        beats_analyze,
    );
    add(
        "beats.status",
        "How the beat analyses stand: {path or file} for one, nothing for all held. Per file: running, progress \
         (0 to 1), beats, downbeats, final_until (s; the grid before it is final, null for none yet), complete, \
         bpm (median), error, cancelled, seconds, envelopes {running, progress, known_until (s), complete, \
         bytes, error}, and sections {ready, running, tracks, sections, kinds, error}. The sections are found once \
         beats and envelopes are complete",
        beats_status,
    );
    add(
        "beats.grid",
        "The beats found in a sound file: {path or file, from? to? (s)} gives beats (those chan.beat_grid \
         uses: passages the network counted at twice or half the tempo around them are folded back to it), \
         downbeats (the bar starts chan.beat_grid uses, voted over 64 beats), found_beats (the network's beats \
         before folding) and marked_downbeats (those the network marks one by one; \
         irregular on DJ mixes), s into the file, between from and to, and final_until and complete as \
         beats.status. sections lists the sections that overlap from to to, once found: start and end (s), bars, \
         label (the kind of sound, the same number anywhere in the file where it sounds alike), letter (repeats \
         within a track share it), track, track_start, strength (0 to 1), seed and zone [start, end] (s, the blend \
         from the section before); null before. Does not start an analysis",
        beats_grid,
    );
    add(
        "beats.cancel",
        "Stops a sound file's beat analysis and the measuring of its levels: {path or file}. Its chan.beat_grid \
         nodes keep the beats and levels found so far; beats.analyze starts it again",
        beats_cancel,
    );
    add(
        "beats.cues",
        "Writes a cue list into a chan.sequence from the sections of the file a chan.beat_grid reads, as one \
         undoable edit: {path (the chan.beat_grid), sequence (the chan.sequence), level? (section: a step per \
         section, the default; track: a step per track), pieces? (what names a step's piece: label, the kind \
         of sound, kind0, kind1...; letter, A, B... within a track; track, track0, track1...; default label, \
         track for level track), transition? (zone: a cross-fade through each blend between sections, the \
         default; cut)}. Sets steps, names (piece k is in<k> of a tex.sequence) and lead_in 0, clears file, \
         and binds clock to the grid's seconds. The show then follows the music through seeks, loops and the \
         grid's offset. A step after a blend begins where the blend ends. Needs the sections: beats.analyze \
         waits for them. Run it again after the file changes",
        beats_cues,
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
    let beats = grid.map(|g| g.grid.beats()).unwrap_or(&[]);
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
        "envelopes": envelopes_json(s),
        "sections": sections_json(s),
    })
}

/// How the sections of one analysis stand.
fn sections_json(s: &State) -> Json {
    let x = s.sections.as_ref().map(|x| &x.sections);
    json!({
        "ready": x.is_some(),
        "running": s.sections_running,
        "tracks": x.map_or(0, |x| x.tracks()),
        "sections": x.map_or(0, |x| x.len()),
        "kinds": x.map_or(0, |x| x.kinds()),
        "error": s.sections_error,
    })
}

/// How the envelopes of one analysis stand.
fn envelopes_json(s: &State) -> Json {
    let env = s.envelopes.as_ref();
    json!({
        "running": s.envelopes_running,
        "progress": s.envelope_progress,
        "known_until": env.map(|e| e.known_until()).filter(|t| t.is_finite()),
        "complete": env.is_some_and(|e| e.complete()),
        "bytes": env.map_or(0, |e| e.bytes()),
        "error": s.envelope_error,
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
        while analysis.state().busy() && Instant::now() < deadline {
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
    let sections: Option<Vec<Json>> = s.sections.as_ref().map(|x| {
        x.sections
            .list()
            .iter()
            .filter(|c| c.end >= from && c.start <= to)
            .map(|c| {
                json!({
                    "start": c.start,
                    "end": c.end,
                    "bars": c.bars,
                    "label": c.label,
                    "letter": letter(c.letter),
                    "track": c.track,
                    "track_start": c.track_start,
                    "strength": c.strength,
                    "seed": c.seed,
                    "zone": [c.zone.0, c.zone.1],
                })
            })
            .collect()
    });
    Ok(json!({
        "file": file.display().to_string(),
        "beats": grid.map(|g| within(g.grid.beats())).unwrap_or_default(),
        "found_beats": grid.map(|g| within(&g.beats.beats)).unwrap_or_default(),
        "downbeats": grid.map(|g| within(&g.bars)).unwrap_or_default(),
        "marked_downbeats": grid.map(|g| within(&g.beats.downbeats)).unwrap_or_default(),
        "final_until": grid.map(|g| g.final_until).filter(|t| t.is_finite()),
        "complete": grid.is_some_and(|g| g.complete()),
        "running": s.running,
        "sections": sections,
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

fn beats_cues(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        sequence: NodePath,
        level: Option<String>,
        pieces: Option<String>,
        transition: Option<String>,
    }
    let a: A = args(p)?;
    let level = match a.level.as_deref() {
        None => Level::Section,
        Some(l) => {
            Level::parse(l).ok_or_else(|| CommandError(format!("level `{l}` is none of section, track")))?
        }
    };
    let pieces = match a.pieces.as_deref() {
        None if level == Level::Track => Pieces::Track,
        None => Pieces::Label,
        Some(k) => Pieces::parse(k)
            .ok_or_else(|| CommandError(format!("pieces `{k}` is none of label, letter, track")))?,
    };
    let blend = match a.transition.as_deref() {
        None => Blend::Zone,
        Some(t) => {
            Blend::parse(t).ok_or_else(|| CommandError(format!("transition `{t}` is none of zone, cut")))?
        }
    };
    let net = e.session().doc().snapshot();
    let kind_at = |path: &NodePath| -> CmdResult<String> {
        let id = require(&net, path)?;
        Ok(net.node(id).map(|n| n.kind.clone()).unwrap_or_default())
    };
    let grid_kind = kind_at(&a.path)?;
    if grid_kind != BEAT_GRID {
        return Err(CommandError(format!(
            "{} is a {grid_kind}, not a {BEAT_GRID}: the cues follow the seconds of a grid",
            a.path
        )));
    }
    let seq_kind = kind_at(&a.sequence)?;
    if seq_kind != SEQUENCE {
        return Err(CommandError(format!("{} is a {seq_kind}, not a {SEQUENCE}", a.sequence)));
    }
    let (file, analysis) = analysis_of(e, &Which { path: Some(a.path.clone()), file: None })?;
    let s = analysis.state();
    let name =
        file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
    let Some(structure) = s.sections.as_ref() else {
        let why =
            if let Some(err) = s.error.as_ref().or(s.envelope_error.as_ref()).or(s.sections_error.as_ref()) {
                format!("{name} has no sections: {err}")
            } else if s.sections_running {
                format!("the sections of {name} are being found; try again in a moment")
            } else {
                format!(
                    "{name} has no sections yet: they are found once beats and levels are complete \
                 (beats.analyze {{path: \"{}\"}} waits for them)",
                    a.path
                )
            };
        return Err(CommandError(why));
    };
    // The file's length, so the last step runs to its end.
    let end = s.envelopes.as_ref().map_or(0.0, |env| env.len() as f64 * env.hop_seconds());
    let c = cues::cues(&structure.sections, end, &name, level, pieces, blend).map_err(CommandError)?;
    let param = |p: &str| -> CmdResult<ParamPath> {
        let name = Name::new(p).map_err(|err| CommandError(err.to_string()))?;
        ParamPath::new(a.sequence.clone(), name).map_err(|err| CommandError(err.to_string()))
    };
    let text = |t: &str| Value::Text(t.to_string());
    let mut ops = Vec::new();
    for p in ["steps", "names", "lead_in", "file"] {
        // A bound param would hide the value written here.
        ops.push(Op::Unbind { param: param(p)?, component: None });
    }
    ops.extend([
        Op::Set { param: param("steps")?, value: text(&c.steps) },
        Op::Set { param: param("names")?, value: text(&c.names) },
        Op::Set { param: param("lead_in")?, value: Value::Float(0.0) },
        Op::Reset { param: param("file")? },
        Op::Bind {
            param: param("clock")?,
            component: None,
            from: a.path.clone(),
            channel: Some(Name::new("seconds").map_err(|err| CommandError(err.to_string()))?),
            cell: None,
            format: None,
        },
    ]);
    let label = format!("cue list from the sections of {name}");
    let out = edit(e, &ops, &label)?;
    let mut reply = outcome(&out);
    let cues: Vec<Json> = c.starts.iter().map(|(t, k)| json!({"start": t, "piece": k})).collect();
    if let Some(obj) = reply.as_object_mut() {
        obj.insert("sequence".into(), json!(a.sequence.to_string()));
        obj.insert("file".into(), json!(file.display().to_string()));
        obj.insert("steps".into(), json!(c.starts.len()));
        obj.insert("names".into(), json!(c.names));
        obj.insert("end".into(), json!(c.end));
        obj.insert("cues".into(), json!(cues));
    }
    Ok(reply)
}
