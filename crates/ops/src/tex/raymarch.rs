//! `tex.raymarch`: draws a volume (`vol` data, such as `vol.smoke`'s) by marching a ray through
//! it for every pixel (`raymarch.wgsl`). The camera is orthographic and looks along -z, the
//! picture's right along +x and its top along +y; `rotate` turns the volume in front of it. The
//! smoke absorbs light by its density and shows in `color`, unlit.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Compute, Dispatch, Gpu, Medium, Out, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::{height_param, keep_size, side, width_param};
use crate::{on_gpu, waiting};

pub(crate) const KIND: &str = "tex.raymarch";
const SOURCE: &str = include_str!("raymarch.wgsl");

/// Most steps per cell.
const MAX_QUALITY: f64 = 8.0;

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
            ParamSpec::float("quality", 1.0)
                .clamp(0.05, MAX_QUALITY)
                .range(0.25, 4.0)
                .curve(Curve::Log)
                .more()
                .doc("Steps per cell along each ray. More shows less banding and costs more"),
        )
        .input("in", DataKind::Vol)
        .describe_input("in", "The volume: its red is the density")
        .output("out", DataKind::Tex)
        .describe_output("out", "The smoke in its colour over transparent black, premultiplied")
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
        ["far", "thickness", "step"].iter().map(|n| ParamSpec::float(n, 0.0)).collect();
    for n in ["center", "box", "axis_x", "axis_y", "axis_z"] {
        p.push(ParamSpec::vec3(n, [0.0; 3]));
    }
    p.push(ParamSpec::vec2("pan", [0.0; 2]));
    p.push(ParamSpec::vec2("view", [1.0; 2]));
    p.push(ParamSpec::color("color", [1.0; 4]));
    p
}

fn program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(KIND, SOURCE, &params())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Raymarch::default()) as Box<dyn Operator>)
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

#[derive(Default)]
struct Raymarch {
    out_ring: TexRing,
    density_ring: TexRing,
}

impl Operator for Raymarch {
    fn gpu_bytes(&self) -> u64 {
        penumbra_gpu::unique_bytes(self.out_ring.iter().chain(self.density_ring.iter()))
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
        let values: Vec<(&str, Value)> = vec![
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
        ];
        let param = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());

        let out = self.out_ring.next_storage(gpu, width, height, KIND)?;
        let input = Dispatch {
            volumes: &[Some(&medium.volume)],
            pictures: &[],
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        program.run(gpu, "march", Out::Picture(&out), &input)?;
        let density = if cx.output_wanted(1) {
            let d = self.density_ring.next_storage(gpu, width, height, KIND)?;
            let input =
                Dispatch { volumes: &[], pictures: &[Some(&out)], seconds: 0.0, frame: 0, param: &param };
            program.run(gpu, "opacity", Out::Picture(&d), &input)?;
            Data::new(DataKind::Tex, d)
        } else {
            Data::skipped(DataKind::Tex)
        };
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
