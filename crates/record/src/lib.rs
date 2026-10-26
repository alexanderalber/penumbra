//! Recording pictures to a movie file.
//!
//! - [`Codec`]: what a recording is encoded as (H.264 in MP4, ProRes 422, 422 HQ or 4444 in MOV)
//!   and the plane [`Layout`] its encoder takes.
//! - [`writer`]: one thread per recording that encodes the frames it is handed and writes them
//!   into the file; the queue in front of it is bounded, and a frame that does not fit is
//!   dropped (the caller counts it), so whoever records never waits for the encoder.
//! - [`hub`]: the recordings of a process: their status, and finishing every file when the app
//!   closes.
//! - [`looping`]: a movie made from a recording that a player can repeat without a visible
//!   jump: the best stretch cut out, a cross-fade of the end into the start, or forward then
//!   backward. It decodes the recording with `penumbra-video`.
//!
//! The pictures arrive as Y'CbCr planes already converted (on the GPU, in `penumbra-gpu`),
//! through the [`Planes`] trait, so this crate needs no GPU. No UI, no graph: the node type
//! lives in `penumbra-ops`, the commands in `penumbra-engine`.

pub mod hub;
pub mod looping;
pub mod writer;

#[cfg(test)]
mod tests;

use std::time::Duration;

use serde::Serialize;

pub use hub::{Hub, Status};
pub use writer::{Recording, Settings, Sink};

/// How hard the H.264 encoder works for each frame: the trade of speed against size and
/// quality. FilmCraft's encoder runs on the CPU; how many frames per second it manages depends
/// on the picture far more than on this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum H264Preset {
    /// Constrained Baseline (CAVLC entropy coding, no 8x8 transform), the encoder's fastest
    /// search, constant quality 23, no adaptive quantisation, no extra keyframes at cuts.
    Fast,
    /// High profile (CABAC), the encoder's fastest search, constant quality 18.
    #[default]
    Balanced,
    /// High profile, the encoder's balanced search (more partitions, a wider motion search),
    /// constant quality 18: about a third slower.
    Best,
}

impl H264Preset {
    /// The names a param uses, in [`H264Preset::ALL`]'s order.
    pub const NAMES: [&'static str; 3] = ["fast", "balanced", "best"];
    pub const ALL: [H264Preset; 3] = [H264Preset::Fast, H264Preset::Balanced, H264Preset::Best];

    pub fn parse(name: &str) -> Option<H264Preset> {
        Self::NAMES.iter().zip(Self::ALL).find(|(n, _)| **n == name).map(|(_, p)| p)
    }

    pub fn name(self) -> &'static str {
        match self {
            H264Preset::Fast => "fast",
            H264Preset::Balanced => "balanced",
            H264Preset::Best => "best",
        }
    }

    /// Sets profile, search, rate control and analysis in `cfg`.
    pub(crate) fn apply(self, cfg: &mut filmcraft_h264enc::EncoderConfig) {
        use filmcraft_h264enc as h;
        match self {
            H264Preset::Fast => {
                cfg.profile = h::Profile::Baseline;
                cfg.preset = h::Preset::Speed;
                cfg.rate = h::RateControl::Crf(23.0);
                cfg.aq_strength = 0.0;
                cfg.scenecut = false;
            }
            H264Preset::Balanced => {
                cfg.profile = h::Profile::High;
                cfg.preset = h::Preset::Speed;
                cfg.rate = h::RateControl::Crf(18.0);
            }
            H264Preset::Best => {
                cfg.profile = h::Profile::High;
                cfg.preset = h::Preset::Balanced;
                cfg.rate = h::RateControl::Crf(18.0);
            }
        }
    }
}

/// What a recording is encoded as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264 High profile, 4:2:0, 8 bits, constant quality, no B-frames; in MP4.
    H264,
    /// Apple ProRes 422, 10 bits; in MOV.
    Prores422,
    /// Apple ProRes 422 HQ, 10 bits; in MOV.
    Prores422Hq,
    /// Apple ProRes 4444, 12 bits, with alpha; in MOV.
    Prores4444,
}

impl Codec {
    /// The names a param or command uses, in [`Codec::ALL`]'s order.
    pub const NAMES: [&'static str; 4] = ["h264", "prores_422", "prores_422_hq", "prores_4444"];
    pub const ALL: [Codec; 4] = [Codec::H264, Codec::Prores422, Codec::Prores422Hq, Codec::Prores4444];

    pub fn parse(name: &str) -> Option<Codec> {
        Self::NAMES.iter().zip(Self::ALL).find(|(n, _)| **n == name).map(|(_, c)| c)
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Prores422 => "prores_422",
            Codec::Prores422Hq => "prores_422_hq",
            Codec::Prores4444 => "prores_4444",
        }
    }

    /// The file extension a recording gets when no file is named.
    pub fn extension(self) -> &'static str {
        match self {
            Codec::H264 => "mp4",
            _ => "mov",
        }
    }

    /// Whether the alpha channel is kept.
    pub fn has_alpha(self) -> bool {
        self == Codec::Prores4444
    }

    /// The planes the encoder takes for a `width` x `height` picture.
    pub fn layout(self, width: u32, height: u32) -> Layout {
        let (shift, bits) = match self {
            Codec::H264 => ((1, 1), 8),
            Codec::Prores422 | Codec::Prores422Hq => ((1, 0), 10),
            Codec::Prores4444 => ((0, 0), 12),
        };
        Layout { width, height, shift, bits, alpha: self.has_alpha() }
    }
}

/// Smallest width and height a recording may have.
pub const MIN_SIDE: u32 = 16;
/// Largest width and height (the H.264 encoder's and ProRes' limits are higher; this keeps a
/// frame's planes within one GPU storage buffer).
pub const MAX_SIDE: u32 = 8192;
/// Largest frame in bytes: wgpu's default limit for a storage buffer binding (128 MiB).
pub const MAX_FRAME_BYTES: usize = 128 << 20;

/// How the planes of one frame lie in memory, one after the other: Y', Cb, Cr and, with
/// `alpha`, A. Each row of a plane is padded to a multiple of 8 luma samples (4 chroma samples
/// when subsampled), so a GPU thread can write whole 32-bit words. Samples of more than 8 bits
/// take two bytes, little-endian; Y'CbCr is video range (16-235 at 8 bits, scaled up for more),
/// alpha full range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Layout {
    /// Even, at least [`MIN_SIDE`].
    pub width: u32,
    pub height: u32,
    /// Chroma subsampling as shifts (x, y): (1, 1) 4:2:0, (1, 0) 4:2:2, (0, 0) 4:4:4.
    pub shift: (u32, u32),
    /// Significant bits per sample: 8, 10 or 12.
    pub bits: u32,
    pub alpha: bool,
}

impl Layout {
    /// Bytes per sample.
    pub fn sample_bytes(&self) -> usize {
        if self.bits > 8 { 2 } else { 1 }
    }

    /// Samples per luma (and alpha) row, padding included.
    pub fn luma_stride(&self) -> u32 {
        self.width.div_ceil(8) * 8
    }

    /// Size of a chroma plane in samples (without padding).
    pub fn chroma_size(&self) -> (u32, u32) {
        (self.width >> self.shift.0, self.height >> self.shift.1)
    }

    /// Samples per chroma row, padding included.
    pub fn chroma_stride(&self) -> u32 {
        self.luma_stride() >> self.shift.0
    }

    /// Byte offsets of the Y', Cb, Cr and A planes (A equals the end without alpha).
    pub fn offsets(&self) -> [usize; 4] {
        let sb = self.sample_bytes();
        let luma = self.luma_stride() as usize * self.height as usize * sb;
        let chroma = self.chroma_stride() as usize * self.chroma_size().1 as usize * sb;
        [0, luma, luma + chroma, luma + 2 * chroma]
    }

    /// Bytes of one frame.
    pub fn bytes(&self) -> usize {
        let [_, _, _, a] = self.offsets();
        let luma = self.luma_stride() as usize * self.height as usize * self.sample_bytes();
        if self.alpha { a + luma } else { a }
    }

    /// Whether the encoders and the GPU conversion can take it: sides even and within
    /// [`MIN_SIDE`]..=[`MAX_SIDE`], at most [`MAX_FRAME_BYTES`].
    pub fn check(&self) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        if w % 2 == 1 || h % 2 == 1 {
            return Err(format!("{w}x{h}: width and height must be even"));
        }
        if !(MIN_SIDE..=MAX_SIDE).contains(&w) || !(MIN_SIDE..=MAX_SIDE).contains(&h) {
            return Err(format!("{w}x{h}: each side must be {MIN_SIDE} to {MAX_SIDE} pixels"));
        }
        if self.bytes() > MAX_FRAME_BYTES {
            return Err(format!("{w}x{h} is too large to record in this codec"));
        }
        Ok(())
    }
}

/// The planes of one frame as [`Layout`] describes them, possibly still on their way (a GPU
/// readback). Dropping it gives whatever holds the bytes back.
pub trait Planes: Send {
    /// Waits at most `timeout` for the bytes, then hands them to `f`, all planes in one slice.
    fn read(
        &mut self,
        timeout: Duration,
        f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String>;
}

/// Planes already in memory (tests, and anything converted on the CPU).
pub struct Bytes(pub Vec<u8>);

impl Planes for Bytes {
    fn read(
        &mut self,
        _timeout: Duration,
        f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        f(&self.0)
    }
}

/// One frame handed to a recording.
pub struct Frame {
    /// Position in the file, in frames of the recording's rate from its start. Frames come in
    /// increasing order; a gap means the frame before lasts longer (a frame dropped or not
    /// cooked), so the recording keeps time.
    pub index: i64,
    pub planes: Box<dyn Planes>,
}

/// A frame rate as a fraction, `num / den` frames per second (the container's timescale is
/// `num`, one frame lasts `den`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    /// `fps` as a fraction: whole rates exactly, others to a thousandth (29.97 is
    /// 29970/1000). `None` outside 1 to 1000 or not finite.
    pub fn from_f64(fps: f64) -> Option<Fps> {
        if !(1.0..=1000.0).contains(&fps) {
            return None;
        }
        if (fps - fps.round()).abs() < 1e-9 {
            // In range, so the cast keeps the value.
            return Some(Fps { num: fps.round() as u32, den: 1 });
        }
        Some(Fps { num: (fps * 1000.0).round() as u32, den: 1000 })
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den.max(1))
    }
}
