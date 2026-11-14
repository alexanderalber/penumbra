//! `tex.liquid`: a 2D liquid with a free surface in side view, MLS-MPM in compute programs
//! (`liquid.wgsl`). The liquid falls, lands, pools and runs off. Particles carry it; every
//! substep they hand mass and momentum to a grid of square cells, the grid applies gravity, drag
//! and the walls, and the particles take their new velocity back.
//!
//! Units are the picture's, as in `tex.fluid`: positions are shares of the picture from the top
//! left, lengths picture heights, speeds picture heights per second, y down.
//!
//! The particles live in one buffer of `particles` places. A free place (material 0) waits for
//! an emitter: once per frame free places take the emitters' slots through an atomic counter,
//! and a particle that leaves through an open edge is free again, so a pour runs for hours at a
//! constant count. The `fill` picture places particles at a start. A few counters come back to
//! the CPU a frame or two late, without waiting, for the warnings.
//!
//! Walls and obstacles are one field on the grid: per node the distance to the nearest solid,
//! its velocity and its friction. Without an `obstacles` picture the CPU writes the picture's
//! edges into it once. With one, a second program (`liquid_solid.wgsl`) makes it on the GPU by
//! jump flooding the mask at twice the grid's resolution, whenever the mask, its velocity picture
//! or the edges change. The simulation program binds 6 of the 7 storage buffers a program may
//! have, so the flooding has buffers of its own in a program of its own.
//!
//! Time steps follow the show clock as `tex.fluid`'s do, at most [`MAX_DT`] long and scaled by
//! `speed`. Each frame takes as many substeps as the sound speed, the fastest flow and the
//! viscosity need on the grid, at most [`MAX_SUBSTEPS`]. A new grid starts the liquid over.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Tick, Value};
use penumbra_gpu::{Batch, Buffer, BufferRead, Compute, Dispatch, Gpu, Out, ReadState, Step, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

use super::fluid::{grid, step};
use super::liquid_emitter::{self, Emitter, MAX_EMITTERS};
use super::{height_param, keep_size, side, width_param};
use crate::on_gpu;

pub(crate) const KIND: &str = "tex.liquid";
const SOURCE: &str = include_str!("liquid.wgsl");
const SOLID_KIND: &str = "tex.liquid obstacles";
const SOLID_SOURCE: &str = include_str!("liquid_solid.wgsl");

/// Input of the `fill` picture, after the run of emitters, then the obstacles and their velocity.
const FILL: usize = MAX_RUN;
const OBSTACLES: usize = MAX_RUN + 1;
const OBSTACLE_VELOCITY: usize = MAX_RUN + 2;

/// Longest time step, in seconds: one frame a 60 fps show skipped is simulated in full, a longer
/// stall is cut short, so a hitch costs at most twice the substeps of a frame.
pub(crate) const MAX_DT: f64 = 1.0 / 30.0;
/// Most substeps per frame: what a frame may cost at most, whatever `speed`, `viscosity` and a
/// long step ask for.
pub(crate) const MAX_SUBSTEPS: usize = 128;
/// Grid nodes of margin around the picture.
const PAD: u32 = 3;
/// Particles per cell at rest: their spacing is half a cell.
const PER_CELL: f64 = 4.0;
/// Speed of sound in the liquid, picture heights per second (12 m/s in a room 3 m high). Any
/// value from 2 to 8 looks the same; less lets the liquid squeeze, more costs substeps.
const SOUND: f64 = 4.0;
/// Stiffness exponent of the pressure (Tait).
const GAMMA: f64 = 4.0;
/// A wave crosses at most this share of a cell per substep.
const CFL: f64 = 0.7;
/// Viscosity times a substep over a cell squared stays below this.
const VISCOUS_STEP: f64 = 0.1;
/// Density ratio from which the pressure grows no more.
const MAX_RATIO: f64 = 2.0;
/// Speed at which a particle leaves a solid it is inside of, picture heights per second.
const PUSH: f64 = 0.1;
/// Nodes nearer a solid than this many cells take its boundary condition.
const BAND: f64 = 0.25;
/// Share of the rest density a solid cell counts as for the particles beside it.
const GHOST: f64 = 1.0;
/// How far new particles stray from their lattice point, in spacings.
const JITTER: f64 = 0.3;
/// The most momentum a particle hands a node, in units of the fastest grid speed.
const MOST_MOMENTUM: f64 = 8.0;
/// The mass a node's fixed-point sum holds, in rest masses of a cell. Rest is 1; a pile-up far
/// beyond what pressure allows stays within it.
const HEADROOM: f64 = 128.0;
/// The fastest flow the substeps are sized for without emitters or a strong gravity: a fall of a
/// picture height at the default gravity, rounded up.
const MIN_FLOW: f64 = 3.0;
/// Most cells along the grid's longer side.
const MAX_GRID: i64 = 1024;
/// Most particles: 64 MiB.
const MAX_PARTICLES: i64 = 1 << 20;
/// Mask cells per grid cell along each side: the mask is sampled at twice the grid. At the grid's
/// own resolution a pool on a shelf sits half a cell low.
const MASK_PER_CELL: f64 = 2.0;
/// The fastest an obstacle moves, picture heights per second: a faster velocity picture is cut
/// down to this.
const MOST_OBSTACLE_SPEED: f64 = 30.0;
/// Highest Coulomb friction coefficient `friction` takes.
const MOST_FRICTION: f64 = 10.0;
/// Bytes per particle (`P` in the shader).
const PARTICLE_BYTES: u64 = 64;
/// 32-bit words per emitter (`Em`) and in the frame's constants (`U`).
const EMITTER_WORDS: usize = 12;
const UNIFORM_WORDS: usize = 64;
/// The counters the shader keeps (`counter`), and their places.
const COUNTERS: usize = 8;
const FREE_SEEN: usize = 0;
const WRAPPED: usize = 2;
const FILL_WANTED: usize = 3;
const LIVE: usize = 4;
/// Material numbers: 0 is a free place.
const LIQUID: u32 = 1;

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "A liquid seen from the side: it pours from emitters, falls, splashes and pools at the bottom of the picture or on obstacles",
    )
    .stateful()
    .time_dependent()
    .input_run(
        "emitter",
        DataKind::Table,
        1,
        "Where liquid pours in: tex.liquid_emitter, or any table whose first row names columns x, y and more \
         (see tex.liquid_emitter's output). 64 emitters in all at most",
    )
    .input("fill", DataKind::Tex)
    .describe_input(
        "fill",
        "Liquid lies where this picture is brighter than half when the node starts or `reset` is on. A changed \
         picture shows at the next reset",
    )
    .input("obstacles", DataKind::Tex)
    .describe_input(
        "obstacles",
        "Solid where brighter than half: the liquid lands on it, pools on it and flows around it. May change \
         every frame",
    )
    .input("obstacle_velocity", DataKind::Tex)
    .describe_input(
        "obstacle_velocity",
        "How the obstacles move, so they push the liquid: red x in widths, green y in heights, down positive, \
         per second, as the `velocity` output. Unwired, obstacles stand still",
    )
    .param(width_param())
    .param(height_param())
    .param(keep_size())
    .param(
        ParamSpec::int("grid", 384)
            .clamp(16.0, MAX_GRID as f64)
            .range(64.0, 1024.0)
            .curve(Curve::Log)
            .doc(
                "Cells across the picture's width; the height follows its shape. More gives finer drops and \
                 thinner streams. At the same `particles`, twice the cells cost twice as much and hold a \
                 quarter of the liquid. A change starts the liquid over",
            ),
    )
    .param(
        ParamSpec::int("particles", 65536)
            .clamp(1024.0, MAX_PARTICLES as f64)
            .range(4096.0, 262_144.0)
            .curve(Curve::Log)
            .doc(
                "Most particles the liquid has: how much liquid there can be. Four particles fill a cell, so at \
                 the default grid 65536 fill a fifth of a 16:9 picture. Cost grows with this number",
            ),
    )
    .param(
        ParamSpec::vec2("gravity", [0.0, 3.27])
            .clamp(-1000.0, 1000.0)
            .range(-10.0, 10.0)
            .doc(
                "Which way the liquid falls and how fast it speeds up, in picture heights per second squared, y \
                 down. 3.27 is the earth's when the picture is 3 m high",
            ),
    )
    .param(
        ParamSpec::choice("edges", &["walls", "open"])
            .doc("What the picture's edges are to the liquid")
            .choice_doc("walls", "the liquid stays in the picture and pools at the bottom")
            .choice_doc("open", "the liquid flows out and is gone, and its particles pour again from the emitters"),
    )
    .param(
        ParamSpec::float("viscosity", 0.000_222)
            .clamp(0.0, 0.01)
            .range(0.0, 0.004)
            .curve(Curve::Power(3.0))
            .doc(
                "Thickness: honey instead of water. In picture heights squared per second: 0.0002 is water in a \
                 picture 3 m high, 0.001 honey, 0.003 mud. A thick liquid needs more substeps on a fine grid",
            ),
    )
    .param(
        ParamSpec::float("drag", 0.5)
            .clamp(0.0, 1000.0)
            .range(0.0, 5.0)
            .doc("How fast the liquid calms: the share of its speed it loses per second. 0 sloshes for minutes"),
    )
    .param(
        ParamSpec::float("friction", 0.0)
            .clamp(0.0, MOST_FRICTION)
            .range(0.0, 2.0)
            .doc(
                "How much the liquid sticks to the obstacles and the picture's edges: 0 slides freely. At 1 a \
                 thin film on a slope slides about a sixth as far as without; deep liquid still runs down over it",
            ),
    )
    .param(ParamSpec::color("color", [0.25, 0.55, 1.0, 1.0]).doc("Colour of the liquid in `out`; transparent around it"))
    .param(
        ParamSpec::float("speed", 1.0)
            .clamp(0.0, 16.0)
            .range(0.0, 4.0)
            .doc("How fast the liquid moves against the show clock: 0 holds it, 0.25 is slow motion. Faster costs more substeps"),
    )
    .param(ParamSpec::bool("reset", false).doc("Starts over from the `fill` picture, every frame while on"))
    .output("out", DataKind::Tex)
    .describe_output("out", "The liquid in `color`, transparent around it, at width x height")
    .output("velocity", DataKind::Tex)
    .describe_output(
        "velocity",
        "The liquid's velocity at the grid's size: red x in widths, green y in heights, down positive, per \
         second; 0 where there is no liquid",
    )
    .lazy_output("velocity")
    .output("density", DataKind::Tex)
    .describe_output(
        "density",
        "How full each cell of the grid is, as grey on black at the grid's size: 1 where the liquid is at rest, \
         less at its surface and in spray. Opaque",
    )
    .lazy_output("density")
}

fn program_params() -> Vec<ParamSpec> {
    vec![ParamSpec::float("t", 0.0)]
}

fn program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(KIND, SOURCE, &program_params())
}

fn solid_params() -> Vec<ParamSpec> {
    vec![ParamSpec::float("step", 1.0), ParamSpec::float("from_b", 0.0)]
}

fn solid_program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(SOLID_KIND, SOLID_SOURCE, &solid_params())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Liquid::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(|gpu| program(gpu).and_then(|_| solid_program(gpu)).map(|_| ()))),
    )?;
    liquid_emitter::register(types)
}

/// The grid for a picture of `width` x `height` and a `grid` cells across: cells across and
/// down the picture. Square cells, at most [`MAX_GRID`] along the longer side.
pub(crate) fn cells(width: u32, height: u32, size: i64) -> (u32, u32) {
    grid(width, height, size.clamp(16, MAX_GRID))
}

/// The longest stable substep on cells `dx` high, with the fastest flow `fastest` and kinematic
/// viscosity `viscosity`: a wave crosses at most [`CFL`] of a cell, and the viscosity stays
/// within [`VISCOUS_STEP`].
fn longest_substep(dx: f64, fastest: f64, viscosity: f64) -> f64 {
    let fastest = if fastest.is_finite() { fastest.max(SOUND) } else { SOUND };
    let viscosity = if viscosity.is_finite() { viscosity.max(0.0) } else { 0.0 };
    let mut longest = CFL * dx / fastest;
    if viscosity > 0.0 {
        longest = longest.min(VISCOUS_STEP * dx * dx / viscosity);
    }
    longest
}

/// Substeps for a frame of `dt` seconds ([`longest_substep`]), at most [`MAX_SUBSTEPS`]. Also
/// the frame's time, shorter than `dt` where the cap binds, and how many substeps the frame
/// would have needed.
pub(crate) fn substeps(dt: f64, dx: f64, fastest: f64, viscosity: f64) -> (usize, f64, usize) {
    if !dt.is_finite() || dt <= 0.0 || !dx.is_finite() || dx <= 0.0 {
        return (0, 0.0, 0);
    }
    let longest = longest_substep(dx, fastest, viscosity);
    let needed = (dt / longest).ceil().max(1.0);
    let needed = if needed < 1e9 { needed as usize } else { usize::MAX };
    if needed > MAX_SUBSTEPS {
        (MAX_SUBSTEPS, MAX_SUBSTEPS as f64 * longest, needed)
    } else {
        (needed, dt, needed)
    }
}

/// What the counters said when they last came back.
#[derive(Clone, Copy, Debug, Default)]
struct Counted {
    /// Particles in use.
    live: u32,
    /// Particles the last fill wanted.
    fill_wanted: u32,
    /// Whether the emitters asked for more particles than were free.
    short: bool,
    /// Grid cells whose mass did not fit the fixed point.
    wrapped: u32,
}

/// Jump flooding steps over a mask of `across` x `down` cells: from half the larger side (as a
/// power of two) down to 1, then 1 once more.
fn flood_steps(across: u32, down: u32) -> Vec<u32> {
    let mut steps = Vec::new();
    let mut s = across.max(down).max(1).next_power_of_two() / 2;
    while s >= 1 {
        steps.push(s);
        s /= 2;
    }
    steps.push(1);
    steps
}

/// The buffers that turn the `obstacles` mask into the solid field.
struct Obstacles {
    /// Mask cells across and down the picture.
    mask: (u32, u32),
    j: Buffer,
    sa: Buffer,
    sb: Buffer,
    /// Whether the edges were open when the field was last made; `None` until a flood went out.
    /// The pictures themselves are not compared: a node may draw a new picture into the texture
    /// it gave last frame (`tex.cache` of length 1 does), so only the cook's own word that an
    /// input changed says the mask is the same.
    flooded: Option<bool>,
}

impl Obstacles {
    fn new(gpu: &Gpu, aspect: f64, dx: f64) -> Result<Self, Error> {
        let side = dx / MASK_PER_CELL;
        let across = (aspect / side).ceil().clamp(1.0, f64::from(MAX_GRID as u32) * 8.0) as u32;
        let down = (1.0 / side).round().clamp(1.0, f64::from(MAX_GRID as u32) * 8.0) as u32;
        let n = u64::from(across) * u64::from(down);
        Ok(Self {
            mask: (across, down),
            j: Buffer::of::<u32>(gpu, 12, "tex.liquid obstacle constants")?,
            sa: Buffer::of::<[i32; 4]>(gpu, n, "tex.liquid obstacle seeds")?,
            sb: Buffer::of::<[i32; 4]>(gpu, n, "tex.liquid obstacle seeds")?,
            flooded: None,
        })
    }

    fn bytes(&self) -> u64 {
        self.j.bytes() + self.sa.bytes() + self.sb.bytes()
    }

    /// The constants, as `J` in `liquid_solid.wgsl` lays them out.
    fn words(&self, nodes: (u32, u32), aspect: f64, dx: f64, open: bool) -> [u32; 12] {
        let f = |v: f64| (v as f32).to_bits();
        [
            self.mask.0,
            self.mask.1,
            nodes.0,
            nodes.1,
            f(aspect),
            f(1.0),
            f(dx / MASK_PER_CELL),
            f(dx),
            f(if open { 1.0 } else { 0.0 }),
            f(MOST_OBSTACLE_SPEED),
            0,
            0,
        ]
    }
}

/// The buffers of one grid.
struct Sim {
    /// Cells across and down the picture, and the nodes of the grid with its margin.
    cells: (u32, u32),
    nodes: (u32, u32),
    /// The picture's width over its height.
    aspect: f64,
    /// The cell's side in picture heights.
    dx: f64,
    budget: u32,
    open: Option<bool>,
    u: Buffer,
    parts: Buffer,
    ga: Buffer,
    gf: Buffer,
    solid: Buffer,
    counter: Buffer,
    emitters: Buffer,
    /// The obstacles' field maker, while `obstacles` is wired.
    obstacles: Option<Obstacles>,
    /// Whether the particles came from the `fill` picture since the last start.
    filled: bool,
    /// Whether the particles in use must be counted anew (the buffer changed size).
    recount: bool,
    /// Per emitter, how far its stream has run since its last whole row, in picture heights.
    runs: Vec<f64>,
    /// Frames simulated since the start, for the jitter of new particles.
    frame: u32,
}

impl Sim {
    fn new(gpu: &Gpu, cells: (u32, u32), aspect: f64, budget: u32) -> Result<Self, Error> {
        let dx = 1.0 / f64::from(cells.1.max(1));
        let across = (aspect / dx).ceil().clamp(1.0, f64::from(MAX_GRID as u32) * 4.0) as u32;
        let nodes = (across + 2 * PAD + 1, cells.1 + 2 * PAD + 1);
        let n = u64::from(nodes.0) * u64::from(nodes.1);
        Ok(Self {
            cells,
            nodes,
            aspect,
            dx,
            budget,
            open: None,
            u: Buffer::of::<u32>(gpu, UNIFORM_WORDS as u64, "tex.liquid constants")?,
            parts: Buffer::new(gpu, u64::from(budget) * PARTICLE_BYTES, "tex.liquid particles")?,
            ga: Buffer::of::<[i32; 3]>(gpu, n, "tex.liquid grid sums")?,
            gf: Buffer::of::<[f32; 4]>(gpu, n, "tex.liquid grid")?,
            solid: Buffer::of::<[f32; 4]>(gpu, n, "tex.liquid solids")?,
            counter: Buffer::of::<u32>(gpu, COUNTERS as u64, "tex.liquid counters")?,
            emitters: Buffer::of::<u32>(gpu, (MAX_EMITTERS * EMITTER_WORDS) as u64, "tex.liquid emitters")?,
            obstacles: None,
            filled: false,
            recount: false,
            runs: Vec::new(),
            frame: 0,
        })
    }

    fn bytes(&self) -> u64 {
        [&self.u, &self.parts, &self.ga, &self.gf, &self.solid, &self.counter, &self.emitters]
            .iter()
            .map(|b| b.bytes())
            .sum::<u64>()
            + self.obstacles.as_ref().map_or(0, Obstacles::bytes)
    }

    /// Every particle free, the counters at zero: the liquid starts over.
    fn restart(&mut self, gpu: &Gpu) -> Result<(), Error> {
        self.parts.clear(gpu)?;
        self.counter.clear(gpu)?;
        self.filled = false;
        self.recount = false;
        self.runs.clear();
        self.frame = 0;
        Ok(())
    }

    /// The solid field of the picture's edges: walls around it, or nothing for open edges.
    fn write_edges(&mut self, gpu: &Gpu, open: bool) -> Result<(), Error> {
        if self.open == Some(open) {
            return Ok(());
        }
        let (w, h) = self.nodes;
        let mut field = Vec::with_capacity(w as usize * h as usize);
        for j in 0..h {
            for i in 0..w {
                let x = (f64::from(i) - f64::from(PAD)) * self.dx;
                let y = (f64::from(j) - f64::from(PAD)) * self.dx;
                let d = if open { 1.0e3 } else { x.min(self.aspect - x).min(y).min(1.0 - y) };
                field.push([d as f32, 0.0, 0.0, 1.0]);
            }
        }
        self.solid.write(gpu, 0, &field)?;
        self.open = Some(open);
        Ok(())
    }

    /// A new number of particles: the first ones stay, new places are free.
    fn resize(&mut self, gpu: &Gpu, budget: u32) -> Result<(), Error> {
        if budget == self.budget {
            return Ok(());
        }
        self.parts.resize(gpu, u64::from(budget) * PARTICLE_BYTES, "tex.liquid particles")?;
        self.budget = budget;
        self.recount = true;
        Ok(())
    }
}

/// What one frame asks of the emitters: the words of their buffer and the particles in all.
fn emitter_words(sim: &mut Sim, emitters: &[Emitter], dt: f64) -> (Vec<u32>, u32) {
    let spacing = sim.dx / PER_CELL.sqrt();
    sim.runs.resize(emitters.len(), 0.0);
    let mut words = Vec::with_capacity(emitters.len() * EMITTER_WORDS);
    let mut slots: u64 = 0;
    for (e, run) in emitters.iter().zip(sim.runs.iter_mut()) {
        let speed = e.velocity[0].hypot(e.velocity[1]);
        let dir = if speed > 1e-9 { [e.velocity[0] / speed, e.velocity[1] / speed] } else { [0.0, 1.0] };
        let cols = (e.width / spacing).round().clamp(1.0, 4096.0);
        let mut rows = 0.0;
        if e.on && speed > 1e-9 && dt > 0.0 {
            *run += speed * dt;
            rows = (*run / spacing).floor().clamp(0.0, 4096.0);
            *run = (*run - rows * spacing).clamp(0.0, spacing);
        } else {
            *run = 0.0;
        }
        let room = u64::from(sim.budget).saturating_sub(slots);
        let wanted = (cols * rows) as u64;
        let rows = if wanted > room { (room as f64 / cols).floor() } else { rows };
        let f = |v: f64| (v as f32).to_bits();
        words.extend([
            f(e.position[0] * sim.aspect),
            f(e.position[1]),
            f(dir[0]),
            f(dir[1]),
            f(speed),
            f(*run),
            f(spacing),
            0,
            cols as u32,
            slots as u32,
            LIQUID + e.material as u32,
            rows as u32,
        ]);
        slots += (cols * rows) as u64;
    }
    (words, slots.min(u64::from(sim.budget)) as u32)
}

/// What the frame's constants take from the grid.
#[derive(Clone, Copy)]
struct Layout {
    nodes: (u32, u32),
    budget: u32,
    dx: f64,
    aspect: f64,
    frame: u32,
}

/// The frame's constants, as `U` in the shader lays them out: vectors of four words.
struct Frame {
    dt: f64,
    vmax: f64,
    gravity: [f64; 2],
    drag: f64,
    viscosity: f64,
    friction: f64,
    open: bool,
    emitters: u32,
    slots: u32,
    color: [f64; 4],
}

fn uniform_words(sim: Layout, f: &Frame) -> Vec<u32> {
    let float = |v: f64| (v as f32).to_bits();
    let rest = PER_CELL;
    let most_mass = HEADROOM * rest;
    let mscale = (2f64.powi(30) / most_mass).floor();
    let pscale = (2f64.powi(30) / (most_mass * f.vmax)).floor().max(1.0);
    let spacing = sim.dx / PER_CELL.sqrt();
    let columns = (sim.aspect / spacing).floor().max(1.0);
    let mut w = vec![
        sim.nodes.0,
        sim.nodes.1,
        sim.budget,
        u32::from(f.open),
        float(sim.dx),
        float(1.0 / sim.dx),
        float(f.dt),
        float(rest),
        float(mscale),
        float(pscale),
        float(f.vmax),
        float(PUSH),
        float(0.0),        // friction rate: none for a liquid
        float(f.friction), // Coulomb friction
        float(BAND),
        float(GHOST),
        float(f.gravity[0]),
        float(f.gravity[1]),
        float(f.drag),
        float(MOST_MOMENTUM),
        float(sim.aspect),
        float(1.0),
        float(JITTER),
        float(spacing),
        f.emitters,
        f.slots,
        sim.frame,
        columns as u32,
        float(f.color[0]),
        float(f.color[1]),
        float(f.color[2]),
        float(f.color[3]),
    ];
    // Materials: liquid first, the other three unused for now.
    let c2g = SOUND * SOUND / GAMMA;
    let liquid = [0.0, 1.0, c2g, GAMMA, f.viscosity, 0.0, MAX_RATIO, 0.0];
    for m in 0..4 {
        let values = if m == 0 { liquid } else { [0.0; 8] };
        w.extend(values.map(float));
    }
    w
}

#[derive(Default)]
struct Liquid {
    program: Option<Arc<Compute>>,
    solid_program: Option<Arc<Compute>>,
    sim: Option<Sim>,
    out_ring: TexRing,
    velocity_ring: TexRing,
    density_ring: TexRing,
    last: Option<Tick>,
    /// The counters on their way back, with the particles the emitters asked for in that frame.
    reading: Option<(BufferRead, u32)>,
    spare: Option<BufferRead>,
    counted: Counted,
}

impl Liquid {
    /// Takes in the counters if they came back.
    fn take_reading(&mut self) {
        let Some((read, asked)) = self.reading.take() else { return };
        match read.state() {
            ReadState::Waiting => self.reading = Some((read, asked)),
            ReadState::Failed => {}
            ReadState::Ready => {
                if let Ok(c) = read.get::<u32>() {
                    let at = |i: usize| c.get(i).copied().unwrap_or(0);
                    self.counted.live = at(LIVE);
                    self.counted.fill_wanted = at(FILL_WANTED);
                    self.counted.wrapped = at(WRAPPED);
                    self.counted.short = asked > 0 && at(FREE_SEEN) < asked;
                }
                self.spare = Some(read);
            }
        }
    }

    /// The warnings the counters give.
    fn warn_counted(&self, cx: &CookContext<'_>) {
        let Some(sim) = &self.sim else { return };
        if self.counted.short {
            cx.warn(format!(
                "the liquid uses all {} particles, so the emitters pour less or nothing: raise `particles`, or let \
                 liquid leave through open `edges`",
                sim.budget
            ));
        }
        if self.counted.fill_wanted > sim.budget {
            cx.warn(format!(
                "the fill needs {} particles but `particles` is {}: part of it is left out",
                self.counted.fill_wanted, sim.budget
            ));
        }
        if self.counted.wrapped > 0 {
            cx.warn(format!(
                "the liquid is squeezed far beyond its rest in {} cells and flickers there",
                self.counted.wrapped
            ));
        }
    }
}

impl Operator for Liquid {
    fn gpu_bytes(&self) -> u64 {
        let rings = [&self.out_ring, &self.velocity_ring, &self.density_ring];
        penumbra_gpu::unique_bytes(rings.into_iter().flat_map(TexRing::iter))
            + self.sim.as_ref().map_or(0, Sim::bytes)
            + self.reading.as_ref().map_or(0, |(r, _)| r.bytes())
            + self.spare.as_ref().map_or(0, BufferRead::bytes)
    }

    fn trim(&mut self) {
        self.spare = None;
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let program = match &self.program {
            Some(p) => p.clone(),
            None => {
                let p = program(gpu)?;
                self.program = Some(p.clone());
                p
            }
        };
        let now = cx.time();
        let period = 1.0 / cx.rate().as_f64().max(1.0);
        let reset = cx.bool("reset")?;
        let (width, height) = (side(cx, "width")?, side(cx, "height")?);
        let cells = cells(width, height, cx.int("grid")?);
        let aspect = f64::from(width) / f64::from(height.max(1));
        let most = (Buffer::max_bytes(gpu) / PARTICLE_BYTES).min(MAX_PARTICLES as u64) as i64;
        let budget = cx.int("particles")?.clamp(1024, most.max(1024)) as u32;
        let open = cx.text("edges")? == "open";
        let speed = cx.float("speed")?;
        let speed = if speed.is_finite() { speed.clamp(0.0, 16.0) } else { 0.0 };
        let dt = step(now.tick, self.last, period).min(MAX_DT) * speed;
        // Before the early return, so a bad table warns on a still show too.
        let emitters = liquid_emitter::from_run(cx, 0, "emitter");
        let want_velocity = cx.output_wanted(1);
        let want_density = cx.output_wanted(2);
        self.take_reading();
        self.warn_counted(cx);
        let kept = self.sim.as_ref().is_some_and(|s| s.cells == cells && s.aspect == aspect);
        if dt == 0.0
            && kept
            && !reset
            && !cx.params_changed()
            && !cx.inputs_changed()
            && !cx.outputs_wanted_changed()
            && !cx.first_cook()
        {
            // The show stands still: so does the liquid.
            return Ok(Cooked::Unchanged);
        }
        self.last = Some(now.tick);

        if !kept {
            self.sim = None;
            self.reading = None;
            self.spare = None;
            self.sim = Some(Sim::new(gpu, cells, aspect, budget)?);
        }
        let Some(sim) = self.sim.as_mut() else { return Err(CookError::failed("the liquid has no grid")) };
        if !kept || reset {
            sim.restart(gpu)?;
            self.reading = None;
            self.counted = Counted::default();
        }
        sim.resize(gpu, budget)?;
        // The solid field: the edges from the CPU, or edges and obstacles by jump flooding, made
        // again when an input or the edges changed. Inputs are told apart only as a whole, so a
        // changing emitter table floods again too.
        let mask = cx.input_as::<Tex>(OBSTACLES);
        let mask_velocity = mask.and(cx.input_as::<Tex>(OBSTACLE_VELOCITY));
        let flood = match mask {
            Some(_) => {
                if sim.obstacles.is_none() {
                    sim.obstacles = Some(Obstacles::new(gpu, sim.aspect, sim.dx)?);
                }
                let inputs_changed = cx.inputs_changed();
                match sim.obstacles.as_mut() {
                    Some(o) if inputs_changed || o.flooded != Some(open) => {
                        // Until this flood went out: a cook that fails on the way floods again
                        // next time, when the inputs no longer count as changed.
                        o.flooded = None;
                        let words = o.words(sim.nodes, sim.aspect, sim.dx, open);
                        o.j.write(gpu, 0, &words)?;
                        true
                    }
                    _ => false,
                }
            }
            None => {
                sim.obstacles = None;
                sim.write_edges(gpu, open)?;
                false
            }
        };
        let solid_program = if flood {
            Some(match &self.solid_program {
                Some(p) => p.clone(),
                None => {
                    let p = solid_program(gpu)?;
                    self.solid_program = Some(p.clone());
                    p
                }
            })
        } else {
            None
        };

        let gravity = match cx.param("gravity") {
            Some(Value::Vec2([x, y])) if x.is_finite() && y.is_finite() => [*x, *y],
            _ => [0.0, 0.0],
        };
        let viscosity = cx.float("viscosity")?;
        let viscosity = if viscosity.is_finite() { viscosity.clamp(0.0, 0.01) } else { 0.0 };
        let drag = cx.float("drag")?;
        let drag = if drag.is_finite() { drag.max(0.0) } else { 0.0 };
        let friction = cx.float("friction")?;
        let friction = if friction.is_finite() { friction.clamp(0.0, MOST_FRICTION) } else { 0.0 };
        // The fastest flow: a fall across the picture, or an emitter's stream.
        let fall = (2.0 * gravity[0].hypot(gravity[1])).sqrt();
        let pour = emitters.iter().map(|e| e.velocity[0].hypot(e.velocity[1])).fold(0.0, f64::max);
        let fastest = MIN_FLOW.max(fall).max(pour);
        let (count, dt, needed) = substeps(dt, sim.dx, fastest, viscosity);
        if needed > count {
            cx.warn(format!(
                "a frame needs {needed} substeps, at most {MAX_SUBSTEPS} run: the liquid moves slower than the show. \
                 Lower `grid`, `viscosity` or `speed`"
            ));
        }
        let sub = if count > 0 { dt / count as f64 } else { 0.0 };
        // The fastest speed on the grid: a node moves at most 0.9 cells a substep. A frame
        // without time measures the liquid with the longest substep there could be.
        let nominal = if sub > 0.0 { sub } else { longest_substep(sim.dx, fastest, viscosity) };
        let vmax = 0.9 * sim.dx / nominal;

        let (emit_words, slots) = emitter_words(sim, &emitters, dt);
        if !emit_words.is_empty() {
            sim.emitters.write(gpu, 0, &emit_words)?;
        }
        let color = match cx.param("color") {
            Some(Value::Vec4(c)) => c.map(|v| if v.is_finite() { v } else { 0.0 }),
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        let frame = Frame {
            dt: sub,
            vmax,
            gravity,
            drag,
            viscosity,
            friction,
            open,
            emitters: emitters.len().min(MAX_EMITTERS) as u32,
            slots,
            color,
        };
        let layout =
            Layout { nodes: sim.nodes, budget: sim.budget, dx: sim.dx, aspect: sim.aspect, frame: sim.frame };
        sim.u.write(gpu, 0, &uniform_words(layout, &frame))?;
        // This frame's counts start at zero; what is in use and the fill's count go on.
        sim.counter.write(gpu, 0, &[0u32; 3])?;
        if sim.recount {
            sim.counter.write(gpu, LIVE as u64, &[0u32])?;
        }

        let fill = cx.input_as::<Tex>(FILL);
        let filling = !sim.filled && fill.is_some();
        let (cw, ch) = sim.cells;
        let out = self.out_ring.next_storage(gpu, width, height, KIND)?;
        let velocity_out =
            if want_velocity { Some(self.velocity_ring.next_storage(gpu, cw, ch, KIND)?) } else { None };
        let density_out =
            if want_density { Some(self.density_ring.next_storage(gpu, cw, ch, KIND)?) } else { None };

        let times: Vec<f64> = (0..count).map(|k| k as f64 * sub).collect();
        let params: Vec<_> =
            times.iter().map(|t| move |n: &str| (n == "t").then_some(Value::Float(*t))).collect();
        let zero = |n: &str| (n == "t").then_some(Value::Float(0.0));
        let dispatch =
            |pictures, param| Dispatch { volumes: &[], pictures, data: None, seconds: 0.0, frame: 0, param };
        let plain = dispatch(&[], &zero);
        let fill_pictures = [fill];
        let filled_from = dispatch(&fill_pictures, &zero);
        let substep_inputs: Vec<Dispatch<'_>> = params.iter().map(|p| dispatch(&[], p)).collect();
        let particles = Out::range(sim.budget);
        let nodes = Out::grid(sim.nodes.0, sim.nodes.1);
        let mut steps = Vec::with_capacity(3 * count + 8);
        if sim.recount {
            steps.push(Step { entry: "recount", out: particles, input: &plain });
        }
        if filling {
            let spacing = sim.dx / PER_CELL.sqrt();
            let lattice = Out::grid((aspect / spacing).floor() as u32, (1.0 / spacing).floor() as u32);
            steps.push(Step { entry: "fill", out: lattice, input: &filled_from });
        }
        if slots > 0 {
            steps.push(Step { entry: "emit", out: particles, input: &plain });
        }
        if count == 0 {
            // No time passes: the grid is measured once, so the pictures show the particles.
            steps.push(Step { entry: "p2g", out: particles, input: &plain });
            steps.push(Step { entry: "grid_update", out: nodes, input: &plain });
        }
        for input in &substep_inputs {
            steps.push(Step { entry: "p2g", out: particles, input });
            steps.push(Step { entry: "grid_update", out: nodes, input });
            steps.push(Step { entry: "g2p", out: particles, input });
        }
        steps.push(Step { entry: "draw", out: Out::Picture(&out), input: &plain });
        if let Some(t) = &velocity_out {
            steps.push(Step { entry: "velocity", out: Out::Picture(t), input: &plain });
        }
        if let Some(t) = &density_out {
            steps.push(Step { entry: "density", out: Out::Picture(t), input: &plain });
        }
        let buffers = [&sim.u, &sim.parts, &sim.ga, &sim.gf, &sim.solid, &sim.counter, &sim.emitters];
        let mut batch = Batch::new(gpu, KIND);
        if let (Some(solid), Some(o), Some(m)) = (&solid_program, &sim.obstacles, mask) {
            // The field first: the fill and the substeps read it.
            let (across, down) = o.mask;
            let jumps = flood_steps(across, down);
            let jump_params: Vec<_> = jumps
                .iter()
                .enumerate()
                .map(|(k, s)| {
                    let (step, from_b) = (f64::from(*s), (k % 2) as f64);
                    move |n: &str| match n {
                        "step" => Some(Value::Float(step)),
                        "from_b" => Some(Value::Float(from_b)),
                        _ => None,
                    }
                })
                .collect();
            // After an odd number of jumps the seeds are in `sb`.
            let last = (jumps.len() % 2) as f64;
            let read_last = move |n: &str| (n == "from_b").then_some(Value::Float(last));
            let mask_pictures = [Some(m)];
            let velocity_pictures = [None, mask_velocity];
            // Not `dispatch`: its pictures and params would have to outlive the frame's steps.
            let own = |pictures, param| Dispatch {
                volumes: &[],
                pictures,
                data: None,
                seconds: 0.0,
                frame: 0,
                param,
            };
            let seeded = own(&mask_pictures, &zero);
            let jump_inputs: Vec<Dispatch<'_>> = jump_params.iter().map(|p| own(&[], p)).collect();
            let resolved = own(&velocity_pictures, &read_last);
            let cells = Out::grid(across, down);
            let mut flood = vec![Step { entry: "seed", out: cells, input: &seeded }];
            flood.extend(jump_inputs.iter().map(|input| Step { entry: "flood", out: cells, input }));
            flood.push(Step { entry: "resolve", out: nodes, input: &resolved });
            solid.encode_with(&mut batch, &[&o.j, &o.sa, &o.sb, &sim.solid], &flood)?;
        }
        program.encode_with(&mut batch, &buffers, &steps)?;
        batch.submit();
        drop(steps);
        if flood && let Some(o) = sim.obstacles.as_mut() {
            o.flooded = Some(open);
            // The edges from the CPU are gone: written again when the obstacles go.
            sim.open = None;
        }
        if filling {
            sim.filled = true;
        }
        sim.recount = false;
        if count > 0 {
            sim.frame = sim.frame.wrapping_add(1);
        }
        if self.reading.is_none() {
            self.reading = Some((sim.counter.read_async(gpu, self.spare.take())?, slots));
        }

        let lazy = |t: Option<Tex>| match t {
            Some(t) => Data::new(DataKind::Tex, t),
            None => Data::skipped(DataKind::Tex),
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, out), lazy(velocity_out), lazy(density_out)]))
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substeps_follow_the_sound_speed_and_the_viscosity_and_stop_at_the_cap() {
        // 512 cells across a room 4 m wide and 3 m high, at 60 fps: 37.
        let dx = 1.0 / 384.0;
        assert_eq!(substeps(1.0 / 60.0, dx, 0.0, 0.0).0, 37);
        // Default grid of a 16:9 picture: 216 cells high.
        assert_eq!(substeps(1.0 / 60.0, 1.0 / 216.0, 3.0, 0.000_222).0, 21);
        // Honey on that grid is still the waves'; mud needs more.
        assert_eq!(substeps(1.0 / 60.0, 1.0 / 216.0, 3.0, 0.001).0, 21);
        assert!(substeps(1.0 / 60.0, 1.0 / 216.0, 3.0, 0.003).0 > 21);
        // A fast pour needs more than the sound speed.
        assert_eq!(substeps(1.0 / 60.0, 1.0 / 216.0, 8.0, 0.0).0, 42);
        // The cap: fewer substeps over a shorter time, and how many were needed.
        let (n, t, needed) = substeps(1.0 / 60.0, 1.0 / 1000.0, 3.0, 0.01);
        assert_eq!(n, MAX_SUBSTEPS);
        assert!(needed > MAX_SUBSTEPS && t < 1.0 / 60.0 && t > 0.0, "{n} {t} {needed}");
        for (dt, dx) in [(0.0, 0.01), (f64::NAN, 0.01), (1.0, 0.0), (-1.0, 0.01), (f64::INFINITY, 0.01)] {
            assert_eq!(substeps(dt, dx, 3.0, 0.0), (0, 0.0, 0), "{dt} {dx}");
        }
        assert_eq!(substeps(1.0 / 60.0, 1.0 / 216.0, f64::NAN, f64::NAN).0, 21);
    }

    #[test]
    fn the_grid_has_square_cells_and_stays_bounded() {
        assert_eq!(cells(1920, 1080, 384), (384, 216));
        assert_eq!(cells(1080, 1920, 1024), (576, 1024));
        assert_eq!(cells(96, 64, 384), (96, 64));
        assert_eq!(cells(1920, 1080, i64::MAX), (1024, 576));
    }

    #[test]
    fn the_constants_fill_the_shaders_struct() {
        // Eight vectors of four words, then four materials of eight.
        let layout = Layout { nodes: (10, 8), budget: 1024, dx: 0.25, aspect: 1.5, frame: 3 };
        let frame = Frame {
            dt: 0.001,
            vmax: 1.0,
            gravity: [0.0, 3.27],
            drag: 0.5,
            viscosity: 0.0,
            friction: 0.0,
            open: true,
            emitters: 0,
            slots: 0,
            color: [1.0; 4],
        };
        let words = uniform_words(layout, &frame);
        assert_eq!(words.len(), UNIFORM_WORDS);
        assert_eq!(UNIFORM_WORDS, 8 * 4 + 4 * 8);
        // The struct in the shader: its members in this order, all vectors of four.
        let start = SOURCE.find("struct U {").unwrap();
        let body = &SOURCE[start..start + SOURCE[start..].find('}').unwrap()];
        let members: Vec<&str> = body
            .lines()
            .skip(1)
            .filter_map(|l| l.trim().split_once(':').map(|(n, t)| (n, t.trim())))
            .map(|(n, t)| {
                assert!(t.starts_with("vec4") || t.starts_with("array<Mat, 4>"), "{n}: {t}");
                n
            })
            .collect();
        assert_eq!(members, ["grid", "cell", "fixed", "wall", "force", "room", "emit", "look", "mats"]);
        let em = &SOURCE[SOURCE.find("struct Em {").unwrap()..];
        let em = &em[..em.find('}').unwrap()];
        assert_eq!(em.matches("vec4").count() * 4, EMITTER_WORDS);
    }
}
