//! `table.rigid_bodies`: rigid bodies that fall, collide and come to rest. A table places the
//! bodies at the start; the node hands out a table of where they are now, in the columns
//! `geo.instance` reads. The solver runs on the CPU (`penumbra-physics`), one step per show
//! frame, on the cook thread.
//!
//! The world is built again, and the bodies start over, when the table of bodies or their
//! shape changes. Obstacles and the params change while the bodies move.

use penumbra_core::{Error, ParamSpec, Table, Tick, Unit, Value};
use penumbra_gpu::Geo;
use penumbra_gpu::geo::Mat4;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Mesh;
use penumbra_physics::{MAX_BODIES, MAX_SUBSTEPS, Obstacle, Placement, Settings, Shape, Start, World};

use crate::tex::fluid::step;

pub const KIND: &str = "table.rigid_bodies";

/// Choices of `shape`.
pub const SHAPES: [&str; 3] = ["hull", "box", "sphere"];

/// The columns of the `bodies` table, by header name: where a body starts (as `geo.instance`
/// reads them), how fast it moves (metres per second) and spins (degrees per second).
pub const START_COLUMNS: [&str; 15] =
    ["tx", "ty", "tz", "rx", "ry", "rz", "sx", "sy", "sz", "vx", "vy", "vz", "wx", "wy", "wz"];

/// The columns of the output, the ones `geo.instance` reads.
pub const OUT_COLUMNS: [&str; 9] = ["tx", "ty", "tz", "rx", "ry", "rz", "sx", "sy", "sz"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Rigid bodies that fall, collide and come to rest: a table places them at the start, the output says where they are now, for geo.instance",
        )
        .param(
            ParamSpec::choice("shape", &SHAPES)
                .doc("What each body collides as, taken from the `shape` input. Without that input: a box of 1 m")
                .choice_doc("hull", "the model with its dents filled in, as if wrapped in foil")
                .choice_doc("box", "the box around the model; faster, and stacks well")
                .choice_doc("sphere", "a ball as wide as the model's longest side; the fastest, and it rolls"),
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
                .doc("How much surfaces grip: 0 slides like ice, 1 holds like rubber"),
        )
        .param(
            ParamSpec::float("bounce", 0.0)
                .clamp(0.0, 1.0)
                .doc("How much of its speed a body keeps when it hits something: 0 lands dead, 1 bounces for ever"),
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
        .param(ParamSpec::bool("reset", false).doc("Puts every body back where the table places it, every frame while on"))
        .input("bodies", DataKind::Table)
        .input("shape", DataKind::Geo)
        .input("obstacles", DataKind::Geo)
        .describe_input(
            "bodies",
            "One row per body, at most 4000. The first row names the columns: tx ty tz to place, rx ry rz to turn (degrees), sx sy sz to scale, vx vy vz for a speed at the start (metres per second), wx wy wz for a spin (degrees per second). A change puts every body back to its start",
        )
        .describe_input(
            "shape",
            "The model the bodies collide as; wire the same model into geo.instance. A change puts every body back to its start",
        )
        .describe_input(
            "obstacles",
            "Geometry that stands still and that the bodies land on and bounce off, hollows and all. It may change while the bodies move; a moving obstacle does not push them along",
        )
        .output("out", DataKind::Table)
        .describe_output("out", "One row per body, in the order of `bodies`: tx ty tz rx ry rz sx sy sz, the columns geo.instance reads")
        .stateful()
        .time_dependent(),
        || Box::new(RigidBodies::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// A model and where it stands: what tells one cook's geometry from the next.
type GeoKey = Vec<(u64, Mat4)>;

#[derive(Default)]
struct RigidBodies {
    world: Option<World>,
    last: Option<Tick>,
    /// What the world was built from.
    bodies: Option<Table>,
    shape: Option<(String, GeoKey)>,
    obstacles: Option<GeoKey>,
    /// What building the world found, said again on every cook while it holds.
    notes: Vec<String>,
    obstacle_note: Option<String>,
}

impl Operator for RigidBodies {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let Some(table) = cx.input_as::<Table>(0) else {
            return Err(if wired(0) {
                CookError::Pending
            } else {
                CookError::failed("connect a table with one row per body")
            });
        };
        let geo = |k: usize| -> Result<Option<&Geo>, CookError> {
            match cx.input_as::<Geo>(k) {
                Some(g) => Ok(Some(g)),
                None if wired(k) => Err(CookError::Pending),
                None => Ok(None),
            }
        };
        let (shape_geo, obstacle_geo) = (geo(1)?, geo(2)?);
        let settings = settings(cx)?;
        let choice = cx.text("shape")?;
        let reset = cx.bool("reset")?;
        let now = cx.time().tick;
        let period = 1.0 / cx.rate().as_f64().max(1.0);

        let shape_key = (choice.to_string(), key(shape_geo));
        let obstacle_key = key(obstacle_geo);
        let same = self.world.is_some()
            && self.bodies.as_ref() == Some(table)
            && self.shape.as_ref() == Some(&shape_key);
        let mut moved = false;
        if !same || reset {
            self.world = None;
            self.obstacles = None;
            self.notes.clear();
            let (shape, note) = shape_of(choice, placed(shape_geo));
            self.notes.extend(note);
            let mut world = World::new(shape, settings).map_err(CookError::failed)?;
            let (starts, note) = starts(table);
            self.notes.extend(note);
            for start in &starts {
                world.add(start).map_err(CookError::failed)?;
            }
            self.world = Some(world);
            self.bodies = Some(table.clone());
            self.shape = Some(shape_key);
            self.last = None;
            moved = true;
        }
        let Some(world) = self.world.as_mut() else { return Err(CookError::failed("no world")) };
        world.apply(settings);
        if self.obstacles.as_ref() != Some(&obstacle_key) {
            let (obstacles, note) = obstacles_of(obstacle_geo);
            self.obstacle_note = note;
            // Kept even when they fail, so a mesh that cannot be collided with is not tried
            // again on every frame.
            self.obstacles = Some(obstacle_key);
            if let Err(e) = world.set_obstacles(obstacles) {
                self.obstacle_note = Some(format!("{e}: the bodies fall through the obstacles"));
            }
            moved = true;
        }
        // A world just built shows its start; time moves it from the next frame on.
        let dt = if moved && self.last.is_none() {
            0.0
        } else {
            step(now, self.last, period) * cx.float("speed")?.max(0.0)
        };
        self.last = Some(now);
        let notes: Vec<&str> = self.notes.iter().chain(&self.obstacle_note).map(String::as_str).collect();
        if !notes.is_empty() {
            cx.warn(notes.join("; "));
        }
        if dt == 0.0 && !moved && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        let substeps = u32::try_from(cx.int("substeps")?).unwrap_or(1);
        world.step(dt as f32, substeps);
        let out = table_of(&world.placements()).map_err(|e| CookError::failed(e.to_string()))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, out)]))
    }
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

fn key(geo: Option<&Geo>) -> GeoKey {
    geo.map(|g| g.items.iter().map(|i| (i.model.id, i.world)).collect()).unwrap_or_default()
}

/// The meshes of `geo` with where they stand. Copies drawn by textures are left out: where they
/// are is known on the GPU only.
fn placed(geo: Option<&Geo>) -> impl Iterator<Item = (&Mesh, &Mat4)> {
    let items = geo.map(|g| g.items.as_slice()).unwrap_or_default();
    items.iter().filter(|i| i.instances.is_none()).map(|i| (&i.model.mesh, &i.world))
}

/// `p` moved by `m` (model to world).
fn moved_by(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = p.map(f64::from);
    m.first_chunk::<3>().map_or(p, |rows| rows.map(|r| (r[0] * x + r[1] * y + r[2] * z + r[3]) as f32))
}

/// What the bodies collide as: `choice` of the meshes, or of a box of 1 m when there are none.
/// Also a note when the choice could not be had.
pub fn shape_of<'a>(
    choice: &str,
    meshes: impl Iterator<Item = (&'a Mesh, &'a Mat4)>,
) -> (Shape, Option<String>) {
    let unit = Shape::Box { center: [0.0; 3], half: [0.5; 3] };
    let mut points: Vec<[f32; 3]> = Vec::new();
    for (mesh, world) in meshes {
        points.extend(mesh.vertices.iter().map(|v| moved_by(world, v.position)));
    }
    points.retain(|p| p.iter().all(|v| v.is_finite()));
    let Some(first) = points.first().copied() else {
        return match choice {
            "sphere" => (Shape::Sphere { center: [0.0; 3], radius: 0.5 }, None),
            _ => (unit, None),
        };
    };
    let ([mut x0, mut y0, mut z0], [mut x1, mut y1, mut z1]) = (first, first);
    for [x, y, z] in &points {
        (x0, y0, z0) = (x0.min(*x), y0.min(*y), z0.min(*z));
        (x1, y1, z1) = (x1.max(*x), y1.max(*y), z1.max(*z));
    }
    let center = [0.5 * (x0 + x1), 0.5 * (y0 + y1), 0.5 * (z0 + z1)];
    let half = [0.5 * (x1 - x0), 0.5 * (y1 - y0), 0.5 * (z1 - z0)];
    let widest = half.iter().copied().fold(0.0, f32::max);
    let boxed = Shape::Box { center, half };
    match choice {
        "box" => (boxed, None),
        "sphere" => (Shape::Sphere { center, radius: widest }, None),
        _ => {
            let hull = Shape::Hull { points };
            // The world tries the hull as it is built; a flat model fails there. Its box does not.
            match World::new(hull.clone(), Settings::default()) {
                Ok(_) => (hull, None),
                Err(e) => (boxed, Some(format!("{e}: the bodies collide as the box around the model"))),
            }
        }
    }
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

/// Where the bodies start: the first row of `table` names the columns (any of
/// [`START_COLUMNS`], in any order, others ignored), every further row is one body. A missing
/// column or an empty cell is 0 (1 for scale). Returns the starts and a note, if any.
pub fn starts(table: &Table) -> (Vec<Start>, Option<String>) {
    let Some(header) = table.rows().first() else { return (Vec::new(), None) };
    let index: Vec<Option<usize>> =
        START_COLUMNS.iter().map(|name| header.iter().position(|h| h.trim() == *name)).collect();
    if index.iter().all(Option::is_none) {
        return (Vec::new(), Some(format!("the first row names the columns: {}", START_COLUMNS.join(", "))));
    }
    let mut note = None;
    let mut out = Vec::new();
    for (r, row) in table.rows().iter().enumerate().skip(1) {
        if out.len() >= MAX_BODIES {
            note = Some(format!("more than {MAX_BODIES} rows: the rest are left out"));
            break;
        }
        let mut v = [0.0f32; 15];
        v[6..9].fill(1.0);
        let mut bad = false;
        for (slot, col) in v.iter_mut().zip(&index) {
            let Some(cell) = col.and_then(|c| row.get(c)).map(|c| c.trim()).filter(|c| !c.is_empty()) else {
                continue;
            };
            match cell.parse::<f32>() {
                Ok(x) if x.is_finite() => *slot = x,
                _ => bad = true,
            }
        }
        if bad {
            note.get_or_insert_with(|| format!("row {r} holds a cell that is not a number: left out"));
            continue;
        }
        out.push(Start {
            translate: [v[0], v[1], v[2]],
            rotate_degrees: [v[3], v[4], v[5]],
            scale: [v[6], v[7], v[8]],
            velocity: [v[9], v[10], v[11]],
            spin: [v[12], v[13], v[14]],
        });
    }
    (out, note)
}

/// The placements as a table: the column names, then one row per body. Places are written as
/// the solver holds them (single precision), angles to a ten-thousandth of a degree.
pub fn table_of(placements: &[Placement]) -> Result<Table, Error> {
    let mut rows = Vec::with_capacity(placements.len() + 1);
    rows.push(OUT_COLUMNS.iter().map(ToString::to_string).collect());
    for p in placements {
        let mut row: Vec<String> = Vec::with_capacity(OUT_COLUMNS.len());
        row.extend(p.translate.iter().map(|v| (*v as f32).to_string()));
        row.extend(p.rotate_degrees.iter().map(|v| {
            let rounded = (v * 1e4).round() / 1e4;
            // No "-0".
            if rounded == 0.0 { "0".to_string() } else { rounded.to_string() }
        }));
        row.extend(p.scale.iter().map(ToString::to_string));
        rows.push(row);
    }
    Table::new(rows)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use penumbra_core::{FrameTime, NodePath, ParamPath, Rate, Table, Tick, Value};
    use penumbra_graph::{Cooker, JobPool, Network, Op, Port};
    use penumbra_mesh::prim;

    use super::*;
    use crate::builtin_types;
    use crate::geo::{placements, trs};

    const IDENTITY: Mat4 = penumbra_gpu::geo::IDENTITY;

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    /// A network of a `table.text` at `/t` holding `text` and the solver at `/r`.
    struct Rig {
        types: Arc<penumbra_graph::NodeTypes>,
        net: Arc<Network>,
        cooker: Cooker,
        frame: i64,
    }

    impl Rig {
        fn new(text: &str) -> Self {
            let types = Arc::new(builtin_types().unwrap());
            let ops = [
                create("/t", "table.text"),
                set("/t.text", text),
                set("/t.format", "csv"),
                create("/r", KIND),
                Op::Connect { from: "/t".parse().unwrap(), output: 0, to: "/r".parse().unwrap(), input: 0 },
            ];
            let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
            let cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
            Self { types, net, cooker, frame: 0 }
        }

        fn apply(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        }

        /// Cooks `frames` show frames at 60 a second and returns the output and the warning.
        fn run(&mut self, frames: i64) -> (Table, Option<String>) {
            let id = self.net.resolve(&"/r".parse().unwrap()).unwrap();
            for _ in 0..frames {
                let time = FrameTime {
                    frame: self.frame,
                    tick: Tick::from_seconds(self.frame as f64 / 60.0),
                    rate: Rate::FPS_60,
                };
                self.cooker.cook(&self.net, time, &[id], &[]);
                self.frame += 1;
            }
            let status = self.cooker.status(id).unwrap();
            assert!(status.error.is_none(), "{:?}", status.error);
            let out =
                self.cooker.output(Port { node: id, output: 0 }).unwrap().get::<Table>().unwrap().clone();
            (out, status.warning)
        }
    }

    fn number(table: &Table, row: usize, col: usize) -> f64 {
        table.cell(row, col).parse().unwrap()
    }

    #[test]
    fn bodies_start_where_the_table_puts_them_then_fall_and_rest() {
        let mut rig = Rig::new("tx,ty,tz,sx\n0,0,3\n5,0,2,2");
        let (out, warning) = rig.run(1);
        assert_eq!(warning, None);
        assert_eq!(out.rows()[0], OUT_COLUMNS);
        assert_eq!(out.rows()[1], ["0", "0", "3", "0", "0", "0", "1", "1", "1"]);
        assert_eq!(out.rows()[2], ["5", "0", "2", "0", "0", "0", "2", "1", "1"]);
        let (out, _) = rig.run(2);
        assert!(number(&out, 1, 2) < 3.0, "time moves the bodies from the second frame on");
        let (out, _) = rig.run(300);
        assert!((number(&out, 1, 2) - 0.5).abs() < 0.02, "{}", out.cell(1, 2));
        assert!((number(&out, 2, 2) - 0.5).abs() < 0.02, "{}", out.cell(2, 2));
        assert!((number(&out, 2, 0) - 5.0).abs() < 0.02);
        // What comes out is what geo.instance reads.
        let (places, warning) = placements(&out);
        assert_eq!((places.len(), warning), (2, None));
    }

    #[test]
    fn reset_and_a_new_table_start_over_and_params_do_not() {
        let mut rig = Rig::new("tz\n3");
        let (out, _) = rig.run(30);
        let fallen = number(&out, 1, 2);
        assert!(fallen < 2.9);
        // Gravity turned off: the body keeps its speed and goes on from where it is.
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3]))]);
        let (out, _) = rig.run(2);
        let z = number(&out, 1, 2);
        assert!(z < fallen && z > 0.6, "{z}");
        rig.apply(&[set("/r.reset", true)]);
        assert_eq!(rig.run(3).0.cell(1, 2), "3");
        rig.apply(&[set("/r.reset", false), set("/r.speed", 0.0)]);
        assert_eq!(rig.run(10).0.cell(1, 2), "3", "speed 0 holds the bodies");
        rig.apply(&[set("/r.speed", 1.0), set("/r.gravity", Value::Vec3([0.0, 0.0, -9.81]))]);
        assert!(number(&rig.run(10).0, 1, 2) < 3.0);
        rig.apply(&[set("/t.text", "tz\n7\n9")]);
        let (out, _) = rig.run(1);
        assert_eq!((out.num_rows(), out.cell(1, 2), out.cell(2, 2)), (3, "7", "9"));
    }

    #[test]
    fn a_table_that_does_not_fit_warns() {
        let (out, warning) = Rig::new("a,b\n1,2").run(2);
        assert_eq!(out.num_rows(), 1);
        assert!(warning.unwrap().contains("names the columns"));
        let (out, warning) = Rig::new("tz\n1\nhigh\n2").run(2);
        assert_eq!(out.num_rows(), 3);
        assert!(warning.unwrap().contains("row 2"));
    }

    #[test]
    fn a_speed_at_the_start_throws_the_body() {
        let mut rig = Rig::new("tz,vx,wz\n1,2,90");
        rig.apply(&[set("/r.gravity", Value::Vec3([0.0; 3])), set("/r.floor", false)]);
        // The first frame shows the start; 60 more are one second.
        let (out, _) = rig.run(61);
        assert!((number(&out, 1, 0) - 2.0).abs() < 0.02, "{}", out.cell(1, 0));
        assert!((number(&out, 1, 5) - 90.0).abs() < 1.0, "{}", out.cell(1, 5));
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
        let mut world = World::new(shape_of("box", std::iter::empty()).0, Settings::default()).unwrap();
        world.set_obstacles(vec![obstacle]).unwrap();
        world.add(&Start { translate: [0.0, 0.0, 4.0], ..Start::default() }).unwrap();
        for _ in 0..300 {
            world.step(1.0 / 60.0, 1);
        }
        let z = world.placements()[0].translate[2];
        assert!((z - 2.6).abs() < 0.03, "{z}");
    }

    #[test]
    fn the_angles_that_come_out_turn_a_copy_as_the_body_is_turned() {
        // A body thrown with a spin about a skew axis, read back through geo.instance's own
        // reading of the table: its matrix must be a rotation (no shear, no mirror).
        let mut rig = Rig::new("tz,wx,wy,wz\n1,200,-310,45");
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
