//! Draws projectors: each a textured triangle mesh (from the surface onto the projector image,
//! computed on the CPU by `ops::out::mapping`) inside its viewport, with soft-edge blending
//! and the alignment grid done per pixel in `projection.wgsl`, and a projector's colour
//! correction ([`Correction`]) last. Used by the output windows (into
//! their surfaces) and by the engine (into a [`Tex`], for the viewer and `node.snapshot`).

use std::collections::HashMap;
use std::sync::Arc;

use penumbra_core::Error;

use crate::{FORMAT, Gpu, Tex};

/// The shader, after the helpers the built-in shaders share.
const SOURCE: &str = concat!(include_str!("common.wgsl"), include_str!("projection.wgsl"));

/// Most other projectors one projector blends with.
pub const MAX_PEERS: usize = 8;
/// Most samples per tone curve of a [`Correction`].
pub const MAX_CURVE: usize = 64;
/// Where projection.wgsl's `Look` has `viewport`, `flat` and `curve`, and its size: the packing
/// pads up to these (a test reads them from the shader).
const VIEWPORT_AT: usize = 192;
const FLAT_AT: usize = 224;
const CURVE_AT: u64 = 304;
const UNIFORM_SIZE: u64 = CURVE_AT + 16 * MAX_CURVE as u64 + 32;

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
    /// The projector's test pattern (in its own pixels, drawn in `tint`) instead of anything
    /// else.
    pub test: Option<Test>,
    pub tint: [f32; 3],
    /// Colours the picture by how fine the surface is per projector pixel (`res.overlay`).
    pub overlay: Option<Overlay>,
    /// The projector's colour correction, applied to everything but `test` and `flat`.
    pub correction: Option<Arc<Correction>>,
    /// How much of the item's gain map ([`Item::gain`]) applies: 0 none, 1 all of it.
    pub flatten: f32,
    /// How much of the item's black lift ([`Item::black`]) applies: 0 none, 1 as measured.
    pub lift: f32,
    /// A flat signal (sRGB-encoded, 0..1) instead of anything else, uncorrected: what a
    /// measurement shows.
    pub flat: Option<[f32; 3]>,
    /// A structured light pattern instead of anything but `flat`, uncorrected: what a scan
    /// shows.
    pub stripes: Option<Stripes>,
}

/// Which test pattern a projector shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TestPattern {
    /// Square cells, centre cross and circle, the image's edge: keystone, overscan, alignment.
    #[default]
    Align,
    /// One-pixel checkers and lines in patches at the corners and the centre, where defocus
    /// shows first, over a sparse one-pixel grid.
    Focus,
}

/// A projector's test pattern and its rank among the projectors. Everything is drawn in the
/// projector's colour, so where projectors overlap each one's marks stay apart (by colour), and
/// the focus patches move by rank, so stacked projectors put them side by side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Test {
    pub pattern: TestPattern,
    pub rank: u32,
}

/// One picture of a structured light scan in a projector's own pixels, in the order and with the
/// stripes of `penumbra_calib::scan::Patterns` (and `tex.scan_pattern`): 0 white, 1 black, then
/// per column bit (most significant first) its Gray-code stripes and their inverse, then the
/// rows; black past the last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stripes {
    pub pattern: u32,
    /// The signal of the lit stripes (sRGB-encoded, 0..1).
    pub level: f32,
    /// The projector's size in pixels the stripes are counted in; the viewport is divided into
    /// that many (one to one when it is the viewport's own size).
    pub size: [u32; 2],
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
    /// Per projector pixel (the viewport stretched over it) a factor per channel on the
    /// corrected drive, in linear light, after the matrix and before the tone curves: evens out
    /// brightness and colour across the projector's picture.
    pub gain: Option<&'a Tex>,
    /// Per projector pixel a lift of the black per channel, in the same drive after the gain:
    /// `drive = lift + (1 - lift) drive`, so black outside an overlap matches the black inside.
    pub black: Option<&'a Tex>,
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
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
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
            let bytes = uniform_bytes(
                &item.look,
                item.tex.size(),
                item.viewport,
                encode,
                item.gain.is_some(),
                item.black.is_some(),
            );
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
                    // Without a map the input stands in; the shader does not read it then.
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(item.gain.unwrap_or(item.tex).view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(item.black.unwrap_or(item.tex).view()),
                    },
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

fn uniform_bytes(
    look: &Look,
    tex: (u32, u32),
    viewport: [f32; 4],
    encode: bool,
    gain: bool,
    black: bool,
) -> Vec<u8> {
    let amount = |v: f32| if v.is_finite() { v.clamp(0.0, 4.0) } else { 0.0 };
    let gain = gain && amount(look.flatten) > 0.0;
    let black = black && amount(look.lift) > 0.0;
    let mut b = Vec::with_capacity(UNIFORM_SIZE as usize);
    let mut f = |v: f32| b.extend_from_slice(&v.to_le_bytes());
    for v in look.region {
        f(v);
    }
    for v in look.tint {
        f(v);
    }
    // The test pattern's rank, where the shader's `tint.w` has room for it.
    f(look.test.map_or(0, |t| t.rank.min(1 << 20)) as f32);
    f(tex.0 as f32);
    f(tex.1 as f32);
    f(if look.gamma.is_finite() && look.gamma > 0.0 { look.gamma } else { 1.0 });
    let flags = u32::from(encode)
        | (u32::from(look.blend) << 1)
        | (u32::from(look.pattern) << 2)
        | (u32::from(look.test.is_some()) << 3)
        | (u32::from(look.overlay.is_some()) << 4)
        | (u32::from(look.flat.is_some()) << 5)
        | (u32::from(
            (look.correction.is_some() || gain || black)
                && look.flat.is_none()
                && look.stripes.is_none()
                && look.test.is_none(),
        ) << 6)
        | (u32::from(look.stripes.is_some() && look.flat.is_none()) << 7)
        | (u32::from(gain) << 8)
        | (u32::from(black) << 9)
        | (u32::from(look.test.is_some_and(|t| t.pattern == TestPattern::Focus)) << 10);
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
    // Offsets as in projection.wgsl's Look: the viewport, then overlay_cap; the flat signal and
    // the correction.
    b.resize(VIEWPORT_AT, 0);
    for v in viewport {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let cap = look.overlay.map_or(f32::MAX, |o| if o.cap.is_nan() { f32::MAX } else { o.cap });
    b.extend_from_slice(&cap.to_le_bytes());
    b.resize(FLAT_AT, 0);
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
    // The stripes after the curves: projector width and height, level, pattern.
    let stripes = look.stripes.unwrap_or(Stripes { pattern: 0, level: 0.0, size: [1, 1] });
    for v in [
        stripes.size[0].max(1) as f32,
        stripes.size[1].max(1) as f32,
        unit(stripes.level),
        stripes.pattern.min(1 << 20) as f32,
    ] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // Then how much of the gain map and of the black lift apply.
    for v in [amount(look.flatten).min(1.0), amount(look.lift), 0.0, 0.0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.resize(UNIFORM_SIZE as usize, 0);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{f32_at, gpu, member, u32_at, wgsl_struct};

    /// The packing of `Look` matches projection.wgsl's struct: its size, the arrays' lengths, and
    /// every member's offset (each gets a value of its own, read back where naga puts it).
    #[test]
    fn uniform_layout_matches_the_shader() {
        let (span, m) = wgsl_struct(SOURCE, "Look");
        assert_eq!(u64::from(span), UNIFORM_SIZE);
        assert_eq!(member(&m, "peers").len, Some(MAX_PEERS as u32));
        assert_eq!(member(&m, "curve").len, Some(MAX_CURVE as u32));
        assert_eq!(member(&m, "viewport").offset as usize, VIEWPORT_AT);
        assert_eq!(member(&m, "flat").offset as usize, FLAT_AT);
        assert_eq!(u64::from(member(&m, "curve").offset), CURVE_AT);
        let look = Look {
            region: [0.125, 0.25, 0.5, 0.625],
            peers: vec![[0.75, 0.0, 0.0, 0.0]],
            blend: true,
            gamma: 2.5,
            pattern: false,
            test: None,
            tint: [0.375, 0.0, 0.0],
            overlay: Some(Overlay { factor: 3.0, scale: [5.0, 6.0], cap: 7.0 }),
            correction: Some(Arc::new(Correction {
                matrix: [[0.5, 0.0, 0.0], [0.25, 0.0, 0.0], [0.125, 0.0, 0.0]],
                curves: [vec![0.0, 0.5, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
            })),
            flatten: 0.875,
            lift: 0.5,
            flat: Some([0.0625, 0.0, 0.0]),
            stripes: Some(Stripes { pattern: 9, level: 0.5, size: [11, 12] }),
        };
        let b = uniform_bytes(&look, (13, 14), [15.0, 16.0, 17.0, 18.0], false, true, true);
        assert_eq!(b.len() as u64, UNIFORM_SIZE);
        let f = |name: &str| f32_at(&b, member(&m, name).offset);
        assert_eq!(f("region"), 0.125);
        assert_eq!(f("tint"), 0.375);
        assert_eq!(f("tex_size"), 13.0);
        assert_eq!(f("gamma"), 2.5);
        assert_eq!(u32_at(&b, member(&m, "flags").offset) & 2, 2);
        assert_eq!(u32_at(&b, member(&m, "peer_count").offset), 1);
        assert_eq!(f("overlay_factor"), 3.0);
        assert_eq!(f("overlay_scale"), 5.0);
        assert_eq!(f("peers"), 0.75);
        assert_eq!(f("viewport"), 15.0);
        assert_eq!(f("overlay_cap"), 7.0);
        assert_eq!(f("flat"), 0.0625);
        assert_eq!(f("m0"), 0.5);
        assert_eq!(f("m1"), 0.25);
        assert_eq!(f("m2"), 0.125);
        assert_eq!(u32_at(&b, member(&m, "counts").offset), 3);
        assert_eq!(f32_at(&b, member(&m, "curve").offset + 16), 0.5);
        assert_eq!(f("stripes"), 11.0);
        assert_eq!(f("field"), 0.875);
    }

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
        pass.render(gpu, &tex, &crate::PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &|_| None })
            .unwrap();
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
                    test: None,
                    overlay: None,
                    tint: [1.0; 3],
                    correction: None,
                    flatten: 1.0,
                    lift: 1.0,
                    flat: None,
                    stripes: None,
                },
                gain: None,
                black: None,
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
            test: Some(Test::default()),
            overlay: None,
            tint: [1.0, 0.0, 0.0],
            correction: None,
            flatten: 1.0,
            lift: 1.0,
            flat: None,
            stripes: None,
        };
        // The right half of the target is this projector's viewport.
        let item = Item {
            key: 3,
            tex: &src,
            vertices: &vertices,
            indices: &indices,
            viewport: [100.0, 0.0, 100.0, 90.0],
            look,
            gain: None,
            black: None,
        };
        drawer.render(&gpu, &target, &[item]).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(50, 45).unwrap()[0], 0.0, "outside the viewport");
        assert_eq!(px.get(101, 45).unwrap()[..3], [1.0, 0.0, 0.0], "its left edge in its colour");
        assert_eq!(px.get(150, 45).unwrap()[..3], [1.0, 0.0, 0.0], "the centre cross, in its colour too");
        // Cells are 11.25 px square around the centre (150, 45): a grid line 11.25 px left of it.
        assert!((px.get(138, 30).unwrap()[0] - 0.45).abs() < 1e-3);
        assert_eq!(px.get(138, 30).unwrap()[1], 0.0);
    }

    #[test]
    fn focus_pattern_in_one_pixel_steps_moved_by_rank() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        let shot = |rank: u32| {
            let target = Tex::new(&gpu, 200, 90, "projector").unwrap();
            let look = Look {
                region: [0.0, 0.0, 1.0, 1.0],
                peers: vec![],
                blend: false,
                gamma: 1.0,
                pattern: false,
                test: Some(Test { pattern: TestPattern::Focus, rank }),
                overlay: None,
                tint: [0.0, 1.0, 0.0],
                correction: None,
                flatten: 1.0,
                lift: 1.0,
                flat: None,
                stripes: None,
            };
            let item = Item {
                key: 4,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [100.0, 0.0, 100.0, 90.0],
                look,
                gain: None,
                black: None,
            };
            Projection::new(&gpu).render(&gpu, &target, &[item]).unwrap();
            gpu.read(&target).unwrap()
        };
        let g = |px: &crate::Pixels, x: u32, y: u32| px.get(x, y).unwrap()[1];
        let a = shot(0);
        // 100 x 90: patches 10 px square, 6 px in from the corners; the top left one starts with
        // its one-pixel checker at (106, 6), framed two pixels out.
        assert_eq!([g(&a, 106, 6), g(&a, 107, 6), g(&a, 107, 7)], [1.0, 0.0, 1.0], "one-pixel checker");
        assert_eq!([g(&a, 104, 6), g(&a, 105, 6)], [1.0, 0.0], "the frame and its gap");
        assert_eq!([g(&a, 100, 40), g(&a, 101, 40), g(&a, 102, 40)], [1.0, 0.0, 1.0], "the edge's two lines");
        assert_eq!(a.get(106, 6).unwrap()[0], 0.0, "in its colour only");
        // The second projector's patches sit 12 px lower.
        let b = shot(1);
        assert_eq!([g(&b, 106, 18), g(&b, 107, 18), g(&b, 107, 19)], [1.0, 0.0, 1.0]);
        assert_eq!(g(&b, 107, 7), 0.0);
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
            test: None,
            overlay: None,
            tint: [1.0; 3],
            correction: None,
            flatten: 1.0,
            lift: 1.0,
            flat: None,
            stripes: None,
        };
        let item = Item {
            key: 9,
            tex: &src,
            vertices: &vertices,
            indices: &indices,
            viewport: [20.0, 0.0, 20.0, 20.0],
            look,
            gain: None,
            black: None,
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
                test: None,
                overlay: None,
                tint: [1.0; 3],
                correction: None,
                flatten: 1.0,
                lift: 1.0,
                flat: None,
                stripes: None,
            },
            (4, 4),
            [0.0; 4],
            true,
            false,
            false,
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
                test: None,
                overlay: None,
                tint: [1.0; 3],
                correction: correction.map(Arc::new),
                flatten: 1.0,
                lift: 1.0,
                flat,
                stripes: None,
            };
            let item = Item {
                key: 5,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [0.0, 0.0, 8.0, 8.0],
                look,
                gain: None,
                black: None,
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
    /// A texture of `rgb(uv)` written as WGSL.
    fn painted(gpu: &Gpu, w: u32, h: u32, rgb: &str) -> Tex {
        let tex = Tex::new(gpu, w, h, "painted").unwrap();
        let source = format!(
            "@fragment fn main(v: PnVertex) -> @location(0) vec4f {{ let uv = v.uv; return vec4f({rgb}, 1.0); }}"
        );
        let pass = crate::Pass::compile(gpu, "painted", &source, &[]).unwrap();
        pass.render(gpu, &tex, &crate::PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &|_| None })
            .unwrap();
        tex
    }

    #[test]
    fn gain_map_and_black_lift_in_the_projectors_pixels() {
        let Some(gpu) = gpu() else { return };
        // Input: mid grey on the left half of the surface, black on the right.
        let src = painted(&gpu, 16, 16, "vec3f(select(0.0, 0.5, uv.x < 0.5))");
        // Gain: 0.5 in red over the top half of the projector, 1 elsewhere. Black lift: 0.1 in
        // blue over the bottom half.
        let gain = painted(&gpu, 4, 4, "vec3f(select(1.0, 0.5, uv.y < 0.5), 1.0, 1.0)");
        let black = painted(&gpu, 4, 4, "vec3f(0.0, 0.0, select(0.0, 0.1, uv.y > 0.5))");
        let mut drawer = Projection::new(&gpu);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        let mut shot = |gain_on: Option<&Tex>, black_on: Option<&Tex>, flatten: f32, lift: f32| {
            // The projector is the right half of the target: its own pixels decide, not the target's.
            let target = Tex::new(&gpu, 16, 8, "projector").unwrap();
            let look = Look {
                region: [0.0, 0.0, 1.0, 1.0],
                peers: vec![],
                blend: false,
                gamma: 1.0,
                pattern: false,
                test: None,
                overlay: None,
                tint: [1.0; 3],
                correction: None,
                flatten,
                lift,
                flat: None,
                stripes: None,
            };
            let item = Item {
                key: 7,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [8.0, 0.0, 8.0, 8.0],
                look,
                gain: gain_on,
                black: black_on,
            };
            drawer.render(&gpu, &target, &[item]).unwrap();
            gpu.read(&target).unwrap()
        };
        let at = |px: &crate::Pixels, x: u32, y: u32| {
            let p = px.get(x, y).unwrap();
            [p[0], p[1], p[2]]
        };
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 3e-3);
        let px = shot(Some(&gain), Some(&black), 1.0, 1.0);
        // Top left: grey, red halved in linear light.
        assert!(near(at(&px, 9, 1), [0.25, 0.5, 0.5]), "{:?}", at(&px, 9, 1));
        // Bottom right: black, blue lifted to a tenth of full drive.
        assert!(near(at(&px, 14, 6), [0.0, 0.0, 0.1]), "{:?}", at(&px, 14, 6));
        // Bottom left: grey lifted, 0.1 + 0.9 * 0.5.
        assert!(near(at(&px, 9, 6), [0.5, 0.5, 0.55]), "{:?}", at(&px, 9, 6));
        // Half of the gain, double the lift.
        let px = shot(Some(&gain), Some(&black), 0.5, 2.0);
        assert!(near(at(&px, 9, 1), [0.375, 0.5, 0.5]), "{:?}", at(&px, 9, 1));
        assert!(near(at(&px, 14, 6), [0.0, 0.0, 0.2]), "{:?}", at(&px, 14, 6));
        // Off by amount or by having no map: the plain picture.
        for px in [shot(Some(&gain), Some(&black), 0.0, 0.0), shot(None, None, 1.0, 1.0)] {
            assert!(near(at(&px, 9, 1), [0.5; 3]) && near(at(&px, 14, 6), [0.0; 3]));
        }
    }

    #[test]
    fn stripes_in_the_projectors_pixels_uncorrected() {
        let Some(gpu) = gpu() else { return };
        let src = white(&gpu);
        let mut drawer = Projection::new(&gpu);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0]);
        // A 20 x 6 projector drawn into a viewport twice its size: each projector pixel is 2 x 2.
        let mut shot = |pattern: u32| {
            let target = Tex::new(&gpu, 40, 12, "projector").unwrap();
            let look = Look {
                region: [0.0, 0.0, 1.0, 1.0],
                peers: vec![],
                blend: false,
                gamma: 1.0,
                pattern: false,
                test: None,
                overlay: None,
                tint: [1.0; 3],
                // Halves everything it applies to: the stripes must not go through it.
                correction: Some(Arc::new(Correction {
                    matrix: [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]],
                    curves: [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
                })),
                flatten: 1.0,
                lift: 1.0,
                flat: None,
                stripes: Some(Stripes { pattern, level: 1.0, size: [20, 6] }),
            };
            let item = Item {
                key: 6,
                tex: &src,
                vertices: &vertices,
                indices: &indices,
                viewport: [0.0, 0.0, 40.0, 12.0],
                look,
                gain: None,
                black: None,
            };
            drawer.render(&gpu, &target, &[item]).unwrap();
            gpu.read(&target).unwrap()
        };
        let gray = |v: u32| v ^ (v >> 1);
        // 20 columns: 5 bits; 6 rows: 3 bits. Pattern 2 + 2 * bit (+1 for the inverse).
        for (pattern, lit) in [
            (0u32, Box::new(|_: u32, _: u32| true) as Box<dyn Fn(u32, u32) -> bool>),
            (1, Box::new(|_, _| false)),
            (2, Box::new(move |x, _| (gray(x) >> 4) & 1 == 1)),
            (7, Box::new(move |x, _| (gray(x) >> 2) & 1 == 0)),
            (12, Box::new(move |_, y| (gray(y) >> 2) & 1 == 1)),
            (17, Box::new(move |_, y| gray(y) & 1 == 0)),
            (18, Box::new(|_, _| false)),
        ] {
            let px = shot(pattern);
            for y in 0..6u32 {
                for x in 0..20u32 {
                    let v = px.get(2 * x + 1, 2 * y + 1).unwrap()[0];
                    let want = if lit(x, y) { 1.0 } else { 0.0 };
                    assert!((v - want).abs() < 2e-3, "pattern {pattern} at {x}, {y}: {v}");
                }
            }
        }
    }
}
