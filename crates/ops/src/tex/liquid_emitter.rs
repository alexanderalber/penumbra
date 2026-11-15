//! `tex.liquid_emitter`: one place that pours liquid, as a table, for the `emitter` inputs of
//! `tex.liquid`. Those inputs read any table by its column names ([`COLUMNS`]), so a hand-made
//! or computed table with the right columns works as well; such a table may hold many emitters,
//! one per row.
//!
//! Positions are shares of the picture from its top left, lengths picture heights, speeds
//! picture heights per second with y down, as everywhere in `tex.liquid`.

use penumbra_core::{Curve, Error, ParamSpec, Table, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, MAX_RUN, NodeDesc, NodeTypes, Operator,
};

pub(crate) const KIND: &str = "tex.liquid_emitter";

/// Most emitters one liquid takes, over all its inputs.
pub(crate) const MAX_EMITTERS: usize = 64;

/// Rows read per table: a table of far more would cost its time every frame.
const MAX_ROWS: usize = 4096;

/// Largest position, as a share of the picture, either way.
const MAX_POSITION: f64 = 1000.0;

/// Widest emitter, in picture heights.
pub(crate) const MAX_WIDTH: f64 = 10.0;

/// Fastest pour, in picture heights per second.
pub(crate) const MAX_SPEED: f64 = 100.0;

/// The materials an emitter pours, by name; the index is the material's number in a table.
pub(crate) const MATERIALS: [&str; 3] = ["liquid", "sand", "snow"];

/// The column names of an emitter table, as `tex.liquid_emitter` writes them.
pub(crate) const COLUMNS: [&str; 7] = ["x", "y", "width", "vx", "vy", "on", "material"];

/// One emitter, its numbers within what the liquid takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Emitter {
    /// Where its middle is: shares of the picture's width and height from the top left.
    pub position: [f64; 2],
    /// How wide the stream is, in picture heights.
    pub width: f64,
    /// Which way and how fast it pours, in picture heights per second, y down.
    pub velocity: [f64; 2],
    pub on: bool,
    /// Index into [`MATERIALS`].
    pub material: usize,
}

/// `tex.liquid_emitter`'s defaults, and what stands in for a column a table does not have.
pub(crate) const DEFAULT: Emitter =
    Emitter { position: [0.3, 0.1], width: 0.04, velocity: [0.0, 0.5], on: true, material: 0 };

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "A place that pours liquid, sand or snow into tex.liquid: where, how wide, which way and how fast",
    )
    .param(ParamSpec::vec2("position", DEFAULT.position).range(0.0, 1.0).doc(
        "Middle of the stream where it leaves the emitter, as shares of the picture: 0, 0 is the top left, \
             1, 1 the bottom right",
    ))
    .param(
        ParamSpec::float("width", DEFAULT.width)
            .clamp(0.0, MAX_WIDTH)
            .range(0.0, 0.3)
            .curve(Curve::Power(2.0))
            .doc("How wide the stream is, in picture heights. 0 pours a stream one particle wide"),
    )
    .param(ParamSpec::vec2("direction", [0.0, 1.0]).clamp(-1.0, 1.0).doc(
        "The way the stream leaves: x right, y down, so 0, 1 pours straight down. Only its direction counts",
    ))
    .param(ParamSpec::float("speed", 0.5).clamp(0.0, MAX_SPEED).range(0.0, 3.0).curve(Curve::Power(2.0)).doc(
        "How fast the stream leaves, in picture heights per second. The amount follows: width times \
                     speed is the area of liquid poured per second. 0 pours nothing",
    ))
    .param(ParamSpec::bool("on", true).doc("Pours while on; bind it to a channel to switch the pour"))
    .param(
        ParamSpec::choice("material", &MATERIALS)
            .doc("What the emitter pours. One tex.liquid can hold all three at once")
            .choice_doc("liquid", "water-like liquid that runs, splashes and pools")
            .choice_doc("sand", "dry sand that pours and forms heaps")
            .choice_doc("snow", "snow that clumps, piles up and breaks into lumps"),
    )
    .output("out", DataKind::Table)
    .describe_output(
        "out",
        "One row under a row of column names: x, y, width, vx, vy, on, material. x and y are shares of the \
             picture, width is in picture heights, vx and vy in picture heights per second",
    )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(EmitterNode) as Box<dyn Operator>))
}

/// The velocity of `speed` along `direction`, whose length does not count: none for a direction
/// of no length or a number that is not finite.
pub(crate) fn velocity(direction: [f64; 2], speed: f64) -> [f64; 2] {
    let len = direction[0].hypot(direction[1]);
    if !len.is_finite() || len <= 1e-9 || !speed.is_finite() {
        return [0.0, 0.0];
    }
    let speed = speed.clamp(0.0, MAX_SPEED);
    [speed * direction[0] / len, speed * direction[1] / len]
}

/// `e` as a table: the column names and one row. Numbers are written so they read back exactly.
fn table_of(e: &Emitter) -> Result<Table, Error> {
    let header = COLUMNS.iter().map(ToString::to_string).collect();
    let number = |v: f64| v.to_string();
    let row = vec![
        number(e.position[0]),
        number(e.position[1]),
        number(e.width),
        number(e.velocity[0]),
        number(e.velocity[1]),
        if e.on { "1".to_string() } else { "0".to_string() },
        MATERIALS.get(e.material).copied().unwrap_or("liquid").to_string(),
    ];
    Table::new(vec![header, row])
}

struct EmitterNode;

impl Operator for EmitterNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let vec2 = |name: &str| match cx.param(name) {
            Some(Value::Vec2(v)) => *v,
            _ => [0.0; 2],
        };
        let material = cx.text("material")?;
        let emitter = Emitter {
            position: vec2("position")
                .map(|v| if v.is_finite() { v.clamp(-MAX_POSITION, MAX_POSITION) } else { 0.0 }),
            width: cx.float("width")?,
            velocity: velocity(vec2("direction"), cx.float("speed")?),
            on: cx.bool("on")?,
            material: MATERIALS.iter().position(|m| *m == material).unwrap_or(0),
        };
        let table = table_of(&emitter).map_err(|e| CookError::failed(e.to_string()))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
    }
}

/// The emitters of a table whose first row names the columns ([`COLUMNS`]), up to `room` of
/// them. A row with a cell that is no finite number, or a material that does not exist, is left
/// out; numbers beyond the bounds are brought within them. Columns other than x and y may be
/// missing: [`DEFAULT`] stands in for them. `Err` for a table with rows but no x and y column.
pub(crate) fn from_table(table: &Table, room: usize) -> Result<Vec<Emitter>, String> {
    let Some((header, rows)) = table.rows().split_first() else { return Ok(Vec::new()) };
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let [x, y, width, vx, vy, on, material] = COLUMNS.map(col);
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
            let material = match material {
                None => DEFAULT.material,
                Some(c) => {
                    let cell = row.get(c)?.trim().to_ascii_lowercase();
                    match MATERIALS.iter().position(|m| *m == cell) {
                        Some(i) => i,
                        None => cell.parse::<usize>().ok().filter(|i| *i < MATERIALS.len())?,
                    }
                }
            };
            let position = [value(row, Some(x), 0.0)?, value(row, Some(y), 0.0)?];
            let velocity = [value(row, vx, DEFAULT.velocity[0])?, value(row, vy, DEFAULT.velocity[1])?];
            Some(Emitter {
                position: position.map(|v| v.clamp(-MAX_POSITION, MAX_POSITION)),
                width: value(row, width, DEFAULT.width)?.clamp(0.0, MAX_WIDTH),
                velocity: velocity.map(|v| v.clamp(-MAX_SPEED, MAX_SPEED)),
                on: value(row, on, 1.0)? >= 0.5,
                material,
            })
        };
        if let Some(e) = read() {
            out.push(e);
        }
    }
    Ok(out)
}

/// The emitters on a node's run of table inputs from index `first`, named `prefix0` and on, in
/// order, at most [`MAX_EMITTERS`]. Warns about a table without the required columns and about
/// emitters beyond the limit.
pub(crate) fn from_run(cx: &CookContext<'_>, first: usize, prefix: &str) -> Vec<Emitter> {
    let mut all = Vec::new();
    for i in 0..MAX_RUN {
        let Some(table) = cx.input_as::<Table>(first + i) else { continue };
        match from_table(table, MAX_EMITTERS + 1 - all.len()) {
            Ok(found) => all.extend(found),
            Err(e) => cx.warn(format!("`{prefix}{i}`: {e}")),
        }
        if all.len() > MAX_EMITTERS {
            cx.warn(format!("more than {MAX_EMITTERS} emitters: the rest are left out"));
            all.truncate(MAX_EMITTERS);
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
            Emitter { position: [0.125, -0.3], width: 0.0, velocity: [1.5, -0.25], on: false, material: 0 },
        ] {
            let t = table_of(&e).unwrap();
            assert_eq!(t.num_rows(), 2);
            assert_eq!(from_table(&t, 64).unwrap(), vec![e]);
        }
    }

    #[test]
    fn direction_and_speed_make_the_velocity() {
        assert_eq!(velocity([0.0, 1.0], 0.5), [0.0, 0.5]);
        let v = velocity([3.0, -4.0], 2.0);
        assert!((v[0] - 1.2).abs() < 1e-12 && (v[1] + 1.6).abs() < 1e-12);
        assert_eq!(velocity([0.0, 0.0], 1.0), [0.0, 0.0]);
        assert_eq!(velocity([f64::NAN, 1.0], 1.0), [0.0, 0.0]);
        assert_eq!(velocity([0.0, 1.0], f64::INFINITY), [0.0, 0.0]);
        assert_eq!(velocity([0.0, 1.0], 1e9), [0.0, MAX_SPEED]);
    }

    #[test]
    fn missing_columns_take_the_defaults_and_a_table_without_x_or_y_is_refused() {
        let found = from_table(&table(&[&["y", "x"], &["0.2", "0.7"]]), 64).unwrap();
        assert_eq!(found, vec![Emitter { position: [0.7, 0.2], ..DEFAULT }]);
        assert!(from_table(&table(&[&["x", "vx"], &["1", "2"]]), 64).is_err());
        assert_eq!(from_table(&table(&[]), 64).unwrap(), vec![]);
        assert_eq!(from_table(&table(&[&["x", "y"]]), 64).unwrap(), vec![]);
    }

    #[test]
    fn bad_rows_are_left_out_and_absurd_numbers_come_within_bounds() {
        let t = table(&[
            &["X", " y ", "width", "vx", "vy", "on", "material"],
            &["0.5", "0.5", "0.1", "0", "1", "1", "Liquid"],
            &["nan", "0.5", "0.1", "0", "1", "1", "liquid"],
            &["0.5", "inf", "0.1", "0", "1", "1", "liquid"],
            &["0.5", "0.5", "0.1", "0", "1", "1", "lava"],
            &["0.5", "0.5", "words", "0", "1", "1", "liquid"],
            &["0.5", "0.5"],
            &["1e300", "-1e300", "-5", "1e300", "-1e300", "0.2", "0"],
        ]);
        let found = from_table(&t, 64).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].on);
        assert_eq!(found[1].position, [MAX_POSITION, -MAX_POSITION]);
        assert_eq!(found[1].width, 0.0);
        assert_eq!(found[1].velocity, [MAX_SPEED, -MAX_SPEED]);
        assert!(!found[1].on);
        assert_eq!(from_table(&t, 1).unwrap().len(), 1);
    }
}
