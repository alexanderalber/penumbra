//! `physics.field`: one field that moves bodies, as a table, for the `field` inputs of
//! `physics.rigid_bodies`. Those inputs read any table by its column names ([`COLUMNS`]), so a
//! table made by hand or from channels works as well; such a table may hold many fields, one
//! per row.

use penumbra_core::{Error, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_physics::{Falloff, Field, FieldKind, MAX_KICK, MAX_PUSH};

pub const KIND: &str = "physics.field";

/// Choices of `type`, in the order a table may number them from 0.
pub const TYPES: [&str; 3] = ["point", "wind", "vortex"];

/// Choices of `falloff`, in the order a table may number them from 0.
pub const FALLOFFS: [&str; 2] = ["none", "linear"];

/// The column names of a table of fields, as `physics.field` writes them.
pub const COLUMNS: [&str; 12] =
    ["type", "strength", "tx", "ty", "tz", "dx", "dy", "dz", "radius", "falloff", "kick", "trigger"];

/// Rows read per table: a table of far more would cost its time every frame.
const MAX_ROWS: usize = 4096;

/// Largest place in metres, any axis, and largest radius.
const MAX_PLACE: f64 = 1e6;

/// `trigger` counts as on above this.
const TRIGGER_ON: f64 = 0.5;

/// One row of a table of fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    /// The field as it acts all the time; `strength` in metres per second squared.
    pub field: Field,
    /// The change of speed the field gives when `trigger` turns on, in metres per second.
    pub kick: f32,
    pub trigger: bool,
}

/// `physics.field`'s defaults, and what stands in for a column a table does not have.
const DEFAULT: Row = Row {
    field: Field {
        kind: FieldKind::Point,
        strength: 10.0,
        position: [0.0; 3],
        direction: [0.0, 0.0, 1.0],
        radius: 0.0,
        falloff: Falloff::None,
    },
    kick: 0.0,
    trigger: false,
};

fn desc() -> NodeDesc {
    let most = f64::from(MAX_PUSH);
    let d = DEFAULT.field;
    NodeDesc::new(
        KIND,
        "A field that moves the bodies of a physics.rigid_bodies: a pull to a point, a wind or a whirl that speeds up every dynamic body it reaches, and a kick on a trigger",
    )
    .param(
        ParamSpec::choice("type", &TYPES)
            .doc("What the field does to the bodies")
            .choice_doc("point", "pulls them towards `position`; a negative `strength` pushes them away from it")
            .choice_doc("wind", "blows them along `direction`")
            .choice_doc("vortex", "drives them round the axis that runs through `position` along `direction`, counterclockwise seen from where `direction` points to. Nothing pulls them inwards, so they drift outwards: add a point field to hold them"),
    )
    .param(
        ParamSpec::float("strength", f64::from(d.strength))
            .clamp(-most, most)
            .range(-30.0, 30.0)
            .doc("How fast the field speeds a body up, in metres per second squared. Gravity is 9.81, so a wind of 10 straight up just lifts the bodies. A large body gains speed as fast as a small one, and a heavy one as fast as a light one. Negative turns the field round. 0: off"),
    )
    .param(
        ParamSpec::vec3("position", d.position.map(f64::from))
            .clamp(-MAX_PLACE, MAX_PLACE)
            .unit(Unit::Metres)
            .doc("The middle of the field: the point that pulls, a point on the axis of a vortex, the middle of a wind with a `radius`. A wind without a radius does not use it"),
    )
    .param(
        ParamSpec::vec3("direction", d.direction.map(f64::from))
            .clamp(-1e3, 1e3)
            .range(-1.0, 1.0)
            .applies_when("type", &["wind", "vortex"])
            .doc("Where a wind blows, and the axis of a vortex. Only the direction counts: 0 0 2 blows up as hard as 0 0 1. 0 0 0 blows nowhere"),
    )
    .param(
        ParamSpec::float("radius", f64::from(d.radius))
            .clamp(0.0, MAX_PLACE)
            .range(0.0, 10.0)
            .unit(Unit::Metres)
            .special(0.0, "everywhere")
            .doc("How far the field reaches from `position`, for a vortex from its axis. Bodies farther away feel nothing. 0: the field reaches everywhere"),
    )
    .param(
        ParamSpec::choice("falloff", &FALLOFFS)
            .doc("How the field weakens between its middle and its `radius`. Without a radius it does nothing")
            .choice_doc("none", "full strength up to the radius, nothing beyond")
            .choice_doc("linear", "full strength at the middle, half of it half way out, nothing at the radius"),
    )
    .param(
        ParamSpec::float("kick", f64::from(DEFAULT.kick))
            .clamp(-f64::from(MAX_KICK), f64::from(MAX_KICK))
            .range(-20.0, 20.0)
            .doc("A change of speed, in metres per second, that the field gives every body it reaches at the moment `trigger` turns on: a blast away from a point (negative), a gust, a twirl. It acts once per trigger, besides `strength`. For a kick alone set `strength` to 0"),
    )
    .param(
        ParamSpec::float("trigger", 0.0)
            .range(0.0, 1.0)
            .doc("Each time it rises past 0.5 the field gives its `kick`. Bind it to a key's gate, a beat or a chan.envelope"),
    )
    .output("out", DataKind::Table)
    .describe_output(
        "out",
        "One row under a row of column names: type, strength, tx, ty, tz, dx, dy, dz, radius, falloff, kick, trigger. Wire it into a `field` input of physics.rigid_bodies",
    )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(FieldNode) as Box<dyn Operator>))
}

fn vec3(cx: &CookContext<'_>, name: &str) -> [f32; 3] {
    match cx.param(name) {
        Some(Value::Vec3(v)) => v.map(|x| x as f32),
        _ => [0.0; 3],
    }
}

fn kind_of(name: &str) -> Option<FieldKind> {
    match name {
        "point" | "0" => Some(FieldKind::Point),
        "wind" | "1" => Some(FieldKind::Wind),
        "vortex" | "2" => Some(FieldKind::Vortex),
        _ => None,
    }
}

fn falloff_of(name: &str) -> Option<Falloff> {
    match name {
        "none" | "0" => Some(Falloff::None),
        "linear" | "1" => Some(Falloff::Linear),
        _ => None,
    }
}

/// `row` as a table: the column names and one row. Numbers are written so they read back exactly.
fn table_of(row: &Row) -> Result<Table, Error> {
    let f = row.field;
    let kind = match f.kind {
        FieldKind::Point => "point",
        FieldKind::Wind => "wind",
        FieldKind::Vortex => "vortex",
    };
    let falloff = match f.falloff {
        Falloff::None => "none",
        Falloff::Linear => "linear",
    };
    let mut cells = vec![kind.to_string(), f.strength.to_string()];
    cells.extend(f.position.iter().chain(&f.direction).map(ToString::to_string));
    cells.extend([f.radius.to_string(), falloff.to_string(), row.kick.to_string()]);
    cells.push(if row.trigger { "1" } else { "0" }.to_string());
    Table::new(vec![COLUMNS.iter().map(ToString::to_string).collect(), cells])
}

struct FieldNode;

impl Operator for FieldNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let row = Row {
            field: Field {
                kind: kind_of(cx.text("type")?).unwrap_or(DEFAULT.field.kind),
                strength: cx.float("strength")? as f32,
                position: vec3(cx, "position"),
                direction: vec3(cx, "direction"),
                radius: cx.float("radius")? as f32,
                falloff: falloff_of(cx.text("falloff")?).unwrap_or(DEFAULT.field.falloff),
            },
            kick: cx.float("kick")? as f32,
            trigger: cx.float("trigger")? > TRIGGER_ON,
        };
        let table = table_of(&row).map_err(|e| CookError::failed(e.to_string()))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
    }
}

/// The fields of a table whose first row names the columns ([`COLUMNS`]), up to `room`
/// of them, each with the number of its row (0 for the first under the header), and how many
/// rows were left out: those with a cell that is no finite number, or with a type or falloff
/// nobody knows. Numbers beyond the params' bounds are brought within them. A column that is
/// missing takes `physics.field`'s default; `type` and `falloff` may be words or the number of
/// the choice. `Err` for a table with rows but neither a `strength` nor a `kick` column: it is
/// some other table, wired here by mistake.
pub fn from_table(table: &Table, room: usize) -> Result<(Vec<(usize, Row)>, usize), String> {
    let Some((header, rows)) = table.rows().split_first() else { return Ok((Vec::new(), 0)) };
    if rows.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let [kind, strength, tx, ty, tz, dx, dy, dz, radius, falloff, kick, trigger] = COLUMNS.map(col);
    if strength.is_none() && kick.is_none() {
        return Err("the table needs a `strength` or a `kick` column in its first row".into());
    }
    let number = |row: &[String], c: Option<usize>, default: f64| -> Option<f64> {
        match c.map(|c| row.get(c).map_or("", |cell| cell.trim())) {
            // A cell left empty is the column's default, as a column left out is.
            None | Some("") => Some(default),
            Some(cell) => cell.parse::<f64>().ok().filter(|v| v.is_finite()),
        }
    };
    // A choice written as a word, or as its number the way a channel carries it.
    let word = |row: &[String], c: Option<usize>| -> Option<String> {
        let cell = row.get(c?)?.trim().to_ascii_lowercase();
        Some(cell.parse::<f64>().map_or(cell.clone(), |n| format!("{n}")))
    };
    let (push, most_kick) = (f64::from(MAX_PUSH), f64::from(MAX_KICK));
    let d = DEFAULT.field;
    let mut out = Vec::new();
    let mut left_out = 0;
    for (line, row) in rows.iter().enumerate().take(MAX_ROWS) {
        if out.len() >= room {
            break;
        }
        let read = || -> Option<Row> {
            let kind = match word(row, kind).as_deref() {
                None | Some("") => d.kind,
                Some(name) => kind_of(name)?,
            };
            let falloff = match word(row, falloff).as_deref() {
                None | Some("") => d.falloff,
                Some(name) => falloff_of(name)?,
            };
            let place = [number(row, tx, 0.0)?, number(row, ty, 0.0)?, number(row, tz, 0.0)?];
            // A direction given in part points along the axes it names.
            let along = dx.or(dy).or(dz).map_or(d.direction.map(f64::from), |_| [0.0; 3]);
            let [ax, ay, az] = along;
            let direction = [number(row, dx, ax)?, number(row, dy, ay)?, number(row, dz, az)?];
            Some(Row {
                field: Field {
                    kind,
                    strength: number(row, strength, 0.0)?.clamp(-push, push) as f32,
                    position: place.map(|v| v.clamp(-MAX_PLACE, MAX_PLACE) as f32),
                    direction: direction.map(|v| v.clamp(-1e3, 1e3) as f32),
                    radius: number(row, radius, f64::from(d.radius))?.clamp(0.0, MAX_PLACE) as f32,
                    falloff,
                },
                kick: number(row, kick, 0.0)?.clamp(-most_kick, most_kick) as f32,
                trigger: number(row, trigger, 0.0)? > TRIGGER_ON,
            })
        };
        match read() {
            Some(row) => out.push((line, row)),
            None => left_out += 1,
        }
    }
    Ok((out, left_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fields of `table` without their row numbers.
    fn fields(table: &Table, room: usize) -> Result<(Vec<Row>, usize), String> {
        from_table(table, room)
            .map(|(rows, left_out)| (rows.into_iter().map(|(_, row)| row).collect(), left_out))
    }

    fn table(rows: &[&[&str]]) -> Table {
        Table::new(rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap()
    }

    #[test]
    fn the_node_table_reads_back_as_its_field() {
        let other = Row {
            field: Field {
                kind: FieldKind::Vortex,
                strength: -3.25,
                position: [1.5, -0.1, 0.123_456_79],
                direction: [0.0, 1.0, -1e-7],
                radius: 2.5,
                falloff: Falloff::Linear,
            },
            kick: 7.5,
            trigger: true,
        };
        for row in [DEFAULT, other] {
            let t = table_of(&row).unwrap();
            assert_eq!((t.num_rows(), t.rows()[0].len()), (2, COLUMNS.len()));
            assert_eq!(fields(&t, 64).unwrap(), (vec![row], 0));
        }
    }

    #[test]
    fn missing_columns_take_the_defaults_and_a_table_of_something_else_is_refused() {
        // A table of attractors made from a tracker's channels: numbers only.
        let t = table(&[&["tx", "ty", "strength", "type"], &["1", "2", "5", "0"], &["3", "", "-5", "1"]]);
        let (rows, left_out) = fields(&t, 64).unwrap();
        assert_eq!(left_out, 0);
        let first = Field { strength: 5.0, position: [1.0, 2.0, 0.0], ..DEFAULT.field };
        assert_eq!(rows[0], Row { field: first, ..DEFAULT });
        assert_eq!((rows[1].field.kind, rows[1].field.position), (FieldKind::Wind, [3.0, 0.0, 0.0]));
        assert_eq!(rows[1].field.direction, [0.0, 0.0, 1.0], "the default: up");
        // Only `dx`: along x, and nothing of the default's z.
        let t = table(&[&["strength", "dx", "type"], &["5", "-2", "wind"]]);
        assert_eq!(fields(&t, 64).unwrap().0[0].field.direction, [-2.0, 0.0, 0.0]);
        // A kick alone has no steady strength.
        let t = table(&[&["Kick", " trigger "], &["4", "1"], &["4", "0.5"]]);
        let (rows, _) = fields(&t, 64).unwrap();
        assert_eq!((rows[0].field.strength, rows[0].kick, rows[0].trigger), (0.0, 4.0, true));
        assert!(!rows[1].trigger, "on above 0.5");
        // The table of a group of bodies is no table of fields.
        assert!(fields(&table(&[&["tx", "ty", "tz"], &["1", "2", "3"]]), 64).is_err());
        // No rows: nothing to complain about.
        assert_eq!(fields(&table(&[]), 64).unwrap(), (vec![], 0));
        assert_eq!(fields(&table(&[&["a", "b"]]), 64).unwrap(), (vec![], 0));
    }

    #[test]
    fn bad_rows_are_left_out_and_absurd_numbers_come_within_bounds() {
        let t = table(&[
            &["strength", "tx", "type", "radius", "falloff", "kick"],
            &["1", "0", "WIND", "1", "Linear", "0"],
            &["nan", "0", "wind", "1", "none", "0"],
            &["1", "inf", "wind", "1", "none", "0"],
            &["1", "0", "storm", "1", "none", "0"],
            &["1", "0", "wind", "1", "steep", "0"],
            &["1", "0", "3", "1", "none", "0"],
            &["1", "0", "wind", "wide", "none", "0"],
            &["1e300", "-1e300", "2", "-5", "1", "-1e300"],
            &["2"],
        ]);
        let (rows, left_out) = fields(&t, 64).unwrap();
        assert_eq!((rows.len(), left_out), (3, 6), "{rows:?}");
        assert_eq!((rows[0].field.kind, rows[0].field.falloff), (FieldKind::Wind, Falloff::Linear));
        let huge = rows[1];
        assert_eq!((huge.field.strength, huge.field.position[0]), (MAX_PUSH, -1e6));
        assert_eq!((huge.field.radius, huge.kick), (0.0, -MAX_KICK));
        assert_eq!((huge.field.kind, huge.field.falloff), (FieldKind::Vortex, Falloff::Linear));
        assert_eq!(
            rows[2],
            Row { field: Field { strength: 2.0, ..DEFAULT.field }, ..DEFAULT },
            "a short row"
        );
        // A table is read only as far as there is room.
        assert_eq!(fields(&t, 2).unwrap().0.len(), 2);
        // Each field comes with the number of its row, the rows left out counted.
        let lines: Vec<usize> = from_table(&t, 64).unwrap().0.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [0, 7, 8]);
    }
}
