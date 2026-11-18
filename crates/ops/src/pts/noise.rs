//! `pts.noise`: smooth random values over space and time, looked up at each point's position (or
//! another attribute) and added to, set into or multiplied with an attribute. TouchDesigner's
//! Noise POP. One number, three numbers or the curl of three (a flow without sources or sinks);
//! along the normal on request.
//!
//! The noise is four-dimensional: three of space and one of time, so with a `speed` the field
//! changes in place rather than sliding.

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::AttrType;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use super::program::{
    COMBINE, Own, attr_name, attr_type, combine, generated_shape, input, operation_param, type_param,
};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.noise";

pub const BASES: [&str; 2] = ["gradient", "value"];
pub const FLAVOURS: [&str; 3] = ["scalar", "vector", "curl"];

/// The params the program reads.
fn program_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::choice("basis", &BASES),
        ParamSpec::choice("flavour", &FLAVOURS),
        ParamSpec::int("seed", 0),
        ParamSpec::float("period", 1.0),
        ParamSpec::int("octaves", 1),
        ParamSpec::float("roughness", 0.5),
        ParamSpec::float("lacunarity", 2.0),
        ParamSpec::float("amplitude", 0.1),
        ParamSpec::float("offset", 0.0),
        ParamSpec::float("speed", 0.0),
        ParamSpec::vec3("translate", [0.0; 3]),
        operation_param("add", "the noise"),
    ]
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Smooth random values over space and time, looked up at each point and added to an attribute: points that wobble, drift or flow, colours that vary",
        )
        .param(
            ParamSpec::choice("flavour", &FLAVOURS)
                .doc("What the noise gives each point")
                .choice_doc("scalar", "one number, the same in x, y and z: points move along a diagonal, or along the normal")
                .choice_doc("vector", "three independent numbers: points move every which way")
                .choice_doc("curl", "the curl of three noises: a flow that neither gathers nor spreads points, for smoke and swarms. Its size follows the period less tightly"),
        )
        .param(
            ParamSpec::choice("basis", &BASES)
                .doc("The kind of noise")
                .choice_doc("gradient", "smooth, round blobs (Perlin-like)")
                .choice_doc("value", "blockier, with a visible grid of cells"),
        )
        .param(
            ParamSpec::float("amplitude", 0.1)
                .range(0.0, 2.0)
                .doc("The noise (between -1 and 1) is multiplied by this, in the attribute's units: metres for a position"),
        )
        .param(
            ParamSpec::float("period", 1.0)
                .clamp(1e-6, 1e6)
                .range(0.01, 10.0)
                .curve(Curve::Log)
                .doc("The size of one bump of the noise, in metres of the lookup attribute: smaller is finer"),
        )
        .param(
            ParamSpec::float("speed", 0.0)
                .range(0.0, 4.0)
                .doc("How fast the noise changes, in bumps per second; 0 is still. It changes in place rather than sliding"),
        )
        .param(
            ParamSpec::int("octaves", 1)
                .clamp(1.0, 8.0)
                .doc("Layers of ever finer noise added up, TouchDesigner's Harmonics; 1 is smooth bumps only"),
        )
        .param(
            ParamSpec::float("roughness", 0.5)
                .range(0.0, 1.0)
                .doc("Strength of each octave relative to the one before, TouchDesigner's Harmonic Gain"),
        )
        .param(
            ParamSpec::float("lacunarity", 2.0)
                .range(1.0, 4.0)
                .more()
                .doc("How much finer each octave is than the one before, TouchDesigner's Harmonic Spread"),
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .range(-1.0, 1.0)
                .doc("Added to the noise after amplitude: with multiply, 1 keeps the attribute around its value"),
        )
        .param(
            ParamSpec::vec3("translate", [0.0; 3])
                .range(-10.0, 10.0)
                .more()
                .doc("Moves the noise along x, y, z, in metres of the lookup attribute"),
        )
        .param(
            ParamSpec::int("seed", 0)
                .clamp(0.0, 1e9)
                .range(0.0, 100.0)
                .doc("Picks another noise of the same look"),
        )
        .param(
            ParamSpec::text("lookup", "position")
                .doc("The attribute the noise is looked up at, TouchDesigner's names too (P). Its first three numbers count, in metres"),
        )
        .param(
            ParamSpec::text("attribute", "position")
                .doc("The attribute the noise goes into, TouchDesigner's names too (P, Color, PointScale). A colour keeps its alpha. A new attribute is made"),
        )
        .param(operation_param("add", "the noise"))
        .param(
            ParamSpec::bool("along_normal", false)
                .doc("The noise moves the attribute along each point's `normal`: a scalar noise by its value, a vector or curl noise by its part along the normal. For a three-number attribute such as position"),
        )
        .param(type_param())
        .input("in", DataKind::Pts)
        .output("out", DataKind::Pts)
        .describe_output("out", "The points with the attribute changed; the others pass on unchanged"),
        || Box::new(Noise::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// 4D gradient and value noise: three dimensions of space, one of time. Clamped to -1..1, so
/// the box around moved points is known.
const NOISE: &str = "
fn pts_hash4(c: vec4i, seed: u32) -> vec3u {
    let s = pn_hash(vec3u(seed, bitcast<u32>(c.w), 0x5bd1e995u));
    return pn_hash(bitcast<vec3u>(c.xyz) ^ s);
}

fn pts_corners(n: array<f32, 16>, u: vec4f) -> f32 {
    var m: array<f32, 8>;
    for (var k = 0u; k < 8u; k++) {
        m[k] = mix(n[k], n[k + 8u], u.w);
    }
    let x0 = mix(mix(m[0], m[1], u.x), mix(m[2], m[3], u.x), u.y);
    let x1 = mix(mix(m[4], m[5], u.x), mix(m[6], m[7], u.x), u.y);
    return mix(x0, x1, u.z);
}

fn pts_noise4(p: vec4f, seed: u32, value: bool) -> f32 {
    let i = vec4i(floor(p));
    let f = p - floor(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    var n: array<f32, 16>;
    for (var k = 0u; k < 16u; k++) {
        let o = vec4i(i32(k & 1u), i32((k >> 1u) & 1u), i32((k >> 2u) & 1u), i32(k >> 3u));
        let h = pts_hash4(i + o, seed);
        // 24 bits per component, -1..1.
        let g = vec4f(vec3f(h >> vec3u(8u)), f32((h.x ^ h.z) & 0xffffffu)) * (2.0 / 16777215.0) - 1.0;
        if value {
            n[k] = g.x;
        } else {
            n[k] = dot(g, f - vec4f(o));
        }
    }
    return pts_corners(n, u);
}

fn pts_fbm(p: vec4f, seed: u32) -> f32 {
    let value = params.basis == basis_value;
    let octaves = clamp(params.octaves, 1, 8);
    var sum = 0.0;
    var total = 0.0;
    var weight = 1.0;
    var q = p;
    for (var o = 0; o < octaves; o++) {
        sum += weight * pts_noise4(q, seed + u32(o) * 0x9e3779b9u, value);
        total += weight;
        weight *= params.roughness;
        q *= params.lacunarity;
    }
    return clamp(sum / max(total, 1e-6), -1.0, 1.0);
}

fn pts_fbm3(p: vec4f, seed: u32) -> vec3f {
    return vec3f(pts_fbm(p, seed), pts_fbm(p, seed ^ 0x68bc21ebu), pts_fbm(p, seed ^ 0x02e5be93u));
}

fn pts_curl(p: vec4f, seed: u32) -> vec3f {
    let e = 0.01;
    let dx = pts_fbm3(p + vec4f(e, 0.0, 0.0, 0.0), seed) - pts_fbm3(p - vec4f(e, 0.0, 0.0, 0.0), seed);
    let dy = pts_fbm3(p + vec4f(0.0, e, 0.0, 0.0), seed) - pts_fbm3(p - vec4f(0.0, e, 0.0, 0.0), seed);
    let dz = pts_fbm3(p + vec4f(0.0, 0.0, e, 0.0), seed) - pts_fbm3(p - vec4f(0.0, 0.0, e, 0.0), seed);
    // A quarter: about the size of the other flavours.
    return 0.25 * vec3f(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x) / (2.0 * e);
}

/// The noise at `at` (in metres): one number in all three for scalar.
fn pts_field(at: vec3f) -> vec3f {
    let q = vec4f((at - params.translate) / max(abs(params.period), 1e-6), time * params.speed);
    let seed = u32(max(params.seed, 0));
    var e = vec3f(0.0);
    if params.flavour == flavour_scalar {
        e = vec3f(pts_fbm(q, seed));
    } else if params.flavour == flavour_vector {
        e = pts_fbm3(q, seed);
    } else {
        e = pts_curl(q, seed);
    }
    return e;
}
";

/// The WGSL for an attribute of type `ty` as a position to look up at.
fn as_vec3(name: &str, ty: AttrType) -> String {
    match ty {
        AttrType::F32 => format!("vec3f({name}, 0.0, 0.0)"),
        AttrType::Vec2 => format!("vec3f({name}, 0.0)"),
        AttrType::Vec3 => name.to_string(),
        AttrType::Vec4 => format!("{name}.xyz"),
        AttrType::U32 | AttrType::I32 => format!("vec3f(f32({name}), 0.0, 0.0)"),
    }
}

/// The program that looks the noise up at `lookup` and puts it into `target`.
fn spec(lookup: &str, lookup_ty: AttrType, target: &str, ty: AttrType, along: bool) -> Spec {
    let value = if along {
        "vec4f((params.amplitude * select(dot(pts_e, normal), pts_e.x, params.flavour == flavour_scalar) + params.offset) * normal, 0.0)"
    } else {
        "vec4f(params.amplitude * pts_e + params.offset, 0.0)"
    };
    let source = format!(
        "{NOISE}{COMBINE}
fn main() {{
    let pts_e = pts_field({});
    let pts_v = {value};
    {}
}}
",
        as_vec3(lookup, lookup_ty),
        combine(target, ty, "pts_v", true),
    );
    let mut decls = vec![Decl::read(lookup), Decl::write(target).ty(ty)];
    if along {
        decls.push(Decl::read("normal"));
    }
    Spec { label: KIND.into(), source, decls, params: program_params() }
}

/// Compiles the program of the defaults over the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let spec = spec("position", AttrType::Vec3, "position", AttrType::Vec3, false);
    let generated = program::generate(&spec, &generated_shape(), program::storage_budget(gpu))?;
    Program::cached(gpu, generated).map(|_| ())
}

#[derive(Default)]
struct Noise {
    own: Own,
    /// Whether the last cook had a speed, so the noise changes with time.
    moving: bool,
}

impl Operator for Noise {
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
        let lookup = attr_name(cx, "lookup")?;
        let target = attr_name(cx, "attribute")?;
        let along = cx.bool("along_normal")?;
        let ty = attr_type(points, target, cx.text("type")?);
        let lookup_ty = attr_type(points, lookup, "auto");
        if along && ty != AttrType::Vec3 {
            return Err(CookError::failed(format!(
                "along_normal moves an attribute of three numbers, such as position; `{target}` is {}",
                ty.wgsl()
            )));
        }
        if along && points.attr("normal").is_none() {
            cx.warn("along_normal: the points have no `normal`, so they move along +z, the default normal");
        }
        let spec = spec(lookup, lookup_ty, target, ty, along);
        self.moving = cx.float("speed")? != 0.0;
        let param = |name: &str| cx.param(name).cloned();
        let out = self.own.run(cx, gpu, &spec, points, self.moving, &param, &[])?;

        // Added noise moves each position by at most |amplitude| + |offset| per axis; other
        // ways leave no box worth keeping.
        let added = cx.text("operation")? == "add" && cx.text("flavour")? != "curl" && !along;
        let reach = (cx.float("amplitude")?.abs() + cx.float("offset")?.abs()) as f32;
        let bounds = match points.bounds() {
            Some(b) if target != "position" => Some(b),
            Some(b) if added && reach.is_finite() => Some(grow(b, [reach; 3])),
            _ => None,
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(self.own.bounds(bounds)))]))
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.moving)
    }
}

/// `b` grown by `by` along each axis, both ways.
pub(crate) fn grow(b: Bounds, by: [f32; 3]) -> Bounds {
    let [x, y, z] = by;
    Bounds {
        min: [b.min[0] - x, b.min[1] - y, b.min[2] - z],
        max: [b.max[0] + x, b.max[1] + y, b.max[2] + z],
    }
}
