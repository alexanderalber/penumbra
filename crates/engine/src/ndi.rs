//! NDI in the engine: `out.ndi` and `tex.ndi_in` need nothing from it but the GPU.
//!
//! Commands: `ndi.sources`.

use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry};

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry) {
    registry.register(FnCommand::new(
        "ndi.sources",
        "The NDI sources on the network, this process's out.ndi nodes included (name as `PC (Source)`, address), and the NDI runtime in use. The first call starts looking: `settling` is true while the list may still be filling",
        sources,
    ));
}

fn sources(_p: &Json) -> CmdResult<Json> {
    let runtime = penumbra_ndi::runtime().map_err(CommandError)?;
    let list = penumbra_ndi::sources().map_err(CommandError)?;
    let sources: Vec<Json> = list.iter().map(|s| json!({ "name": s.name, "address": s.url })).collect();
    Ok(json!({
        "sources": sources,
        "settling": penumbra_ndi::settling(),
        "runtime": runtime,
        "about": "NDI\u{ae} is a registered trademark of Vizrt NDI AB, https://ndi.video/",
    }))
}
