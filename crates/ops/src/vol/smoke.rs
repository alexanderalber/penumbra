//! `vol.smoke`: smoke in three dimensions, Stam's Stable Fluids on a grid of cubic cells in a
//! box (`smoke.wgsl`). Every show frame: forces (buoyancy by temperature against `gravity`,
//! vorticity confinement, damping, the emitters' velocities), the velocity carried along by
//! itself, Jacobi steps on the pressure that make it free of divergence except where fuel burns
//! (`expansion`), and smoke, temperature and fuel carried by the result, burnt, cooled, faded and
//! emitted.
//!
//! The emitters come as tables on the `emitter` inputs (see [`super::emitter`]). Their numbers
//! reach the shader exactly, in a [`DataBlock`].
//!
//! The parameter names follow NVIDIA Flow's where they mean the same, so a TouchDesigner setup
//! carries over by name. Time steps follow the show clock as `tex.fluid`'s do. A new grid size
//! starts over. Velocity and fields go into the state snapshots as atlases (made every frame
//! while `persist` is on, since a snapshot cannot run the GPU), so a restart carries on
//! close to where it stopped.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Tick, Unit, Value};
use penumbra_gpu::compute::{atlas_size, check_size, unique_volume_bytes};
use penumbra_gpu::{
    Batch, Compute, DataBlock, Dispatch, Gpu, Medium, Out, Step, Tex, TexRing, Volume, VolumeRing,
};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, StateItem,
};

use super::emitter::{self, Emitter, MAX_EMITTERS};
use crate::on_gpu;
use crate::tex::fluid::step;

pub(crate) const KIND: &str = "vol.smoke";
const SOURCE: &str = include_str!("smoke.wgsl");

/// Most pressure steps per frame.
const MAX_ITERATIONS: i64 = 200;
/// How much of last frame's pressure the first Jacobi step starts from.
const WARM: f64 = 0.8;
/// Most cells along the box's longest side.
const MAX_RESOLUTION: i64 = 512;
/// Divergence per fuel burnt per second at `expansion` 1. NVIDIA Flow does not document its
/// unit; this scale makes Flow's default of 4 swell the gas about threefold, as Flow's scenes
/// look. At 1 the gas grew sixtyfold and filled the box.
const EXPANSION_SCALE: f64 = 0.075;
/// The fastest the smoke moves, in cells per step.
const MAX_CELLS_PER_STEP: f64 = 6.0;

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Smoke in a box in space: emitters blow hot smoke in, it rises, swirls and fades. tex.raymarch draws it")
        .stateful()
        .time_dependent()
        .input_run(
            "emitter",
            DataKind::Table,
            1,
            "Where the smoke comes from: vol.emitter, or any table whose first row names columns x, y \
             and more (see vol.emitter's output). With nothing wired the box stays empty",
        )
        .param(ParamSpec::vec3("center", [0.0, 0.0, 0.0]).unit(Unit::Metres).doc("Centre of the box the smoke lives in"))
        .param(
            ParamSpec::vec3("size", [4.0, 1.5, 1.5])
                .clamp(0.001, 1e4)
                .unit(Unit::Metres)
                .doc("Size of the box along x, y and z. Smoke that leaves it is gone"),
        )
        .param(
            ParamSpec::int("resolution", 128)
                .clamp(4.0, MAX_RESOLUTION as f64)
                .range(32.0, 256.0)
                .curve(Curve::Log)
                .doc(
                    "Cells along the box's longest side; the other sides get as many as cubic cells need. \
                     Doubling it costs about eight times as much. A change starts the smoke over",
                ),
        )
        .param(
            ParamSpec::vec3("gravity", [0.0, -1.0, 0.0])
                .clamp(-1000.0, 1000.0)
                .doc("Which way is down and how strongly: hot smoke rises against it, times `buoyancy`"),
        )
        .param(
            ParamSpec::float("buoyancy", 4.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 10.0)
                .doc("How strongly temperature lifts the smoke against `gravity`, per degree"),
        )
        .param(
            ParamSpec::float("cooling_rate", 1.5)
                .clamp(0.0, 1000.0)
                .range(0.0, 5.0)
                .doc("How fast the smoke cools, per second: at 1, to about a third in a second"),
        )
        .param(
            ParamSpec::float("velocity_damping", 0.1)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Share of its speed the smoke loses per second. Fast smoke slows by this, slow smoke by `velocity_fade`"),
        )
        .param(
            ParamSpec::float("velocity_fade", 0.1)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Speed the smoke loses per second along each axis, in metres per second, where that is more than `velocity_damping` takes"),
        )
        .param(
            ParamSpec::float("smoke_damping", 0.1)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Share of its density the smoke loses per second. Dense smoke thins by this, thin smoke by `smoke_fade`"),
        )
        .param(
            ParamSpec::float("smoke_fade", 0.4)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Density the smoke loses per second where that is more than `smoke_damping` takes: thin smoke vanishes"),
        )
        .param(
            ParamSpec::float("vorticity_strength", 10.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 40.0)
                .group("Vorticity")
                .doc(
                    "How strongly the smoke's eddies are kept spinning: more curls and billows. The grid \
                     smears small eddies out, this puts them back. 0: off, the cheapest",
                ),
        )
        .param(
            ParamSpec::float("vorticity_from_velocity", 1.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Vorticity")
                .doc("How much faster smoke spins more: the strength grows with the speed times this"),
        )
        .param(
            ParamSpec::float("vorticity_from_smoke", 0.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Vorticity")
                .doc("How much denser smoke spins more"),
        )
        .param(
            ParamSpec::float("vorticity_from_temp", 0.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Vorticity")
                .doc("How much hotter smoke spins more"),
        )
        .param(
            ParamSpec::float("vorticity_from_fuel", 0.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Vorticity")
                .doc("How much smoke with more fuel spins more"),
        )
        .param(
            ParamSpec::float("vorticity_constant", 0.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Vorticity")
                .doc("Strength everywhere, whatever the smoke"),
        )
        .param(
            ParamSpec::float("ignition_temp", 0.1)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 2.0)
                .group("Burning")
                .doc("Temperature from which fuel burns. Temperatures run from -1 to 1"),
        )
        .param(
            ParamSpec::float("burn_per_temp", 4.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 10.0)
                .group("Burning")
                .doc("How fast fuel burns per degree above `ignition_temp`, per second, fastest at temperature 1. 0: nothing burns"),
        )
        .param(
            ParamSpec::float("smoke_per_burn", 4.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 10.0)
                .group("Burning")
                .doc("Smoke that burning makes"),
        )
        .param(
            ParamSpec::float("temp_per_burn", 8.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 20.0)
                .group("Burning")
                .doc("Heat that burning makes. The smoke gets no hotter than 1"),
        )
        .param(
            ParamSpec::float("fuel_per_burn", 1.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 4.0)
                .group("Burning")
                .doc("Fuel that burning uses up: less makes the fuel last longer"),
        )
        .param(
            ParamSpec::float("expansion", 4.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 10.0)
                .group("Burning")
                .doc("How much burning gas swells: the flame pushes out and widens"),
        )
        .param(
            ParamSpec::float("speed", 1.0)
                .clamp(0.0, 100.0)
                .range(0.0, 4.0)
                .doc("How fast the simulation runs against the show clock; 0 holds it"),
        )
        .param(
            ParamSpec::int("iterations", 30).clamp(1.0, MAX_ITERATIONS as f64).range(4.0, 100.0).more().doc(
                "Steps that keep the smoke from being squeezed together. Fewer cost less and let it bulge and leak",
            ),
        )
        .param(
            ParamSpec::choice("advection", &["sharp", "smooth"])
                .more()
                .doc("How the smoke is carried from frame to frame")
                .choice_doc("sharp", "keeps the smoke's edges and small detail, costs two more steps a frame")
                .choice_doc("smooth", "smears them a little every frame, the cheapest"),
        )
        .param(
            ParamSpec::choice("edges", &["open", "walls"])
                .more()
                .doc("What the box's sides are to the smoke")
                .choice_doc("open", "the smoke flows out and is gone")
                .choice_doc("walls", "the smoke turns along them"),
        )
        .param(ParamSpec::bool("reset", false).doc("Starts over from still, clear air, every frame while on"))
        .param(
            ParamSpec::bool("persist", true)
                .more()
                .doc("Keep the smoke in the state snapshots, so a restart carries on from it"),
        )
        .output("out", DataKind::Vol)
        .describe_output(
            "out",
            "The smoke: density in red, temperature in green, fuel in blue, fuel burning per second in \
             alpha, for tex.raymarch",
        )
}

/// The program's params: worked out per frame, none set by the user directly.
fn program_params() -> Vec<ParamSpec> {
    let mut params: Vec<ParamSpec> = [
        "dt",
        "h",
        "vel_keep",
        "vel_fade",
        "smoke_keep",
        "smoke_fade",
        "temp_keep",
        "warm",
        "vort",
        "vort_constant",
        "vort_velocity",
        "vort_smoke",
        "vort_temp",
        "vort_fuel",
        "ignition",
        "burn_per_temp",
        "smoke_per_burn",
        "temp_per_burn",
        "fuel_per_burn",
        "expansion",
        "direction",
        "max_speed",
    ]
    .iter()
    .map(|n| ParamSpec::float(n, 0.0))
    .collect();
    for n in ["box", "lift"] {
        params.push(ParamSpec::vec3(n, [0.0; 3]));
    }
    params.push(ParamSpec::int("edges", 0));
    params.push(ParamSpec::int("emitters", 0));
    params.push(ParamSpec::int("inflow", 0));
    params.push(ParamSpec::int("sharp", 0));
    params
}

fn program(gpu: &Gpu) -> Result<Arc<Compute>, Error> {
    gpu.cached_compute(KIND, SOURCE, &program_params())
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Smoke::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(|gpu| program(gpu).map(|_| ()))),
    )
}

/// Cells of a box of `size` metres with `resolution` cells along its longest side: cubic cells,
/// at least one along each side, the total within what a volume may hold. Also the cell's side.
pub(crate) fn grid(size: [f64; 3], resolution: i64) -> ([u32; 3], f64) {
    let size = size.map(|s| if s.is_finite() && s > 0.0 { s } else { 1.0 });
    let longest = size.iter().copied().fold(0.0, f64::max);
    let mut n = resolution.clamp(1, MAX_RESOLUTION) as f64;
    loop {
        let h = longest / n;
        let cells = size.map(|s| (s / h).round().max(1.0) as u32);
        let total: u64 = cells.iter().map(|c| u64::from(*c)).product();
        if total <= penumbra_gpu::compute::MAX_VOLUME_CELLS || n <= 1.0 {
            return (cells, h);
        }
        n = (n * 0.9).floor();
    }
}

/// The per-second rate `rate` (exponential decay) as a factor for a step of `dt`.
fn keep(rate: f64, dt: f64) -> f64 {
    if !rate.is_finite() || rate <= 0.0 { 1.0 } else { (-rate * dt).exp() }
}

/// What a step of `dt` keeps when `share` of the value goes per second: 1 or more loses all.
fn keep_share(share: f64, dt: f64) -> f64 {
    if !share.is_finite() || share <= 0.0 {
        1.0
    } else if share >= 1.0 {
        0.0
    } else {
        (1.0 - share).powf(dt)
    }
}

/// The vectors of `smoke.wgsl`'s emitters, at most [`MAX_EMITTERS`], for a box with its lower
/// corner at `corner` and a step of `dt`.
fn emitter_vectors(emitters: &[Emitter], corner: [f64; 3], dt: f64) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity(emitters.len().min(MAX_EMITTERS) * 4);
    for e in emitters.iter().take(MAX_EMITTERS) {
        let at = [e.position[0] - corner[0], e.position[1] - corner[1], e.position[2] - corner[2]];
        let rate = (e.correction_rate * dt).min(1.0);
        let half = e.size.map(|s| s * 0.5);
        out.push([at[0] as f32, at[1] as f32, at[2] as f32, if e.is_box { 1.0 } else { 0.0 }]);
        out.push([half[0] as f32, half[1] as f32, half[2] as f32, e.radius as f32]);
        out.push([e.velocity[0] as f32, e.velocity[1] as f32, e.velocity[2] as f32, rate as f32]);
        out.push([e.smoke as f32, e.temp as f32, e.fuel as f32, 0.0]);
    }
    out
}

/// A [`Value::Vec3`] param as numbers; zeros when it is something else.
fn vec3(cx: &CookContext<'_>, name: &str) -> [f64; 3] {
    match cx.param(name) {
        Some(Value::Vec3(v)) => *v,
        _ => [0.0; 3],
    }
}

#[derive(Default)]
struct Smoke {
    velocity_ring: VolumeRing,
    fields_ring: VolumeRing,
    pressure_ring: VolumeRing,
    curl_ring: VolumeRing,
    /// Steps forward and back for sharp advection.
    scratch_ring: VolumeRing,
    velocity: Option<Volume>,
    fields: Option<Volume>,
    pressure: Option<Volume>,
    /// Velocity and fields from a state snapshot, as atlases of a grid, taken over next cook.
    restored: Option<([u32; 3], Tex, Tex)>,
    /// Atlases of the last state for the snapshots.
    atlases: Option<([u32; 3], Tex, Tex)>,
    velocity_atlases: TexRing,
    fields_atlases: TexRing,
    last: Option<Tick>,
    persist: bool,
    /// The emitters for the shader, four vectors each.
    emitters: Option<DataBlock>,
    /// What `emitters` holds, to write only what changed.
    written: Vec<[f32; 4]>,
}

impl Smoke {
    fn clear(&mut self) {
        self.velocity = None;
        self.fields = None;
        self.pressure = None;
        self.restored = None;
        self.atlases = None;
    }
}

impl Operator for Smoke {
    fn gpu_bytes(&self) -> u64 {
        let rings = [
            &self.velocity_ring,
            &self.fields_ring,
            &self.pressure_ring,
            &self.curl_ring,
            &self.scratch_ring,
        ];
        let volumes = rings.into_iter().flat_map(VolumeRing::iter);
        let kept = [&self.velocity, &self.fields, &self.pressure].into_iter().flatten();
        let atlases = self.atlases.iter().chain(self.restored.iter()).flat_map(|(_, a, b)| [a, b]);
        let atlases = atlases.chain(self.velocity_atlases.iter()).chain(self.fields_atlases.iter());
        unique_volume_bytes(volumes.chain(kept))
            + penumbra_gpu::unique_bytes(atlases)
            + self.emitters.as_ref().map_or(0, DataBlock::bytes)
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let program = program(gpu)?;
        self.persist = cx.bool("persist")?;
        let now = cx.time().tick;
        let period = 1.0 / cx.rate().as_f64().max(1.0);
        let reset = cx.bool("reset")?;
        if reset {
            self.clear();
        }
        let box_size = vec3(cx, "size");
        let center = vec3(cx, "center");
        let (cells, h) = grid(box_size, cx.int("resolution")?);
        check_size(gpu, cells)?;
        let dt = step(now, self.last, period) * cx.float("speed")?.max(0.0);
        // Before the early return, so a bad table warns on a still show too.
        let sources = emitter::from_inputs(cx);
        let kept = self.velocity.as_ref().is_some_and(|v| v.size() == cells);
        // A table that changed does not count here: with no time passing, emitters do nothing.
        if dt == 0.0 && kept && self.restored.is_none() && !reset && !cx.params_changed() && !cx.first_cook()
        {
            return Ok(Cooked::Unchanged);
        }
        self.last = Some(now);

        if let Some((size, v, f)) = self.restored.take() {
            if size == cells {
                self.velocity = Some(gpu.volume_from_atlas(&v, size)?);
                self.fields = Some(gpu.volume_from_atlas(&f, size)?);
                self.pressure = None;
            } else {
                cx.warn("the kept smoke is of another grid; it starts over");
            }
        }
        if !self.velocity.as_ref().is_some_and(|v| v.size() == cells)
            || !self.fields.as_ref().is_some_and(|f| f.size() == cells)
        {
            // A new grid (or none yet) starts from still, clear air: fresh volumes are zero.
            self.velocity_ring.clear();
            self.fields_ring.clear();
            self.pressure_ring.clear();
            self.curl_ring.clear();
            self.scratch_ring.clear();
            self.velocity = Some(Volume::new(gpu, cells, KIND)?);
            self.fields = Some(Volume::new(gpu, cells, KIND)?);
            self.pressure = None;
            self.atlases = None;
        }

        let lift = {
            let g = vec3(cx, "gravity");
            let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            let b = cx.float("buoyancy")?;
            if len.is_finite() && b.is_finite() { g.map(|x| -x * b) } else { [0.0; 3] }
        };
        // Positions in the shader count from the box's lower corner.
        let corner =
            [center[0] - box_size[0] * 0.5, center[1] - box_size[1] * 0.5, center[2] - box_size[2] * 0.5];
        let emit_data = emitter_vectors(&sources, corner, dt);
        let block = match &self.emitters {
            Some(b) => b.clone(),
            None => {
                let b = DataBlock::new(gpu, MAX_EMITTERS * 4, KIND)?;
                self.emitters = Some(b.clone());
                self.written.clear();
                b
            }
        };
        if emit_data != self.written {
            block.write(gpu, &emit_data)?;
            self.written = emit_data;
        }
        let previous_pressure = self.pressure.take().filter(|p| p.size() == cells);
        let edges = i64::from(cx.text("edges")? == "walls");
        let vort = cx.float("vorticity_strength")?.max(0.0);
        let sharp = cx.text("advection")? == "sharp";
        let values: Vec<(&str, Value)> = vec![
            ("dt", Value::Float(dt)),
            ("h", Value::Float(h)),
            ("box", Value::Vec3(box_size)),
            ("emitters", Value::Int(sources.len().min(MAX_EMITTERS) as i64)),
            ("lift", Value::Vec3(lift)),
            ("vel_keep", Value::Float(keep_share(cx.float("velocity_damping")?, dt))),
            ("vel_fade", Value::Float(cx.float("velocity_fade")?.max(0.0) * dt)),
            ("smoke_keep", Value::Float(keep_share(cx.float("smoke_damping")?, dt))),
            ("smoke_fade", Value::Float(cx.float("smoke_fade")?.max(0.0) * dt)),
            ("temp_keep", Value::Float(keep(cx.float("cooling_rate")?, dt))),
            ("edges", Value::Int(edges)),
            ("vort", Value::Float(vort)),
            ("vort_constant", Value::Float(cx.float("vorticity_constant")?)),
            ("vort_velocity", Value::Float(cx.float("vorticity_from_velocity")?)),
            ("vort_smoke", Value::Float(cx.float("vorticity_from_smoke")?)),
            ("vort_temp", Value::Float(cx.float("vorticity_from_temp")?)),
            ("vort_fuel", Value::Float(cx.float("vorticity_from_fuel")?)),
            ("ignition", Value::Float(cx.float("ignition_temp")?)),
            ("burn_per_temp", Value::Float(cx.float("burn_per_temp")?.max(0.0))),
            ("smoke_per_burn", Value::Float(cx.float("smoke_per_burn")?.max(0.0))),
            ("temp_per_burn", Value::Float(cx.float("temp_per_burn")?)),
            ("fuel_per_burn", Value::Float(cx.float("fuel_per_burn")?.max(0.0))),
            ("expansion", Value::Float(cx.float("expansion")? * EXPANSION_SCALE)),
            ("direction", Value::Float(1.0)),
            // Six cells a step; at a still clock any speed passes (nothing moves anyway).
            ("max_speed", Value::Float(if dt > 0.0 { MAX_CELLS_PER_STEP * h / dt } else { 1.0e30 })),
            ("sharp", Value::Int(i64::from(sharp))),
        ];
        let iterations = cx.int("iterations")?.clamp(1, MAX_ITERATIONS);

        let (Some(velocity), Some(fields)) = (self.velocity.take(), self.fields.take()) else {
            return Err(CookError::failed("the smoke has no state"));
        };
        // Every target first, then the steps as one recording. A volume read by one step and
        // written by a later one is fine: the steps run in order. `forced` takes the projected
        // velocity, since nothing reads it after `advect_velocity`.
        let forced = self.velocity_ring.next(gpu, cells, KIND)?;
        let advected = self.velocity_ring.next(gpu, cells, KIND)?;
        let pressures =
            [self.pressure_ring.next(gpu, cells, KIND)?, self.pressure_ring.next(gpu, cells, KIND)?];
        let carried = self.fields_ring.next(gpu, cells, KIND)?;
        let projected = forced.clone();
        // The curl only when something reads it.
        let curl = if vort > 0.0 { Some(self.curl_ring.next(gpu, cells, KIND)?) } else { None };

        // Steps forward and back for sharp advection of the fields.
        let scratch = if sharp {
            Some([self.scratch_ring.next(gpu, cells, KIND)?, self.scratch_ring.next(gpu, cells, KIND)?])
        } else {
            None
        };

        let warm = if previous_pressure.is_some() { WARM } else { 0.0 };
        // Each dispatch's own values over the frame's: `over` names what differs.
        let with = |over: Vec<(&'static str, Value)>| {
            let values = &values;
            move |name: &str| {
                over.iter().chain(values.iter()).find(|(n, _)| *n == name).map(|(_, v)| v.clone())
            }
        };
        let plain = with(vec![]);
        let warmed = with(vec![("warm", Value::Float(warm))]);
        let fields_ahead = with(vec![("inflow", Value::Int(1))]);
        let fields_back = with(vec![("inflow", Value::Int(1)), ("direction", Value::Float(-1.0))]);
        let fields_final = with(vec![("inflow", Value::Int(1))]);
        fn dispatch<'a>(
            volumes: &'a [Option<&'a Volume>],
            data: &'a DataBlock,
            param: &'a dyn Fn(&str) -> Option<Value>,
        ) -> Dispatch<'a> {
            Dispatch { volumes, pictures: &[], data: Some(data), seconds: 0.0, frame: 0, param }
        }
        let [first, second] = &pressures;
        // An even number of steps ends in the first pressure volume, an odd one in the second.
        let last = if iterations % 2 == 0 { first } else { second };
        let (ahead, behind) = match &scratch {
            Some([a, b]) => (Some(a), Some(b)),
            None => (None, None),
        };
        let inputs_curl = [Some(&velocity)];
        let inputs_forces = [Some(&velocity), Some(&fields), curl.as_ref()];
        let inputs_ahead = [Some(&forced), Some(&forced)];
        let inputs_divergence = [Some(&advected), previous_pressure.as_ref(), Some(&fields)];
        let inputs_jacobi = [[Some(first)], [Some(second)]];
        let inputs_project = [Some(&advected), Some(last)];
        let inputs_fields_ahead = [Some(&fields), Some(&projected)];
        let inputs_fields_back = [ahead, Some(&projected)];
        let inputs_fields = [Some(&fields), Some(&projected), ahead, behind];
        let curling = dispatch(&inputs_curl, &block, &plain);
        let forces = dispatch(&inputs_forces, &block, &plain);
        let carry_ahead = dispatch(&inputs_ahead, &block, &plain);
        let divergence = dispatch(&inputs_divergence, &block, &warmed);
        let jacobi =
            [dispatch(&inputs_jacobi[0], &block, &plain), dispatch(&inputs_jacobi[1], &block, &plain)];
        let project = dispatch(&inputs_project, &block, &plain);
        let fields_carry_ahead = dispatch(&inputs_fields_ahead, &block, &fields_ahead);
        let fields_carry_back = dispatch(&inputs_fields_back, &block, &fields_back);
        let carry = dispatch(&inputs_fields, &block, &fields_final);
        let step = |entry, out, input| Step { entry, out: Out::Volume(out), input };
        let mut steps = Vec::with_capacity(iterations as usize + 10);
        if let Some(c) = &curl {
            steps.push(step("curl", c, &curling));
        }
        steps.push(step("forces", &forced, &forces));
        // The velocity is carried plainly: sharpened, vorticity confinement spun up eddies one
        // cell wide until the flow blew up.
        steps.push(step("carry", &advected, &carry_ahead));
        steps.push(step("divergence", first, &divergence));
        let [from_first, from_second] = &jacobi;
        for k in 0..iterations {
            // Even steps read the first volume and write the second, odd ones the other way.
            if k % 2 == 0 {
                steps.push(step("jacobi", second, from_first));
            } else {
                steps.push(step("jacobi", first, from_second));
            }
        }
        steps.push(step("project", &projected, &project));
        if let (Some(a), Some(b)) = (ahead, behind) {
            steps.push(step("carry", a, &fields_carry_ahead));
            steps.push(step("carry", b, &fields_carry_back));
        }
        steps.push(step("advect_fields", &carried, &carry));
        let mut batch = Batch::new(gpu, KIND);
        program.encode_steps(&mut batch, &steps)?;
        batch.submit();
        let pressure = last.clone();
        drop(steps);
        drop((velocity, fields, previous_pressure, advected, curl, scratch));

        if self.persist {
            let (w, h) = atlas_size(cells);
            let v = self.velocity_atlases.next_storage(gpu, w, h, KIND)?;
            let f = self.fields_atlases.next_storage(gpu, w, h, KIND)?;
            gpu.pack_atlas(&projected, &v)?;
            gpu.pack_atlas(&carried, &f)?;
            self.atlases = Some((cells, v, f));
        } else {
            self.atlases = None;
        }

        self.velocity = Some(projected);
        self.fields = Some(carried.clone());
        self.pressure = Some(pressure);
        let medium =
            Medium { volume: carried, center: center.map(|c| c as f32), size: box_size.map(|s| s as f32) };
        if reset {
            self.clear();
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Vol, medium)]))
    }

    fn save_state(&self) -> Option<Vec<StateItem>> {
        if !self.persist {
            return None;
        }
        let (size, v, f) = self.restored.as_ref().or(self.atlases.as_ref())?;
        let header = format!("{} {} {}", size[0], size[1], size[2]).into_bytes();
        Some(vec![
            StateItem::Bytes(header),
            StateItem::Data(Data::new(DataKind::Tex, v.clone())),
            StateItem::Data(Data::new(DataKind::Tex, f.clone())),
        ])
    }

    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        let [StateItem::Bytes(header), StateItem::Data(v), StateItem::Data(f)] = items.as_slice() else {
            return Err("smoke keeps its grid size and two atlases".into());
        };
        let text = std::str::from_utf8(header).map_err(|_| "the smoke's grid size is not text")?;
        let parts: Vec<u32> = text.split_whitespace().filter_map(|p| p.parse().ok()).collect();
        let [x, y, z] = parts.as_slice() else { return Err(format!("`{text}` is no grid size")) };
        let size = [*x, *y, *z];
        let (Some(v), Some(f)) = (v.get::<Tex>(), f.get::<Tex>()) else {
            return Err("smoke keeps two pictures".into());
        };
        if v.size() != atlas_size(size) || f.size() != atlas_size(size) {
            return Err("the kept smoke's pictures do not fit its grid".into());
        }
        self.restored = Some((size, v.clone(), f.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_has_cubic_cells_and_stays_within_bounds() {
        let (cells, h) = grid([4.0, 1.5, 1.5], 128);
        assert_eq!(cells, [128, 48, 48]);
        assert!((h - 4.0 / 128.0).abs() < 1e-12);
        let (cells, _) = grid([1.0, 1.0, 1.0], 512);
        assert!(
            cells.iter().map(|c| u64::from(*c)).product::<u64>() <= penumbra_gpu::compute::MAX_VOLUME_CELLS
        );
        assert_eq!(grid([f64::NAN, 0.0, -1.0], 8).0, [8, 8, 8]);
        assert_eq!(grid([10.0, 0.001, 0.001], 16).0, [16, 1, 1]);
    }

    #[test]
    fn keeping_follows_the_rate_whatever_the_steps() {
        let sixty: f64 = (0..60).map(|_| keep(1.0, 1.0 / 60.0)).product();
        assert!((sixty - (-1.0f64).exp()).abs() < 1e-9);
        assert_eq!(keep(0.0, 0.1), 1.0);
        assert_eq!(keep(f64::NAN, 0.1), 1.0);
        let second: f64 = (0..60).map(|_| keep_share(0.1, 1.0 / 60.0)).product();
        assert!((second - 0.9).abs() < 1e-9);
        assert_eq!(keep_share(1.0, 0.1), 0.0);
        assert_eq!(keep_share(f64::NAN, 0.1), 1.0);
    }
}
