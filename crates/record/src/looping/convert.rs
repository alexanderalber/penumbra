//! Decoded frames as the planes an encoder takes, small luma thumbnails of them, and blends of
//! two frames' planes.

use penumbra_video::{PixelData, Range, VideoFrame};

use crate::Layout;

/// Width and height of a thumbnail in samples.
pub const THUMB_W: usize = 32;
pub const THUMB_H: usize = 18;
/// Samples of one thumbnail.
pub const THUMB: usize = THUMB_W * THUMB_H;

/// `frame` as the planes `layout` describes, into `out` (resized to [`Layout::bytes`]). The
/// frame is cropped to the layout's size; chroma is resampled (nearest sample), bit depth and
/// range converted to video range. RGBA frames are converted with the BT.709 matrix.
pub fn to_planes(frame: &VideoFrame, layout: &Layout, out: &mut Vec<u8>) -> Result<(), String> {
    let (w, h) = (layout.width as usize, layout.height as usize);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    if fw < w || fh < h {
        return Err(format!("a frame of {fw}x{fh} is smaller than the file's {w}x{h}"));
    }
    out.clear();
    out.resize(layout.bytes(), 0);
    let [_, ou, ov, oa] = layout.offsets();
    let short = || "the planes are shorter than their layout".to_string();
    let (luma, rest) = out.split_at_mut_checked(ou).ok_or_else(short)?;
    let (cb, rest) = rest.split_at_mut_checked(ov - ou).ok_or_else(short)?;
    let (cr, alpha) = rest.split_at_mut_checked(oa - ov).ok_or_else(short)?;
    let sb = layout.sample_bytes();
    let tbits = layout.bits;
    let ls = layout.luma_stride() as usize;
    let cs = layout.chroma_stride() as usize;
    let (cw, ch) = layout.chroma_size();
    let (cw, ch) = (cw as usize, ch as usize);
    let shift = layout.shift;
    let full = frame.color.range == Range::Full;
    let top = (1u32 << tbits) - 1;
    match &frame.data {
        PixelData::Yuv8 { planes: [y, u, v], chroma, alpha: a } => {
            let src = Source { w: fw, h: fh, shift: chroma.shifts(), bits: 8, full };
            let (pl, pc) = (src.luma_lut(tbits), src.chroma_lut(tbits));
            fill(luma, ls, w, h, (0, 0), sb, |x, yy| src.at(y, x, yy, false, &pl));
            fill(cb, cs, cw, ch, shift, sb, |x, yy| src.at(u, x, yy, true, &pc));
            fill(cr, cs, cw, ch, shift, sb, |x, yy| src.at(v, x, yy, true, &pc));
            if layout.alpha {
                let pa = src.alpha_lut(tbits);
                match a {
                    Some(a) => fill(alpha, ls, w, h, (0, 0), sb, |x, yy| src.at(a, x, yy, false, &pa)),
                    None => fill(alpha, ls, w, h, (0, 0), sb, |_, _| top as u16),
                }
            }
        }
        PixelData::Yuv16 { planes: [y, u, v], chroma, bits, alpha: a } => {
            let src = Source { w: fw, h: fh, shift: chroma.shifts(), bits: (*bits).clamp(1, 16), full };
            let (pl, pc) = (src.luma_lut(tbits), src.chroma_lut(tbits));
            fill(luma, ls, w, h, (0, 0), sb, |x, yy| src.at(y, x, yy, false, &pl));
            fill(cb, cs, cw, ch, shift, sb, |x, yy| src.at(u, x, yy, true, &pc));
            fill(cr, cs, cw, ch, shift, sb, |x, yy| src.at(v, x, yy, true, &pc));
            if layout.alpha {
                let pa = src.alpha_lut(tbits);
                match a {
                    Some(a) => fill(alpha, ls, w, h, (0, 0), sb, |x, yy| src.at(a, x, yy, false, &pa)),
                    None => fill(alpha, ls, w, h, (0, 0), sb, |_, _| top as u16),
                }
            }
        }
        PixelData::Rgba8(px) => {
            let scale = f32::from(1u16 << (tbits.clamp(8, 16) - 8));
            let ycc = |x: usize, y: usize| -> [f32; 4] {
                let i = (y * fw + x) * 4;
                let p = px.get(i..i + 4).unwrap_or(&[0, 0, 0, 255]);
                let c = |k: usize| f32::from(p.get(k).copied().unwrap_or(0)) / 255.0;
                let (r, g, b) = (c(0), c(1), c(2));
                let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let pb = (b - l) / 1.8556;
                let pr = (r - l) / 1.5748;
                [16.0 + 219.0 * l, 128.0 + 224.0 * pb, 128.0 + 224.0 * pr, c(3)]
            };
            let q = |v: f32| (v * scale).round().clamp(0.0, top as f32) as u16;
            fill(luma, ls, w, h, (0, 0), sb, |x, y| q(ycc(x, y)[0]));
            fill(cb, cs, cw, ch, shift, sb, |x, y| q(ycc(x, y)[1]));
            fill(cr, cs, cw, ch, shift, sb, |x, y| q(ycc(x, y)[2]));
            if layout.alpha {
                fill(alpha, ls, w, h, (0, 0), sb, |x, y| (ycc(x, y)[3] * top as f32).round() as u16);
            }
        }
        PixelData::RgbaF32(_) => return Err("float frames cannot be looped".into()),
    }
    Ok(())
}

/// One plane's source: the frame's size, its chroma subsampling, bit depth and range.
struct Source {
    w: usize,
    h: usize,
    shift: (u32, u32),
    bits: u32,
    full: bool,
}

impl Source {
    /// Sample of `plane` at luma position (`x`, `y`) through `lut`.
    fn at<T: Copy + Into<u32>>(&self, plane: &[T], x: usize, y: usize, chroma: bool, lut: &[u16]) -> u16 {
        let (sx, sy) = if chroma { self.shift } else { (0, 0) };
        let pw = self.w.div_ceil(1 << sx);
        let ph = self.h.div_ceil(1 << sy);
        let (px, py) = ((x >> sx).min(pw.saturating_sub(1)), (y >> sy).min(ph.saturating_sub(1)));
        let v = plane.get(py * pw + px).map_or(0, |&v| v.into());
        lut.get(v as usize).copied().unwrap_or(0)
    }

    /// A table from source values to `tbits` video-range values, through `map` from the
    /// source value normalised to 0..1 to an 8-bit video-range value.
    fn lut(&self, tbits: u32, map: impl Fn(f32) -> f32) -> Vec<u16> {
        let smax = ((1u32 << self.bits) - 1) as f32;
        let tscale = (1u32 << tbits.saturating_sub(8)) as f32;
        let top = ((1u32 << tbits) - 1) as f32;
        (0..(1u32 << self.bits))
            .map(|v| (map(v as f32 / smax) * tscale).round().clamp(0.0, top) as u16)
            .collect()
    }

    fn luma_lut(&self, tbits: u32) -> Vec<u16> {
        if self.full { self.lut(tbits, |n| 16.0 + 219.0 * n) } else { self.lut(tbits, |n| n * 255.0) }
    }

    fn chroma_lut(&self, tbits: u32) -> Vec<u16> {
        if self.full {
            let half = (1u32 << (self.bits - 1)) as f32 / ((1u32 << self.bits) - 1) as f32;
            self.lut(tbits, move |n| 128.0 + 224.0 * (n - half))
        } else {
            self.lut(tbits, |n| n * 255.0)
        }
    }

    fn alpha_lut(&self, tbits: u32) -> Vec<u16> {
        let top = ((1u32 << tbits) - 1) as f32;
        let smax = ((1u32 << self.bits) - 1) as f32;
        (0..(1u32 << self.bits)).map(|v| (v as f32 / smax * top).round() as u16).collect()
    }
}

/// Fills a plane of `w` x `h` samples (rows `stride` samples apart, `sb` bytes each) with
/// `value` at each sample's luma position.
fn fill(
    plane: &mut [u8],
    stride: usize,
    w: usize,
    h: usize,
    shift: (u32, u32),
    sb: usize,
    value: impl Fn(usize, usize) -> u16,
) {
    for (y, row) in plane.chunks_mut(stride * sb).take(h).enumerate() {
        for (x, d) in row.chunks_exact_mut(sb).take(w).enumerate() {
            let v = value(x << shift.0, y << shift.1);
            match d {
                [b] => *b = v.min(255) as u8,
                [lo, hi] => [*lo, *hi] = v.to_le_bytes(),
                _ => {}
            }
        }
    }
}

/// The luma of `frame` reduced to [`THUMB_W`] x [`THUMB_H`] cell means, 8 bits. Of a frame
/// at least twice the thumbnail's size, every second pixel of every second row counts.
pub fn thumbnail(frame: &VideoFrame) -> Result<[u8; THUMB], String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 {
        return Err("an empty frame".into());
    }
    let step = if w >= 2 * THUMB_W && h >= 2 * THUMB_H { 2 } else { 1 };
    let mut sum = [0u32; THUMB];
    let mut count = [0u32; THUMB];
    let mut add = |x: usize, y: usize, v: u32| {
        let cell = (y * THUMB_H / h) * THUMB_W + x * THUMB_W / w;
        if let (Some(s), Some(c)) = (sum.get_mut(cell), count.get_mut(cell)) {
            *s += v;
            *c += 1;
        }
    };
    match &frame.data {
        PixelData::Yuv8 { planes: [y, ..], .. } => {
            for (row, line) in y.chunks(w).take(h).enumerate().step_by(step) {
                for (x, &v) in line.iter().enumerate().step_by(step) {
                    add(x, row, u32::from(v));
                }
            }
        }
        PixelData::Yuv16 { planes: [y, ..], bits, .. } => {
            let down = bits.saturating_sub(8);
            for (row, line) in y.chunks(w).take(h).enumerate().step_by(step) {
                for (x, &v) in line.iter().enumerate().step_by(step) {
                    add(x, row, u32::from(v) >> down);
                }
            }
        }
        PixelData::Rgba8(px) => {
            for (row, line) in px.chunks(w * 4).take(h).enumerate().step_by(step) {
                for (x, p) in line.as_chunks::<4>().0.iter().enumerate().step_by(step) {
                    let [r, g, b, _] = [0, 1, 2, 3].map(|k| u32::from(p.get(k).copied().unwrap_or(0)));
                    add(x, row, (54 * r + 183 * g + 19 * b) >> 8);
                }
            }
        }
        PixelData::RgbaF32(_) => return Err("float frames cannot be looped".into()),
    }
    let mut out = [0u8; THUMB];
    for ((o, s), c) in out.iter_mut().zip(sum).zip(count) {
        *o = s.checked_div(c).map_or(0, |v| v.min(255) as u8);
    }
    Ok(out)
}

/// Mixes `other` into `planes`, both laid out alike: `weight` 0 keeps `planes`, 1 is
/// `other`. Samples of `sample_bytes` bytes (1, or 2 little-endian).
pub fn blend(planes: &mut [u8], other: &[u8], weight: f64, sample_bytes: usize) {
    let w = (weight.clamp(0.0, 1.0) * 256.0).round() as u32;
    let mix = |a: u32, b: u32| (a * (256 - w) + b * w + 128) >> 8;
    if sample_bytes == 1 {
        for (a, &b) in planes.iter_mut().zip(other) {
            *a = mix(u32::from(*a), u32::from(b)) as u8;
        }
    } else {
        for (a, b) in planes.as_chunks_mut::<2>().0.iter_mut().zip(other.as_chunks::<2>().0) {
            let v = mix(u32::from(u16::from_le_bytes(*a)), u32::from(u16::from_le_bytes(*b)));
            *a = (v as u16).to_le_bytes();
        }
    }
}
