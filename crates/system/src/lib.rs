//! The show machine around penumbra: which displays are connected and how to name them
//! stably ([`display`]), and the operating-system settings that can ruin a show, checked and
//! fixed on request ([`checks`], the commands `system.check` and `system.fix`). Also whether the
//! taskbar is light, for the app icon ([`appearance`]). How busy CPU and GPU are ([`load`]),
//! with NVIDIA GPUs' power, temperature and clock ([`nvml`]). Keeping the displays on while
//! output windows are open ([`awake`]).
//!
//! OS-specific code lives here and in the output crate only; the Windows calls are in one module
//! and NVIDIA's library in another, the crate's only `unsafe`. No graphics API, no UI.

pub mod appearance;
pub mod awake;
pub mod checks;
pub mod display;
pub mod load;
pub mod nvml;
pub mod process;
#[cfg(windows)]
mod windows;

pub use awake::KeepAwake;
pub use checks::{Check, CheckStatus, Previous};
pub use display::{DisplayDetails, Resolved, displays, resolve};
pub use load::{GpuSensors, Load, LoadMeter};
pub use process::ProcessStats;
