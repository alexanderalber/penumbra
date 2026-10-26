//! Mip chains for the textures a render samples: the surfaces' textures and the projector
//! pictures. Without them a texture much larger than it appears gives one texel per pixel, so
//! fine detail turns into stairs along its edges and shimmers when anything moves. The textures
//! flowing between nodes have a single level and are rendered into again from frame to frame,
//! so each render copies the ones it uses into chains of its own and halves them level by
//! level, every time. The chains of one render are kept for the next, by size.

use crate::{FORMAT, Gpu, Tex};

/// The pipeline that halves a level, and what it samples with.
pub(super) struct MipPipeline {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl MipPipeline {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render mips"),
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
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("render mips"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../mips.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("render mips"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render mips"),
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
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("render mips"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { layout, pipeline, sampler }
    }
}

/// Levels down to one texel along the longer side.
pub(super) fn levels(size: (u32, u32)) -> u32 {
    32 - size.0.max(size.1).max(1).leading_zeros()
}

/// One texture's chain: all levels for sampling, and each level alone to draw into.
struct Chain {
    size: (u32, u32),
    texture: wgpu::Texture,
    all: wgpu::TextureView,
    each: Vec<wgpu::TextureView>,
}

impl Chain {
    fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        let count = levels(size);
        let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("render mips"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let all = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let each = (0..count)
            .map(|k| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: k,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self { size, texture, all, each }
    }
}

/// The chains a renderer keeps between renders.
#[derive(Default)]
pub(super) struct Mips {
    chains: Vec<Chain>,
}

/// One render's textures, each with the view the render samples it through.
pub(super) struct Mipped {
    views: Vec<(Tex, wgpu::TextureView)>,
}

impl Mipped {
    /// The view to sample `t` through: its chain, or itself where it has none (a single texel).
    pub(super) fn view<'a>(&'a self, t: &'a Tex) -> &'a wgpu::TextureView {
        self.views.iter().find(|(x, _)| x.same(t)).map_or(t.view(), |(_, v)| v)
    }
}

impl Mips {
    /// Chains for `textures` (each once, however often it is named), encoded into `encoder`:
    /// a copy of the texture, then each level from the one above. Chains of the last render are
    /// taken again where the size fits; those left over are dropped.
    pub(super) fn update<'t>(
        &mut self,
        gpu: &Gpu,
        pipe: &MipPipeline,
        encoder: &mut wgpu::CommandEncoder,
        textures: impl IntoIterator<Item = &'t Tex>,
    ) -> Mipped {
        let device = gpu.device();
        let mut old = std::mem::take(&mut self.chains);
        let mut views: Vec<(Tex, wgpu::TextureView)> = Vec::new();
        for t in textures {
            let size = t.size();
            if levels(size) < 2 || views.iter().any(|(x, _)| x.same(t)) {
                continue;
            }
            let chain = match old.iter().position(|c| c.size == size) {
                Some(k) => old.swap_remove(k),
                None => Chain::new(gpu, size),
            };
            encoder.copy_texture_to_texture(
                t.texture().as_image_copy(),
                chain.texture.as_image_copy(),
                wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            );
            for pair in chain.each.windows(2) {
                let [above, level] = pair else { continue };
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("render mips"),
                    layout: &pipe.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(above),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&pipe.sampler),
                        },
                    ],
                });
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("render mips"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: level,
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
                rp.set_pipeline(&pipe.pipeline);
                rp.set_bind_group(0, &group, &[]);
                rp.draw(0..3, 0..1);
            }
            views.push((t.clone(), chain.all.clone()));
            self.chains.push(chain);
        }
        Mipped { views }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_reach_one_texel() {
        assert_eq!(levels((1, 1)), 1);
        assert_eq!(levels((2, 1)), 2);
        assert_eq!(levels((600, 840)), 10);
        assert_eq!(levels((1024, 16)), 11);
        assert_eq!(levels((0, 0)), 1);
    }
}
