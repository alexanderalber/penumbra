//! Frame stacks: many frames of one size kept on the GPU, written one at a time into a ring and
//! read back with a per-pixel choice of how many frames back to look (`tex.timemachine`).
//!
//! The frames live in array textures of up to [`CHUNK`] layers each (wgpu's default limit on
//! layers), at most [`CHUNKS`] of them, so one stack holds up to [`MAX_FRAMES`]. In practice
//! memory runs out first: a 1080p frame is 16 MB at 16 bits, 8 MB at 8 bits.

use std::sync::{Arc, PoisonError};

use penumbra_core::Error;

use crate::{FORMAT, Gpu, Tex};

/// Most layers per array texture.
pub const CHUNK: u32 = 256;
/// Most array textures per stack; the read pass binds them all, plus the map.
pub const CHUNKS: usize = 12;
/// Most frames one stack holds.
pub const MAX_FRAMES: u32 = CHUNK * CHUNKS as u32;

/// How a stack stores its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// [`FORMAT`], as the frames come in: HDR values kept.
    Half,
    /// 8 bits per channel, sRGB encoded (so dark parts do not band); half the memory, values
    /// clamped to 0..1.
    Byte,
}

impl Precision {
    fn format(self) -> wgpu::TextureFormat {
        match self {
            Precision::Half => FORMAT,
            Precision::Byte => wgpu::TextureFormat::Rgba8UnormSrgb,
        }
    }

    /// Bytes per pixel.
    pub fn bytes(self) -> u64 {
        match self {
            Precision::Half => 8,
            Precision::Byte => 4,
        }
    }
}

/// How [`Stack::read`] maps the map to frames back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Frames back at a map value (luminance) of 1.
    pub reach: f32,
    /// Frames back added everywhere.
    pub offset: f32,
    /// Mix the two frames around a fractional value; otherwise the nearer one.
    pub blend: bool,
}

struct Chunk {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// A ring of frames, newest written last. See the module docs.
pub struct Stack {
    chunks: Vec<Chunk>,
    width: u32,
    height: u32,
    frames: u32,
    chunk: u32,
    precision: Precision,
    /// The layer holding the newest frame.
    head: u32,
    /// Frames written, up to `frames`.
    count: u32,
}

impl Stack {
    /// Bytes a stack of this shape takes on the GPU.
    pub fn bytes_for(width: u32, height: u32, frames: u32, precision: Precision) -> u64 {
        u64::from(width) * u64::from(height) * u64::from(frames) * precision.bytes()
    }

    /// An empty stack with room for `frames` (1 to [`MAX_FRAMES`]). Sizes are checked against
    /// the device, and running out of GPU memory is an error, not a crash.
    pub fn new(gpu: &Gpu, width: u32, height: u32, frames: u32, precision: Precision) -> Result<Self, Error> {
        let max = gpu.max_texture_size();
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::new(format!("resolution {width}x{height} must be 1 to {max} on each side")));
        }
        let chunk = gpu.device().limits().max_texture_array_layers.clamp(1, CHUNK);
        let most = chunk * CHUNKS as u32;
        if frames == 0 || frames > most {
            return Err(Error::new(format!("a frame stack holds 1 to {most} frames, not {frames}")));
        }
        let (chunks, error) = gpu.scoped(|| {
            let mut chunks = Vec::new();
            let mut left = frames;
            while left > 0 {
                let layers = left.min(chunk);
                left -= layers;
                let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("frame stack"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: layers },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: precision.format(),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });
                chunks.push(Chunk { texture, view });
            }
            chunks
        });
        if let Some(e) = error {
            let mb = Self::bytes_for(width, height, frames, precision) >> 20;
            return Err(Error::new(format!(
                "cannot keep {frames} frames of {width}x{height} ({mb} MB): {e}"
            )));
        }
        Ok(Self { chunks, width, height, frames, chunk, precision, head: 0, count: 0 })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Frames it has room for.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Frames written so far, up to [`Stack::frames`].
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn precision(&self) -> Precision {
        self.precision
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        Self::bytes_for(self.width, self.height, self.frames, self.precision)
    }

    /// Forgets every frame (the memory stays).
    pub fn clear(&mut self) {
        self.count = 0;
    }

    /// Writes `src` (any size, averaged down or stretched to the stack's) as the newest frame,
    /// over the oldest once the stack is full.
    pub fn push(&mut self, gpu: &Gpu, src: &Tex) -> Result<(), Error> {
        let p = gpu.stack_pipelines()?;
        let next = if self.count == 0 { 0 } else { (self.head + 1) % self.frames };
        let chunk = self
            .chunks
            .get((next / self.chunk) as usize)
            .ok_or_else(|| Error::new("frame stack: layer out of range"))?;
        let device = gpu.device();
        let (sw, sh) = src.size();
        let values =
            [sw as f32 / self.width as f32, sh as f32 / self.height as f32, 1.0 / sw as f32, 1.0 / sh as f32];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let ((), error) = gpu.scoped(|| {
            let buffer = gpu.uniform_buffer("frame stack push", &bytes);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("frame stack push"),
                layout: &p.push_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src.view()),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&p.sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
                ],
            });
            let target = chunk.texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: next % self.chunk,
                array_layer_count: Some(1),
                ..Default::default()
            });
            let pipeline = match self.precision {
                Precision::Half => &p.push_half,
                Precision::Byte => &p.push_byte,
            };
            let mut encoder = device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame stack push") });
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("frame stack push"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(pipeline);
                rp.set_bind_group(0, &group, &[]);
                rp.draw(0..3, 0..1);
            }
            gpu.submit([encoder.finish()]);
        });
        if let Some(e) = error {
            return Err(Error::new(format!("frame stack: {e}")));
        }
        self.head = next;
        self.count = (self.count + 1).min(self.frames);
        Ok(())
    }

    /// Renders into `target` (any size): per pixel, the frame as many frames back as `look`
    /// makes of `map`'s luminance there (black, or no map, is the newest plus the offset),
    /// clamped to the oldest written. Nothing written yet gives transparent black.
    pub fn read(&self, gpu: &Gpu, map: Option<&Tex>, target: &Tex, look: Look) -> Result<(), Error> {
        let p = gpu.stack_pipelines()?;
        let device = gpu.device();
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let mut bytes = Vec::with_capacity(32);
        bytes.extend(finite(look.reach).to_le_bytes());
        bytes.extend(finite(look.offset).to_le_bytes());
        for v in [self.head, self.count, self.frames, self.chunk, u32::from(look.blend), 0] {
            bytes.extend(v.to_le_bytes());
        }
        let ((), error) = gpu.scoped(|| {
            let buffer = gpu.uniform_buffer("frame stack read", &bytes);
            let mut entries = vec![
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(map.map_or(&p.empty, Tex::view)),
                },
                wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
            ];
            for i in 0..CHUNKS {
                let view = self.chunks.get(i).map_or(&p.empty_array, |c| &c.view);
                entries.push(wgpu::BindGroupEntry {
                    binding: 2 + i as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: 2 + CHUNKS as u32,
                resource: wgpu::BindingResource::Sampler(&p.sampler),
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("frame stack read"),
                layout: &p.read_layout,
                entries: &entries,
            });
            let mut encoder = device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame stack read") });
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("frame stack read"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if self.count > 0 {
                    rp.set_pipeline(&p.read);
                    rp.set_bind_group(0, &group, &[]);
                    rp.draw(0..3, 0..1);
                }
            }
            gpu.submit([encoder.finish()]);
        });
        match error {
            Some(e) => Err(Error::new(format!("frame stack: {e}"))),
            None => Ok(()),
        }
    }
}

/// The stack's pipelines, one set per device.
pub struct Pipelines {
    push_layout: wgpu::BindGroupLayout,
    read_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    push_half: wgpu::RenderPipeline,
    push_byte: wgpu::RenderPipeline,
    read: wgpu::RenderPipeline,
    /// Stand-ins for no map and for chunks a stack does not have; transparent black.
    empty: wgpu::TextureView,
    empty_array: wgpu::TextureView,
}

impl Pipelines {
    fn new(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let (p, error) = gpu.scoped(|| {
            let texture = |binding, view_dimension| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension,
                    multisampled: false,
                },
                count: None,
            };
            let sampler = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            };
            let uniform = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            };
            let push_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame stack push"),
                entries: &[texture(0, wgpu::TextureViewDimension::D2), sampler(1), uniform(2)],
            });
            let mut entries = vec![texture(0, wgpu::TextureViewDimension::D2), uniform(1)];
            entries.extend((0..CHUNKS as u32).map(|i| texture(2 + i, wgpu::TextureViewDimension::D2Array)));
            entries.push(sampler(2 + CHUNKS as u32));
            let read_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame stack read"),
                entries: &entries,
            });
            let pipeline =
                |label: &str, source: &str, layout: &wgpu::BindGroupLayout, entry: &str, format| {
                    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some(label),
                        source: wgpu::ShaderSource::Wgsl(source.into()),
                    });
                    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    });
                    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some(label),
                        layout: Some(&pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &module,
                            entry_point: Some("vs"),
                            compilation_options: Default::default(),
                            buffers: &[],
                        },
                        primitive: wgpu::PrimitiveState::default(),
                        depth_stencil: None,
                        multisample: wgpu::MultisampleState::default(),
                        fragment: Some(wgpu::FragmentState {
                            module: &module,
                            entry_point: Some(entry),
                            compilation_options: Default::default(),
                            targets: &[Some(wgpu::ColorTargetState {
                                format,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            })],
                        }),
                        multiview_mask: None,
                        cache: None,
                    })
                };
            let push = include_str!("stack_push.wgsl");
            let read = include_str!("stack_read.wgsl");
            let empty = |label, dimension| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(dimension),
                        ..Default::default()
                    })
            };
            Self {
                push_half: pipeline(
                    "frame stack push",
                    push,
                    &push_layout,
                    "fs_push",
                    Precision::Half.format(),
                ),
                push_byte: pipeline(
                    "frame stack push",
                    push,
                    &push_layout,
                    "fs_push",
                    Precision::Byte.format(),
                ),
                read: pipeline("frame stack read", read, &read_layout, "fs_read", FORMAT),
                sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("frame stack"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                }),
                // wgpu zero-initialises resources: transparent black.
                empty: empty("frame stack no map", wgpu::TextureViewDimension::D2),
                empty_array: empty("frame stack no chunk", wgpu::TextureViewDimension::D2Array),
                push_layout,
                read_layout,
            }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot build the frame stack shaders: {e}"))),
            None => Ok(p),
        }
    }
}

impl Gpu {
    /// The frame stack pipelines, built once per device.
    pub fn stack_pipelines(&self) -> Result<Arc<Pipelines>, Error> {
        let mut slot = self.inner.stack.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = Arc::new(Pipelines::new(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;
    use crate::{Pass, PassInput};
    use penumbra_core::{ParamSpec, Value};

    /// A texture of one colour.
    fn flat(gpu: &Gpu, w: u32, h: u32, rgba: [f64; 4]) -> Tex {
        let pass = Pass::compile(
            gpu,
            "flat",
            "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return params.c; }",
            &[ParamSpec::vec4("c", [0.0; 4])],
        )
        .unwrap();
        let t = Tex::new(gpu, w, h, "flat").unwrap();
        let c = move |name: &str| (name == "c").then_some(Value::Vec4(rgba));
        pass.render(gpu, &t, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &c });
        t
    }

    fn red(gpu: &Gpu, t: &Tex, x: u32, y: u32) -> f32 {
        gpu.read(t).unwrap().get(x, y).unwrap()[0]
    }

    #[test]
    fn reads_frames_back_through_the_ring_and_across_chunks() {
        let Some(gpu) = gpu() else { return };
        let chunk = gpu.device().limits().max_texture_array_layers.clamp(1, CHUNK);
        // Two chunks plus a bit, so frames wrap and sit in different array textures.
        let frames = chunk + 3;
        let mut stack = Stack::new(&gpu, 4, 2, frames, Precision::Half).unwrap();
        let out = Tex::new(&gpu, 4, 2, "out").unwrap();
        let look = |offset: f32| Look { reach: 0.0, offset, blend: true };
        stack.read(&gpu, None, &out, look(0.0)).unwrap();
        assert_eq!(red(&gpu, &out, 0, 0), 0.0, "empty is transparent black");
        // Frame i has red i / 1024 (exact in f16 up to 2048 / 1024).
        let total = frames + 5;
        for i in 0..total {
            stack.push(&gpu, &flat(&gpu, 4, 2, [f64::from(i) / 1024.0, 0.0, 0.0, 1.0])).unwrap();
        }
        assert_eq!(stack.count(), frames);
        for back in [0, 1, 4, chunk, frames - 1] {
            stack.read(&gpu, None, &out, look(back as f32)).unwrap();
            let want = (total - 1 - back) as f32 / 1024.0;
            assert!((red(&gpu, &out, 1, 1) - want).abs() < 1e-4, "{back} back");
        }
        // Past the oldest is the oldest; halfway between two frames blends them.
        stack.read(&gpu, None, &out, look(1e9)).unwrap();
        assert!((red(&gpu, &out, 0, 0) - (total - frames) as f32 / 1024.0).abs() < 1e-4);
        stack.read(&gpu, None, &out, look(2.5)).unwrap();
        assert!((red(&gpu, &out, 0, 0) - (total as f32 - 3.5) / 1024.0).abs() < 1e-4);
        stack.read(&gpu, None, &out, Look { blend: false, ..look(2.75) }).unwrap();
        assert!((red(&gpu, &out, 0, 0) - (total - 4) as f32 / 1024.0).abs() < 1e-4);
        assert_eq!(gpu.errors().count, 0);
    }

    #[test]
    fn the_map_picks_per_pixel_and_bytes_average_down() {
        let Some(gpu) = gpu() else { return };
        let mut stack = Stack::new(&gpu, 2, 1, 3, Precision::Byte).unwrap();
        for r in [0.25, 0.5, 0.75] {
            // Twice the stack's size: averaged down.
            stack.push(&gpu, &flat(&gpu, 4, 2, [r, 0.0, 0.0, 1.0])).unwrap();
        }
        // A map black on the left, white on the right: newest there, two back here.
        let pass = Pass::compile(
            &gpu,
            "split",
            "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(vec3f(step(0.5, v.uv.x)), 1.0); }",
            &[],
        )
        .unwrap();
        let map = Tex::new(&gpu, 2, 1, "map").unwrap();
        pass.render(&gpu, &map, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &|_| None });
        let out = Tex::new(&gpu, 2, 1, "out").unwrap();
        stack.read(&gpu, Some(&map), &out, Look { reach: 2.0, offset: 0.0, blend: true }).unwrap();
        // 8-bit sRGB keeps linear values to within about half a percent at these levels.
        assert!((red(&gpu, &out, 0, 0) - 0.75).abs() < 0.005);
        assert!((red(&gpu, &out, 1, 0) - 0.25).abs() < 0.005);
        stack.clear();
        stack.read(&gpu, Some(&map), &out, Look { reach: 2.0, offset: 0.0, blend: true }).unwrap();
        assert_eq!(red(&gpu, &out, 1, 0), 0.0);
        assert_eq!(gpu.errors().count, 0);
    }

    #[test]
    fn hostile_shapes_are_errors() {
        let Some(gpu) = gpu() else { return };
        assert!(Stack::new(&gpu, 0, 4, 1, Precision::Half).is_err());
        assert!(Stack::new(&gpu, 4, 4, 0, Precision::Half).is_err());
        assert!(Stack::new(&gpu, 4, 4, MAX_FRAMES + 1, Precision::Half).is_err());
    }
}
