//! What the commands of the file writing nodes share (`table_out.*`, `mesh_out.*`): writing is a
//! request to the node, which writes at its next cook, on a job, while the command waits for the
//! answer.

use std::time::{Duration, Instant};

use penumbra_core::{NodeId, NodePath};
use penumbra_ops::out::file::{Report, Writers};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, args};
use crate::commands::require;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// How long a write command waits for the node to write.
const ANSWER: Duration = Duration::from_secs(5);

/// One kind of writer: its node kind, the status command and the key `status` lists under.
pub struct Kind {
    pub node: &'static str,
    pub status: &'static str,
    pub list: &'static str,
}

/// The node of `kind` at `path`.
fn node_at(e: &Engine, path: &NodePath, kind: &Kind) -> CmdResult<NodeId> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let found = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if found != kind.node {
        return Err(CommandError(format!("{path} is a {found}, not an {}", kind.node)));
    }
    Ok(id)
}

/// The open project's writers of one kind.
fn outs<S: Report + Sync>(e: &Engine) -> CmdResult<Writers<S>> {
    e.on_cook(|st| st.cooker.resource::<Writers<S>>().cloned())?
        .ok_or_else(|| CommandError("file writers are not available in this process".into()))
}

/// `s` with the node's path instead of nothing.
fn with_path<S: Report + Serialize>(e: &Engine, id: NodeId, mut s: S) -> Json {
    if let Some(p) = e.session().doc().snapshot().path_of(id) {
        s.set_node(p.to_string());
    }
    serde_json::to_value(s).unwrap_or(Json::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: NodePath,
}

/// `<kind>_out.write {path}`.
pub fn write<S: Report + Serialize + Sync>(e: &Engine, p: &Json, kind: &Kind) -> CmdResult<Json> {
    let a: PathArg = args(p)?;
    let id = node_at(e, &a.path, kind)?;
    let outs = outs::<S>(e)?;
    let ticket = outs
        .request(id)
        .ok_or_else(|| CommandError(format!("{} has not cooked yet; try again in a moment", a.path)))?;
    let deadline = Instant::now() + ANSWER;
    while outs.answered(id) < ticket {
        if Instant::now() >= deadline {
            let mut out = outs.status(id).map_or(Json::Null, |s| with_path(e, id, s));
            if let Some(o) = out.as_object_mut() {
                o.insert(
                    "note".into(),
                    json!(format!("the node has not written yet; {} tells when it has", kind.status)),
                );
            }
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = outs.status(id).unwrap_or_default();
    if let Some(err) = s.error() {
        return Err(CommandError(format!("{}: {err}", a.path)));
    }
    Ok(with_path(e, id, s))
}

/// `<kind>_out.status {path?}`.
pub fn status<S: Report + Serialize + Sync>(e: &Engine, p: &Json, kind: &Kind) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
    }
    let a: A = args(p)?;
    let outs = outs::<S>(e)?;
    if let Some(path) = a.path {
        let id = node_at(e, &path, kind)?;
        let s = outs.status(id).unwrap_or_default();
        return Ok(with_path(e, id, s));
    }
    let list: Vec<Json> = outs.all().into_iter().map(|(id, s)| with_path(e, id, s)).collect();
    Ok(json!({ kind.list: list }))
}
