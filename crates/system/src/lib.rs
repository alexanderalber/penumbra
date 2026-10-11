//! The show machine around penumbra: which displays are connected and how to name them
//! stably ([`display`]), and the operating-system settings that can ruin a show, checked and
//! fixed on request ([`checks`], the commands `system.check` and `system.fix`).
//!
//! OS-specific code lives here and in the output crate only; the Windows calls are in one module,
//! the crate's only `unsafe`. No GPU, no UI.

pub mod checks;
pub mod display;
pub mod process;
#[cfg(windows)]
mod windows;

pub use checks::{Check, CheckStatus, Previous};
pub use display::{DisplayDetails, Resolved, displays, resolve};
pub use process::ProcessStats;
