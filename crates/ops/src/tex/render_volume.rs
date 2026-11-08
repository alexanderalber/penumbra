//! `tex.render_volume`: draws a volume (`vol` data, such as `vol.smoke`'s) by marching a ray through
//! it for every pixel (`render_volume.wgsl`). The camera is orthographic and looks along -z, the
//! picture's right along +x and its top along +y; `rotate` turns the volume in front of it. The
//! smoke absorbs light by its density and shows in `color`, unlit; hot smoke glows in the colour
//! its temperature has on a palette.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Compute, DataBlock, Dispatch, Gpu, Medium, Out, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::{height_param, keep_size, side, width_param};
use crate::{on_gpu, waiting};

pub(crate) const KIND: &str = "tex.render_volume";
const SOURCE: &str = include_str!("render_volume.wgsl");

/// Most steps per cell.
const MAX_QUALITY: f64 = 8.0;

/// Most shapes drawn over the picture.
const MAX_SHAPES: usize = 128;

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Draws a volume such as vol.smoke's as seen from the front: thick smoke covers, thin smoke lets through")
        .param(width_param())
        .param(height_param())
        .param(keep_size())
        .param(
            ParamSpec::vec3("rotate", [0.0, 0.0, 0.0])
                .clamp(-36000.0, 36000.0)
                .unit(Unit::Degrees)
                .doc("Turns the volume about its centre: about x, then y, then z"),
        )
        .param(
            ParamSpec::float("view_height", 0.0)
                .clamp(0.0, 1e5)
                .range(0.0, 10.0)
                .unit(Unit::Metres)
                .doc("How high a slice of the world the picture shows; 0: as much as the whole box needs"),
        )
        .param(
            ParamSpec::vec2("pan", [0.0, 0.0])
                .unit(Unit::Metres)
                .doc("Moves the view across: the picture's centre this far right and up of the box's centre"),
        )
        .param(
            ParamSpec::float("thickness", 4.0)
                .clamp(0.0, 1e5)
                .range(0.0, 20.0)
                .curve(Curve::Power(2.0))
                .doc(
                    "How much light smoke of density 1 takes away per metre: at 4, a metre of it lets about \
                     2 % through",
                ),
        )
        .param(ParamSpec::color("color", [1.0, 1.0, 1.0, 1.0]).doc("Colour of the smoke; its alpha thins it"))
        .param(
            ParamSpec::float("glow", 0.0)
                .clamp(0.0, 1e4)
                .range(0.0, 4.0)
                .doc(
                    "How brightly hot smoke glows, on top of `color`: in the palette's colour at its \
                     temperature. 0: no glow",
                ),
        )
        .param(
            ParamSpec::float("hot", 2.0)
                .clamp(1e-6, 1e5)
                .range(0.1, 10.0)
                .curve(Curve::Log)
                .doc("Temperature that glows in the palette's right end; cold smoke takes its left end"),
        )
        .param(
            ParamSpec::float("quality", 1.0)
                .clamp(0.05, MAX_QUALITY)
                .range(0.25, 4.0)
                .curve(Curve::Log)
                .more()
                .doc("Steps per cell along each ray. More shows less banding and costs more"),
        )
        .param(
            ParamSpec::bool("show_emitters", false)
                .group("Debug")
                .doc("Draws the outline of every emitter over the picture, in orange"),
        )
        .param(
            ParamSpec::bool("show_obstacles", false)
                .group("Debug")
                .doc("Draws the outline of every obstacle over the picture, in cyan"),
        )
        .param(ParamSpec::bool("show_grid", false).group("Debug").doc(
            "Draws the box and its cells on the box's far sides. Where cells would be under 4 pixels, \
             only every 2nd, 4th or 8th line shows",
        ))
        .input("in", DataKind::Vol)
        .describe_input("in", "The volume: its red is the density, its green the temperature")
        .input("palette", DataKind::Tex)
        .describe_input(
            "palette",
            "Colours of the glow from cold on the left to hot on the right, read along the middle row. \
             Unwired: black over red, orange and yellow to white",
        )
        .output("out", DataKind::Tex)
        .describe_output(
            "out",
            "The smoke in its colour over transparent black, premultiplied, with what the Debug params \
             draw on top",
        )
        .output("density", DataKind::Tex)
        .describe_output(
            "density",
            "How much the smoke covers, as grey on black: white where nothing shows through. Opaque, so \
             tex.lookup reads the cover rather than the colour",
        )
        .lazy_output("density")
}

fn params() -> Vec<ParamSpec> {
    let mut p: Vec<ParamSpec> =
        ["far", "thickness", "step", "glow", "hot"].iter().map(|n| ParamSpec::float(n, 0.0)).collect();
    for n in ["center", "box", "axis_x", "axis_y", "axis_z"] {
        p.push(ParamSpec::vec3(n, [0.0; 3]));
    }
    p.push(ParamSpec::vec2("pan", [0.0; 2]));
    p.push(ParamSpec::vec2("view", [1.0; 2]));
    p.push(ParamSpec::color("color", [1.0; 4]));
    p.push(ParamSpec::int("palette", 0));
    p.push(ParamSpec::int("shapes", 0));
    p.push(ParamSpec::int("grid", 0));
    p.push(ParamSpec::float("line", 0.0));
    p.push(ParamSpec::vec3("cell", [0.0; 3]));
    p
}

fn program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(KIND, SOURCE, &params())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(RenderVolume::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(|gpu| program(gpu).map(|_| ()))),
    )
}

/// The rotation by `degrees` about x, then y, then z, as its columns: the box's axes in the world.
pub(crate) fn rotation(degrees: [f64; 3]) -> [[f64; 3]; 3] {
    let [a, b, c] = degrees.map(|d| if d.is_finite() { d.to_radians() } else { 0.0 });
    let (sa, ca) = a.sin_cos();
    let (sb, cb) = b.sin_cos();
    let (sc, cc) = c.sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, ca, sa], [0.0, -sa, ca]];
    let ry = [[cb, 0.0, -sb], [0.0, 1.0, 0.0], [sb, 0.0, cb]];
    let rz = [[cc, sc, 0.0], [-sc, cc, 0.0], [0.0, 0.0, 1.0]];
    // Columns: m[i] is column i. Product Rz * Ry * Rx.
    // Column c of p * q is the sum over k of p's column k times q[c][k].
    let mul = |p: [[f64; 3]; 3], q: [[f64; 3]; 3]| -> [[f64; 3]; 3] {
        q.map(|qc| {
            let mut out = [0.0; 3];
            for (pk, qk) in p.iter().zip(qc) {
                for (o, v) in out.iter_mut().zip(pk) {
                    *o += v * qk;
                }
            }
            out
        })
    };
    mul(rz, mul(ry, rx))
}

/// Width and height in metres of the box of `size` turned by `axes`, as the camera sees it.
pub(crate) fn extent(size: [f64; 3], axes: [[f64; 3]; 3]) -> [f64; 2] {
    let half = size.map(|s| s * 0.5);
    let x: f64 = axes.iter().zip(half).map(|(a, h)| (a[0] * h).abs()).sum();
    let y: f64 = axes.iter().zip(half).map(|(a, h)| (a[1] * h).abs()).sum();
    [x * 2.0, y * 2.0]
}

/// The vectors of `render_volume.wgsl`'s shapes, two each, those `emitters` and `obstacles` ask for.
/// Positions count from the box's centre.
fn shape_vectors(medium: &Medium, emitters: bool, obstacles: bool) -> Vec<[f32; 4]> {
    let [cx, cy, cz] = medium.center;
    let wanted = medium.shapes.iter().filter(|s| if s.solid { obstacles } else { emitters });
    let mut out = Vec::new();
    for s in wanted.take(MAX_SHAPES) {
        let kind = f32::from(u8::from(s.is_box)) + 2.0 * f32::from(u8::from(s.solid));
        out.push([s.center[0] - cx, s.center[1] - cy, s.center[2] - cz, kind]);
        out.push([s.half[0], s.half[1], s.half[2], s.radius]);
    }
    out
}

#[derive(Default)]
struct RenderVolume {
    out_ring: TexRing,
    density_ring: TexRing,
    /// The shapes for the shader, made when the first is drawn.
    shapes: Option<DataBlock>,
}

impl Operator for RenderVolume {
    fn gpu_bytes(&self) -> u64 {
        penumbra_gpu::unique_bytes(self.out_ring.iter().chain(self.density_ring.iter()))
            + self.shapes.as_ref().map_or(0, DataBlock::bytes)
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let program = program(gpu)?;
        let Some(medium) = cx.input_as::<Medium>(0) else { return Err(waiting(cx)) };
        let (width, height) = (side(cx, "width")?, side(cx, "height")?);
        let size = medium.size.map(f64::from);
        let center = medium.center.map(f64::from);
        let axes = rotation(match cx.param("rotate") {
            Some(Value::Vec3(r)) => *r,
            _ => [0.0; 3],
        });
        let aspect = f64::from(width) / f64::from(height.max(1));
        let [ex, ey] = extent(size, axes);
        let fit = ey.max(ex / aspect) * 1.05;
        let requested = cx.float("view_height")?;
        let view_height = if requested > 0.0 { requested } else { fit.max(1e-6) };
        let pan = match cx.param("pan") {
            Some(Value::Vec2(p)) => [center[0] + p[0], center[1] + p[1]],
            _ => [center[0], center[1]],
        };
        let cells = medium.volume.size();
        let longest = size.iter().zip(cells).map(|(s, c)| s / f64::from(c.max(1))).fold(f64::MAX, f64::min);
        let step = longest / cx.float("quality")?.clamp(0.05, MAX_QUALITY);
        let diagonal = (size[0] * size[0] + size[1] * size[1] + size[2] * size[2]).sqrt();
        let color = cx.param("color").cloned().unwrap_or(Value::Vec4([1.0; 4]));
        let palette = cx.input_as::<penumbra_gpu::Tex>(1);
        if palette.is_none() && cx.def().inputs.get(1).is_some_and(Option::is_some) && !cx.input_stuck(1) {
            return Err(waiting(cx));
        }
        let shape_data = shape_vectors(medium, cx.bool("show_emitters")?, cx.bool("show_obstacles")?);
        let grid = cx.bool("show_grid")?;
        let [nx, ny, nz] = cells.map(|c| f64::from(c.max(1)));
        let cell = [size[0] / nx, size[1] / ny, size[2] / nz];
        let values: Vec<(&str, Value)> = vec![
            ("shapes", Value::Int((shape_data.len() / 2) as i64)),
            ("grid", Value::Int(i64::from(grid))),
            ("line", Value::Float(view_height / f64::from(height.max(1)))),
            ("cell", Value::Vec3(cell)),
            ("far", Value::Float(center[2] + diagonal + 1.0)),
            ("thickness", Value::Float(cx.float("thickness")?.max(0.0))),
            ("step", Value::Float(step.max(1e-6))),
            ("center", Value::Vec3(center)),
            ("box", Value::Vec3(size)),
            ("axis_x", Value::Vec3(axes[0])),
            ("axis_y", Value::Vec3(axes[1])),
            ("axis_z", Value::Vec3(axes[2])),
            ("pan", Value::Vec2(pan)),
            ("view", Value::Vec2([view_height * aspect, view_height])),
            ("color", color),
            ("glow", Value::Float(cx.float("glow")?.max(0.0))),
            ("hot", Value::Float(cx.float("hot")?.max(1e-6))),
            ("palette", Value::Int(i64::from(palette.is_some()))),
        ];
        let param = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());

        let out = self.out_ring.next_storage(gpu, width, height, KIND)?;
        let input = Dispatch {
            volumes: &[Some(&medium.volume)],
            pictures: &[palette],
            data: None,
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        program.run(gpu, "march", Out::Picture(&out), &input)?;
        let density = if cx.output_wanted(1) {
            let d = self.density_ring.next_storage(gpu, width, height, KIND)?;
            let input = Dispatch {
                volumes: &[],
                pictures: &[Some(&out)],
                data: None,
                seconds: 0.0,
                frame: 0,
                param: &param,
            };
            program.run(gpu, "opacity", Out::Picture(&d), &input)?;
            Data::new(DataKind::Tex, d)
        } else {
            Data::skipped(DataKind::Tex)
        };
        // A picture of its own, so the density output stays the smoke's alone.
        if grid || !shape_data.is_empty() {
            let block = match &self.shapes {
                Some(b) => b.clone(),
                None => {
                    let b = DataBlock::new(gpu, MAX_SHAPES * 2, KIND)?;
                    self.shapes = Some(b.clone());
                    b
                }
            };
            block.write(gpu, &shape_data)?;
            let marked = self.out_ring.next_storage(gpu, width, height, KIND)?;
            let input = Dispatch {
                volumes: &[],
                pictures: &[Some(&out)],
                data: Some(&block),
                seconds: 0.0,
                frame: 0,
                param: &param,
            };
            program.run(gpu, "overlay", Out::Picture(&marked), &input)?;
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, marked), density]));
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, out), density]))
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_turn_the_axes_the_right_way() {
        let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
        let r = rotation([0.0, 0.0, 90.0]);
        // About z by a quarter turn: x points along y.
        assert!(near(r[0], [0.0, 1.0, 0.0]), "{r:?}");
        assert!(near(r[1], [-1.0, 0.0, 0.0]), "{r:?}");
        let r = rotation([90.0, 0.0, 0.0]);
        assert!(near(r[1], [0.0, 0.0, 1.0]), "{r:?}");
        let r = rotation([0.0, 90.0, 0.0]);
        assert!(near(r[2], [1.0, 0.0, 0.0]), "{r:?}");
        assert!(near(rotation([f64::NAN, 0.0, 0.0])[0], [1.0, 0.0, 0.0]));
    }

    #[test]
    fn the_extent_follows_the_turn() {
        let e = extent([4.0, 1.0, 2.0], rotation([0.0; 3]));
        assert!((e[0] - 4.0).abs() < 1e-9 && (e[1] - 1.0).abs() < 1e-9, "{e:?}");
        let e = extent([4.0, 1.0, 2.0], rotation([0.0, 90.0, 0.0]));
        assert!((e[0] - 2.0).abs() < 1e-9, "{e:?}");
    }
}
