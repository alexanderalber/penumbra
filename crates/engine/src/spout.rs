//! Spout in the engine: `out.spout` and `tex.spout_in` need nothing from it but the GPU.
//!
//! Commands: `spout.senders`.

use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry};

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry) {
    registry.register(FnCommand::new(
        "spout.senders",
        "The Spout senders running on this machine, this process's out.spout nodes included (name, width, height, format, program), and the one a tex.spout_in without a name takes",
        senders,
    ));
}

fn senders(_p: &Json) -> CmdResult<Json> {
    if !penumbra_spout::SUPPORTED {
        return Err(CommandError("Spout exists only on Windows".into()));
    }
    Ok(json!({ "senders": penumbra_spout::senders(), "default": penumbra_spout::default_sender() }))
}
