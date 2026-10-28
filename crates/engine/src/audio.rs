//! Audio in the engine: the hub's streams follow the network's `audio.device_in` nodes (the cook
//! thread calls [`penumbra_ops::audio::wants`] when the network changes).
//!
//! Commands: `audio.devices`, `audio.status`, `audio.config`.

use std::sync::Arc;

use penumbra_audio::capture::{self, Config};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
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
        "audio.status",
        "Audio input streams (device, rate, channels, open, error, frames, glitches, peak per channel in dBFS)",
        audio_status,
    );
    add("audio.config", "Audio settings for this run: {enabled?}", audio_config);
}

fn audio_devices(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = capture::devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn audio_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.audio();
    Ok(json!({
        "enabled": hub.config().enabled,
        "wanted": hub.wanted(),
        "streams": hub.status(),
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
