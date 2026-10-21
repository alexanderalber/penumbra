//! Movie recording in the engine: the record hub is a resource of the cooker; each `out.video`
//! node records through it.
//!
//! Commands: `video_out.start`, `video_out.stop`, `video_out.status`. Starting and stopping set
//! the node's `record` param, so there is one switch whoever flips it: each is an edit of its
//! own (undoable) through the gate. When the user changed something less than
//! [`Gate::QUIET`] ago, the command waits for that quiet rather than fail; while the user keeps
//! editing, or while `record` follows a channel (a binding), it fails and says why.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_ops::out::video::VIDEO_OUT;
use penumbra_project::{Actor, Gate};
use penumbra_record::Status;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// How long `video_out.start` and `.stop` wait for the node to act on the request.
const ANSWER: Duration = Duration::from_secs(3);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> CmdResult<Json>| {
        let engine = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    };
    add(
        "video_out.start",
        "Start recording an out.video node to a new file (its params say which and how) by turning its record param on (an edit); {path}; answers with the status once recording",
        start,
    );
    add(
        "video_out.stop",
        "Stop an out.video's recording and finish the file by turning record off (an edit); {path}; answers with the status",
        stop,
    );
    add(
        "video_out.status",
        "Recordings: node, recording, finishing, done, file, codec, size, fps, frames, dropped, skipped, seconds, bytes, encoder_fps, queue, error; {path?}",
        status,
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
