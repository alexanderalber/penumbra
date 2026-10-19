//! Textures to Y'CbCr planes for a video encoder, read back without waiting
//! (`out.video`).
//!
//! A compute pass ([`yuv.wgsl`](../yuv.wgsl)) converts the texture into a storage buffer in the
//! byte layout the encoder takes ([`PlaneLayout`]); a copy moves that into one of a small ring
//! of readback buffers, which then maps whenever the GPU gets there. The cook thread only
//! records and submits; the encoder's thread waits for the mapping ([`MappedPlanes::wait`]),
//! reads the bytes and drops the frame, which hands the buffer back to the ring. When every
//! buffer of the ring is on its way, [`YuvReader::grab`] says so instead of waiting, and the
//! caller drops the frame. Planes are a third (4:2:0, 8 bits) to the same size (4:4:4 with
//! alpha, 16 bits) of the texture's 8 bytes per pixel, so reading back costs less than the
//! texture would.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::{Error, lock};

use crate::{Gpu, Tex};

const SOURCE: &str = include_str!("yuv.wgsl");

/// How the planes lie in the buffer; mirrors the encoder's layout (`penumbra_record::Layout`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneLayout {
    /// Output size; the texture is scaled to it.
    pub width: u32,
    pub height: u32,
    /// Chroma subsampling as shifts (x, y): (1, 1) 4:2:0, (1, 0) 4:2:2, (0, 0) 4:4:4.
    pub shift: (u32, u32),
    /// Significant bits per sample, 8 to 16; more than 8 take two bytes.
    pub bits: u32,
    pub alpha: bool,
    /// Samples per row: luma (and alpha), a multiple of 8; chroma, a multiple of 4.
    pub luma_stride: u32,
    pub chroma_stride: u32,
    /// Byte offsets of the Y', Cb, Cr and A planes, multiples of 4.
    pub offsets: [u64; 4],
    /// Bytes of a frame, a multiple of 4.
    pub bytes: u64,
}

impl PlaneLayout {
    fn check(&self) -> Result<(), Error> {
        let ok = self.width >= 2
            && self.height >= 2
            && self.height.is_multiple_of(2)
            && (8..=16).contains(&self.bits)
            && self.shift.0 <= 1
            && self.shift.1 <= self.shift.0
            && self.luma_stride.is_multiple_of(8)
            && self.luma_stride >= self.width
            && self.chroma_stride == self.luma_stride >> self.shift.0
            && self.offsets.iter().all(|o| o.is_multiple_of(4) && *o <= self.bytes)
            && self.bytes.is_multiple_of(4);
        if ok { Ok(()) } else { Err(Error::new(format!("unusable plane layout {self:?}"))) }
    }

    /// The uniform block of `yuv.wgsl`.
    fn uniform(&self) -> Vec<u8> {
        let word = |b: u64| u32::try_from(b / 4).unwrap_or(u32::MAX);
        let words: [u32; 16] = [
            self.width,
            self.height,
            self.shift.0,
            self.shift.1,
            word(self.offsets[0]),
            word(self.offsets[1]),
            word(self.offsets[2]),
            word(self.offsets[3]),
            self.luma_stride,
            self.chroma_stride,
            self.bits,
            u32::from(self.bits > 8),
            u32::from(self.alpha),
            0,
            0,
            0,
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

/// The compiled conversion, shared per device.
pub struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Pipeline {
    fn compile(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let (pipeline, error) = gpu.scoped(|| {
            let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty,
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("yuv"),
                entries: &[
                    entry(
                        0,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    entry(
                        1,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                    ),
                    entry(2, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                    entry(
                        3,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("yuv"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("yuv.wgsl"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("yuv"),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("yuv"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            Self { pipeline, layout, sampler }
        });
        match error {
            Some(e) => Err(Error::new(format!("yuv.wgsl: {e}"))),
            None => Ok(pipeline),
        }
    }
}

impl Gpu {
    /// The texture-to-planes conversion, compiled on first use.
    pub fn yuv_pipeline(&self) -> Result<Arc<Pipeline>, Error> {
        let mut slot = lock(&self.inner.yuv);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = Arc::new(Pipeline::compile(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}

const WAITING: u8 = 0;
const MAPPED: u8 = 1;
const FAILED: u8 = 2;

/// Converts textures to planes and reads them back through a ring of at most `max` buffers.
/// Owned by the node that records.
pub struct YuvReader {
    max: usize,
    in_flight: Arc<AtomicUsize>,
    free: Vec<wgpu::Buffer>,
    back: Sender<wgpu::Buffer>,
    returned: Receiver<wgpu::Buffer>,
    /// The conversion's output, reused while the size stays.
    storage: Option<wgpu::Buffer>,
}

impl YuvReader {
    pub fn new(max: usize) -> Self {
        let (back, returned) = channel();
        Self {
            max: max.max(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            free: Vec::new(),
            back,
            returned,
            storage: None,
        }
    }

    /// Frames read back and not yet dropped.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Converts `tex` into `layout`'s planes and starts reading them back. `Ok(None)` when
    /// every buffer of the ring is still on its way (the frame is to be dropped). Never waits
    /// for the GPU.
    pub fn grab(
        &mut self,
        gpu: &Gpu,
        tex: &Tex,
        layout: &PlaneLayout,
    ) -> Result<Option<MappedPlanes>, Error> {
        layout.check()?;
        let size = layout.bytes;
        while let Ok(b) = self.returned.try_recv() {
            self.free.push(b);
        }
        self.free.retain(|b| b.size() == size);
        if self.in_flight() >= self.max {
            return Ok(None);
        }
        let pipeline = gpu.yuv_pipeline()?;
        let device = gpu.device();
        let uniform = layout.uniform();
        let reuse_storage = self.storage.as_ref().is_some_and(|s| s.size() == size);
        let free = self.free.pop();
        let ((storage, staging), error) = gpu.scoped(|| {
            let storage = match self.storage.take().filter(|_| reuse_storage) {
                Some(s) => s,
                None => device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("yuv planes"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
            };
            let staging = free.unwrap_or_else(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("yuv readback"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            });
            let params = gpu.uniform_buffer("yuv params", &uniform);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuv"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(tex.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                    wgpu::BindGroupEntry { binding: 3, resource: storage.as_entire_binding() },
                ],
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuv") });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("yuv"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &group, &[]);
                let blocks_x = (layout.luma_stride / 8).div_ceil(8);
                let blocks_y = (layout.height / 2).div_ceil(8);
                pass.dispatch_workgroups(blocks_x, blocks_y, 1);
            }
            encoder.copy_buffer_to_buffer(&storage, 0, &staging, 0, size);
            gpu.submit([encoder.finish()]);
            (storage, staging)
        });
        self.storage = Some(storage);
        if let Some(e) = error {
            return Err(Error::new(format!("converting for the recording failed: {e}")));
        }
        let state = Arc::new(AtomicU8::new(WAITING));
        let mapped = state.clone();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            mapped.store(if r.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
        });
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        Ok(Some(MappedPlanes {
            buffer: Some(staging),
            state,
            device: device.clone(),
            back: Mutex::new(self.back.clone()),
            in_flight: self.in_flight.clone(),
        }))
    }
}

/// One frame's planes on their way back from the GPU. Wait for them on any thread but the
/// cook thread; dropping it returns the buffer to its [`YuvReader`].
pub struct MappedPlanes {
    buffer: Option<wgpu::Buffer>,
    state: Arc<AtomicU8>,
    device: wgpu::Device,
    /// In a mutex only to make the whole `Sync` (a sender is `Send` but not `Sync`).
    back: Mutex<Sender<wgpu::Buffer>>,
    in_flight: Arc<AtomicUsize>,
}

impl MappedPlanes {
    /// Waits up to `timeout` for the bytes, polling the device without blocking it for others.
    pub fn wait(&self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.state.load(Ordering::Acquire) {
                MAPPED => return Ok(()),
                FAILED => return Err(Error::new("the GPU could not hand the frame back")),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(Error::new("the GPU did not hand the frame back in time"));
            }
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|e| Error::new(format!("waiting for the GPU: {e}")))?;
            if self.state.load(Ordering::Acquire) == WAITING {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Hands the planes to `f` once mapped ([`MappedPlanes::wait`] first).
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Result<R, Error> {
        if self.state.load(Ordering::Acquire) != MAPPED {
            return Err(Error::new("the frame is not back yet"));
        }
        let buffer = self.buffer.as_ref().ok_or_else(|| Error::new("the frame is gone"))?;
        let view = buffer.get_mapped_range(..).map_err(|e| Error::new(format!("reading the frame: {e}")))?;
        Ok(f(&view))
    }
}

impl Drop for MappedPlanes {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        if let Some(buffer) = self.buffer.take()
            && self.state.load(Ordering::Acquire) == MAPPED
        {
            buffer.unmap();
            // A closed channel: the reader is gone, the buffer goes with this frame.
            lock(&self.back).send(buffer).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;
    use half::f16;

    /// A 4:2:0 8-bit layout as the H.264 recording uses it.
    fn layout420(w: u32, h: u32) -> PlaneLayout {
        let ls = w.div_ceil(8) * 8;
        let cs = ls / 2;
        let y = u64::from(ls * h);
        let c = u64::from(cs * h / 2);
        PlaneLayout {
            width: w,
            height: h,
            shift: (1, 1),
            bits: 8,
            alpha: false,
            luma_stride: ls,
            chroma_stride: cs,
            offsets: [0, y, y + c, y + 2 * c],
            bytes: y + 2 * c,
        }
    }

    fn flat(gpu: &Gpu, w: u32, h: u32, rgba: [f32; 4]) -> Tex {
        let mut bytes = Vec::new();
        for _ in 0..w * h {
            for c in rgba {
                bytes.extend_from_slice(&f16::from_f32(c).to_le_bytes());
            }
        }
        gpu.upload_rgba16f(w, h, &mut bytes, "flat").unwrap()
    }

    #[test]
    fn colours_become_bt709_video_range() {
        let Some(gpu) = gpu() else { return };
        let mut reader = YuvReader::new(2);
        let l = layout420(20, 6);
        // White, black, 18 % grey (sRGB 0.46), linear red; NaN reads as black.
        for (rgba, want) in [
            ([1.0, 1.0, 1.0, 1.0], (235, 128, 128)),
            ([0.0, 0.0, 0.0, 1.0], (16, 128, 128)),
            ([0.18, 0.18, 0.18, 1.0], (117, 128, 128)),
            ([1.0, 0.0, 0.0, 1.0], (63, 102, 240)),
            ([f32::NAN, 0.0, 0.0, 1.0], (16, 128, 128)),
        ] {
            let tex = flat(&gpu, 32, 12, rgba);
            let planes = reader.grab(&gpu, &tex, &l).unwrap().unwrap();
            planes.wait(Duration::from_secs(10)).unwrap();
            let (y, cb, cr) = planes
                .with_bytes(|b| {
                    let o = l.offsets.map(|o| o as usize);
                    // Last real column of the last row, and a chroma sample.
                    let yl = b[(l.height as usize - 1) * l.luma_stride as usize + l.width as usize - 1];
                    (yl, b[o[1] + 3], b[o[2] + 3])
                })
                .unwrap();
            let near = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs() <= 1;
            assert!(near(y, want.0) && near(cb, want.1) && near(cr, want.2), "{rgba:?}: {:?}", (y, cb, cr));
        }
    }

    #[test]
    fn the_ring_runs_full_instead_of_waiting() {
        let Some(gpu) = gpu() else { return };
        let mut reader = YuvReader::new(2);
        let l = layout420(16, 16);
        let tex = flat(&gpu, 16, 16, [0.5, 0.5, 0.5, 1.0]);
        let a = reader.grab(&gpu, &tex, &l).unwrap().unwrap();
        let b = reader.grab(&gpu, &tex, &l).unwrap().unwrap();
        assert!(reader.grab(&gpu, &tex, &l).unwrap().is_none(), "two in flight, the third has no buffer");
        a.wait(Duration::from_secs(10)).unwrap();
        drop(a);
        assert_eq!(reader.in_flight(), 1);
        let c = reader.grab(&gpu, &tex, &l).unwrap().unwrap();
        assert_eq!(reader.free.len(), 0, "the returned buffer was taken again");
        // Waited for on another thread, as the encoder does.
        let t = std::thread::spawn(move || {
            b.wait(Duration::from_secs(10)).unwrap();
            c.wait(Duration::from_secs(10)).unwrap();
            c.with_bytes(|bytes| bytes.len()).unwrap()
        });
        assert_eq!(t.join().unwrap() as u64, l.bytes);
        assert_eq!(reader.in_flight(), 0);
        let mut bad = l;
        bad.luma_stride = 12;
        assert!(reader.grab(&gpu, &tex, &bad).is_err());
    }
}
