//! The engine, its commands and the control channel.
//!
//! Everything the app can do is a [`Command`] in a [`Registry`]; the UI, the CLI and the
//! control channel all call the same registry. [`Engine`] runs an open project: the cook thread
//! and the project thread (autosave, external edits). It uses the GPU (snapshots) but no UI.

pub mod audio;
pub mod blobtrack;
pub mod calib;
pub mod clipboard;
pub mod command;
pub mod commands;
pub mod components;
pub mod control;
pub mod engine;
pub mod navigation;
pub mod osc;
pub mod pictures;
pub mod projects;
pub mod state;
pub mod state_commands;
pub mod videoin;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use command::{Command, CommandError, FnCommand, Registry};
pub use engine::{CookState, Engine, OutputFrame, PassStats, ViewFrame, ViewRequest};

/// Version of this build, reported by `ping`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A registry with the commands every penumbra process offers.
pub fn builtin_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(command::Ping);
    registry
}

/// The built-in commands plus everything `engine` offers.
pub fn engine_registry(engine: &Arc<Engine>) -> Registry {
    let mut registry = builtin_registry();
    commands::register(&mut registry, engine);
    components::register(&mut registry, engine);
    clipboard::register(&mut registry, engine);
    calib::register(&mut registry);
    state_commands::register(&mut registry, engine);
    osc::register(&mut registry, engine);
    audio::register(&mut registry, engine);
    videoin::register(&mut registry, engine);
    blobtrack::register(&mut registry, engine);
    projects::register(&mut registry, engine);
    navigation::register(&mut registry, engine);
    registry
}
