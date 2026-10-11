//! Node output textures shown by egui, on the same device the cook thread renders with.
//!
//! egui treats what it samples from a texture as sRGB-encoded (gamma) and premultiplied, while
//! a [`Tex`] holds linear light. Shown directly, every picture came out too dark (a linear 0.2
//! showed as 51 of 255 instead of 124). So each shown texture gets a display copy: one pass
//! encodes it to 8-bit sRGB, premultiplied after encoding as egui blends, whenever the node's
//! texture changes. Values above 1 clip on screen. Each entry holds its `Tex`, so the node's
//! texture ring does not render into a texture still being copied.

use std::collections::HashMap;

use egui::TextureId;
use egui_wgpu::RenderState;
use penumbra_core::NodeId;
use penumbra_gpu::Tex;

/// One node output.
pub type Key = (NodeId, u32);

const DISPLAY: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r"
@group(0) @binding(0) var src: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn encode(v: vec3f) -> vec3f {
    let lo = v * 12.92;
    let hi = 1.055 * pow(v, vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, v <= vec3f(0.0031308));
}

@fragment
fn fs(@builtin(position) p: vec4f) -> @location(0) vec4f {
    let c = textureLoad(src, vec2i(p.xy), 0);
    let a = clamp(c.a, 0.0, 1.0);
    if (!(a > 0.0)) {
        return vec4f(0.0);
    }
    let straight = clamp(c.rgb / a, vec3f(0.0), vec3f(1.0));
    return vec4f(encode(straight) * a, a);
}
";

struct Entry {
    tex: Tex,
    display: wgpu::Texture,
    id: TextureId,
    used: bool,
}

struct Encoder {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl Encoder {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("display copy"),
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
            label: Some("display copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("display copy"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("display copy"),
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
                targets: &[Some(DISPLAY.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// Encodes `tex` into `display` (same size).
    fn encode(&self, render: &RenderState, tex: &Tex, display: &wgpu::Texture) {
        let device = &render.device;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display copy"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(tex.view()),
            }],
        });
        let view = display.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("display copy") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("display copy"),
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
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &group, &[]);
            rp.draw(0..3, 0..1);
        }
        render.queue.submit([encoder.finish()]);
    }
}

fn display_texture(device: &wgpu::Device, (width, height): (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("display copy"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DISPLAY,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

#[derive(Default)]
pub struct Textures {
    render: Option<RenderState>,
    encoder: Option<Encoder>,
    entries: HashMap<Key, Entry>,
}

impl Textures {
    pub fn new(render: Option<RenderState>) -> Self {
        let encoder = render.as_ref().map(|r| Encoder::new(&r.device));
        Self { render, encoder, entries: HashMap::new() }
    }

    /// The egui texture showing `tex` for `key`, registering or updating it as needed. `None`
    /// without a wgpu renderer.
    pub fn show(&mut self, key: Key, tex: &Tex) -> Option<TextureId> {
        let render = self.render.as_ref()?;
        let encoder = self.encoder.as_ref()?;
        if let Some(e) = self.entries.get_mut(&key) {
            e.used = true;
            if !e.tex.same(tex) {
                let size = e.display.size();
                if (size.width, size.height) != tex.size() {
                    e.display = display_texture(&render.device, tex.size());
                    let view = e.display.create_view(&wgpu::TextureViewDescriptor::default());
                    render.renderer.write().update_egui_texture_from_wgpu_texture(
                        &render.device,
                        &view,
                        wgpu::FilterMode::Linear,
                        e.id,
                    );
                }
                encoder.encode(render, tex, &e.display);
                e.tex = tex.clone();
            }
            return Some(e.id);
        }
        let display = display_texture(&render.device, tex.size());
        encoder.encode(render, tex, &display);
        let view = display.create_view(&wgpu::TextureViewDescriptor::default());
        let id =
            render.renderer.write().register_native_texture(&render.device, &view, wgpu::FilterMode::Linear);
        self.entries.insert(key, Entry { tex: tex.clone(), display, id, used: true });
        Some(id)
    }

    /// Frees every entry (another project opened).
    pub fn clear(&mut self) {
        for e in self.entries.values_mut() {
            e.used = false;
        }
        self.end_frame();
    }

    /// Frees what was not shown since the last call. Called once per frame, after drawing.
    pub fn end_frame(&mut self) {
        let Some(render) = self.render.as_ref() else { return };
        let mut stale = Vec::new();
        for (key, e) in &mut self.entries {
            if !e.used {
                stale.push(*key);
            }
            e.used = false;
        }
        if stale.is_empty() {
            return;
        }
        let mut renderer = render.renderer.write();
        for key in stale {
            if let Some(e) = self.entries.remove(&key) {
                renderer.free_texture(&e.id);
            }
        }
    }
}
