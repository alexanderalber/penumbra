//! The penumbra editor UI (egui): node editor with live thumbnails, param panel, viewer,
//! change log, editing-mode colours, automation ids and the `ui.*` commands, plus a headless
//! runner that drives the same app from a script and saves screenshots.
//!
//! Layering: this is the only crate besides the apps that may use egui/eframe.

pub mod about;
pub mod add_dialog;
pub mod app;
pub mod arrange;
pub mod automation;
mod blend_match;
pub mod changes;
mod clipboard;
mod components;
pub mod control;
mod cook_meter;
pub mod editor;
pub mod headless;
pub mod help;
pub mod helpers;
pub mod icons;
mod keyboard;
pub mod layout;
mod mapping;
pub mod memory;
pub mod meters;
mod model_map;
pub mod navigate;
mod node_controls;
mod overview;
pub mod params;
pub mod plot;
mod pointer;
pub mod projects;
mod regions;
mod scope;
mod shapes;
mod show_screen;
pub mod textures;
pub mod theme;
pub mod titlebar;
pub mod viewer;
pub mod window_place;
mod wires;

use std::collections::BTreeSet;

use egui::Pos2;
use penumbra_core::{GroupId, NodeId};
use penumbra_graph::Op;

pub use app::{AppOptions, PenumbraApp};
pub use control::{Bridge, UiRequest};

/// What a panel asks the app to do; applied after drawing, in order.
pub enum Action {
    /// Edit the network as the user. `group` joins the edits of one drag into one undo step; a
    /// created node becomes the selection.
    Edit {
        ops: Vec<Op>,
        label: String,
        group: Option<GroupId>,
    },
    /// Set param values outside the history: what only shows the state of a tool, which undo must
    /// neither step through nor bring back.
    SetLive(Vec<Op>),
    Undo,
    Redo,
    TakeOver,
    SelectNodes(BTreeSet<NodeId>),
    /// Pin the viewer to a node, or follow the selection with `None`.
    Pin(Option<NodeId>),
    Play(bool),
    /// Show mode on or off (`show.lock`, `show.unlock`).
    Show(bool),

    /// Open the Helpers window.
    OpenHelpers,
    /// Open the Model mapping window on this mapped camera.
    OpenModelMap(NodeId),
    /// Show a message in the status bar.
    Message(String),
    /// An `audio.file` jumps to `seconds` into its file (`audio.seek`).
    SeekAudio {
        node: NodeId,
        seconds: f64,
    },
    /// Group these nodes (side by side) into a new component beside them.
    Group(Vec<NodeId>),
    /// Dissolve a component: its nodes move up beside it.
    Dissolve(NodeId),
    /// Write a component to the library.
    SaveComponent(NodeId),
    /// Take the component file `name` out of the library (the one the add dialog offers).
    RemoveComponent(String),
    /// Add the component file `name` from the library inside `level`, at `at` (graph units).
    AddComponent {
        name: String,
        level: Option<NodeId>,
        at: Pos2,
    },
    /// Copy these nodes (side by side): to the system clipboard as text, and to the engine.
    Copy(Vec<NodeId>),
    /// Copy these nodes, then delete them.
    Cut(Vec<NodeId>),
    /// Paste copied nodes inside `level` around `at` (graph units): `text` from the system
    /// clipboard, or what was copied last in this run.
    Paste {
        text: Option<String>,
        level: Option<NodeId>,
        at: Pos2,
    },
    /// Bind this param (`/n.scale`, a vector as a whole) to the next OSC address that arrives
    /// (`osc.learn`).
    LearnOsc(String),
    /// Replace this node with a new one of `kind` under the same name and at the same place,
    /// keeping the params, wires and bindings that fit (`node.replace`).
    Replace {
        node: NodeId,
        kind: String,
    },
    /// Start these nodes over (`node.reset`): stateful ones forget what they hold, a component
    /// stands for the stateful nodes inside it.
    Reset(Vec<NodeId>),
    /// A `tex.blob_track` takes the first frame after `delay` seconds as its background.
    LearnBackground {
        node: NodeId,
        delay: f64,
    },
    /// A `tex.video_file` starts its clip over (`video_file.restart`).
    RestartVideo(NodeId),
    /// A momentary `chan.button` goes down or up, held by the editor (`chan.press`); live
    /// state, no edit.
    PressButton {
        node: NodeId,
        down: bool,
    },
    /// An `audio.instrument` plays one note on lane 0 at its next frame, an `audio.file` starts
    /// over (`audio.play`).
    PlayInstrument(NodeId),
    /// The outlines of `from`'s picture (a path, or a name beside `node`) become `poly` lines
    /// in the `tex.shapes` `node` (`shapes.trace`).
    Trace {
        node: NodeId,
        from: String,
    },
    /// Scan the projectors of this `tex.scan` and of every other naming the same camera
    /// (`scan.run`).
    Scan(NodeId),
    /// Stop the scan going on (`scan.cancel`).
    CancelScan,
    /// Make the template of face `face` of the scene in folder `scene` (`scan.face_template`),
    /// off the UI thread.
    FaceTemplate {
        scene: String,
        face: String,
    },
    /// The documentation at this node kind's entry, in the browser (`help.open`).
    Help(String),
}

impl Action {
    /// A one-off edit: its own undo step.
    pub fn edit(ops: Vec<Op>) -> Self {
        Action::Edit { ops, label: String::new(), group: None }
    }
}

#[cfg(test)]
mod node_controls_tests;
#[cfg(test)]
mod note_tests;
#[cfg(test)]
mod record_tests;
#[cfg(test)]
mod reference;
#[cfg(test)]
mod show_screen_tests;
#[cfg(test)]
mod tests;
