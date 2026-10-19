//! The colour conversion between penumbra's textures and Spout's ([`convert.wgsl`](../convert.wgsl)):
//! one fullscreen pass, its pipelines made once per target format and mode.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use penumbra_gpu::Gpu;

const SOURCE: &str = include_str!("convert.wgsl");

/// How alpha travels in the Spout texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Alpha {
    /// Colour and alpha separate, as most programs expect.
    #[default]
    Straight,
    /// Colour multiplied by alpha (after encoding).
    Premultiplied,
    /// No transparency: sending composites over black, receiving ignores the alpha.
    Opaque,
}

impl Alpha {
    pub const NAMES: [&'static str; 3] = ["straight", "premultiplied", "opaque"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "straight" => Self::Straight,
            "premultiplied" => Self::Premultiplied,
            "opaque" => Self::Opaque,
            _ => return None,
        })
    }

    fn constant(self) -> f64 {
        match self {
            Self::Straight => 0.0,
            Self::Premultiplied => 1.0,
            Self::Opaque => 2.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    ToSpout,
    FromSpout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    format: wgpu::TextureFormat,
    direction: Direction,
    alpha: Alpha,
}

#[derive(Default)]
pub struct Converter {
    layout: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout, wgpu::ShaderModule)>,
    pipelines: HashMap<Key, wgpu::RenderPipeline>,
}

impl Converter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the conversion of `src` into `dst` (same size) into `encoder`.
    pub fn encode(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::Texture,
        direction: Direction,
        alpha: Alpha,
    ) -> Result<(), String> {
        let key = Key { format: dst.format(), direction, alpha };
        let device = gpu.device();
        if self.layout.is_none() {
            let ((group, pipeline_layout, module), error) = gpu.scoped(|| {
                let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("spout convert"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    }],
                });
                let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("spout convert"),
                    bind_group_layouts: &[Some(&group)],
                    immediate_size: 0,
                });
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("spout convert"),
                    source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
                });
                (group, pipeline_layout, module)
            });
            if let Some(e) = error {
                return Err(format!("cannot build the Spout conversion: {e}"));
            }
            self.layout = Some((group, pipeline_layout, module));
        }
        let Some((group_layout, pipeline_layout, module)) = &self.layout else {
            return Err("the Spout conversion is missing".into());
        };
        if let Entry::Vacant(slot) = self.pipelines.entry(key) {
            let clamp =
                !matches!(key.format, wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float);
            let constants = [
                ("DIRECTION", if direction == Direction::ToSpout { 0.0 } else { 1.0 }),
                ("ALPHA", alpha.constant()),
                ("CLAMP", if clamp { 1.0 } else { 0.0 }),
            ];
            let (pipeline, error) = gpu.scoped(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("spout convert"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some("vs"),
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants: &constants,
                            ..Default::default()
                        },
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs"),
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants: &constants,
                            ..Default::default()
                        },
                        targets: &[Some(wgpu::ColorTargetState {
                            format: key.format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            });
            if let Some(e) = error {
                return Err(format!("cannot build the Spout conversion for {:?}: {e}", key.format));
            }
            slot.insert(pipeline);
        }
        let Some(pipeline) = self.pipelines.get(&key) else {
            return Err("the Spout conversion is missing".into());
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("spout convert"),
            layout: group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src),
            }],
        });
        let view = dst.create_view(&wgpu::TextureViewDescriptor::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("spout convert"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
