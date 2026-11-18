//! `pts.field`: a `weight` per point from a shape with a soft edge, TouchDesigner's Field POP.
//! 1 inside a ball, a box, a half space or a capsule, 0 beyond its edge plus `falloff`, smooth
//! between. Masks forces (`pts.force` multiplies by `weight`), colours, deletions.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::AttrType;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::{Own, attr_name, generated_shape, input};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.field";

/// Choices of `shape`.
pub const SHAPES: [&str; 4] = ["sphere", "box", "plane", "capsule"];

/// Choices of `combine`.
pub const COMBINES: [&str; 5] = ["set", "multiply", "add", "max", "min"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                KIND,
                "A weight per point from a shape with a soft edge: 1 inside, 0 outside. Masks forces, colours and deletions",
            )
            .param(
                ParamSpec::choice("shape", &SHAPES)
                    .doc("Where the weight is 1")
                    .choice_doc("sphere", "inside the ball of `radius` around `center`")
                    .choice_doc("box", "inside the box of `size` around `center`")
                    .choice_doc("plane", "on the side `normal` points to of the plane through `center`")
                    .choice_doc("capsule", "within `radius` of the line of `length` through `center` along `axis`: a rod with round ends"),
            )
            .param(ParamSpec::vec3("center", [0.0; 3]).range(-5.0, 5.0).doc("The middle of the shape, in metres"))
            .param(
                ParamSpec::float("radius", 0.5)
                    .clamp(0.0, 1e6)
                    .range(0.0, 5.0)
                    .doc("sphere and capsule: the radius, in metres")
                    .applies_when("shape", &["sphere", "capsule"]),
            )
            .param(
                ParamSpec::vec3("size", [1.0; 3])
                    .range(0.0, 10.0)
                    .doc("box: its size along x, y and z, in metres")
                    .applies_when("shape", &["box"]),
            )
            .param(
                ParamSpec::vec3("normal", [0.0, 0.0, 1.0])
                    .range(-1.0, 1.0)
                    .doc("plane: the side where the weight is 1; its length does not count")
                    .applies_when("shape", &["plane"]),
            )
            .param(
                ParamSpec::vec3("axis", [0.0, 0.0, 1.0])
                    .range(-1.0, 1.0)
                    .doc("capsule: the direction of its line; its length does not count")
                    .applies_when("shape", &["capsule"]),
            )
            .param(
                ParamSpec::float("length", 1.0)
                    .clamp(0.0, 1e6)
                    .range(0.0, 10.0)
                    .doc("capsule: the length of its line, without the round ends, in metres")
                    .applies_when("shape", &["capsule"]),
            )
            .param(
                ParamSpec::float("falloff", 0.2)
                    .clamp(0.0, 1e6)
                    .range(0.0, 2.0)
                    .doc("The soft edge, in metres outside the shape: the weight goes smoothly from 1 at the shape to 0 this far out. 0 is a hard edge"),
            )
            .param(ParamSpec::bool("invert", false).doc("1 outside the shape and 0 inside"))
            .param(
                ParamSpec::choice("combine", &COMBINES)
                    .doc("How the shape's weight meets the weight the points have already (1 where they have none)")
                    .choice_doc("set", "it takes its place")
                    .choice_doc("multiply", "multiplies it: only where both are")
                    .choice_doc("add", "is added to it")
                    .choice_doc("max", "the larger of the two: where either is")
                    .choice_doc("min", "the smaller of the two"),
            )
            .param(
                ParamSpec::text("attribute", "weight")
                    .more()
                    .doc("The attribute the weight goes into, TouchDesigner's names too (Weight)"),
            )
            .param(
                ParamSpec::text("distance", "")
                    .more()
                    .doc("Also writes each point's distance from the shape's edge into this attribute, in metres: negative inside. Empty: none"),
            )
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts)
            .describe_output("out", "The points with the weight; the others pass on unchanged"),
            || Box::new(Field::default()) as Box<dyn Operator>,
        )
        .warm_up(on_gpu(warm_up)),
    )
}

/// The program that writes the weight into `target`, and the distance into `distance`.
fn spec(target: &str, distance: Option<&str>) -> Spec {
    let write_distance = distance.map_or(String::new(), |d| format!("    {d} = pts_d;\n"));
    let source = format!(
        "
fn pts_unit(v: vec3f, fallback: vec3f) -> vec3f {{
    let l = length(v);
    return select(fallback, v / l, l > 1e-9);
}}

// The distance from the shape's edge: negative inside.
fn pts_distance(p: vec3f) -> f32 {{
    let q = p - params.center;
    var d = 0.0;
    if params.shape == shape_sphere {{
        d = length(q) - params.radius;
    }} else if params.shape == shape_box {{
        let e = abs(q) - abs(params.size) * 0.5;
        d = length(max(e, vec3f(0.0))) + min(max(e.x, max(e.y, e.z)), 0.0);
    }} else if params.shape == shape_plane {{
        d = -dot(q, pts_unit(params.normal, vec3f(0.0, 0.0, 1.0)));
    }} else {{
        let u = pts_unit(params.axis, vec3f(0.0, 0.0, 1.0));
        let h = 0.5 * params.length;
        let along = clamp(dot(q, u), -h, h);
        d = length(q - along * u) - params.radius;
    }}
    return d;
}}

fn main() {{
    let pts_d = pts_distance(position);
    var pts_w = select(0.0, 1.0, pts_d <= 0.0);
    if params.falloff > 0.0 {{
        pts_w = 1.0 - smoothstep(0.0, params.falloff, pts_d);
    }}
    if params.invert != 0 {{
        pts_w = 1.0 - pts_w;
    }}
    let pts_old = {target};
    if params.combine == combine_multiply {{
        pts_w = pts_old * pts_w;
    }} else if params.combine == combine_add {{
        pts_w = pts_old + pts_w;
    }} else if params.combine == combine_max {{
        pts_w = max(pts_old, pts_w);
    }} else if params.combine == combine_min {{
        pts_w = min(pts_old, pts_w);
    }}
    {target} = pts_w;
{write_distance}}}
"
    );
    let mut decls = vec![Decl::read("position"), Decl::write(target).ty(AttrType::F32).default([1.0; 4])];
    if let Some(d) = distance {
        decls.push(Decl::set(d).ty(AttrType::F32));
    }
    Spec {
        label: KIND.into(),
        source,
        decls,
        params: vec![
            ParamSpec::choice("shape", &SHAPES),
            ParamSpec::vec3("center", [0.0; 3]),
            ParamSpec::float("radius", 0.5),
            ParamSpec::vec3("size", [1.0; 3]),
            ParamSpec::vec3("normal", [0.0, 0.0, 1.0]),
            ParamSpec::vec3("axis", [0.0, 0.0, 1.0]),
            ParamSpec::float("length", 1.0),
            ParamSpec::float("falloff", 0.2),
            ParamSpec::bool("invert", false),
            ParamSpec::choice("combine", &COMBINES),
        ],
    }
}

/// Compiles the program of the defaults over the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let generated =
        program::generate(&spec("weight", None), &generated_shape(), program::storage_budget(gpu))?;
    Program::cached(gpu, generated).map(|_| ())
}

#[derive(Default)]
struct Field {
    own: Own,
}

impl Operator for Field {
    fn gpu_bytes(&self) -> u64 {
        self.own.bytes()
    }

    fn trim(&mut self) {
        self.own.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.own.status())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let target = attr_name(cx, "attribute")?;
        if let Some(a) = points.attr(target).filter(|a| a.ty() != AttrType::F32) {
            return Err(CookError::failed(format!(
                "`{target}` is {}; a weight is one number",
                a.ty().wgsl()
            )));
        }
        let distance =
            if cx.text("distance")?.trim().is_empty() { None } else { Some(attr_name(cx, "distance")?) };
        if distance == Some(target) {
            return Err(CookError::failed("`distance` and `attribute` name the same attribute"));
        }
        let spec = spec(target, distance);
        let param = |name: &str| cx.param(name).cloned();
        let out = self.own.run(cx, gpu, &spec, points, false, &param, &[])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(self.own.bounds(points.bounds())))]))
    }
}
