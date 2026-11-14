//! `physics.bodies`: one group of rigid bodies for `physics.rigid_bodies`. A table places the
//! bodies, a model says what they look like, and the node's params say who moves them, what
//! they collide as and what they are made of. Two more tables steer dynamic bodies while they
//! move, without starting them over: `forces` pushes each body, `targets` pulls each body to a
//! place by a spring. The group travels as one [`Bodies`] on a wire of its own kind.

use penumbra_core::{Curve, Error, ParamSpec, Table};
use penumbra_gpu::Geo;
use penumbra_gpu::geo::{Item, Mat4};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Mesh;
use penumbra_physics::{MAX_BODIES, MAX_PUSH, MAX_STIFFNESS, Material, Mode, Push, Shape, Spring, Start};

pub const KIND: &str = "physics.bodies";

/// Choices of `shape`.
pub const SHAPES: [&str; 3] = ["hull", "box", "sphere"];

/// Choices of `mode`, in the order of [`mode_of`].
pub const MODES: [&str; 3] = ["dynamic", "driven", "fixed"];

/// The columns of the `table` input, by header name: where a body starts (as `geo.instance`
/// reads them), how fast it moves (metres per second) and spins (degrees per second).
pub const START_COLUMNS: [&str; 15] =
    ["tx", "ty", "tz", "rx", "ry", "rz", "sx", "sy", "sz", "vx", "vy", "vz", "wx", "wy", "wz"];

/// The columns of the `forces` input, by header name: how fast a body gains speed along x, y
/// and z (metres per second squared) and spin about them (degrees per second squared).
pub const FORCE_COLUMNS: [&str; 6] = ["fx", "fy", "fz", "mx", "my", "mz"];

/// The columns of the `targets` input, by header name: the place a body is pulled to.
pub const TARGET_COLUMNS: [&str; 3] = ["tx", "ty", "tz"];

/// The inputs, by index.
const TABLE: usize = 0;
const MODEL: usize = 1;
const FORCES: usize = 2;
const TARGETS: usize = 3;

/// The payload of `bodies` data: one group of bodies as a solver takes it.
#[derive(Clone)]
pub struct Bodies {
    /// One per row of the table, in its order: where a dynamic body starts, and where a driven
    /// or fixed body is. Row k is body k.
    pub starts: Vec<Start>,
    /// Per row, whether it held a cell that is not a number. Such a cell counts as the
    /// column's default in `starts`. A driven or fixed body that exists stays as it is while
    /// its row is held.
    pub held: Vec<bool>,
    /// Who moves the bodies.
    pub mode: Mode,
    /// What every body is drawn as. Empty: a box of 1 m.
    pub model: Vec<Item>,
    /// What every body collides as, at scale 1.
    pub shape: Shape,
    pub material: Material,
    /// What pushes body k, by k, while the bodies are dynamic. May be shorter or longer than
    /// `starts`.
    pub pushes: Vec<Push>,
    /// Where body k is pulled to, by k, while the bodies are dynamic; none for a body that is
    /// not pulled. May be shorter or longer than `starts`.
    pub targets: Vec<Option<[f32; 3]>>,
    /// The spring that pulls the bodies to `targets`.
    pub spring: Spring,
}

impl Bodies {
    /// The name of the group's collision shape, as the `shape` param calls it.
    pub fn shape_name(&self) -> &'static str {
        match self.shape {
            Shape::Hull { .. } => "hull",
            Shape::Box { .. } => "box",
            Shape::Sphere { .. } => "sphere",
        }
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "One group of rigid bodies for physics.rigid_bodies: a table places them, a model shows them, the params say who moves them, what they collide as and what they are made of",
        )
        .param(
            ParamSpec::choice("mode", &MODES)
                .doc("Who moves these bodies. A switch puts them where the table has them")
                .choice_doc("dynamic", "the solver: the table is their start, then they fall and collide")
                .choice_doc("driven", "the table, frame by frame: each body goes where its row is now and pushes the others out of its way; nothing pushes it back")
                .choice_doc("fixed", "nobody: they stand where the table puts them and the others land on them; a change to the table puts them elsewhere at once"),
        )
        .param(
            ParamSpec::choice("shape", &SHAPES)
                .doc("What each body collides as, taken from the `model` input. Without a model: a box of 1 m")
                .choice_doc("hull", "the model with its dents filled in, as if wrapped in foil")
                .choice_doc("box", "the box around the model; faster, and stacks well")
                .choice_doc("sphere", "a ball as wide as the model's longest side; the fastest, and it rolls"),
        )
        .param(
            ParamSpec::float("friction", 0.5)
                .clamp(0.0, 10.0)
                .range(0.0, 2.0)
                .doc("How much these bodies grip: 0 slides like ice, 1 holds like rubber. Where two things touch, the mean of their two values counts"),
        )
        .param(
            ParamSpec::float("bounce", 0.0)
                .clamp(0.0, 1.0)
                .doc("How much of its speed a body keeps when it hits something: 0 lands dead, 1 bounces for ever. Where two things touch, the larger of their two values counts"),
        )
        .param(
            ParamSpec::float("density", 1.0)
                // What the solver takes (`penumbra_physics::DENSITY_RANGE`).
                .clamp(0.001, 10_000.0)
                .range(0.1, 10.0)
                .applies_when("mode", &["dynamic"])
                .doc("How heavy these bodies are for their size, compared with other groups. A body of density 10 pushes one of density 1 aside and is hardly slowed by it. Alone, a group falls the same at any density"),
        )
        .param(
            ParamSpec::float("pull", 1.0)
                .clamp(0.0, 1.0)
                .applies_when_wired("targets")
                .doc("How much of the spring to the `targets` acts: 1 holds the bodies at their targets, 0 lets them go and they fall. A pulled body feels that much less of gravity, so at 1 the figure stands where the targets are and does not sag. Bind it to a channel to form the figure and drop it"),
        )
        .param(
            ParamSpec::float("stiffness", f64::from(Spring::default().stiffness))
                .clamp(0.0, f64::from(MAX_STIFFNESS))
                .range(1.0, 400.0)
                .curve(Curve::Log)
                .applies_when_wired("targets")
                .doc("How hard the spring pulls: the speed a body gains per second for every metre it is away from its target. 40 brings a body home in about a second, 400 in a quarter of one, 4 lets it drift there in three"),
        )
        .param(
            ParamSpec::float("pull_damping", f64::from(Spring::default().damping))
                .clamp(0.0, 100.0)
                .range(0.0, 2.0)
                .applies_when_wired("targets")
                .doc("How much the spring brakes. At 1 a body arrives without swinging past its target. Below 1 it swings about the target before it rests, at 0 for ever. Above 1 it creeps. A pulled body loses its spin at the same rate"),
        )
        .input("table", DataKind::Table)
        .input("model", DataKind::Geo)
        .input("forces", DataKind::Table)
        .input("targets", DataKind::Table)
        .describe_input(
            "forces",
            "A push per body, read on every frame: row k pushes body k of the table. The first row names the columns: fx fy fz for the speed the body gains per second along x, y and z (metres per second squared), mx my mz for the spin it gains per second about them (degrees per second squared). A heavy body gains as much as a light one. A change here never puts the bodies back to their start. Bodies without a row are not pushed. Only with mode dynamic",
        )
        .describe_input(
            "targets",
            "A place per body that a spring pulls it to: row k pulls body k of the table. The first row names the columns tx ty tz, in metres. The bodies form the figure these places draw, and collide on their way. `pull` at 0 lets them go. A change here never puts the bodies back to their start. Bodies without a row are not pulled. Only with mode dynamic",
        )
        .describe_input(
            "table",
            "One row per body: row k is body k. The first row names the columns: tx ty tz to place, rx ry rz to turn (degrees), sx sy sz to scale, vx vy vz for a speed at the start (metres per second), wx wy wz for a spin (degrees per second). With mode dynamic a change puts the bodies of this group back to their start. With driven and fixed the bodies follow the table, and the speed and spin columns do nothing. A cell that is not a number counts as 0 (1 for a scale), and a driven or fixed body stays where it is while its row holds one",
        )
        .describe_input(
            "model",
            "What each body looks like, with its material, and what `shape` is taken from. A change to its form puts the bodies of this group back to their start",
        )
        .output("out", DataKind::Bodies)
        .describe_output("out", "The group, for a `bodies` input of physics.rigid_bodies"),
        || Box::new(Group::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// A model and where it stands: what tells one cook's geometry from the next.
pub(crate) type GeoKey = Vec<(u64, Mat4)>;

pub(crate) fn key(geo: Option<&Geo>) -> GeoKey {
    geo.map(|g| g.items.iter().map(|i| (i.model.id, i.world)).collect()).unwrap_or_default()
}

/// Keeps the shape it made last: a hull is costly, and the table may change every frame.
#[derive(Default)]
struct Group {
    shape: Option<((String, GeoKey), Shape, Option<String>)>,
}

impl Operator for Group {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let Some(table) = cx.input_as::<Table>(TABLE) else {
            return Err(if wired(TABLE) {
                CookError::Pending
            } else {
                CookError::failed("connect a table with one row per body")
            });
        };
        let geo = match cx.input_as::<Geo>(MODEL) {
            Some(g) => Some(g),
            None if wired(MODEL) => return Err(CookError::Pending),
            None => None,
        };
        // A table that steers: none while unwired, and the group waits for one on its way.
        let steering = |input: usize| match cx.input_as::<Table>(input) {
            Some(t) => Ok(Some(t)),
            None if wired(input) && !cx.input_stuck(input) => Err(CookError::Pending),
            None => Ok(None),
        };
        let (pushes, push_note) = steering(FORCES)?.map(pushes).unwrap_or_default();
        let (targets, target_note) = steering(TARGETS)?.map(targets).unwrap_or_default();
        let failed = [(FORCES, "forces"), (TARGETS, "targets")]
            .into_iter()
            .filter(|(input, _)| wired(*input) && cx.input_stuck(*input))
            .map(|(_, name)| format!("the node on `{name}` failed: the bodies go without it"));
        let shape_key = (cx.text("shape")?.to_string(), key(geo));
        let (shape, shape_note) = match &self.shape {
            Some((k, shape, note)) if *k == shape_key => (shape.clone(), note.clone()),
            _ => {
                let (shape, note) = shape_of(&shape_key.0, placed(geo));
                // A model that holds nothing a shape can be taken from is not the same as no
                // model: the bodies are drawn as it and collide as something else.
                let unseen = geo.is_some_and(|g| !g.items.is_empty()) && placed(geo).next().is_none();
                let note = note.or_else(|| unseen.then(|| COPIES_ONLY.to_string()));
                self.shape = Some((shape_key, shape.clone(), note.clone()));
                (shape, note)
            }
        };
        let (starts, held, table_note) = starts(table);
        let notes: Vec<String> = table_note
            .into_iter()
            .chain(shape_note)
            .chain(push_note)
            .chain(target_note)
            .chain(failed)
            .collect();
        if !notes.is_empty() {
            cx.warn(notes.join("; "));
        }
        let material = Material {
            friction: cx.float("friction")? as f32,
            bounce: cx.float("bounce")? as f32,
            density: cx.float("density")? as f32,
        };
        let spring = Spring {
            pull: cx.float("pull")? as f32,
            stiffness: cx.float("stiffness")? as f32,
            damping: cx.float("pull_damping")? as f32,
        };
        let model = geo.map(|g| g.items.clone()).unwrap_or_default();
        let mode = mode_of(cx.text("mode")?);
        Ok(Cooked::Outputs(vec![Data::new(
            DataKind::Bodies,
            Bodies { starts, held, mode, model, shape, material, pushes, targets, spring },
        )]))
    }
}

/// The note for a model made of nothing but copies placed by textures.
const COPIES_ONLY: &str = "the model holds only copies placed by textures (geo.tex_instance), whose places exist on the GPU only: the bodies collide as a box of 1 m";

/// The mode the `mode` param calls `name`.
pub fn mode_of(name: &str) -> Mode {
    match name {
        "driven" => Mode::Driven,
        "fixed" => Mode::Fixed,
        _ => Mode::Dynamic,
    }
}

/// The meshes of `geo` with where they stand. Copies drawn by textures are left out: where they
/// are is known on the GPU only.
pub(crate) fn placed(geo: Option<&Geo>) -> impl Iterator<Item = (&Mesh, &Mat4)> {
    let items = geo.map(|g| g.items.as_slice()).unwrap_or_default();
    items.iter().filter(|i| i.instances.is_none()).map(|i| (&i.model.mesh, &i.world))
}

/// `p` moved by `m` (model to world).
pub(crate) fn moved_by(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
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
        // A flat model has no hull. Its box does.
        _ => match penumbra_physics::check(Shape::Hull { points }) {
            Ok(hull) => (hull, None),
            Err(e) => (boxed, Some(format!("{e}: the bodies collide as the box around the model"))),
        },
    }
}

/// Where a table's header names `name`: the first cell that reads so, whatever its case.
fn column(header: &[String], name: &str) -> Option<usize> {
    header.iter().position(|h| h.trim().eq_ignore_ascii_case(name))
}

/// Where the bodies start: the first row of `table` names the columns (any of
/// [`START_COLUMNS`], in any order and case, others ignored), every further row is one body, up
/// to [`MAX_BODIES`] rows. A missing column or an empty cell is 0 (1 for scale).
///
/// Row k is body k, whatever its cells hold: a cell that is not a finite number counts as
/// empty, and its row is marked as held. Returns the starts, per row whether it is held, and a
/// note, if any.
pub fn starts(table: &Table) -> (Vec<Start>, Vec<bool>, Option<String>) {
    let Some((header, rows)) = table.rows().split_first() else { return (Vec::new(), Vec::new(), None) };
    let index = START_COLUMNS.map(|name| column(header, name));
    if index.iter().all(Option::is_none) {
        let note = format!("the first row names the columns: {}", START_COLUMNS.join(", "));
        return (Vec::new(), Vec::new(), Some(note));
    }
    let mut first_bad = None;
    let mut out = Vec::with_capacity(rows.len().min(MAX_BODIES));
    let mut held = Vec::with_capacity(rows.len().min(MAX_BODIES));
    for (r, row) in rows.iter().enumerate().take(MAX_BODIES) {
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
            first_bad.get_or_insert(r + 1);
        }
        held.push(bad);
        out.push(Start {
            translate: [v[0], v[1], v[2]],
            rotate_degrees: [v[3], v[4], v[5]],
            scale: [v[6], v[7], v[8]],
            velocity: [v[9], v[10], v[11]],
            spin: [v[12], v[13], v[14]],
        });
    }
    let bad = first_bad.map(|r| {
        format!(
            "row {r} holds a cell that is not a number: the cell counts as 0 (1 for a scale), and a driven or fixed body stays where it is"
        )
    });
    let cut =
        (rows.len() > MAX_BODIES).then(|| format!("more than {MAX_BODIES} rows: the rest are left out"));
    let note = match (bad, cut) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    };
    (out, held, note)
}

/// The numbers of the rows of `table` in the columns `names`, found by the header in its first
/// row: a missing column and an empty cell are 0. None for a row with a cell that is not a
/// finite number. Also a note about a header that names none of the columns and about the
/// first such row; `input` and `effect` word it.
fn rows_of<const N: usize>(
    table: &Table,
    names: &[&str; N],
    input: &str,
    effect: &str,
) -> (Vec<Option<[f32; N]>>, Option<String>) {
    let Some((header, rows)) = table.rows().split_first() else { return (Vec::new(), None) };
    let index = names.map(|name| column(header, name));
    if index.iter().all(Option::is_none) {
        let note = format!("the first row of `{input}` names the columns: {}", names.join(", "));
        return (Vec::new(), Some(note));
    }
    let mut note = None;
    let read = |(r, row): (usize, &Vec<String>)| {
        let mut bad = false;
        let values = index.map(|col| {
            let cell = col.and_then(|c| row.get(c)).map_or("", |c| c.trim());
            match cell.parse::<f32>() {
                _ if cell.is_empty() => 0.0,
                Ok(x) if x.is_finite() => x,
                _ => {
                    bad = true;
                    0.0
                }
            }
        });
        if bad {
            let r = r + 1;
            note.get_or_insert_with(|| {
                format!("row {r} of `{input}` holds a cell that is not a number: {effect}")
            });
        }
        (!bad).then_some(values)
    };
    let out = rows.iter().enumerate().take(MAX_BODIES).map(read).collect();
    (out, note)
}

/// What pushes each body: the rows of `table` under a header that names any of
/// [`FORCE_COLUMNS`]. A row that cannot be read pushes nothing, so the rows after it keep
/// their bodies. Returns the pushes and a note, if any.
pub fn pushes(table: &Table) -> (Vec<Push>, Option<String>) {
    let (rows, note) = rows_of(table, &FORCE_COLUMNS, "forces", "that body is not pushed");
    let most = |v: f32| v.clamp(-MAX_PUSH, MAX_PUSH);
    let push = |row: Option<[f32; 6]>| {
        let [fx, fy, fz, mx, my, mz] = row.unwrap_or_default().map(most);
        Push { push: [fx, fy, fz], twist: [mx, my, mz] }
    };
    (rows.into_iter().map(push).collect(), note)
}

/// Where each body is pulled to: the rows of `table` under a header that names any of
/// [`TARGET_COLUMNS`]. A row that cannot be read pulls nothing, so the rows after it keep
/// their bodies. Returns the targets and a note, if any.
pub fn targets(table: &Table) -> (Vec<Option<[f32; 3]>>, Option<String>) {
    rows_of(table, &TARGET_COLUMNS, "targets", "that body is not pulled")
}
