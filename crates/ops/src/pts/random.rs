//! `pts.random`: a random value per point into an attribute, TouchDesigner's Random POP:
//! uniform in a range, from a normal distribution, a direction or a point in a ball. The same
//! `seed` gives each point the same value every time.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_gpu::points::{AttrType, Points};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use super::program::{
    COMBINE, Own, attr_name, attr_type, combine, generated_shape, input, operation_param, type_param,
};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.random";

pub const DISTRIBUTIONS: [&str; 4] = ["uniform", "normal", "direction", "sphere"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "A random value per point into an attribute: random colours, sizes, directions or places, the same for the same seed",
        )
        .param(
            ParamSpec::text("attribute", "color")
                .doc("The attribute the values go into, TouchDesigner's names too (Color, PointScale, P). A new attribute is made"),
        )
        .param(
            ParamSpec::choice("distribution", &DISTRIBUTIONS)
                .doc("How the values spread")
                .choice_doc("uniform", "evenly between min and max, each number on its own")
                .choice_doc("normal", "around mean, most within deviation of it (a bell curve). A colour keeps its alpha")
                .choice_doc("direction", "a direction of length radius, evenly over all directions. A colour keeps its alpha")
                .choice_doc("sphere", "a point inside a ball of radius around 0, evenly spread. A colour keeps its alpha"),
        )
        .param(
            ParamSpec::vec4("min", [0.0, 0.0, 0.0, 1.0])
                .range(-1.0, 1.0)
                .doc("uniform: the lowest value of each number. An attribute of fewer numbers takes the first ones")
                .applies_when("distribution", &["uniform"]),
        )
        .param(
            ParamSpec::vec4("max", [1.0; 4])
                .range(-1.0, 1.0)
                .doc("uniform: the highest value of each number")
                .applies_when("distribution", &["uniform"]),
        )
        .param(
            ParamSpec::vec4("mean", [0.0; 4])
                .range(-1.0, 1.0)
                .doc("normal: the middle of the values")
                .applies_when("distribution", &["normal"]),
        )
        .param(
            ParamSpec::vec4("deviation", [1.0; 4])
                .range(0.0, 1.0)
                .doc("normal: how far the values spread, the standard deviation; about two in three lie within it")
                .applies_when("distribution", &["normal"]),
        )
        .param(
            ParamSpec::float("radius", 1.0)
                .range(0.0, 10.0)
                .doc("direction and sphere: the length of the directions, the radius of the ball")
                .applies_when("distribution", &["direction", "sphere"]),
        )
        .param(
            ParamSpec::int("seed", 0)
                .clamp(0.0, 1e9)
                .range(0.0, 100.0)
                .doc("Another number gives every point another value. Points with an `id` (particles) keep their value as long as they live: the values follow the id, not the place in the list"),
        )
        .param(operation_param("set", "the random value"))
        .param(type_param())
        .input("in", DataKind::Pts)
        .output("out", DataKind::Pts)
        .describe_output("out", "The points with the attribute set; the others pass on unchanged"),
        || Box::new(Random::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// The params the program reads. `min` and `max` are WGSL functions, so the program reads them
/// as `low` and `high`.
fn program_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::choice("distribution", &DISTRIBUTIONS),
        ParamSpec::vec4("low", [0.0; 4]),
        ParamSpec::vec4("high", [1.0; 4]),
        ParamSpec::vec4("mean", [0.0; 4]),
        ParamSpec::vec4("deviation", [1.0; 4]),
        ParamSpec::float("radius", 1.0),
        ParamSpec::int("seed", 0),
        ParamSpec::int("salt", 0),
        operation_param("set", "the random value"),
    ]
}

/// A number of the attribute's name that the program mixes into its random numbers, so two
/// attributes given random values with the same seed get unrelated ones, and none of them
/// follows `rand(k)` of `pts.math` or `pts.shader`. The same name always gives the same number
/// (FNV-1a), within the 31 bits an int param holds.
fn salt(name: &str) -> i64 {
    let h = name.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    i64::from(h & 0x7fff_ffff)
}

const RANDOM: &str = "
fn pts_random() -> vec4f {
    // Seed and attribute hashed into the key, so no other node's numbers line up with these.
    let k = pn_hash(vec3u(u32(max(params.seed, 0)), u32(max(params.salt, 0)), 0x1b873593u)).x;
    let u = vec4f(pts_rand3(k), pts_rand3(k + 1u).x);
    let tau = 6.2831853;
    var v = vec4f(0.0);
    if params.distribution == distribution_uniform {
        v = mix(params.low, params.high, u);
    } else if params.distribution == distribution_normal {
        // Box and Muller: two normal numbers from each two uniform ones; 1 - u is above 0.
        let r1 = sqrt(-2.0 * log(1.0 - u.x));
        let r2 = sqrt(-2.0 * log(1.0 - u.z));
        let z = vec4f(r1 * cos(tau * u.y), r1 * sin(tau * u.y), r2 * cos(tau * u.w), r2 * sin(tau * u.w));
        v = params.mean + params.deviation * z;
    } else {
        let h = 2.0 * u.x - 1.0;
        let phi = tau * u.y;
        let r = sqrt(max(1.0 - h * h, 0.0));
        var d = vec3f(r * cos(phi), r * sin(phi), h) * params.radius;
        if params.distribution == distribution_sphere {
            d *= pow(u.z, 1.0 / 3.0);
        }
        v = vec4f(d, 0.0);
    }
    return v;
}
";

/// The numbers of a vec4 param, as `f32`.
fn vec4(cx: &CookContext<'_>, name: &str) -> Result<[f32; 4], CookError> {
    match cx.param(name).and_then(Value::components) {
        Some(&[x, y, z, w]) => Ok([x as f32, y as f32, z as f32, w as f32]),
        _ => Err(CookError::failed(format!("`{name}` must be four numbers"))),
    }
}

/// Whether the random numbers of `points` follow their `id`: a whole number that names each
/// point (each particle) while its place in the list moves. Not where `target` is the id itself.
fn keyed_by_id(points: &Points, target: &str) -> bool {
    target != "id" && points.attr("id").is_some_and(|a| matches!(a.ty(), AttrType::U32 | AttrType::I32))
}

/// The program that puts a random value into `target`; with `keyed`, keyed by the `id` of each
/// point rather than its index.
fn spec(target: &str, ty: AttrType, rgb_only: bool, keyed: bool) -> Spec {
    let key = if keyed { "rand3_key(u32(id), k)" } else { "rand3(k)" };
    let source = format!(
        "fn pts_rand3(k: u32) -> vec3f {{
    return {key};
}}
{RANDOM}{COMBINE}
fn main() {{
    let pts_v = pts_random();
    {}
}}
",
        combine(target, ty, "pts_v", rgb_only)
    );
    let mut decls = vec![Decl::write(target).ty(ty)];
    if keyed {
        decls.push(Decl::read("id"));
    }
    Spec { label: KIND.into(), source, decls, params: program_params() }
}

/// Compiles the program of the defaults over the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let generated = program::generate(
        &spec("color", AttrType::Vec4, false, false),
        &generated_shape(),
        program::storage_budget(gpu),
    )?;
    Program::cached(gpu, generated).map(|_| ())
}

#[derive(Default)]
struct Random {
    own: Own,
}

impl Operator for Random {
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
        let ty = attr_type(points, target, cx.text("type")?);
        let distribution = cx.text("distribution")?;
        // Only uniform has a range per number, alpha included; the others keep a colour's alpha.
        let rgb_only = distribution != "uniform";
        let spec = spec(target, ty, rgb_only, keyed_by_id(points, target));
        let param = |name: &str| match name {
            "low" => cx.param("min").cloned(),
            "high" => cx.param("max").cloned(),
            "salt" => Some(Value::Int(salt(target))),
            other => cx.param(other).cloned(),
        };
        let out = self.own.run(cx, gpu, &spec, points, false, &param, &[])?;

        // The box the values lie in, where there is one: positions set lie in it, positions
        // added to move by at most it.
        let radius = cx.float("radius")?.abs() as f32;
        let range = match distribution {
            "uniform" => {
                let (lo, hi) = (vec4(cx, "min")?, vec4(cx, "max")?);
                Bounds::of([[lo[0], lo[1], lo[2]], [hi[0], hi[1], hi[2]]])
            }
            "direction" | "sphere" => Bounds::of([[-radius; 3], [radius; 3]]),
            _ => None,
        }
        .filter(|b| b.min.iter().chain(&b.max).all(|v| v.is_finite()));
        let bounds = match (points.bounds(), range, cx.text("operation")?) {
            (b, _, _) if target != "position" => b,
            (_, Some(r), "set") => Some(r),
            (Some(b), Some(r), "add") => Some(Bounds {
                min: [b.min[0] + r.min[0], b.min[1] + r.min[1], b.min[2] + r.min[2]],
                max: [b.max[0] + r.max[0], b.max[1] + r.max[1], b.max[2] + r.max[2]],
            }),
            _ => None,
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(self.own.bounds(bounds)))]))
    }
}
