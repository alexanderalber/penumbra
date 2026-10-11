//! Cameras in the engine: the hub's streams follow the network's `tex.videoin` nodes (the cook
//! thread calls [`penumbra_ops::tex::videoin::wants`] when the network changes).
//!
//! Commands: `videoin.devices`, `videoin.status`, `videoin.config`.

use std::sync::Arc;

use penumbra_camera::Config;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> CmdResult<Json>| {
        let engine = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    };
    add(
        "videoin.devices",
        "The cameras of this machine with their modes (size, rate, pixel format, whether penumbra reads it)",
        videoin_devices,
    );
    add(
        "videoin.status",
        "Camera streams (device, mode, open, error, frames, fps, dropped, delay_ms from the camera's timestamp)",
        videoin_status,
    );
    add("videoin.config", "Camera settings for this run: {enabled?}", videoin_config);
}

fn videoin_devices(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = e.camera().devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn videoin_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.camera();
    Ok(json!({
        "enabled": hub.config().enabled,
        "wanted": hub.wanted(),
        "streams": hub.status(),
    }))
}

fn videoin_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
    }
    let a: A = args(p)?;
    let hub = e.camera();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}
