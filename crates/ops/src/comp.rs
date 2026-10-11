//! Components: `comp.base`, a node that holds nodes, and its input and output nodes per family
//! (`tex.in`, `tex.out`, `chan.in`, ...), which pass their input on unchanged and are where wires
//! cross the component's edge.

use penumbra_core::{Error, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, DataKind, EXPOSE, FnNodeType, NodeDesc, NodeTypes, Operator, PortRole,
};

/// The kind of a component.
pub const COMP_KIND: &str = "comp.base";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(COMP_KIND, "Holds nodes; its input and output nodes are its ports")
            .param(ParamSpec::text(EXPOSE, "").multiline().rarely_used().doc(
                "Inner params shown on the component, one per line: `name = node.param` or `node.param`",
            ))
            .container(),
        || Box::new(Holder) as Box<dyn Operator>,
    ))?;
    for (family, kind) in [("tex", DataKind::Tex), ("chan", DataKind::Chan), ("table", DataKind::Table)] {
        for (role, name, summary) in [
            (
                PortRole::In,
                "in",
                "An input of the component it sits in: what is wired to it outside comes out here",
            ),
            (
                PortRole::Out,
                "out",
                "An output of the component it sits in: what is wired into it here comes out there",
            ),
        ] {
            types.register(FnNodeType::new(
                NodeDesc::new(&format!("{family}.{name}"), summary)
                    .input("in", kind)
                    .output("out", kind)
                    .port(role),
                || Box::new(Pass) as Box<dyn Operator>,
            ))?;
        }
    }
    Ok(())
}

/// A component does not cook; its nodes do.
struct Holder;

impl Operator for Holder {
    fn cook(&mut self, _cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        Ok(Cooked::Outputs(Vec::new()))
    }
}

/// Passes the input on as it is (no copy).
struct Pass;

impl Operator for Pass {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        match cx.input(0) {
            Some(data) => Ok(Cooked::Outputs(vec![data.clone()])),
            None if cx.def().inputs.iter().any(Option::is_some) => Err(CookError::Pending),
            None => Err(CookError::failed("nothing is wired to this port")),
        }
    }
}
