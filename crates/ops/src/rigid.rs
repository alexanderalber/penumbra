//! `physics.rigid_bodies`: rigid bodies that fall, collide and come to rest. Each `bodies` input
//! takes one group ([`group::Bodies`], made by `physics.bodies`): a table of starts, a model, a
//! collision shape and a material. All groups live in one world and collide with each other.
//! The node hands out the bodies as geometry, each group drawn as its own model, and as a table
//! of where they are now, in the columns `geo.instance` reads. The solver runs on the CPU
//! (`penumbra-physics`), one step per show frame, on the cook thread.
//!
//! A group starts over when its starts or its collision shape change; the other groups go on.
//! Materials, obstacles and the params change while the bodies move.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Table, Tick, Unit, Value};
use penumbra_gpu::geo::{Item, Mat4, Model, mul};
use penumbra_gpu::render::MAX_DRAWS;
use penumbra_gpu::{Geo, Gpu};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Mesh;
use penumbra_physics::{MAX_BODIES, MAX_GROUPS, MAX_SUBSTEPS, Obstacle, Placement, Settings, World};

use crate::geo::trs;
use crate::tex::fluid::step;

pub mod group;

use group::{Bodies, GeoKey, key, moved_by, placed};

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
        .param(ParamSpec::bool("reset", false).doc("Puts every body of every group back to its start, every frame while on"))
        .input_run(
            "bodies",
            DataKind::Bodies,
            1,
            "A group of bodies from physics.bodies. The groups of all these inputs collide with each other, 4000 bodies in all at most. A group whose table or shape changes starts over; the others go on",
        )
        .input("obstacles", DataKind::Geo)
        .describe_input(
            "obstacles",
            "Geometry that stands still and that the bodies land on and bounce off, hollows and all. It may change while the bodies move; a moving obstacle does not push them along",
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
    obstacles: Option<GeoKey>,
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
        for i in 0..MAX_RUN {
            match cx.input(i) {
                Some(data) => match data.get::<Bodies>() {
                    Some(bodies) => groups.push((i, data, bodies)),
                    None => return Err(CookError::Pending),
                },
                None if wired(i) => return Err(CookError::Pending),
                None => {}
            }
        }
        if groups.is_empty() {
            return Err(CookError::failed("connect a physics.bodies: a group of bodies to move"));
        }
        let obstacle_geo = match cx.input_as::<Geo>(OBSTACLES) {
            Some(g) => Some(g),
            None if wired(OBSTACLES) => return Err(CookError::Pending),
            None => None,
        };
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
                Some(slot) if slot.data.get::<Bodies>().is_some_and(|old| same_start(old, bodies)) => {
                    world.set_material(i, bodies.material);
                    slot.data = data.clone();
                    changed = true;
                }
                _ => restart.push((i, data, bodies)),
            }
        }
        let obstacle_key = key(obstacle_geo);
        if self.obstacles.as_ref() != Some(&obstacle_key) {
            let (obstacles, note) = obstacles_of(obstacle_geo);
            self.obstacle_note = note;
            // Kept even when they fail, so a mesh that cannot be collided with is not tried
            // again on every frame.
            self.obstacles = Some(obstacle_key);
            if let Err(e) = world.set_obstacles(obstacles) {
                self.obstacle_note = Some(format!("{e}: the bodies fall through the obstacles"));
            }
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
            .chain(&self.draw_note)
            .map(String::as_str)
            .collect();
        if !notes.is_empty() {
            cx.warn(notes.join("; "));
        }
        Ok(outputs.map_or(Cooked::Unchanged, Cooked::Outputs))
    }
}

/// Whether the bodies of `new` may go on from where those of `old` are: the same starts and
/// the same collision shape. The material and the look may differ.
fn same_start(old: &Bodies, new: &Bodies) -> bool {
    old.starts == new.starts && old.shape == new.shape
}

/// Makes group `index` of `world` anew from `bodies`, every body at its start. Returns what
/// kept bodies out, if anything did.
fn start_group(world: &mut World, index: usize, bodies: &Bodies) -> Option<String> {
    if let Err(e) = world.set_group(index, bodies.shape.clone(), bodies.material) {
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

/// The obstacles of `geo`, each mesh where it stands, and a note about what was left out.
fn obstacles_of(geo: Option<&Geo>) -> (Vec<Obstacle>, Option<String>) {
    let Some(geo) = geo else { return (Vec::new(), None) };
    let left_out = geo.items.iter().filter(|i| i.instances.is_some()).count();
    let note = (left_out > 0).then(|| {
        "copies placed by textures (geo.tex_instance) are no obstacles: the bodies pass through".into()
    });
    (placed(Some(geo)).map(|(mesh, world)| obstacle_of(mesh, world)).collect(), note)
}

pub fn obstacle_of(mesh: &Mesh, world: &Mat4) -> Obstacle {
    Obstacle {
        vertices: mesh.vertices.iter().map(|v| moved_by(world, v.position)).collect(),
        triangles: mesh.indices.as_chunks::<3>().0.to_vec(),
    }
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
        world.set_group(0, shape_of("box", std::iter::empty()).0, Material::default()).unwrap();
        world.set_obstacles(vec![obstacle]).unwrap();
        world.add(0, &Start { translate: [0.0, 0.0, 4.0], ..Start::default() }).unwrap();
        for _ in 0..300 {
            world.step(1.0 / 60.0, 1);
        }
        let z = world.placements(0)[0].translate[2];
        assert!((z - 2.6).abs() < 0.03, "{z}");
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
