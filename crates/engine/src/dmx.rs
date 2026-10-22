//! DMX in the engine: the hub is a resource of the cooker, the `out.dmx` nodes put their slots
//! into it; the inputs are another, the `chan.dmx_in` nodes read from them.
//!
//! Commands: `dmx.ports`, `dmx.status`, `dmx.config`.

use std::sync::Arc;

use penumbra_dmx::serial;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "dmx.ports",
        "The serial ports of this machine (name, USB ids, whether it is an FT232R like the Open DMX USB)",
        dmx_ports,
    );
    add(
        "dmx.status",
        "DMX outputs (target, writers, device, open, error, frames, rate asked and measured, timings in ms) \
         and inputs (listen, readers, open, error, packets, universes with their sources)",
        dmx_status,
    );
    add(
        "dmx.config",
        "DMX output settings for this run: {enabled?, blackout?}; blackout sends zeros on every output",
        dmx_config,
    );
}

fn dmx_ports(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let ports = serial::ports().map_err(CommandError)?;
    Ok(json!({ "ports": ports }))
}

fn dmx_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.dmx();
    Ok(json!({ "config": hub.config(), "outputs": hub.status(), "inputs": e.dmx_in().status() }))
}

fn dmx_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
        blackout: Option<bool>,
    }
    let a: A = args(p)?;
    let hub = e.dmx();
    let mut c = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    if let Some(v) = a.blackout {
        c.blackout = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}
