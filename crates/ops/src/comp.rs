//! Components: `comp.base`, a node that holds nodes, and its input and output nodes per data kind
//! (`tex.in`, `tex.out`, `chan.in`, `geo.out`, ...), which pass their input on unchanged and are
//! where wires cross the component's edge. Every data kind has its pair, so grouping nodes never
//! fails on the kind of a wire that crosses the new edge.

use penumbra_core::{Error, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, PARAMS, PortRole,
};

/// The kind of a component.
pub const COMP_KIND: &str = "comp.base";
/// A component's param naming its place in the library.
pub const SHELF: &str = "shelf";
/// A component's param saying in one line what it does.
pub const ABOUT: &str = "about";

/// The families with component ports, one per data kind.
const PORT_FAMILIES: [(&str, DataKind); 9] = [
    ("tex", DataKind::Tex),
    ("chan", DataKind::Chan),
    ("audio", DataKind::Audio),
    ("table", DataKind::Table),
    ("geo", DataKind::Geo),
    ("mat", DataKind::Mat),
    ("light", DataKind::Light),
    ("view", DataKind::View),
    ("vol", DataKind::Vol),
];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(COMP_KIND, "A box of nodes: groups part of a network into one node, with inputs and outputs of its own (the in and out nodes inside)")
            .param(ParamSpec::text(PARAMS, "").multiline().more().doc(
                "The component's own params, one per line. `@param speed float = 1 0..4 -- doc` adds a param with its own value, in the grammar of a shader header. `@expose rate = lfo1.rate` shows an inner param on the component. `@group look` puts the params after it in a fold",
            ))
            .param(ParamSpec::text(SHELF, "").more().doc(
                "Where this component stands in the add dialog's library tab: one or two levels, `dmx` or `dmx/safety`. Empty: only your own components' shelf `mine` shows it",
            ))
            .param(ParamSpec::text(ABOUT, "").more().doc(
                "One line on what this component does. The add dialog shows it under the pointer, and its search finds it",
            ))
            .container(),
        || Box::new(Holder) as Box<dyn Operator>,
    ))?;
    for (family, kind) in PORT_FAMILIES {
        for (role, name, summary) in [
            (
                PortRole::In,
                "in",
                "Component input: one input socket of the component this node sits in. Whatever is wired to that socket outside comes out of this node. Cameras, files and streams come in through other nodes",
            ),
            (
                PortRole::Out,
                "out",
                "Component output: one output socket of the component this node sits in. Whatever is wired into this node comes out of that socket outside. Files, projectors and streams are written by the `out` nodes",
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
