//! The engine, its commands and the control channel.
//!
//! Everything the app can do is a [`Command`] in a [`Registry`]; the UI, the CLI and the
//! control channel all call the same registry. [`Engine`] runs an open project: the cook thread
//! and the project thread (autosave, external edits). It uses the GPU (snapshots) but no UI.

pub mod audio;
pub mod blob_track;
pub mod calib;
pub mod clipboard;
pub mod command;
pub mod commands;
pub mod components;
pub mod control;
pub mod crash;
pub mod dmx;
pub mod engine;
pub mod file_out;
pub mod flatfield;
pub mod hubs;
pub mod jobs;
pub mod keyboard;
pub mod mesh_out;
pub mod meter;
pub mod mouse;
pub mod navigation;
pub mod ndi;
pub mod osc;
pub mod palettes;
pub mod pictures;
pub mod projector;
pub mod projector_stored;
pub mod projects;
pub mod scan;
pub mod scan_align;
pub mod scan_capture;
pub mod scan_faces;
pub mod scan_regions;
pub mod scan_relief;
pub mod scan_run;
pub mod scan_scene;
pub mod scan_synthetic;
pub mod shapes;
pub mod show;
pub mod spout;
pub mod state;
pub mod state_commands;
pub mod table_out;
pub mod ticker;
pub mod video_file;
pub mod video_in;
pub mod video_out;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use command::{Command, CommandError, FnCommand, Registry};
pub use engine::{CookState, Engine, OutputFrame, PassStats, ResetReport, ViewFrame, ViewRequest};
pub use penumbra_audio::keep_off_devices;

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
    keyboard::register(&mut registry, engine);
    mouse::register(&mut registry, engine);
    palettes::register(&mut registry, engine);
    audio::register(&mut registry, engine);
    dmx::register(&mut registry, engine);
    ndi::register(&mut registry);
    spout::register(&mut registry);
    video_file::register(&mut registry, engine);
    video_in::register(&mut registry, engine);
    video_out::register(&mut registry, engine);
    table_out::register(&mut registry, engine);
    mesh_out::register(&mut registry, engine);
    blob_track::register(&mut registry, engine);
    shapes::register(&mut registry, engine);
    scan::register(&mut registry, engine);
    scan_capture::register(&mut registry, engine);
    scan_align::register(&mut registry, engine);
    scan_relief::register(&mut registry, engine);
    scan_scene::register(&mut registry, engine);
    scan_regions::register(&mut registry, engine);
    scan_faces::register(&mut registry, engine);
    scan_synthetic::register(&mut registry, engine);
    scan_run::register(&mut registry, engine);
    projector::register(&mut registry, engine);
    flatfield::register(&mut registry, engine);
    jobs::register(&mut registry, engine);
    projects::register(&mut registry, engine);
    navigation::register(&mut registry, engine);
    show::register(&mut registry, engine);
    registry
}
