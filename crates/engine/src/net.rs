//! Serial ports, UDP and TCP in the engine: the hub is a resource of the cooker, the
//! `chan.data_in` and `chan.data_out` nodes open their links through it.
//!
//! Commands: `net.ports`, `net.status`.

use std::sync::Arc;

use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry};
use crate::commands::engine_commands;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "net.ports",
        "The serial ports of this machine for chan.data_in and chan.data_out (name, USB ids, product)",
        net_ports,
    );
    add(
        "net.status",
        "Serial, UDP and TCP links of chan.data_in and chan.data_out: endpoint, readers, writers, open, local \
         address, peers, error, bytes and messages in and out, drops, connects",
        net_status,
    );
}

fn net_ports(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let ports = penumbra_dmx::serial::ports().map_err(CommandError)?;
    Ok(json!({ "ports": ports }))
}

fn net_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    Ok(json!({ "links": e.net().status() }))
}
