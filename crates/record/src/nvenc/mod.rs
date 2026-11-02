//! H.264 on an NVIDIA graphics card's video encoder (NVENC), for recordings the CPU encoder
//! cannot keep up with.
//!
//! The encoder runs on the card's own encoding engine through the driver's `nvEncodeAPI64.dll`
//! (API 12.1), loaded at run time on a Direct3D 11 device of its own. Nothing is linked or
//! shipped: on a machine without an NVIDIA card or driver, [`Nvenc::open`] says why and the
//! recording takes the CPU encoder. The Y'CbCr 4:2:0 planes a recording gets go into NV12 input
//! buffers (a ring of [`RING`]); the Annex B output comes back as length-prefixed samples, the
//! parameter sets split out for the `avcC`.
//!
//! Windows only; elsewhere [`Nvenc::open`] always fails. `unsafe` is confined to `ffi` (data),
//! `device` and `session` (every driver call); this module is safe code. Taken from FilmCraft
//! (github.com/storytold/filmcraft, MIT OR Apache-2.0), trimmed to what a recording needs.

// Elsewhere only the stubs of `Nvenc` are reached; the shared helpers stay tested.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
#[allow(unsafe_code)]
mod device;
#[cfg(windows)]
#[allow(unsafe_code)]
mod ffi;
#[cfg(windows)]
#[allow(unsafe_code)]
mod session;

#[cfg(all(test, windows))]
mod abi_tests;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;

#[cfg(windows)]
use session::{Locked, Session};

/// Pictures in flight: input / output buffer pairs.
const RING: usize = 8;

/// How hard NVENC works and how good the picture stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quality {
    /// 4 to 6: NVENC's presets P4 to P6, each slower and a little better.
    pub preset: u8,
    /// Constant quality, 1 (best) to 51.
    pub cq: u8,
}

/// What to encode.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Even.
    pub width: u32,
    pub height: u32,
    /// Frame rate as numerator / denominator.
    pub fps: (u32, u32),
    /// Frames between IDR pictures.
    pub keyint: u32,
    pub quality: Quality,
}

/// One encoded picture; pictures come out in the order they went in.
pub struct Packet {
    /// Length-prefixed (4 bytes) NAL units, without parameter sets or access unit delimiters.
    pub data: Vec<u8>,
    pub key: bool,
    /// The index the picture went in with.
    pub pts: i64,
}

/// An NVENC H.264 encoder.
pub struct Nvenc {
    #[cfg(windows)]
    session: Session,
    sps: Vec<u8>,
    pps: Vec<u8>,
    free: Vec<usize>,
    /// Slots submitted and not read yet, in submission order, and how many of them are ready.
    pending: VecDeque<usize>,
    ready: usize,
    size: (u32, u32),
}

impl Nvenc {
    /// Opens an encoder, or says why NVENC cannot take this configuration (no NVIDIA card or
    /// driver, a size outside the card's limits, every encoder session of the card in use).
    #[cfg(windows)]
    pub fn open(cfg: &Config) -> Result<Self, String> {
        let (w, h) = (cfg.width, cfg.height);
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
            return Err(format!("{w}x{h}: NVENC needs even sides"));
        }
        if cfg.fps.0 == 0 || cfg.fps.1 == 0 {
            return Err("the frame rate is 0".into());
        }
        let mut session = Session::open()?;
        let caps = session.caps()?;
        if w < caps.min_size.0 || h < caps.min_size.1 || w > caps.max_size.0 || h > caps.max_size.1 {
            return Err(format!(
                "{w}x{h} is outside NVENC's {}x{} to {}x{}",
                caps.min_size.0, caps.min_size.1, caps.max_size.0, caps.max_size.1
            ));
        }
        let preset = match cfg.quality.preset {
            0..=4 => session::Preset::P4,
            5 => session::Preset::P5,
            _ => session::Preset::P6,
        };
        let params = session::Params {
            width: w,
            height: h,
            fps: cfg.fps,
            gop: cfg.keyint.max(1),
            preset,
            quality: cfg.quality.cq,
        };
        session.initialize(&params, RING)?;
        let (sps, pps) = split_parameter_sets(&session.sequence_params()?)?;
        let slots = session.slots();
        Ok(Self {
            session,
            sps,
            pps,
            free: (0..slots).rev().collect(),
            pending: VecDeque::new(),
            ready: 0,
            size: (w, h),
        })
    }

    #[cfg(not(windows))]
    pub fn open(_cfg: &Config) -> Result<Self, String> {
        Err("the GPU encoder (NVENC) is built for Windows only".into())
    }

    /// The sequence and picture parameter sets (NAL units without start codes).
    pub fn parameter_sets(&self) -> (&[u8], &[u8]) {
        (&self.sps, &self.pps)
    }

    /// The `avcC` record of the stream (ISO/IEC 14496-15), one SPS and one PPS.
    pub fn avcc(&self) -> Result<Vec<u8>, String> {
        avcc(&self.sps, &self.pps)
    }

    /// Encodes picture `index` from planar 8-bit 4:2:0 in one slice: Y' rows `luma_stride`
    /// bytes apart from `offsets[0]`, Cb and Cr rows `chroma_stride` apart from `offsets[1]`
    /// and `offsets[2]`. Returns the pictures that came out (none while the ring fills).
    pub fn encode(
        &mut self,
        planes: &[u8],
        offsets: [usize; 3],
        strides: (usize, usize),
        index: i64,
    ) -> Result<Vec<Packet>, String> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let src = Source::new(planes, offsets, strides, w, h)?;
        let mut out = Vec::new();
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                // The ring is full: the oldest picture must come out first.
                out.push(self.read_oldest()?);
                self.free.pop().ok_or("no free encoder buffer")?
            }
        };
        let pts = u64::try_from(index).map_err(|_| "a negative frame index")?;
        match self.submit(slot, pts, &src) {
            Ok(ready) => {
                self.pending.push_back(slot);
                if ready {
                    self.ready = self.pending.len();
                }
            }
            Err(e) => {
                self.free.push(slot);
                return Err(e);
            }
        }
        Ok(out)
    }

    #[cfg(windows)]
    fn submit(&mut self, slot: usize, pts: u64, src: &Source<'_>) -> Result<bool, String> {
        self.session.submit(slot, pts, |l: Locked<'_>| src.fill_nv12(l.data, l.pitch))
    }

    #[cfg(not(windows))]
    fn submit(&mut self, _slot: usize, _pts: u64, _src: &Source<'_>) -> Result<bool, String> {
        Err("no GPU encoder".into())
    }

    /// Finishes the stream: every picture still inside comes out.
    pub fn flush(&mut self) -> Result<Vec<Packet>, String> {
        if !self.pending.is_empty() {
            self.end_of_stream()?;
            self.ready = self.pending.len();
        }
        let mut out = Vec::with_capacity(self.pending.len());
        while !self.pending.is_empty() {
            out.push(self.read_oldest()?);
        }
        Ok(out)
    }

    #[cfg(windows)]
    fn end_of_stream(&mut self) -> Result<(), String> {
        self.session.end_of_stream()
    }

    #[cfg(not(windows))]
    fn end_of_stream(&mut self) -> Result<(), String> {
        Ok(())
    }

    #[cfg(windows)]
    fn read_oldest(&mut self) -> Result<Packet, String> {
        if self.ready == 0 {
            return Err("the encoder holds back more pictures than it has buffers for".into());
        }
        let slot = self.pending.pop_front().ok_or("no picture in flight")?;
        self.ready -= 1;
        let r = self.session.read(slot);
        self.free.push(slot);
        let o = r?;
        let data = annex_b_to_length_prefixed(&o.data)?;
        let pts = i64::try_from(o.pts).unwrap_or(i64::MAX);
        Ok(Packet { data, key: o.pic_type == ffi::NV_ENC_PIC_TYPE_IDR, pts })
    }

    #[cfg(not(windows))]
    fn read_oldest(&mut self) -> Result<Packet, String> {
        Err("no GPU encoder".into())
    }
}

/// The planes of one picture, checked against its size.
struct Source<'a> {
    planes: &'a [u8],
    offsets: [usize; 3],
    strides: (usize, usize),
    w: usize,
    h: usize,
}

impl<'a> Source<'a> {
    fn new(
        planes: &'a [u8],
        offsets: [usize; 3],
        strides: (usize, usize),
        w: usize,
        h: usize,
    ) -> Result<Self, String> {
        let (ls, cs) = strides;
        let (cw, ch) = (w / 2, h / 2);
        let end = |off: usize, stride: usize, width: usize, rows: usize| {
            rows.checked_sub(1)
                .map_or(Some(off), |r| off.checked_add(r.checked_mul(stride)?)?.checked_add(width))
        };
        let fits = ls >= w
            && cs >= cw
            && [end(offsets[0], ls, w, h), end(offsets[1], cs, cw, ch), end(offsets[2], cs, cw, ch)]
                .into_iter()
                .all(|e| e.is_some_and(|e| e <= planes.len()));
        if !fits {
            return Err("the frame is too short".into());
        }
        Ok(Source { planes, offsets, strides, w, h })
    }

    fn row(&self, plane: usize, row: usize) -> &[u8] {
        let (stride, width) =
            if plane == 0 { (self.strides.0, self.w) } else { (self.strides.1, self.w / 2) };
        let start = self.offsets.get(plane).map_or(0, |o| o + row * stride);
        self.planes.get(start..start + width).unwrap_or(&[])
    }

    /// Copies the picture into an NV12 buffer of `pitch` bytes per row: luma rows, then rows of
    /// interleaved Cb, Cr after `pitch * h` bytes. A row the buffer cannot hold is skipped,
    /// never overrun.
    fn fill_nv12(&self, data: &mut [u8], pitch: usize) {
        let (w, h) = (self.w, self.h);
        if w == 0 || pitch < w {
            return;
        }
        let (luma, chroma) = data.split_at_mut(pitch.saturating_mul(h).min(data.len()));
        for (r, dst) in luma.chunks_mut(pitch).take(h).enumerate() {
            let src = self.row(0, r);
            if let Some(d) = dst.get_mut(..src.len()) {
                d.copy_from_slice(src);
            }
        }
        for (r, dst) in chroma.chunks_mut(pitch).take(h / 2).enumerate() {
            let (u, v) = (self.row(1, r), self.row(2, r));
            let Some(dst) = dst.get_mut(..w) else { continue };
            for (([du, dv], a), b) in dst.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
                *du = *a;
                *dv = *b;
            }
        }
    }
}

/// Whether an NVENC session opens on this machine: an NVIDIA card with a recent enough driver.
#[cfg(windows)]
pub fn available() -> Result<(), String> {
    Session::open().map(|_| ())
}

#[cfg(not(windows))]
pub fn available() -> Result<(), String> {
    Err("the GPU encoder (NVENC) is built for Windows only".into())
}

/// The NAL units of an Annex B byte stream (start codes `00 00 01` / `00 00 00 01`).
fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while let Some(w) = data.get(i..i.saturating_add(3)) {
        if w == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n.saturating_sub(3));
        // Trailing zero bytes belong to the next start code (4-byte form) or are padding.
        while e > s && data.get(e - 1) == Some(&0) {
            e -= 1;
        }
        if let Some(n) = data.get(s..e).filter(|n| !n.is_empty()) {
            out.push(n);
        }
    }
    out
}

/// Annex B to 4-byte length-prefixed NAL units, dropping parameter sets (they live in the
/// `avcC`) and access unit delimiters (types 7 to 9). Errors on a stream without picture data.
fn annex_b_to_length_prefixed(data: &[u8]) -> Result<Vec<u8>, String> {
    let nals = annex_b_nals(data);
    if nals.is_empty() {
        return Err("the encoder produced no NAL units".into());
    }
    let mut out = Vec::with_capacity(data.len());
    for n in nals {
        if n.first().is_none_or(|b| matches!(b & 0x1f, 7..=9)) {
            continue;
        }
        let len = u32::try_from(n.len()).map_err(|_| "a NAL unit longer than 4 GiB".to_string())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(n);
    }
    if out.is_empty() {
        return Err("the encoder produced no picture data".into());
    }
    Ok(out)
}

/// The SPS and PPS NAL units of an Annex B byte string.
fn split_parameter_sets(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let nals = annex_b_nals(data);
    let find = |t: u8| nals.iter().find(|n| n.first().is_some_and(|b| b & 0x1f == t)).map(|n| n.to_vec());
    match (find(7), find(8)) {
        (Some(s), Some(p)) => Ok((s, p)),
        _ => Err("the encoder returned no SPS / PPS".into()),
    }
}

/// An `AVCDecoderConfigurationRecord` for one SPS and one PPS. High profile and up carry the
/// chroma format and bit depths (4:2:0, 8 bits) after the PPS.
fn avcc(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, String> {
    let (&[_, profile, compat, level], true) =
        (sps.first_chunk::<4>().ok_or("a short SPS")?, !pps.is_empty())
    else {
        return Err("an empty PPS".into());
    };
    let sps_len = u16::try_from(sps.len()).map_err(|_| "an SPS longer than 64 KiB")?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| "a PPS longer than 64 KiB")?;
    let mut out = vec![1, profile, compat, level, 0xff, 0xe1];
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    if matches!(profile, 100 | 110 | 122 | 244) {
        // chroma_format_idc 1, bit depths 8, no SPS extensions.
        out.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Ok(out)
}
