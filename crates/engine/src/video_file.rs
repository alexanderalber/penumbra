//! Video file players in the engine. Commands: `video_file.restart`, `video_file.seek`; the seek
//! itself also serves `audio.seek`.

use std::sync::Arc;

use penumbra_core::NodePath;
use penumbra_ops::seek::Seeks;
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
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "video_file.seek",
        "A tex.video_file jumps to `seconds` into its clip at the next frame and plays on from there (a looping clip stays in its pass): {path, seconds}. Not while its position is bound to a channel",
        move |p| seek_command(&e, p, VIDEO_FILE),
    ));
}

/// `video_file.seek` and `audio.seek`: {path, seconds} for a node of `kind`.
pub fn seek_command(e: &Engine, p: &Json, kind: &str) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        seconds: f64,
    }
    let a: A = args(p)?;
    if !a.seconds.is_finite() || a.seconds < 0.0 {
        return Err(CommandError(format!("seconds must be 0 or more, not {}", a.seconds)));
    }
    seek(e, &a.path, kind, a.seconds)?;
    Ok(json!({"path": a.path.to_string(), "seconds": a.seconds}))
}

/// The node of `kind` at `path` jumps to `seconds` into its file at its next cook.
pub fn seek(e: &Engine, path: &NodePath, kind: &str, seconds: f64) -> CmdResult<()> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let node = net.node(id);
    let found = node.map(|n| n.kind.clone()).unwrap_or_default();
    if found != kind {
        return Err(CommandError(format!("{path} is a {found}: this command is for {kind} nodes")));
    }
    if node.is_some_and(|n| n.is_bound("position")) {
        return Err(CommandError(format!(
            "{path} has its position bound to a channel, which decides where it is: unbind it to seek"
        )));
    }
    let asked = e.on_cook(move |st| st.cooker.resource::<Seeks>().is_some_and(|s| s.request(id, seconds)))?;
    if !asked {
        return Err(CommandError(format!("{path} has not cooked yet (is it shown or connected?)")));
    }
    Ok(())
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
