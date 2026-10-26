//! Video file players in the engine. Command: `video_file.restart`.

use std::sync::Arc;

use penumbra_core::NodePath;
use penumbra_ops::tex::VIDEO_FILE;
use penumbra_ops::tex::video_file::VideoRestarts;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::require;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "video_file.restart",
        "A tex.video_file starts its clip over from its `start`, at the next frame: {path}",
        move |p| restart_command(&e, p),
    ));
}

fn restart_command(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
    }
    let a: A = args(p)?;
    restart(e, &a.path)?;
    Ok(json!({"path": a.path.to_string()}))
}

/// `video_file.restart`: the `tex.video_file` at `path` starts its clip over at its next cook.
pub fn restart(e: &Engine, path: &NodePath) -> CmdResult<()> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != VIDEO_FILE {
        return Err(CommandError(format!("{path} is a {kind}, not a {VIDEO_FILE}")));
    }
    let asked = e.on_cook(move |st| st.cooker.resource::<VideoRestarts>().is_some_and(|r| r.request(id)))?;
    if !asked {
        return Err(CommandError(format!("{path} has not cooked yet (is it shown or connected?)")));
    }
    Ok(())
}
