//! Reading textures back to the CPU: snapshots for Claude and the user, golden-image tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use half::f16;
use penumbra_core::Error;
use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};

use crate::{Gpu, Tex};

/// How long a readback may wait for the GPU.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// A texture's pixels on the CPU, as stored: linear, premultiplied RGBA, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<[f32; 4]>,
}

/// Per-channel minimum, maximum and mean of the stored (premultiplied) values: a quick check of
/// a result without looking at the picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub min: [f32; 4],
    pub max: [f32; 4],
    pub mean: [f32; 4],
    /// Pixels with a NaN or infinite component.
    pub non_finite: u64,
}

impl Pixels {
    pub fn get(&self, x: u32, y: u32) -> Option<[f32; 4]> {
        if x >= self.width {
            return None;
        }
        self.data.get(y as usize * self.width as usize + x as usize).copied()
    }

    pub fn stats(&self) -> Stats {
        let mut min = [f32::INFINITY; 4];
        let mut max = [f32::NEG_INFINITY; 4];
        let mut sum = [0f64; 4];
        let mut non_finite = 0;
        let mut finite = 0u64;
        for px in &self.data {
            if px.iter().any(|c| !c.is_finite()) {
                non_finite += 1;
                continue;
            }
            finite += 1;
            for (c, v) in px.iter().enumerate() {
                if let (Some(lo), Some(hi), Some(s)) = (min.get_mut(c), max.get_mut(c), sum.get_mut(c)) {
                    *lo = lo.min(*v);
                    *hi = hi.max(*v);
                    *s += f64::from(*v);
                }
            }
        }
        let n = finite.max(1) as f64;
        if finite == 0 {
            (min, max) = ([0.0; 4], [0.0; 4]);
        }
        Stats { min, max, mean: sum.map(|s| (s / n) as f32), non_finite }
    }

    /// 8-bit sRGB with straight alpha, as image viewers expect; values outside 0..1 clip.
    pub fn to_srgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for &[r, g, b, a] in &self.data {
            let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
            let straight = |c: f32| if a > 0.0 && c.is_finite() { c / a } else { 0.0 };
            for c in [r, g, b] {
                out.push(unit_to_u8(linear_to_srgb(straight(c))));
            }
            out.push(unit_to_u8(a));
        }
        out
    }

    /// A PNG of [`Pixels::to_srgb8`].
    pub fn to_png(&self) -> Result<Vec<u8>, Error> {
        let image = Image::from_u8(self.width, self.height, ChannelLayout::Rgba, self.to_srgb8())
            .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
        photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default())
            .map_err(|e| Error::new(format!("cannot encode the PNG: {e}")))
    }
}

/// A PNG of 8-bit RGBA rows (straight alpha), e.g. a UI screenshot.
pub fn encode_png_rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, Error> {
    let image = Image::from_u8(width, height, ChannelLayout::Rgba, rgba)
        .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
    photocraft_codecs::encode(&image, Format::Png, &EncodeOptions::default())
        .map_err(|e| Error::new(format!("cannot encode the PNG: {e}")))
}

pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn unit_to_u8(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Gpu {
    /// Copies `tex` to the CPU and waits for it. Blocks the calling thread for a few
    /// milliseconds (more on a software adapter), so call it off the cook thread; it may run
    /// concurrently with cooking.
    pub fn read(&self, tex: &Tex) -> Result<Pixels, Error> {
        let (width, height) = tex.size();
        let row = u64::from(width) * 8;
        let padded = row.div_ceil(256) * 256;
        let size = padded * u64::from(height);
        let device = self.device();
        let ((buffer, index), error) = self.scoped(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: tex.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(u32::try_from(padded).unwrap_or(u32::MAX)),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            let index = self.submit([encoder.finish()]);
            (buffer, index)
        });
        if let Some(e) = error {
            return Err(Error::new(format!("readback failed: {e}")));
        }
        let (tx, rx) = mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            tx.send(r).ok();
        });
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(READ_TIMEOUT) })
            .map_err(|e| Error::new(format!("waiting for the GPU: {e}")))?;
        match rx.recv_timeout(READ_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(Error::new(format!("cannot map the readback buffer: {e}"))),
            Err(_) => return Err(Error::new("the GPU did not deliver the readback in time")),
        }
        let view = buffer.get_mapped_range(..).map_err(|e| Error::new(format!("readback: {e}")))?;
        let mut data = Vec::with_capacity(width as usize * height as usize);
        for y in 0..height as usize {
            let start = y * padded as usize;
            let line =
                view.get(start..start + row as usize).ok_or_else(|| Error::new("readback too short"))?;
            for px in line.as_chunks::<8>().0 {
                let (halves, _) = px.as_chunks::<2>();
                let mut c = [0f32; 4];
                for (out, h) in c.iter_mut().zip(halves) {
                    *out = f16::from_le_bytes(*h).to_f32();
                }
                data.push(c);
            }
        }
        drop(view);
        buffer.unmap();
        Ok(Pixels { width, height, data })
    }

    /// Starts reading `textures` back without waiting ([`PendingRead`]). One submission for all
    /// of them; the cost on the calling thread is creating the buffers and recording the copies.
    pub fn read_async(&self, textures: &[&Tex]) -> Result<PendingRead, Error> {
        self.read_async_reusing(textures, &mut ReadPool::default())
    }

    /// [`Gpu::read_async`] taking buffers of the right size from `spare` before creating new
    /// ones ([`PendingRead::recycle`] gives them back). Repeated readbacks of the same textures
    /// (state snapshots every minute) then allocate nothing: on DX12 a new readback buffer each
    /// time made the allocator reserve another block of system memory now and then.
    pub fn read_async_reusing(&self, textures: &[&Tex], spare: &mut ReadPool) -> Result<PendingRead, Error> {
        self.read_async_notify(textures, spare, None)
    }

    /// [`Gpu::read_async_reusing`] calling `ready` once every buffer is mapped (from inside
    /// [`Gpu::poll`] or [`PendingRead::wait`], so it must be quick: waking a node is).
    pub fn read_async_notify(
        &self,
        textures: &[&Tex],
        spare: &mut ReadPool,
        ready: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<PendingRead, Error> {
        let device = self.device();
        let ((items, index), error) = self.scoped(|| {
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("read_async") });
            let mut items = Vec::with_capacity(textures.len());
            for tex in textures {
                let (width, height) = tex.size();
                let padded = (u64::from(width) * 8).div_ceil(256) * 256;
                let size = padded * u64::from(height);
                let buffer = match spare.0.iter().position(|b| b.size() == size) {
                    Some(i) => spare.0.swap_remove(i),
                    None => device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("read_async"),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                };
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex.texture(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(u32::try_from(padded).unwrap_or(u32::MAX)),
                            rows_per_image: Some(height),
                        },
                    },
                    wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                );
                items.push(ReadItem { buffer, width, height, padded });
            }
            let index = self.submit([encoder.finish()]);
            (items, index)
        });
        if let Some(e) = error {
            return Err(Error::new(format!("readback failed: {e}")));
        }
        let mapped = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let count = items.len();
        for item in &items {
            let (mapped, failed, ready) = (mapped.clone(), failed.clone(), ready.clone());
            item.buffer.map_async(wgpu::MapMode::Read, .., move |r| match r {
                Ok(()) => {
                    if mapped.fetch_add(1, Ordering::AcqRel) + 1 == count
                        && let Some(f) = &ready
                    {
                        f();
                    }
                }
                Err(_) => failed.store(true, Ordering::Release),
            });
        }
        Ok(PendingRead { items, mapped, failed, index })
    }

    /// A texture from rows of little-endian half floats as [`PendingRead::rows`] delivers them
    /// (RGBA, linear, premultiplied), `width * height * 8` bytes. The bytes may come from a
    /// file: NaN and infinities become 0 (one would spread through a feedback loop forever).
    pub fn upload_rgba16f(
        &self,
        width: u32,
        height: u32,
        bytes: &mut [u8],
        label: &str,
    ) -> Result<Tex, Error> {
        let tex = Tex::new(self, width, height, label)?;
        self.write_rgba16f(&tex, bytes)?;
        Ok(tex)
    }

    /// [`Gpu::upload_rgba16f`] into an existing texture of the same size (a [`crate::TexRing`]'s).
    pub fn write_rgba16f(&self, tex: &Tex, bytes: &mut [u8]) -> Result<(), Error> {
        let (width, height) = tex.size();
        let expected = u64::from(width).saturating_mul(u64::from(height)).saturating_mul(8);
        if bytes.len() as u64 != expected {
            return Err(Error::new(format!(
                "{} bytes for a {width}x{height} texture, expected {expected}",
                bytes.len()
            )));
        }
        for h in bytes.as_chunks_mut::<2>().0 {
            // Exponent all ones: infinity or NaN.
            if u16::from_le_bytes(*h) & 0x7c00 == 0x7c00 {
                *h = [0, 0];
            }
        }
        let ((), error) = self.scoped(|| {
            self.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 8),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        });
        match error {
            Some(e) => Err(Error::new(format!("upload failed: {e}"))),
            None => Ok(()),
        }
    }
}

/// Readbacks started without waiting ([`Gpu::read_async`]): the copies are submitted at once,
/// the buffers map whenever the GPU gets there, driven by [`Gpu::poll`] (the cook thread calls
/// it every frame). The rows can then be read on any thread, so turning them into a file never
/// runs on the cook thread.
pub struct PendingRead {
    items: Vec<ReadItem>,
    mapped: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    index: wgpu::SubmissionIndex,
}

struct ReadItem {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded: u64,
}

/// Readback buffers kept for the next [`Gpu::read_async_reusing`].
#[derive(Default)]
pub struct ReadPool(Vec<wgpu::Buffer>);

impl ReadPool {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Bytes held.
    pub fn bytes(&self) -> u64 {
        self.0.iter().map(wgpu::Buffer::size).sum()
    }

    pub fn append(&mut self, other: ReadPool) {
        self.0.extend(other.0);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Where a [`PendingRead`] stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadState {
    Waiting,
    Ready,
    Failed,
}

/// Why [`PendingRead::rows`] stopped: the GPU side or the caller's sink.
#[derive(Debug)]
pub enum RowsError<E> {
    Gpu(Error),
    Sink(E),
}

impl PendingRead {
    /// The buffers, unmapped, for [`Gpu::read_async_reusing`]. Only a readback that is
    /// [`ReadState::Ready`] gives them back (the others may still be in use by the GPU).
    pub fn recycle(self) -> ReadPool {
        if self.state() != ReadState::Ready {
            return ReadPool::default();
        }
        ReadPool(
            self.items
                .into_iter()
                .map(|i| {
                    i.buffer.unmap();
                    i.buffer
                })
                .collect(),
        )
    }

    /// Waits at most `timeout` for the copies (and everything submitted before them), then
    /// says where the readback stands. For a small readback whose result is wanted in the same
    /// frame; the wait blocks the calling thread, so keep `timeout` short.
    pub fn wait(&self, gpu: &Gpu, timeout: Duration) -> ReadState {
        if self.state() == ReadState::Waiting {
            let wait =
                wgpu::PollType::Wait { submission_index: Some(self.index.clone()), timeout: Some(timeout) };
            // A timeout is not an error here: the caller looks again later.
            let _ = gpu.device().poll(wait);
        }
        self.state()
    }

    pub fn state(&self) -> ReadState {
        if self.failed.load(Ordering::Acquire) {
            ReadState::Failed
        } else if self.mapped.load(Ordering::Acquire) >= self.items.len() {
            ReadState::Ready
        } else {
            ReadState::Waiting
        }
    }

    /// Textures in this readback.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Bytes of the buffers (row padding included).
    pub fn buffer_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.buffer.size()).sum()
    }

    /// Size of texture `i`.
    pub fn size(&self, i: usize) -> Option<(u32, u32)> {
        self.items.get(i).map(|t| (t.width, t.height))
    }

    /// Hands the rows of texture `i` to `f`, top to bottom, each `width * 8` bytes of
    /// little-endian half floats (RGBA, as stored: linear, premultiplied). Only once ready.
    pub fn rows<E>(&self, i: usize, mut f: impl FnMut(&[u8]) -> Result<(), E>) -> Result<(), RowsError<E>> {
        let gpu_error = |m: String| RowsError::Gpu(Error::new(m));
        if self.state() != ReadState::Ready {
            return Err(gpu_error("the readback is not ready".into()));
        }
        let item = self.items.get(i).ok_or_else(|| gpu_error(format!("no texture {i}")))?;
        let view = item.buffer.get_mapped_range(..).map_err(|e| gpu_error(format!("readback: {e}")))?;
        let row = usize::try_from(u64::from(item.width) * 8).unwrap_or(usize::MAX);
        let padded = usize::try_from(item.padded).unwrap_or(usize::MAX);
        for y in 0..item.height as usize {
            let start = y.saturating_mul(padded);
            let line = view
                .get(start..start.saturating_add(row))
                .ok_or_else(|| gpu_error("readback too short".into()))?;
            f(line).map_err(RowsError::Sink)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip_and_png() {
        for v in [0.0f32, 0.001, 0.2, 0.5, 1.0] {
            assert!((srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-5);
        }
        let px = Pixels { width: 2, height: 1, data: vec![[0.5, 0.0, 0.0, 0.5], [0.0, 0.0, 0.0, 0.0]] };
        // Premultiplied 0.5 at alpha 0.5 is straight 1.0.
        assert_eq!(px.to_srgb8(), [255, 0, 0, 128, 0, 0, 0, 0]);
        let png = px.to_png().unwrap();
        assert_eq!(png.get(1..4), Some(&b"PNG"[..]));
        let s = px.stats();
        assert_eq!(s.max, [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(s.mean, [0.25, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn read_without_waiting_and_upload_again() {
        let Some(gpu) = crate::testing::gpu() else { return };
        // 33 pixels across: rows need padding to 256 bytes.
        let (w, h) = (33u32, 5u32);
        let mut bytes = Vec::new();
        for i in 0..w * h * 4 {
            bytes.extend_from_slice(&f16::from_f32(i as f32 / 1000.0).to_le_bytes());
        }
        let mut hostile = bytes.clone();
        hostile[0..2].copy_from_slice(&f16::NAN.to_le_bytes());
        hostile[2..4].copy_from_slice(&f16::INFINITY.to_le_bytes());
        let tex = gpu.upload_rgba16f(w, h, &mut hostile, "test").unwrap();
        let pending = gpu.read_async(&[&tex, &tex]).unwrap();
        assert_eq!(pending.size(1), Some((w, h)));
        let deadline = std::time::Instant::now() + READ_TIMEOUT;
        while pending.state() == ReadState::Waiting {
            assert!(std::time::Instant::now() < deadline);
            gpu.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(pending.state(), ReadState::Ready);
        let mut back = Vec::new();
        pending
            .rows::<()>(1, |row| {
                back.extend_from_slice(row);
                Ok(())
            })
            .unwrap();
        assert_eq!(back.len(), bytes.len());
        assert_eq!(back[0..4], [0, 0, 0, 0], "NaN and infinity became 0");
        assert_eq!(back[4..], bytes[4..]);
        assert!(pending.rows::<()>(2, |_| Ok(())).is_err());
        // The buffers serve the next readback of the same sizes.
        let mut pool = pending.recycle();
        assert_eq!(pool.len(), 2);
        let again = gpu.read_async_reusing(&[&tex], &mut pool).unwrap();
        assert_eq!((pool.len(), again.len()), (1, 1));
        assert!(gpu.upload_rgba16f(w, h, &mut [0u8; 8], "short").is_err());
    }
}
