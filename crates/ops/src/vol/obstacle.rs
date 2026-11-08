//! `vol.obstacle`: one still body as a table, for the `obstacle` inputs of `vol.smoke`. Those
//! inputs read the place and shape columns of any table ([`COLUMNS`]), so one table can hold many
//! obstacles, one per row, and an emitter table works as well.

use penumbra_core::{Curve, Error, ParamSpec, Table, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::emitter::MAX_SIZE;

pub(crate) const KIND: &str = "vol.obstacle";

/// Most obstacles one smoke takes, over all its inputs.
pub(crate) const MAX_OBSTACLES: usize = 64;

/// The column names of an obstacle table, as `vol.obstacle` writes them.
const COLUMNS: [&str; 8] = ["x", "y", "z", "type", "size_x", "size_y", "size_z", "radius"];

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "A still body for vol.smoke: the smoke flows around it")
        .param(
            ParamSpec::vec3("position", [0.0, 0.0, 0.0])
                .unit(Unit::Metres)
                .doc("Centre of the obstacle in space, in the same metres as the box of vol.smoke"),
        )
        .param(
            ParamSpec::choice("type", &["sphere", "box"])
                .doc("Shape of the obstacle")
                .choice_doc("sphere", "a ball of `radius`")
                .choice_doc("box", "a box of `size`"),
        )
        .param(ParamSpec::vec3("size", [0.5, 0.5, 0.5]).clamp(0.0, MAX_SIZE).unit(Unit::Metres).doc(
            "Size of the box obstacle along x, y and z. Thinner than a cell of the smoke, it can do nothing",
        ))
        .param(
            ParamSpec::float("radius", 0.3)
                .clamp(0.0, MAX_SIZE)
                .range(0.01, 1.0)
                .curve(Curve::Log)
                .unit(Unit::Metres)
                .doc("Radius of the sphere obstacle; 0: off"),
        )
        .output("out", DataKind::Table)
        .describe_output(
            "out",
            "One row under a row of column names: x, y, z, type, size_x, size_y, size_z, radius",
        )
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(ObstacleNode) as Box<dyn Operator>))
}

fn vec3(cx: &CookContext<'_>, name: &str) -> [f64; 3] {
    match cx.param(name) {
        Some(Value::Vec3(v)) => *v,
        _ => [0.0; 3],
    }
}

/// The column names and one row. Numbers are written so they read back exactly.
fn table_of(position: [f64; 3], is_box: bool, size: [f64; 3], radius: f64) -> Result<Table, Error> {
    let header = COLUMNS.iter().map(ToString::to_string).collect();
    let row = vec![
        position[0].to_string(),
        position[1].to_string(),
        position[2].to_string(),
        if is_box { "box".to_string() } else { "sphere".to_string() },
        size[0].to_string(),
        size[1].to_string(),
        size[2].to_string(),
        radius.to_string(),
    ];
    Table::new(vec![header, row])
}

struct ObstacleNode;

impl Operator for ObstacleNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let table =
            table_of(vec3(cx, "position"), cx.text("type")? == "box", vec3(cx, "size"), cx.float("radius")?)
                .map_err(|e| CookError::failed(e.to_string()))?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Table, table)]))
    }
}

#[cfg(test)]
mod tests {
    use super::super::emitter::from_table;
    use super::*;

    #[test]
    fn the_node_table_reads_back_as_its_shape() {
        let t = table_of([1.25, -0.1, 0.5], true, [1.0, 0.25, 7.5], 0.75).unwrap();
        let found = from_table(&t, MAX_OBSTACLES).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].position, [1.25, -0.1, 0.5]);
        assert!(found[0].is_box);
        assert_eq!(found[0].size, [1.0, 0.25, 7.5]);
        assert_eq!(found[0].radius, 0.75);
    }
}
