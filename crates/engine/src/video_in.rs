//! Cameras in the engine: the hub's streams follow the network's `tex.video_in` nodes (the cook
//! thread calls [`penumbra_ops::tex::video_in::wants`] when the network changes).
//!
//! Commands: `video_in.devices`, `video_in.status`, `video_in.config`.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use penumbra_camera::Config;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "video_in.devices",
        "The cameras of this machine with their modes (size, rate, pixel format, whether penumbra reads it)",
        video_in_devices,
    );
    add(
        "video_in.status",
        "Camera streams (device, mode, open, error, frames, fps, dropped, delay_ms from the camera's timestamp)",
        video_in_status,
    );
    add("video_in.config", "Camera settings for this run: {enabled?}", video_in_config);
}

/// How long `video_in.devices` waits for the system's list of cameras. Listing opens each
/// camera, and a driver that hangs would otherwise hold the control channel.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

fn video_in_devices(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.camera().clone();
    let (tx, rx) = mpsc::channel();
    // Not joined: a listing stuck in a driver is left behind rather than waited for.
    std::thread::Builder::new()
        .name("camera list".into())
        .spawn(move || {
            let _ = tx.send(hub.devices());
        })
        .map_err(|e| CommandError(format!("cannot start listing the cameras: {e}")))?;
    let list = match rx.recv_timeout(LIST_TIMEOUT) {
        Ok(list) => list.map_err(CommandError)?,
        Err(_) => {
            return Err(CommandError(format!(
                "the system did not list its cameras within {} s; a camera driver may hang",
                LIST_TIMEOUT.as_secs()
            )));
        }
    };
    Ok(json!({ "devices": list }))
}

fn video_in_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.camera();
    Ok(json!({
        "enabled": hub.config().enabled,
        "wanted": hub.wanted(),
        "streams": hub.status(),
    }))
}

fn video_in_config(e: &Engine, p: &Json) -> CmdResult<Json> {
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
