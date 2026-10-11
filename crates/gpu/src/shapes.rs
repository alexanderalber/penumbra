//! Draws shapes (closed polygons in target pixels) as a mask, a signed distance field or
//! outlines, exactly per pixel in `shapes.wgsl`: the `tex.shapes` node's three pictures. The
//! shapes go to the GPU as two storage buffers; each pixel walks the edges of the shapes whose
//! bounding box is near enough, so a facade of a hundred windows costs about what a few do.

use penumbra_core::Error;

use crate::{FORMAT, Gpu, Tex};

const UNIFORM_SIZE: u64 = 32;
const POLY_SIZE: usize = 48;

/// What a pixel shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShapeMode {
    /// White where a shape is, coverage as alpha (premultiplied), edges antialiased.
    Mask,
    /// Red: the signed distance to the nearest shape's edge (negative inside) times `unit` per
    /// pixel; green and blue: the direction in which it grows (away from the shapes), a unit
    /// vector in pixels, y down; alpha 1.
    Sdf { unit: f32 },
    /// Every shape's edge, `width` pixels wide, in its colour.
    Outline { width: f32 },
}

/// One shape: its outline in target pixels (closed; the last point is not repeated) and its
/// colour for outlines (linear, straight alpha).
pub struct ShapePoly<'a> {
    pub points: &'a [[f32; 2]],
    pub color: [f32; 4],
}

/// The pipeline; one per thread that draws.
pub struct Shapes {
    pipeline: Option<wgpu::RenderPipeline>,
    uniform: Option<wgpu::Buffer>,
}

impl Default for Shapes {
    fn default() -> Self {
        Self::new()
    }
}

impl Shapes {
    pub fn new() -> Self {
        Self { pipeline: None, uniform: None }
    }

    fn pipeline(&mut self, gpu: &Gpu) -> Result<wgpu::RenderPipeline, Error> {
        if let Some(p) = &self.pipeline {
            return Ok(p.clone());
        }
        let device = gpu.device();
        let (pipeline, error) = gpu.scoped(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shapes"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shapes.wgsl").into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shapes"),
                layout: None,
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
                    targets: &[Some(FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the shapes pipeline: {e}")));
        }
        self.pipeline = Some(pipeline.clone());
        Ok(pipeline)
    }

    /// Compiles the pipeline ahead of the first draw.
    pub fn warm_up(&mut self, gpu: &Gpu) -> Result<(), Error> {
        self.pipeline(gpu).map(|_| ())
    }

    /// Draws `polys` into `target` as `mode` says. Polygons with fewer than three points or
    /// with points that are not finite are left out.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        target: &Tex,
        polys: &[ShapePoly<'_>],
        mode: ShapeMode,
    ) -> Result<(), Error> {
        let pipeline = self.pipeline(gpu)?;
        let mut records = Vec::with_capacity(polys.len().max(1) * POLY_SIZE);
        let mut points: Vec<u8> = Vec::new();
        let mut count = 0u32;
        let mut first = 0u32;
        for poly in polys {
            let n = poly.points.len();
            if n < 3 || poly.points.iter().any(|p| !(p[0].is_finite() && p[1].is_finite())) {
                continue;
            }
            let Ok(n32) = u32::try_from(n) else { continue };
            let mut lo = [f32::INFINITY; 2];
            let mut hi = [f32::NEG_INFINITY; 2];
            for p in poly.points {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                points.extend_from_slice(&p[0].to_le_bytes());
                points.extend_from_slice(&p[1].to_le_bytes());
            }
            for v in [lo[0], lo[1], hi[0], hi[1]] {
                records.extend_from_slice(&v.to_le_bytes());
            }
            for v in [first, n32, 0, 0] {
                records.extend_from_slice(&v.to_le_bytes());
            }
            for v in poly.color {
                records.extend_from_slice(&v.to_le_bytes());
            }
            first = first.saturating_add(n32);
            count += 1;
        }
        // Bindings cannot be empty.
        if records.is_empty() {
            records.resize(POLY_SIZE, 0);
        }
        if points.is_empty() {
            points.resize(8, 0);
        }
        let (mode_id, width, unit) = match mode {
            ShapeMode::Mask => (0u32, 0.0, 0.0),
            ShapeMode::Sdf { unit } => (1, 0.0, unit),
            ShapeMode::Outline { width } => (2, width, 0.0),
        };
        let mut u = Vec::with_capacity(UNIFORM_SIZE as usize);
        for v in [target.width() as f32, target.height() as f32] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        for v in [mode_id, count] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        for v in [width, unit, 0.0, 0.0] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        let device = gpu.device();
        let uniform = self
            .uniform
            .get_or_insert_with(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shapes"),
                    size: UNIFORM_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        gpu.queue().write_buffer(&uniform, 0, &u);
        let storage = |bytes: &[u8], label: &str| {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue().write_buffer(&b, 0, bytes);
            b
        };
        let (group, error) = gpu.scoped(|| {
            let polys = storage(&records, "shapes polys");
            let points = storage(&points, "shapes points");
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shapes"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: polys.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: points.as_entire_binding() },
                ],
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot upload the shapes: {e}")));
        }
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shapes") });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shapes"),
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
            rp.set_pipeline(&pipeline);
            rp.set_bind_group(0, &group, &[]);
            rp.draw(0..3, 0..1);
        }
        gpu.submit([encoder.finish()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    #[test]
    fn mask_sdf_and_outline_of_a_square() {
        let Some(gpu) = gpu() else { return };
        let target = Tex::new(&gpu, 40, 20, "shapes").unwrap();
        let square = [[10.0, 5.0], [20.0, 5.0], [20.0, 15.0], [10.0, 15.0]];
        let polys = [ShapePoly { points: &square, color: [1.0, 0.5, 0.0, 1.0] }];
        let mut drawer = Shapes::new();

        drawer.render(&gpu, &target, &polys, ShapeMode::Mask).unwrap();
        let px = gpu.read(&target).unwrap();
        assert_eq!(px.get(15, 10).unwrap(), [1.0; 4]);
        assert_eq!(px.get(5, 10).unwrap(), [0.0; 4]);

        drawer.render(&gpu, &target, &polys, ShapeMode::Sdf { unit: 1.0 }).unwrap();
        let px = gpu.read(&target).unwrap();
        // Pixel 30 has its centre at 30.5: 10.5 right of the square, growing to the right.
        let v = px.get(30, 10).unwrap();
        assert!((v[0] - 10.5).abs() < 0.02 && (v[1] - 1.0).abs() < 1e-3 && v[2].abs() < 1e-3, "{v:?}");
        // Inside, 2.5 below the top edge: -2.5, growing upwards (towards the nearest edge).
        let v = px.get(15, 7).unwrap();
        assert!((v[0] + 2.5).abs() < 0.01 && (v[2] + 1.0).abs() < 1e-3, "{v:?}");

        drawer.render(&gpu, &target, &polys, ShapeMode::Outline { width: 2.0 }).unwrap();
        let px = gpu.read(&target).unwrap();
        assert!((px.get(10, 10).unwrap()[0] - 1.0).abs() < 1e-3, "on the left edge");
        assert_eq!(px.get(15, 10).unwrap()[3], 0.0, "inside, away from the edges");

        // No shapes: nothing in the mask, far away in the SDF.
        drawer.render(&gpu, &target, &[], ShapeMode::Mask).unwrap();
        assert_eq!(gpu.read(&target).unwrap().get(15, 10).unwrap(), [0.0; 4]);
        drawer.render(&gpu, &target, &[], ShapeMode::Sdf { unit: 1.0 }).unwrap();
        assert!(gpu.read(&target).unwrap().get(15, 10).unwrap()[0] >= 999.0);
    }
}
