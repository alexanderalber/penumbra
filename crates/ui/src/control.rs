//! The `ui.*` commands: the control channel's way into the UI thread.
//!
//! The commands live in the same [`Registry`] as the engine's. Each one sends a [`UiRequest`]
//! through the [`Bridge`] and waits for the UI thread to answer between two frames (it asks
//! for a repaint, so an idle window wakes up). Synthetic input is fed one pointer event per
//! frame, so egui sees a press, moves and a release as a real drag; the reply comes once it is
//! processed.
//!
//! - `ui.state {positions?}`: selection, viewer, pan and zoom, editing mode, the status bar's
//!   message (and node positions)
//! - `ui.elements {prefix?}`: automation ids with their rects ([`crate::automation`])
//! - `ui.click {id | x, y, button?, double?, ctrl?, shift?, alt?}`
//! - `ui.drag {from: {id | x, y}, to: {id | x, y}, button?, steps?}`
//! - `ui.scroll {id | x, y, dy}` (the editor zooms on the wheel)
//! - `ui.key {key, ctrl?, shift?, alt?}`, `ui.type {text}`
//! - `ui.select {paths}`, `ui.view {path?}` (pin the viewer; no path unpins)
//! - `ui.camera {mode?, home?, camera?, orbit?, pan?, dolly?, travel?, look?, roll?, take?}`: the
//!   viewer's free view and steering ([`crate::navigate`])
//! - `ui.arrange {nodes?: [path]}` (moves are edits: `node.place` sets places directly),
//!   `ui.frame`
//! - `ui.measure`: how tidy the level shown looks (size, wire length, crossings, overlaps)
//! - `ui.screenshot {file?}`: PNG of the window
//!
//! `help.open {kind?, open?}` is registered here too but answers on the calling thread: it needs
//! nothing of the UI, only whether this run may open a browser ([`Bridge::allow_browser`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use penumbra_core::lock;
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
    /// Whether `help.open` and the `?` buttons may start a browser: only in the app with a
    /// window, never in headless runs and tests.
    browser: AtomicBool,
}

impl Bridge {
    /// The bridge and the receiver the app reads requests from.
    pub fn new() -> (Arc<Bridge>, Receiver<UiRequest>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(Bridge { tx: Mutex::new(tx), ctx: OnceLock::new(), browser: AtomicBool::new(false) }), rx)
    }

    /// Lets `help.open` and the `?` buttons open the documentation in the browser.
    pub fn allow_browser(&self) {
        self.browser.store(true, Ordering::Relaxed);
    }

    pub fn browser(&self) -> bool {
        self.browser.load(Ordering::Relaxed)
    }

    /// Called by the app once its egui context exists, so requests can wake it up.
    pub fn attach(&self, ctx: egui::Context) {
        self.ctx.set(ctx).ok();
    }

    pub fn call(&self, method: &str, params: &Json) -> Result<Json, CommandError> {
        let (reply, answer) = mpsc::channel();
        let request = UiRequest { method: method.to_string(), params: params.clone(), reply };
        lock(&self.tx).send(request).map_err(|_| CommandError("the UI has closed".into()))?;
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
    ("ui.state", "Selection, viewer, pan and zoom, editing mode, the status bar's message; {positions?}"),
    ("ui.elements", "Automation ids with rects in points; {prefix?}"),
    ("ui.click", "Click an element or a point; {id | x, y, button?, double?, ctrl?, shift?, alt?}"),
    ("ui.drag", "Drag between elements or points; {from: {id | x, y}, to: {id | x, y}, button?, steps?}"),
    ("ui.scroll", "Mouse wheel over an element or point; {id | x, y, dy}"),
    ("ui.key", "Press a key (egui name: Delete, Tab, F, Z, Enter, ...); {key, ctrl?, shift?, alt?}"),
    ("ui.type", "Type text into the focused field; {text}"),
    ("ui.select", "Select nodes in the editor (the editor shows the component they sit in); {paths}"),
    ("ui.enter", "Show the inside of a component in the editor, the top level without a path; {path?}"),
    ("ui.view", "Pin the viewer to a node, or unpin without a path; {path?, output?: name or index}"),
    (
        "ui.camera",
        "The viewer's 3D view of a render or geometry: {mode?: picture|free|steer, home?, camera?: {eye, target, roll, fov, ortho?}, \
         orbit?: [yaw, pitch], pan?: [right, up], dolly?, travel?: [right, up, forward], look?: [yaw, pitch], roll?, take?}; \
         moves go to the free view's camera, in steer mode to the camera node (orthographic: dolly scales the view's size); take copies the free view into the camera node",
    ),
    (
        "ui.options",
        "The editor's options (right-click on empty space) and the viewer's channel options; {chan_bars?, viewer_background?, binding_lines?, \
         chan_scale?: auto|shared|per_channel (auto: a lane per channel when one that moves would be flat on a shared scale), \
         audio_view?: meters|plot (what the viewer shows of a chan.audio_analysis: meters, beat and spectrum, or the channel plot), \
         viewer_aspect?: auto|number (width over height of the viewer's picture area; the height follows the column's width; \
         dragging the viewer's edge sets the aspect, auto is 16:9)}",
    ),
    (
        "ui.arrange",
        "Arrange the nodes so wires cross little and run straight (one undo step): {nodes?: [path]}, else every level",
    ),
    ("ui.frame", "Fit all nodes into the editor"),
    (
        "ui.measure",
        "How tidy the level shown is, in graph units: width and height of the nodes, total and longest wire length, crossings, wire length on top of a wire from another output (overlap; where: overlaps), wire length through nodes",
    ),
    ("ui.screenshot", "PNG of the window; {file?} (default %TEMP%/penumbra/ui.png)"),
];

/// Registers the `ui.*` commands, all forwarded to the UI thread through `bridge`.
pub fn register(registry: &mut Registry, bridge: &Arc<Bridge>) {
    for (id, summary) in COMMANDS {
        let bridge = Arc::clone(bridge);
        registry.register(FnCommand::new(id, summary, move |p| bridge.call(id, p)));
    }
    let bridge = Arc::clone(bridge);
    registry.register(FnCommand::new(
        "help.open",
        "The documentation page (docs/penumbra.html) in the default browser, at the entry of a node kind,          a command or a guide; {kind?, open?}. Answers with its file and URL; open: false, and headless runs,          open nothing",
        move |p| crate::help::command(p, bridge.browser()).map_err(CommandError),
    ));
}
