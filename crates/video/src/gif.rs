//! Animated GIFs. FilmCraft reads only a GIF's first frame, so these are decoded with the `gif`
//! crate and composited here: each frame is a rectangle drawn onto the canvas, and its disposal
//! method says what happens to that rectangle before the next one (keep, clear to transparent,
//! restore what was there). Only the rectangle is touched, so small frames on a large canvas
//! stay cheap. (`image`'s GIF animation decoder took 260 ms per frame on a 1500 x 2250 GIF, for
//! a frame period of 30 ms.) Frame delays come from a first scan that skips the pixel data.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gif::DisposalMethod;

use crate::{ClipInfo, Decoder, Frame, Limits, Timing, VideoFrame};

/// Most bytes the canvas (RGBA) and the decoder's buffers may take.
const MAX_BYTES: u64 = 512 << 20;

/// Whether the file starts like a GIF.
pub fn sniff(path: &Path) -> Result<bool, String> {
    let mut head = [0u8; 4];
    let mut f = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match f.read_exact(&mut head) {
        Ok(()) => Ok(&head == b"GIF8"),
        Err(_) => Ok(false),
    }
}

fn options(skip_pixels: bool) -> gif::DecodeOptions {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.skip_frame_decoding(skip_pixels);
    if let Some(limit) = std::num::NonZeroU64::new(MAX_BYTES) {
        options.set_memory_limit(gif::MemoryLimit::Bytes(limit));
    }
    options
}

/// What to undo before the next frame is drawn.
struct Disposal {
    method: DisposalMethod,
    /// The frame's rectangle clipped to the canvas: x, y, width, height.
    rect: (usize, usize, usize, usize),
    /// The rectangle's pixels before the frame was drawn (for `Previous`).
    saved: Vec<u8>,
}

/// Reading position: the decoder and the canvas after frame `next - 1`.
struct Reading {
    decoder: gif::Decoder<BufReader<File>>,
    canvas: Vec<u8>,
    disposal: Option<Disposal>,
    next: usize,
}

pub struct GifDecoder {
    path: PathBuf,
    info: ClipInfo,
    reading: Option<Reading>,
}

impl GifDecoder {
    pub fn open(path: &Path, limits: &Limits) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let mut scan = options(true).read_info(BufReader::new(file)).map_err(|e| format!("GIF: {e}"))?;
        let (width, height) = (u32::from(scan.width()), u32::from(scan.height()));
        if u64::from(width) * u64::from(height) * 4 > MAX_BYTES {
            return Err(format!("GIF: a {width}x{height} canvas is too large"));
        }
        let mut ends = Vec::new();
        // In hundredths, so the ends are exact sums.
        let mut t = 0u64;
        while let Some(frame) = scan.next_frame_info().map_err(|e| format!("GIF: {e}"))? {
            // Delays of 0 or 1 hundredths play at 10 fps in browsers; do the same.
            let delay = if frame.delay <= 1 { 10 } else { frame.delay };
            t += u64::from(delay);
            ends.push(t as f64 / 100.0);
            if ends.len() > limits.max_frames {
                return Err(format!("more than {} frames", limits.max_frames));
            }
        }
        let info = ClipInfo {
            width,
            height,
            frames: ends.len(),
            timing: Timing::Ends(ends.into()),
            codec: "GIF".into(),
            container: "gif".into(),
            seeks: false,
        };
        Ok(Self { path: path.to_path_buf(), info, reading: None })
    }

    fn rewind(&self) -> Result<Reading, String> {
        let file = File::open(&self.path).map_err(|e| format!("cannot open {}: {e}", self.path.display()))?;
        let decoder = options(false).read_info(BufReader::new(file)).map_err(|e| format!("GIF: {e}"))?;
        if (u32::from(decoder.width()), u32::from(decoder.height())) != (self.info.width, self.info.height) {
            return Err("GIF: the file changed while playing".into());
        }
        let n = self.info.width as usize * self.info.height as usize * 4;
        Ok(Reading { decoder, canvas: vec![0; n], disposal: None, next: 0 })
    }
}

impl Reading {
    /// Decodes the next frame and draws it onto the canvas.
    fn advance(&mut self, width: usize, height: usize) -> Result<(), String> {
        let stride = width * 4;
        if let Some(d) = self.disposal.take() {
            let (x, y, w, h) = d.rect;
            for row in 0..h {
                let at = (y + row) * stride + x * 4;
                let Some(dst) = self.canvas.get_mut(at..at + w * 4) else { continue };
                match d.method {
                    DisposalMethod::Background => dst.fill(0),
                    DisposalMethod::Previous => {
                        if let Some(src) = d.saved.get(row * w * 4..(row + 1) * w * 4) {
                            dst.copy_from_slice(src);
                        }
                    }
                    DisposalMethod::Any | DisposalMethod::Keep => break,
                }
            }
        }
        let frame = self
            .decoder
            .read_next_frame()
            .map_err(|e| format!("GIF frame {}: {e}", self.next))?
            .ok_or_else(|| format!("GIF: frame {} is missing", self.next))?;
        let (fx, fy) = (usize::from(frame.left), usize::from(frame.top));
        let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
        // The rectangle clipped to the canvas.
        let x = fx.min(width);
        let y = fy.min(height);
        let w = (fx + fw).min(width) - x;
        let h = (fy + fh).min(height) - y;
        let mut saved = Vec::new();
        if frame.dispose == DisposalMethod::Previous {
            saved.reserve(w * h * 4);
            for row in 0..h {
                let at = (y + row) * stride + x * 4;
                if let Some(src) = self.canvas.get(at..at + w * 4) {
                    saved.extend_from_slice(src);
                }
            }
        }
        let src_stride = fw * 4;
        for row in 0..h {
            let at = (y + row) * stride + x * 4;
            let from = row * src_stride;
            let (Some(dst), Some(src)) =
                (self.canvas.get_mut(at..at + w * 4), frame.buffer.get(from..from + w * 4))
            else {
                continue;
            };
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                // Transparent pixels of a frame leave the canvas as it is.
                if s[3] != 0 {
                    *d = *s;
                }
            }
        }
        self.disposal = Some(Disposal { method: frame.dispose, rect: (x, y, w, h), saved });
        self.next += 1;
        Ok(())
    }
}

impl Decoder for GifDecoder {
    fn info(&self) -> &ClipInfo {
        &self.info
    }

    fn frame(&mut self, index: usize) -> Result<Frame, String> {
        if index >= self.info.frames {
            return Err(format!("frame {index} is past the end"));
        }
        let (width, height) = (self.info.width as usize, self.info.height as usize);
        let mut reading = match self.reading.take() {
            Some(r) if r.next <= index => r,
            _ => self.rewind()?,
        };
        while reading.next <= index {
            // After an error the reading is dropped, so the next call starts over.
            reading.advance(width, height)?;
        }
        let frame = VideoFrame::rgba8(self.info.width, self.info.height, reading.canvas.clone());
        self.reading = Some(reading);
        Ok(Frame::Pixels(Arc::new(frame)))
    }
}
