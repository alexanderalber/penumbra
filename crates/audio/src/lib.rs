//! Audio input and its analysis.
//!
//! - [`capture`]: the [`Hub`] opens the input devices the network wants (cpal: WASAPI on
//!   Windows, ALSA on Linux), keeps their samples in a ring per device and hands readers what
//!   arrived since they last looked.
//! - [`analysis`]: an [`Analyzer`] turns blocks of samples into levels, bands, a spectrum,
//!   onsets, a tempo, and beat and bar phase.
//! - [`fft`]: the transform the analysis uses.

pub mod analysis;
pub mod capture;
pub mod fft;

pub use analysis::{Analyzer, Features, Settings};
pub use capture::{Block, Cursor, DeviceInfo, Hub, StreamStatus, Watch};
