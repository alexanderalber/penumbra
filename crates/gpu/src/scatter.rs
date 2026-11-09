//! Adds points into a picture: every texel of a position texture is a point, and the point adds
//! a value into the pixel it falls on (`scatter.wgsl`). The `tex.scatter` node's picture.
//!
//! Each point is drawn as a point primitive of one pixel (or a square of them), blended by plain
//! addition into a target cleared to 0. In a [`FLOAT_FORMAT`] target that makes the sums exact:
//! N points of value 1 on one pixel give N, up to 2^24. Blending keeps the order the points come
//! in, so the result is the same every frame. No camera, depth or material is involved.

use penumbra_core::Error;

use crate::{FLOAT_FORMAT, FORMAT, Gpu, Tex};

/// The shader.
const SOURCE: &str = include_str!("scatter.wgsl");

const UNIFORM_SIZE: u64 = 48;

/// Largest side of the square a point covers, in pixels.
pub const MAX_SPLAT: u32 = 8;

/// How positions are given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScatterUnits {
    /// Pixels of the target from its top left corner: x right, y down. A point at (x, y) lands
    /// in pixel (floor(x), floor(y)), the pixel `textureLoad` reads at that place.
    Pixels,
    /// Shares of the target, 0 to 1 across and down: (x * width, y * height) in pixels.
    Share,
}

/// What happens to a point beyond the target's edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScatterEdge {
    /// It adds nothing.
    Drop,
    /// It comes back in from the other side, as on a torus.
    Wrap,
    /// It lands on the nearest pixel of the edge.
    Clamp,
}

/// Which points add nothing, besides those with a position that is not a finite number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScatterDead {
    /// Every point with a finite position adds.
    None,
    /// A point whose third channel (blue) of the position texture is below 0.
    Blue,
    /// A point whose fourth channel (alpha) of the position texture is below 0.
    Alpha,
}

/// How [`Scatter::render`] adds the points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScatterSettings {
    pub units: ScatterUnits,
    pub edge: ScatterEdge,
    pub dead: ScatterDead,
    /// The side of the square of pixels each point adds into, 1 to [`MAX_SPLAT`]. Even sides
    /// put the extra column and row right of and below the point's pixel.
    pub splat: u32,
    /// What each point adds when no texture of values is given.
    pub value: [f32; 4],
}

impl Default for ScatterSettings {
    fn default() -> Self {
        Self {
            units: ScatterUnits::Pixels,
            edge: ScatterEdge::Drop,
            dead: ScatterDead::None,
            splat: 1,
            value: [1.0; 4],
        }
    }
}

/// The pipelines (one per target format) and the uniform; one per node that draws.
#[derive(Default)]
pub struct Scatter {
    half: Option<wgpu::RenderPipeline>,
    float: Option<wgpu::RenderPipeline>,
    layout: Option<wgpu::BindGroupLayout>,
    uniform: Option<wgpu::Buffer>,
}

impl Scatter {
    pub fn new() -> Self {
        Self::default()
    }

    fn layout(&mut self, gpu: &Gpu) -> wgpu::BindGroupLayout {
        if let Some(l) = &self.layout {
            return l.clone();
        }
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            // Read with textureLoad only, so 32-bit floats bind without filtering.
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = gpu.device().create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scatter"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1),
                texture(2),
            ],
        });
        self.layout = Some(layout.clone());
        layout
    }

    fn pipeline(&mut self, gpu: &Gpu, format: wgpu::TextureFormat) -> Result<wgpu::RenderPipeline, Error> {
        let float = format == FLOAT_FORMAT;
        if format != FORMAT && !float {
            return Err(Error::new(format!("points cannot be added into a picture of {format:?}")));
        }
        if float && !gpu.float_blending() {
            return Err(Error::new("this GPU cannot add into 32-bit float pictures"));
        }
        let slot = if float { &self.float } else { &self.half };
        if let Some(p) = slot {
            return Ok(p.clone());
        }
        let bind = self.layout(gpu);
        let device = gpu.device();
        let (pipeline, error) = gpu.scoped(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("scatter"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("scatter"),
                bind_group_layouts: &[Some(&bind)],
                immediate_size: 0,
            });
            let add = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("scatter"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::PointList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState { color: add, alpha: add }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the scatter pipeline: {e}")));
        }
        if float {
            self.float = Some(pipeline.clone());
        } else {
            self.half = Some(pipeline.clone());
        }
        Ok(pipeline)
    }

    /// Compiles the pipelines ahead of the first draw.
    pub fn warm_up(&mut self, gpu: &Gpu) -> Result<(), Error> {
        self.pipeline(gpu, FORMAT)?;
        if gpu.float_blending() {
            self.pipeline(gpu, FLOAT_FORMAT)?;
        }
        Ok(())
    }

    /// Clears `target` to 0 and adds every texel of `positions` into it as a point, as
    /// `settings` say. Point `i` is texel `(i % width, i / width)` of `positions` (red: x,
    /// green: y) and adds the texel of `values` at the same place, or `settings.value` without
    /// values. Values that are not finite add 0. Without positions the target stays 0.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        target: &Tex,
        positions: Option<&Tex>,
        values: Option<&Tex>,
        settings: &ScatterSettings,
    ) -> Result<(), Error> {
        let pipeline = self.pipeline(gpu, target.format())?;
        let splat = settings.splat.clamp(1, MAX_SPLAT);
        let vertices = match positions {
            Some(p) => vertex_count(p.size(), splat)?,
            None => 0,
        };
        let u = uniform_bytes(target.size(), settings, splat, values.is_some());
        let device = gpu.device();
        let uniform = self
            .uniform
            .get_or_insert_with(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("scatter"),
                    size: UNIFORM_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        gpu.queue().write_buffer(&uniform, 0, &u);
        // Bindings cannot be empty: an absent texture is stood in for by the target's own view
        // when nothing is drawn, and by the positions when there are no values.
        let pos_view = positions.map_or(target.view(), Tex::view);
        let val_view = values.map_or(pos_view, Tex::view);
        let bind = self.layout(gpu);
        let group = match positions {
            Some(_) => {
                let (group, error) = gpu.scoped(|| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("scatter"),
                        layout: &bind,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(pos_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(val_view),
                            },
                        ],
                    })
                });
                if let Some(e) = error {
                    return Err(Error::new(format!("cannot bind the points: {e}")));
                }
                Some(group)
            }
            None => None,
        };
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scatter") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scatter"),
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
            if let Some(group) = &group
                && vertices > 0
            {
                rp.set_pipeline(&pipeline);
                rp.set_bind_group(0, group, &[]);
                rp.draw(0..vertices, 0..1);
            }
        }
        gpu.submit([encoder.finish()]);
        Ok(())
    }
}

/// Vertices drawn for a position texture of `size` with squares of `splat` pixels: one per pixel
/// of every point's square. More than a draw call counts is an error.
fn vertex_count(size: (u32, u32), splat: u32) -> Result<u32, Error> {
    let points = u64::from(size.0) * u64::from(size.1);
    let n = points * u64::from(splat) * u64::from(splat);
    u32::try_from(n).map_err(|_| {
        Error::new(format!(
            "{points} points of {splat} x {splat} pixels are too many to draw at once; use a smaller `splat` or fewer points"
        ))
    })
}

/// The uniform `U` of scatter.wgsl.
fn uniform_bytes(size: (u32, u32), s: &ScatterSettings, splat: u32, has_values: bool) -> Vec<u8> {
    let mut u = Vec::with_capacity(UNIFORM_SIZE as usize);
    for v in s.value.map(|v| if v.is_finite() { v } else { 0.0 }) {
        u.extend_from_slice(&v.to_le_bytes());
    }
    for v in [size.0 as f32, size.1 as f32] {
        u.extend_from_slice(&v.to_le_bytes());
    }
    let units = match s.units {
        ScatterUnits::Pixels => 0u32,
        ScatterUnits::Share => 1,
    };
    let edge = match s.edge {
        ScatterEdge::Drop => 0u32,
        ScatterEdge::Wrap => 1,
        ScatterEdge::Clamp => 2,
    };
    let dead = match s.dead {
        ScatterDead::None => 0u32,
        ScatterDead::Blue => 2,
        ScatterDead::Alpha => 3,
    };
    for v in [units, edge, dead, splat, u32::from(has_values), 0] {
        u.extend_from_slice(&v.to_le_bytes());
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{gpu, wgsl_struct};
    use crate::{AdapterChoice, Pixels};

    #[test]
    fn layout_matches_the_shader() {
        assert_eq!(u64::from(wgsl_struct(SOURCE, "U").0), UNIFORM_SIZE);
        let bytes = uniform_bytes((4, 4), &ScatterSettings::default(), 1, false);
        assert_eq!(bytes.len() as u64, UNIFORM_SIZE);
    }

    /// A texture of 32-bit floats holding `points` as (x, y, b, a), one per texel in a row, as
    /// they are: NaN and infinities stay.
    fn positions(gpu: &Gpu, points: &[[f32; 4]]) -> Tex {
        positions_in_rows(gpu, u32::try_from(points.len()).unwrap(), points)
    }

    /// [`positions`] in rows of `w` texels.
    fn positions_in_rows(gpu: &Gpu, w: u32, points: &[[f32; 4]]) -> Tex {
        let h = u32::try_from(points.len()).unwrap() / w;
        let bytes: Vec<u8> = points.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        let tex = Tex::new_float(gpu, w, h, "positions").unwrap();
        gpu.queue().write_texture(
            tex.texture().as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 16), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        tex
    }

    fn scatter(gpu: &Gpu, w: u32, h: u32, points: &Tex, values: Option<&Tex>, s: ScatterSettings) -> Pixels {
        let format = if gpu.float_blending() { FLOAT_FORMAT } else { FORMAT };
        let target = Tex::new_in(gpu, w, h, format, "scatter").unwrap();
        Scatter::new().render(gpu, &target, Some(points), values, &s).unwrap();
        gpu.read(&target).unwrap()
    }

    fn red(px: &Pixels, w: u32, h: u32) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| px.get(x, y).unwrap()[0]).collect()
    }

    #[test]
    fn many_points_on_one_pixel_add_up_exactly() {
        let Some(gpu) = gpu() else { return };
        assert!(gpu.float_blending(), "WARP blends 32-bit floats");
        // 70 000 points: beyond what half floats count (2048) and beyond the old 400 limit.
        let n = 70_000;
        let p = positions_in_rows(&gpu, 280, &vec![[2.5, 1.25, 0.0, 0.0]; n]);
        let px = scatter(&gpu, 4, 3, &p, None, ScatterSettings::default());
        assert_eq!(px.get(2, 1).unwrap(), [n as f32; 4]);
        assert_eq!(red(&px, 4, 3).iter().sum::<f32>(), n as f32);
    }

    #[test]
    fn points_land_on_the_pixel_they_are_in_with_y_down() {
        let Some(gpu) = gpu() else { return };
        let p = positions(
            &gpu,
            &[[0.0, 0.0, 0.0, 0.0], [3.99, 0.5, 0.0, 0.0], [1.5, 2.0, 0.0, 0.0], [1.0, 2.99, 0.0, 0.0]],
        );
        let px = scatter(&gpu, 4, 3, &p, None, ScatterSettings::default());
        #[rustfmt::skip]
        let want = [
            1.0, 0.0, 0.0, 1.0,
            0.0, 0.0, 0.0, 0.0,
            0.0, 2.0, 0.0, 0.0,
        ];
        assert_eq!(red(&px, 4, 3), want);
        // The same places as shares of the picture.
        let share: Vec<[f32; 4]> = [[0.1, 0.1], [0.99, 0.2], [0.4, 0.7], [0.3, 0.9]]
            .iter()
            .map(|[x, y]| [*x, *y, 0.0, 0.0])
            .collect();
        let p = positions(&gpu, &share);
        let s = ScatterSettings { units: ScatterUnits::Share, ..Default::default() };
        assert_eq!(red(&scatter(&gpu, 4, 3, &p, None, s), 4, 3), want);
    }

    #[test]
    fn values_come_from_their_texel_and_bad_ones_add_nothing() {
        let Some(gpu) = gpu() else { return };
        let p = positions(&gpu, &[[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]]);
        let v = positions(
            &gpu,
            &[[1.0, 2.0, 3.0, 4.0], [0.25, -1.0, f32::NAN, f32::INFINITY], [7.0, 0.0, 0.0, 0.0]],
        );
        let px = scatter(&gpu, 2, 1, &p, Some(&v), ScatterSettings::default());
        assert_eq!(px.get(1, 0).unwrap(), [1.25, 1.0, 3.0, 4.0]);
        assert_eq!(px.get(0, 0).unwrap(), [7.0, 0.0, 0.0, 0.0]);
        let s = ScatterSettings { value: [0.5, 0.0, 2.0, 1.0], ..Default::default() };
        let px = scatter(&gpu, 2, 1, &p, None, s);
        assert_eq!(px.get(1, 0).unwrap(), [1.0, 0.0, 4.0, 2.0]);
    }

    #[test]
    fn dead_and_broken_points_add_nothing() {
        let Some(gpu) = gpu() else { return };
        let p = positions(
            &gpu,
            &[
                [1.0, 1.0, 0.0, 0.0],
                [1.0, 1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0, -1.0],
                [f32::NAN, 1.0, 0.0, 0.0],
                [1.0, f32::INFINITY, 0.0, 0.0],
                [f32::NEG_INFINITY, f32::NAN, 0.0, 0.0],
            ],
        );
        let count = |dead, edge| {
            let s = ScatterSettings { dead, edge, ..Default::default() };
            red(&scatter(&gpu, 3, 3, &p, None, s), 3, 3).iter().sum::<f32>()
        };
        for edge in [ScatterEdge::Drop, ScatterEdge::Wrap, ScatterEdge::Clamp] {
            assert_eq!(count(ScatterDead::None, edge), 3.0, "{edge:?}");
            assert_eq!(count(ScatterDead::Blue, edge), 2.0, "{edge:?}");
            assert_eq!(count(ScatterDead::Alpha, edge), 2.0, "{edge:?}");
        }
    }

    #[test]
    fn edges_drop_wrap_or_clamp() {
        let Some(gpu) = gpu() else { return };
        let p = positions(
            &gpu,
            &[[-0.5, 0.0, 0.0, 0.0], [4.0, 1.0, 0.0, 0.0], [1.0, -3.0, 0.0, 0.0], [1e30, 2.0, 0.0, 0.0]],
        );
        let at =
            |edge| red(&scatter(&gpu, 4, 3, &p, None, ScatterSettings { edge, ..Default::default() }), 4, 3);
        assert_eq!(at(ScatterEdge::Drop), [0.0; 12]);
        #[rustfmt::skip]
        let clamp = [
            1.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        assert_eq!(at(ScatterEdge::Clamp), clamp);
        // -0.5 is in column -1, which is column 3; column 4 is column 0; row -3 is row 0. A
        // position as far out as 1e30 lands somewhere on row 2, never off the picture.
        let wrap = at(ScatterEdge::Wrap);
        assert_eq!(&wrap[..8], [0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        assert_eq!(wrap[8..].iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn a_splat_covers_a_square_around_the_point() {
        let Some(gpu) = gpu() else { return };
        let p = positions(&gpu, &[[2.5, 2.5, 0.0, 0.0]]);
        let s = ScatterSettings { splat: 3, ..Default::default() };
        let r = red(&scatter(&gpu, 5, 5, &p, None, s), 5, 5);
        for y in 0..5 {
            for x in 0..5 {
                let inside = (1..=3).contains(&x) && (1..=3).contains(&y);
                assert_eq!(r[y * 5 + x], f32::from(u8::from(inside)), "({x}, {y})");
            }
        }
        let s = ScatterSettings { splat: 2, ..Default::default() };
        let r = red(&scatter(&gpu, 5, 5, &p, None, s), 5, 5);
        assert_eq!([r[12], r[13], r[17], r[18]], [1.0; 4]);
        assert_eq!(r.iter().sum::<f32>(), 4.0);
    }

    #[test]
    fn without_positions_the_picture_is_empty() {
        let Some(gpu) = gpu() else { return };
        let target = Tex::new_float(&gpu, 3, 2, "scatter").unwrap();
        Scatter::new().render(&gpu, &target, None, None, &ScatterSettings::default()).unwrap();
        assert_eq!(red(&gpu.read(&target).unwrap(), 3, 2), [0.0; 6]);
        // Too many vertices is an error, not a crash.
        assert_eq!(vertex_count((4096, 4096), 8).unwrap(), 1 << 30);
        assert!(vertex_count((8192, 8192), 8).is_err());
        assert!(vertex_count((16384, 16384), 1).is_ok());
    }

    #[test]
    fn half_float_pictures_count_exactly_up_to_2048() {
        let Ok(gpu) = Gpu::headless_without_float_blending(AdapterChoice::Software) else { return };
        assert!(!gpu.float_blending());
        let target = Tex::new(&gpu, 2, 1, "scatter").unwrap();
        let mut scatter = Scatter::new();
        assert!(
            scatter
                .render(&gpu, &Tex::new_float(&gpu, 1, 1, "f").unwrap(), None, None, &Default::default())
                .is_err()
        );
        let p = positions(&gpu, &[[0.5, 0.5, 0.0, 0.0]; 2048]);
        scatter.render(&gpu, &target, Some(&p), None, &ScatterSettings::default()).unwrap();
        assert_eq!(gpu.read(&target).unwrap().get(0, 0).unwrap()[0], 2048.0);
        assert_eq!(gpu.errors().count, 0);
    }
}
