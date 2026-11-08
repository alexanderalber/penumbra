//! Screen capture in the engine: `tex.screen_in` needs nothing from it but the GPU.
//!
//! Commands: `screen.sources`.

use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry};

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry) {
    registry.register(FnCommand::new(
        "screen.sources",
        "What a tex.screen_in can capture now: the monitors (index, id, os_name, width, height, primary) and the windows with a title, front to back (title, program, width, height)",
        sources,
    ));
}

fn sources(_p: &Json) -> CmdResult<Json> {
    if !penumbra_screen::SUPPORTED {
        return Err(CommandError("screen capture exists only on Windows so far".into()));
    }
    let s = penumbra_screen::sources();
    Ok(json!({ "monitors": s.monitors, "windows": s.windows }))
}
