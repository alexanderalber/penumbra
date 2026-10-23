//! Flat 2D drawing into a texture: filled and outlined rounded rectangles (circles are
//! rectangles rounded all the way), arcs, lines and glyphs from a coverage atlas. Each primitive
//! is one instanced quad over its own bounds whose fragments cover themselves by their signed
//! distance to the edge (`canvas.wgsl`), so a few hundred primitives at 1080p cost about what
//! filling the picture once does.
//!
//! Colours are premultiplied and sRGB-encoded, as an 8-bit UI colour is, and blend in that
//! space the way an egui window does, so a picture drawn here matches the same drawing in the
//! UI. A resolve pass turns the result into the [`FORMAT`] convention (linear light) at the end.

use penumbra_core::{Error, lock};

use crate::{FORMAT, Gpu, Tex};

/// The shader, after the helpers the built-in shaders share.
const SOURCE: &str = concat!(include_str!("common.wgsl"), include_str!("canvas.wgsl"));

/// Width and height of the glyph atlas in texels.
pub const ATLAS: u32 = 2048;
/// Bytes of one primitive in the storage buffer.
const PRIM_SIZE: usize = 80;
/// Bytes of the uniform block.
const UNIFORM_SIZE: u64 = 16;
/// Most primitives drawn at once; more are left out.
pub const MAX_PAINTS: usize = 1 << 16;

/// One primitive, in target pixels (origin top left), with its colour (premultiplied,
/// sRGB-encoded, 0 to 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// A filled rectangle with corners rounded by `radius` (half the smaller side: a circle or
    /// a capsule).
    Rect { min: [f32; 2], max: [f32; 2], radius: f32, color: [f32; 4] },
    /// The outline of such a rectangle, `width` wide, inside its edge.
    Frame { min: [f32; 2], max: [f32; 2], radius: f32, width: f32, color: [f32; 4] },
    /// A ring around `center`, `width` wide, its middle at `radius`, from the top clockwise for
    /// `fraction` of a turn.
    Arc { center: [f32; 2], radius: f32, width: f32, fraction: f32, color: [f32; 4] },
    /// A straight line `width` wide with square ends at the end points.
    Line { from: [f32; 2], to: [f32; 2], width: f32, color: [f32; 4] },
    /// The atlas texels from `uv_min` to `uv_max` (texel units) stretched over `min..max`, as
    /// coverage of `color`.
    Glyph { min: [f32; 2], max: [f32; 2], uv_min: [f32; 2], uv_max: [f32; 2], color: [f32; 4] },
}

impl Paint {
    /// The quad drawn: the shape's bounds plus a pixel for the antialiased edge; `None` when a
    /// coordinate is not finite.
    fn record(&self) -> Option<[u8; PRIM_SIZE]> {
        let (kind, bounds, a, b, color) = match *self {
            Paint::Rect { min, max, radius, color } => {
                (0u32, grow(min, max, 1.0), [min[0], min[1], max[0], max[1]], [radius, 0.0, 0.0, 0.0], color)
            }
            Paint::Frame { min, max, radius, width, color } => (
                1,
                grow(min, max, 1.0),
                [min[0], min[1], max[0], max[1]],
                [radius, width.max(0.0), 0.0, 0.0],
                color,
            ),
            Paint::Arc { center, radius, width, fraction, color } => {
                let r = radius.abs() + width.abs() * 0.5;
                (
                    2,
                    grow([center[0] - r, center[1] - r], [center[0] + r, center[1] + r], 1.0),
                    [center[0], center[1], radius.abs(), width.abs()],
                    [fraction, 0.0, 0.0, 0.0],
                    color,
                )
            }
            Paint::Line { from, to, width, color } => {
                let min = [from[0].min(to[0]), from[1].min(to[1])];
                let max = [from[0].max(to[0]), from[1].max(to[1])];
                (
                    3,
                    grow(min, max, width.abs() * 0.5 + 1.0),
                    [from[0], from[1], to[0], to[1]],
                    [width.abs(), 0.0, 0.0, 0.0],
                    color,
                )
            }
            Paint::Glyph { min, max, uv_min, uv_max, color } => (
                4,
                [min[0], min[1], max[0], max[1]],
                [min[0], min[1], max[0], max[1]],
                [uv_min[0], uv_min[1], uv_max[0], uv_max[1]],
                color,
            ),
        };
        if !bounds.iter().chain(&a).chain(&b).chain(&color).all(|v| v.is_finite()) {
            return None;
        }
        let mut out = [0u8; PRIM_SIZE];
        let words = bounds.iter().chain(&a).chain(&b).chain(&color).map(|v| v.to_le_bytes());
        let words = words.chain([kind.to_le_bytes(), [0; 4], [0; 4], [0; 4]]);
        for (slot, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *slot = word;
        }
        Some(out)
    }
}

fn grow(min: [f32; 2], max: [f32; 2], by: f32) -> [f32; 4] {
    [min[0].min(max[0]) - by, min[1].min(max[1]) - by, max[0].max(min[0]) + by, max[1].max(min[1]) + by]
}

/// The two pipelines, compiled once per device ([`Gpu::canvas_pipelines`]).
pub struct Pipelines {
    draw: wgpu::RenderPipeline,
    resolve: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl Pipelines {
    fn compile(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let (pipelines, error) = gpu.scoped(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("canvas"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let pipeline = |label, vs, fs, blend| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some(vs),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: FORMAT,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            let draw = pipeline(
                "canvas draw",
                "vs_draw",
                "fs_draw",
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            );
            let resolve = pipeline("canvas resolve", "vs_resolve", "fs_resolve", None);
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("canvas atlas"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            Pipelines { draw, resolve, sampler }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot build the canvas pipelines: {e}"))),
            None => Ok(pipelines),
        }
    }
}

impl Gpu {
    /// The canvas pipelines, compiled on first use and shared.
    pub fn canvas_pipelines(&self) -> Result<std::sync::Arc<Pipelines>, Error> {
        let mut slot = lock(&self.inner.canvas);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = std::sync::Arc::new(Pipelines::compile(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}

/// The GPU side of one drawing: the glyph atlas, the primitive buffer and the blending
/// texture, kept between frames.
#[derive(Default)]
pub struct Canvas {
    atlas: Option<(wgpu::Texture, wgpu::TextureView)>,
    prims: Option<(wgpu::Buffer, usize)>,
    uniform: Option<wgpu::Buffer>,
    scratch: Option<Tex>,
    bytes: Vec<u8>,
}

impl Canvas {
    pub fn new() -> Self {
        Self::default()
    }

    fn atlas(&mut self, gpu: &Gpu) -> Result<&(wgpu::Texture, wgpu::TextureView), Error> {
        if self.atlas.is_none() {
            let (texture, error) = gpu.scoped(|| {
                gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("canvas atlas"),
                    size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            });
            if let Some(e) = error {
                return Err(Error::new(format!("cannot create the glyph atlas: {e}")));
            }
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.atlas = Some((texture, view));
        }
        self.atlas.as_ref().ok_or_else(|| Error::new("no glyph atlas"))
    }

    /// Writes `coverage` (`width` x `height` bytes, rows top down) into the atlas at `x`, `y`.
    pub fn put(
        &mut self,
        gpu: &Gpu,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        coverage: &[u8],
    ) -> Result<(), Error> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let fits = x.checked_add(width).is_some_and(|r| r <= ATLAS)
            && y.checked_add(height).is_some_and(|b| b <= ATLAS)
            && coverage.len() as u64 == u64::from(width) * u64::from(height);
        if !fits {
            return Err(Error::new(format!("a {width}x{height} glyph does not fit the atlas at {x}, {y}")));
        }
        let (texture, _) = self.atlas(gpu)?;
        let ((), error) = gpu.scoped(|| {
            gpu.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                coverage,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot write a glyph: {e}"))),
            None => Ok(()),
        }
    }

    /// Draws `paints` in order over `background` (linear, straight alpha) into `target`.
    /// Primitives with coordinates that are not finite are left out, as are those past
    /// [`MAX_PAINTS`].
    pub fn render(
        &mut self,
        gpu: &Gpu,
        target: &Tex,
        background: [f32; 4],
        paints: &[Paint],
    ) -> Result<(), Error> {
        let pipelines = gpu.canvas_pipelines()?;
        let (w, h) = target.size();
        if self.scratch.as_ref().is_none_or(|s| s.size() != (w, h)) {
            self.scratch = Some(Tex::new(gpu, w, h, "canvas blend")?);
        }
        let scratch = self.scratch.clone().ok_or_else(|| Error::new("no canvas texture"))?;
        self.bytes.clear();
        let mut count = 0u32;
        for p in paints.iter().take(MAX_PAINTS) {
            if let Some(r) = p.record() {
                self.bytes.extend_from_slice(&r);
                count += 1;
            }
        }
        if self.bytes.is_empty() {
            // Bindings cannot be empty.
            self.bytes.resize(PRIM_SIZE, 0);
        }
        let device = gpu.device();
        let needed = self.bytes.len();
        if self.prims.as_ref().is_none_or(|(_, cap)| *cap < needed) {
            let cap = needed.next_power_of_two().max(PRIM_SIZE * 64);
            let (buffer, error) = gpu.scoped(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("canvas prims"),
                    size: cap as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            if let Some(e) = error {
                return Err(Error::new(format!("cannot create the canvas buffer: {e}")));
            }
            self.prims = Some((buffer, cap));
        }
        let uniform = self
            .uniform
            .get_or_insert_with(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("canvas"),
                    size: UNIFORM_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        let mut u = Vec::with_capacity(UNIFORM_SIZE as usize);
        for v in [w as f32, h as f32, ATLAS as f32, ATLAS as f32] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        gpu.queue().write_buffer(&uniform, 0, &u);
        let prims =
            self.prims.as_ref().map(|(b, _)| b.clone()).ok_or_else(|| Error::new("no canvas buffer"))?;
        gpu.queue().write_buffer(&prims, 0, &self.bytes);
        let atlas = self.atlas(gpu)?.1.clone();
        let (groups, error) = gpu.scoped(|| {
            let draw = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("canvas draw"),
                layout: &pipelines.draw.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: prims.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&atlas) },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                    },
                ],
            });
            let resolve = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("canvas resolve"),
                layout: &pipelines.resolve.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(scratch.view()),
                }],
            });
            (draw, resolve)
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot bind the canvas: {e}")));
        }
        let (draw_group, resolve_group) = groups;
        let clear = blend_space(background);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("canvas") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: scratch.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear[0]),
                            g: f64::from(clear[1]),
                            b: f64::from(clear[2]),
                            a: f64::from(clear[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if count > 0 {
                rp.set_pipeline(&pipelines.draw);
                rp.set_bind_group(0, &draw_group, &[]);
                rp.draw(0..6, 0..count);
            }
        }
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas resolve"),
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
            rp.set_pipeline(&pipelines.resolve);
            rp.set_bind_group(0, &resolve_group, &[]);
            rp.draw(0..3, 0..1);
        }
        gpu.submit([encoder.finish()]);
        Ok(())
    }
}

/// A linear colour with straight alpha as the canvas blends it: sRGB-encoded, premultiplied.
fn blend_space(c: [f32; 4]) -> [f32; 4] {
    let a = if c[3].is_finite() { c[3].clamp(0.0, 1.0) } else { 0.0 };
    let e = |v: f32| crate::readback::linear_to_srgb(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }) * a;
    [e(c[0]), e(c[1]), e(c[2]), a]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{f32_at, gpu, member, u32_at, wgsl_struct};

    /// A primitive's record and the uniform block match canvas.wgsl's structs.
    #[test]
    fn layout_matches_the_shader() {
        let source = SOURCE;
        let (span, _) = wgsl_struct(source, "U");
        assert_eq!(u64::from(span), UNIFORM_SIZE);
        let (span, m) = wgsl_struct(source, "Prim");
        assert_eq!(span as usize, PRIM_SIZE);
        let line = Paint::Line { from: [2.0, 3.0], to: [6.0, 7.0], width: 0.5, color: [0.25, 0.0, 0.0, 1.0] };
        let b = line.record().unwrap();
        let f = |name: &str| f32_at(&b, member(&m, name).offset);
        assert_eq!(f("bounds"), 2.0 - 1.25);
        assert_eq!(f("a"), 2.0);
        assert_eq!(f("b"), 0.5);
        assert_eq!(f("color"), 0.25);
        assert_eq!(u32_at(&b, member(&m, "kind").offset), 3);
    }

    #[test]
    fn shapes_cover_their_pixels_and_blend_like_the_ui() {
        let Some(gpu) = gpu() else { return };
        let target = Tex::new(&gpu, 64, 32, "canvas").unwrap();
        let mut canvas = Canvas::new();
        let white = [1.0; 4];
        // 50 % white over black in sRGB space is sRGB 0.5, about 0.214 linear.
        let half = [0.5; 4];
        let paints = [
            Paint::Rect { min: [2.0, 2.0], max: [12.0, 12.0], radius: 0.0, color: white },
            Paint::Rect { min: [20.0, 2.0], max: [30.0, 12.0], radius: 5.0, color: half },
            Paint::Line { from: [40.0, 0.0], to: [40.0, 32.0], width: 2.0, color: white },
            Paint::Frame { min: [2.0, 16.0], max: [18.0, 30.0], radius: 0.0, width: 2.0, color: white },
            Paint::Arc { center: [50.0, 20.0], radius: 8.0, width: 2.0, fraction: 0.25, color: white },
            Paint::Rect { min: [f32::NAN, 0.0], max: [1.0, 1.0], radius: 0.0, color: white },
        ];
        canvas.render(&gpu, &target, [0.0, 0.0, 0.0, 1.0], &paints).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(6, 6).unwrap(), [1.0; 4], "inside the white square");
        assert_eq!(px.get(15, 6).unwrap(), [0.0, 0.0, 0.0, 1.0], "the background");
        let v = px.get(25, 7).unwrap();
        assert!((v[0] - 0.214).abs() < 0.01 && v[3] == 1.0, "half white blended in sRGB: {v:?}");
        assert_eq!(px.get(20, 2).unwrap()[0], 0.0, "the rounded corner is cut");
        assert!(px.get(39, 16).unwrap()[0] > 0.99 && px.get(42, 16).unwrap()[0] < 0.01, "a 2 px line");
        assert!(px.get(2, 23).unwrap()[0] > 0.99 && px.get(10, 23).unwrap()[0] < 0.01, "a frame is hollow");
        // The arc's first quarter: right of the top, not left of it.
        assert!(px.get(53, 12).unwrap()[0] > 0.5, "{:?}", px.get(53, 12));
        assert!(px.get(46, 12).unwrap()[0] < 0.01);

        // A transparent background stays transparent where nothing is drawn; glyph coverage
        // comes from the atlas.
        canvas.put(&gpu, 0, 0, 2, 2, &[255, 255, 255, 255]).unwrap();
        let glyph = Paint::Glyph {
            min: [4.0, 4.0],
            max: [8.0, 8.0],
            uv_min: [0.5, 0.5],
            uv_max: [1.5, 1.5],
            color: white,
        };
        canvas.render(&gpu, &target, [0.0; 4], &[glyph]).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(30, 20).unwrap(), [0.0; 4]);
        assert!(px.get(6, 6).unwrap()[3] > 0.99);
        assert!(canvas.put(&gpu, ATLAS - 1, 0, 2, 1, &[0, 0]).is_err(), "past the atlas' edge");
    }
}
