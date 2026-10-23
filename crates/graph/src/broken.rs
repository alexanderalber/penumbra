//! The placeholder a node becomes when its entry in a project file does not load: its kind is
//! unknown (renamed, or from a newer build), it names an input its kind does not have, a param
//! value does not pass its declaration, and the like. One such node must not keep the rest of a
//! project from opening, and opening and saving must not lose it, so the node is kept as a
//! [`BROKEN_KIND`] node that holds its entry as text ([`ENTRY`]) and the reason ([`ERROR`]); the
//! project file writes the entry back as it was.
//!
//! A placeholder does not cook: its cook fails with the reason, so it shows as an error on the
//! node. It has no inputs and no outputs. Nodes that read it keep their wires (any output number
//! is allowed from a placeholder) and get nothing from them. Only loading and merging the
//! project file ([`Params::Keep`]) create, change or wire placeholders; edits from the UI and
//! the control channel may delete and rename one, and are refused anything else.

use penumbra_core::{ParamSpec, Value};

use crate::cook::CookContext;
use crate::network::{NodeDef, Params};
use crate::node::{CookError, Cooked, FnNodeType, NodeDesc, NodeType, Operator};

/// The kind of a placeholder.
pub const BROKEN_KIND: &str = "node.broken";
/// The node's entry from the project file, as one line of RON.
pub const ENTRY: &str = "entry";
/// Why the entry did not load.
pub const ERROR: &str = "error";
/// The output names other nodes read from it, one per line: a placeholder's kind may be
/// unknown, so a wire from its output `name` is stored as output number [`NAMED_OUTPUT`] plus
/// the line of `name` here, and written back by name.
pub const OUTPUTS: &str = "outputs";
/// The first output number that stands for a name in [`OUTPUTS`]; far above any real output
/// count, so it never meets a wire given by number.
pub const NAMED_OUTPUT: u32 = 1 << 16;

/// The declaration of [`BROKEN_KIND`].
pub fn desc() -> NodeDesc {
    NodeDesc::new(BROKEN_KIND, "A node whose entry in the project file did not load, kept as it was")
        .param(
            ParamSpec::text(ENTRY, "")
                .multiline()
                .doc("The node's entry from the project file, written back unchanged on every save"),
        )
        .param(ParamSpec::text(ERROR, "").multiline().doc("Why the entry did not load as a node"))
        .param(
            ParamSpec::text(OUTPUTS, "")
                .multiline()
                .doc("Names of the outputs other nodes read from this one, one per line"),
        )
}

/// The node type of [`BROKEN_KIND`], to register with the built-in types.
pub fn node_type() -> impl NodeType {
    FnNodeType::new(desc(), || Box::new(Broken) as Box<dyn Operator>)
}

/// Whether `node` is a placeholder.
pub fn is_broken(node: &NodeDef) -> bool {
    node.kind == BROKEN_KIND
}

/// Text param `name` of a placeholder, empty when missing.
pub fn text<'a>(node: &'a NodeDef, name: &str) -> &'a str {
    node.params.get(name).and_then(Value::as_str).unwrap_or("")
}

/// The refusal of an edit of placeholder `node` (at `path`) that does not come from the
/// project file.
pub(crate) fn refuse(node: &NodeDef, path: &str, params: Params) -> Result<(), penumbra_core::Error> {
    if params == Params::Keep || !is_broken(node) {
        return Ok(());
    }
    Err(penumbra_core::Error::new(format!(
        "{path} did not load ({}). The node is kept as written in the project file; here it can only be \
         deleted or renamed. Fix its entry in the file instead",
        text(node, ERROR)
    )))
}

struct Broken;

impl Operator for Broken {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        Err(CookError::failed(format!("did not load: {}", text(cx.def(), ERROR))))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use penumbra_core::{LatePolicy, NodePath, Rate, ShowClock};

    use super::*;
    use crate::cook::Cooker;
    use crate::job::JobPool;
    use crate::network::{Network, Op, Port};
    use crate::testing::{create, test_types};

    fn p(s: &str) -> NodePath {
        NodePath::parse(s).unwrap()
    }

    #[test]
    fn the_declaration_is_valid() {
        desc().validate().unwrap();
    }

    #[test]
    fn a_placeholder_fails_its_cook_its_readers_cook_and_edits_are_refused() {
        let mut types = test_types();
        types.register(node_type()).unwrap();
        let types = Arc::new(types);
        let error = Value::Text("unknown node kind `gone.kind`".into());
        let ops = [
            Op::Create {
                path: p("/x"),
                kind: BROKEN_KIND.into(),
                params: [(ERROR.parse().unwrap(), error)].into(),
            },
            create("/x/inner", "test.const"),
            create("/c", "test.const"),
            create("/sum", "test.add"),
            Op::Connect { from: p("/x"), output: NAMED_OUTPUT, to: p("/sum"), input: 0 },
            Op::Connect { from: p("/c"), output: 0, to: p("/sum"), input: 1 },
            Op::Bind { param: "/c.value".parse().unwrap(), component: None, from: p("/x"), channel: None },
        ];
        let e = Network::new().apply(&types, &ops).unwrap_err();
        assert!(e.0.contains("only loading a project"), "{e}");
        let net = Network::new().apply_with(&types, &ops, Params::Keep).unwrap().network;
        let id = |s: &str| net.resolve(&p(s)).unwrap();
        let (x, sum) = (id("/x"), id("/sum"));
        assert_eq!(net.node(sum).unwrap().inputs[0], Some(Port { node: x, output: NAMED_OUTPUT }));

        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        let shared = Arc::new(net.clone());
        cooker.cook(&shared, clock.now(), &[x, sum, id("/x/inner")], &[]);
        let failed = cooker.status(x).unwrap().error.unwrap();
        assert!(failed.contains("did not load: unknown node kind"), "{failed}");
        assert!(cooker.status(sum).unwrap().has_output, "a reader cooks with nothing from it");
        assert!(cooker.status(id("/x/inner")).unwrap().has_output);

        for op in [
            Op::Set { param: "/x.entry".parse().unwrap(), value: Value::Text(String::new()) },
            Op::Reset { param: "/x.error".parse().unwrap() },
            Op::Lock { path: p("/x"), locked: true },
            Op::Connect { from: p("/x"), output: 0, to: p("/sum"), input: 0 },
            create("/x/more", "test.const"),
        ] {
            let e = net.apply(&types, std::slice::from_ref(&op)).unwrap_err();
            assert!(e.0.contains("did not load") || e.0.contains("cannot hold nodes"), "{op:?}: {e}");
        }
        let moved = net.apply(&types, &[Op::Place { path: p("/x"), pos: Some([5.0, 6.0]) }]).unwrap();
        assert_eq!(moved.network.node(x).unwrap().pos, Some([5.0, 6.0]), "a placeholder moves like any node");
        let renamed = net.apply(&types, &[Op::Rename { path: p("/x"), name: "y".parse().unwrap() }]).unwrap();
        assert_eq!(renamed.network.resolve(&p("/y")), Some(x));
        let deleted = net.apply(&types, &[Op::Delete { path: p("/x") }]).unwrap().network;
        assert_eq!(deleted.node(sum).unwrap().inputs[0], None);
        assert!(deleted.node(id("/c")).unwrap().binds.is_empty());
    }
}
