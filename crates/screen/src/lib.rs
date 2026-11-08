//! Screen and window capture: a monitor or a window of this machine as a wgpu texture, through
//! Windows.Graphics.Capture. Windows only; elsewhere every capture fails with a message and
//! nothing is listed.
//!
//! The captured frames are Direct3D 11 textures. They reach wgpu through a shared texture and
//! Direct3D 11 on 12 on wgpu's own DirectX 12 device and queue (`wgc.rs`): two GPU copies and
//! one conversion pass per frame, and the picture never leaves the graphics card.
//!
//! - [`Capture`]: one per node. A thread of its own finds the monitor or window, starts the
//!   capture, notices when the window closes or the monitor goes, and tries again with a
//!   growing pause. The cook takes the newest frame with [`Capture::take`].
//! - [`sources`]: the monitors and windows that can be captured now, for menus and
//!   `screen.sources`.
//! - [`pick`]: the pure rules that choose a monitor or a window, tested everywhere.

pub mod pick;

#[cfg(not(windows))]
#[path = "none.rs"]
mod none;
#[cfg(windows)]
mod wgc;

#[cfg(not(windows))]
pub use none::{Capture, sources};
#[cfg(windows)]
pub use wgc::{Capture, sources};

/// Whether screen capture works on this platform at all (a Windows machine may still refuse
/// it: older than Windows 10 1903, or a session without a desktop).
pub const SUPPORTED: bool = cfg!(windows);

/// What to capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A monitor: empty for the primary one, a number from 1 for the n-th of [`sources`], or a
    /// display id or name as `out.projector` takes it.
    Monitor(String),
    /// The first window whose title contains this text, ignoring case.
    Window(String),
}

/// How to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Draw the mouse pointer into the picture.
    pub cursor: bool,
    /// Let Windows draw its coloured frame around what is captured (Windows 11 can leave it out).
    pub border: bool,
}

/// How a capture is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// What is captured now: the monitor's id or the window's title.
    pub capturing: Option<String>,
    /// Why nothing is captured now; the capture tries again by itself.
    pub error: Option<String>,
    /// The size of the last frame.
    pub size: Option<(u32, u32)>,
}

/// A monitor that can be captured.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MonitorListing {
    /// What `display` takes for this monitor as a number.
    pub index: usize,
    /// The stable display id `out.projector` uses; the OS name when the display is not known.
    pub id: String,
    /// The OS name (`\\.\DISPLAY2`).
    pub os_name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// A window that can be captured.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WindowListing {
    pub title: String,
    /// The program's file name, such as `notepad.exe`; empty when Windows does not say.
    pub program: String,
    pub width: u32,
    pub height: u32,
}

/// What can be captured now.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sources {
    pub monitors: Vec<MonitorListing>,
    /// Top-level windows with a title, front to back.
    pub windows: Vec<WindowListing>,
    /// Why capture does not work here, if it does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
#[cfg(windows)]
mod tests;
