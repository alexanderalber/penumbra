//! Audio in the engine: the hub's streams follow the network's `audio.device_in` nodes (the cook
//! thread calls [`penumbra_ops::audio::wants`] when the network changes), the output buses the
//! `out.audio` nodes ([`penumbra_ops::out::audio::wants`]).
//!
//! Commands: `audio.devices`, `audio.outputs`, `audio.status`, `audio.config`, `audio.mute`,
//! `audio.measure_latency`, `audio.play`, `audio.file_info`, and the beat analyses of sound files:
//! `beats.analyze`, `beats.status`, `beats.grid`, `beats.cancel`, `beats.cues`, which writes a
//! `chan.sequence`'s steps from the sections of a file, and `beats.arrange`, which writes the
//! arrangement of a music stage as a table read live.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_audio::capture::{self, Config};
use penumbra_audio::measure;
use penumbra_audio::playback::{self, Settings};
use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::{Network, Op};
use penumbra_ops::audio::arrange::{self, Part, Slot};
use penumbra_ops::audio::beat_grid::{Analysis, BEAT_GRID, State, letter};
use penumbra_ops::audio::cues::{self, Blend, Level, Pieces};
use penumbra_ops::audio::file::{FILE, FileStatus};
use penumbra_ops::audio::instrument::{INSTRUMENT, Presses};
use penumbra_ops::comp::COMP_KIND;
use penumbra_ops::sequence::SEQUENCE;
use penumbra_ops::table::FROM_TABLE;
use penumbra_project::component;
use penumbra_project::{Content, diff};
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
         bytes, error}, sections {ready, running, tracks, sections, kinds, error} and bus {ready} (travelled, \
         energy and drop on chan.beat_grid). The sections and the bus are found once beats and envelopes are complete",
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
         from the section before), and with the music bus energy, bass, density (onsets per beat), kick (share \
         of beats with a kick), jump (energy minus the section before's), role (intro, break, build, drop, outro \
         or main) and change (0 to 1: how strongly the music changes where the section begins); null before. \
         Does not start an analysis",
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
        "beats.arrange",
        "Writes the arrangement of a music stage from the sections of the file a chan.beat_grid reads, as one \
         undoable edit: {path (the chan.beat_grid), table (a table.text, created when missing), sequence (a \
         chan.sequence, created when missing), under? (a component that holds the table as table, the sequence \
         as cues and the readers, created when missing; table and sequence are then optional, and given ones \
         that stand beside the component move into it with their readers, keeping every binding and wire), slots ({slot: [element paths]}, e.g. {floor: [\"/blocks\", \
         \"/mountains\"], sky: [\"/galaxy\"]}; an element may be {path, weight} instead, e.g. {path: \
         \"/glider\", weight: 3}, to come on three times as often as one of weight 1 (default 1, at most \
         100), and with stages: [\"/stage\"] to show only while one of those stages shows (elsewhere it stays at \
         presence 0 and its slot takes another element), and with instead_of: \"/bars\" to stand in for \
         that element of any slot on its stages (it takes no turns, shows where /bars would with the same \
         presence, and /bars stays at 0 there; every other cell stays as without it), and with with: \
         \"/glider\" to come along with that element on its stages (it takes no turns, shows with the same \
         presence in a build or a main section of energy below 0.8 where /glider is the only element of \
         its slot on, not next to a change of stage, and flight is 4 there; every other cell stays as \
         without it), and with extra: \"lively\", \"calm\" or \"steady\" to come now and then by itself on \
         its stages (it takes no turns, shows with its slot's presence in every second stretch of lively \
         rows, a drop, a build or a main section of energy from 0.8, of calm rows, an intro, an outro or a \
         break, or of steady rows, a main section below energy 0.8, not next to a change of stage; every \
         other cell stays as without it), and with base: true to be the slot's base, at most one per slot \
         (it takes no turns and stays on under whichever element has the slot's turn, with that element's \
         presence, on its stages; an empty section keeps no base; every other cell stays as without it): \
         the first element of each slot that is no base is on at the start, slots take turns in name order, the slots named sky and mid stay on through a break, the slot post holds \
         effects that are on or off each by itself (off in an intro, outro or break, on from a drop), every \
         other slot (floor, ceiling) follows the floor's rules), stage? (a path, or a list of stages side by \
         side; each gets warmth, and camera where it has that param), switch? (a param that picks the stage, \
         e.g. \"/stages.index\" of a tex.switch, or a list of them; it fades on a tex.cross, for two stages, \
         and on a tex.switch with blend on), intensity? (a param that gets intensity, e.g. \"/stage.dim\", \
         or a list), lights? (a path or a list: what lights the room beside the stage, such as a \
         room_lights from the library; each gets pattern, and warmth and intensity where it has them), \
         fade_min? (s, default 2: the shortest fade from one row into the \
         next, also where the music cuts), stage_fade? (s, default 4: the shortest fade into a row whose \
         stage changes), dwell? (sections, default 3: an element on that long gives way at the next \
         boundary with change 0.3 or more, its slot's turn or not; a slot its stage hides counts on; 0 \
         turns it off), opening? (default true: the file's first section brings its slots in one after another \
         on bar starts, sky first, then mid, the floor and the rest, the ceiling last, a row per step of at least 2 bars), apart? (pairs of elements of different slots that never share a row, e.g. [[\"/gates\", \"/rings\"]]: where both would be on, the first gives way for that row and its slot shows its next element; every other cell stays as without it), split_bars? (cuts each section at every so many bar starts, for a single song: each new boundary has change 0.3, so one slot swaps there; a break stays one row, the pieces after a drop's first are main), find_break? (default false, for a single song: a stretch of 4 bars or more where the bass falls 10 dB below its loud level becomes a row of role break, followed by a row of role drop with change 1; a track start in the break is no track start; the reply lists breaks), stage_gap? (s, default 480: the least time between two changes of stage; with find_break the stage may also change on the drop after a break), gaps? ({build?, break?}: the share of the floor in gaps, default \
         0.3 in a build and 0.6 in a break)}. The table has a row per section: start, end, role, change, the \
         presence of each element (the column is named as the element), warmth, intensity, camera (the \
         stage's camera preset: low in a main section, higher in a build, high and wide on a drop, level in \
         a break, a variant per track), stage (changes only at a track start with change 0.8 or more, \
         at most once per stage_gap, to the next stage in the list and back again: 0, 1, 2, 1, 0), gaps (the share of the floor broken off: only in a build and a \
         break) and flight (how the glider flies: 0 along the lane's turns, 1 straight, 2 along the turns \
         with a roll on a downbeat, 3 straight with a lean on each downbeat, 4 through the corridor of a \
         companion such as mid_traps; straight in a break, a roll on a drop, the lean in a build, 0, 1 and \
         3 by turns from track to track elsewhere, 4 where a companion comes) and pattern (what \
         the room's lights show: 0 stripes in an intro, outro or break, 2 a chase in a build, 1 a pulse on \
         a drop, 2 and 1 by turns from track to track in a main section) and see_through (how far a \
         stage with walls opens them: 1 in every second stretch of breaks and builds on each stage, \
         not next to a change of stage, else 0). A strong boundary into a main section may leave the stage and one slot \
         alone, drawn from the section's seed. Edit the table by hand to override a row. The sequence gets a \
         step per row, its clock on the grid's seconds. Beside the table it creates or updates <table>_now, \
         _plus, _next, _diff, _ramp, _mix, _half, _snap, _jump and _pick; _mix has a channel per column that \
         moves through each fade, _pick jumps to the next row half way through it. Binds each element's \
         presence, warmth, camera, gap and see_through (on a stage that has them), flight (on an element that has it), \
         pattern (on the lights) and switch to them, and intensity. Run it again after the file changes: \
         it overwrites the table",
        beats_arrange,
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
        "bus": json!({ "ready": s.bus.is_some() }),
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
    let character = s.bus.as_ref().map_or(&[][..], |b| b.sections());
    let sections: Option<Vec<Json>> = s.sections.as_ref().map(|x| {
        x.sections
            .list()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.end >= from && c.start <= to)
            .map(|(i, c)| {
                let e = character.get(i);
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
                    "energy": e.map(|e| e.energy),
                    "bass": e.map(|e| e.bass),
                    "density": e.map(|e| e.density),
                    "kick": e.map(|e| e.kick),
                    "jump": e.map(|e| e.jump),
                    "role": e.map(|e| e.role.name()),
                    "change": e.map(|e| e.change),
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
        return Err(no_sections(&name, &s, &a.path));
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
            from_param: None,
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

/// Why a file has no sections (yet).
fn no_sections(name: &str, s: &State, grid: &NodePath) -> CommandError {
    CommandError(
        if let Some(err) = s.error.as_ref().or(s.envelope_error.as_ref()).or(s.sections_error.as_ref()) {
            format!("{name} has no sections: {err}")
        } else if s.sections_running {
            format!("the sections of {name} are being found; try again in a moment")
        } else {
            format!(
                "{name} has no sections yet: they are found once beats and levels are complete \
                 (beats.analyze {{path: \"{grid}\"}} waits for them)"
            )
        },
    )
}

/// Whether the node at `path` has a param `name`, its own or exposed by a component. An exposed
/// param that leads to a shader's own params counts: those are known only after a cook.
fn has_param(e: &Engine, net: &penumbra_graph::Network, path: &NodePath, name: &str) -> bool {
    let Ok(n) = Name::new(name) else { return false };
    let Ok(pp) = ParamPath::new(path.clone(), n) else { return false };
    let Ok(target) = net.resolve_alias(e.types(), &pp) else { return false };
    let alias = target != pp;
    net.resolve(target.node())
        .and_then(|id| net.node(id))
        .and_then(|n| Some((n, e.types().desc(&n.kind)?)))
        .is_some_and(|(n, d)| (alias && d.extra_params) || n.spec(d, target.param().as_str()).is_some())
}

fn name_of(text: &str) -> CmdResult<Name> {
    Name::new(text).map_err(|err| CommandError(err.to_string()))
}

fn param_of(node: &NodePath, p: &str) -> CmdResult<ParamPath> {
    ParamPath::new(node.clone(), name_of(p)?).map_err(|err| CommandError(err.to_string()))
}

/// A binding of `param` to channel `channel` of `from`.
fn bind_to(param: ParamPath, from: &NodePath, channel: &str) -> CmdResult<Op> {
    Ok(Op::Bind {
        param,
        component: None,
        from: from.clone(),
        channel: Some(name_of(channel)?),
        cell: None,
        format: None,
        from_param: None,
    })
}

/// The kinds of the readers `beats.arrange` puts beside its table, by the end of their names.
/// `_mix` moves through each fade; `_pick` jumps from one row to the next half way through it,
/// for the columns that are an index (camera, stage, flight, pattern).
const READERS: [(&str, &str); 10] = [
    ("now", FROM_TABLE),
    ("plus", "chan.math"),
    ("next", FROM_TABLE),
    ("diff", "chan.math"),
    ("ramp", "chan.math"),
    ("mix", "chan.math"),
    ("half", "chan.math"),
    ("snap", "chan.math"),
    ("jump", "chan.math"),
    ("pick", "chan.math"),
];

/// One stage or several side by side.
#[derive(Deserialize)]
#[serde(untagged)]
enum Stages {
    One(NodePath),
    Many(Vec<NodePath>),
}

/// One param or several that take the same column.
#[derive(Deserialize)]
#[serde(untagged)]
enum Targets {
    One(ParamPath),
    Many(Vec<ParamPath>),
}

impl Targets {
    fn list(self) -> Vec<ParamPath> {
        match self {
            Targets::One(p) => vec![p],
            Targets::Many(v) => v,
        }
    }
}

/// An element in a slot: its path, or its path with how often it comes on, the stages it
/// shows in, the element it stands in for or comes with, when it comes by itself, and whether it is
/// the slot's base.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Path(NodePath),
    Weighted {
        path: NodePath,
        #[serde(default)]
        weight: Option<f64>,
        #[serde(default)]
        stages: Option<Vec<NodePath>>,
        #[serde(default)]
        instead_of: Option<NodePath>,
        #[serde(default)]
        with: Option<NodePath>,
        #[serde(default)]
        extra: Option<String>,
        #[serde(default)]
        base: Option<bool>,
    },
}

/// What the table and the sequence are called in the component `under` names.
const UNDER_TABLE: &str = "table";
const UNDER_SEQUENCE: &str = "cues";

/// Where the arrangement's table and sequence stand, and what puts them there.
struct Home {
    table: NodePath,
    sequence: NodePath,
    /// The edit that moves nodes standing elsewhere into the component.
    moves: Vec<Op>,
    /// Old path to new path of every node moved.
    moved: BTreeMap<NodePath, NodePath>,
    /// The component to create, when nothing was moved and it is missing.
    make: Option<NodePath>,
}

/// The table and sequence in the component `under`. A `table` or `sequence` given that stands
/// elsewhere moves into it with the table's readers; wires that leave run through output nodes of
/// the component and bindings keep naming the same nodes (`comp.collapse` does the same).
fn home_under(
    e: &Engine,
    net: &Network,
    under: &NodePath,
    table: Option<&NodePath>,
    sequence: Option<&NodePath>,
) -> CmdResult<Home> {
    let bad = |err: penumbra_core::Error| CommandError(err.to_string());
    let Some(level) = under.parent() else {
        return Err(CommandError("under must name a component, not the root".into()));
    };
    let own_table = under.child(name_of(UNDER_TABLE)?).map_err(bad)?;
    let own_sequence = under.child(name_of(UNDER_SEQUENCE)?).map_err(bad)?;
    let kind_at = |path: &NodePath| net.resolve(path).and_then(|id| net.node(id)).map(|n| n.kind.as_str());
    // What stands outside the component's own two places and has to move.
    let mut members: Vec<NodePath> = Vec::new();
    let mut names: Vec<(NodePath, String)> = Vec::new();
    let mut old_table: Option<(String, NodePath)> = None;
    if let Some(t) = table.filter(|t| **t != own_table && kind_at(t) == Some("table.text")) {
        let (Some(name), Some(parent)) = (t.name(), t.parent()) else {
            return Err(CommandError("table must name a node, not the root".into()));
        };
        members.push(t.clone());
        names.push((t.clone(), UNDER_TABLE.to_string()));
        old_table = Some((name.as_str().to_string(), parent.clone()));
        for (end, kind) in READERS {
            let reader = parent.child(name_of(&format!("{name}_{end}"))?).map_err(bad)?;
            if kind_at(&reader) == Some(kind) {
                members.push(reader.clone());
                names.push((reader, format!("{UNDER_TABLE}_{end}")));
            }
        }
    }
    if let Some(q) = sequence.filter(|q| **q != own_sequence && kind_at(q) == Some(SEQUENCE)) {
        members.push(q.clone());
        names.push((q.clone(), UNDER_SEQUENCE.to_string()));
    }
    for (given, kind) in [(table, "table.text"), (sequence, SEQUENCE)] {
        if let Some(path) = given
            && *path != own_table
            && *path != own_sequence
            && *path != *under
            && let Some(k) = kind_at(path).filter(|k| *k != kind)
        {
            return Err(CommandError(format!("{path} is a {k}, not a {kind}")));
        }
    }
    if members.is_empty() {
        let make = match kind_at(under) {
            None => {
                if !level.is_root() && net.resolve(&level).is_none() {
                    return Err(CommandError(format!(
                        "no node at {level}: create the component that holds {under} first"
                    )));
                }
                Some(under.clone())
            }
            Some(kind) if e.types().desc(kind).is_some_and(|d| d.container) => None,
            Some(kind) => {
                return Err(CommandError(format!("{under} is a {kind}, not a component")));
            }
        };
        return Ok(Home {
            table: own_table,
            sequence: own_sequence,
            moves: Vec::new(),
            moved: BTreeMap::new(),
            make,
        });
    }
    if let Some(found) = kind_at(under)
        && !members.contains(under)
    {
        return Err(CommandError(format!(
            "{under} exists already ({found}) and {} stands outside it: rename or delete one of them",
            members.first().map_or_else(String::new, ToString::to_string)
        )));
    }
    if let Some(m) = members.iter().find(|m| m.parent().as_ref() != Some(&level)) {
        return Err(CommandError(format!(
            "{m} cannot move into {under}: it stands at another level; move it beside {under} first"
        )));
    }
    let base = Content::from_network(net).map_err(bad)?;
    let done = component::collapse(&base, e.types(), &members, under, COMP_KIND).map_err(bad)?;
    let mut moves = diff(&base, &done.content);
    let pos = table.and_then(|t| net.resolve(t)).and_then(|id| net.node(id)).and_then(|n| n.pos);
    moves.push(Op::Place { path: under.clone(), pos });
    // The nodes keep their old names through the move; they get their names here.
    let mut moved = done.moved.clone();
    let mut rename = |path: NodePath, name: &str, old: Option<&NodePath>| -> CmdResult<()> {
        let new_name = name_of(name)?;
        if path.name() != Some(&new_name) {
            let renamed = under.child(new_name.clone()).map_err(bad)?;
            if let Some(old) = old {
                moved.insert(old.clone(), renamed);
            }
            moves.push(Op::Rename { path, name: new_name });
        }
        Ok(())
    };
    for (old, name) in &names {
        let Some(new) = done.moved.get(old) else { continue };
        rename(new.clone(), name, Some(old))?;
    }
    // The output nodes for wires that leave are named after the readers they carry.
    for port in &done.ports {
        let Some(name) = port.name().map(|n| n.as_str().to_string()) else { continue };
        if let Some(rest) = old_table.as_ref().and_then(|(old, _)| name.strip_prefix(&format!("{old}_"))) {
            rename(port.clone(), &format!("{UNDER_TABLE}_{rest}"), None)?;
        }
    }
    Ok(Home { table: own_table, sequence: own_sequence, moves, moved, make: None })
}

/// The stretches without bass by the file's level bands ([`arrange::bass_gone`]).
fn without_bass(env: &penumbra_audio::beats::Envelopes, bars: &[f64], end: f64) -> Vec<(f64, f64)> {
    let mut edges: Vec<f64> = bars.iter().copied().filter(|b| *b < end).collect();
    edges.push(end);
    let bass: Vec<f64> = edges
        .windows(2)
        .map(|w| match w {
            [from, to] => match env.energy_db(*from, *to) {
                Some([_, low, ..]) => low,
                None => f64::NEG_INFINITY,
            },
            _ => f64::NEG_INFINITY,
        })
        .collect();
    arrange::bass_gone(&edges, &bass)
}

/// The mean level of the music bus from `from` to `to` seconds, 0 to 1.
fn mean_energy(bus: &penumbra_audio::beats::Bus, from: f64, to: f64) -> f64 {
    /// Readings per stretch; enough for a mean, and a section is cut only a few times.
    const SAMPLES: u32 = 64;
    let sum: f64 = (0..SAMPLES)
        .map(|k| {
            let t = from + (to - from) * (f64::from(k) + 0.5) / f64::from(SAMPLES);
            bus.at(t).energy.first().copied().unwrap_or(0.0)
        })
        .sum();
    sum / f64::from(SAMPLES)
}

fn beats_arrange(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        table: Option<NodePath>,
        sequence: Option<NodePath>,
        under: Option<NodePath>,
        slots: BTreeMap<String, Vec<Entry>>,
        stage: Option<Stages>,
        switch: Option<Targets>,
        intensity: Option<Targets>,
        lights: Option<Stages>,
        fade_min: Option<f64>,
        stage_fade: Option<f64>,
        dwell: Option<f64>,
        opening: Option<bool>,
        gaps: Option<GapShares>,
        apart: Option<Vec<(NodePath, NodePath)>>,
        split_bars: Option<f64>,
        find_break: Option<bool>,
        stage_gap: Option<f64>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GapShares {
        build: Option<f64>,
        #[serde(rename = "break")]
        brk: Option<f64>,
    }
    let a: A = args(p)?;
    let mut gaps = arrange::Gaps::default();
    if let Some(g) = &a.gaps {
        for (value, slot, name) in [(g.build, &mut gaps.build, "build"), (g.brk, &mut gaps.brk, "break")] {
            if let Some(v) = value {
                if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                    return Err(CommandError(format!("gaps.{name} is a share of the floor from 0 to 1")));
                }
                *slot = v;
            }
        }
    }
    let stages: Vec<NodePath> = match a.stage {
        None => Vec::new(),
        Some(Stages::One(p)) => vec![p],
        Some(Stages::Many(v)) => v,
    };
    let lights: Vec<NodePath> = match a.lights {
        None => Vec::new(),
        Some(Stages::One(p)) => vec![p],
        Some(Stages::Many(v)) => v,
    };
    let switches = a.switch.map(Targets::list).unwrap_or_default();
    let intensities = a.intensity.map(Targets::list).unwrap_or_default();
    let fade_min = a.fade_min.unwrap_or(arrange::FADE_MIN);
    if !fade_min.is_finite() || !(0.0..=arrange::FADE_MIN_MAX).contains(&fade_min) {
        return Err(CommandError(format!("fade_min is seconds from 0 to {}", arrange::FADE_MIN_MAX)));
    }
    let stage_fade = a.stage_fade.unwrap_or(arrange::STAGE_FADE);
    if !stage_fade.is_finite() || !(0.0..=arrange::FADE_MIN_MAX).contains(&stage_fade) {
        return Err(CommandError(format!("stage_fade is seconds from 0 to {}", arrange::FADE_MIN_MAX)));
    }
    let dwell = match a.dwell {
        None => arrange::DWELL,
        // `as`: a whole number from 0 to DWELL_MAX, checked first.
        Some(n) if n.is_finite() && n.fract() == 0.0 && (0.0..=arrange::DWELL_MAX as f64).contains(&n) => {
            n as usize
        }
        Some(_) => {
            return Err(CommandError(format!(
                "dwell is a whole number of sections from 0 (off) to {}",
                arrange::DWELL_MAX
            )));
        }
    };
    let split_bars = match a.split_bars {
        None => None,
        // `as`: a whole number from 1 to SPLIT_BARS_MAX, checked first.
        Some(n)
            if n.is_finite() && n.fract() == 0.0 && (1.0..=arrange::SPLIT_BARS_MAX as f64).contains(&n) =>
        {
            Some(n as usize)
        }
        Some(_) => {
            return Err(CommandError(format!(
                "split_bars is a whole number of bars from 1 to {}",
                arrange::SPLIT_BARS_MAX
            )));
        }
    };
    let find_break = a.find_break.unwrap_or(false);
    let stage_gap = a.stage_gap.unwrap_or(arrange::STAGE_GAP);
    if !stage_gap.is_finite() || stage_gap < 0.0 {
        return Err(CommandError("stage_gap is seconds from 0 up".into()));
    }
    let net = e.session().doc().snapshot();
    let home = match &a.under {
        Some(under) => home_under(e, &net, under, a.table.as_ref(), a.sequence.as_ref())?,
        None => {
            let (Some(table), Some(sequence)) = (a.table.clone(), a.sequence.clone()) else {
                return Err(CommandError(
                    "give table and sequence, or under with the component that holds them".into(),
                ));
            };
            Home { table, sequence, moves: Vec::new(), moved: BTreeMap::new(), make: None }
        }
    };
    // The checks below and the ops see the network as it stands after the move.
    let net = if home.moves.is_empty() {
        net
    } else {
        Arc::new(net.apply(e.types(), &home.moves).map_err(|err| CommandError(err.to_string()))?.network)
    };
    let kind_at = |path: &NodePath| -> Option<String> {
        net.resolve(path).and_then(|id| net.node(id)).map(|n| n.kind.clone())
    };
    let grid_kind = kind_at(&a.path).ok_or_else(|| CommandError(format!("no node at {}", a.path)))?;
    if grid_kind != BEAT_GRID {
        return Err(CommandError(format!(
            "{} is a {grid_kind}, not a {BEAT_GRID}: the arrangement follows the seconds of a grid",
            a.path
        )));
    }
    for (path, kind) in [(&home.table, "table.text"), (&home.sequence, SEQUENCE)] {
        if let Some(k) = kind_at(path).filter(|k| k != kind) {
            return Err(CommandError(format!("{path} is a {k}, not a {kind}")));
        }
    }
    let (Some(table_name), Some(parent)) = (home.table.name(), home.table.parent()) else {
        return Err(CommandError("table must name a node, not the root".into()));
    };
    let mut slots = Vec::new();
    let mut elements: Vec<NodePath> = Vec::new();
    let mut stand_ins: Vec<(NodePath, NodePath, &str)> = Vec::new();
    for (slot, entries) in &a.slots {
        let mut names = Vec::new();
        let mut weights = Vec::new();
        let mut shows = Vec::new();
        let mut instead = Vec::new();
        let mut along = Vec::new();
        let mut extras = Vec::new();
        let mut bases = Vec::new();
        for entry in entries {
            let (path, weight, on, of, with, extra, base) = match entry {
                Entry::Path(path) => (path, 1.0, None, None, None, None, false),
                Entry::Weighted { path, weight, stages: on, instead_of, with, extra, base } => (
                    path,
                    weight.unwrap_or(1.0),
                    on.as_ref(),
                    instead_of.as_ref(),
                    with.as_ref(),
                    extra.as_deref(),
                    base.unwrap_or(false),
                ),
            };
            bases.push(base);
            let extra = match extra {
                None => None,
                Some(name) => Some(arrange::Extra::named(name).ok_or_else(|| {
                    CommandError(format!("{path}: extra is lively, calm or steady, not {name}"))
                })?),
            };
            extras.push(extra);
            let column = |p: &NodePath| p.name().map(|n| n.as_str().to_string()).unwrap_or_default();
            if let Some(of) = of {
                stand_ins.push((path.clone(), of.clone(), "instead_of"));
            }
            if let Some(w) = with {
                stand_ins.push((path.clone(), w.clone(), "with"));
            }
            instead.push(of.map(column));
            along.push(with.map(column));
            let mut list = Vec::new();
            for st in on.into_iter().flatten() {
                let Some(k) = stages.iter().position(|s| s == st) else {
                    return Err(CommandError(format!(
                        "{path}: stage {st} is not one of the stages given in stage"
                    )));
                };
                list.push(k);
            }
            shows.push(list);
            if !weight.is_finite() || !(weight > 0.0 && weight <= arrange::WEIGHT_MAX) {
                return Err(CommandError(format!(
                    "{path}: weight is a number above 0, at most {}",
                    arrange::WEIGHT_MAX
                )));
            }
            weights.push(weight);
            require(&net, path)?;
            if !has_param(e, &net, path, "presence") {
                return Err(CommandError(format!("{path} has no param presence: an element needs one")));
            }
            names.push(path.name().map(|n| n.as_str().to_string()).unwrap_or_default());
            elements.push(path.clone());
        }
        slots.push(Slot {
            name: slot.clone(),
            elements: names,
            weights,
            stages: shows,
            instead,
            with: along,
            extra: extras,
            base: bases,
        });
    }
    for (path, of, field) in &stand_ins {
        if !elements.contains(of) {
            return Err(CommandError(format!(
                "{path}: {field} {of} is not an element of a slot: name it in one of the slots too"
            )));
        }
    }
    arrange::check(&slots).map_err(CommandError)?;
    let mut apart = Vec::new();
    for (first, second) in a.apart.iter().flatten() {
        for path in [first, second] {
            if !elements.contains(path) {
                return Err(CommandError(format!(
                    "apart names {path}, which is not an element of a slot: name it in one of the slots too"
                )));
            }
        }
        let column = |p: &NodePath| p.name().map(|n| n.as_str().to_string()).unwrap_or_default();
        apart.push((column(first), column(second)));
    }
    arrange::check_apart(&slots, &apart).map_err(CommandError)?;
    for stage in &stages {
        require(&net, stage)?;
        if !has_param(e, &net, stage, "warmth") {
            return Err(CommandError(format!("{stage} has no param warmth")));
        }
    }
    for light in &lights {
        require(&net, light)?;
        if !has_param(e, &net, light, "pattern") {
            return Err(CommandError(format!(
                "{light} has no param pattern: lights take the pattern column, as a room_lights does"
            )));
        }
        if stages.contains(light) || elements.contains(light) {
            return Err(CommandError(format!("{light} is a stage or an element already, not a light")));
        }
    }
    for target in switches.iter().chain(&intensities) {
        require(&net, target.node())?;
        if !has_param(e, &net, target.node(), target.param().as_str()) {
            return Err(CommandError(format!("{} has no param {}", target.node(), target.param())));
        }
    }
    if !switches.is_empty() && stages.len() < 2 {
        return Err(CommandError(
            "switch picks between stages: give them as stage: [\"/stage_a\", \"/stage_b\", ...]".into(),
        ));
    }
    // How each switch follows the column: through the row's fade, or with a jump half way.
    let mut fades: Vec<bool> = Vec::new();
    for target in &switches {
        let node = net.resolve(target.node()).and_then(|id| net.node(id));
        let kind = node.map(|n| n.kind.as_str()).unwrap_or_default();
        let blend = node.and_then(|n| n.params.get("blend")).and_then(Value::as_bool) == Some(true);
        if kind == "tex.cross" && stages.len() > 2 {
            return Err(CommandError(format!(
                "{target}: a tex.cross fades between two stages; for {} take a tex.switch with blend on",
                stages.len()
            )));
        }
        fades.push(kind == "tex.cross" || (kind == "tex.switch" && blend));
    }
    // The readers beside the table.
    let mut readers: Vec<NodePath> = Vec::new();
    for (end, kind) in READERS {
        let path = parent
            .child(name_of(&format!("{table_name}_{end}"))?)
            .map_err(|err| CommandError(err.to_string()))?;
        if let Some(k) = kind_at(&path).filter(|k| k != kind) {
            return Err(CommandError(format!(
                "{path} is a {k}, not a {kind}: beats.arrange writes the table's readers there; rename it"
            )));
        }
        readers.push(path);
    }
    let [now, plus, next, diff, ramp, mix, half, snap, jump, pick] = &readers[..] else {
        return Err(CommandError("the readers could not be named".into()));
    };

    let (file, analysis) = analysis_of(e, &Which { path: Some(a.path.clone()), file: None })?;
    let s = analysis.state();
    let file_name =
        file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
    let Some(structure) = s.sections.as_ref() else {
        return Err(no_sections(&file_name, &s, &a.path));
    };
    let Some(bus) = s.bus.as_ref() else {
        return Err(CommandError(format!(
            "{file_name} has sections but no music bus, which says how strongly each boundary changes: \
             try beats.analyze {{path: \"{}\", again: true}}",
            a.path
        )));
    };
    let parts: Vec<Part> = structure
        .sections
        .list()
        .iter()
        .zip(bus.sections())
        .map(|(sec, c)| Part {
            start: sec.start,
            end: sec.end,
            zone: sec.zone,
            role: c.role,
            change: c.change,
            energy: c.energy,
            track_start: sec.track_start,
            seed: sec.seed,
            after_break: false,
            whole: false,
        })
        .collect();
    let end = s.envelopes.as_ref().map_or(0.0, |env| env.len() as f64 * env.hop_seconds());
    let bars: &[f64] = s.grid.as_ref().map_or(&[], |grid| &grid.bars);
    let energy = |from: f64, to: f64| mean_energy(bus, from, to);
    let parts = match (find_break, s.envelopes.as_ref()) {
        (true, Some(env)) => arrange::with_breaks(&parts, &without_bass(env, bars, end), bars, &energy),
        _ => parts,
    };
    let breaks: Vec<[f64; 2]> = parts
        .windows(2)
        .filter_map(|w| match w {
            [before, drop] if drop.after_break => Some([before.start, before.end]),
            _ => None,
        })
        .collect();
    let parts = match split_bars {
        Some(every) => arrange::split(&parts, bars, every, &energy),
        None => parts,
    };
    // The file's first section brings its slots in one after another, at the grid's bar starts.
    let (parts, opening) = match (a.opening.unwrap_or(true), s.grid.as_ref()) {
        (true, Some(grid)) => arrange::opening(&parts, &slots, &grid.bars),
        _ => (parts, 1),
    };
    let rules = arrange::Rules { gaps, dwell, opening, apart, stage_gap };
    let rows = arrange::rows_with(&parts, &slots, stages.len(), rules);
    let table = arrange::table(&slots, &rows);
    if table.len() > penumbra_core::param::MAX_TEXT {
        return Err(CommandError(format!("{} rows do not fit into a table.text", rows.len())));
    }
    let row_stages: Vec<usize> = rows.iter().map(|r| r.stage).collect();
    let steps = arrange::steps_with(&parts, &row_stages, end, fade_min, stage_fade, &file_name)
        .map_err(CommandError)?;

    // New nodes go beside the table, or below the grid when the table is new too.
    let pos_of = |path: &NodePath| net.resolve(path).and_then(|id| net.node(id)).and_then(|n| n.pos);
    let below_grid = pos_of(&a.path).map(|[x, y]| [x + 250.0, y + 400.0]).unwrap_or([0.0, 0.0]);
    // In a new component the nodes start at its own origin; the component goes below the grid.
    let [x, y] = match &home.make {
        Some(_) => [0.0, 0.0],
        None => pos_of(&home.table).unwrap_or(below_grid),
    };
    let mut ops = home.moves.clone();
    if let Some(under) = &home.make {
        ops.push(Op::Create { path: under.clone(), kind: COMP_KIND.to_string(), params: BTreeMap::new() });
        ops.push(Op::Place { path: under.clone(), pos: Some(below_grid) });
    }
    let mut create = |path: &NodePath, kind: &str, at: [f32; 2]| {
        if net.resolve(path).is_none() {
            ops.push(Op::Create { path: path.clone(), kind: kind.to_string(), params: BTreeMap::new() });
            ops.push(Op::Place { path: path.clone(), pos: Some(at) });
        }
    };
    create(&home.table, "table.text", [x, y]);
    create(&home.sequence, SEQUENCE, [x, y + 150.0]);
    for (i, (path, (_, kind))) in readers.iter().zip(READERS).enumerate() {
        // The picking readers in a second row under the blending ones.
        let (col, row) = if i < 6 { (i, 0.0) } else { (i - 4, 300.0) };
        let below = if i == 1 { 150.0 } else { 0.0 };
        create(path, kind, [x + 200.0 * (col + 1) as f32, y + below + row]);
    }
    let text = |t: &str| Value::Text(t.to_string());
    let set = |node: &NodePath, p: &str, value: Value| -> CmdResult<Op> {
        Ok(Op::Set { param: param_of(node, p)?, value })
    };
    let connect = |from: &NodePath, to: &NodePath, input: u32| Op::Connect {
        from: from.clone(),
        output: 0,
        to: to.clone(),
        input,
    };
    // A bound param would hide the value written here.
    let mut unbind = vec![param_of(&home.table, "text")?, param_of(&home.table, "format")?];
    for p in ["steps", "names", "lead_in", "file"] {
        unbind.push(param_of(&home.sequence, p)?);
    }
    for (r, p) in [
        (plus, "op"),
        (plus, "b"),
        (diff, "op"),
        (ramp, "op"),
        (mix, "op"),
        (half, "op"),
        (half, "b"),
        (snap, "op"),
        (snap, "b"),
        (jump, "op"),
        (pick, "op"),
    ] {
        unbind.push(param_of(r, p)?);
    }
    ops.extend(unbind.into_iter().map(|param| Op::Unbind { param, component: None }));
    // The table, and the sequence: a step per row, on the music's seconds.
    ops.extend([
        set(&home.table, "text", text(&table))?,
        set(&home.table, "format", text("csv"))?,
        set(&home.sequence, "steps", text(&steps.text))?,
        set(&home.sequence, "names", text(arrange::PIECE))?,
        set(&home.sequence, "lead_in", Value::Float(0.0))?,
        Op::Reset { param: param_of(&home.sequence, "file")? },
        bind_to(param_of(&home.sequence, "clock")?, &a.path, "seconds")?,
    ]);
    // The row now and the next, and between them by the share of the next.
    ops.extend([
        connect(&home.table, now, 0),
        bind_to(param_of(now, "row")?, &home.sequence, "step")?,
        connect(&home.sequence, plus, 0),
        Op::Disconnect { to: plus.clone(), input: 1 },
        set(plus, "op", text("add"))?,
        set(plus, "b", Value::Float(1.0))?,
        connect(&home.table, next, 0),
        bind_to(param_of(next, "row")?, plus, "step")?,
        connect(next, diff, 0),
        connect(now, diff, 1),
        set(diff, "op", text("subtract"))?,
        connect(diff, ramp, 0),
        Op::Disconnect { to: ramp.clone(), input: 1 },
        set(ramp, "op", text("multiply"))?,
        bind_to(param_of(ramp, "b")?, &home.sequence, "mix")?,
        connect(now, mix, 0),
        connect(ramp, mix, 1),
        set(mix, "op", text("add"))?,
        // The share of the next row rounded: 0 before the fade's middle, 1 after it.
        connect(&home.sequence, half, 0),
        Op::Disconnect { to: half.clone(), input: 1 },
        set(half, "op", text("add"))?,
        set(half, "b", Value::Float(0.5))?,
        connect(half, snap, 0),
        Op::Disconnect { to: snap.clone(), input: 1 },
        set(snap, "op", text("floor"))?,
        set(snap, "b", Value::Float(1.0))?,
        connect(diff, jump, 0),
        Op::Disconnect { to: jump.clone(), input: 1 },
        set(jump, "op", text("multiply"))?,
        bind_to(param_of(jump, "b")?, snap, "mix")?,
        connect(now, pick, 0),
        connect(jump, pick, 1),
        set(pick, "op", text("add"))?,
    ]);
    for r in [now, next] {
        for p in ["layout", "names"] {
            ops.push(Op::Unbind { param: param_of(r, p)?, component: None });
            ops.push(Op::Reset { param: param_of(r, p)? });
        }
    }
    // What the columns drive.
    let mut bound: Vec<String> = Vec::new();
    for (path, column) in elements.iter().zip(arrange::columns(&slots)) {
        ops.push(bind_to(param_of(path, "presence")?, mix, &column)?);
        bound.push(format!("{path}.presence"));
    }
    let warm: Vec<&NodePath> = stages
        .iter()
        .chain(elements.iter().filter(|p| !stages.contains(p) && has_param(e, &net, p, "warmth")))
        .collect();
    for path in warm {
        ops.push(bind_to(param_of(path, "warmth")?, mix, "warmth")?);
        bound.push(format!("{path}.warmth"));
    }
    // The camera is an index into the stage's presets: it jumps half way through a fade, and
    // the stage glides to the new preset.
    for path in stages.iter().filter(|p| has_param(e, &net, p, "camera")) {
        ops.push(bind_to(param_of(path, "camera")?, pick, "camera")?);
        bound.push(format!("{path}.camera"));
    }
    // The flight is an index too: an element that flies (the glider) glides to the new one.
    for path in elements.iter().filter(|p| !stages.contains(p) && has_param(e, &net, p, "flight")) {
        ops.push(bind_to(param_of(path, "flight")?, pick, "flight")?);
        bound.push(format!("{path}.flight"));
    }
    // The gaps fade in and out with the rows, on every stage that has them.
    for path in stages.iter().filter(|p| has_param(e, &net, p, "gap")) {
        ops.push(bind_to(param_of(path, "gap")?, mix, "gaps")?);
        bound.push(format!("{path}.gap"));
    }
    // The walls open and close through the fades, on every stage that has them.
    for path in stages.iter().filter(|p| has_param(e, &net, p, "see_through")) {
        ops.push(bind_to(param_of(path, "see_through")?, mix, "see_through")?);
        bound.push(format!("{path}.see_through"));
    }
    // A cross or a blending switch fades between two stages through the fade; anything else
    // takes the index. The stage only ever moves to a neighbour, so a fade never shows a third.
    for (target, fades) in switches.iter().zip(&fades) {
        ops.push(bind_to(target.clone(), if *fades { mix } else { pick }, "stage")?);
        bound.push(target.to_string());
    }
    for target in &intensities {
        ops.push(bind_to(target.clone(), mix, "intensity")?);
        bound.push(target.to_string());
    }
    // The room's lights: the pattern is an index and jumps half way; colour and brightness fade.
    for light in &lights {
        ops.push(bind_to(param_of(light, "pattern")?, pick, "pattern")?);
        bound.push(format!("{light}.pattern"));
        for column in ["warmth", "intensity"] {
            if has_param(e, &net, light, column) {
                ops.push(bind_to(param_of(light, column)?, mix, column)?);
                bound.push(format!("{light}.{column}"));
            }
        }
    }
    let label = format!("arrangement from the sections of {file_name}");
    let out = edit(e, &ops, &label)?;
    let mut reply = outcome(&out);
    let mut columns: Vec<String> = arrange::LEADING.iter().map(|c| c.to_string()).collect();
    columns.extend(arrange::columns(&slots));
    columns.extend(arrange::TRAILING.iter().map(|c| c.to_string()));
    if let Some(obj) = reply.as_object_mut() {
        obj.insert("file".into(), json!(file.display().to_string()));
        obj.insert("table".into(), json!(home.table.to_string()));
        obj.insert("sequence".into(), json!(home.sequence.to_string()));
        if !home.moved.is_empty() {
            let moved: BTreeMap<String, String> =
                home.moved.iter().map(|(from, to)| (from.to_string(), to.to_string())).collect();
            obj.insert("moved".into(), json!(moved));
        }
        if find_break {
            obj.insert("breaks".into(), json!(breaks));
        }
        obj.insert("rows".into(), json!(rows.len()));
        obj.insert("columns".into(), json!(columns));
        obj.insert("channels".into(), json!(mix.to_string()));
        obj.insert("picks".into(), json!(pick.to_string()));
        obj.insert("empty".into(), json!(rows.iter().filter(|r| r.empty).count()));
        obj.insert("readers".into(), json!(readers.iter().map(|p| p.to_string()).collect::<Vec<_>>()));
        obj.insert("bound".into(), json!(bound));
        obj.insert("end".into(), json!(steps.end));
    }
    Ok(reply)
}
