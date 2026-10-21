//! `tex.fluid`: a 2D fluid (Stam's Stable Fluids) on the GPU (`fluid.wgsl`). Every show frame:
//! forces in (a motion picture such as `tex.optical_flow`'s, splats from a table such as
//! `tex.blob_track`'s, vorticity confinement), the velocity carried along by itself, optionally
//! diffused, made free of divergence by Jacobi steps on the pressure, and the dye carried along
//! by the result. The velocity lives on a coarse grid (`size`), the dye at the output's size.
//!
//! The state (velocity, dye, and the pressure as the next frame's first guess) is kept from cook
//! to cook. Time steps follow the show clock: a paused show stands still, a frame skipped by a
//! late show is a longer step (up to [`MAX_DT`]), time running backwards is one frame's step.
//! A new `size` or output size carries the state over, resampled. Velocity and dye go into the
//! state snapshots, so a restart carries on where it stopped.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Table, Tick, Unit, Value};
use penumbra_gpu::{Batch, Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
    StateItem,
};

use super::{gpu, height_param, keep_size, side, width_param};

pub(crate) const KIND: &str = "tex.fluid";
const SOURCE: &str = include_str!("fluid.wgsl");

/// Longest time step, in seconds: a longer gap (a stalled show, a jump ahead) does not simulate
/// the time in between, and semi-Lagrangian steps much longer than this smear.
pub(crate) const MAX_DT: f64 = 0.05;
/// Most splats taken from the table (the shader's `PN_FL_MAX_SPLATS`).
pub(crate) const MAX_SPLATS: usize = 64;
/// Most pressure steps per frame.
const MAX_ITERATIONS: i64 = 80;
/// Jacobi steps of viscosity per frame, when there is any.
const VISCOSITY_STEPS: usize = 8;
/// How much of last frame's pressure the first Jacobi step starts from.
const WARM: f64 = 0.8;
/// Largest side of the simulation grid.
const MAX_GRID: u32 = 1024;

const FORCE: i64 = 0;
const ADVECT: i64 = 1;
const DIFFUSE: i64 = 2;
const DIVERGENCE: i64 = 3;
const JACOBI: i64 = 4;
const GRADIENT: i64 = 5;
const DYE: i64 = 6;
const RESAMPLE: i64 = 7;
const VELOCITY: i64 = 8;
const JACOBI2: i64 = 9;

/// Column names a splat table may have; `x` and `y` are needed.
const COLUMNS: [&str; 8] = ["x", "y", "vx", "vy", "r", "g", "b", "radius"];

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "A 2D fluid: motion pushes it, dye flows with it (Stable Fluids)")
        .stateful()
        .time_dependent()
        .param(width_param())
        .param(height_param())
        .param(keep_size())
        .param(
            ParamSpec::float("force", 4.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 20.0)
                .curve(Curve::Power(2.0))
                .doc(
                    "How strongly motion pushes the fluid: per second of motion, this many times its \
                     speed is added to the fluid's (the force input and the splats)",
                ),
        )
        .param(
            ParamSpec::float("velocity_fade", 2.0)
                .clamp(0.0, 3600.0)
                .range(0.05, 30.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("Time in which the fluid loses half its speed; longer keeps it swirling"),
        )
        .param(
            ParamSpec::float("dye_fade", 3.0)
                .clamp(0.0, 3600.0)
                .range(0.05, 60.0)
                .curve(Curve::Log)
                .unit(Unit::Seconds)
                .doc("Time in which the dye loses half its strength"),
        )
        .param(
            ParamSpec::choice("inject", &["moving", "everywhere"])
                .doc("Where dye comes in")
                .choice_doc(
                    "moving",
                    "where the force input or a splat moves, the faster the more: the dye input's \
                     colours, or `color` when nothing is wired there",
                )
                .choice_doc("everywhere", "the dye input everywhere, splats whether they move or not"),
        )
        .param(
            ParamSpec::float("amount", 4.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 30.0)
                .curve(Curve::Power(2.0))
                .doc(
                    "How fast the dye takes on the colour poured in, per second (where it moves fast):                      at 4 about two thirds in a quarter of a second; it paints over, so the dye never                      gets brighter than its colours",
                ),
        )
        .param(
            ParamSpec::color("color", [0.35, 0.7, 1.0, 1.0]).doc(
                "The dye's colour where the dye input is not wired (its alpha slows the pouring), and                  of splats without r, g, b",
            ),
        )
        .param(
            ParamSpec::float("vorticity", 0.3)
                .clamp(0.0, 100.0)
                .range(0.0, 2.0)
                .doc("Brings back the small swirls the coarse grid smooths away; 0: off"),
        )
        .param(
            ParamSpec::float("viscosity", 0.0)
                .clamp(0.0, 1.0)
                .range(0.0, 0.01)
                .curve(Curve::Power(3.0))
                .more()
                .doc(
                    "Thickness: neighbouring parts take on each other's motion (honey rather than \
                     smoke), in image heights squared per second; 0: off, costs nothing",
                ),
        )
        .param(
            ParamSpec::int("size", 256)
                .clamp(16.0, f64::from(MAX_GRID))
                .range(64.0, 512.0)
                .curve(Curve::Log)
                .unit(Unit::Pixels)
                .more()
                .doc(
                    "Width of the simulation grid (the height follows the picture's shape); more \
                     gives finer swirls and costs more; the dye keeps the output's size",
                ),
        )
        .param(
            ParamSpec::int("iterations", 20)
                .clamp(1.0, MAX_ITERATIONS as f64)
                .range(4.0, 60.0)
                .more()
                .doc(
                    "Steps that keep the fluid from being squeezed together; fewer cost less and let \
                     it bulge and leak",
                ),
        )
        .param(
            ParamSpec::float("splat_radius", 0.05)
                .clamp(0.0001, 10.0)
                .range(0.005, 0.3)
                .curve(Curve::Log)
                .more()
                .doc("Radius of a splat from the table without a radius column, in image heights"),
        )
        .param(
            ParamSpec::choice("edges", &["walls", "open"])
                .more()
                .doc("What the picture's edges are to the fluid")
                .choice_doc("walls", "it turns along them")
                .choice_doc("open", "it flows out, with the dye"),
        )
        .param(
            ParamSpec::bool("reset", false)
                .doc("Starts over from a still fluid without dye, every frame while on"),
        )
        .param(
            ParamSpec::bool("persist", true)
                .more()
                .doc("Keep velocity and dye in the state snapshots, so a restart carries on from them"),
        )
        .input("force", DataKind::Tex)
        .describe_input(
            "force",
            "Motion that pushes the fluid, in image sizes per input frame (red x in widths, green y in \
             heights, down positive): tex.optical_flow's output as it is",
        )
        .input("dye", DataKind::Tex)
        .describe_input("dye", "Colour poured in (see `inject`); unwired, `color` where it moves")
        .input("obstacles", DataKind::Tex)
        .describe_input("obstacles", "Solid where brighter than half: the fluid flows around it")
        .input("splats", DataKind::Table)
        .describe_input(
            "splats",
            "Pushes and dye at points, one per row (at most 64): columns x, y (0 to 1, top left), \
             optional vx, vy (image sizes per second), r, g, b and radius (image heights); \
             tex.blob_track's table as it is",
        )
        .output("out", DataKind::Tex)
        .describe_output("out", "The dye, at width x height")
        .output("velocity", DataKind::Tex)
        .describe_output(
            "velocity",
            "The fluid's velocity at the grid's size, in image sizes per second (red x in widths, green y \
             in heights, down positive); tex.displace with midpoint 0 moves a picture along it",
        )
        .lazy_output("velocity")
}

/// The pass's params: nothing the user sets directly, the node works them out per pass.
fn pass_params() -> Vec<ParamSpec> {
    let mut params: Vec<ParamSpec> = [
        "dt",
        "aspect",
        "h",
        "rate",
        "force",
        "vel_fade",
        "dye_fade",
        "vorticity",
        "viscous",
        "warm",
        "amount",
    ]
    .iter()
    .map(|n| ParamSpec::float(n, 0.0))
    .collect();
    for n in ["stage", "edges", "obstacles", "force_in", "dye_in", "moving", "splats"] {
        params.push(ParamSpec::int(n, 0));
    }
    params.push(ParamSpec::color("color", [0.0, 0.0, 0.0, 1.0]));
    params
}

fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    gpu.cached_pass(KIND, SOURCE, &pass_params())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(Fluid::default()) as Box<dyn Operator>))
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

/// The simulation grid for an output of `width` x `height` and a grid `size` wide: square
/// cells, at most [`MAX_GRID`] on a side and never finer than the output.
pub(crate) fn grid(width: u32, height: u32, size: i64) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let gw = (size.clamp(16, i64::from(MAX_GRID)) as f64).min(w);
    let gh = (gw * h / w).round();
    let scale = (f64::from(MAX_GRID) / gh).min(1.0);
    let (gw, gh) = ((gw * scale).round().max(1.0), (gh * scale).round().max(1.0));
    (gw as u32, gh as u32)
}

/// The time step for a show frame at `now`, the last one at `last`, one frame lasting `period`
/// seconds: none while time stands still, one frame's when it ran backwards, at most
/// [`MAX_DT`].
pub(crate) fn step(now: Tick, last: Option<Tick>, period: f64) -> f64 {
    let Some(last) = last else { return period.min(MAX_DT) };
    let d = Tick(now.0.saturating_sub(last.0)).seconds();
    if d > 0.0 {
        d.min(MAX_DT)
    } else if d < 0.0 {
        period.min(MAX_DT)
    } else {
        0.0
    }
}

/// The factor that leaves half after `half_life` seconds, for a step of `dt`.
pub(crate) fn fade(dt: f64, half_life: f64) -> f64 {
    if half_life.is_infinite() && half_life > 0.0 {
        return 1.0;
    }
    if half_life.is_nan() || half_life <= 0.0 {
        return 0.0;
    }
    0.5f64.powf(dt / half_life)
}

/// One splat: position (0..1), velocity (heights per second), colour, radius (heights).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Splat {
    pub at: [f32; 2],
    pub velocity: [f32; 2],
    pub color: [f32; 3],
    pub radius: f32,
}

/// The splats of a table whose first row names the columns ([`COLUMNS`]); rows with a value
/// that is not a finite number are left out. `aspect` is the picture's width over its height
/// (vx comes in widths per second, the fluid wants heights); `color` and `radius` stand in for
/// missing columns. `Err` when the table has rows but no x and y columns.
pub(crate) fn splats(table: &Table, aspect: f32, color: [f32; 3], radius: f32) -> Result<Vec<Splat>, String> {
    let Some((header, rows)) = table.rows().split_first() else { return Ok(Vec::new()) };
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let [x, y, vx, vy, r, g, b, rad] = COLUMNS.map(col);
    let (Some(x), Some(y)) = (x, y) else {
        return Err("the splats table needs columns x and y in its first row".into());
    };
    let value = |row: &[String], c: Option<usize>, default: f32| -> Option<f32> {
        match c {
            None => Some(default),
            Some(c) => row.get(c)?.trim().parse::<f32>().ok().filter(|v| v.is_finite()),
        }
    };
    let mut out = Vec::new();
    for row in rows.iter().take(MAX_SPLATS) {
        let read = || -> Option<Splat> {
            let at = [value(row, Some(x), 0.0)?, value(row, Some(y), 0.0)?];
            let velocity = [value(row, vx, 0.0)? * aspect, value(row, vy, 0.0)?];
            let color = [value(row, r, color[0])?, value(row, g, color[1])?, value(row, b, color[2])?];
            let radius = value(row, rad, radius)?;
            Some(Splat {
                at: at.map(|v| v.clamp(-1.0, 2.0)),
                velocity: velocity.map(|v| v.clamp(-16.0, 16.0)),
                color: color.map(|v| v.clamp(0.0, 64.0)),
                radius: radius.clamp(1e-4, 10.0),
            })
        };
        if let Some(s) = read() {
            out.push(s);
        }
    }
    Ok(out)
}

/// The splats as a [`MAX_SPLATS`] x 2 texture's half floats: row 0 position and velocity, row
/// 1 colour and radius.
fn splat_bytes(splats: &[Splat]) -> Vec<u8> {
    use half::f16;
    let mut values = vec![0.0f32; MAX_SPLATS * 2 * 4];
    for (i, s) in splats.iter().take(MAX_SPLATS).enumerate() {
        let top = i * 4;
        let bottom = (MAX_SPLATS + i) * 4;
        if let Some(t) = values.get_mut(top..top + 4) {
            t.copy_from_slice(&[s.at[0], s.at[1], s.velocity[0], s.velocity[1]]);
        }
        if let Some(b) = values.get_mut(bottom..bottom + 4) {
            b.copy_from_slice(&[s.color[0], s.color[1], s.color[2], s.radius]);
        }
    }
    values.iter().flat_map(|v| f16::from_f32(*v).to_le_bytes()).collect()
}

#[derive(Default)]
struct Fluid {
    velocity_ring: TexRing,
    pressure_ring: TexRing,
    divergence_ring: TexRing,
    dye_ring: TexRing,
    out_ring: TexRing,
    splat_ring: TexRing,
    /// The state: velocity (red, green; blue where the force moved), pressure, dye.
    velocity: Option<Tex>,
    pressure: Option<Tex>,
    dye: Option<Tex>,
    /// Velocity and dye from a state snapshot, taken over at the next cook.
    restored: Option<(Tex, Tex)>,
    /// The show time of the last step.
    last: Option<Tick>,
    /// The force input's last frame, when it came, and the smoothed time between its frames.
    force_frame: Option<Data>,
    force_at: Option<Tick>,
    interval: Option<f64>,
    /// The last velocity output, while it is read.
    velocity_out: Option<Tex>,
    persist: bool,
}

/// What every pass of one step shares.
struct Shared {
    dt: f64,
    aspect: f64,
    h: f64,
    rate: f64,
    force: f64,
    vel_fade: f64,
    dye_fade: f64,
    vorticity: f64,
    viscous: f64,
    amount: f64,
    edges: i64,
    obstacles: bool,
    force_in: bool,
    dye_in: bool,
    moving: bool,
    splats: i64,
    color: Value,
}

impl Shared {
    fn param(&self, name: &str, stage: i64, warm: f64) -> Option<Value> {
        let f = |v: f64| Some(Value::Float(v));
        let i = |v: i64| Some(Value::Int(v));
        match name {
            "stage" => i(stage),
            "warm" => f(warm),
            "dt" => f(self.dt),
            "aspect" => f(self.aspect),
            "h" => f(self.h),
            "rate" => f(self.rate),
            "force" => f(self.force),
            "vel_fade" => f(self.vel_fade),
            "dye_fade" => f(self.dye_fade),
            "vorticity" => f(self.vorticity),
            "viscous" => f(self.viscous),
            "amount" => f(self.amount),
            "edges" => i(self.edges),
            "obstacles" => i(i64::from(self.obstacles)),
            "force_in" => i(i64::from(self.force_in)),
            "dye_in" => i(i64::from(self.dye_in)),
            "moving" => i(i64::from(self.moving)),
            "splats" => i(self.splats),
            "color" => Some(self.color.clone()),
            _ => None,
        }
    }
}

impl Fluid {
    /// Forgets velocity, pressure and dye.
    fn clear(&mut self) {
        self.velocity = None;
        self.pressure = None;
        self.dye = None;
        self.restored = None;
    }

    /// Updates the smoothed time between the force input's frames with a new frame at `now`.
    fn count_force_frame(&mut self, data: Option<&Data>, now: Tick) {
        let Some(data) = data else {
            self.force_frame = None;
            self.force_at = None;
            return;
        };
        if self.force_frame.as_ref().is_some_and(|f| f.same(data)) {
            return;
        }
        if let Some(at) = self.force_at {
            let gap = Tick(now.0.saturating_sub(at.0)).seconds();
            if gap > 0.0 {
                let gap = gap.clamp(1.0 / 480.0, 0.5);
                self.interval = Some(match self.interval {
                    Some(i) => i + (gap - i) * 0.25,
                    None => gap,
                });
            }
        }
        self.force_frame = Some(data.clone());
        self.force_at = Some(now);
    }
}

impl Operator for Fluid {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = pass(gpu)?;
        self.persist = cx.bool("persist")?;
        let now = cx.time();
        let period = 1.0 / cx.rate().as_f64().max(1.0);
        let reset = cx.bool("reset")?;
        if reset {
            self.clear();
        }

        let (width, height) = (side(cx, "width")?, side(cx, "height")?);
        let (gw, gh) = grid(width, height, cx.int("size")?);
        let want_velocity = cx.output_wanted(1);
        let dt = step(now.tick, self.last, period);
        let force_data = cx.input(0);
        self.count_force_frame(force_data, now.tick);
        let sizes_kept = self.velocity.as_ref().is_some_and(|v| v.size() == (gw, gh))
            && self.dye.as_ref().is_some_and(|d| d.size() == (width, height));
        if dt == 0.0
            && sizes_kept
            && self.restored.is_none()
            && !reset
            && !cx.params_changed()
            && !cx.outputs_wanted_changed()
            && !cx.first_cook()
        {
            // The show stands still: so does the fluid.
            return Ok(Cooked::Unchanged);
        }
        self.last = Some(now.tick);

        let force = force_data.and_then(|d| d.get::<Tex>());
        let dye_in = cx.input_as::<Tex>(1);
        let obstacles = cx.input_as::<Tex>(2);
        let aspect = f64::from(gw) / f64::from(gh);
        let color = match cx.param("color") {
            Some(Value::Vec4(c)) => *c,
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        let paint = [color[0], color[1], color[2]].map(|v| v as f32);
        let splats = match cx.input_as::<Table>(3) {
            Some(t) => {
                let radius = cx.float("splat_radius")? as f32;
                match splats(t, f64::from(width) as f32 / height.max(1) as f32, paint, radius) {
                    Ok(s) => s,
                    Err(e) => {
                        cx.warn(e);
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };
        let splat_tex = if splats.is_empty() {
            None
        } else {
            let t = self.splat_ring.next(gpu, MAX_SPLATS as u32, 2, KIND)?;
            gpu.write_rgba16f(&t, &mut splat_bytes(&splats))?;
            Some(t)
        };
        let h = 1.0 / f64::from(gh);
        let viscosity = cx.float("viscosity")?.max(0.0);
        let shared = Shared {
            dt,
            aspect,
            h,
            rate: 1.0 / self.interval.unwrap_or(period).max(1e-3),
            force: cx.float("force")?,
            vel_fade: fade(dt, cx.float("velocity_fade")?),
            dye_fade: fade(dt, cx.float("dye_fade")?),
            vorticity: cx.float("vorticity")?.max(0.0),
            viscous: viscosity * dt / (h * h),
            amount: cx.float("amount")?,
            edges: i64::from(cx.text("edges")? == "open"),
            obstacles: obstacles.is_some(),
            force_in: force.is_some(),
            dye_in: dye_in.is_some(),
            moving: cx.text("inject")? == "moving",
            splats: splats.len() as i64,
            color: Value::Vec4(color),
        };

        let mut batch = Batch::new(gpu, KIND);
        let mut render = |target: &Tex, stage: i64, warm: f64, inputs: &[Option<&Tex>]| {
            let param = |name: &str| shared.param(name, stage, warm);
            pass.encode(&mut batch, target, &PassInput { inputs, seconds: 0.0, frame: 0, param: &param });
        };

        // State from a snapshot, or of another size: resampled onto the current sizes.
        if let Some((v, d)) = self.restored.take() {
            self.velocity = Some(v);
            self.dye = Some(d);
            self.pressure = None;
        }
        if let Some(v) = self.velocity.take() {
            self.velocity = Some(if v.size() == (gw, gh) {
                v
            } else {
                let t = self.velocity_ring.next(gpu, gw, gh, KIND)?;
                render(&t, RESAMPLE, 1.0, &[Some(&v)]);
                t
            });
        }
        if let Some(d) = self.dye.take() {
            self.dye = Some(if d.size() == (width, height) {
                d
            } else {
                let t = self.dye_ring.next(gpu, width, height, KIND)?;
                render(&t, RESAMPLE, 1.0, &[Some(&d)]);
                t
            });
        }
        let previous_pressure = self.pressure.take().filter(|p| p.size() == (gw, gh));

        // Forces, then the velocity carried along by itself.
        let forced = self.velocity_ring.next(gpu, gw, gh, KIND)?;
        render(&forced, FORCE, 1.0, &[self.velocity.as_ref(), force, splat_tex.as_ref(), obstacles]);
        self.velocity = None;
        let advected = self.velocity_ring.next(gpu, gw, gh, KIND)?;
        render(&advected, ADVECT, 1.0, &[Some(&forced), None, None, obstacles]);
        drop(forced);
        let mut velocity = advected;
        if shared.viscous > 0.0 {
            let before = velocity.clone();
            for _ in 0..VISCOSITY_STEPS {
                let t = self.velocity_ring.next(gpu, gw, gh, KIND)?;
                render(&t, DIFFUSE, 1.0, &[Some(&velocity), Some(&before), None, obstacles]);
                velocity = t;
            }
        }

        // Projection: the divergence, the pressure that cancels it, its gradient taken off.
        let divergence = self.divergence_ring.next(gpu, gw, gh, KIND)?;
        render(&divergence, DIVERGENCE, 1.0, &[Some(&velocity), None, None, obstacles]);
        let iterations = cx.int("iterations")?.clamp(1, MAX_ITERATIONS);
        let mut pressure = previous_pressure;
        let mut done = 0;
        while done < iterations {
            // Two steps per pass where two are left. The warm start only scales the first
            // pass's reads, which all come from last frame's pressure.
            let (stage, steps) = if iterations - done >= 2 { (JACOBI2, 2) } else { (JACOBI, 1) };
            let warm = if done == 0 { WARM } else { 1.0 };
            let t = self.pressure_ring.next(gpu, gw, gh, KIND)?;
            render(&t, stage, warm, &[pressure.as_ref(), Some(&divergence), None, obstacles]);
            pressure = Some(t);
            done += steps;
        }
        let projected = self.velocity_ring.next(gpu, gw, gh, KIND)?;
        render(&projected, GRADIENT, 1.0, &[Some(&velocity), pressure.as_ref(), None, obstacles]);
        drop(velocity);

        // The dye, carried by the new velocity.
        let dye = self.dye_ring.next(gpu, width, height, KIND)?;
        render(&dye, DYE, 1.0, &[self.dye.as_ref(), Some(&projected), dye_in, splat_tex.as_ref()]);

        let velocity_out = if want_velocity {
            let t = self.out_ring.next(gpu, gw, gh, KIND)?;
            render(&t, VELOCITY, 1.0, &[Some(&projected)]);
            Some(t)
        } else {
            None
        };
        batch.submit();

        self.velocity = Some(projected);
        self.pressure = pressure;
        self.dye = Some(dye.clone());
        self.velocity_out = velocity_out.clone();
        if reset {
            // Cleared again next cook, so the output stays black while `reset` is on.
            self.clear();
        }
        let velocity = match velocity_out {
            Some(t) => Data::new(DataKind::Tex, t),
            None => Data::skipped(DataKind::Tex),
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, dye), velocity]))
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn save_state(&self) -> Option<Vec<StateItem>> {
        if !self.persist {
            return None;
        }
        let (v, d) = match &self.restored {
            Some((v, d)) => (v.clone(), d.clone()),
            None => (self.velocity.clone()?, self.dye.clone()?),
        };
        Some(vec![StateItem::Data(Data::new(DataKind::Tex, v)), StateItem::Data(Data::new(DataKind::Tex, d))])
    }

    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        match items.as_slice() {
            [StateItem::Data(v), StateItem::Data(d)] => {
                let (Some(v), Some(d)) = (v.get::<Tex>(), d.get::<Tex>()) else {
                    return Err("a fluid keeps two textures".into());
                };
                self.restored = Some((v.clone(), d.clone()));
                Ok(())
            }
            _ => Err("a fluid keeps two textures, velocity and dye".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_has_square_cells_and_stays_bounded() {
        assert_eq!(grid(1920, 1080, 256), (256, 144));
        assert_eq!(grid(100, 50, 256), (100, 50));
        assert_eq!(grid(16, 16384, 256), (1, 1024));
        assert_eq!(grid(1080, 1920, 1024), (576, 1024));
        assert_eq!(grid(1, 1, -5), (1, 1));
    }

    #[test]
    fn steps_follow_the_show_clock_within_bounds() {
        let s = |x: f64| Tick::from_seconds(x);
        assert!((step(s(1.0), None, 1.0 / 60.0) - 1.0 / 60.0).abs() < 1e-9);
        assert!((step(s(1.0 + 1.0 / 30.0), Some(s(1.0)), 1.0 / 60.0) - 1.0 / 30.0).abs() < 1e-6);
        assert_eq!(step(s(1.0), Some(s(1.0)), 1.0 / 60.0), 0.0);
        assert_eq!(step(s(9.0), Some(s(1.0)), 1.0 / 60.0), MAX_DT);
        assert!((step(s(1.0), Some(s(5.0)), 1.0 / 60.0) - 1.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn fading_halves_in_the_half_life_whatever_the_steps() {
        assert!((fade(1.0, 1.0) - 0.5).abs() < 1e-12);
        let sixty: f64 = (0..60).map(|_| fade(1.0 / 60.0, 1.0)).product();
        assert!((sixty - 0.5).abs() < 1e-9);
        assert_eq!(fade(0.1, 0.0), 0.0);
        assert_eq!(fade(0.1, f64::NAN), 0.0);
        assert_eq!(fade(0.1, f64::INFINITY), 1.0);
    }

    #[test]
    fn splats_come_from_named_columns_and_bad_rows_are_left_out() {
        let t = Table::new(vec![
            vec!["id".into(), "x".into(), "y".into(), "vx".into(), "vy".into(), "r".into()],
            vec!["1".into(), "0.25".into(), "0.5".into(), "0.1".into(), "-0.2".into(), "2".into()],
            vec!["2".into(), "nan".into(), "0.5".into(), "0".into(), "0".into(), "1".into()],
            vec!["3".into(), "0.75".into()],
        ])
        .unwrap();
        let s = splats(&t, 2.0, [0.1, 0.2, 0.3], 0.05).unwrap();
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].at, [0.25, 0.5]);
        assert!((s[0].velocity[0] - 0.2).abs() < 1e-6 && (s[0].velocity[1] + 0.2).abs() < 1e-6);
        assert_eq!(s[0].color, [2.0, 0.2, 0.3]);
        assert_eq!(s[0].radius, 0.05);

        let empty = Table::new(Vec::new()).unwrap();
        assert!(splats(&empty, 1.0, [1.0; 3], 0.05).unwrap().is_empty());
        let header_only = Table::new(vec![vec!["x".into(), "y".into()]]).unwrap();
        assert!(splats(&header_only, 1.0, [1.0; 3], 0.05).unwrap().is_empty());
        let wrong = Table::new(vec![vec!["a".into()], vec!["1".into()]]).unwrap();
        assert!(splats(&wrong, 1.0, [1.0; 3], 0.05).is_err());
        let many: Vec<Vec<String>> = std::iter::once(vec!["x".to_string(), "y".into()])
            .chain((0..200).map(|i| vec![format!("{}", i as f32 / 200.0), "0.5".into()]))
            .collect();
        assert_eq!(splats(&Table::new(many).unwrap(), 1.0, [1.0; 3], 0.05).unwrap().len(), MAX_SPLATS);
    }
}
