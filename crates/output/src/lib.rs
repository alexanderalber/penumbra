//! Projector outputs: one borderless fullscreen window per display that has an active
//! `out.projector` node, assigned by a stable display id, presented from its own thread on the
//! shared wgpu device. The vsync of one window paces the show clock.
//!
//! The app owns the event loop and hands this crate the events of its windows; see
//! [`Outputs`]. Nothing here is used by headless runs.

pub mod manager;
pub mod window;

pub use manager::Outputs;
