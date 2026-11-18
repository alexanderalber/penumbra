//! `pts.particles`: points that are born, move and die, TouchDesigner's Particle POP.
//!
//! Each step the node takes the particles it gave out a step ago, ages them, moves them and lets
//! those whose `age` reached their `life` go, keeping the order of the rest; then it adds the
//! births of the step at the end. New particles start on the points wired into `source`, with
//! their attributes (colour, scale, ...), and get an `id` that no particle of the run had before,
//! an `age` of 0, a `life` and a `velocity`.
//!
//! The loop: wire the end of a chain that starts at this node back into `previous`, as for
//! `tex.feedback`. The chain's nodes add to `force` (`pts.force`, `pts.noise` with attribute
//! `force`) or change `velocity`, `position` or anything else; the next step starts from what the
//! end of the chain made. The chain must keep the particles as they came: their count and their
//! order. A node that drops points (`pts.delete`) breaks that, and the node says so and steps on
//! from its own last result. Without anything wired into `previous` the node steps on its own.
//!
//! Moving, when `integrate` is on: `velocity += (gravity + force / mass) * dt`, the speed then
//! shrinks by `exp(-drag * dt)`, and `position += velocity * dt`. The node gives out `force` as 0
//! for every particle, so the chain adds this step's forces afresh.
//!
//! Time follows the show clock: one step per frame, as long as the frame (at most
//! [`MAX_DT`]); none while the show stands still. So a run of the same steps gives the same
//! particles. Every random number of a particle is keyed by its `id`, not by its place in the
//! list, so it stays with the particle while those before it die.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use penumbra_core::{Curve, Error, ParamSpec, Tick, Value};
use penumbra_gpu::points::append::{self, Births, Item, Pick};
use penumbra_gpu::points::compact::Compactor;
use penumbra_gpu::points::program::{self, Decl, Program, Shape, Spec};
use penumbra_gpu::points::{Attr, AttrType, Count, Points, Seen, default_of, most_elements};
use penumbra_gpu::{Buffer, BufferRing, Gpu};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::delete::KEEP;
use super::force::WIND;
use super::program::Own;
use crate::tex::fluid::{MAX_DT, step};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.particles";

/// Choices of `emission`.
pub const EMISSIONS: [&str; 2] = ["rate", "per_point"];

/// The default of `max`.
pub const DEFAULT_MAX: u32 = 100_000;

/// The attributes every particle has, with their types.
const OWN: [(&str, AttrType); 5] = [
    ("position", AttrType::Vec3),
    ("velocity", AttrType::Vec3),
    ("age", AttrType::F32),
    ("life", AttrType::F32),
    ("id", AttrType::U32),
];

/// The attribute the chain adds forces to. Given out as 0, never part of the particles kept.
const FORCE: &str = "force";

/// Longest `warm_up`, in seconds.
const MAX_WARM_UP: f64 = 30.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || {
            Box::new(Particles { compactor: Compactor::quiet(), ..Particles::default() }) as Box<dyn Operator>
        })
        .warm_up(on_gpu(warm_up)),
    )
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "Particles: points born on the source points that move, age and die. Wire the end of a chain of forces back into `previous` to push them",
    )
    .stateful()
    .time_dependent()
    .param(
        ParamSpec::choice("emission", &EMISSIONS)
            .doc("How many particles are born")
            .choice_doc("rate", "`rate` particles a second in all, each from a source point picked at random")
            .choice_doc("per_point", "`rate` particles a second from every source point"),
    )
    .param(
        ParamSpec::float("rate", 100.0)
            .clamp(0.0, 1e9)
            .range(0.0, 1000.0)
            .doc("Births a second. Below one a frame works too: a birth comes every few frames"),
    )
    .param(
        ParamSpec::int("burst", 0)
            .clamp(0.0, 1e9)
            .range(0.0, 10_000.0)
            .doc("Particles born at once each time `fire` turns on, as well as the `rate`. With per_point, that many from every source point"),
    )
    .param(ParamSpec::bool("fire", false).doc("Turning it on gives birth to `burst` particles. Bind it to a key or a beat"))
    .param(
        ParamSpec::float("life", 2.0)
            .clamp(0.0, 1e6)
            .range(0.0, 20.0)
            .doc("How long a particle lives, in seconds. Its `age` counts up from 0; it dies when the age reaches its `life`"),
    )
    .param(
        ParamSpec::float("life_variance", 0.0)
            .clamp(0.0, 1e6)
            .range(0.0, 10.0)
            .doc("Each particle lives up to this many seconds more or less than `life`, at random"),
    )
    .param(
        ParamSpec::vec3("velocity", [0.0; 3])
            .range(-5.0, 5.0)
            .doc("The speed a particle starts with, in metres a second along x, y, z. z is up"),
    )
    .param(
        ParamSpec::float("along_normal", 0.0)
            .range(-5.0, 5.0)
            .doc("Adds this speed along the source point's `normal`, in metres a second: particles leave a sphere outwards. Without normals it points up (+z)"),
    )
    .param(
        ParamSpec::float("inherit", 1.0)
            .range(0.0, 2.0)
            .more()
            .doc("Adds the source point's `velocity` times this, where the source points have one: particles take the speed of a moving emitter"),
    )
    .param(
        ParamSpec::float("spread", 0.0)
            .clamp(0.0, 1e6)
            .range(0.0, 5.0)
            .doc("Adds a random speed of up to this, in metres a second, in any direction: a fountain fans out"),
    )
    .param(
        ParamSpec::vec3("gravity", [0.0; 3])
            .range(-20.0, 20.0)
            .doc("Pulls every particle the same way, in metres a second squared. [0, 0, -9.81] is the earth's"),
    )
    .param(
        ParamSpec::float("drag", 0.0)
            .clamp(0.0, 1e6)
            .range(0.0, 10.0)
            .doc("How fast particles lose speed, as through air: 1 takes about two thirds of the speed away in a second. A `drag` attribute of the particles takes its place"),
    )
    .param(
        ParamSpec::float("mass", 1.0)
            .clamp(1e-6, 1e9)
            .range(0.01, 10.0)
            .curve(Curve::Log)
            .doc("Divides the `force` the loop adds: heavier particles follow it slower. Gravity pulls every mass alike. A `mass` attribute of the particles takes its place"),
    )
    .param(ParamSpec::bool("integrate", true).doc(
        "Move the particles by their velocity and the forces. Off, they stay where they are unless a node in the loop moves them",
    ))
    .param(
        ParamSpec::int("max", i64::from(DEFAULT_MAX))
            .clamp(1.0, 64.0 * 1024.0 * 1024.0)
            .range(1.0, 1_000_000.0)
            .curve(Curve::Log)
            .doc("The most particles there are at once. While that many live, the births wait. Changing it starts over"),
    )
    .param(
        ParamSpec::text("kill", "kill")
            .more()
            .doc("A particle whose attribute of this name is above 0 dies at the next step: a node in the loop can end particles (pts.math, result kill, position.z < 0.0). Empty: only age kills"),
    )
    .param(
        ParamSpec::float("warm_up", 0.0)
            .clamp(0.0, MAX_WARM_UP)
            .range(0.0, 10.0)
            .doc("Seconds simulated ahead when the node starts or starts over, so the particles are there in full when they appear. The nodes of the loop take no part in them. While the show runs, the pre-roll takes about 4 ms of each frame, and the node gives out no particles until it is done: 30 seconds took about 4 seconds on a fast desktop. Stepping and recording do it all in the first frame"),
    )
    .param(ParamSpec::int("seed", 0).clamp(0.0, 1e9).range(0.0, 100.0).doc("Another number gives other random lives, speeds and source points. Two nodes with the same seed and the same params give the same particles: give each its own"))
    .param(ParamSpec::bool("reset", false).doc("No particles while on; they start over when it turns off"))
    .input("source", DataKind::Pts)
    .describe_input("source", "Where particles are born: a position and every attribute of a source point go to the particles born on it")
    .input_previous("previous", DataKind::Pts)
    .describe_input(
        "previous",
        "The end of the loop: what it held a step ago is where the next step starts. TouchDesigner calls it the target. Nothing wired: the node steps on its own",
    )
    .output("out", DataKind::Pts)
    .describe_output(
        "out",
        "The particles, oldest first, with id, age, life, velocity, force (0), and the source's attributes. Their number is on the GPU: the viewer shows it a frame late, chan.from_pts makes it a channel",
    )
}

/// The program that ages the particles and flags those that live on.
fn age_spec(kill: Option<&str>) -> Spec {
    let dies = kill.map_or(String::new(), |k| format!(" && !(f32({k}) > 0.0)"));
    let mut decls = vec![Decl::write("age"), Decl::read("life"), Decl::set(KEEP).ty(AttrType::U32)];
    if let Some(k) = kill {
        decls.push(Decl::read(k));
    }
    Spec {
        label: KIND.into(),
        source: format!(
            "fn main() {{\n    age += params.step;\n    {KEEP} = select(0u, 1u, age < life{dies});\n}}\n"
        ),
        decls,
        params: vec![ParamSpec::float("step", 0.0)],
    }
}

/// The program that moves the particles: from the attribute `mass` and `drag` where the
/// particles have them, else from the params. With `wind` (the pull of `pts.force`'s wind) the
/// change of speed is divided by `1 + pull * dt / mass`: an implicit step for the wind's part,
/// so the speed moves towards the wind's and never past it, whatever the mass and the step.
fn motion_spec(mass: bool, drag: bool, wind: bool) -> Spec {
    let m = if mass { "max(mass, 1e-6)" } else { "max(params.mass, 1e-6)" };
    let d = if drag { "drag" } else { "params.drag" };
    let mut decls = vec![Decl::write("position"), Decl::write("velocity"), Decl::read(FORCE)];
    if mass {
        decls.push(Decl::read("mass"));
    }
    if drag {
        decls.push(Decl::read("drag"));
    }
    let implicit = if wind {
        decls.push(Decl::read(WIND));
        format!(" / (1.0 + max({WIND}, 0.0) * params.step / {m})")
    } else {
        String::new()
    };
    Spec {
        label: KIND.into(),
        source: format!(
            "fn main() {{
    let pts_a = (params.gravity + force / {m}){implicit};
    velocity = (velocity + pts_a * params.step) * exp(-max({d}, 0.0) * params.step);
    position += velocity * params.step;
}}
"
        ),
        decls,
        params: vec![
            ParamSpec::float("step", 0.0),
            ParamSpec::vec3("gravity", [0.0; 3]),
            ParamSpec::float("mass", 1.0),
            ParamSpec::float("drag", 0.0),
        ],
    }
}

/// The params the births read.
fn birth_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::vec3("velocity", [0.0; 3]),
        ParamSpec::float("along_normal", 0.0),
        ParamSpec::float("inherit", 1.0),
        ParamSpec::float("spread", 0.0),
        ParamSpec::float("life", 2.0),
        ParamSpec::float("life_variance", 0.0),
    ]
}

/// How a newborn's own attributes are made; the others come from its source point.
fn birth_items() -> Vec<Item> {
    use append::Value::{Expr, Fixed};
    vec![
        Item::new(
            "velocity",
            Expr {
                wgsl: "params.velocity + params.inherit * src_velocity + params.along_normal * src_normal + params.spread * pts_ball(id, 2u)".into(),
                reads: vec!["velocity".into(), "normal".into()],
            },
        ),
        Item::new("age", Fixed([0.0; 4])),
        Item::new(
            "life",
            Expr {
                wgsl: "max(params.life + params.life_variance * (2.0 * pts_rand(id, 1u) - 1.0), 0.0)".into(),
                reads: Vec::new(),
            },
        ),
        Item::new("id", Expr { wgsl: "id".into(), reads: Vec::new() }),
    ]
}

/// A number from the seed, so two seeds give unrelated particles.
fn salt(seed: i64) -> u32 {
    let h = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_f491_4f6c_dd1d;
    (h >> 33) as u32 & 0x7fff_ffff
}

/// The attributes the particles of a step after the first have, for compiling the programs
/// before the first cook.
fn particle_shape() -> Shape {
    Shape::new(&[
        ("position", AttrType::Vec3),
        ("velocity", AttrType::Vec3),
        ("force", AttrType::Vec3),
        ("age", AttrType::F32),
        ("life", AttrType::F32),
        ("id", AttrType::U32),
    ])
}

/// Compiles the programs of the defaults, and the births from the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let budget = program::storage_budget(gpu);
    for spec in [age_spec(None), motion_spec(false, false, false)] {
        Program::cached(gpu, program::generate(&spec, &particle_shape(), budget)?)?;
    }
    // One birth into a set of one, as a sphere of `pts.generate` gives them.
    let buffer = |ty: AttrType| Buffer::new(gpu, ty.stride(), "pts warm up");
    let mut source = Vec::new();
    for (name, ty) in [("position", AttrType::Vec3), ("normal", AttrType::Vec3), ("uv", AttrType::Vec2)] {
        source.push(Attr::new(name, ty, buffer(ty)?)?);
    }
    let source = Points::new(1, 1, source)?;
    let names = state_attrs(&source);
    let attrs = names.iter().map(|(n, t)| Attr::new(n, *t, buffer(*t)?)).collect::<Result<Vec<_>, _>>()?;
    let header = Buffer::new(gpu, penumbra_gpu::points::HEADER_BYTES, "pts warm up")?;
    let state = Points::new(1, 0, attrs)?.with_count(Count::gpu(header, 1, Seen::default())?)?;
    let ids = Buffer::new(gpu, 16, "pts warm up")?;
    let none = |_: &str| None;
    let params = birth_params();
    let births = Births {
        source: &source,
        wanted: 1,
        per_point: false,
        pick: Pick::Random,
        salt: 0,
        room: 1,
        params: &params,
        param: &none,
    };
    append::append(gpu, &state, &ids, &births, &birth_items())
}

/// The attributes particles born from `source` have: their own, then the source's but `force`
/// and those of the particles' own names.
fn state_attrs(source: &Points) -> Vec<(String, AttrType)> {
    let mut out: Vec<(String, AttrType)> = OWN.iter().map(|(n, t)| (n.to_string(), *t)).collect();
    for a in source.attrs() {
        if [FORCE, KEEP, WIND].contains(&a.name()) || out.iter().any(|(n, _)| n == a.name()) {
            continue;
        }
        out.push((a.name().to_string(), a.ty()));
    }
    out
}

/// What the node keeps from step to step.
struct Sim {
    capacity: u32,
    /// The next id, in its first word.
    ids: Buffer,
    /// What the last step gave out.
    out: Points,
    /// Births of a fraction not yet born: below one a frame, they add up over the frames.
    carry: f64,
    /// `fire` at the last step, so a burst comes when it turns on.
    fired: bool,
    bound: Bound,
    /// The count headers of the outputs of the steps before the last, newest first: a loop
    /// that hands one of them back did not cook at the last step.
    older: Vec<Buffer>,
}

/// Outputs before the last that [`Sim::older`] knows.
const OLDER: usize = 4;

/// One step's settings, read from the params once.
struct Settings {
    per_point: bool,
    rate: f64,
    burst: u32,
    fire: bool,
    integrate: bool,
    kill: String,
    salt: u32,
}

#[derive(Default)]
struct Particles {
    age: Own,
    motion: Own,
    /// Quiet: the count after the births is read back, not the one before.
    compactor: Compactor,
    sim: Option<Sim>,
    last: Option<Tick>,
    /// Buffers of attributes the particles did not have yet, and of the set at the start.
    fresh: Vec<(String, BufferRing)>,
    headers: BufferRing,
    /// A `force` of 0 for every particle, shared by every step.
    zero: Option<Buffer>,
    /// The warm-up of a new start while it runs over several frames.
    warming: Option<Warming>,
}

/// A warm-up under way: `done` of `steps` steps of `length` seconds.
struct Warming {
    capacity: u32,
    ids: Buffer,
    carry: f64,
    bound: Bound,
    out: Option<Points>,
    done: u32,
    steps: u32,
    length: f64,
}

/// Longest a cook spends on a warm-up while the show runs; the rest waits for the next frame.
/// Stepping and recording take the whole warm-up at once.
const WARM_BUDGET: Duration = Duration::from_millis(4);

impl Particles {
    /// No particles, with the attributes they will have.
    fn nothing(&mut self, gpu: &Gpu, source: &Points, capacity: u32) -> Result<Cooked, CookError> {
        let empty = self.empty(gpu, source, capacity)?;
        let zero = self.zero(gpu, capacity)?;
        let out = empty.with_attr(Attr::new(FORCE, AttrType::Vec3, zero)?)?.with_count(Count::Known(0))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }

    /// Steps of the warm-up `w`, as many as [`WARM_BUDGET`] allows (all of them under exact
    /// frames), at least one.
    fn warm(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &Gpu,
        source: &Points,
        set: &Settings,
        mut w: Warming,
    ) -> Result<Warming, CookError> {
        let start = Instant::now();
        while w.done < w.steps {
            let (wanted, rest) = births_of(set.rate, w.length, w.carry);
            let (o, b) = self.advance(
                cx,
                gpu,
                source,
                w.out.as_ref(),
                w.capacity,
                &w.ids,
                w.length,
                wanted,
                set,
                &w.bound,
            )?;
            w.carry = rest;
            w.out = Some(o);
            w.bound = b;
            w.done += 1;
            if !cx.exact_frames() && start.elapsed() >= WARM_BUDGET {
                break;
            }
        }
        Ok(w)
    }

    /// A buffer for attribute `name` of `bytes` from the node's own rings.
    fn fresh(&mut self, gpu: &Gpu, name: &str, bytes: u64) -> Result<Buffer, Error> {
        if let Some((_, ring)) = self.fresh.iter_mut().find(|(n, _)| n == name) {
            return ring.next(gpu, bytes, &format!("pts {name}"));
        }
        let mut ring = BufferRing::new();
        let b = ring.next(gpu, bytes, &format!("pts {name}"))?;
        self.fresh.push((name.to_string(), ring));
        Ok(b)
    }

    /// No particles, in buffers for `capacity` of them with the attributes particles born from
    /// `source` have; their count on the GPU, 0.
    fn empty(&mut self, gpu: &Gpu, source: &Points, capacity: u32) -> Result<Points, Error> {
        let mut attrs = Vec::new();
        for (name, ty) in state_attrs(source) {
            let b = self.fresh(gpu, &name, u64::from(capacity) * ty.stride())?;
            attrs.push(Attr::new(&name, ty, b)?);
        }
        let header = self.headers.next(gpu, penumbra_gpu::points::HEADER_BYTES, "pts count")?;
        header.write(gpu, 0, &[0u32; 4])?;
        Points::new(capacity, 0, attrs)?.with_count(Count::gpu(header, capacity, Seen::default())?)
    }

    /// `alive` with every attribute the particles born from `source` have: one it lacks is
    /// added, its default at every particle.
    fn complete(&mut self, gpu: &Gpu, alive: Points, source: &Points) -> Result<Points, Error> {
        let mut out = alive;
        for (name, ty) in state_attrs(source) {
            if out.attr(&name).is_some() {
                continue;
            }
            let b = self.fresh(gpu, &name, u64::from(out.capacity()) * ty.stride())?;
            let d = default_of(&name).filter(|d| d.ty == ty).map_or([0.0f32; 4], |d| d.default);
            let words = (ty.stride() / 4) as usize;
            let one: Vec<f32> = (0..words)
                .map(|k| if ty == AttrType::Vec3 && k == 3 { 0.0 } else { d.get(k).copied().unwrap_or(0.0) })
                .collect();
            let all: Vec<f32> = one.iter().copied().cycle().take(words * out.capacity() as usize).collect();
            b.write(gpu, 0, &all)?;
            out = out.with_attr(Attr::new(&name, ty, b)?)?;
        }
        Ok(out)
    }

    /// The `force` of 0 for `capacity` particles.
    fn zero(&mut self, gpu: &Gpu, capacity: u32) -> Result<Buffer, Error> {
        let bytes = u64::from(capacity) * AttrType::Vec3.stride();
        if let Some(z) = self.zero.as_ref().filter(|z| z.bytes() == bytes) {
            return Ok(z.clone());
        }
        let z = Buffer::new(gpu, bytes, "pts force 0")?;
        self.zero = Some(z.clone());
        Ok(z)
    }

    /// One step of `dt` seconds from `state` (`None`: no particles yet), with `births` born.
    /// Changes nothing of the node's but the buffers it hands out, so a step that cannot run
    /// yet (a program still compiling) leaves the simulation as it was.
    #[allow(clippy::too_many_arguments)]
    fn advance(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &Gpu,
        source: &Points,
        state: Option<&Points>,
        capacity: u32,
        ids: &Buffer,
        dt: f64,
        wanted: u32,
        set: &Settings,
        bound: &Bound,
    ) -> Result<(Points, Bound), CookError> {
        let mut bound = bound.clone();
        bound.settle();
        let state = state.filter(|s| s.count().at_most() > 0 && s.capacity() == capacity);
        let alive = match state {
            None => {
                // No particles before the births: a count known to be 0.
                bound.anchor = (0, bound.born);
                self.empty(gpu, source, capacity)?
            }
            Some(s) => {
                let step = |name: &str| match name {
                    "step" => Some(Value::Float(dt)),
                    other => cx.param(other).cloned(),
                };
                let kill = (!set.kill.is_empty())
                    .then_some(set.kill.as_str())
                    .filter(|k| s.attr(k).is_some_and(|a| a.ty().components() == 1));
                let aged = self.age.run(cx, gpu, &age_spec(kill), s, false, &step, &[])?;
                let moved = if set.integrate {
                    let f32_attr = |n: &str| aged.attr(n).is_some_and(|a| a.ty() == AttrType::F32);
                    let spec = motion_spec(f32_attr("mass"), f32_attr("drag"), f32_attr(WIND));
                    self.motion.run(cx, gpu, &spec, &aged, false, &step, &[])?
                } else {
                    aged
                };
                let Some(keep) = moved.attr(KEEP).map(|a| a.buffer().clone()) else {
                    return Err(CookError::failed("the flags of the particles that live on are missing"));
                };
                let body = moved.without(KEEP).without(FORCE).without(WIND);
                let alive = self.compactor.compact(gpu, &body, &keep)?;
                self.complete(gpu, alive, source)?
            }
        };
        let params = birth_params();
        let param = |name: &str| cx.param(name).cloned();
        let births = Births {
            source,
            wanted,
            per_point: set.per_point,
            pick: if set.per_point { Pick::Cycle } else { Pick::Random },
            salt: set.salt,
            room: capacity,
            params: &params,
            param: &param,
        };
        // The count after the births is at most the last one read back plus every birth since;
        // the births stop there, so it holds on the GPU too.
        bound.born = bound.born.saturating_add(u64::from(births.most()));
        let room = bound.most(capacity);
        let births = Births { room, ..births };
        append::append(gpu, &alive, ids, &births, &birth_items())?;
        let Some(header) = alive.count().header().cloned() else {
            return Err(CookError::failed("the count of the particles is not on the GPU"));
        };
        // One readback a step: for display, and to tighten the bound a frame or two later. One
        // that cannot start leaves the bound as loose as the births make it.
        let seen = Seen::reading(gpu, &header, bound.pending.back().map(|(s, _)| s)).unwrap_or_default();
        bound.read(seen.clone());
        let zero = self.zero(gpu, capacity)?;
        let out = alive
            .with_attr(Attr::new(FORCE, AttrType::Vec3, zero)?)?
            .with_bounds(None)
            .with_count(Count::gpu(header, room, seen)?)?;
        Ok((out, bound))
    }
}

/// Births for a step of `dt` seconds: whole ones, and the fraction carried to the next step.
fn births_of(rate: f64, dt: f64, carry: f64) -> (u32, f64) {
    let total = carry + rate * dt;
    if !total.is_finite() || total <= 0.0 {
        return (0, 0.0);
    }
    let whole = total.floor().min(f64::from(u32::MAX));
    (whole as u32, (total - whole).clamp(0.0, 1.0))
}

/// Whether `p` is what the node gave out last: the same count on the GPU (a node that drops or
/// reorders points makes a count of its own) in buffers of the same capacity. No particles at
/// all are the same whatever carries them.
fn same_particles(p: &Points, out: &Points) -> bool {
    match (p.count().header(), out.count().header()) {
        (Some(a), Some(b)) => {
            a.same(b) && p.capacity() == out.capacity() && p.count().at_most() == out.count().at_most()
        }
        (None, None) => p.count().known() == Some(0) && out.count().known() == Some(0),
        _ => false,
    }
}

/// Readbacks of the count in flight at most; an older one is let go.
const MAX_READS: usize = 8;

/// What the CPU knows of the count: never less than it, and close to it a frame or two after
/// a readback arrives. Programs, compaction and pictures work over this many, so few particles
/// in a large `max` cost little.
#[derive(Clone, Default)]
struct Bound {
    /// Births at most since the start, each step's [`Births::most`] at the capacity.
    born: u64,
    /// The last count known (read back, or 0 at a start) and `born` at that step.
    anchor: (u32, u64),
    /// The counts read back after each step that have not arrived, oldest first, with `born`
    /// at their step.
    pending: VecDeque<(Seen, u64)>,
}

impl Bound {
    /// Takes the newest count that arrived as the anchor, and lets go of the older ones.
    fn settle(&mut self) {
        let newest =
            self.pending.iter().enumerate().rev().find_map(|(k, (s, b))| s.arrived().map(|n| (k, n, *b)));
        if let Some((k, n, b)) = newest {
            self.anchor = (n, b);
            self.pending.drain(..=k);
        }
    }

    /// The most particles there may be now: the anchor plus every birth since, within the
    /// capacity.
    fn most(&self, capacity: u32) -> u32 {
        let since = self.born.saturating_sub(self.anchor.1);
        let n = u64::from(self.anchor.0).saturating_add(since);
        u32::try_from(n).unwrap_or(u32::MAX).min(capacity)
    }

    /// The readback of the count after this step's births.
    fn read(&mut self, seen: Seen) {
        if self.pending.len() >= MAX_READS {
            self.pending.pop_front();
        }
        self.pending.push_back((seen, self.born));
    }
}

impl Operator for Particles {
    fn gpu_bytes(&self) -> u64 {
        let rings: u64 = self.fresh.iter().map(|(_, r)| r.bytes()).sum();
        self.age.bytes()
            + self.motion.bytes()
            + self.compactor.bytes()
            + rings
            + self.headers.bytes()
            + self.zero.as_ref().map_or(0, Buffer::bytes)
            + self.sim.as_ref().map_or(0, |s| s.ids.bytes())
    }

    fn trim(&mut self) {
        self.age.trim();
        self.motion.trim();
        self.compactor.trim();
        for (_, r) in &mut self.fresh {
            r.trim();
        }
        self.headers.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        if let Some(w) = &self.warming {
            let progress = w.done as f32 / w.steps.max(1) as f32;
            return Some(JobStatus::Running { progress: Some(progress) });
        }
        let busy = |s: &JobStatus| matches!(s, JobStatus::Queued | JobStatus::Running { .. });
        let (a, m) = (self.age.status(), self.motion.status());
        Some(if busy(&a) || !busy(&m) { a } else { m })
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let Some(source) = cx.input_as::<Points>(0) else {
            return Err(if cx.def().inputs.first().is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect the points particles are born on to `source`")
            });
        };
        let most = i64::from(most_elements(gpu, AttrType::Vec4));
        let capacity = u32::try_from(cx.int("max")?.clamp(1, most)).unwrap_or(DEFAULT_MAX);
        let now = cx.time();
        let period = 1.0 / cx.rate().as_f64().max(1.0);
        let dt = step(now.tick, self.last, period);
        let set = Settings {
            per_point: cx.text("emission")? == "per_point",
            rate: cx.float("rate")?,
            burst: u32::try_from(cx.int("burst")?.max(0)).unwrap_or(u32::MAX),
            fire: cx.bool("fire")?,
            integrate: cx.bool("integrate")?,
            kill: penumbra_gpu::points::canonical(cx.text("kill")?).to_string(),
            salt: salt(cx.int("seed")?),
        };
        if !set.kill.is_empty() {
            program::check_name(&set.kill).map_err(|e| CookError::failed(format!("kill: {}", e.0)))?;
        }

        if cx.bool("reset")? {
            // No particles, with the attributes they will have.
            self.sim = None;
            self.warming = None;
            self.last = Some(now.tick);
            return self.nothing(gpu, source, capacity);
        }
        let kept = self.sim.as_ref().is_some_and(|s| s.capacity == capacity);
        if dt == 0.0 && kept && !cx.params_changed() && !cx.inputs_changed() && !cx.first_cook() {
            // The show stands still: so do the particles.
            return Ok(Cooked::Unchanged);
        }
        if !kept {
            self.sim = None;
        }

        // Where this step starts: the end of the loop, if it holds what the node gave out.
        let previous = cx.input_as::<Points>(1);
        let state: Option<Points>;
        let mut fresh = None;
        match self.sim.as_ref() {
            Some(sim) => {
                state = Some(match previous {
                    Some(p) if dt > 0.0 && same_particles(p, &sim.out) => p.clone(),
                    Some(p)
                        if dt > 0.0
                            && p.count().header().is_some_and(|h| sim.older.iter().any(|o| o.same(h))) =>
                    {
                        cx.warn(
                            "the loop into `previous` did not run at the last step: a node in it is in error or still waiting (for a compile, an input), so it handed back older particles. Stepping on without the loop meanwhile",
                        );
                        sim.out.clone()
                    }
                    Some(_) if dt > 0.0 => {
                        cx.warn(
                            "the points on `previous` are not the particles this node gave out a step ago: a node in the loop changes their number or order (pts.delete does), or they come from elsewhere. Only pts.particles adds and removes particles; set `kill` to end them from the loop. Stepping on without the loop meanwhile",
                        );
                        sim.out.clone()
                    }
                    _ => sim.out.clone(),
                });
            }
            None => {
                // A new start: the ids from 0, the warm-up first, in steps of at most MAX_DT.
                let warming = match self.warming.take() {
                    Some(w) if w.capacity == capacity => w,
                    _ => {
                        let warm = cx.float("warm_up")?.clamp(0.0, MAX_WARM_UP);
                        let length = period.min(MAX_DT);
                        Warming {
                            capacity,
                            ids: Buffer::new(gpu, 16, "pts particle ids")?,
                            carry: 0.0,
                            bound: Bound::default(),
                            out: None,
                            done: 0,
                            steps: (warm / length).round() as u32,
                            length,
                        }
                    }
                };
                let w = self.warm(cx, gpu, source, &set, warming)?;
                if w.done < w.steps {
                    // Not there yet: no particles meanwhile, and the rest at the next frames.
                    self.last = Some(now.tick);
                    self.warming = Some(w);
                    return self.nothing(gpu, source, capacity);
                }
                state = w.out.clone();
                fresh = Some((w.ids, w.carry, w.bound));
            }
        }

        let (ids, carry, bound) = match (&fresh, &self.sim) {
            (Some((ids, carry, bound)), _) => (ids.clone(), *carry, bound.clone()),
            (None, Some(sim)) => (sim.ids.clone(), sim.carry, sim.bound.clone()),
            (None, None) => return Err(CookError::failed("the particles have no state")),
        };
        // A burst waits for time to move, as every birth does.
        let fired_before = self.sim.as_ref().is_some_and(|s| s.fired);
        let fired = if dt > 0.0 { set.fire } else { fired_before };
        let (mut wanted, carry) = births_of(set.rate, dt, carry);
        if fired && !fired_before {
            wanted = wanted.saturating_add(set.burst);
        }
        let (out, bound) =
            self.advance(cx, gpu, source, state.as_ref(), capacity, &ids, dt, wanted, &set, &bound)?;
        self.last = Some(now.tick);
        let older = match &self.sim {
            Some(sim) => {
                let last = sim.out.count().header().cloned();
                last.into_iter().chain(sim.older.iter().cloned()).take(OLDER).collect()
            }
            None => Vec::new(),
        };
        self.sim = Some(Sim { capacity, ids, out: out.clone(), carry, fired, bound, older });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
