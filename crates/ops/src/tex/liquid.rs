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
//!
//! Sand and snow are materials of their own in the same particles and grid ([`MATERIALS`]): each
//! emitter names its material, `fill_material` the fill's. They carry a deformation gradient,
//! and stiff packed snow takes about twice the substeps of the liquid; the node takes them from
//! the first frame a material is poured or filled in until it starts over.
//!
//! A third program (`liquid_draw.wgsl`) draws the particles: a kernel per particle that grows
//! where the liquid thins out, blurred and thresholded to a clean edge, then shaded. Each liquid
//! particle keeps a smoothed density and its foam for the drawing, in the places a solid keeps
//! its deformation gradient; the substeps move them on with time constants, so the look does not
//! depend on the frame rate.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Table, Tick, Unit, Value};
use penumbra_gpu::{Batch, Buffer, BufferRead, Compute, Dispatch, Gpu, Out, ReadState, Step, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

use super::fluid::{FrameGaps, MAX_SPLATS, Splat, grid, splats as read_splats, step};
use super::liquid_emitter::{self, Emitter, MATERIALS, MAX_EMITTERS};
use super::{height_param, keep_size, side, width_param};
use crate::on_gpu;

pub(crate) const KIND: &str = "tex.liquid";
const SOURCE: &str = include_str!("liquid.wgsl");
const SOLID_KIND: &str = "tex.liquid obstacles";
const SOLID_SOURCE: &str = include_str!("liquid_solid.wgsl");
const DRAW_KIND: &str = "tex.liquid drawing";
const DRAW_SOURCE: &str = include_str!("liquid_draw.wgsl");

/// Input of the `fill` picture, after the run of emitters, then the obstacles and their velocity,
/// the force picture and the splats.
const FILL: usize = MAX_RUN;
const OBSTACLES: usize = MAX_RUN + 1;
const OBSTACLE_VELOCITY: usize = MAX_RUN + 2;
const FORCE: usize = MAX_RUN + 3;
const SPLATS: usize = MAX_RUN + 4;

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
/// The strongest push the `force` picture and the splats give together, picture heights per
/// second squared (15 times the earth's gravity in a picture 3 m high): a stronger one is cut down
/// to this, so the substeps it needs stay within reach.
const MOST_PUSH: f64 = 50.0;
/// Largest value the `force` picture is read with, in widths or heights per input frame.
const MOST_MOTION: f64 = 1.0e4;
/// Bytes per particle (`P` in the shader).
const PARTICLE_BYTES: u64 = 64;
/// 32-bit words per emitter (`Em`) and in the frame's constants (`U`). The splats follow the
/// emitters in their buffer, one `Em` each.
const EMITTER_WORDS: usize = 12;
const UNIFORM_WORDS: usize = 72;
/// The counters the shader keeps (`counter`), and their places.
const COUNTERS: usize = 8;
const FREE_SEEN: usize = 0;
const WRAPPED: usize = 2;
const FILL_WANTED: usize = 3;
const LIVE: usize = 4;
/// The strongest push of the `force` picture where there is liquid, before the cut to
/// [`MOST_PUSH`], as the bits of a float.
const PEAK: usize = 5;
/// Material numbers: 0 is a free place, then [`MATERIALS`] in their order.
const LIQUID: u32 = 1;
const SAND: u32 = 2;
const SNOW: u32 = 3;
/// Stiffness of sand and snow as the speed of a shear-free wave would be without Poisson's
/// ratio, picture heights per second (12 m/s in a room 3 m high, the spike's number): the
/// Lame numbers follow from it and the ratio. Stiffer costs substeps and looks no different.
const SAND_STIFFNESS: f64 = 4.0;
const SAND_POISSON: f64 = 0.3;
const SNOW_STIFFNESS: f64 = 4.0;
const SNOW_POISSON: f64 = 0.2;
/// Snow from powder over fresh to wet: critical compression, critical stretch, hardening. The
/// most hardening, 4, doubles the wave speed.
const SNOW_POWDER: [f64; 3] = [0.015, 0.003, 10.0];
const SNOW_FRESH: [f64; 3] = [0.025, 0.0075, 10.0];
const SNOW_WET: [f64; 3] = [0.05, 0.01, 5.0];
const SNOW_MOST_HARDENING: f64 = 4.0;
/// Snow's Coulomb friction on solids, from powder to wet.
const SNOW_WALL: [f64; 2] = [0.6, 2.0];
/// The angles of repose `sand_angle` takes, degrees.
const MIN_ANGLE: f64 = 10.0;
const MAX_ANGLE: f64 = 40.0;
/// A poured heap stands flatter than the friction angle the plasticity is given: that angle is
/// this many times the angle of repose wanted. Measured at the default grid: heaps of 20, 30,
/// 35 and 40 degrees asked for stand at 21, 31, 35 and 38.
const SAND_ANGLE_GAIN: f64 = 1.33;
/// Sand's Coulomb friction on solids, in units of the tangent of its angle of repose: the grid
/// holds about half the friction asked for (sand needed 0.8 to stay on a box of 20 degrees,
/// where 0.36 should do).
const SAND_WALL_GAIN: f64 = 2.0;
/// Share of the difference between the volume the grid's density gives a sand particle and the
/// volume its deformation holds that the deformation takes on per substep, where the sand is
/// denser than at rest. Without it particles drift into clumps of up to 3.5 times the rest
/// density, and a heap loses 8 to 28 % of its area as it settles.
const SAND_SETTLE: f64 = 0.1;
/// Colours of sand and snow, linear: #C9A26B and #EEF2F8.
const SAND_COLOR: [f64; 4] = [0.584, 0.361, 0.147, 1.0];
const SNOW_COLOR: [f64; 4] = [0.855, 0.888, 0.939, 1.0];
/// Time over which a particle's density is smoothed for the drawing, seconds: the kernel's size
/// follows it, and a noisy density would make the edge flicker.
const DENSITY_TIME: f64 = 0.039;
/// Time over which a particle's velocity is smoothed to tell an impact, seconds: a frame at
/// 60 fps, where the foam's numbers were found.
const IMPACT_TIME: f64 = 1.0 / 60.0;
/// Particles faster than this, picture heights per second (0.3 m/s), draw as ellipses along
/// their velocity.
const FAST: f64 = 0.1;
/// Particle spacing aimed at in the picture the particles are drawn into, in its pixels: the
/// drawing's resolution follows from it.
const SPLAT_SPACING: f64 = 1.4;
/// Blurs of the drawing, in particle spacings: the shape's and the surface's.
const SHAPE_BLUR: f64 = 1.8;
const SURFACE_BLUR: f64 = 1.4;
/// Rows of the picture the depth is blurred in, and the blur, in picture heights (16 pixels at
/// 1080).
const DEPTH_ROWS: u32 = 270;
const DEPTH_BLUR: f64 = 16.0 / 1080.0;
/// Fixed point of the drawing's sums: 2^20 particles at their largest weight still fit 32 bits.
const DRAW_FIXED: f64 = 2048.0;

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "A liquid, sand or snow seen from the side: it pours from emitters, falls, and pools or heaps up at the \
         bottom of the picture or on obstacles",
    )
    .stateful()
    .time_dependent()
    .input_run(
        "emitter",
        DataKind::Table,
        1,
        "Where liquid, sand or snow pours in: tex.liquid_emitter, or any table whose first row names columns x, y and more \
         (see tex.liquid_emitter's output). 64 emitters in all at most",
    )
    .input("fill", DataKind::Tex)
    .describe_input(
        "fill",
        "The `fill_material` lies where this picture is brighter than half when the node starts or `reset` is \
         on. A changed picture shows at the next reset",
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
    .input("force", DataKind::Tex)
    .describe_input(
        "force",
        "Motion that pushes the liquid, sand and snow: tex.optical_flow's output as it is, as in tex.fluid. Red x \
         in widths, green y in heights, down positive, per input frame. Per second, `force` times this motion's \
         speed is added to the liquid's speed",
    )
    .input("splats", DataKind::Table)
    .describe_input(
        "splats",
        "Pushes at points, one per row, at most 64: tex.blob_track's table as it is, as in tex.fluid. Columns x, \
         y (0 to 1, from the top left); optional vx, vy (picture sizes per second) and radius (picture \
         heights). Colour columns are left alone: only emitters and `fill` add liquid",
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
                 thin film on a slope slides about a sixth as far as without; deep liquid still runs down over it. \
                 Sand and snow stick by their own friction; this adds to it where it is higher",
            ),
    )
    .param(
        ParamSpec::float("force", 4.0).clamp(0.0, 1000.0).range(0.0, 20.0).curve(Curve::Power(2.0)).doc(
            "How strongly the `force` picture and the splats push, as in tex.fluid. Per second, this many times \
             the motion's speed is added to the liquid's speed. A push beyond 50 heights per second squared is \
             cut to 50",
        ),
    )
    .param(
        ParamSpec::choice("splat_motion", &["push", "set"])
            .doc("What a splat's vx and vy do to the liquid under it")
            .choice_doc("push", "add to its speed every frame, times `force`: a stirring hand")
            .choice_doc(
                "set",
                "become its speed, whatever `force` says: a paddle, or a still hand that holds the liquid back",
            ),
    )
    .param(
        ParamSpec::float("splat_radius", 0.05)
            .clamp(0.0001, 10.0)
            .range(0.005, 0.3)
            .curve(Curve::Log)
            .more()
            .doc("Radius of a splat from a table without a radius column, in picture heights"),
    )
    .param(
        ParamSpec::choice("fill_material", &MATERIALS)
            .doc("What the `fill` picture is made of. Emitters choose their own material")
            .choice_doc("liquid", "water-like liquid that runs, splashes and pools")
            .choice_doc("sand", "dry sand that pours and forms heaps")
            .choice_doc("snow", "snow that clumps, piles up and breaks into lumps"),
    )
    .param(
        ParamSpec::float("sand_angle", 35.0)
            .clamp(MIN_ANGLE, MAX_ANGLE)
            .range(20.0, 50.0)
            .unit(Unit::Degrees)
            .doc(
                "How steep a sand heap stands, in degrees: 30 to 35 is dry sand, 45 damp sand. Also how much sand \
                 sticks to slopes",
            ),
    )
    .param(
        ParamSpec::float("snow_wetness", 0.5)
            .clamp(0.0, 1.0)
            .doc(
                "0 is powder that crumbles and slides, 0.5 fresh snow, 1 wet snow that sticks together, packs and \
                 holds on slopes",
            ),
    )
    .param(ParamSpec::color("color", [0.028, 0.275, 0.807, 1.0]).doc(
        "Colour of the liquid in `out` where it is thin. Deep liquid is drawn darker, the band under its surface lighter",
    ))
    .param(ParamSpec::color("sand_color", SAND_COLOR).doc(
        "Colour of sand in `out`. Each grain is drawn a little lighter or darker, sand deep in a heap darker",
    ))
    .param(
        ParamSpec::color("snow_color", SNOW_COLOR)
            .doc("Colour of snow in `out` where it is lit. Snow deep in a pile is drawn bluer and darker"),
    )
    .param(
        ParamSpec::float("foam_strength", 0.6)
            .clamp(0.0, 1.0)
            .doc("How white foam shows in `out`: 0 not at all, 1 fully white. The `foam` output stays the same"),
    )
    .param(
        ParamSpec::float("foam_time", 0.4)
            .clamp(0.01, 60.0)
            .range(0.05, 4.0)
            .curve(Curve::Log)
            .unit(Unit::Seconds)
            .doc(
                "How long foam lasts after a splash, in seconds: after this time a third of it is left. \
                 Slowed down by `speed` as the liquid is",
            ),
    )
    .param(
        ParamSpec::float("speed", 1.0)
            .clamp(0.0, 16.0)
            .range(0.0, 4.0)
            .doc("How fast the liquid moves against the show clock: 0 holds it, 0.25 is slow motion. Faster costs more substeps"),
    )
    .param(ParamSpec::bool("reset", false).doc("Starts over from the `fill` picture, every frame while on"))
    .output("out", DataKind::Tex)
    .describe_output(
        "out",
        "The liquid in `color` with foam, sand in `sand_color`, snow in `snow_color`, at width x height. \
         Transparent around it, with an edge a pixel soft, ready to composite",
    )
    .output("velocity", DataKind::Tex)
    .describe_output(
        "velocity",
        "The liquid's velocity at the grid's size: red x in widths, green y in heights, down positive, per \
         second; blue the speed in heights per second; alpha how full the cell is. Goes on a few cells beyond the \
         liquid and fades out there",
    )
    .lazy_output("velocity")
    .output("density", DataKind::Tex)
    .describe_output(
        "density",
        "How full each cell of the grid is, as grey on black at the grid's size: 1 where the liquid is at rest, \
         less at its surface and in spray. Opaque",
    )
    .lazy_output("density")
    .output("foam", DataKind::Tex)
    .describe_output(
        "foam",
        "Where the liquid foams after a splash or in fast spray: white over transparent, 0 to 1, at width x \
         height. Fades in `foam_time`",
    )
    .lazy_output("foam")
    .output("grain", DataKind::Tex)
    .describe_output(
        "grain",
        "A random grey per particle, averaged, on the liquid's alpha, at width x height. The pattern moves with \
         the liquid: blur it and raise its contrast for a crust or a texture. On sand it changes from grain to \
         grain",
    )
    .lazy_output("grain")
    .output("surface", DataKind::Tex)
    .describe_output(
        "surface",
        "For shading downstream, at width x height, solids counted as liquid: red the distance into the liquid in \
         pixels (negative outside), green the depth 0 to 1, blue and alpha the outward normal (x, y down)",
    )
    .lazy_output("surface")
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

fn draw_program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(DRAW_KIND, DRAW_SOURCE, &[])
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(Liquid::default()) as Box<dyn Operator>).warm_up(
        on_gpu(|gpu| {
            program(gpu).and_then(|_| solid_program(gpu)).and_then(|_| draw_program(gpu)).map(|_| ())
        }),
    ))?;
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

/// The Lame numbers over the density, mu and lambda, of a solid of `stiffness` (heights per
/// second, squared is Young's modulus over the density) and Poisson's ratio `poisson`.
fn lame(stiffness: f64, poisson: f64) -> (f64, f64) {
    let e = stiffness * stiffness;
    (e / (2.0 * (1.0 + poisson)), e * poisson / ((1.0 + poisson) * (1.0 - 2.0 * poisson)))
}

/// Speed of the pressure wave in a solid of `stiffness` and Poisson's ratio `poisson`.
fn pressure_wave(stiffness: f64, poisson: f64) -> f64 {
    stiffness * ((1.0 - poisson) / ((1.0 + poisson) * (1.0 - 2.0 * poisson))).sqrt()
}

/// Sand and snow as the node's params set them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Granular {
    /// Sand's angle of repose, degrees.
    angle: f64,
    /// Snow from powder (0) over fresh (0.5) to wet (1).
    wetness: f64,
}

impl Default for Granular {
    fn default() -> Self {
        Self { angle: 35.0, wetness: 0.5 }
    }
}

impl Granular {
    /// Sand's Drucker-Prager friction coefficient and its Coulomb friction on solids, for its
    /// angle of repose.
    fn sand_friction(&self) -> (f64, f64) {
        let angle = if self.angle.is_finite() { self.angle.clamp(MIN_ANGLE, MAX_ANGLE) } else { 35.0 };
        let sin = (angle * SAND_ANGLE_GAIN).min(80.0).to_radians().sin();
        let alpha = (2.0f64 / 3.0).sqrt() * 2.0 * sin / (3.0 - sin);
        (alpha, angle.to_radians().tan() * SAND_WALL_GAIN)
    }

    /// Snow's critical compression, critical stretch and hardening.
    fn snow(&self) -> [f64; 3] {
        let w = if self.wetness.is_finite() { self.wetness.clamp(0.0, 1.0) } else { 0.5 };
        let (from, to, t) =
            if w < 0.5 { (SNOW_POWDER, SNOW_FRESH, w * 2.0) } else { (SNOW_FRESH, SNOW_WET, w * 2.0 - 1.0) };
        let [a0, a1, a2] = from;
        let [b0, b1, b2] = to;
        [a0 + (b0 - a0) * t, a1 + (b1 - a1) * t, a2 + (b2 - a2) * t]
    }

    /// Snow's Coulomb friction on solids: wet snow sticks.
    fn snow_friction(&self) -> f64 {
        let w = if self.wetness.is_finite() { self.wetness.clamp(0.0, 1.0) } else { 0.5 };
        SNOW_WALL[0] + (SNOW_WALL[1] - SNOW_WALL[0]) * w
    }

    /// The shader's material table: liquid, sand, snow and a place unused, eight numbers each
    /// as `Mat` lays them out.
    fn table(&self, viscosity: f64) -> [[f64; 8]; 4] {
        let c2g = SOUND * SOUND / GAMMA;
        let (sand_mu, sand_la) = lame(SAND_STIFFNESS, SAND_POISSON);
        let (snow_mu, snow_la) = lame(SNOW_STIFFNESS, SNOW_POISSON);
        let (alpha, sand_wall) = self.sand_friction();
        let [tc, ts, xi] = self.snow();
        [
            [0.0, 1.0, c2g, GAMMA, viscosity, 0.0, MAX_RATIO, 0.0],
            [sand_wall, 1.0, sand_mu, sand_la, alpha, 0.0, SAND_SETTLE, 0.0],
            [self.snow_friction(), 1.0, snow_mu, snow_la, tc, ts, xi, SNOW_MOST_HARDENING],
            [0.0; 8],
        ]
    }
}

/// The fastest wave in the materials of `used` (bit n for material n), heights per second.
fn fastest_wave(used: u32) -> f64 {
    let mut wave = SOUND;
    if used & (1 << SAND) != 0 {
        wave = wave.max(pressure_wave(SAND_STIFFNESS, SAND_POISSON));
    }
    if used & (1 << SNOW) != 0 {
        wave = wave.max(pressure_wave(SNOW_STIFFNESS, SNOW_POISSON) * SNOW_MOST_HARDENING.sqrt());
    }
    wave
}

/// The material number of a name in [`MATERIALS`]; liquid for any other name.
fn material_number(name: &str) -> u32 {
    LIQUID + MATERIALS.iter().position(|m| *m == name).unwrap_or(0) as u32
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
    /// The strongest push of the `force` picture where the liquid is, heights per second squared,
    /// before the cut to [`MOST_PUSH`].
    peak: f64,
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

/// Sizes of the drawing for an output of `out` pixels and cells `dx` picture heights high: the
/// picture the particles are drawn into, at about [`SPLAT_SPACING`] pixels between particles
/// and with its sums within `most_bytes`, and the picture the depth is blurred in.
fn draw_sizes(out: (u32, u32), dx: f64, most_bytes: u64) -> ((u32, u32), (u32, u32)) {
    let (w, h) = (out.0.max(1), out.1.max(1));
    let spacing = dx / PER_CELL.sqrt() * f64::from(h);
    let k = (spacing / SPLAT_SPACING).round();
    let mut k = if k.is_finite() { k.clamp(1.0, f64::from(u32::MAX)) as u32 } else { 1 }.max(1);
    let splat = loop {
        let s = (w.div_ceil(k), h.div_ceil(k));
        if u64::from(s.0) * u64::from(s.1) * 16 <= most_bytes || s == (1, 1) {
            break s;
        }
        k = k.saturating_add(1);
    };
    let rows = h.min(DEPTH_ROWS);
    let across = ((f64::from(rows) * f64::from(w) / f64::from(h)).round() as u32).clamp(1, w);
    (splat, (across, rows))
}

/// The drawing's buffers and its pictures between passes, for one output size and grid.
struct Drawing {
    out: (u32, u32),
    dx: f64,
    splat: (u32, u32),
    depth: (u32, u32),
    d: Buffer,
    acc: Buffer,
    /// The sums as (field, solid, foam, grain); blurred along x; along y.
    shape: [Tex; 3],
    /// The filled coverage blurred along x; along y with the depth beside it.
    surface: [Tex; 2],
    /// The filled coverage at the depth's size; blurred along x; along y.
    deep: [Tex; 3],
    /// While sand or snow is in the liquid: the sums of the materials' shares and their picture.
    /// Until then a buffer of one pixel stands in, which nothing reads.
    shares: Buffer,
    share_picture: Option<Tex>,
}

impl Drawing {
    fn new(gpu: &Gpu, out: (u32, u32), dx: f64) -> Result<Self, Error> {
        let (splat, depth) = draw_sizes(out, dx, Buffer::max_bytes(gpu));
        let t = |size: (u32, u32)| Tex::new_storage(gpu, size.0, size.1, DRAW_KIND);
        Ok(Self {
            out,
            dx,
            splat,
            depth,
            d: Buffer::of::<u32>(gpu, DRAW_WORDS as u64, "tex.liquid drawing constants")?,
            acc: Buffer::of::<[u32; 4]>(
                gpu,
                u64::from(splat.0) * u64::from(splat.1),
                "tex.liquid drawing sums",
            )?,
            shape: [t(splat)?, t(splat)?, t(splat)?],
            surface: [t(splat)?, t(splat)?],
            deep: [t(depth)?, t(depth)?, t(depth)?],
            shares: Buffer::of::<[u32; 4]>(gpu, 1, "tex.liquid drawing shares")?,
            share_picture: None,
        })
    }

    /// The shares' sums and picture at the splat picture's size, from the first frame with sand
    /// or snow on.
    fn with_shares(&mut self, gpu: &Gpu) -> Result<(), Error> {
        if self.share_picture.is_none() {
            let n = u64::from(self.splat.0) * u64::from(self.splat.1);
            self.shares = Buffer::of::<[u32; 4]>(gpu, n, "tex.liquid drawing shares")?;
            self.share_picture = Some(Tex::new_storage(gpu, self.splat.0, self.splat.1, DRAW_KIND)?);
        }
        Ok(())
    }

    fn bytes(&self) -> u64 {
        self.d.bytes()
            + self.acc.bytes()
            + self.shares.bytes()
            + penumbra_gpu::unique_bytes(
                self.shape.iter().chain(&self.surface).chain(&self.deep).chain(&self.share_picture),
            )
    }

    /// The constants, as `D` in `liquid_draw.wgsl` lays them out.
    fn words(
        &self,
        budget: u32,
        obstacles: bool,
        aspect: f64,
        foam: f64,
        colors: [[f64; 4]; 3],
    ) -> [u32; DRAW_WORDS] {
        let f = |v: f64| (v as f32).to_bits();
        let (sw, sh) = (f64::from(self.splat.0), f64::from(self.splat.1));
        let spacing = self.dx / PER_CELL.sqrt();
        let s = spacing * sh;
        [
            self.splat.0,
            self.splat.1,
            budget,
            u32::from(obstacles),
            f(sw / aspect.max(1e-9)),
            f(sh),
            f(s),
            0,
            f(FAST),
            f(if self.share_picture.is_some() { 1.0 } else { 0.0 }),
            0,
            f(DRAW_FIXED),
            f(SHAPE_BLUR * s),
            f(SURFACE_BLUR * s),
            f(DEPTH_BLUR * f64::from(self.depth.1)),
            0,
            f(spacing * f64::from(self.out.1)),
            f(foam),
            f(PER_CELL),
            0,
        ]
        .into_iter()
        .chain(colors.into_iter().flatten().map(f))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or([0; DRAW_WORDS])
    }
}

/// 32-bit words of the drawing's constants (`D` in `liquid_draw.wgsl`).
const DRAW_WORDS: usize = 32;

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
    /// The materials poured or filled in since the start, bit n for material n: the substeps
    /// and the drawing follow them.
    used: u32,
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
            ga: Buffer::of::<[i32; 4]>(gpu, n, "tex.liquid grid sums")?,
            gf: Buffer::of::<[f32; 4]>(gpu, n, "tex.liquid grid")?,
            solid: Buffer::of::<[f32; 4]>(gpu, n, "tex.liquid solids")?,
            counter: Buffer::of::<u32>(gpu, COUNTERS as u64, "tex.liquid counters")?,
            emitters: Buffer::of::<u32>(
                gpu,
                ((MAX_EMITTERS + MAX_SPLATS) * EMITTER_WORDS) as u64,
                "tex.liquid emitters and splats",
            )?,
            obstacles: None,
            filled: false,
            recount: false,
            runs: Vec::new(),
            frame: 0,
            used: 0,
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
        self.used = 0;
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
        // `run` is the gap between the nozzle and the row poured nearest to it, which the substeps
        // carried on since. The rows that fit the gap pour now; this frame's substeps then open
        // the gap of the next frame. Rows that filled this frame's travel before its substeps
        // would land on the last frame's rows whenever a frame is longer than the one before (a
        // hitch, a project that starts) and burst apart.
        let mut rows = 0.0;
        let mut nearest = 0.0;
        if e.on && speed > 1e-9 {
            rows = (*run / spacing).floor().clamp(0.0, 4096.0);
            nearest = (*run - rows * spacing).clamp(0.0, spacing);
            *run = nearest + if dt > 0.0 { speed * dt } else { 0.0 };
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
            f(nearest),
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
    /// How long foam lasts, seconds.
    foam_time: f64,
    granular: Granular,
    /// The material the `fill` picture places.
    fill_material: u32,
    /// Whether sand or snow is in the liquid: the grid sums their friction then.
    granular_in_use: bool,
    /// The pushes of the `force` picture and the splats.
    push: Push,
}

/// What pushes the liquid besides gravity: the `force` picture and the splats.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Push {
    /// Per second squared per motion of a height per input frame: `force` times the input's
    /// frames per second; 0 while the picture is unwired.
    picture: f64,
    /// Per second: `force`, for the splats.
    splats: f64,
    /// Splats in the buffer after the emitters.
    count: u32,
    /// Whether the splats set the velocity instead of pushing.
    set: bool,
}

/// The shares a substep of `sub` seconds moves the drawing's per-particle values on by: the
/// smoothed density, the foam kept, the smoothed velocity; and one over the velocity's time.
fn look_shares(sub: f64, foam_time: f64) -> [f64; 4] {
    let toward = |time: f64| 1.0 - (-sub / time).exp();
    [toward(DENSITY_TIME), (-sub / foam_time.max(1e-3)).exp(), toward(IMPACT_TIME), 1.0 / IMPACT_TIME]
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
        u32::from(f.open) | (u32::from(f.granular_in_use) << 1) | (f.fill_material.clamp(LIQUID, SNOW) << 8),
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
    ];
    w.extend(look_shares(f.dt, f.foam_time).map(float));
    for values in f.granular.table(f.viscosity) {
        w.extend(values.map(float));
    }
    let p = &f.push;
    w.extend([float(p.picture), float(p.splats), float(MOST_PUSH), float(MOST_MOTION)]);
    w.extend([
        u32::from(p.picture > 0.0),
        p.count.min(MAX_SPLATS as u32),
        u32::from(p.set),
        MAX_EMITTERS as u32,
    ]);
    w
}

/// The splats as the shader reads them, one `Em` each after the emitters: position in heights
/// from the top left and velocity in heights per second, then the radius in heights.
fn splat_words(splats: &[Splat], aspect: f64) -> Vec<u32> {
    let f = |v: f64| (v as f32).to_bits();
    let mut words = Vec::with_capacity(splats.len().min(MAX_SPLATS) * EMITTER_WORDS);
    for s in splats.iter().take(MAX_SPLATS) {
        words.extend([
            f(f64::from(s.at[0]) * aspect),
            f(f64::from(s.at[1])),
            s.velocity[0].to_bits(),
            s.velocity[1].to_bits(),
            s.radius.to_bits(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
    }
    words
}

/// The fastest speed a push of `push` heights per second squared gives across a picture height:
/// the substeps count it as they count a fall.
fn pushed_speed(gravity: f64, push: f64) -> f64 {
    let a = gravity.max(0.0) + push.clamp(0.0, MOST_PUSH);
    if a.is_finite() { (2.0 * a).sqrt() } else { 0.0 }
}

/// Records the drawing of the particles in `parts` into `targets`: `out`, then `foam`, `grain`
/// and `surface` where given. `mask` is the `obstacles` picture.
fn encode_drawing(
    program: &Compute,
    batch: &mut Batch<'_>,
    drawing: &Drawing,
    parts: &Buffer,
    budget: u32,
    mask: Option<&Tex>,
    targets: [Option<&Tex>; 4],
) -> Result<(), Error> {
    let none = |_: &str| None;
    let input =
        |pictures| Dispatch { volumes: &[], pictures, data: None, seconds: 0.0, frame: 0, param: &none };
    let [s0, s1, s2] = &drawing.shape;
    let [f0, f1] = &drawing.surface;
    let [q0, q1, q2] = &drawing.deep;
    let shares = drawing.share_picture.as_ref();
    let plain = input(&[]);
    let masked = [mask];
    let resolve = input(&masked);
    let p_s0 = [Some(s0)];
    let p_s1_s0 = [Some(s1), Some(s0)];
    let p_s2 = [Some(s2), shares];
    let p_q0 = [Some(q0)];
    let p_q1 = [Some(q1)];
    let p_f0_q2 = [Some(f0), Some(q2)];
    let p_s2_f1 = [Some(s2), Some(f1)];
    let (shape_x, shape_y) = (input(&p_s0), input(&p_s1_s0));
    let (from_shape, depth_x, depth_y) = (input(&p_s2), input(&p_q0), input(&p_q1));
    let surface_y = input(&p_f0_q2);
    let shaded = input(&p_s2_f1);
    let mut steps = vec![
        Step { entry: "splat", out: Out::range(budget), input: &plain },
        Step { entry: "resolve", out: Out::Picture(s0), input: &resolve },
    ];
    if let Some(m) = shares {
        steps.push(Step { entry: "resolve_shares", out: Out::Picture(m), input: &plain });
    }
    steps.extend([
        Step { entry: "shape_x", out: Out::Picture(s1), input: &shape_x },
        Step { entry: "shape_y", out: Out::Picture(s2), input: &shape_y },
        Step { entry: "depth_down", out: Out::Picture(q0), input: &from_shape },
        Step { entry: "depth_x", out: Out::Picture(q1), input: &depth_x },
        Step { entry: "depth_y", out: Out::Picture(q2), input: &depth_y },
        Step { entry: "surface_x", out: Out::Picture(f0), input: &from_shape },
        Step { entry: "surface_y", out: Out::Picture(f1), input: &surface_y },
    ]);
    let [out, foam, grain, surface] = targets;
    for (entry, target, input) in [
        ("shade", out, &shaded),
        ("foam_out", foam, &from_shape),
        ("grain_out", grain, &from_shape),
        ("surface_out", surface, &shaded),
    ] {
        if let Some(t) = target {
            steps.push(Step { entry, out: Out::Picture(t), input });
        }
    }
    program.encode_with(batch, &[&drawing.d, parts, &drawing.acc, &drawing.shares], &steps)
}

#[derive(Default)]
struct Liquid {
    program: Option<Arc<Compute>>,
    solid_program: Option<Arc<Compute>>,
    draw_program: Option<Arc<Compute>>,
    sim: Option<Sim>,
    drawing: Option<Drawing>,
    out_ring: TexRing,
    velocity_ring: TexRing,
    density_ring: TexRing,
    foam_ring: TexRing,
    grain_ring: TexRing,
    surface_ring: TexRing,
    last: Option<Tick>,
    /// The counters on their way back, with the particles the emitters asked for in that frame.
    reading: Option<(BufferRead, u32)>,
    spare: Option<BufferRead>,
    counted: Counted,
    /// How often the `force` picture brings a new frame.
    force_gaps: FrameGaps,
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
                    let peak = f64::from(f32::from_bits(at(PEAK)));
                    self.counted.peak = if peak.is_finite() { peak.max(0.0) } else { MOST_PUSH * 2.0 };
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
        let rings = [
            &self.out_ring,
            &self.velocity_ring,
            &self.density_ring,
            &self.foam_ring,
            &self.grain_ring,
            &self.surface_ring,
        ];
        penumbra_gpu::unique_bytes(rings.into_iter().flat_map(TexRing::iter))
            + self.sim.as_ref().map_or(0, Sim::bytes)
            + self.drawing.as_ref().map_or(0, Drawing::bytes)
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
        let force_data = cx.input(FORCE);
        self.force_gaps.count(force_data, now.tick);
        let want_velocity = cx.output_wanted(1);
        let want_density = cx.output_wanted(2);
        let (want_foam, want_grain, want_surface) =
            (cx.output_wanted(3), cx.output_wanted(4), cx.output_wanted(5));
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
        let fill = cx.input_as::<Tex>(FILL);
        let filling = !sim.filled && fill.is_some();
        let fill_material = material_number(cx.text("fill_material")?);
        // The materials in the liquid from now on: what the fill places and what the emitters
        // pour. Kept until a start over, as their particles may be.
        if filling {
            sim.used |= 1 << fill_material;
        }
        for e in emitters.iter().filter(|e| e.on && e.velocity[0].hypot(e.velocity[1]) > 1e-9) {
            sim.used |= 1 << (LIQUID + e.material.min(MATERIALS.len() - 1) as u32);
        }
        let granular = Granular { angle: cx.float("sand_angle")?, wetness: cx.float("snow_wetness")? };
        // The pushes: the force picture's at its input's frame rate, the splats from their table.
        let gain = cx.float("force")?;
        let gain = if gain.is_finite() { gain.clamp(0.0, 1000.0) } else { 0.0 };
        let force = force_data.and_then(|d| d.get::<Tex>()).filter(|_| gain > 0.0);
        if force.is_none() {
            self.counted.peak = 0.0;
        }
        let splats = match cx.input_as::<Table>(SPLATS) {
            Some(t) => {
                let radius = cx.float("splat_radius")?;
                let radius = if radius.is_finite() { radius.clamp(1e-4, 10.0) } else { 0.05 };
                match read_splats(t, aspect as f32, [0.0; 3], radius as f32) {
                    Ok(s) => s,
                    Err(e) => {
                        cx.warn(e);
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };
        let push = Push {
            picture: if force.is_some() { gain * self.force_gaps.rate(period) } else { 0.0 },
            splats: gain,
            count: splats.len().min(MAX_SPLATS) as u32,
            set: cx.text("splat_motion")? == "set",
        };
        let splat_speed =
            splats.iter().map(|s| f64::from(s.velocity[0].hypot(s.velocity[1]))).fold(0.0, f64::max);
        let splat_push = if push.set { 0.0 } else { gain * splat_speed };
        let pushing = self.counted.peak + splat_push;
        if pushing > MOST_PUSH {
            cx.warn(format!(
                "`force` and the splats push with up to {pushing:.0} heights per second squared, more than \
                 {MOST_PUSH:.0}: the push is cut to {MOST_PUSH:.0}. Lower `force`"
            ));
        }
        // The fastest flow: a fall across the picture under gravity and the pushes, an emitter's
        // stream or a splat that sets its speed, or a wave in sand or snow.
        let fall = pushed_speed(gravity[0].hypot(gravity[1]), pushing);
        let pour = emitters.iter().map(|e| e.velocity[0].hypot(e.velocity[1])).fold(0.0, f64::max);
        let pour = if push.set { pour.max(splat_speed) } else { pour };
        let fastest = MIN_FLOW.max(fall).max(pour).max(fastest_wave(sim.used));
        let (count, dt, needed) = substeps(dt, sim.dx, fastest, viscosity);
        if needed > count {
            let stiff = if sim.used & !(1 << LIQUID) != 0 { ", or use less sand and snow" } else { "" };
            cx.warn(format!(
                "a frame needs {needed} substeps, at most {MAX_SUBSTEPS} run: the liquid moves slower than the show. \
                 Lower `grid`, `viscosity` or `speed`{stiff}"
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
        if !splats.is_empty() {
            sim.emitters.write(
                gpu,
                (MAX_EMITTERS * EMITTER_WORDS) as u64,
                &splat_words(&splats, sim.aspect),
            )?;
        }
        let color_of = |name: &str| match cx.param(name) {
            Some(Value::Vec4(c)) => c.map(|v| if v.is_finite() { v } else { 0.0 }),
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        let colors = [color_of("color"), color_of("sand_color"), color_of("snow_color")];
        let foam_strength = cx.float("foam_strength")?;
        let foam_strength = if foam_strength.is_finite() { foam_strength.clamp(0.0, 1.0) } else { 0.0 };
        let foam_time = cx.float("foam_time")?;
        let foam_time = if foam_time.is_finite() { foam_time.clamp(0.01, 60.0) } else { 0.4 };
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
            foam_time,
            granular,
            fill_material,
            granular_in_use: sim.used & !(1 << LIQUID) != 0,
            push,
        };
        let layout =
            Layout { nodes: sim.nodes, budget: sim.budget, dx: sim.dx, aspect: sim.aspect, frame: sim.frame };
        sim.u.write(gpu, 0, &uniform_words(layout, &frame))?;
        // This frame's counts start at zero; what is in use and the fill's count go on.
        sim.counter.write(gpu, 0, &[0u32; 3])?;
        if sim.recount {
            sim.counter.write(gpu, LIVE as u64, &[0u32])?;
        }
        // The force picture is measured in the substeps only: a cook without them keeps the last
        // measure.
        if force.is_some() && count > 0 {
            sim.counter.write(gpu, PEAK as u64, &[0u32])?;
        }

        let (cw, ch) = sim.cells;
        let out = self.out_ring.next_storage(gpu, width, height, KIND)?;
        let velocity_out =
            if want_velocity { Some(self.velocity_ring.next_storage(gpu, cw, ch, KIND)?) } else { None };
        let density_out =
            if want_density { Some(self.density_ring.next_storage(gpu, cw, ch, KIND)?) } else { None };
        let foam_out =
            if want_foam { Some(self.foam_ring.next_storage(gpu, width, height, KIND)?) } else { None };
        let grain_out =
            if want_grain { Some(self.grain_ring.next_storage(gpu, width, height, KIND)?) } else { None };
        let surface_out =
            if want_surface { Some(self.surface_ring.next_storage(gpu, width, height, KIND)?) } else { None };
        let draw_program = match &self.draw_program {
            Some(p) => p.clone(),
            None => {
                let p = draw_program(gpu)?;
                self.draw_program = Some(p.clone());
                p
            }
        };
        if !self.drawing.as_ref().is_some_and(|d| d.out == (width, height) && d.dx == sim.dx) {
            self.drawing = None;
            self.drawing = Some(Drawing::new(gpu, (width, height), sim.dx)?);
        }
        let Some(drawing) = self.drawing.as_mut() else {
            return Err(CookError::failed("the liquid has no drawing"));
        };
        if sim.used & !(1 << LIQUID) != 0 {
            drawing.with_shares(gpu)?;
        }
        let drawing = &*drawing;
        drawing.d.write(
            gpu,
            0,
            &drawing.words(sim.budget, mask.is_some(), sim.aspect, foam_strength, colors),
        )?;

        let times: Vec<f64> = (0..count).map(|k| k as f64 * sub).collect();
        let params: Vec<_> =
            times.iter().map(|t| move |n: &str| (n == "t").then_some(Value::Float(*t))).collect();
        let zero = |n: &str| (n == "t").then_some(Value::Float(0.0));
        let dispatch =
            |pictures, param| Dispatch { volumes: &[], pictures, data: None, seconds: 0.0, frame: 0, param };
        let plain = dispatch(&[], &zero);
        let fill_pictures = [fill];
        let filled_from = dispatch(&fill_pictures, &zero);
        // The force picture is read in the substeps only.
        let force_pictures = [force];
        let pushed: &[Option<&Tex>] = if force.is_some() { &force_pictures } else { &[] };
        let substep_inputs: Vec<Dispatch<'_>> = params.iter().map(|p| dispatch(pushed, p)).collect();
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
        encode_drawing(
            &draw_program,
            &mut batch,
            drawing,
            &sim.parts,
            sim.budget,
            mask,
            [Some(&out), foam_out.as_ref(), grain_out.as_ref(), surface_out.as_ref()],
        )?;
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
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Tex, out),
            lazy(velocity_out),
            lazy(density_out),
            lazy(foam_out),
            lazy(grain_out),
            lazy(surface_out),
        ]))
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
    fn sand_and_snow_take_more_substeps_and_liquid_alone_keeps_its_count() {
        let (frame, dx) = (1.0 / 60.0, 1.0 / 216.0);
        let at = |used: u32| substeps(frame, dx, MIN_FLOW.max(fastest_wave(used)), 0.000_222).0;
        assert_eq!(fastest_wave(0), SOUND);
        assert_eq!(fastest_wave(1 << LIQUID), SOUND);
        assert_eq!(at(1 << LIQUID), 21);
        assert_eq!(at(1 << LIQUID | 1 << SAND), 24);
        assert_eq!(at(1 << SNOW), 44);
        assert_eq!(at(1 << LIQUID | 1 << SAND | 1 << SNOW), 44);
        // Snow on the finest grid at 30 fps hits the cap; liquid there does not.
        assert!(substeps(1.0 / 30.0, 1.0 / 576.0, SOUND, 0.0).0 < MAX_SUBSTEPS);
        let (n, _, needed) = substeps(1.0 / 30.0, 1.0 / 576.0, fastest_wave(1 << SNOW), 0.0);
        assert!(n == MAX_SUBSTEPS && needed > MAX_SUBSTEPS, "{n} {needed}");
    }

    #[test]
    fn the_material_table_follows_the_params_and_survives_bad_numbers() {
        let base = Granular::default().table(0.000_222);
        // The liquid's row is what it was before sand and snow.
        let c2g = SOUND * SOUND / GAMMA;
        assert_eq!(base[0], [0.0, 1.0, c2g, GAMMA, 0.000_222, 0.0, MAX_RATIO, 0.0]);
        // A steeper angle of repose: more internal friction and more friction on solids.
        let steep = Granular { angle: 40.0, ..Granular::default() }.table(0.0);
        assert!(steep[1][4] > base[1][4] && steep[1][0] > base[1][0]);
        // Wet snow yields later, hardens less and sticks more than powder.
        let powder = Granular { wetness: 0.0, ..Granular::default() }.table(0.0);
        let wet = Granular { wetness: 1.0, ..Granular::default() }.table(0.0);
        assert_eq!([powder[2][4], powder[2][5], powder[2][6]], SNOW_POWDER);
        assert_eq!([wet[2][4], wet[2][5], wet[2][6]], SNOW_WET);
        assert_eq!([base[2][4], base[2][5], base[2][6]], SNOW_FRESH);
        assert!(wet[2][0] > powder[2][0]);
        for bad in [f64::NAN, f64::INFINITY, -1e30] {
            let t = Granular { angle: bad, wetness: bad }.table(0.0);
            assert!(t.iter().flatten().all(|v| v.is_finite()), "{bad}: {t:?}");
        }
        // Every material is named, and a name the node does not know is liquid.
        assert_eq!(material_number("sand"), SAND);
        assert_eq!(material_number("snow"), SNOW);
        assert_eq!(material_number("lava"), LIQUID);
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
        // Eight vectors of four words, four materials of eight, then two vectors of the pushes.
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
            foam_time: 0.4,
            granular: Granular::default(),
            fill_material: SNOW,
            granular_in_use: true,
            push: Push { picture: 240.0, splats: 4.0, count: 70, set: true },
        };
        let words = uniform_words(layout, &frame);
        assert_eq!(words.len(), UNIFORM_WORDS);
        // Open edges in bit 0, friction summed per cell in bit 1, the fill's material in bits 8
        // to 15.
        assert_eq!(words[3], 1 | 2 | (SNOW << 8));
        assert_eq!(UNIFORM_WORDS, 8 * 4 + 4 * 8 + 2 * 4);
        // The pushes: the picture read, at most 64 splats, set, and the splats after the emitters.
        assert_eq!(words[68..72], [1, MAX_SPLATS as u32, 1, MAX_EMITTERS as u32]);
        assert_eq!(f32::from_bits(words[66]), MOST_PUSH as f32);
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
        assert_eq!(
            members,
            ["grid", "cell", "fixed", "wall", "force", "room", "emit", "draw", "mats", "push", "pushes"]
        );
        let em = &SOURCE[SOURCE.find("struct Em {").unwrap()..];
        let em = &em[..em.find('}').unwrap()];
        assert_eq!(em.matches("vec4").count() * 4, EMITTER_WORDS);
    }

    /// The members of `struct name` in `source`, without comments.
    fn members(source: &str, name: &str) -> Vec<String> {
        let start = source.find(&format!("struct {name} {{")).unwrap();
        let body = &source[start..start + source[start..].find('}').unwrap()];
        body.lines()
            .skip(1)
            .map(|l| l.split("//").next().unwrap().trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    fn the_drawing_reads_the_particles_and_its_constants_as_laid_out() {
        // The drawing's particle is the simulation's.
        assert_eq!(members(DRAW_SOURCE, "P"), members(SOURCE, "P"));
        let d = members(DRAW_SOURCE, "D");
        assert!(d.iter().all(|m| m.ends_with("vec4u,") || m.ends_with("vec4f,")), "{d:?}");
        assert_eq!(d.len() * 4, DRAW_WORDS);
    }

    #[test]
    fn the_drawing_puts_particles_about_one_and_a_half_pixels_apart() {
        let most = 1u64 << 30;
        // The default: 2.5 pixels between particles at 1080, so half the size.
        let dx = 1.0 / 216.0;
        assert_eq!(draw_sizes((1920, 1080), dx, most), ((960, 540), (480, 270)));
        // A fine grid on a small picture: the picture's own size.
        assert_eq!(draw_sizes((128, 96), 1.0 / 48.0, most).0, (128, 96));
        // A coarse grid on a large picture: much smaller, and the depth at most 270 rows.
        let ((w, h), depth) = draw_sizes((16384, 16384), 1.0 / 16.0, most);
        let spacing = f64::from(h) / 32.0;
        assert!(w == h && spacing > 1.0 && spacing < 2.2, "{w} {h} {spacing}");
        assert_eq!(depth, (270, 270));
        // Sums that would not fit a buffer take a coarser picture.
        let ((w, h), _) = draw_sizes((8192, 8192), 1.0 / 4096.0, 1 << 20);
        assert!(u64::from(w) * u64::from(h) * 16 <= 1 << 20, "{w} {h}");
        // Nothing breaks on a picture of one pixel.
        assert_eq!(draw_sizes((1, 1), 1.0, most), ((1, 1), (1, 1)));
        assert_eq!(draw_sizes((0, 0), f64::NAN, most).0, (1, 1));
    }

    #[test]
    fn pushes_count_into_the_substeps_as_a_fall_and_stop_at_the_cut() {
        // Nothing pushing: the fall under gravity alone, as before the pushes.
        assert_eq!(pushed_speed(3.27, 0.0), (2.0f64 * 3.27).sqrt());
        assert!(pushed_speed(3.27, 10.0) > pushed_speed(3.27, 1.0));
        // Beyond the cut the speed grows no more, and bad numbers give no speed.
        assert_eq!(pushed_speed(0.0, 1e9), pushed_speed(0.0, MOST_PUSH));
        assert_eq!(pushed_speed(f64::INFINITY, 0.0), 0.0);
        assert_eq!(pushed_speed(0.0, f64::NAN), 0.0);
        // The strongest push takes the default grid to about 2.6 times the substeps.
        let (frame, dx) = (1.0 / 60.0, 1.0 / 216.0);
        let most = substeps(frame, dx, MIN_FLOW.max(pushed_speed(3.27, MOST_PUSH)), 0.000_222).0;
        assert!(most > 21 && most < 64, "{most}");
    }

    #[test]
    fn splats_go_after_the_emitters_in_heights() {
        let s = Splat { at: [0.5, 0.25], velocity: [1.0, -2.0], color: [9.0; 3], radius: 0.1 };
        let words = splat_words(&[s; 70], 2.0);
        assert_eq!(words.len(), MAX_SPLATS * EMITTER_WORDS);
        let f = |i: usize| f32::from_bits(words[i]);
        assert_eq!([f(0), f(1), f(2), f(3), f(4)], [1.0, 0.25, 1.0, -2.0, 0.1]);
    }

    #[test]
    fn the_looks_shares_do_not_depend_on_the_substeps() {
        // A frame of 1/60 s in 21 or in 5 substeps keeps the same share of the foam and moves
        // the smoothed density as far.
        let frame = 1.0 / 60.0;
        for n in [1, 5, 21, 128] {
            let [rho, foam, vel, inv] = look_shares(frame / n as f64, 0.4);
            let kept = |share: f64| (1.0 - share).powi(n);
            assert!((foam.powi(n) - (-frame / 0.4f64).exp()).abs() < 1e-12, "{n}");
            assert!((kept(rho) - (-frame / DENSITY_TIME).exp()).abs() < 1e-12, "{n}");
            assert!((kept(vel) - (-frame / IMPACT_TIME).exp()).abs() < 1e-12, "{n}");
            assert_eq!(inv, 60.0);
        }
        // No time keeps everything.
        let [rho, foam, vel, _] = look_shares(0.0, 0.4);
        assert_eq!((rho, foam, vel), (0.0, 1.0, 0.0));
    }
}
