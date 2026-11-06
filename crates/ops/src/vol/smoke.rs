//! `vol.smoke`: smoke in three dimensions, Stam's Stable Fluids on a grid of cubic cells in a
//! box (`smoke.wgsl`). Every show frame: forces (buoyancy by temperature along the opposite of
//! `gravity`, damping, the emitter's velocity), the velocity carried along by itself, Jacobi
//! steps on the pressure that make it free of divergence, and smoke and temperature carried by
//! the result, cooled, faded and emitted.
//!
//! The parameter names follow NVIDIA Flow's where they mean the same, so a TouchDesigner setup
//! carries over by name. Time steps follow the show clock as `tex.fluid`'s do. A new grid size
//! starts over. Velocity and fields go into the state snapshots as atlases (made every frame
//! while `persist` is on, since a snapshot cannot run the GPU), so a restart carries on
//! close to where it stopped.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Tick, Unit, Value};
use penumbra_gpu::compute::{atlas_size, check_size, unique_volume_bytes};
use penumbra_gpu::{Batch, Compute, Dispatch, Gpu, Medium, Out, Step, Tex, TexRing, Volume, VolumeRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, StateItem,
};

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

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Smoke in a box in space: an emitter blows hot smoke in, it rises, swirls and fades. tex.raymarch draws it")
        .stateful()
        .time_dependent()
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
            ParamSpec::vec3("emitter_position", [-1.5, 0.0, 0.0])
                .unit(Unit::Metres)
                .group("Emitter")
                .doc("Centre of the emitter, a sphere that blows smoke in"),
        )
        .param(
            ParamSpec::float("emitter_radius", 0.3)
                .clamp(0.0, 1e4)
                .range(0.01, 1.0)
                .curve(Curve::Log)
                .unit(Unit::Metres)
                .group("Emitter")
                .doc("Radius of the emitter; 0: off"),
        )
        .param(
            ParamSpec::vec3("linear_velocity", [0.0, 0.8, 0.0])
                .clamp(-1000.0, 1000.0)
                .group("Emitter")
                .doc("Speed the emitter blows at, in metres per second along x, y and z"),
        )
        .param(
            ParamSpec::float("smoke", 0.5)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .group("Emitter")
                .doc("Density of the smoke the emitter makes"),
        )
        .param(
            ParamSpec::float("temp", 2.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 5.0)
                .group("Emitter")
                .doc("Temperature of the smoke the emitter makes: hotter rises faster (`buoyancy`)"),
        )
        .param(
            ParamSpec::float("correction_rate", 0.5)
                .clamp(0.0, 1.0)
                .group("Emitter")
                .doc(
                    "How much of the way to its own velocity, smoke and temperature the emitter takes the \
                     cells inside it each 60th of a second; 1 sets them",
                ),
        )
        .param(
            ParamSpec::vec3("gravity", [0.0, -1.0, 0.0])
                .clamp(-1000.0, 1000.0)
                .doc("Which way is down; hot smoke rises against it. Only its direction counts, `buoyancy` its strength"),
        )
        .param(
            ParamSpec::float("buoyancy", 4.0)
                .clamp(-1000.0, 1000.0)
                .range(0.0, 10.0)
                .doc("How strongly temperature lifts the smoke, in metres per second squared per degree"),
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
                .doc("Share of its speed the smoke loses per second"),
        )
        .param(
            ParamSpec::float("velocity_fade", 0.1)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Speed the smoke loses per second, in metres per second, whatever its speed"),
        )
        .param(
            ParamSpec::float("smoke_damping", 0.1)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Share of its density the smoke loses per second"),
        )
        .param(
            ParamSpec::float("smoke_fade", 0.4)
                .clamp(0.0, 1000.0)
                .range(0.0, 2.0)
                .doc("Density the smoke loses per second, whatever its density"),
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
        .describe_output("out", "The smoke: density in red, temperature in green, for tex.raymarch")
}

/// The program's params: worked out per frame, none set by the user directly.
fn program_params() -> Vec<ParamSpec> {
    let mut params: Vec<ParamSpec> = [
        "dt",
        "h",
        "emit_radius",
        "emit_smoke",
        "emit_temp",
        "emit_rate",
        "vel_keep",
        "vel_fade",
        "smoke_keep",
        "smoke_fade",
        "temp_keep",
        "warm",
    ]
    .iter()
    .map(|n| ParamSpec::float(n, 0.0))
    .collect();
    for n in ["box", "emit_at", "emit_velocity", "lift"] {
        params.push(ParamSpec::vec3(n, [0.0; 3]));
    }
    params.push(ParamSpec::int("edges", 0));
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

/// The per-second rate `rate` (share lost per second) as a factor for a step of `dt`.
fn keep(rate: f64, dt: f64) -> f64 {
    if !rate.is_finite() || rate <= 0.0 { 1.0 } else { (-rate * dt).exp() }
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
        let rings = [&self.velocity_ring, &self.fields_ring, &self.pressure_ring];
        let volumes = rings.into_iter().flat_map(VolumeRing::iter);
        let kept = [&self.velocity, &self.fields, &self.pressure].into_iter().flatten();
        let atlases = self.atlases.iter().chain(self.restored.iter()).flat_map(|(_, a, b)| [a, b]);
        let atlases = atlases.chain(self.velocity_atlases.iter()).chain(self.fields_atlases.iter());
        unique_volume_bytes(volumes.chain(kept)) + penumbra_gpu::unique_bytes(atlases)
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
        let kept = self.velocity.as_ref().is_some_and(|v| v.size() == cells);
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
            self.velocity = Some(Volume::new(gpu, cells, KIND)?);
            self.fields = Some(Volume::new(gpu, cells, KIND)?);
            self.pressure = None;
            self.atlases = None;
        }

        let lift = {
            let g = vec3(cx, "gravity");
            let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            let b = cx.float("buoyancy")?;
            if len > 1e-9 && len.is_finite() && b.is_finite() { g.map(|x| -x / len * b) } else { [0.0; 3] }
        };
        let emitter = vec3(cx, "emitter_position");
        // Positions in the shader count from the box's lower corner.
        let corner =
            [center[0] - box_size[0] * 0.5, center[1] - box_size[1] * 0.5, center[2] - box_size[2] * 0.5];
        let emit_at = [emitter[0] - corner[0], emitter[1] - corner[1], emitter[2] - corner[2]];
        let rate = cx.float("correction_rate")?.clamp(0.0, 1.0);
        let emit_rate = if rate >= 1.0 { 1.0 } else { 1.0 - (1.0 - rate).powf(dt * 60.0) };
        let previous_pressure = self.pressure.take().filter(|p| p.size() == cells);
        let edges = i64::from(cx.text("edges")? == "walls");
        let values: Vec<(&str, Value)> = vec![
            ("dt", Value::Float(dt)),
            ("h", Value::Float(h)),
            ("box", Value::Vec3(box_size)),
            ("emit_at", Value::Vec3(emit_at)),
            ("emit_radius", Value::Float(cx.float("emitter_radius")?.max(0.0))),
            ("emit_velocity", Value::Vec3(vec3(cx, "linear_velocity"))),
            ("emit_smoke", Value::Float(cx.float("smoke")?)),
            ("emit_temp", Value::Float(cx.float("temp")?)),
            ("emit_rate", Value::Float(emit_rate)),
            ("lift", Value::Vec3(lift)),
            ("vel_keep", Value::Float(keep(cx.float("velocity_damping")?, dt))),
            ("vel_fade", Value::Float(cx.float("velocity_fade")?.max(0.0) * dt)),
            ("smoke_keep", Value::Float(keep(cx.float("smoke_damping")?, dt))),
            ("smoke_fade", Value::Float(cx.float("smoke_fade")?.max(0.0) * dt)),
            ("temp_keep", Value::Float(keep(cx.float("cooling_rate")?, dt))),
            ("edges", Value::Int(edges)),
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

        let param = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone());
        let warm = if previous_pressure.is_some() { WARM } else { 0.0 };
        let param_warm = |name: &str| if name == "warm" { Some(Value::Float(warm)) } else { param(name) };
        let dispatch = |volumes| Dispatch { volumes, pictures: &[], seconds: 0.0, frame: 0, param: &param };
        let inputs_forces = [Some(&velocity), Some(&fields)];
        let inputs_advect = [Some(&forced)];
        let inputs_divergence = [Some(&advected), previous_pressure.as_ref()];
        let [first, second] = &pressures;
        let inputs_jacobi = [[Some(first)], [Some(second)]];
        // An even number of steps ends in the first pressure volume, an odd one in the second.
        let last = if iterations % 2 == 0 { first } else { second };
        let inputs_project = [Some(&advected), Some(last)];
        let inputs_fields = [Some(&fields), Some(&projected)];
        let forces = dispatch(&inputs_forces);
        let advect = dispatch(&inputs_advect);
        let divergence = Dispatch {
            volumes: &inputs_divergence,
            pictures: &[],
            seconds: 0.0,
            frame: 0,
            param: &param_warm,
        };
        let jacobi = [dispatch(&inputs_jacobi[0]), dispatch(&inputs_jacobi[1])];
        let project = dispatch(&inputs_project);
        let carry = dispatch(&inputs_fields);
        let step = |entry, out, input| Step { entry, out: Out::Volume(out), input };
        let mut steps = vec![
            step("forces", &forced, &forces),
            step("advect_velocity", &advected, &advect),
            step("divergence", first, &divergence),
        ];
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
        steps.push(step("advect_fields", &carried, &carry));
        let mut batch = Batch::new(gpu, KIND);
        program.encode_steps(&mut batch, &steps)?;
        batch.submit();
        let pressure = last.clone();
        drop(steps);
        drop((velocity, fields, previous_pressure, advected));

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
    }
}
