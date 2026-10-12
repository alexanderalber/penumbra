//! Node types and helpers for this crate's tests.

use std::collections::BTreeMap;

use penumbra_core::{ParamSpec, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, PortRole,
};

use crate::content::{Content, NodeContent, PortRef};

struct Const;

impl Operator for Const {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let value = penumbra_core::Name::new("value")?;
        Ok(Cooked::Outputs(vec![Data::new(
            DataKind::Chan,
            penumbra_core::Chans::single(value, cx.float("value")?, 60.0)?,
        )]))
    }
}

fn register(t: &mut NodeTypes, desc: NodeDesc) {
    t.register(FnNodeType::new(desc, || Box::new(Const) as Box<dyn Operator>)).unwrap();
}

pub fn test_types() -> NodeTypes {
    let mut t = NodeTypes::new();
    register(
        &mut t,
        NodeDesc::new("test.const", "A number")
            .param(ParamSpec::float("value", 0.0))
            .output("out", DataKind::Chan),
    );
    register(
        &mut t,
        NodeDesc::new("test.add", "a + b")
            .input("a", DataKind::Chan)
            .input("b", DataKind::Chan)
            .output("out", DataKind::Chan)
            .output("neg", DataKind::Chan),
    );
    register(
        &mut t,
        NodeDesc::new("test.ref", "Names another node")
            .param(ParamSpec::node("node"))
            .output("out", DataKind::Chan),
    );
    register(&mut t, NodeDesc::new("test.group", "Holds nodes").container());
    register(
        &mut t,
        NodeDesc::new("test.comp", "A component")
            .param(ParamSpec::text(penumbra_graph::EXPOSE, "").multiline())
            .container(),
    );
    for (kind, role) in [("test.in", PortRole::In), ("test.out", PortRole::Out)] {
        register(
            &mut t,
            NodeDesc::new(kind, "A component port")
                .input("in", DataKind::Chan)
                .output("out", DataKind::Chan)
                .port(role),
        );
    }
    t
}

type Spec<'a> = (&'a str, &'a str, &'a [(&'a str, f64)], &'a [Option<(&'a str, u32)>], bool);

/// Content from `(path, kind, params, inputs, locked)` rows. Inputs of `test.add` are padded to
/// its two slots.
pub fn content(rows: &[Spec<'_>]) -> Content {
    let mut nodes = BTreeMap::new();
    for (path, kind, params, inputs, locked) in rows {
        let mut inputs: Vec<Option<PortRef>> =
            inputs.iter().map(|i| i.map(|(n, o)| PortRef { node: n.parse().unwrap(), output: o })).collect();
        if *kind == "test.add" {
            inputs.resize(2, None);
        }
        if *kind == "test.in" || *kind == "test.out" {
            inputs.resize(1, None);
        }
        nodes.insert(
            path.parse().unwrap(),
            NodeContent {
                kind: kind.to_string(),
                params: params.iter().map(|(n, v)| (n.parse().unwrap(), Value::Float(*v))).collect(),
                inputs,
                binds: BTreeMap::new(),
                locked: *locked,
            },
        );
    }
    Content { nodes, ..Content::default() }
}
