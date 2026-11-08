//! MIDI in the engine: the hub's devices follow the network's `chan.midi_in` and
//! `out.midi` nodes ([`penumbra_ops::midi::wants`]); the controls a device moves go to the
//! OSC hub's learn as `/midi/<device>/cc/<channel>/<number>`, so `osc.learn` binds a param to
//! the next control moved.
//!
//! Commands: `midi.status`, `midi.config`, `midi.inject`, `midi.send`.

use std::sync::Arc;

use penumbra_midi::hub::Config;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Bytes one `midi.inject` or `midi.send` takes at most.
const MAX_BYTES: usize = 65_536;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "midi.status",
        "MIDI devices of this machine, the inputs and outputs open (messages, drops, errors, the clock's tempo), recent messages",
        midi_status,
    );
    add("midi.config", "MIDI settings for this run: {enabled?}; off closes every device", midi_config);
    add(
        "midi.inject",
        "Take in MIDI bytes as if the input `device` had sent them (made if no device has that name): {device, bytes: [176, 7, 127]}. For trying a project without the controller",
        midi_inject,
    );
    add(
        "midi.send",
        "Send MIDI messages to an output: {device (part of its name), bytes: [144, 36, 127]}; system exclusive is left out. The output must be open (an out.midi names it)",
        midi_send,
    );
}

fn midi_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    serde_json::to_value(e.midi().status()).map_err(|err| err.to_string().into())
}

fn midi_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
    }
    let a: A = args(p)?;
    let hub = e.midi();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    hub.set_config(c.clone());
    Ok(json!({"config": c}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bytes {
    device: String,
    bytes: Vec<u8>,
}

fn bytes_of(p: &Json) -> CmdResult<Bytes> {
    let a: Bytes = args(p)?;
    if a.device.trim().is_empty() {
        return Err("`device` names the device".into());
    }
    if a.bytes.len() > MAX_BYTES {
        return Err(format!("at most {MAX_BYTES} bytes").into());
    }
    Ok(a)
}

fn midi_inject(e: &Engine, p: &Json) -> CmdResult<Json> {
    let a = bytes_of(p)?;
    e.midi().inject(a.device.trim(), &a.bytes, None);
    Ok(json!({"device": a.device.trim(), "bytes": a.bytes.len()}))
}

fn midi_send(e: &Engine, p: &Json) -> CmdResult<Json> {
    let a = bytes_of(p)?;
    let hub = e.midi();
    let name = hub
        .output_for(&a.device)
        .ok_or_else(|| CommandError::from(format!("no MIDI output matches `{}`", a.device)))?;
    if !hub.output(&name).is_some_and(|s| s.connected) {
        return Err(format!("{name} is not open: an out.midi has to name it").into());
    }
    // One message per send, as a driver wants them; whatever is no whole message is left out.
    let mut events = Vec::new();
    let mut reader = penumbra_midi::Reader::new();
    reader.read(&a.bytes, &mut |ev| events.push(ev));
    let mut sent = 0;
    for bytes in events.iter().filter_map(penumbra_midi::encode) {
        if !hub.send(&name, bytes) {
            return Err(format!("the MIDI queue is full after {sent} messages").into());
        }
        sent += 1;
    }
    Ok(json!({"device": name, "messages": sent, "skipped": reader.skipped}))
}
