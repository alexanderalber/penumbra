//! Table files in the engine: `out.table` nodes write through [`TableOuts`], a resource of the
//! cooker.
//!
//! Commands: `table_out.write`, `table_out.status`. Writing is a request to the node: the node
//! writes at its next cook, on a job, and the command waits for the answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, NodePath};
use penumbra_ops::out::table::{OUT_TABLE, Status, TableOuts};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, require};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// How long `table_out.write` waits for the node to write.
const ANSWER: Duration = Duration::from_secs(5);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "table_out.write",
        "Write an out.table's input to its file now, in either mode; {path}; answers with the status once written, fails with the node's error",
        write,
    );
    add(
        "table_out.status",
        "Table files: node, file, mode, written_seconds_ago, rows, cols, bytes, writing, error; {path?}",
        status,
    );
}

/// The `out.table` node at `path`.
fn node_at(e: &Engine, path: &NodePath) -> CmdResult<NodeId> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != OUT_TABLE {
        return Err(CommandError(format!("{path} is a {kind}, not an {OUT_TABLE}")));
    }
    Ok(id)
}

/// The open project's table writers.
fn outs(e: &Engine) -> CmdResult<TableOuts> {
    e.on_cook(|st| st.cooker.resource::<TableOuts>().cloned())?
        .ok_or_else(|| CommandError("table files are not available in this process".into()))
}

/// `s` with the node's path instead of nothing.
fn with_path(e: &Engine, id: NodeId, mut s: Status) -> Json {
    if let Some(p) = e.session().doc().snapshot().path_of(id) {
        s.node = p.to_string();
    }
    serde_json::to_value(s).unwrap_or(Json::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: NodePath,
}

fn write(e: &Engine, p: &Json) -> CmdResult<Json> {
    let a: PathArg = args(p)?;
    let id = node_at(e, &a.path)?;
    let outs = outs(e)?;
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
                    json!("the node has not written yet; table_out.status tells when it has"),
                );
            }
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = outs.status(id).unwrap_or_default();
    if let Some(err) = &s.error {
        return Err(CommandError(format!("{}: {err}", a.path)));
    }
    Ok(with_path(e, id, s))
}

fn status(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
    }
    let a: A = args(p)?;
    let outs = outs(e)?;
    if let Some(path) = a.path {
        let id = node_at(e, &path)?;
        let s = outs.status(id).unwrap_or_default();
        return Ok(with_path(e, id, s));
    }
    let list: Vec<Json> = outs.all().into_iter().map(|(id, s)| with_path(e, id, s)).collect();
    Ok(json!({ "tables": list }))
}
