//! `pts.transform`: moves, turns and scales points about a pivot, TouchDesigner's Transform POP.
//! It acts on `position` and turns `normal` with it, or on another attribute of three numbers.
//! The matrix is made on the CPU; the program multiplies.

use penumbra_core::{Error, ParamSpec, Unit, Value};
use penumbra_gpu::Gpu;
use penumbra_gpu::geo::{Mat4, mul, normal_matrix};
use penumbra_gpu::points::AttrType;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use super::program::{Own, attr_name, generated_shape, input};
use crate::geo::{order_param, ordered};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.transform";

pub const MEANINGS: [&str; 2] = ["point", "direction"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(KIND, "Moves, turns and scales points about a pivot; turns their normals with them")
            .param(
                ParamSpec::vec3("translate", [0.0; 3])
                    .range(-10.0, 10.0)
                    .doc("Move along x, y, z (z up), in metres"),
            )
            .param(
                ParamSpec::vec3("rotate", [0.0; 3])
                    .range(-180.0, 180.0)
                    .unit(Unit::Degrees)
                    .doc("Turn about the pivot: about x, then y, then z"),
            )
            .param(
                ParamSpec::vec3("scale", [1.0; 3])
                    .range(0.0, 4.0)
                    .doc("Size along x, y, z around the pivot: 1 keeps the size, negative mirrors"),
            )
            .param(
                ParamSpec::float("uniform", 1.0)
                    .range(0.001, 1000.0)
                    .curve(penumbra_core::Curve::Log)
                    .doc("Times the scale along every axis: 0.001 turns millimetres into metres"),
            )
            .param(
                ParamSpec::vec3("pivot", [0.0; 3])
                    .range(-10.0, 10.0)
                    .doc("The point the turn and the scale happen about, in metres"),
            )
            .param(order_param("cloud", "the pivot", "world"))
            .param(
                ParamSpec::text("attribute", "position")
                    .more()
                    .doc("The attribute of three numbers that is transformed, TouchDesigner's names too (P, PartVel)"),
            )
            .param(
                ParamSpec::choice("meaning", &MEANINGS)
                    .more()
                    .doc("What the attribute's numbers are")
                    .choice_doc("point", "a place: it moves, turns and scales about the pivot")
                    .choice_doc("direction", "a direction or a speed, such as velocity: it turns and scales, translate and pivot do not count"),
            )
            .param(
                ParamSpec::bool("normals", true)
                    .more()
                    .doc("Turns `normal` with the points, where they have one, so lit copies stay right. Only when the attribute is position"),
            )
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts)
            .describe_output("out", "The points transformed, and the box around them with them"),
        || Box::new(Transform::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// The rows of the matrix and of the normals' matrix, for the program.
fn program_params() -> Vec<ParamSpec> {
    ["m0", "m1", "m2", "n0", "n1", "n2"].iter().map(|n| ParamSpec::vec4(n, [0.0; 4])).collect()
}

/// `m` applied to the point `p`.
fn apply(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = p.map(f64::from);
    let [a, b, c, _] = m;
    let row = |r: &[f64; 4]| (r[0] * x + r[1] * y + r[2] * z + r[3]) as f32;
    [row(a), row(b), row(c)]
}

/// The box around `b` moved by `m`: around its eight corners.
fn moved(b: Bounds, m: &Mat4) -> Option<Bounds> {
    let corners = (0..8u8).map(|k| {
        let pick = |bit: u8, axis: usize| if k & bit == 0 { b.min.get(axis) } else { b.max.get(axis) };
        [pick(1, 0), pick(2, 1), pick(4, 2)].map(|v| v.copied().unwrap_or(0.0))
    });
    Bounds::of(corners.map(|c| apply(m, c)))
}

fn vec3(cx: &CookContext<'_>, name: &str) -> Result<[f64; 3], CookError> {
    match cx.param(name).and_then(Value::components) {
        Some(&[x, y, z]) => Ok([x, y, z]),
        _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
    }
}

/// The program that transforms `target` (a `vec4f` with `vec4`, keeping its fourth number), as
/// a point or a direction, and turns `normal` with `turn_normals`.
fn spec(target: &str, vec4: bool, point: bool, turn_normals: bool) -> Spec {
    let w = if point { "1.0" } else { "0.0" };
    let at = if vec4 { format!("{target}.xyz") } else { target.to_string() };
    let moved_value = format!(
        "vec3f(dot(params.m0, vec4f({at}, {w})), dot(params.m1, vec4f({at}, {w})), dot(params.m2, vec4f({at}, {w})))"
    );
    let mut body = if vec4 {
        format!(
            "    {target} = vec4f({moved_value}, {target}.w);
"
        )
    } else {
        format!(
            "    {target} = {moved_value};
"
        )
    };
    let mut decls = vec![Decl::write(target)];
    if turn_normals {
        decls.push(Decl::write("normal"));
        // The normals' matrix (the cofactors) takes a normal to the one of the moved surface.
        // Where the transform squashes the points flat, it gives zero for a normal lying in
        // the flat plane: such a normal turns as a direction does. One left zero stays.
        body.push_str(
            "    let pts_n = vec3f(dot(params.n0.xyz, normal), dot(params.n1.xyz, normal), dot(params.n2.xyz, normal));
    let pts_d = vec3f(dot(params.m0.xyz, normal), dot(params.m1.xyz, normal), dot(params.m2.xyz, normal));
    let pts_t = select(pts_d, pts_n, dot(pts_n, pts_n) > 1e-12);
    normal = select(normal, normalize(pts_t), dot(pts_t, pts_t) > 1e-24);
",
        );
    }
    Spec {
        label: KIND.into(),
        source: format!(
            "fn main() {{
{body}}}
"
        ),
        decls,
        params: program_params(),
    }
}

/// Compiles the program of the defaults over the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let generated = program::generate(
        &spec("position", false, true, true),
        &generated_shape(),
        program::storage_budget(gpu),
    )?;
    Program::cached(gpu, generated).map(|_| ())
}

#[derive(Default)]
struct Transform {
    own: Own,
}

impl Operator for Transform {
    fn gpu_bytes(&self) -> u64 {
        self.own.bytes()
    }

    fn trim(&mut self) {
        self.own.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let target = attr_name(cx, "attribute")?;
        let point = cx.text("meaning")? == "point";
        let u = cx.float("uniform")?;
        let scale = vec3(cx, "scale")?.map(|s| s * u);
        let (translate, pivot) =
            if point { (vec3(cx, "translate")?, vec3(cx, "pivot")?) } else { ([0.0; 3], [0.0; 3]) };
        let m = ordered(cx.text("order")?, translate, vec3(cx, "rotate")?, scale);
        // About the pivot: to the origin, transformed, back.
        let [px, py, pz] = pivot;
        let to = |x: f64, y: f64, z: f64| {
            [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z], [0.0, 0.0, 0.0, 1.0]]
        };
        let m = mul(&to(px, py, pz), &mul(&m, &to(-px, -py, -pz)));
        if !m.iter().flatten().all(|v| v.is_finite()) {
            return Err(CookError::failed("the transform is not finite"));
        }
        let ty = points.attr(target).map(|a| a.ty());
        if !matches!(ty, Some(AttrType::Vec3 | AttrType::Vec4) | None) {
            return Err(CookError::failed(format!(
                "`{target}` is {}; pts.transform moves attributes of three numbers",
                ty.map_or("", AttrType::wgsl)
            )));
        }
        let vec4 = ty == Some(AttrType::Vec4);
        // Normals belong to the positions: they turn when the points move, not with another
        // attribute.
        let turn_normals = cx.bool("normals")? && target == "position" && points.attr("normal").is_some();
        let spec = spec(target, vec4, point, turn_normals);
        let row4 = |r: &[f64; 4]| Value::Vec4(*r);
        let row3 = |r: &[f64; 3]| Value::Vec4([r[0], r[1], r[2], 0.0]);
        let [m0, m1, m2, _] = &m;
        let [n0, n1, n2] = &normal_matrix(&m);
        let param = |name: &str| match name {
            "m0" => Some(row4(m0)),
            "m1" => Some(row4(m1)),
            "m2" => Some(row4(m2)),
            "n0" => Some(row3(n0)),
            "n1" => Some(row3(n1)),
            "n2" => Some(row3(n2)),
            _ => None,
        };
        let out = self.own.run(cx, gpu, &spec, points, false, &param, &[])?;
        let bounds = match points.bounds() {
            Some(b) if target == "position" => moved(b, &m),
            b => b,
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(bounds))]))
    }
}
