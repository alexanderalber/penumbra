//! Blob trackers in the engine. Command: `blob_track.learn`.

use std::sync::Arc;
use std::time::Duration;

use penumbra_core::NodePath;
use penumbra_ops::tex::blob_track::{BLOB_TRACK, BlobTracks};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::require;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "blob_track.learn",
        "A tex.blob_track takes a frame as the background (the empty scene): {path, delay?} (seconds before it does, default 0)",
        move |p| learn_command(&e, p),
    ));
}

fn learn_command(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        delay: Option<f64>,
    }
    let a: A = args(p)?;
    let delay = learn(e, &a.path, a.delay.unwrap_or(0.0))?;
    Ok(json!({"path": a.path.to_string(), "delay": delay.as_secs_f64()}))
}

/// `blob_track.learn`: the `tex.blob_track` at `path` takes the first frame after `delay`
/// seconds as its background. Returns the delay.
pub fn learn(e: &Engine, path: &NodePath, delay: f64) -> CmdResult<Duration> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != BLOB_TRACK {
        return Err(CommandError(format!("{path} is a {kind}, not a {BLOB_TRACK}")));
    }
    if !(0.0..=600.0).contains(&delay) {
        return Err(CommandError("`delay` must be 0 to 600 seconds".into()));
    }
    let delay = Duration::from_secs_f64(delay);
    let learned =
        e.on_cook(move |st| st.cooker.resource::<BlobTracks>().is_some_and(|t| t.learn(id, delay)))?;
    if !learned {
        return Err(CommandError(format!("{path} has not cooked yet (is it shown or connected?)")));
    }
    Ok(delay)
}
