//! Audio input, its analysis, and audio output.
//!
//! - [`capture`]: the [`Hub`] opens the input devices the network wants (cpal: WASAPI on
//!   Windows, ALSA on Linux), keeps their samples in a ring per device and hands readers what
//!   arrived since they last looked.
//! - [`analysis`]: an [`Analyzer`] turns blocks of samples into levels, bands, a spectrum,
//!   onsets, a tempo, and beat and bar phase.
//! - [`fft`]: the transform the analysis uses.
//! - [`spectrum`]: a [`Spectrum`] of the newest samples in bands, and the band code the
//!   analysis shares.
//! - [`envelope`]: an [`Envelope`] follows a signal's level with an attack and a release.
//! - [`playback`]: a [`Player`] per output device takes one block per show frame and plays it
//!   gaplessly: resampled to the device's rate, its latency held against clock drift, faded at
//!   gaps and drops.
//! - [`outputs`]: the [`Outputs`] hub, one bus per output device: sums what the `out.audio`
//!   nodes on it played in a frame and pushes it to the device's player.
//! - [`limiter`]: the look-ahead peak limiter every output ends in.
//! - [`reverb`]: the room [`Reverb`] of every output, a feedback delay network.
//! - [`measure`]: measures the delay from a push to the sound arriving back at an input.

pub mod analysis;
pub mod capture;
pub mod envelope;
pub mod fft;
pub mod limiter;
pub mod measure;
pub mod outputs;
pub mod playback;
pub mod reverb;
#[cfg(test)]
mod reverb_tests;
pub mod spectrum;

pub use analysis::{Analyzer, Features, Settings};
pub use capture::{Block, Cursor, DeviceInfo, Hub, StreamStatus, Watch};
pub use envelope::{Detect, Envelope};
pub use limiter::Limiter;
pub use outputs::{BusStatus, LaneStatus, Outputs, Tap, TapBlock, Want, keep_off_devices};
pub use playback::{Engine, EngineStatus, Player, PlayerStatus, output_devices};
pub use reverb::Reverb;
pub use spectrum::Spectrum;
