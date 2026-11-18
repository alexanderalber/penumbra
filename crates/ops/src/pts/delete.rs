//! `pts.delete`: drops points, TouchDesigner's Delete POP. Which ones: those an expression you
//! type holds true for (`weight <= 0.0`, `position.z < a`, `rand(0) < 0.5`), a range of
//! indices, those inside a box or a ball, or a random share. `invert` keeps them instead.
//!
//! The rest keep their order and every attribute ([`Compactor`]). Their number is known on the
//! GPU only: the nodes downstream work over as many points as there were and stop at the live
//! count, the viewer shows it a frame late, `chan.from_pts` turns it into a channel. The box of
//! the input stays the box of the output. Rows and columns (`dims`) are gone.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::compact::Compactor;
use penumbra_gpu::points::program::{self, Decl, Program, Shape, Spec};
use penumbra_gpu::points::{AttrType, Points, default_of};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::{Compiling, Own, generated_shape, input};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.delete";

/// Choices of `by`.
pub const BY: [&str; 5] = ["expression", "index", "box", "sphere", "random"];

/// The attribute the flag program writes: 1 keeps a point. Never part of the output.
pub(crate) const KEEP: &str = "pts_keep";

/// Longest expression.
const MAX_EXPRESSION: usize = 4096;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                KIND,
                "Drops points: those an expression you type holds true for, a range of indices, those inside a box or a ball, or a random share. The rest keep their order and attributes",
            )
            .param(
                ParamSpec::choice("by", &BY)
                    .doc("Which points are deleted")
                    .choice_doc("expression", "those for which `expression` is true")
                    .choice_doc("index", "those from index `first` to `last`, every `every`-th of them")
                    .choice_doc("box", "those inside the box of `size` around `center`")
                    .choice_doc("sphere", "those inside the ball of `radius` around `center`")
                    .choice_doc("random", "a random share of them, `share` of all, the same for the same `seed`"),
            )
            .param(
                ParamSpec::text("expression", "weight <= 0.0")
                    .lines(2)
                    .doc("A WGSL expression worked out once per point; where it is true (or not 0) the point goes. It can name attributes (position, weight, P), index, count, index01, time, rand(k) and WGSL's functions: position.z < 0.0, rand(0) < 0.5, index % 2u == 0u. The default deletes the points of weight 0, such as the empty texels of pts.from_tex, and keeps every other point")
                    .applies_when("by", &["expression"]),
            )
            .param(
                ParamSpec::int("first", 0)
                    .clamp(0.0, f64::from(u32::MAX))
                    .range(0.0, 10_000.0)
                    .doc("index: the first index deleted")
                    .applies_when("by", &["index"]),
            )
            .param(
                ParamSpec::int("last", 0)
                    .clamp(0.0, f64::from(u32::MAX))
                    .range(0.0, 10_000.0)
                    .doc("index: the last index deleted, itself included")
                    .applies_when("by", &["index"]),
            )
            .param(
                ParamSpec::int("every", 1)
                    .clamp(1.0, f64::from(u32::MAX))
                    .range(1.0, 16.0)
                    .doc("index: 1 deletes every point of the range, 2 every second one starting at `first`, and so on")
                    .applies_when("by", &["index"]),
            )
            .param(
                ParamSpec::vec3("center", [0.0; 3])
                    .range(-1.0, 1.0)
                    .doc("box and sphere: the middle, in metres")
                    .applies_when("by", &["box", "sphere"]),
            )
            .param(
                ParamSpec::vec3("size", [1.0; 3])
                    .range(0.0, 10.0)
                    .doc("box: its size along x, y and z, in metres")
                    .applies_when("by", &["box"]),
            )
            .param(
                ParamSpec::float("radius", 0.5)
                    .clamp(0.0, 1e9)
                    .range(0.0, 10.0)
                    .doc("sphere: its radius, in metres")
                    .applies_when("by", &["sphere"]),
            )
            .param(
                ParamSpec::float("share", 0.5)
                    .clamp(0.0, 1.0)
                    .range(0.0, 1.0)
                    .doc("random: the part of the points deleted, 0.25 for a quarter. Each point is drawn on its own, so the number varies a little")
                    .applies_when("by", &["random"]),
            )
            .param(
                ParamSpec::int("seed", 0)
                    .clamp(0.0, 1e9)
                    .range(0.0, 100.0)
                    .doc("random: another seed deletes other points")
                    .applies_when("by", &["random"]),
            )
            .param(ParamSpec::bool("invert", false).doc(
                "Keep the points the choice above names, and delete every other one",
            ))
            .param(
                ParamSpec::float("a", 0.0)
                    .range(-1.0, 1.0)
                    .more()
                    .doc("expression: a number the expression reads as a, such as a height in position.z < a. Bind it to a channel to move it")
                    .applies_when("by", &["expression"]),
            )
            .param(
                ParamSpec::float("b", 0.0)
                    .range(-1.0, 1.0)
                    .more()
                    .doc("expression: a number the expression reads as b")
                    .applies_when("by", &["expression"]),
            )
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts)
            .describe_output(
                "out",
                "The points that are left, in their order, with every attribute. Their number is on the GPU: the viewer shows it a frame late, chan.from_pts makes it a channel",
            ),
            || Box::new(Delete::default()) as Box<dyn Operator>,
        )
        .warm_up(on_gpu(warm_up)),
    )
}

/// The program that flags the points kept, for every choice of `by` but `expression`: one source
/// per choice, its numbers as params.
fn fixed(by: &str) -> Spec {
    let hit = match by {
        "index" => {
            "index >= u32(params.lo) && index <= u32(params.hi) && (index - u32(params.lo)) % u32(max(params.every, 1)) == 0u"
        }
        "box" => "all(abs(position - params.center) <= abs(params.extent) * 0.5)",
        "sphere" => "distance(position, params.center) <= params.radius",
        _ => "rand(u32(params.salt)) < params.share",
    };
    let reads = matches!(by, "box" | "sphere");
    let mut decls = vec![Decl::set(KEEP).ty(AttrType::U32)];
    if reads {
        decls.push(Decl::read("position"));
    }
    Spec {
        label: KIND.into(),
        source: flag_main(hit),
        decls,
        params: vec![
            ParamSpec::int("lo", 0),
            ParamSpec::int("hi", 0),
            ParamSpec::int("every", 1),
            ParamSpec::vec3("center", [0.0; 3]),
            ParamSpec::vec3("extent", [1.0; 3]),
            ParamSpec::float("radius", 0.5),
            ParamSpec::float("share", 0.5),
            ParamSpec::int("salt", 0),
            ParamSpec::int("flip", 0),
        ],
    }
}

/// `fn main` that keeps a point where `hit` (a WGSL expression, a bool or a number) is false, or
/// true with `params.flip`.
fn flag_main(hit: &str) -> String {
    format!(
        "fn main() {{\n    let pts_hit = bool(\n{hit}\n    );\n    {KEEP} = select(0u, 1u, pts_hit == (params.flip != 0));\n}}\n"
    )
}

/// The program for a typed expression over `points`.
fn typed(expression: &str, points: &Points) -> Result<Spec, Error> {
    if expression.trim().is_empty() {
        return Err(Error::new("the expression is empty"));
    }
    if expression.len() > MAX_EXPRESSION {
        return Err(Error::new(format!("the expression is longer than {MAX_EXPRESSION} characters")));
    }
    let is_attr = |n: &str| n != KEEP && (points.attr(n).is_some() || default_of(n).is_some());
    let (expression, reads) = super::math::scan(expression, is_attr);
    let mut decls: Vec<Decl> = reads.iter().map(|n| Decl::read(n)).collect();
    decls.push(Decl::set(KEEP).ty(AttrType::U32));
    Ok(Spec {
        label: KIND.into(),
        source: flag_main(&expression),
        decls,
        params: ["a", "b", "c", "d"]
            .iter()
            .map(|v| ParamSpec::float(v, 0.0))
            .chain([ParamSpec::int("flip", 0)])
            .collect(),
    })
}

/// A number from the seed, so two seeds give unrelated choices.
fn salt(seed: i64) -> i64 {
    let h = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5bd1_e995;
    i64::from((h >> 33) as u32 & 0x7fff_ffff)
}

/// Compiles the program of the box, the choice that needs positions.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    for by in ["box", "index"] {
        let generated = program::generate(&fixed(by), &generated_shape(), program::storage_budget(gpu))?;
        Program::cached(gpu, generated)?;
    }
    Ok(())
}

#[derive(Default)]
struct Delete {
    own: Own,
    compiling: Compiling,
    compactor: Compactor,
    /// Whether the last cook ran a typed expression.
    typed: bool,
}

impl Operator for Delete {
    fn gpu_bytes(&self) -> u64 {
        self.own.bytes() + self.compiling.bytes() + self.compactor.bytes()
    }

    fn trim(&mut self) {
        self.own.trim();
        self.compiling.trim();
        self.compactor.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(if self.typed { self.compiling.status() } else { self.own.status() })
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.typed && self.compiling.latest().is_some_and(|p| p.uses_time()))
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let by = cx.text("by")?;
        let flip = i64::from(cx.bool("invert")?);
        self.typed = by == "expression";
        let flagged = if self.typed {
            self.compiling.poll();
            let made = typed(cx.text("expression")?, points)
                .and_then(|s| program::generate(&s, &Shape::of(points), program::storage_budget(gpu)));
            let made_error = match made {
                Ok(g) => {
                    self.compiling.want(cx, gpu, g);
                    None
                }
                Err(e) => Some(e.0),
            };
            let problem = made_error.or(self.compiling.error().map(str::to_string));
            let Some(program) = self.compiling.current(points) else {
                return Err(match problem {
                    Some(e) => CookError::failed(e),
                    None => CookError::Pending,
                });
            };
            if let Some(e) = problem {
                cx.warn(format!("{e}\n(running the last expression that compiled)"));
            }
            // `c` and `d` are no params of this node: they read 0.
            let param = |name: &str| match name {
                "flip" => Some(Value::Int(flip)),
                "a" | "b" => cx.param(name).cloned(),
                _ => None,
            };
            self.compiling.run(cx, gpu, &program, points, &param, &[])?
        } else {
            let first = cx.int("first")?.max(0);
            let last = cx.int("last")?.max(0);
            let seed = cx.int("seed")?;
            let param = |name: &str| match name {
                "lo" => Some(Value::Int(first.min(i64::from(i32::MAX)))),
                "hi" => Some(Value::Int(last.min(i64::from(i32::MAX)))),
                "every" => cx.param("every").cloned(),
                "extent" => cx.param("size").cloned(),
                "salt" => Some(Value::Int(salt(seed))),
                "flip" => Some(Value::Int(flip)),
                other => cx.param(other).cloned(),
            };
            self.own.run(cx, gpu, &fixed(by), points, false, &param, &[])?
        };
        let Some(keep) = flagged.attr(KEEP) else {
            return Err(CookError::failed("the flags of the points kept are missing"));
        };
        let out = self.compactor.compact(gpu, points, keep.buffer())?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
