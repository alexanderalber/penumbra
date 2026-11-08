//! `vol.emitter`: one emitter as a table, for the `emitter` inputs of `vol.smoke`. Those inputs
//! read any table by its column names ([`COLUMNS`]), so a hand-made or computed table with the
//! right columns works as well; such a table may hold many emitters, one per row.

use penumbra_core::{Curve, Error, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDesc, NodeTypes, Operator,
};

pub(crate) const KIND: &str = "vol.emitter";

/// Most emitters one smoke takes, over all its inputs.
pub(crate) const MAX_EMITTERS: usize = 64;

/// Rows read per table: a table of far more would cost its time every frame.
const MAX_ROWS: usize = 4096;

/// Largest position in metres, any axis.
const MAX_POSITION: f64 = 1e6;

/// Most metres of a box emitter's size and of a sphere's radius.
pub(crate) const MAX_SIZE: f64 = 1e4;

/// Most metres per second and most of smoke, fuel and correction.
const MAX_AMOUNT: f64 = 1000.0;

/// The column names of an emitter table, as `vol.emitter` writes them.
pub(crate) const COLUMNS: [&str; 15] = [
    "x",
    "y",
    "z",
    "type",
    "size_x",
    "size_y",
    "size_z",
    "radius",
    "vx",
    "vy",
    "vz",
    "smoke",
    "temp",
    "fuel",
    "correction_rate",
];

/// One emitter, its numbers within what the smoke takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Emitter {
    pub position: [f64; 3],
    pub is_box: bool,
    pub size: [f64; 3],
    pub radius: f64,
    pub velocity: [f64; 3],
    pub smoke: f64,
    pub temp: f64,
    pub fuel: f64,
    pub correction_rate: f64,
}

/// `vol.emitter`'s defaults, and what stands in for a column a table does not have.
pub(crate) const DEFAULT: Emitter = Emitter {
    position: [-1.5, 0.0, 0.0],
    is_box: false,
    size: [0.5, 0.5, 0.5],
    radius: 0.3,
    velocity: [0.0, 0.8, 0.0],
    smoke: 0.5,
    temp: 2.0,
    fuel: 1.0,
    correction_rate: 0.5,
};

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "An emitter for vol.smoke: a place in the box that blows smoke, heat and fuel in")
        .param(
            ParamSpec::vec3("position", DEFAULT.position)
                .unit(Unit::Metres)
                .doc("Centre of the emitter in space, in the same metres as the box of vol.smoke"),
        )
        .param(
            ParamSpec::choice("type", &["sphere", "box"])
                .doc("Shape of the emitter")
                .choice_doc("sphere", "a ball of `radius`")
                .choice_doc("box", "a box of `size`"),
        )
        .param(
            ParamSpec::vec3("size", DEFAULT.size)
                .clamp(0.0, MAX_SIZE)
                .unit(Unit::Metres)
                .doc("Size of the box emitter along x, y and z"),
        )
        .param(
            ParamSpec::float("radius", DEFAULT.radius)
                .clamp(0.0, MAX_SIZE)
                .range(0.01, 1.0)
                .curve(Curve::Log)
                .unit(Unit::Metres)
                .doc("Radius of the sphere emitter; 0: off"),
        )
        .param(
            ParamSpec::vec3("linear_velocity", DEFAULT.velocity)
                .clamp(-MAX_AMOUNT, MAX_AMOUNT)
                .doc("Speed the emitter blows at, in metres per second along x, y and z"),
        )
        .param(
            ParamSpec::float("smoke", DEFAULT.smoke)
                .clamp(0.0, MAX_AMOUNT)
                .range(0.0, 2.0)
                .doc("Density of the smoke the emitter makes"),
        )
        .param(ParamSpec::float("temp", DEFAULT.temp).clamp(-MAX_AMOUNT, MAX_AMOUNT).range(0.0, 5.0).doc(
            "Temperature the emitter heats towards: hotter rises faster (`buoyancy` of vol.smoke). \
                 The smoke itself stays between -1 and 1, so above 1 it only heats up faster",
        ))
        .param(ParamSpec::float("fuel", DEFAULT.fuel).clamp(0.0, MAX_AMOUNT).range(0.0, 5.0).doc(
            "Fuel the emitter blows in with the smoke. Fuel hotter than `ignition_temp` of vol.smoke \
             burns: it heats, makes smoke and swells. 0: smoke without fire",
        ))
        .param(
            ParamSpec::float("correction_rate", DEFAULT.correction_rate)
                .clamp(0.0, MAX_AMOUNT)
                .range(0.0, 10.0)
                .doc(
                    "How fast the emitter takes the cells inside it to its own velocity, smoke, temperature \
             and fuel, as a share per second: 0.5 goes half of the way in a second. 60 or more sets \
             them every frame",
                ),
        )
        .output("out", DataKind::Table)
        .describe_output(
            "out",
            "One row under a row of column names: x, y, z, type, size_x, size_y, size_z, radius, vx, vy, \
             vz, smoke, temp, fuel, correction_rate",
        )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(EmitterNode) as Box<dyn Operator>))
}

fn vec3(cx: &CookContext<'_>, name: &str) -> [f64; 3] {
    match cx.param(name) {
        Some(Value::Vec3(v)) => *v,
        _ => [0.0; 3],
    }
}

/// `e` as a table: the column names and one row. Numbers are written so they read back exactly.
fn table_of(e: &Emitter) -> Result<Table, Error> {
    let header = COLUMNS.iter().map(ToString::to_string).collect();
    let number = |v: f64| v.to_string();
    let row = vec![
        number(e.position[0]),
        number(e.position[1]),
        number(e.position[2]),
        if e.is_box { "box".to_string() } else { "sphere".to_string() },
        number(e.size[0]),
        number(e.size[1]),
        number(e.size[2]),
        number(e.radius),
        number(e.velocity[0]),
        number(e.velocity[1]),
        number(e.velocity[2]),
        number(e.smoke),
        number(e.temp),
        number(e.fuel),
        number(e.correction_rate),
    ];
    Table::new(vec![header, row])
}

struct EmitterNode;

impl Operator for EmitterNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let emitter = Emitter {
            position: vec3(cx, "position"),
            is_box: cx.text("type")? == "box",
            size: vec3(cx, "size"),
            radius: cx.float("radius")?,
            velocity: vec3(cx, "linear_velocity"),
            smoke: cx.float("smoke")?,
            temp: cx.float("temp")?,
            fuel: cx.float("fuel")?,
            correction_rate: cx.float("correction_rate")?,
        };
        let table = table_of(&emitter).map_err(|e| CookError::failed(e.to_string()))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
    }
}

/// The emitters of a table whose first row names the columns ([`COLUMNS`]), up to `room` of
/// them. A row with a cell that is no finite number, or a type that is neither sphere nor box, is
/// left out; numbers beyond the params' bounds are brought within them. Columns other than x and
/// y may be missing: [`DEFAULT`] stands in for them, and z is 0. `Err` for a table with rows but
/// no x and y column.
pub(crate) fn from_table(table: &Table, room: usize) -> Result<Vec<Emitter>, String> {
    let Some((header, rows)) = table.rows().split_first() else { return Ok(Vec::new()) };
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let [x, y, z, kind, sx, sy, sz, radius, vx, vy, vz, smoke, temp, fuel, rate] = COLUMNS.map(col);
    let (Some(x), Some(y)) = (x, y) else {
        return Err("the table needs columns x and y in its first row".into());
    };
    let value = |row: &[String], c: Option<usize>, default: f64| -> Option<f64> {
        match c {
            None => Some(default),
            Some(c) => row.get(c)?.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
        }
    };
    let mut out = Vec::new();
    for row in rows.iter().take(MAX_ROWS) {
        if out.len() >= room {
            break;
        }
        let read = || -> Option<Emitter> {
            let is_box = match kind {
                None => DEFAULT.is_box,
                Some(c) => match row.get(c)?.trim().to_ascii_lowercase().as_str() {
                    "box" | "1" => true,
                    "sphere" | "0" => false,
                    _ => return None,
                },
            };
            let position = [value(row, Some(x), 0.0)?, value(row, Some(y), 0.0)?, value(row, z, 0.0)?];
            let size = [
                value(row, sx, DEFAULT.size[0])?,
                value(row, sy, DEFAULT.size[1])?,
                value(row, sz, DEFAULT.size[2])?,
            ];
            let velocity = [
                value(row, vx, DEFAULT.velocity[0])?,
                value(row, vy, DEFAULT.velocity[1])?,
                value(row, vz, DEFAULT.velocity[2])?,
            ];
            Some(Emitter {
                position: position.map(|v| v.clamp(-MAX_POSITION, MAX_POSITION)),
                is_box,
                size: size.map(|v| v.clamp(0.0, MAX_SIZE)),
                radius: value(row, radius, DEFAULT.radius)?.clamp(0.0, MAX_SIZE),
                velocity: velocity.map(|v| v.clamp(-MAX_AMOUNT, MAX_AMOUNT)),
                smoke: value(row, smoke, DEFAULT.smoke)?.clamp(0.0, MAX_AMOUNT),
                temp: value(row, temp, DEFAULT.temp)?.clamp(-MAX_AMOUNT, MAX_AMOUNT),
                fuel: value(row, fuel, DEFAULT.fuel)?.clamp(0.0, MAX_AMOUNT),
                correction_rate: value(row, rate, DEFAULT.correction_rate)?.clamp(0.0, MAX_AMOUNT),
            })
        };
        if let Some(e) = read() {
            out.push(e);
        }
    }
    Ok(out)
}

/// The rows on a node's run of table inputs from index `first`, named `prefix0` and on, in order,
/// at most `limit`. Warns about a table without the required columns and about `noun` beyond the
/// limit.
pub(crate) fn from_run(
    cx: &CookContext<'_>,
    first: usize,
    prefix: &str,
    noun: &str,
    limit: usize,
) -> Vec<Emitter> {
    let mut all = Vec::new();
    for i in 0..MAX_RUN {
        let Some(table) = cx.input_as::<Table>(first + i) else { continue };
        match from_table(table, limit + 1 - all.len()) {
            Ok(found) => all.extend(found),
            Err(e) => cx.warn(format!("`{prefix}{i}`: {e}")),
        }
        if all.len() > limit {
            cx.warn(format!("more than {limit} {noun}: the rest are left out"));
            all.truncate(limit);
            break;
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[&[&str]]) -> Table {
        Table::new(rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap()
    }

    #[test]
    fn the_node_table_reads_back_as_its_emitter() {
        for e in [
            DEFAULT,
            Emitter {
                position: [1.25, -0.1, 0.123456789012345],
                is_box: true,
                size: [1.0, 0.25, 7.5],
                radius: 0.0,
                velocity: [4.5, -1000.0, 1e-7],
                smoke: 3.0,
                temp: -2.65,
                fuel: 0.0,
                correction_rate: 60.0,
            },
        ] {
            let t = table_of(&e).unwrap();
            assert_eq!(t.num_rows(), 2);
            assert_eq!(from_table(&t, 64).unwrap(), vec![e]);
        }
    }

    #[test]
    fn missing_columns_take_the_defaults_and_a_table_without_x_or_y_is_refused() {
        let found = from_table(&table(&[&["y", "x"], &["2", "1"]]), 64).unwrap();
        assert_eq!(found, vec![Emitter { position: [1.0, 2.0, 0.0], ..DEFAULT }]);
        assert!(from_table(&table(&[&["x", "z"], &["1", "2"]]), 64).is_err());
        assert!(from_table(&table(&[&["y", "z"], &["1", "2"]]), 64).is_err());
        // No rows: nothing to complain about.
        assert_eq!(from_table(&table(&[]), 64).unwrap(), vec![]);
        assert_eq!(from_table(&table(&[&["a", "b"]]), 64).unwrap(), vec![]);
    }

    #[test]
    fn bad_rows_are_left_out_and_absurd_numbers_come_within_bounds() {
        let t = table(&[
            &["X", " y ", "type", "radius", "vx", "smoke", "correction_rate"],
            &["0", "0", "box", "0.1", "1", "1", "1"],
            &["nan", "0", "box", "0.1", "1", "1", "1"],
            &["0", "inf", "sphere", "0.1", "1", "1", "1"],
            &["0", "0", "pyramid", "0.1", "1", "1", "1"],
            &["0", "0", "box", "words", "1", "1", "1"],
            &["0", "0"],
            &["1e300", "-1e300", "sphere", "-5", "1e300", "-3", "1e300"],
        ]);
        let found = from_table(&t, 64).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].is_box);
        assert_eq!(found[1].position, [MAX_POSITION, -MAX_POSITION, 0.0]);
        assert_eq!(found[1].radius, 0.0);
        assert_eq!(found[1].velocity, [MAX_AMOUNT, DEFAULT.velocity[1], DEFAULT.velocity[2]]);
        assert_eq!(found[1].smoke, 0.0);
        assert_eq!(found[1].correction_rate, MAX_AMOUNT);
    }

    #[test]
    fn a_table_is_read_only_as_far_as_there_is_room() {
        let t = table(&[&["x", "y"], &["0", "0"], &["1", "1"], &["2", "2"]]);
        assert_eq!(from_table(&t, 2).unwrap().len(), 2);
    }
}
