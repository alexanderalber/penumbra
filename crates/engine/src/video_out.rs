//! Movie recording in the engine: the record hub is a resource of the cooker; each `out.video`
//! node records through it.
//!
//! Commands: `video_out.start`, `video_out.stop`, `video_out.status`. Starting and stopping are
//! requests to the node, not edits (its `record` param stays as it is), so they work while
//! someone else is editing; the node takes them at its next cook.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, NodePath};
use penumbra_ops::out::video::VIDEO_OUT;
use penumbra_record::{Request, Status};
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
        "Start recording an out.video node to a new file (its params say which and how); {path}; answers with the status once recording",
        start,
    );
    add(
        "video_out.stop",
        "Stop an out.video's recording and finish the file; {path}; answers with the status",
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

fn request(e: &Engine, p: &Json, r: Request) -> CmdResult<Json> {
    let a: PathArg = args(p)?;
    let id = node_at(e, &a.path)?;
    let hub = e.record();
    let before = hub.status(id);
    hub.request(id, r).map_err(|m| CommandError(format!("{}: {m}", a.path)))?;
    // The node acts at its next cook: wait until its status moves.
    let deadline = Instant::now() + ANSWER;
    loop {
        let now = hub.status(id);
        let settled = match (r, &now) {
            (Request::Start, Some(s)) => (s.recording || s.error.is_some()) && now != before,
            (Request::Start, None) => false,
            (Request::Stop, s) => s.as_ref().is_none_or(|s| !s.recording),
        };
        if settled || Instant::now() >= deadline {
            return Ok(match now {
                Some(s) => with_path(e, id, s),
                None => json!({ "node": a.path.to_string(), "recording": false }),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start(e: &Engine, p: &Json) -> CmdResult<Json> {
    request(e, p, Request::Start)
}

fn stop(e: &Engine, p: &Json) -> CmdResult<Json> {
    request(e, p, Request::Stop)
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
