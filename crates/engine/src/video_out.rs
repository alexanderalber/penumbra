//! Movie recording in the engine: the record hub is a resource of the cooker; each `out.video`
//! node records through it.
//!
//! Commands: `video_out.start`, `video_out.stop`, `video_out.status`, and `video.loop` /
//! `video.loop_status` for loops of recordings (or any movie). Starting and stopping set
//! the node's `record` param, so there is one switch whoever flips it: each is an edit of its
//! own (undoable) through the gate. When the user changed something less than
//! [`Gate::QUIET`] ago, the command waits for that quiet rather than fail; while the user keeps
//! editing, or while `record` follows a channel (a binding), it fails and says why.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_ops::out::video::VIDEO_OUT;
use penumbra_project::{Actor, Gate};
use penumbra_record::looping::{LoopStatus, Method, Spec};
use penumbra_record::{Codec, Status};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// How long `video_out.start` and `.stop` wait for the node to act on the request.
const ANSWER: Duration = Duration::from_secs(3);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "video_out.start",
        "Start recording an out.video node to a new file (its params say which and how): turns its record param on (an edit); {path}; answers with the status once recording",
        start,
    );
    add(
        "video_out.stop",
        "Stop an out.video's recording and finish the file: turns record off (an edit); {path}; answers with the status",
        stop,
    );
    add(
        "video_out.status",
        "Recordings: node, recording, finishing, done, file, codec, size, fps, frames, dropped, skipped, seconds, bytes, encoder_fps, queue, error, and loop (as video.loop_status) once a loop was asked for; {path?}",
        status,
    );
    add(
        "video.loop",
        "Make a movie loop seamlessly, as a job in the background: method cut (the best stretch), fade (end cross-faded into the start), pingpong (forward, then backward) or cut_fade; writes <stem>_loop.<ext> beside it unless out names a file, never overwriting; {file, method, min_seconds? (5), fade_seconds? (1), out?, codec?}; answers at once with the status",
        make_loop,
    );
    add(
        "video.loop_status",
        "Loops: file, out, method, codec, frames_in, frames_out, loop_start, loop_end, fade_frames, score, motion, progress, done, error, seconds; {file?} (the recording or the loop) for the newest of that file",
        loop_status,
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: NodePath,
}

/// The `out.video` node at `path`.
fn node_at(e: &Engine, path: &NodePath) -> CmdResult<NodeId> {
    let net = e.session().doc().snapshot();
    let id = net.resolve(path).ok_or_else(|| CommandError(format!("no node {path}")))?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != VIDEO_OUT {
        return Err(CommandError(format!("{path} is a {kind}, not an {VIDEO_OUT}")));
    }
    Ok(id)
}

/// `s` with the node's path instead of its name.
fn with_path(e: &Engine, id: NodeId, mut s: Status) -> Json {
    if let Some(p) = e.session().doc().snapshot().path_of(id) {
        s.node = p.to_string();
    }
    serde_json::to_value(s).unwrap_or(Json::Null)
}

/// The node's `record` as stored, and whether a channel drives it.
fn record_param(e: &Engine, id: NodeId) -> (bool, bool) {
    let net = e.session().doc().snapshot();
    let Some(node) = net.node(id) else { return (false, false) };
    let on = node.params.get("record").and_then(Value::as_bool).unwrap_or(false);
    let bound = node.binds.keys().any(|k| k.param.as_str() == "record");
    (on, bound)
}

/// Sets `record` as an edit, waiting out the gate's quiet after a user edit.
fn set_record(e: &Engine, path: &NodePath, on: bool) -> CmdResult<()> {
    let name = Name::new("record").map_err(|e| CommandError(e.0))?;
    let param = ParamPath::new(path.clone(), name).map_err(|e| CommandError(e.0))?;
    let ops = [Op::Set { param, value: Value::Bool(on) }];
    let label = if on { "start recording" } else { "stop recording" };
    let deadline = Instant::now() + Gate::QUIET + Duration::from_millis(500);
    loop {
        let now = Instant::now();
        match e.session().edit(Actor::Claude, &ops, None, label, now) {
            Ok(_) => return Ok(()),
            Err(err) => {
                let gate = e.session().status(now).edit;
                let quiet = Gate::QUIET.as_secs_f64();
                let waiting = gate.mode == "user" && gate.user_idle.is_some_and(|idle| idle < quiet);
                if !waiting || now >= deadline {
                    return Err(CommandError(format!("{path}: {}", err.0)));
                }
                let rest = quiet - gate.user_idle.unwrap_or(0.0);
                std::thread::sleep(
                    Duration::from_secs_f64(rest.clamp(0.01, quiet)) + Duration::from_millis(20),
                );
            }
        }
    }
}

fn switch(e: &Engine, p: &Json, on: bool) -> CmdResult<Json> {
    let a: PathArg = args(p)?;
    let id = node_at(e, &a.path)?;
    let hub = e.record();
    let (stored, bound) = record_param(e, id);
    if bound {
        return Err(CommandError(format!(
            "{}: `record` follows a channel (a binding); unbind it to start and stop by command",
            a.path
        )));
    }
    let before = hub.status(id);
    let recording = before.as_ref().is_some_and(|s| s.recording);
    if stored == on {
        if on && !recording {
            let why = before.and_then(|s| s.error).unwrap_or_else(|| "see node.info".into());
            return Err(CommandError(format!(
                "{}: `record` is on but it is not recording ({why}); video_out.stop, then start again",
                a.path
            )));
        }
    } else {
        set_record(e, &a.path, on)?;
    }
    // The node acts at its next cook: wait until its status moves. A new recording's thread
    // creates the file, numbered when the name is taken, so the answer waits for that too and
    // names the file actually written.
    let already = stored && recording;
    let deadline = Instant::now() + ANSWER;
    loop {
        let now = hub.status(id);
        let settled = if on {
            now.as_ref().is_some_and(|s| {
                let ended = s.error.is_some() || s.done;
                (already || now != before) && (ended || (s.recording && s.opened))
            })
        } else {
            now.as_ref().is_none_or(|s| !s.recording)
        };
        let late = Instant::now() >= deadline;
        if settled || late {
            let mut out = match now {
                Some(s) => with_path(e, id, s),
                None => json!({ "node": a.path.to_string(), "recording": false }),
            };
            if late
                && !settled
                && let Some(o) = out.as_object_mut()
            {
                let note = "the node has not acted yet; node.info shows its error, if any";
                o.insert("note".into(), json!(note));
            }
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start(e: &Engine, p: &Json) -> CmdResult<Json> {
    switch(e, p, true)
}

fn stop(e: &Engine, p: &Json) -> CmdResult<Json> {
    switch(e, p, false)
}

fn status(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
    }
    let a: A = args(p)?;
    let hub = e.record();
    if let Some(path) = a.path {
        let id = node_at(e, &path)?;
        return Ok(match hub.status(id) {
            Some(s) => with_path(e, id, s),
            None => json!({ "node": path.to_string(), "recording": false }),
        });
    }
    let list: Vec<Json> = hub.all().into_iter().map(|(id, s)| with_path(e, id, s)).collect();
    Ok(json!({ "recordings": list }))
}

/// `path` relative to the project's folder, unless absolute.
fn project_path(e: &Engine, path: &str) -> PathBuf {
    let p = Path::new(path.trim());
    if p.is_absolute() { p.to_path_buf() } else { e.session().dir().join(p) }
}

fn loop_json(s: LoopStatus) -> Json {
    serde_json::to_value(s).unwrap_or(Json::Null)
}

fn make_loop(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        file: String,
        method: String,
        min_seconds: Option<f64>,
        fade_seconds: Option<f64>,
        out: Option<String>,
        codec: Option<String>,
    }
    let a: A = args(p)?;
    let method = Method::parse(&a.method).ok_or_else(|| {
        CommandError(format!("unknown method `{}`; one of {}", a.method, Method::NAMES.join(", ")))
    })?;
    let codec = match a.codec.as_deref() {
        None => None,
        Some(c) => Some(Codec::parse(c).ok_or_else(|| {
            CommandError(format!("unknown codec `{c}`; one of {}", Codec::NAMES.join(", ")))
        })?),
    };
    let seconds = |name: &str, v: Option<f64>, default: f64| match v {
        None => Ok(default),
        Some(v) if v.is_finite() && (0.0..=3600.0).contains(&v) => Ok(v),
        Some(v) => Err(CommandError(format!("{name} {v} is not 0 to 3600 seconds"))),
    };
    let mut spec = Spec::new(method);
    spec.min_seconds = seconds("min_seconds", a.min_seconds, spec.min_seconds)?;
    spec.fade_seconds = seconds("fade_seconds", a.fade_seconds, spec.fade_seconds)?;
    spec.out = a.out.as_deref().filter(|o| !o.trim().is_empty()).map(|o| project_path(e, o));
    spec.codec = codec;
    let file = project_path(e, &a.file);
    let job = e.record().loops().start(&file, spec).map_err(CommandError)?;
    Ok(loop_json(job.status()))
}

fn loop_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        file: Option<String>,
    }
    let a: A = args(p)?;
    let all = e.record().loops().all();
    let Some(file) = a.file else {
        return Ok(json!({ "loops": all.into_iter().map(loop_json).collect::<Vec<_>>() }));
    };
    let path = project_path(e, &file);
    all.into_iter()
        .rev()
        .find(|s| Path::new(&s.file) == path || Path::new(&s.out) == path)
        .map(loop_json)
        .ok_or_else(|| CommandError(format!("no loop of {} was made since the app started", path.display())))
}
