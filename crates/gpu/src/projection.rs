//! Draws projectors: each a textured triangle mesh (from the surface onto the projector image,
//! computed on the CPU by `ops::out::mapping`) inside its viewport, with soft-edge blending
//! and the alignment grid done per pixel in `projection.wgsl`, and a projector's colour
//! correction ([`Correction`]) last. Used by the output windows (into
//! their surfaces) and by the engine (into a [`Tex`], for the viewer and `node.snapshot`).

use std::collections::HashMap;
use std::sync::Arc;

use penumbra_core::Error;

use crate::{FORMAT, Gpu, Tex};

/// Most other projectors one projector blends with.
pub const MAX_PEERS: usize = 8;
/// Most samples per tone curve of a [`Correction`].
pub const MAX_CURVE: usize = 64;
const UNIFORM_SIZE: u64 = 304 + 16 * MAX_CURVE as u64;

/// A projector's colour correction, the last step before its signal: a matrix in linear light
/// (`drive = matrix * colour`, clamped to 0..1), then per channel a tone curve from the drive,
/// sRGB-encoded, to the signal. Without one the output encodes to sRGB as usual; an identity
/// correction gives the same signal (within rounding) at a little more cost, so callers pass
/// `None` for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Correction {
    /// Row by row.
    pub matrix: [[f32; 3]; 3],
    /// Per channel (red, green, blue) the signal at encoded drives `i / (n - 1)`, 2 to
    /// [`MAX_CURVE`] samples each (fewer: the straight line, more: cut).
    pub curves: [Vec<f32>; 3],
}

/// One vertex of a projector's mesh: its position in the projector's clip space (homogeneous,
/// so the surface is interpolated perspective-correctly) and its point on the surface.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshVertex {
    pub pos: [f32; 4],
    pub uv: [f32; 2],
}

/// How the pixels of one projector are coloured.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The part of the surface it shows, x, y, w, h in surface sizes.
    pub region: [f32; 4],
    /// Regions of the other projectors it blends with (at most [`MAX_PEERS`] are used).
    pub peers: Vec<[f32; 4]>,
    pub blend: bool,
    pub gamma: f32,
    /// The alignment grid in `tint` instead of the input.
    pub pattern: bool,
    /// The projector's test pattern (in its own pixels, edge and circle in `tint`) instead of
    /// anything else.
    pub test: bool,
    pub tint: [f32; 3],
    /// Colours the picture by how fine the surface is per projector pixel (`res.overlay`).
    pub overlay: Option<Overlay>,
    /// The projector's colour correction, applied to everything but `test` and `flat`.
    pub correction: Option<Arc<Correction>>,
    /// A flat signal (sRGB-encoded, 0..1) instead of anything else, uncorrected: what a
    /// measurement shows.
    pub flat: Option<[f32; 3]>,
}

/// The resolution overlay: green where a surface pixel covers about one projector pixel, red where
/// the surface is too coarse, blue where it is finer than anything shown. Measured per pixel from
/// the surface's size and how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlay {
    /// Multiplies the measured ratio: below 1 when a node upstream of the surface is coarser
    /// than the surface itself, so the overlay shows the weakest link.
    pub factor: f32,
    /// Target pixels per needed projector pixel, across and down (1 when the target is shown
    /// one to one).
    pub scale: [f32; 2],
    /// At most this everywhere: the target's own pixels per needed projector pixel when the
    /// target itself is too coarse (a mapper smaller than its projector), else `f32::MAX`.
    pub cap: f32,
}

/// One projector to draw.
pub struct Item<'a> {
    /// Identifies the geometry: the same key is uploaded once and reused.
    pub key: u64,
    pub tex: &'a Tex,
    pub vertices: &'a [MeshVertex],
    pub indices: &'a [u32],
    /// x, y, width, height in target pixels.
    pub viewport: [f32; 4],
    pub look: Look,
}

struct Geometry {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// Pipelines per target format and the uploaded meshes. One per thread that draws.
pub struct Projection {
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sampler: wgpu::Sampler,
    geometry: HashMap<u64, Geometry>,
    uniforms: Vec<wgpu::Buffer>,
}

impl Projection {
    pub fn new(gpu: &Gpu) -> Self {
        let sampler = gpu.device().create_sampler(&wgpu::SamplerDescriptor {
            label: Some("projection"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self { pipelines: Vec::new(), sampler, geometry: HashMap::new(), uniforms: Vec::new() }
    }

    fn pipeline(&mut self, gpu: &Gpu, format: wgpu::TextureFormat) -> Result<wgpu::RenderPipeline, Error> {
        if let Some((_, p)) = self.pipelines.iter().find(|(f, _)| *f == format) {
            return Ok(p.clone());
        }
        let device = gpu.device();
        let (pipeline, error) = gpu.scoped(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("projection"),
                source: wgpu::ShaderSource::Wgsl(include_str!("projection.wgsl").into()),
            });
            let attributes = [
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 16,
                    shader_location: 1,
                },
            ];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("projection"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 24,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the projection pipeline: {e}")));
        }
        self.pipelines.push((format, pipeline.clone()));
        Ok(pipeline)
    }

    fn uniform(&mut self, gpu: &Gpu, index: usize) -> Option<wgpu::Buffer> {
        while self.uniforms.len() <= index {
            self.uniforms.push(gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("projection"),
                size: UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        self.uniforms.get(index).cloned()
    }

    fn upload(&mut self, gpu: &Gpu, item: &Item<'_>) -> Option<()> {
        if self.geometry.contains_key(&item.key) {
            return Some(());
        }
        if item.vertices.is_empty() || item.indices.is_empty() {
            return None;
        }
        let mut vbytes = Vec::with_capacity(item.vertices.len() * 24);
        for v in item.vertices {
            for f in v.pos.iter().chain(&v.uv) {
                vbytes.extend_from_slice(&f.to_le_bytes());
            }
        }
        let ibytes: Vec<u8> = item.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let buffer = |bytes: &[u8], usage: wgpu::BufferUsages| {
            let b = gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("projection mesh"),
                size: bytes.len() as u64,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue().write_buffer(&b, 0, bytes);
            b
        };
        let geometry = Geometry {
            vertices: buffer(&vbytes, wgpu::BufferUsages::VERTEX),
            indices: buffer(&ibytes, wgpu::BufferUsages::INDEX),
            count: u32::try_from(item.indices.len()).ok()?,
        };
        self.geometry.insert(item.key, geometry);
        Some(())
    }

    /// Clears `target` (of `format`, `size` pixels) to black and draws every item. Meshes not
    /// drawn this time are dropped. Into a format that is not sRGB the shader encodes, unless
    /// `linear` (a [`Tex`], which stays linear light).
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        linear: bool,
        items: &[Item<'_>],
    ) -> Result<(), Error> {
        let pipeline = self.pipeline(gpu, format)?;
        let layout = pipeline.get_bind_group_layout(0);
        let encode = !format.is_srgb() && !linear;
        let mut draws = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let [x, y, w, h] = item.viewport;
            let x0 = x.max(0.0).floor();
            let y0 = y.max(0.0).floor();
            let x1 = (x + w).min(size.0 as f32).ceil();
            let y1 = (y + h).min(size.1 as f32).ceil();
            if !(x1 - x0 >= 1.0 && y1 - y0 >= 1.0) || self.upload(gpu, item).is_none() {
                continue;
            }
            let Some(buffer) = self.uniform(gpu, i) else { continue };
            let bytes = uniform_bytes(&item.look, item.tex.size(), item.viewport, encode);
            gpu.queue().write_buffer(&buffer, 0, &bytes);
            let group = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("projection"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(item.tex.view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
                ],
            });
            let scissor = [x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32];
            draws.push((item.key, item.viewport, scissor, group));
        }
        self.geometry.retain(|k, _| items.iter().any(|i| i.key == *k));
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("projection") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("projection"),
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
            rp.set_pipeline(&pipeline);
            for (key, [x, y, w, h], [sx, sy, sw, sh], group) in &draws {
                let Some(g) = self.geometry.get(key) else { continue };
                rp.set_viewport(*x, *y, *w, *h, 0.0, 1.0);
                rp.set_scissor_rect(*sx, *sy, *sw, *sh);
                rp.set_bind_group(0, group, &[]);
                rp.set_vertex_buffer(0, g.vertices.slice(..));
                rp.set_index_buffer(g.indices.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..g.count, 0, 0..1);
            }
        }
        gpu.submit([encoder.finish()]);
        Ok(())
    }

    /// Draws `items` into `target` (linear light, as every texture).
    pub fn render(&mut self, gpu: &Gpu, target: &Tex, items: &[Item<'_>]) -> Result<(), Error> {
        self.draw(gpu, target.view(), FORMAT, target.size(), true, items)
    }
}

fn uniform_bytes(look: &Look, tex: (u32, u32), viewport: [f32; 4], encode: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(UNIFORM_SIZE as usize);
    let mut f = |v: f32| b.extend_from_slice(&v.to_le_bytes());
    for v in look.region {
        f(v);
    }
    for v in look.tint {
        f(v);
    }
    f(1.0);
    f(tex.0 as f32);
    f(tex.1 as f32);
    f(if look.gamma.is_finite() && look.gamma > 0.0 { look.gamma } else { 1.0 });
    let flags = u32::from(encode)
        | (u32::from(look.blend) << 1)
        | (u32::from(look.pattern) << 2)
        | (u32::from(look.test) << 3)
        | (u32::from(look.overlay.is_some()) << 4)
        | (u32::from(look.flat.is_some()) << 5)
        | (u32::from(look.correction.is_some() && look.flat.is_none() && !look.test) << 6);
    let peers: Vec<[f32; 4]> = look.peers.iter().take(MAX_PEERS).copied().collect();
    let finite = |v: f32| if v.is_finite() { v } else { 1.0 };
    let overlay =
        look.overlay.map_or([1.0; 3], |o| [finite(o.factor), finite(o.scale[0]), finite(o.scale[1])]);
    for v in [flags, peers.len() as u32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in overlay {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for p in &peers {
        for v in p {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    // Offsets as in projection.wgsl's Look: viewport at 192, overlay_cap at 208, the flat
    // signal and the correction from 224.
    b.resize(192, 0);
    for v in viewport {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let cap = look.overlay.map_or(f32::MAX, |o| if o.cap.is_nan() { f32::MAX } else { o.cap });
    b.extend_from_slice(&cap.to_le_bytes());
    b.resize(224, 0);
    let unit = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let flat = look.flat.unwrap_or([0.0; 3]);
    for v in flat {
        b.extend_from_slice(&unit(v).to_le_bytes());
    }
    b.extend_from_slice(&0f32.to_le_bytes());
    let identity = Correction {
        matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        curves: [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
    };
    let c = look.correction.as_deref().unwrap_or(&identity);
    for row in c.matrix {
        for v in row {
            let v = if v.is_finite() { v } else { 0.0 };
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&0f32.to_le_bytes());
    }
    let curves = c.curves.clone().map(|mut curve| {
        if curve.len() < 2 {
            curve = vec![0.0, 1.0];
        }
        curve.truncate(MAX_CURVE);
        curve
    });
    for curve in &curves {
        b.extend_from_slice(&(curve.len() as u32).to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    for i in 0..MAX_CURVE {
        for curve in &curves {
            b.extend_from_slice(&unit(curve.get(i).copied().unwrap_or(0.0)).to_le_bytes());
        }
        b.extend_from_slice(&0f32.to_le_bytes());
    }
    b.resize(UNIFORM_SIZE as usize, 0);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    /// A full-screen quad: the unit square onto the whole target, the surface's `region`.
    fn quad(region: [f32; 4]) -> (Vec<MeshVertex>, Vec<u32>) {
        let [x, y, w, h] = region;
        let v = |px: f32, py: f32| MeshVertex {
            pos: [px * 2.0 - 1.0, 1.0 - py * 2.0, 0.0, 1.0],
            uv: [x + px * w, y + py * h],
        };
        (vec![v(0.0, 0.0), v(1.0, 0.0), v(0.0, 1.0), v(1.0, 1.0)], vec![0, 1, 3, 0, 3, 2])
    }

    fn white(gpu: &Gpu) -> Tex {
        let tex = Tex::new(gpu, 64, 16, "white").unwrap();
        let source = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(1.0); }";
        let pass = crate::Pass::compile(gpu, "white", source, &[]).unwrap();
        pass.render(gpu, &tex, &crate::PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &|_| None });
        tex
    }

    #[test]
    fn two_projectors_add_up_to_white_across_the_overlap() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let a = [0.0, 0.0, 0.6, 1.0];
        let b = [0.4, 0.0, 0.6, 1.0];
        let mut drawer = Projection::new(&gpu);
        let mut shot = |key: u64, own: [f32; 4], peer: [f32; 4]| {
            let target = Tex::new(&gpu, 120, 8, "projector").unwrap();
            let (vertices, indices) = quad(own);
            let item = Item {
                key,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [0.0, 0.0, 120.0, 8.0],
                look: Look {
                    region: own,
                    peers: vec![peer],
                    blend: true,
                    gamma: 1.0,
                    pattern: false,
                    test: false,
                    overlay: None,
                    tint: [1.0; 3],
                    correction: None,
                    flat: None,
                },
            };
            drawer.render(&gpu, &target, &[item]).unwrap();
            gpu.read(&target).unwrap()
        };
        let pa = shot(1, a, b);
        let pb = shot(2, b, a);
        // Projector pixel x shows surface 0.6 * (x + 0.5) / 120 (a) and 0.4 + the same (b).
        for k in 0..30 {
            let s = 0.405 + 0.19 * k as f32 / 29.0;
            let xa = (s / 0.6 * 120.0 - 0.5).round() as u32;
            let xb = ((s - 0.4) / 0.6 * 120.0 - 0.5).round() as u32;
            let va = pa.get(xa, 4).unwrap()[0];
            let vb = pb.get(xb, 4).unwrap()[0];
            assert!((va + vb - 1.0).abs() < 0.03, "surface {s}: {va} + {vb}");
        }
        assert!((pa.get(10, 4).unwrap()[0] - 1.0).abs() < 1e-3);
        assert!(pa.get(119, 4).unwrap()[0] < 0.02);
        assert!(pb.get(0, 4).unwrap()[0] < 0.02);
    }

    #[test]
    fn test_pattern_in_the_projectors_own_pixels() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let mut drawer = Projection::new(&gpu);
        let target = Tex::new(&gpu, 200, 90, "projector").unwrap();
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        let look = Look {
            region: [0.0, 0.0, 1.0, 1.0],
            peers: vec![],
            blend: false,
            gamma: 1.0,
            pattern: false,
            test: true,
            overlay: None,
            tint: [1.0, 0.0, 0.0],
            correction: None,
            flat: None,
        };
        // The right half of the target is this projector's viewport.
        let item = Item {
            key: 3,
            tex: &src,
            vertices: &vertices,
            indices: &indices,
            viewport: [100.0, 0.0, 100.0, 90.0],
            look,
        };
        drawer.render(&gpu, &target, &[item]).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(50, 45).unwrap()[0], 0.0, "outside the viewport");
        assert_eq!(px.get(101, 45).unwrap()[..3], [1.0, 0.0, 0.0], "its left edge in its colour");
        assert_eq!(px.get(150, 45).unwrap()[..3], [1.0; 3], "the centre cross");
        // Cells are 11.25 px square around the centre (150, 45): a grid line 11.25 px left of it.
        assert!((px.get(138, 30).unwrap()[0] - 0.35).abs() < 1e-3);
    }

    #[test]
    fn viewport_and_encoding() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let mut drawer = Projection::new(&gpu);
        let target = Tex::new(&gpu, 40, 20, "projector").unwrap();
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        let look = Look {
            region: [0.0, 0.0, 1.0, 1.0],
            peers: vec![],
            blend: true,
            gamma: 1.0,
            pattern: false,
            test: false,
            overlay: None,
            tint: [1.0; 3],
            correction: None,
            flat: None,
        };
        let item = Item {
            key: 9,
            tex: &src,
            vertices: &vertices,
            indices: &indices,
            viewport: [20.0, 0.0, 20.0, 20.0],
            look,
        };
        drawer.render(&gpu, &target, &[item]).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(5, 10).unwrap()[0], 0.0);
        assert!((px.get(30, 10).unwrap()[0] - 1.0).abs() < 1e-3);
        // Nothing to draw (a viewport outside the target) still clears.
        drawer.render(&gpu, &target, &[]).unwrap();
        assert_eq!(gpu.read(&target).unwrap().get(30, 10).unwrap()[0], 0.0);
        let bytes = uniform_bytes(
            &Look {
                region: [0.0; 4],
                peers: vec![[1.0; 4]; 12],
                blend: true,
                gamma: 1.0,
                pattern: true,
                test: false,
                overlay: None,
                tint: [1.0; 3],
                correction: None,
                flat: None,
            },
            (4, 4),
            [0.0; 4],
            true,
        );
        assert_eq!(bytes.len(), UNIFORM_SIZE as usize);
    }

    #[test]
    fn correction_and_flat_signal() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let mut drawer = Projection::new(&gpu);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        let mut shot = |correction: Option<Correction>, flat: Option<[f32; 3]>| {
            let target = Tex::new(&gpu, 8, 8, "projector").unwrap();
            let look = Look {
                region: [0.0, 0.0, 1.0, 1.0],
                peers: vec![],
                blend: false,
                gamma: 1.0,
                pattern: false,
                test: false,
                overlay: None,
                tint: [1.0; 3],
                correction: correction.map(Arc::new),
                flat,
            };
            let item = Item {
                key: 5,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [0.0, 0.0, 8.0, 8.0],
                look,
            };
            drawer.render(&gpu, &target, &[item]).unwrap();
            let px = gpu.read(&target).unwrap();
            let p = px.get(4, 4).unwrap();
            [p[0], p[1], p[2]]
        };
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let straight = || [vec![0.0, 1.0], vec![0.0, 0.5, 1.0], vec![0.0; 1]];
        let plain = shot(None, None);
        let same = shot(Some(Correction { matrix: identity, curves: straight() }), None);
        for (a, b) in plain.iter().zip(same) {
            assert!((a - b).abs() < 2e-3, "{plain:?} {same:?}");
        }
        // Half the drive in linear light, the straight curve: half the light.
        let half =
            shot(Some(Correction { matrix: identity.map(|r| r.map(|v| v * 0.5)), curves: straight() }), None);
        assert!(half.iter().all(|v| (v - 0.5).abs() < 2e-3), "{half:?}");
        // Red's curve halves its signal: full drive is signal 0.5, light 0.214 on an sRGB display.
        let curved = shot(
            Some(Correction { matrix: identity, curves: [vec![0.0, 0.5], vec![0.0, 1.0], vec![0.0, 1.0]] }),
            None,
        );
        assert!((curved[0] - 0.2140).abs() < 2e-3 && (curved[1] - 1.0).abs() < 2e-3, "{curved:?}");
        // A flat signal bypasses the correction.
        let flat = shot(
            Some(Correction { matrix: identity.map(|r| r.map(|v| v * 0.5)), curves: straight() }),
            Some([0.5, 0.0, 1.0]),
        );
        assert!(
            (flat[0] - 0.2140).abs() < 2e-3 && flat[1].abs() < 1e-3 && (flat[2] - 1.0).abs() < 2e-3,
            "{flat:?}"
        );
    }
}
