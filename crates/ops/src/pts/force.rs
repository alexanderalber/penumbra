//! `pts.force`: a push on every point, added to its `force`, TouchDesigner's Force Radial POP
//! and what its Force POP does. Towards a point or away from it, towards a line, around a line
//! (a vortex), one way for all (gravity), or wind that pulls each point's speed towards its own.
//! Each grows weaker with distance as `falloff` says, and is multiplied by a mask attribute
//! (`weight` by default, as `pts.field` writes it).
//!
//! Put it in the loop of a `pts.particles`, between the particles and the end wired into
//! `previous`: the particles move by the force at their next step.

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::AttrType;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::{Own, attr_name, input};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.force";

/// Choices of `kind`.
pub const KINDS: [&str; 6] = ["attract", "repel", "axis", "vortex", "direction", "wind"];

/// Choices of `falloff`.
pub const FALLOFFS: [&str; 4] = ["none", "linear", "smooth", "inverse_square"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                KIND,
                "A push on every point, added to its force: towards a point or away, towards a line, around a line, one way for all, or wind. For the loop of pts.particles",
            )
            .param(
                ParamSpec::choice("kind", &KINDS)
                    .doc("Which way the force pushes")
                    .choice_doc("attract", "towards `center`")
                    .choice_doc("repel", "away from `center`")
                    .choice_doc("axis", "towards the line through `center` along `axis`")
                    .choice_doc("vortex", "around the line through `center` along `axis`, anticlockwise seen from where `axis` points. A negative strength turns the other way")
                    .choice_doc("direction", "along `direction`, the same everywhere: gravity, a steady push")
                    .choice_doc("wind", "towards the speed `direction` (metres a second): each point is pulled to move with the wind, harder the more its `velocity` differs. In pts.particles a particle never passes the wind's speed, however light it is. A negative strength pushes nothing"),
            )
            .param(
                ParamSpec::float("strength", 1.0)
                    .range(-10.0, 10.0)
                    .doc("How hard it pushes, in newtons: a particle of mass 1 gains this many metres a second each second. Negative turns it round, except for wind. For wind it is per metre a second of difference in speed"),
            )
            .param(
                ParamSpec::vec3("center", [0.0; 3])
                    .range(-5.0, 5.0)
                    .doc("The point the force comes from or goes to, and where its distance is measured from, in metres"),
            )
            .param(
                ParamSpec::vec3("axis", [0.0, 0.0, 1.0])
                    .range(-1.0, 1.0)
                    .doc("axis and vortex: the line's direction through `center`; its length does not count")
                    .applies_when("kind", &["axis", "vortex"]),
            )
            .param(
                ParamSpec::vec3("direction", [0.0, 0.0, -1.0])
                    .range(-10.0, 10.0)
                    .doc("direction: the way it pushes; its length does not count. wind: the wind's speed, in metres a second")
                    .applies_when("kind", &["direction", "wind"]),
            )
            .param(
                ParamSpec::choice("falloff", &FALLOFFS)
                    .doc("How the force weakens with the distance from `center` (from the line for axis and vortex)")
                    .choice_doc("none", "as strong everywhere")
                    .choice_doc("linear", "full at the centre, nothing from `radius` on, in a straight line between")
                    .choice_doc("smooth", "as linear, with soft ends")
                    .choice_doc("inverse_square", "half at `radius`, ever weaker beyond, never quite nothing"),
            )
            .param(
                ParamSpec::float("radius", 1.0)
                    .clamp(1e-6, 1e6)
                    .range(0.01, 10.0)
                    .curve(Curve::Log)
                    .doc("The distance the falloff is measured in, in metres")
                    .applies_when("falloff", &["linear", "smooth", "inverse_square"]),
            )
            .param(
                ParamSpec::text("mask", "weight")
                    .doc("The force is multiplied by this attribute of each point: `weight` follows a pts.field. Points without it count as 1. Empty: no mask"),
            )
            .param(
                ParamSpec::text("attribute", "force")
                    .more()
                    .doc("The attribute the force is added to. velocity changes the speed at once, by this much at every step; wind then moves the speed at most all the way to the wind's"),
            )
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts)
            .describe_output("out", "The points with the force added; the others pass on unchanged. Wind into the `force` of particles also adds to `pts_wind`, how hard it pulls each point: pts.particles reads it to step the speed without overshooting"),
            || Box::new(Force::default()) as Box<dyn Operator>,
        )
        .warm_up(on_gpu(warm_up)),
    )
}

/// The attribute wind adds its pull to (per metre a second of difference in speed), besides
/// `force`: `pts.particles` divides the step's change of speed by `1 + pull * dt / mass`, an
/// implicit step for the wind, so a light particle never passes the wind's speed.
pub(crate) const WIND: &str = "pts_wind";

/// The force of each kind, its falloff and mask, added to `target`. `carry`: wind into `force`
/// also adds its pull to [`WIND`].
fn spec(target: &str, mask: Option<&str>, wind: bool, carry: bool) -> Spec {
    let m = mask.map_or("1.0".to_string(), |m| format!("f32({m})"));
    // Wind pulls towards the wind's speed, never away from it: a negative pull would grow
    // without end. Into `velocity` it is a change per step, at most all the way.
    let push = match (wind, target) {
        (false, _) => format!("{target} += pts_dir * params.strength * pts_falloff(pts_d) * {m};"),
        (true, "velocity") => "velocity += pts_dir * min(pts_k, 1.0);".to_string(),
        (true, "force") if carry => format!(
            "force += pts_dir * pts_k;
    {WIND} += pts_k;"
        ),
        (true, _) => format!("{target} += pts_dir * pts_k;"),
    };
    let pull = if wind {
        format!("let pts_k = max(params.strength * pts_falloff(pts_d) * {m}, 0.0);")
    } else {
        String::new()
    };
    let source = format!(
        "
fn pts_falloff(d: f32) -> f32 {{
    let r = max(params.radius, 1e-6);
    var f = 1.0;
    if params.falloff == falloff_linear {{
        f = clamp(1.0 - d / r, 0.0, 1.0);
    }} else if params.falloff == falloff_smooth {{
        f = 1.0 - smoothstep(0.0, r, d);
    }} else if params.falloff == falloff_inverse_square {{
        f = 1.0 / (1.0 + (d / r) * (d / r));
    }}
    return f;
}}

fn pts_unit(v: vec3f) -> vec3f {{
    let l = length(v);
    return select(vec3f(0.0), v / l, l > 1e-9);
}}

fn main() {{
    let pts_rel = position - params.center;
    let pts_axis = pts_unit(params.axis);
    // Away from the line through the centre along the axis.
    let pts_out = pts_rel - dot(pts_rel, pts_axis) * pts_axis;
    var pts_d = length(pts_rel);
    var pts_dir = vec3f(0.0);
    if params.kind == kind_attract {{
        pts_dir = -pts_unit(pts_rel);
    }} else if params.kind == kind_repel {{
        pts_dir = pts_unit(pts_rel);
    }} else if params.kind == kind_axis {{
        pts_d = length(pts_out);
        pts_dir = -pts_unit(pts_out);
    }} else if params.kind == kind_vortex {{
        pts_d = length(pts_out);
        pts_dir = pts_unit(cross(pts_axis, pts_out));
    }} else if params.kind == kind_direction {{
        pts_dir = pts_unit(params.direction);
    }} else {{
        pts_dir = params.direction - {velocity};
    }}
    {pull}
    {push}
}}
",
        velocity = if wind { "velocity" } else { "vec3f(0.0)" },
    );
    let mut decls = vec![Decl::read("position"), Decl::write(target).ty(AttrType::Vec3)];
    if wind && target != "velocity" {
        decls.push(Decl::read("velocity"));
    }
    if wind && carry && target == "force" {
        decls.push(Decl::write(WIND).ty(AttrType::F32).default([0.0; 4]));
    }
    if let Some(m) = mask {
        decls.push(Decl::read(m));
    }
    Spec {
        label: KIND.into(),
        source,
        decls,
        params: vec![
            ParamSpec::choice("kind", &KINDS),
            ParamSpec::float("strength", 1.0),
            ParamSpec::vec3("center", [0.0; 3]),
            ParamSpec::vec3("axis", [0.0, 0.0, 1.0]),
            ParamSpec::vec3("direction", [0.0, 0.0, -1.0]),
            ParamSpec::choice("falloff", &FALLOFFS),
            ParamSpec::float("radius", 1.0),
        ],
    }
}

/// Compiles the program of the defaults over particles.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let shape = program::Shape::new(&[
        ("position", AttrType::Vec3),
        ("force", AttrType::Vec3),
        ("velocity", AttrType::Vec3),
    ]);
    for wind in [false, true] {
        let generated =
            program::generate(&spec("force", None, wind, wind), &shape, program::storage_budget(gpu))?;
        Program::cached(gpu, generated)?;
    }
    Ok(())
}

#[derive(Default)]
struct Force {
    own: Own,
}

impl Operator for Force {
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
        if let Some(a) = points.attr(target).filter(|a| a.ty() != AttrType::Vec3) {
            return Err(CookError::failed(format!(
                "`{target}` is {}; a force goes into an attribute of three numbers",
                a.ty().wgsl()
            )));
        }
        // A mask the points lack counts as 1: no binding for it.
        let mask_name = penumbra_gpu::points::canonical(cx.text("mask")?);
        let mask = match points.attr(mask_name) {
            _ if mask_name.is_empty() => None,
            Some(a) if a.ty().components() == 1 => Some(attr_name(cx, "mask")?),
            Some(a) => {
                return Err(CookError::failed(format!(
                    "the mask `{mask_name}` is {}; it must be one number",
                    a.ty().wgsl()
                )));
            }
            None => None,
        };
        let wind = cx.text("kind")? == "wind";
        // The pull is for pts.particles, which reads it from points that come round its loop:
        // those carry `force` from the particles. Points without it (points that are no
        // particles) get no pull to carry on.
        let pull = wind && points.attr("force").is_some();
        let spec = spec(target, mask, wind, pull);
        let param = |name: &str| cx.param(name).cloned();
        let out = self.own.run(cx, gpu, &spec, points, false, &param, &[])?;
        let bounds = if target == "position" { None } else { points.bounds() };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(self.own.bounds(bounds)))]))
    }
}
