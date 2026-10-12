//! Camera frames: what a device delivers (packed or semi-planar Y'CbCr) rearranged into the
//! three planes the GPU's video conversion reads. One pass over the bytes on the capture thread,
//! so the cook thread only uploads.

use std::time::Instant;

use serde::Serialize;

/// Pixel layouts a camera may offer. Only the 8-bit Y'CbCr ones are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum PixelFormat {
    /// 4:2:0, a Y plane, then Cb and Cr interleaved at half size.
    Nv12,
    /// 4:2:2 packed as Y0 Cb Y1 Cr.
    Yuy2,
    /// 4:2:2 packed as Cb Y0 Cr Y1.
    Uyvy,
    /// 4:2:0 planar, Y then Cb then Cr.
    I420,
    /// Anything else (MJPEG, RGB, 10-bit), listed but not opened.
    Other,
}

impl PixelFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::Yuy2 => "YUY2",
            Self::Uyvy => "UYVY",
            Self::I420 => "I420",
            Self::Other => "other",
        }
    }

    pub fn readable(self) -> bool {
        self != Self::Other
    }

    /// Preference when nothing is asked for: the 4:2:0 formats move fewer bytes.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Nv12 => 0,
            Self::I420 => 1,
            Self::Yuy2 => 2,
            Self::Uyvy => 3,
            Self::Other => 9,
        }
    }

    /// Chroma shifts (horizontal, vertical) after rearranging.
    pub fn shift(self) -> (u32, u32) {
        match self {
            Self::Nv12 | Self::I420 => (1, 1),
            Self::Yuy2 | Self::Uyvy | Self::Other => (1, 0),
        }
    }

    /// Bytes of one row of luma samples as packed by the device (the default stride).
    pub fn min_stride(self, width: u32) -> usize {
        let w = width as usize;
        match self {
            Self::Nv12 | Self::I420 | Self::Other => w,
            Self::Yuy2 | Self::Uyvy => w.div_ceil(2) * 4,
        }
    }
}

/// The matrix a device's Y'CbCr is coded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Matrix {
    Bt601,
    Bt709,
}

/// One frame, rearranged to planes.
#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    /// As delivered by the device.
    pub format: PixelFormat,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    /// Chroma shifts of `cb` and `cr`: (1, 1) is 4:2:0, (1, 0) 4:2:2.
    pub shift: (u32, u32),
    pub matrix: Matrix,
    pub full_range: bool,
    /// Counts frames since the stream opened, from 1.
    pub sequence: u64,
    /// When the capture thread had the frame.
    pub arrived: Instant,
    /// Milliseconds from the device's timestamp to `arrived`, when the device stamps with the
    /// system clock (Media Foundation's capture sources do).
    pub delay_ms: Option<f64>,
}

/// The planes of a frame, as [`rearrange`] makes them.
#[derive(Debug, PartialEq)]
pub struct Planes {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

/// Rearranges a device buffer into planes. `stride` is the byte distance between luma rows (at
/// least the format's row size). Refuses buffers that are too short instead of reading past
/// them.
pub fn rearrange(
    format: PixelFormat,
    width: u32,
    height: u32,
    stride: usize,
    data: &[u8],
) -> Result<Planes, String> {
    if width == 0 || height == 0 {
        return Err("an empty frame".into());
    }
    let row = format.min_stride(width);
    if stride < row {
        return Err(format!("a stride of {stride} bytes is less than a row of {row}"));
    }
    let (w, h) = (width as usize, height as usize);
    let (sx, sy) = format.shift();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let short = |need: usize| {
        format!("{} bytes, a {width}x{height} {} frame needs {need}", data.len(), format.name())
    };
    let mut out = Planes {
        y: Vec::with_capacity(w * h),
        cb: Vec::with_capacity(cw * ch),
        cr: Vec::with_capacity(cw * ch),
    };
    match format {
        PixelFormat::Nv12 => {
            // Chroma rows follow the luma rows at the same stride.
            let need = stride * (h + ch - 1) + w.max(cw * 2);
            if data.len() < need {
                return Err(short(need));
            }
            for r in data.chunks(stride).take(h) {
                out.y.extend_from_slice(r.get(..w).ok_or_else(|| short(need))?);
            }
            let chroma = data.get(stride * h..).ok_or_else(|| short(need))?;
            for r in chroma.chunks(stride).take(ch) {
                for &[u, v] in r.get(..cw * 2).ok_or_else(|| short(need))?.as_chunks::<2>().0 {
                    out.cb.push(u);
                    out.cr.push(v);
                }
            }
        }
        PixelFormat::I420 => {
            // Chroma planes at half the stride, as Windows lays out I420 and IYUV.
            let cstride = stride.div_ceil(2);
            let need = stride * h + cstride * ch * 2;
            if data.len() < need {
                return Err(short(need));
            }
            for r in data.chunks(stride).take(h) {
                out.y.extend_from_slice(r.get(..w).ok_or_else(|| short(need))?);
            }
            let rest = data.get(stride * h..).ok_or_else(|| short(need))?;
            let (cb, cr) = rest.split_at_checked(cstride * ch).ok_or_else(|| short(need))?;
            for r in cb.chunks(cstride).take(ch) {
                out.cb.extend_from_slice(r.get(..cw).ok_or_else(|| short(need))?);
            }
            for r in cr.chunks(cstride).take(ch) {
                out.cr.extend_from_slice(r.get(..cw).ok_or_else(|| short(need))?);
            }
        }
        PixelFormat::Yuy2 | PixelFormat::Uyvy => {
            let need = stride * (h - 1) + row;
            if data.len() < need {
                return Err(short(need));
            }
            let yuy2 = format == PixelFormat::Yuy2;
            for r in data.chunks(stride).take(h) {
                let r = r.get(..row).ok_or_else(|| short(need))?;
                let mut left = w;
                for &[a, b, c, d] in r.as_chunks::<4>().0 {
                    let (y0, u, y1, v) = if yuy2 { (a, b, c, d) } else { (b, a, d, c) };
                    out.y.push(y0);
                    if left > 1 {
                        out.y.push(y1);
                    }
                    left = left.saturating_sub(2);
                    out.cb.push(u);
                    out.cr.push(v);
                }
            }
        }
        PixelFormat::Other => return Err("this pixel format is not read".into()),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_splits_its_chroma() {
        // 4x2, stride 6 (two bytes of padding per row).
        let data = [
            1, 2, 3, 4, 0, 0, //
            5, 6, 7, 8, 0, 0, //
            10, 20, 11, 21, 0, 0,
        ];
        let p = rearrange(PixelFormat::Nv12, 4, 2, 6, &data).unwrap();
        assert_eq!(p.y, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(p.cb, vec![10, 11]);
        assert_eq!(p.cr, vec![20, 21]);
        // The last row's padding may be missing, anything less is refused.
        assert!(rearrange(PixelFormat::Nv12, 4, 2, 6, &data[..16]).is_ok());
        assert!(rearrange(PixelFormat::Nv12, 4, 2, 6, &data[..15]).is_err());
        assert!(rearrange(PixelFormat::Nv12, 4, 2, 3, &data).is_err());
    }

    #[test]
    fn packed_422_unpacks_both_orders() {
        // 3x1 (odd width: the last pair's second luma is dropped), stride 8.
        let yuy2 = [1, 10, 2, 20, 3, 11, 9, 21];
        let p = rearrange(PixelFormat::Yuy2, 3, 1, 8, &yuy2).unwrap();
        assert_eq!((p.y, p.cb, p.cr), (vec![1, 2, 3], vec![10, 11], vec![20, 21]));
        let uyvy = [10, 1, 20, 2, 11, 3, 21, 9];
        let p = rearrange(PixelFormat::Uyvy, 3, 1, 8, &uyvy).unwrap();
        assert_eq!((p.y, p.cb, p.cr), (vec![1, 2, 3], vec![10, 11], vec![20, 21]));
        assert!(rearrange(PixelFormat::Yuy2, 3, 2, 8, &yuy2).is_err());
    }

    #[test]
    fn i420_reads_half_stride_chroma() {
        let data = [
            1, 2, 3, 4, //
            5, 6, 7, 8, //
            10, 11, //
            20, 21,
        ];
        let p = rearrange(PixelFormat::I420, 4, 2, 4, &data).unwrap();
        assert_eq!((p.y.len(), p.cb, p.cr), (8, vec![10, 11], vec![20, 21]));
    }

    #[test]
    fn hostile_sizes_are_errors() {
        assert!(rearrange(PixelFormat::Nv12, 0, 2, 4, &[0; 64]).is_err());
        assert!(rearrange(PixelFormat::Other, 2, 2, 4, &[0; 64]).is_err());
        assert!(rearrange(PixelFormat::Yuy2, 100_000, 100_000, 400_000, &[0; 64]).is_err());
    }
}
