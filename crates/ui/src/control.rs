//! The `ui.*` commands: the control channel's way into the UI thread.
//!
//! The commands live in the same [`Registry`] as the engine's. Each one sends a [`UiRequest`]
//! through the [`Bridge`] and waits for the UI thread to answer between two frames (it asks
//! for a repaint, so an idle window wakes up). Synthetic input is fed one pointer event per
//! frame, so egui sees a press, moves and a release as a real drag; the reply comes once it is
//! processed.
//!
//! - `ui.state {positions?}`: selection, viewer, pan and zoom, editing mode (and node positions)
//! - `ui.elements {prefix?}`: automation ids with their rects ([`crate::automation`])
//! - `ui.click {id | x, y, button?, double?, ctrl?, shift?, alt?}`
//! - `ui.drag {from: {id | x, y}, to: {id | x, y}, button?, steps?}`
//! - `ui.scroll {id | x, y, dy}` (the editor zooms on the wheel)
//! - `ui.key {key, ctrl?, shift?, alt?}`, `ui.type {text}`
//! - `ui.select {paths}`, `ui.view {path?}` (pin the viewer; no path unpins)
//! - `ui.place {nodes: {path: [x, y]}}`, `ui.arrange`, `ui.frame`
//! - `ui.screenshot {file?}`: PNG of the window

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use penumbra_engine::{CommandError, FnCommand, Registry};
use serde_json::Value as Json;

/// How long a `ui.*` command waits for the UI thread (a minimised window may not draw at all).
const TIMEOUT: Duration = Duration::from_secs(10);

pub type Reply = Result<Json, String>;

pub struct UiRequest {
    pub method: String,
    pub params: Json,
    pub reply: Sender<Reply>,
}

/// The sending side, shared with the control channel's threads.
pub struct Bridge {
    tx: Mutex<Sender<UiRequest>>,
    ctx: OnceLock<egui::Context>,
}

impl Bridge {
    /// The bridge and the receiver the app reads requests from.
    pub fn new() -> (Arc<Bridge>, Receiver<UiRequest>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(Bridge { tx: Mutex::new(tx), ctx: OnceLock::new() }), rx)
    }

    /// Called by the app once its egui context exists, so requests can wake it up.
    pub fn attach(&self, ctx: egui::Context) {
        self.ctx.set(ctx).ok();
    }

    pub fn call(&self, method: &str, params: &Json) -> Result<Json, CommandError> {
        let (reply, answer) = mpsc::channel();
        let request = UiRequest { method: method.to_string(), params: params.clone(), reply };
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(request)
            .map_err(|_| CommandError("the UI has closed".into()))?;
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
        match answer.recv_timeout(TIMEOUT) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(CommandError(e)),
            Err(RecvTimeoutError::Timeout) => {
                Err(CommandError("the UI did not answer in time (is the window minimised?)".into()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(CommandError("the UI dropped the request".into())),
        }
    }
}

const COMMANDS: &[(&str, &str)] = &[
    ("ui.state", "Selection, viewer, pan and zoom, editing mode; {positions?}"),
    ("ui.elements", "Automation ids with rects in points; {prefix?}"),
    ("ui.click", "Click an element or a point; {id | x, y, button?, double?, ctrl?, shift?, alt?}"),
    ("ui.drag", "Drag between elements or points; {from: {id | x, y}, to: {id | x, y}, button?, steps?}"),
    ("ui.scroll", "Mouse wheel over an element or point; {id | x, y, dy}"),
    ("ui.key", "Press a key (egui name: Delete, Tab, F, Z, Enter, ...); {key, ctrl?, shift?, alt?}"),
    ("ui.type", "Type text into the focused field; {text}"),
    ("ui.select", "Select nodes in the editor (it shows the component they sit in); {paths}"),
    ("ui.enter", "Show the inside of a component in the editor, the top level without a path; {path?}"),
    ("ui.view", "Pin the viewer to a node, or unpin without a path; {path?}"),
    ("ui.place", "Move nodes in the editor; {nodes: {path: [x, y]}}"),
    ("ui.options", "The editor's options (right-click on empty space); {chan_bars?}"),
    ("ui.arrange", "Lay out all nodes again by depth and frame them"),
    ("ui.frame", "Fit all nodes into the editor"),
    ("ui.screenshot", "PNG of the window; {file?} (default %TEMP%/penumbra/ui.png)"),
];

/// Registers the `ui.*` commands, all forwarded to the UI thread through `bridge`.
pub fn register(registry: &mut Registry, bridge: &Arc<Bridge>) {
    for (id, summary) in COMMANDS {
        let bridge = Arc::clone(bridge);
        registry.register(FnCommand::new(id, summary, move |p| bridge.call(id, p)));
    }
}
