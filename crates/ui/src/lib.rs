//! The penumbra editor UI (egui): node editor with live thumbnails, param panel, viewer,
//! change log, editing-mode colours, automation ids and the `ui.*` commands, plus a headless
//! runner that drives the same app from a script and saves screenshots.
//!
//! Layering: this is the only crate besides the apps that may use egui/eframe.

pub mod add_dialog;
pub mod app;
pub mod arrange;
pub mod automation;
pub mod changes;
mod clipboard;
mod components;
pub mod control;
mod cook_meter;
pub mod editor;
pub mod headless;
pub mod helpers;
pub mod icons;
pub mod layout;
mod mapping;
pub mod meters;
pub mod navigate;
pub mod params;
pub mod plot;
pub mod projects;
mod shapes;
pub mod textures;
pub mod theme;
pub mod titlebar;
pub mod viewer;
mod wires;

use std::collections::BTreeSet;

use egui::Pos2;
use penumbra_core::{GroupId, NodeId};
use penumbra_graph::{Op, ThumbnailMode};

pub use app::{AppOptions, PenumbraApp};
pub use control::{Bridge, UiRequest};

/// What a panel asks the app to do; applied after drawing, in order.
pub enum Action {
    /// Edit the network as the user. `group` joins the edits of one drag into one undo step;
    /// `place` puts the node the batch creates at this editor position (without it the layout
    /// places it, right of its first input); a created node becomes the selection.
    Edit {
        ops: Vec<Op>,
        label: String,
        group: Option<GroupId>,
        place: Option<Pos2>,
    },
    Undo,
    Redo,
    TakeOver,
    /// The editor changed its selection itself (nothing to do; kept for the record).
    Selected,
    SelectNodes(BTreeSet<NodeId>),
    /// Pin the viewer to a node, or follow the selection with `None`.
    Pin(Option<NodeId>),
    Play(bool),
    Thumbnails(ThumbnailMode),
    /// Cook passes between two thumbnail cooks of a node.
    ThumbnailEvery(u32),
    /// Open the Helpers window.
    OpenHelpers,
    /// Show a message in the status bar.
    Message(String),
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
}

impl Action {
    /// A one-off edit: its own undo step.
    pub fn edit(ops: Vec<Op>) -> Self {
        Action::Edit { ops, label: String::new(), group: None, place: None }
    }
}

#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;
