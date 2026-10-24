//! Video files for `tex.video_file`.
//!
//! - [`open`] reads a clip: FilmCraft's pure-Rust decoders for movies (H.264, HEVC, ProRes,
//!   DNxHR, VP9, AV1, MPEG-2 in MP4, MOV, MKV, MXF, TS) and the `gif` crate plus our own
//!   compositing for animated GIFs, our own reader for HAP (block-compressed textures the GPU
//!   samples directly). A [`Decoder`] is sequential: asking for the next frame is cheap, any
//!   other frame seeks.
//! - [`Player`] decodes on two threads of its own, each with a bounded frame queue, and hands the
//!   cook thread the frame due at a clip time without ever waiting. While one thread plays an
//!   iteration of a looping clip, the other has already decoded the start of the next one, so the
//!   jump from the last frame to the first costs nothing (a seek back to the start takes 4 to 17
//!   frame periods otherwise).
//!
//! Frames stay as the decoder made them (Y'CbCr planes, RGBA8 or BC blocks); the GPU converts
//! them (`penumbra_gpu::video`, `penumbra_gpu::bc`). No GPU and no UI here.

mod film;
mod gif;
mod hap;
mod player;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

pub use filmcraft_color::{ColorInfo, Matrix, Range, Transfer};
pub use filmcraft_frame::{Chroma, PixelData, VideoFrame};
pub use hap::{BcFormat, BcTexture, HapColor, HapFrame};
pub use player::{OnFrame, Player, PlayerStats, Shown};

/// A decoded frame, shared between the queue and whoever shows it.
#[derive(Clone, Debug)]
pub enum Frame {
    /// Pixels (Y'CbCr planes or RGBA8), from FilmCraft or the GIF reader.
    Pixels(Arc<VideoFrame>),
    /// Block-compressed textures (HAP).
    Hap(Arc<HapFrame>),
}

impl Frame {
    pub fn width(&self) -> u32 {
        match self {
            Self::Pixels(f) => f.width,
            Self::Hap(f) => f.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Pixels(f) => f.height,
            Self::Hap(f) => f.height,
        }
    }

    /// Memory the frame holds.
    pub fn byte_size(&self) -> usize {
        match self {
            Self::Pixels(f) => f.byte_size(),
            Self::Hap(f) => f.byte_size(),
        }
    }

    pub fn pixels(&self) -> Option<&VideoFrame> {
        match self {
            Self::Pixels(f) => Some(f),
            Self::Hap(_) => None,
        }
    }
}

/// Extensions the file picker offers; opening goes by content, not by name.
pub const EXTENSIONS: &[&str] =
    &["mp4", "m4v", "mov", "mkv", "webm", "mxf", "ts", "m2ts", "mts", "mpg", "mpeg", "m2v", "gif"];

/// What a file may ask for before anything is decoded.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest width or height (the GPU's texture limit).
    pub max_side: u32,
    /// Largest frame count.
    pub max_frames: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_side: 8192, max_frames: 10_000_000 }
    }
}

/// When frames start.
#[derive(Clone, Debug, PartialEq)]
pub enum Timing {
    /// A constant rate, `num / den` frames per second.
    Rate { num: i64, den: i64 },
    /// Per-frame end times in seconds, increasing (animated GIFs).
    Ends(Arc<[f64]>),
}

/// A clip as the file describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipInfo {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub timing: Timing,
    /// `H.264`, `GIF`, ...
    pub codec: String,
    pub container: String,
    /// Whether jumping ahead is cheaper than decoding every frame up to the target (keyframes).
    /// Not for GIFs, which can only be read from the start.
    pub seeks: bool,
}

impl ClipInfo {
    /// Length in seconds.
    pub fn duration(&self) -> f64 {
        match &self.timing {
            Timing::Rate { num, den } => self.frames as f64 * *den as f64 / *num as f64,
            Timing::Ends(ends) => ends.last().copied().unwrap_or(0.0),
        }
    }

    /// Frames per second (the mean for GIFs).
    pub fn rate(&self) -> f64 {
        match &self.timing {
            Timing::Rate { num, den } => *num as f64 / *den as f64,
            Timing::Ends(_) => {
                let d = self.duration();
                if d > 0.0 { self.frames as f64 / d } else { 0.0 }
            }
        }
    }

    /// The frame shown at `t` seconds into the clip, clamped to the clip.
    pub fn index_at(&self, t: f64) -> usize {
        let last = self.frames.saturating_sub(1);
        // NaN lands here too.
        if t.is_nan() || t <= 0.0 {
            return 0;
        }
        let i = match &self.timing {
            // A hair of tolerance, so a time computed as exactly frame k does not land on k - 1.
            Timing::Rate { num, den } => (t * *num as f64 / *den as f64 + 1e-6).floor() as usize,
            Timing::Ends(ends) => ends.partition_point(|&e| e <= t + 1e-9),
        };
        i.min(last)
    }

    /// Time at which frame `index` starts.
    pub fn start_of(&self, index: usize) -> f64 {
        match &self.timing {
            Timing::Rate { num, den } => index as f64 * *den as f64 / *num as f64,
            Timing::Ends(ends) => match index.checked_sub(1) {
                None => 0.0,
                Some(i) => ends.get(i).copied().unwrap_or_else(|| self.duration()),
            },
        }
    }

    fn check(&self, limits: &Limits) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 || w > limits.max_side || h > limits.max_side {
            return Err(format!("{w}x{h} is outside 1 to {} pixels per side", limits.max_side));
        }
        if self.frames == 0 {
            return Err("the clip has no frames".into());
        }
        if self.frames > limits.max_frames {
            return Err(format!("{} frames, more than {}", self.frames, limits.max_frames));
        }
        if !(self.duration() > 0.0 && self.duration().is_finite()) {
            return Err("the clip has no duration".into());
        }
        Ok(())
    }
}

/// One sequential reader of a clip. Every thread of a [`Player`] opens its own and keeps it, so
/// a decoder need not be `Send`.
pub trait Decoder {
    fn info(&self) -> &ClipInfo;
    /// Frame `index`. The next one after the last asked for is cheap; anything else seeks.
    fn frame(&mut self, index: usize) -> Result<Frame, String>;
}

/// Opens a clip. Animated GIFs go by their signature, movies whose video track is HAP to our
/// own reader, everything else to FilmCraft. Foreign parsers run inside `catch_unwind`: a panic
/// is an error, not a crash.
pub fn open(path: &Path, limits: &Limits) -> Result<Box<dyn Decoder>, String> {
    let decoder = guarded(|| {
        if gif::sniff(path)? {
            return gif::GifDecoder::open(path, limits).map(|d| Box::new(d) as Box<dyn Decoder>);
        }
        if let Some(d) = hap::HapDecoder::open(path)? {
            return Ok(Box::new(d) as Box<dyn Decoder>);
        }
        film::FilmDecoder::open(path).map(|d| Box::new(d) as Box<dyn Decoder>)
    })?;
    decoder.info().check(limits)?;
    Ok(decoder)
}

/// Runs foreign decoding code, turning a panic into an error.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            Err(format!("the decoder panicked: {msg}"))
        }
    }
}

#[cfg(test)]
mod tests;
