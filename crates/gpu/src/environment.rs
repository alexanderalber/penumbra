//! Environment lights: an equirectangular picture of everything around, baked once per picture into
//! what the render samples. Directions follow Blender's equirectangular convention in penumbra's z
//! up world: the picture's middle looks along +x, u grows clockwise seen from above, its top row is
//! straight up.
//!
//! Baked into [`EnvironmentMaps`]: a mip chain prefiltered for roughness (mip 0 the sharp
//! picture, the last mip roughness 1; GGX importance sampling, 64 samples per texel, from a
//! plain mip chain of the picture) for reflections, and a 32 x 16 irradiance map (the cosine
//! weighted sum over a small mip) for diffuse light.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use penumbra_core::{Error, lock};

use crate::{FORMAT, Gpu, Tex};

/// The smallest mip's height; below it a picture of the sky says nothing more.
const SMALLEST: u32 = 4;
/// The irradiance map's size.
const IRRADIANCE: (u32, u32) = (32, 16);
/// Widths a bake accepts.
pub const WIDTHS: (u32, u32) = (16, 4096);

/// An environment picture baked for the render ([`crate::render::Light::environment`]).
pub struct EnvironmentMaps {
    /// Prefiltered for roughness, all mips.
    pub(crate) specular: wgpu::TextureView,
    /// Light per normal, over pi.
    pub(crate) irradiance: wgpu::TextureView,
    /// Mips of `specular`.
    pub(crate) levels: u32,
    size: (u32, u32),
    /// Unique per bake, for comparing.
    id: u64,
}

impl std::fmt::Debug for EnvironmentMaps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentMaps").field("size", &self.size).field("levels", &self.levels).finish()
    }
}

impl PartialEq for EnvironmentMaps {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl EnvironmentMaps {
    /// The prefiltered chain's top mip size.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        let (w, h) = (u64::from(self.size.0), u64::from(self.size.1));
        // Two chains of about 4/3 of their top mip, plus the irradiance map.
        2 * w * h * 8 * 4 / 3 + u64::from(IRRADIANCE.0 * IRRADIANCE.1) * 8
    }

    /// Bakes `source` (equirectangular, any size; alpha ignored) at `width` x `width / 2` texels.
    pub fn bake(gpu: &Gpu, source: &Tex, width: u32) -> Result<Self, Error> {
        let width = width.clamp(WIDTHS.0, WIDTHS.1.min(gpu.max_texture_size()));
        let size = (width, (width / 2).max(1));
        let mut levels = 1;
        while (size.1 >> levels) >= SMALLEST && levels < 16 {
            levels += 1;
        }
        let p = gpu.environment_pipelines()?;
        let device = gpu.device();
        let (maps, error) = gpu.scoped(|| {
            let chain = |label: &str| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: levels,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            let plain = chain("environment plain");
            let specular = chain("environment specular");
            let irradiance = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("environment irradiance"),
                size: wgpu::Extent3d { width: IRRADIANCE.0, height: IRRADIANCE.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let mip = |t: &wgpu::Texture, k: u32| {
                t.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: k,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            };
            let all_plain = plain.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("environment") });
            let mut pass = |pipeline: &wgpu::RenderPipeline,
                            src: &wgpu::TextureView,
                            target: &wgpu::TextureView,
                            bake: [f32; 8]| {
                let bytes: Vec<u8> =
                    bake.iter().flat_map(|v| (if v.is_finite() { *v } else { 0.0 }).to_le_bytes()).collect();
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("environment bake"),
                    size: bytes.len() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.queue().write_buffer(&buffer, 0, &bytes);
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("environment bake"),
                    layout: &p.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&p.sampler),
                        },
                        wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
                    ],
                });
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("environment bake"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
            };
            // The plain chain: the source averaged down to mip 0, then halved mip by mip.
            let (sw, sh) = source.size();
            let footprint = [sw as f32 / size.0 as f32, sh as f32 / size.1 as f32];
            let src_texel = [1.0 / sw as f32, 1.0 / sh as f32];
            let base = [footprint[0], footprint[1], src_texel[0], src_texel[1], 0.0, 0.0, 0.0, 0.0];
            pass(&p.down, source.view(), &mip(&plain, 0), base);
            for k in 1..levels {
                let (w, h) = ((size.0 >> (k - 1)).max(1), (size.1 >> (k - 1)).max(1));
                let bake = [2.0, 2.0, 1.0 / w as f32, 1.0 / h as f32, 0.0, 0.0, 0.0, 0.0];
                pass(&p.down, &mip(&plain, k - 1), &mip(&plain, k), bake);
            }
            // The prefiltered chain: mip 0 sharp, then rougher mip by mip.
            pass(&p.down, source.view(), &mip(&specular, 0), base);
            let texels = size.0 as f32 * size.1 as f32;
            let lods = (levels - 1) as f32;
            for k in 1..levels {
                let roughness = k as f32 / lods.max(1.0);
                let bake = [0.0, 0.0, 0.0, 0.0, roughness, lods, texels, 0.0];
                pass(&p.prefilter, &all_plain, &mip(&specular, k), bake);
            }
            // Irradiance from the mip at most 16 texels high.
            let small = (0..levels).find(|k| (size.1 >> k) <= IRRADIANCE.1).unwrap_or(levels - 1);
            let target = irradiance.create_view(&wgpu::TextureViewDescriptor::default());
            pass(&p.irradiance, &mip(&plain, small), &target, [0.0; 8]);
            gpu.submit([encoder.finish()]);
            static NEXT: AtomicU64 = AtomicU64::new(1);
            EnvironmentMaps {
                specular: specular.create_view(&wgpu::TextureViewDescriptor::default()),
                irradiance: target,
                levels,
                size,
                id: NEXT.fetch_add(1, Ordering::Relaxed),
            }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot bake the environment: {e}"))),
            None => Ok(maps),
        }
    }
}

/// The baking pipelines, one set per device.
pub struct Pipelines {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    down: wgpu::RenderPipeline,
    prefilter: wgpu::RenderPipeline,
    irradiance: wgpu::RenderPipeline,
}

/// The sampler for equirectangular pictures: around in u, clamped at the poles, linear between
/// mips.
pub(crate) fn sampler(gpu: &Gpu, label: &str) -> wgpu::Sampler {
    gpu.device().create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    })
}

impl Pipelines {
    fn new(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let (p, error) = gpu.scoped(|| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("environment bake"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(32),
                        },
                        count: None,
                    },
                ],
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("environment"),
                source: wgpu::ShaderSource::Wgsl(include_str!("environment.wgsl").into()),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("environment"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = |entry: &str| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(entry),
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
                            format: FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            Self {
                sampler: sampler(gpu, "environment bake"),
                down: pipeline("fs_down"),
                prefilter: pipeline("fs_prefilter"),
                irradiance: pipeline("fs_irradiance"),
                layout,
            }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot build the environment shader: {e}"))),
            None => Ok(p),
        }
    }
}

impl Gpu {
    /// The environment baking pipelines, built once per device.
    pub fn environment_pipelines(&self) -> Result<Arc<Pipelines>, Error> {
        let mut slot = lock(&self.inner.environment);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = Arc::new(Pipelines::new(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}
