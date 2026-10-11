//! Camera input for `tex.videoin`.
//!
//! - [`hub`]: the [`Hub`] opens the cameras the network wants, one capture thread each, and
//!   keeps the newest frame of each for whoever shows it.
//! - [`mode`]: devices, their modes, and picking a mode from a text like `1920x1080@60`.
//! - [`frame`]: frames rearranged from the camera's packing into the planes the GPU converts.
//!
//! Backends: Media Foundation on Windows; other systems list no cameras yet (V4L2 is planned).
//! No GPU and no UI here.

pub mod frame;
pub mod hub;
pub mod mode;

#[cfg(windows)]
#[path = "mf.rs"]
mod backend;
#[cfg(not(windows))]
#[path = "none.rs"]
mod backend;

pub use frame::{CameraFrame, Matrix, PixelFormat};
pub use hub::{Config, Hub, Key, StreamStatus, Watch, devices};
pub use mode::{DeviceInfo, Mode, ModeInfo, Wanted};
