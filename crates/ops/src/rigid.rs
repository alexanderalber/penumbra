//! `physics.rigid_bodies`: rigid bodies that fall, collide and come to rest. Each `bodies` input
//! takes one group ([`group::Bodies`], made by `physics.bodies`): a table of starts, a model, a
//! collision shape and a material. All groups live in one world and collide with each other.
//! The node hands out the bodies as geometry, each group drawn as its own model, and as a table
//! of where they are now, in the columns `geo.instance` reads. The solver runs on the CPU
//! (`penumbra-physics`), one step per show frame, on the cook thread.
//!
//! A dynamic group starts over when its starts or its collision shape change; the other groups
//! go on. A driven or fixed group follows its table instead: the solver places its bodies anew
//! whenever the table changes. Materials, obstacles and the params change while the bodies move.
//!
//! An obstacle that is moved or turned pushes the bodies it meets: the solver takes it from its
//! old place to its new one within the frame. One whose size or mesh changes is built anew
//! where it is now, and pushes nothing.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Table, Tick, Unit, Value};
use penumbra_gpu::geo::{Item, Mat4, Model, mul};
use penumbra_gpu::render::MAX_DRAWS;
use penumbra_gpu::{Geo, Gpu};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Mesh;
use penumbra_physics::{
    MAX_BODIES, MAX_GROUPS, MAX_SUBSTEPS, Mode, Obstacle, Place, Placement, Settings, World,
};

use crate::geo::trs;
use crate::tex::fluid::step;

pub mod group;

use group::Bodies;

pub const KIND: &str = "physics.rigid_bodies";

/// The columns of the `table` output: the ones `geo.instance` reads, then the number of the
/// `bodies` input the body came in by.
pub const OUT_COLUMNS: [&str; 10] = ["tx", "ty", "tz", "rx", "ry", "rz", "sx", "sy", "sz", "group"];

/// The `obstacles` input, after the run of `bodies` inputs.
const OBSTACLES: usize = MAX_RUN;

/// The outputs, by index.
const GEO_OUT: usize = 0;
const TABLE_OUT: usize = 1;

// Every `bodies` input is a group of its own in the world.
const _: () = assert!(MAX_RUN <= MAX_GROUPS);

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    group::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Rigid bodies that fall, collide and come to rest: every group of bodies on its inputs lives in one world, and the output shows where they are now",
        )
        .param(
            ParamSpec::vec3("gravity", [0.0, 0.0, -9.81])
                .range(-20.0, 20.0)
                .doc("The pull on every body, in metres per second squared; z is up, so -9.81 in z is the earth's"),
        )
        .param(ParamSpec::bool("floor", true).doc("A level floor without end that the bodies land on"))
        .param(
            ParamSpec::float("floor_height", 0.0)
                .clamp(-1e4, 1e4)
                .range(-5.0, 5.0)
                .unit(Unit::Metres)
                .doc("How high the floor lies, along z"),
        )
        .param(
            ParamSpec::float("friction", 0.5)
                .clamp(0.0, 10.0)
                .range(0.0, 2.0)
                .doc("How much the floor and the obstacles grip: 0 slides like ice, 1 holds like rubber. A body on them slides by the mean of this and its group's `friction`"),
        )
        .param(
            ParamSpec::float("bounce", 0.0)
                .clamp(0.0, 1.0)
                .doc("How much of its speed a body keeps when it hits the floor or an obstacle: 0 lands dead, 1 bounces for ever. The larger of this and the group's `bounce` counts"),
        )
        .param(
            ParamSpec::float("damping", 0.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 5.0)
                .more()
                .doc("Slows every body down as thick air would: the share of its speed it loses per second"),
        )
        .param(
            ParamSpec::float("spin_damping", 0.0)
                .clamp(0.0, 1000.0)
                .range(0.0, 5.0)
                .more()
                .doc("Slows every body's turning down: the share of its spin it loses per second"),
        )
        .param(
            ParamSpec::float("speed", 1.0)
                .clamp(0.0, 16.0)
                .range(0.0, 4.0)
                .doc("How fast time runs for the bodies: 0 holds them still, 0.25 is slow motion"),
        )
        .param(
            ParamSpec::int("substeps", 1)
                .clamp(1.0, f64::from(MAX_SUBSTEPS))
                .more()
                .doc("Solver steps per frame: raise it when fast bodies pass through thin things or a tall stack wobbles. Each step costs the same again"),
        )
        .param(
            ParamSpec::float("jump", 0.5)
                .clamp(0.0, 1e6)
                .range(0.0, 5.0)
                .unit(Unit::Metres)
                .more()
                .doc("How far a driven body or an obstacle may move in one frame and still push what is in its way. Moved farther, it is put at its new place at once and pushes nothing. This keeps a tracker that loses a blob and finds it elsewhere from sweeping the scene. 0: driven bodies and obstacles never push"),
        )
        .param(ParamSpec::bool("reset", false).doc("Puts every body of every group back to its start, every frame while on"))
        .input_run(
            "bodies",
            DataKind::Bodies,
            1,
            "A group of bodies from physics.bodies. The groups of all these inputs collide with each other, 4000 bodies in all at most. A dynamic group whose table or shape changes starts over; the others go on. A driven or fixed group follows its table",
        )
        .input("obstacles", DataKind::Geo)
        .describe_input(
            "obstacles",
            "Geometry that the bodies land on and bounce off, hollows and all. An obstacle that is moved or turned, by a geo.transform with a bound param for one, carries along the bodies on it and pushes those in its way. One that changes its size or its mesh jumps and pushes nothing",
        )
        .output("out", DataKind::Geo)
        .describe_output("out", "Every body where it is now, drawn as the model of its group. A group without a model shows boxes of 1 m")
        .lazy_output("out")
        .output("table", DataKind::Table)
        .describe_output("table", "One row per body, group after group in the order of the inputs: tx ty tz rx ry rz sx sy sz, the columns geo.instance reads, and `group`, the number of the `bodies` input (0 for `bodies0`)")
        .lazy_output("table")
        .stateful()
        .time_dependent(),
        || Box::new(RigidBodies::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("physics.null", "Passes its group of bodies on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Bodies)
            .output("out", DataKind::Bodies),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// What the solver knows about the group on one `bodies` input.
struct Slot {
    /// The group as it was last taken over.
    data: Data,
    /// Why the group, or the last of its bodies, is not in the world.
    note: Option<String>,
}

#[derive(Default)]
struct RigidBodies {
    world: Option<World>,
    last: Option<Tick>,
    /// By the number of the `bodies` input.
    slots: Vec<Option<Slot>>,
    /// What the obstacles were built from.
    obstacles: Option<Vec<Form>>,
    /// Why the obstacles are not in the world.
    obstacle_note: Option<String>,
    /// What making the `out` geometry found, said again on every cook while it holds.
    draw_note: Option<String>,
    /// The box of 1 m a group without a model is drawn as.
    unit: Option<Arc<Model>>,
}

impl Operator for RigidBodies {
    fn gpu_bytes(&self) -> u64 {
        self.unit.as_ref().map_or(0, |m| m.bytes())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let mut groups: Vec<(usize, &Data, &Bodies)> = Vec::new();
        // Wired inputs that deliver nothing: the first that may still come, and those that
        // will not until something changes upstream.
        let (mut awaited, mut failed) = (None, Vec::new());
        for i in 0..MAX_RUN {
            match cx.input(i) {
                Some(data) => match data.get::<Bodies>() {
                    Some(bodies) => groups.push((i, data, bodies)),
                    None => awaited = awaited.or(Some(i)),
                },
                None if wired(i) && cx.input_stuck(i) => failed.push(i),
                None if wired(i) => awaited = awaited.or(Some(i)),
                None => {}
            }
        }
        if let Some(i) = awaited {
            cx.warn(format!("waiting for the group on `bodies{i}`"));
            return Err(CookError::Pending);
        }
        let names = failed.iter().map(|i| format!("`bodies{i}`")).collect::<Vec<_>>().join(", ");
        if groups.is_empty() {
            return Err(CookError::failed(match failed.len() {
                0 => "connect a physics.bodies: a group of bodies to move".to_string(),
                _ => format!("the node on {names} failed, so there is no group of bodies to move"),
            }));
        }
        let mut input_note = (!failed.is_empty())
            .then(|| format!("the node on {names} failed: the solver runs without its bodies"));
        let obstacle_geo = match cx.input_as::<Geo>(OBSTACLES) {
            Some(g) => Some(g),
            None if wired(OBSTACLES) && cx.input_stuck(OBSTACLES) => {
                let note = "the node on `obstacles` failed: the solver runs without obstacles";
                input_note = Some(input_note.map_or(note.to_string(), |n| format!("{n}; {note}")));
                None
            }
            None if wired(OBSTACLES) => {
                cx.warn("waiting for the geometry on `obstacles`");
                return Err(CookError::Pending);
            }
            None => None,
        };
        let jump = cx.float("jump")? as f32;
        let settings = settings(cx)?;
        let now = cx.time().tick;
        let period = 1.0 / cx.rate().as_f64().max(1.0);

        let built = self.world.is_none() || cx.bool("reset")?;
        if built {
            self.world = None;
            self.slots.clear();
            self.obstacles = None;
            self.last = None;
        }
        self.slots.resize_with(MAX_RUN, || None);
        let Self { world, slots, .. } = self;
        let world = world.get_or_insert_with(|| World::new(settings));
        world.apply(settings);
        let mut changed = built;

        // Groups whose wire left, materials that changed, and the groups to start over.
        for (i, slot) in slots.iter_mut().enumerate() {
            if slot.is_some() && !groups.iter().any(|(g, _, _)| *g == i) {
                world.remove_group(i);
                *slot = None;
                changed = true;
            }
        }
        let mut restart = Vec::new();
        for &(i, data, bodies) in &groups {
            match slots.get_mut(i).and_then(Option::as_mut) {
                Some(slot) if slot.data.same(data) => {}
                Some(slot) if slot.data.get::<Bodies>().is_some_and(|old| goes_on(old, bodies)) => {
                    world.set_material(i, bodies.material);
                    if bodies.mode != Mode::Dynamic {
                        // Before the step, so that a driven body travels to its row in it.
                        slot.note = world.place(i, &bodies.starts, jump).err().map(|e| left_out(i, &e));
                    }
                    slot.data = data.clone();
                    changed = true;
                }
                _ => restart.push((i, data, bodies)),
            }
        }
        let (forms, places, instanced_note) = obstacle_parts(obstacle_geo);
        if self.obstacles.as_ref().is_some_and(|old| same_forms(old, &forms)) {
            // Only moved, if at all: before the step, so that they travel in it and push. An
            // error means the obstacles failed when they were built, and the note says so.
            let _ = world.move_obstacles(&places, jump);
        } else {
            let obstacles = obstacles_of(obstacle_geo, &forms, &places);
            // Kept even when they fail, so a mesh that cannot be collided with is not tried
            // again on every frame.
            self.obstacles = Some(forms);
            self.obstacle_note = world
                .set_obstacles(obstacles)
                .err()
                .map(|e| format!("{e}: the bodies fall through the obstacles"));
            changed = true;
        }

        // A world just built shows its start; time moves it from the next frame on.
        let dt = if built { 0.0 } else { step(now, self.last, period) * cx.float("speed")?.max(0.0) };
        self.last = Some(now);
        world.step(dt as f32, u32::try_from(cx.int("substeps")?).unwrap_or(1));

        // A group starts after the step, so the frame it starts in shows it where its table
        // places it, while the other groups have moved on.
        for (i, data, bodies) in restart {
            let note = start_group(world, i, bodies);
            if let Some(slot) = slots.get_mut(i) {
                *slot = Some(Slot { data: data.clone(), note });
            }
            changed = true;
        }

        let still =
            dt == 0.0 && !changed && !cx.params_changed() && !cx.first_cook() && !cx.outputs_wanted_changed();
        let outputs = if still {
            None
        } else {
            let placed: Vec<(usize, &Bodies, Vec<Placement>)> =
                groups.iter().map(|&(i, _, bodies)| (i, bodies, world.placements(i))).collect();
            let geo = if cx.output_wanted(GEO_OUT) {
                if self.unit.is_none() && placed.iter().any(|(_, bodies, _)| bodies.model.is_empty()) {
                    self.unit = unit_model(cx.resource::<Gpu>());
                }
                let (geo, note) = geo_of(&placed, self.unit.as_ref());
                self.draw_note = note;
                Data::new(DataKind::Geo, geo)
            } else {
                self.draw_note = None;
                Data::skipped(DataKind::Geo)
            };
            let table = if cx.output_wanted(TABLE_OUT) {
                let table = table_of(&placed).map_err(|e| CookError::failed(e.to_string()))?;
                Data::new(DataKind::Table, table)
            } else {
                Data::skipped(DataKind::Table)
            };
            Some(vec![geo, table])
        };
        let notes: Vec<&str> = slots
            .iter()
            .flatten()
            .filter_map(|s| s.note.as_ref())
            .chain(&self.obstacle_note)
            .chain(&instanced_note)
            .chain(&self.draw_note)
            .chain(&input_note)
            .map(String::as_str)
            .collect();
        if !notes.is_empty() {
            cx.warn(notes.join("; "));
        }
        Ok(outputs.map_or(Cooked::Unchanged, Cooked::Outputs))
    }
}

/// Whether the bodies of `new` may go on from where those of `old` are: the same collision
/// shape, moved by the same hand, and for bodies the solver moves the same starts. The material
/// and the look may differ, and so may the rows of bodies that follow their table.
fn goes_on(old: &Bodies, new: &Bodies) -> bool {
    old.shape == new.shape && old.mode == new.mode && (new.mode != Mode::Dynamic || old.starts == new.starts)
}

/// The note for bodies of the group on input `index` that the world did not take.
fn left_out(index: usize, why: &str) -> String {
    format!("`bodies{index}`: {why} in all: the last bodies of this group are left out")
}

/// Makes group `index` of `world` anew from `bodies`, every body at its start. Returns what
/// kept bodies out, if anything did.
fn start_group(world: &mut World, index: usize, bodies: &Bodies) -> Option<String> {
    if let Err(e) = world.set_group(index, bodies.shape.clone(), bodies.material, bodies.mode) {
        world.remove_group(index);
        return Some(format!("`bodies{index}`: {e}: its bodies are left out"));
    }
    for (k, start) in bodies.starts.iter().enumerate() {
        if let Err(e) = world.add(index, start) {
            let rest = bodies.starts.len() - k;
            return Some(format!("`bodies{index}`: {e} in all: the last {rest} of this group are left out"));
        }
    }
    None
}

fn settings(cx: &CookContext<'_>) -> Result<Settings, CookError> {
    let gravity = match cx.param("gravity") {
        Some(Value::Vec3(v)) => v.map(|g| g as f32),
        _ => Settings::default().gravity,
    };
    Ok(Settings {
        gravity,
        floor: cx.bool("floor")?.then_some(cx.float("floor_height")? as f32),
        friction: cx.float("friction")? as f32,
        bounce: cx.float("bounce")? as f32,
        damping: cx.float("damping")? as f32,
        spin_damping: cx.float("spin_damping")? as f32,
    })
}

/// A box of 1 m on the GPU, or none without a GPU or when the upload fails.
fn unit_model(gpu: Option<&Gpu>) -> Option<Arc<Model>> {
    let mesh = penumbra_mesh::prim::cuboid([1.0; 3]).ok()?;
    Model::upload(gpu?, mesh, "physics body").ok().map(|(model, _)| Arc::new(model))
}

/// Every body as geometry: the model of its group, placed where the body is. A group without a
/// model is drawn as `unit`. Also a note when the renderer's limit of draws cut the bodies short.
fn geo_of(groups: &[(usize, &Bodies, Vec<Placement>)], unit: Option<&Arc<Model>>) -> (Geo, Option<String>) {
    let mut items: Vec<Item> = Vec::new();
    let mut offset = 0u32;
    let mut left_out = 0usize;
    for (_, bodies, placements) in groups {
        let stand_in: Vec<Item> = unit.map(|u| Geo::single(u.clone()).items).unwrap_or_default();
        let model = if bodies.model.is_empty() { &stand_in } else { &bodies.model };
        if model.is_empty() {
            continue;
        }
        // One past the highest part id of the model, so every body keeps part ids of its own.
        let parts = Geo { items: model.clone(), preview: None }.part_count();
        for p in placements {
            if items.len() + model.len() > MAX_DRAWS {
                left_out += 1;
                continue;
            }
            let m = trs(p.translate, p.rotate_degrees, p.scale.map(f64::from));
            items.extend(model.iter().map(|i| Item {
                world: mul(&m, &i.world),
                part_offset: i.part_offset.saturating_add(offset),
                ..i.clone()
            }));
            offset = offset.saturating_add(parts);
        }
    }
    let note = (left_out > 0)
        .then(|| format!("the renderer draws {MAX_DRAWS} pieces at most: {left_out} bodies are not shown"));
    (Geo { items, preview: None }, note)
}

/// The items of `geo` that can be obstacles: the copies placed by textures are left out, since
/// where they are is known on the GPU only.
fn solid(geo: Option<&Geo>) -> impl Iterator<Item = &Item> {
    geo.map(|g| g.items.as_slice()).unwrap_or_default().iter().filter(|i| i.instances.is_none())
}

/// Per obstacle of `geo` what its mesh is built from and where it stands, and a note about
/// what was left out.
fn obstacle_parts(geo: Option<&Geo>) -> (Vec<Form>, Vec<Place>, Option<String>) {
    let (forms, places) = solid(geo)
        .map(|i| {
            let (form, place) = split(&i.world);
            ((i.model.id, form), place)
        })
        .unzip();
    let left_out = geo.is_some_and(|g| g.items.iter().any(|i| i.instances.is_some()));
    let note = left_out.then(|| {
        "copies placed by textures (geo.tex_instance) are no obstacles: the bodies pass through".into()
    });
    (forms, places, note)
}

/// The obstacles of `geo`, built from `forms` and standing at `places` ([`obstacle_parts`]).
fn obstacles_of(geo: Option<&Geo>, forms: &[Form], places: &[Place]) -> Vec<Obstacle> {
    let parts = solid(geo).zip(forms).zip(places);
    parts.map(|((item, (_, form)), place)| shaped(&item.model.mesh, form, *place)).collect()
}

/// `mesh` standing where `world` puts it, as an obstacle.
pub fn obstacle_of(mesh: &Mesh, world: &Mat4) -> Obstacle {
    let (form, place) = split(world);
    shaped(mesh, &form, place)
}

/// `mesh` sized and sheared by `form`, as an obstacle at `place`.
fn shaped(mesh: &Mesh, form: &[[f64; 3]; 3], place: Place) -> Obstacle {
    let formed = |p: [f32; 3]| {
        let [x, y, z] = p.map(f64::from);
        form.map(|r| (r[0] * x + r[1] * y + r[2] * z) as f32)
    };
    Obstacle {
        vertices: mesh.vertices.iter().map(|v| formed(v.position)).collect(),
        triangles: mesh.indices.as_chunks::<3>().0.to_vec(),
        place,
    }
}

/// Whether obstacles built from `old` still have the forms `new`: the same models in the same
/// order, sized and sheared alike. The split of a matrix that only turns gives its form anew
/// on every frame, off by rounding, so alike allows for a millionth.
fn same_forms(old: &[Form], new: &[Form]) -> bool {
    let alike = |a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]| {
        let size = a.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() <= 1e-6 * size)
    };
    old.len() == new.len() && old.iter().zip(new).all(|((ia, fa), (ib, fb))| ia == ib && alike(fa, fb))
}

/// What the mesh of an obstacle is built from: its model, and the part of its matrix that
/// changes its form (size and shear). While both stay, the obstacle only moves.
type Form = (u64, [[f64; 3]; 3]);

/// Splits where an item stands into the part that changes its form and the part that only
/// moves it: `world` is the move (a turn, then a shift) applied after the form. A matrix that
/// flattens the item has no such split: all of it then counts as form, around the origin.
pub fn split(world: &Mat4) -> ([[f64; 3]; 3], Place) {
    let [[a, b, c, tx], [d, e, f, ty], [g, h, i, tz], _] = *world;
    let whole = ([[a, b, c], [d, e, f], [g, h, i]], Place::default());
    let dot = |p: [f64; 3], q: [f64; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
    // The columns, made square to each other one after the other (Gram-Schmidt): the turn is
    // what is left of them, the form is what was taken off.
    let (c0, c1, c2) = ([a, d, g], [b, e, h], [c, f, i]);
    let size = dot(c0, c0).max(dot(c1, c1)).max(dot(c2, c2)).sqrt();
    let unit = |v: [f64; 3]| {
        let len = dot(v, v).sqrt();
        (len.is_finite() && len > 1e-9 * size && len > 0.0).then(|| (v.map(|x| x / len), len))
    };
    let less = |v: [f64; 3], q: [f64; 3], by: f64| [v[0] - by * q[0], v[1] - by * q[1], v[2] - by * q[2]];
    let Some((q0, u00)) = unit(c0) else { return whole };
    let u01 = dot(q0, c1);
    let Some((q1, u11)) = unit(less(c1, q0, u01)) else { return whole };
    let (u02, u12) = (dot(q0, c2), dot(q1, c2));
    let Some((q2, u22)) = unit(less(less(c2, q0, u02), q1, u12)) else { return whole };
    // A mirror is form: a turn never mirrors.
    let cross = [q0[1] * q1[2] - q0[2] * q1[1], q0[2] * q1[0] - q0[0] * q1[2], q0[0] * q1[1] - q0[1] * q1[0]];
    let (q2, u22) = if dot(cross, q2) < 0.0 { (q2.map(|x| -x), -u22) } else { (q2, u22) };
    let turn = [[q0[0], q1[0], q2[0]], [q0[1], q1[1], q2[1]], [q0[2], q1[2], q2[2]]];
    let place = Place { translate: [tx, ty, tz].map(|v| v as f32), turn: turn.map(|r| r.map(|v| v as f32)) };
    ([[u00, u01, u02], [0.0, u11, u12], [0.0, 0.0, u22]], place)
}

/// The placements as a table: the column names, then one row per body, group after group.
/// Places are written as the solver holds them (single precision), angles to a ten-thousandth
/// of a degree.
fn table_of(groups: &[(usize, &Bodies, Vec<Placement>)]) -> Result<Table, Error> {
    let count: usize = groups.iter().map(|(_, _, p)| p.len()).sum();
    let mut rows = Vec::with_capacity(count.min(MAX_BODIES) + 1);
    rows.push(OUT_COLUMNS.iter().map(ToString::to_string).collect());
    for (index, _, placements) in groups {
        let group = index.to_string();
        for p in placements {
            let mut row: Vec<String> = Vec::with_capacity(OUT_COLUMNS.len());
            row.extend(p.translate.iter().map(|v| (*v as f32).to_string()));
            row.extend(p.rotate_degrees.iter().map(|v| {
                let rounded = (v * 1e4).round() / 1e4;
                // No "-0".
                if rounded == 0.0 { "0".to_string() } else { rounded.to_string() }
            }));
            row.extend(p.scale.iter().map(ToString::to_string));
            row.push(group.clone());
            rows.push(row);
        }
    }
    Table::new(rows)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use penumbra_core::{FrameTime, NodePath, ParamPath, Rate, Table, Tick, Value};
    use penumbra_graph::{Cooker, JobPool, Network, Op, Port};
    use penumbra_mesh::prim;
    use penumbra_physics::{Material, Shape, Start};

    use super::group::shape_of;
    use super::*;
    use crate::builtin_types;
    use crate::geo::placements;

    const IDENTITY: Mat4 = penumbra_gpu::geo::IDENTITY;

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    fn connect(from: &str, to: &str, input: u32) -> Op {
        Op::Connect { from: from.parse().unwrap(), output: 0, to: to.parse().unwrap(), input }
    }

    /// A `table.text` at `/t<n>` holding `text`, wired into a `physics.bodies` at `/b<n>`, wired
    /// into input `n` of the solver at `/r`.
    fn group(n: u32, text: &str) -> Vec<Op> {
        let (t, b) = (format!("/t{n}"), format!("/b{n}"));
        vec![
            create(&t, "table.text"),
            set(&format!("{t}.text"), text),
            set(&format!("{t}.format"), "csv"),
            create(&b, group::KIND),
            connect(&t, &b, 0),
            connect(&b, "/r", n),
        ]
    }

    /// A network of the solver at `/r` and one group per text ([`group`]).
    struct Rig {
        types: Arc<penumbra_graph::NodeTypes>,
        net: Arc<Network>,
        cooker: Cooker,
        frame: i64,
    }

    impl Rig {
        fn new(texts: &[&str]) -> Self {
            let types = Arc::new(builtin_types().unwrap());
            let mut ops = vec![create("/r", KIND)];
            for (n, text) in texts.iter().enumerate() {
                ops.extend(group(n as u32, text));
            }
            let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
            let cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
            Self { types, net, cooker, frame: 0 }
        }

        fn apply(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        }

        /// Cooks `frames` show frames at 60 a second and returns the table and the warning.
        fn run(&mut self, frames: i64) -> (Table, Option<String>) {
            let id = self.net.resolve(&"/r".parse().unwrap()).unwrap();
            for _ in 0..frames {
                let time = FrameTime {
                    frame: self.frame,
                    tick: Tick::from_seconds(self.frame as f64 / 60.0),
                    rate: Rate::FPS_60,
                };
                // Both outputs are made only while something reads them.
                let shown = [GEO_OUT, TABLE_OUT].map(|output| Port { node: id, output: output as u32 });
                self.cooker.cook_showing(&self.net, time, &[id], &[], &shown);
                self.frame += 1;
            }
            let status = self.cooker.status(id).unwrap();
            assert!(status.error.is_none(), "{:?}", status.error);
            let port = Port { node: id, output: TABLE_OUT as u32 };
            let out = self.cooker.output(port).unwrap().get::<Table>().unwrap().clone();
            (out, status.warning)
        }

        /// The warning of the node at `path`.
        fn warning(&self, path: &str) -> Option<String> {
            let id = self.net.resolve(&path.parse().unwrap()).unwrap();
            self.cooker.status(id).unwrap().warning
        }
    }

    fn number(table: &Table, row: usize, col: usize) -> f64 {
        table.cell(row, col).parse().unwrap()
    }

    #[test]
    fn bodies_start_where_the_table_puts_them_then_fall_and_rest() {
        let mut rig = Rig::new(&["tx,ty,tz,sx\n0,0,3\n5,0,2,2"]);
        let (out, warning) = rig.run(1);
        assert_eq!(warning, None);
        assert_eq!(out.rows()[0], OUT_COLUMNS);
        assert_eq!(out.rows()[1], ["0", "0", "3", "0", "0", "0", "1", "1", "1", "0"]);
        assert_eq!(out.rows()[2], ["5", "0", "2", "0", "0", "0", "2", "1", "1", "0"]);
        let (out, _) = rig.run(2);
        assert!(number(&out, 1, 2) < 3.0, "time moves the bodies from the second frame on");
        let (out, _) = rig.run(300);
        assert!((number(&out, 1, 2) - 0.5).abs() < 0.02, "{}", out.cell(1, 2));
        assert!((number(&out, 2, 2) - 0.5).abs() < 0.02, "{}", out.cell(2, 2));
        assert!((number(&out, 2, 0) - 5.0).abs() < 0.02);
        // What comes out is what geo.instance reads.
        let (places, warning) = placements(&out);
        assert_eq!((places.len(), warning), (2, None));
        // Without a GPU there is no box to draw the bodies as: the geometry is empty, no error.
        let id = rig.net.resolve(&"/r".parse().unwrap()).unwrap();
        let geo = rig.cooker.output(Port { node: id, output: GEO_OUT as u32 }).unwrap();
        assert!(geo.get::<Geo>().unwrap().items.is_empty());
    }

    #[test]
    fn reset_and_a_new_table_start_over_and_params_do_not() {
        let mut rig = Rig::new(&["tz\n3"]);
        let (out, _) = rig.run(30);
        let fallen = number(&out, 1, 2);
        assert!(fallen < 2.9);
        // Gravity turned off: the body keeps its speed and goes on from where it is.
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3]))]);
        let (out, _) = rig.run(2);
        let z = number(&out, 1, 2);
        assert!(z < fallen && z > 0.6, "{z}");
        // So it does when its group gets another material.
        rig.apply(&[set("/b0.friction", 0.1), set("/b0.density", 5.0)]);
        let (out, _) = rig.run(2);
        assert!(number(&out, 1, 2) < z, "a new material does not start the bodies over");
        rig.apply(&[set("/r.reset", true)]);
        assert_eq!(rig.run(3).0.cell(1, 2), "3");
        rig.apply(&[set("/r.reset", false), set("/r.speed", 0.0)]);
        assert_eq!(rig.run(10).0.cell(1, 2), "3", "speed 0 holds the bodies");
        rig.apply(&[set("/r.speed", 1.0), set("/r.gravity", Value::Vec3([0.0, 0.0, -9.81]))]);
        assert!(number(&rig.run(10).0, 1, 2) < 3.0);
        rig.apply(&[set("/t0.text", "tz\n7\n9")]);
        let (out, _) = rig.run(1);
        assert_eq!((out.num_rows(), out.cell(1, 2), out.cell(2, 2)), (3, "7", "9"));
        // The shape is part of the start: balls instead of boxes begin again.
        rig.run(20);
        rig.apply(&[set("/b0.shape", "sphere")]);
        assert_eq!(rig.run(1).0.cell(1, 2), "7");
    }

    #[test]
    fn a_table_that_does_not_fit_warns_at_its_group() {
        let mut rig = Rig::new(&["a,b\n1,2"]);
        let (out, warning) = rig.run(2);
        assert_eq!((out.num_rows(), warning), (1, None));
        assert!(rig.warning("/b0").unwrap().contains("names the columns"));
        let mut rig = Rig::new(&["tz\n1\nhigh\n2"]);
        assert_eq!(rig.run(2).0.num_rows(), 3);
        assert!(rig.warning("/b0").unwrap().contains("row 2"));
    }

    #[test]
    fn a_solver_without_a_group_says_what_to_connect() {
        let types = Arc::new(builtin_types().unwrap());
        let net = Arc::new(Network::new().apply(&types, &[create("/r", KIND)]).unwrap().network);
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/r".parse().unwrap()).unwrap();
        let time = FrameTime { frame: 0, tick: Tick::from_seconds(0.0), rate: Rate::FPS_60 };
        cooker.cook(&net, time, &[id], &[]);
        assert!(cooker.status(id).unwrap().error.unwrap().contains("physics.bodies"));
    }

    #[test]
    fn a_group_that_fails_is_named_and_the_others_go_on() {
        // A physics.bodies without a table is in error. Alone on the solver it is the error.
        let types = Arc::new(builtin_types().unwrap());
        let ops = [create("/r", KIND), create("/b0", group::KIND), connect("/b0", "/r", 0)];
        let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
        let mut cooker = Cooker::new(types, Arc::new(JobPool::new(1).unwrap()));
        let id = net.resolve(&"/r".parse().unwrap()).unwrap();
        let time = FrameTime { frame: 0, tick: Tick::from_seconds(0.0), rate: Rate::FPS_60 };
        cooker.cook(&net, time, &[id], &[]);
        let error = cooker.status(id).unwrap().error.unwrap();
        assert!(error.contains("`bodies0`") && error.contains("failed"), "{error}");
        // Beside a group that works it is a warning, and the working group's bodies fall.
        let mut rig = Rig::new(&["tz\n3"]);
        rig.apply(&[create("/broken", group::KIND), connect("/broken", "/r", 1)]);
        let (out, warning) = rig.run(30);
        let warning = warning.unwrap();
        assert!(warning.contains("`bodies1`") && warning.contains("without its bodies"), "{warning}");
        assert!(out.num_rows() == 2 && number(&out, 1, 2) < 2.9);
        // Mended, it joins the world and the warning goes.
        let mut ops = vec![create("/t1", "table.text"), set("/t1.text", "tz\n7"), set("/t1.format", "csv")];
        ops.push(connect("/t1", "/broken", 0));
        rig.apply(&ops);
        let (out, warning) = rig.run(2);
        assert_eq!((out.num_rows(), warning), (3, None));
    }

    /// The text of a table with one row at `x`, 0.5 m over the floor.
    fn row_at(x: f64) -> String {
        format!("tx,tz\n{x},0.5")
    }

    #[test]
    fn a_driven_group_follows_its_table_and_pushes_what_is_in_its_way() {
        // Group 0: a box at rest at the origin. Group 1: a box the table moves.
        let mut rig = Rig::new(&["tz\n0.5", &row_at(-3.0)]);
        rig.apply(&[set("/b1.mode", "driven")]);
        let (out, warning) = rig.run(60);
        assert_eq!((out.num_rows(), warning), (3, None));
        assert_eq!((out.cell(2, 0), out.cell(2, 2)), ("-3", "0.5"), "gravity does not move a driven body");
        // The table moves its row by 5 cm a frame (3 m/s), through the box at rest.
        for k in 1..=120 {
            rig.apply(&[set("/t1.text", row_at(-3.0 + 0.05 * f64::from(k)))]);
            let (out, _) = rig.run(1);
            let (pushed, driven) = (number(&out, 1, 0), number(&out, 2, 0));
            // The frame a row changes in shows the body there: it does not lag a frame.
            assert!((driven - (-3.0 + 0.05 * f64::from(k))).abs() < 1e-4, "frame {k}: {driven}");
            assert!(pushed > driven + 0.95 || k < 30, "frame {k}: the driven box is inside the other");
        }
        // Let go of, the pushed box slides on a little and stops.
        let (out, _) = rig.run(120);
        let pushed = number(&out, 1, 0);
        assert!(pushed > 3.95 && pushed < 8.0, "pushed ahead: {pushed}");
        // A row that moves farther than `jump` in a frame puts the body there: to the far
        // side of the box it pushed, which is not swept along.
        rig.apply(&[set("/t1.text", row_at(12.0))]);
        let (out, _) = rig.run(30);
        assert_eq!(out.cell(2, 0), "12");
        assert_eq!(number(&out, 1, 0), pushed);
        // Rows come and go with their bodies, and row k stays body k.
        rig.apply(&[set("/t1.text", "tx,tz,sx\n8,0.5,1\n8,3,2\n8,6,1")]);
        let (out, _) = rig.run(1);
        assert_eq!(out.num_rows(), 5);
        assert_eq!((out.cell(3, 2), out.cell(3, 6), out.cell(4, 9)), ("3", "2", "1"));
        rig.apply(&[set("/t1.text", "tx,tz,sx\n8,0.5,3")]);
        let (out, warning) = rig.run(1);
        assert_eq!((out.num_rows(), out.cell(2, 6), warning), (3, "3", None));
    }

    #[test]
    fn time_that_stands_holds_no_driven_body_back() {
        let mut rig = Rig::new(&["tz\n0.5", &row_at(-3.0)]);
        rig.apply(&[set("/b1.mode", "driven")]);
        rig.run(60);
        rig.apply(&[set("/r.speed", 0.0)]);
        for k in 1..=60 {
            rig.apply(&[set("/t1.text", row_at(-3.0 + 0.1 * f64::from(k)))]);
            rig.run(1);
        }
        let (out, _) = rig.run(1);
        assert_eq!(out.cell(2, 0), "3", "the body is where its row is");
        rig.apply(&[set("/r.speed", 1.0)]);
        let (out, _) = rig.run(30);
        assert!(number(&out, 1, 0).abs() < 1e-3, "nothing was pushed meanwhile: {}", out.cell(1, 0));
    }

    #[test]
    fn a_fixed_group_holds_what_lands_on_it_and_a_switch_of_mode_starts_it_over() {
        // Group 0: a box hung at 2 m, with a speed its mode ignores. Group 1: a box dropped
        // on it.
        let mut rig = Rig::new(&["tz,vx,wz\n2,3,90", "tz\n5"]);
        rig.apply(&[set("/b0.mode", "fixed")]);
        let (out, warning) = rig.run(240);
        assert_eq!(warning, None);
        assert_eq!(out.rows()[1][..6], ["0", "0", "2", "0", "0", "0"], "it hangs where its row puts it");
        assert!((number(&out, 2, 2) - 3.0).abs() < 0.03, "the other rests on it: {}", out.cell(2, 2));
        // A change to the table moves it at once, and does not start anything over: the box
        // that rested on it falls on from where it is.
        rig.apply(&[set("/t0.text", "tx,tz\n6,2")]);
        let (out, _) = rig.run(1);
        assert_eq!((out.cell(1, 0), out.cell(1, 2)), ("6", "2"));
        assert!(number(&out, 2, 2) < 3.1 && number(&out, 2, 2) > 2.9);
        let (out, _) = rig.run(120);
        assert!((number(&out, 2, 2) - 0.5).abs() < 0.03, "{}", out.cell(2, 2));
        // Released, the fixed box starts from its row and falls too.
        rig.apply(&[set("/b0.mode", "dynamic")]);
        let (out, _) = rig.run(1);
        assert_eq!(out.cell(1, 2), "2");
        let (out, _) = rig.run(120);
        assert!((number(&out, 1, 2) - 0.5).abs() < 0.03, "{}", out.cell(1, 2));
        // Density does nothing for a body nothing moves: the param says so.
        let types = builtin_types().unwrap();
        let desc = types.get(group::KIND).unwrap().desc();
        let density = desc.params.iter().find(|p| p.name.as_str() == "density").unwrap();
        assert!(density.applies_label().contains("dynamic"), "{}", density.applies_label());
    }

    #[test]
    fn a_speed_at_the_start_throws_the_body() {
        let mut rig = Rig::new(&["tz,vx,wz\n1,2,90"]);
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3])), set("/r.floor", false)]);
        // The first frame shows the start; 60 more are one second.
        let (out, _) = rig.run(61);
        assert!((number(&out, 1, 0) - 2.0).abs() < 0.02, "{}", out.cell(1, 0));
        assert!((number(&out, 1, 5) - 90.0).abs() < 1.0, "{}", out.cell(1, 5));
    }

    #[test]
    fn a_ball_of_one_group_lands_on_a_box_of_another() {
        // Group 0: a box of 1 m at rest on the floor. Group 1: a ball of 1 m dropped on it.
        let mut rig = Rig::new(&["tz\n0.5", "tz\n4"]);
        rig.apply(&[set("/b1.shape", "sphere")]);
        let (out, warning) = rig.run(300);
        assert_eq!((out.num_rows(), warning), (3, None));
        assert_eq!((out.cell(1, 9), out.cell(2, 9)), ("0", "1"), "the group column names the input");
        assert!((number(&out, 1, 2) - 0.5).abs() < 0.02, "the box stays: {}", out.cell(1, 2));
        assert!((number(&out, 2, 2) - 1.5).abs() < 0.03, "the ball rests on it: {}", out.cell(2, 2));
        // The box's group leaves: its row goes, and the ball falls on to the floor.
        rig.apply(&[Op::Disconnect { to: "/r".parse().unwrap(), input: 0 }]);
        let (out, _) = rig.run(120);
        assert_eq!((out.num_rows(), out.cell(1, 9)), (2, "1"));
        assert!((number(&out, 1, 2) - 0.5).abs() < 0.03, "{}", out.cell(1, 2));
    }

    #[test]
    fn a_changed_group_starts_over_and_the_others_go_on() {
        let mut rig = Rig::new(&["tz,vx\n1,1", "tx,tz,vx\n0,5,1"]);
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3])), set("/r.floor", false)]);
        let (out, _) = rig.run(31);
        let (a, b) = (number(&out, 1, 0), number(&out, 2, 0));
        assert!((a - 0.5).abs() < 0.02 && (b - 0.5).abs() < 0.02, "{a} {b}");
        rig.apply(&[set("/t1.text", "tx,tz,vx\n9,5,1\n9,7,1")]);
        let (out, _) = rig.run(1);
        assert_eq!(out.num_rows(), 4);
        assert!(number(&out, 1, 0) > a, "group 0 went on: {}", out.cell(1, 0));
        assert_eq!((out.cell(2, 0), out.cell(3, 0)), ("9", "9"), "group 1 shows its new start");
        let (out, _) = rig.run(30);
        assert!((number(&out, 1, 0) - 1.0).abs() < 0.04, "{}", out.cell(1, 0));
        assert!((number(&out, 2, 0) - 9.5).abs() < 0.02, "{}", out.cell(2, 0));
    }

    #[test]
    fn bodies_beyond_the_limit_of_the_world_are_left_out_with_a_warning() {
        // Two groups of 2500 bodies, far apart and at rest in empty space.
        let rows = |y: usize| -> String {
            let mut text = String::from("tx,ty");
            for k in 0..2500 {
                text.push_str(&format!("\n{},{y}", k * 3));
            }
            text
        };
        let mut rig = Rig::new(&[&rows(0), &rows(10)]);
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3])), set("/r.floor", false)]);
        let (out, warning) = rig.run(2);
        assert_eq!(out.num_rows(), 1 + MAX_BODIES);
        assert_eq!((out.cell(2500, 9), out.cell(2501, 9)), ("0", "1"));
        let warning = warning.unwrap();
        assert!(warning.contains("`bodies1`") && warning.contains("the last 1000"), "{warning}");
    }

    #[test]
    fn the_shape_follows_the_model_and_the_choice() {
        let mesh = prim::moved(prim::cuboid([2.0, 4.0, 6.0]).unwrap(), [1.0, 0.0, 0.0]);
        let (shape, note) = shape_of("box", [(&mesh, &IDENTITY)].into_iter());
        assert_eq!((shape, note), (Shape::Box { center: [1.0, 0.0, 0.0], half: [1.0, 2.0, 3.0] }, None));
        let (shape, _) = shape_of("sphere", [(&mesh, &IDENTITY)].into_iter());
        assert_eq!(shape, Shape::Sphere { center: [1.0, 0.0, 0.0], radius: 3.0 });
        // Where the model stands counts: twice as big, one up.
        let world = trs([0.0, 0.0, 1.0], [0.0; 3], [2.0; 3]);
        let (shape, _) = shape_of("box", [(&mesh, &world)].into_iter());
        assert_eq!(shape, Shape::Box { center: [2.0, 0.0, 1.0], half: [2.0, 4.0, 6.0] });
        let (shape, note) = shape_of("hull", [(&mesh, &IDENTITY)].into_iter());
        assert!(matches!(shape, Shape::Hull { .. }) && note.is_none());
        // A flat model has no hull: its box stands in, and the node says so.
        let flat = prim::plane([1.0, 1.0]).unwrap();
        let (shape, note) = shape_of("hull", [(&flat, &IDENTITY)].into_iter());
        assert!(matches!(shape, Shape::Box { .. }));
        assert!(note.unwrap().contains("box around the model"));
        // No model: a box of 1 m, or a ball as wide.
        assert_eq!(shape_of("hull", std::iter::empty()).0, Shape::Box { center: [0.0; 3], half: [0.5; 3] });
        assert_eq!(shape_of("sphere", std::iter::empty()).0, Shape::Sphere { center: [0.0; 3], radius: 0.5 });
    }

    #[test]
    fn bodies_land_on_a_mesh_where_it_stands() {
        // A slab 4 x 4 x 0.2 lifted to z = 2: its top is at 2.1.
        let slab = prim::cuboid([4.0, 4.0, 0.2]).unwrap();
        let world_of_slab = trs([0.0, 0.0, 2.0], [0.0; 3], [1.0; 3]);
        let obstacle = obstacle_of(&slab, &world_of_slab);
        assert_eq!(obstacle.triangles.len(), 12);
        let mut world = World::new(Settings::default());
        world
            .set_group(0, shape_of("box", std::iter::empty()).0, Material::default(), Mode::Dynamic)
            .unwrap();
        world.set_obstacles(vec![obstacle]).unwrap();
        world.add(0, &Start { translate: [0.0, 0.0, 4.0], ..Start::default() }).unwrap();
        for _ in 0..300 {
            world.step(1.0 / 60.0, 1);
        }
        let z = world.placements(0)[0].translate[2];
        assert!((z - 2.6).abs() < 0.03, "{z}");
    }

    /// The matrix `split` took apart, put together again: the turn times the form, then the shift.
    fn joined(form: &[[f64; 3]; 3], place: &Place) -> [[f64; 4]; 3] {
        std::array::from_fn(|r| {
            std::array::from_fn(|c| match c {
                3 => f64::from(place.translate[r]),
                _ => (0..3).map(|k| f64::from(place.turn[r][k]) * form[k][c]).sum(),
            })
        })
    }

    #[test]
    fn a_matrix_splits_into_a_form_and_a_move() {
        let unit = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let near = |a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]| {
            a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-9)
        };
        // Moved and turned only: the form is that of the model, whatever the turn. Two turns
        // of one model are therefore the same obstacle at another place.
        let (form_a, place_a) = split(&trs([1.0, 2.0, 3.0], [20.0, -35.0, 70.0], [1.0; 3]));
        let (form_b, place_b) = split(&trs([-4.0, 0.0, 0.5], [170.0, 80.0, -160.0], [1.0; 3]));
        assert!(near(&form_a, &unit) && near(&form_b, &unit), "{form_a:?} {form_b:?}");
        assert_eq!(place_a.translate, [1.0, 2.0, 3.0]);
        assert_ne!(place_a.turn, place_b.turn);
        assert!(same_forms(&[(7, form_a)], &[(7, form_b)]));
        assert!(!same_forms(&[(7, form_a)], &[(8, form_b)]), "another model is another obstacle");
        assert!(!same_forms(&[(7, form_a)], &[(7, form_b), (7, form_b)]));
        // A size is form, and stays the same form while the obstacle turns.
        let (wide, _) = split(&trs([0.0; 3], [0.0, 0.0, 30.0], [2.0, 1.0, 0.5]));
        let (wide_turned, _) = split(&trs([5.0, 0.0, 0.0], [40.0, 10.0, -80.0], [2.0, 1.0, 0.5]));
        assert!(near(&wide, &[[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]]), "{wide:?}");
        assert!(same_forms(&[(7, wide)], &[(7, wide_turned)]));
        assert!(!same_forms(&[(7, wide)], &[(7, form_a)]), "a new size is a new obstacle");
        // Every split gives back its matrix, a mirror and a shear included, and its turn is
        // a rotation: square columns, no mirror.
        let mut sheared = trs([1.0, -2.0, 0.25], [15.0, 25.0, 35.0], [1.5, -0.7, 2.0]);
        sheared[0][1] += 0.4;
        sheared[2][0] -= 0.3;
        for world in [trs([3.0, 1.0, 2.0], [10.0, 20.0, 30.0], [-1.0, 2.0, 3.0]), sheared, IDENTITY] {
            let (form, place) = split(&world);
            let back = joined(&form, &place);
            for r in 0..3 {
                for c in 0..4 {
                    assert!((back[r][c] - world[r][c]).abs() < 1e-5, "{world:?} came back as {back:?}");
                }
            }
            let t = place.turn.map(|r| r.map(f64::from));
            for a in 0..3 {
                for b in 0..3 {
                    let dot: f64 = (0..3).map(|k| t[k][a] * t[k][b]).sum();
                    assert!((dot - if a == b { 1.0 } else { 0.0 }).abs() < 1e-5);
                }
            }
            let det = t[0][0] * (t[1][1] * t[2][2] - t[1][2] * t[2][1])
                - t[0][1] * (t[1][0] * t[2][2] - t[1][2] * t[2][0])
                + t[0][2] * (t[1][0] * t[2][1] - t[1][1] * t[2][0]);
            assert!((det - 1.0).abs() < 1e-5, "the turn mirrors: {det}");
        }
        // A matrix that flattens has no turn to take out: all of it is form, and a matrix
        // that is not a number does not panic.
        let flat = trs([1.0, 2.0, 3.0], [0.0, 30.0, 0.0], [1.0, 1.0, 0.0]);
        let (form, place) = split(&flat);
        assert_eq!(place, Place::default());
        assert!((form[0][0] - flat[0][0]).abs() < 1e-12);
        let _ = split(&[[f64::NAN; 4]; 4]);
        let _ = split(&[[0.0; 4]; 4]);
    }

    #[test]
    fn a_turned_slab_holds_a_body_as_one_baked_into_the_world_did() {
        // A slab 4 x 4 x 0.2, tilted by 90 degrees about x so that its 4 x 4 face stands
        // upright in the plane y = 0, then lifted: its top edge is at z = 3.
        let slab = prim::cuboid([4.0, 4.0, 0.2]).unwrap();
        let obstacle = obstacle_of(&slab, &trs([0.0, 0.0, 1.0], [90.0, 0.0, 0.0], [1.0; 3]));
        assert!(obstacle.vertices.iter().all(|v| v[2].abs() == 0.1), "the mesh stays in its own space");
        let mut world = World::new(Settings::default());
        world
            .set_group(0, shape_of("box", std::iter::empty()).0, Material::default(), Mode::Dynamic)
            .unwrap();
        world.set_obstacles(vec![obstacle]).unwrap();
        world.add(0, &Start { translate: [0.0, 0.0, 5.0], ..Start::default() }).unwrap();
        for _ in 0..300 {
            world.step(1.0 / 60.0, 1);
        }
        // The box of 1 m balances on the 0.2 m wide top edge, or has tipped off it to the floor.
        let p = world.placements(0)[0].translate;
        assert!((p[2] - 3.5).abs() < 0.03 || p[2] < 1.0, "{p:?}");
        assert!(p[1].abs() > 0.55 || (p[2] - 3.5).abs() < 0.03, "it is not inside the slab: {p:?}");
    }

    #[test]
    fn the_angles_that_come_out_turn_a_copy_as_the_body_is_turned() {
        // A body thrown with a spin about a skew axis, read back through geo.instance's own
        // reading of the table: its matrix must be a rotation (no shear, no mirror).
        let mut rig = Rig::new(&["tz,wx,wy,wz\n1,200,-310,45"]);
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3])), set("/r.floor", false)]);
        let (out, _) = rig.run(40);
        let (places, _) = placements(&out);
        let m = places[0];
        for a in 0..3 {
            for b in 0..3 {
                let dot: f64 = (0..3).map(|k| m[k][a] * m[k][b]).sum();
                let want = if a == b { 1.0 } else { 0.0 };
                assert!((dot - want).abs() < 1e-4, "columns {a} and {b}: {dot}");
            }
        }
        assert!(out.rows()[1][3..6].iter().any(|c| c != "0"), "it turned");
    }
}
