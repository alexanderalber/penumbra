//! The params a component declares itself, in its text param [`PARAMS`]
//! ([`penumbra_core::declare::component`]).
//!
//! Two kinds of line give a component a param. An `@param` line makes a param the component
//! owns: the node stores its value like any other param's, and set, reset and bind check it
//! against the declaration at once. An `@expose` line shows an inner param on the component:
//! an alias, its value and spec kept by the inner node ([`Network::resolve_alias`]).
//!
//! The declarations are read from the node's own params, so they are known without a cook and
//! hold for every copy of the node.

use std::borrow::Cow;
use std::collections::BTreeMap;

use penumbra_core::declare::{self, Declared};
use penumbra_core::{Name, NodeId, NodePath, ParamPath, ParamSpec, Value};

use crate::network::{Network, NodeDef};
use crate::node::{NodeDesc, NodeTypes};

/// The text param of a component that declares its params, one per line (see the module).
pub const PARAMS: &str = "params";

/// Names a component's text may not declare beyond its kind's own params: words kept for
/// params every component will have.
pub const RESERVED: [&str; 1] = ["reset"];

/// One `@expose` line of a component.
#[derive(Clone, Debug, PartialEq)]
pub struct Exposed {
    pub name: Name,
    /// The inner param, or why the line does not name one.
    pub target: Result<ParamPath, String>,
}

/// Whether nodes of kind `desc` declare params in a [`PARAMS`] text.
pub fn declares(desc: &NodeDesc) -> bool {
    desc.container && desc.param_spec(PARAMS).is_some()
}

/// What the [`PARAMS`] text among `params` declares for a node of kind `desc`; `None` for a
/// kind that declares nothing. Names the kind has already, and [`RESERVED`] ones, are refused
/// line by line.
pub fn declared_in(desc: &NodeDesc, params: &BTreeMap<Name, Value>) -> Option<Declared> {
    if !declares(desc) {
        return None;
    }
    let text = params.get(PARAMS).and_then(Value::as_str).unwrap_or("");
    Some(declare::component(text, |name| {
        if desc.param_spec(name).is_some() {
            Some(format!("`{name}` is a param of every {} already", desc.kind))
        } else if RESERVED.contains(&name) {
            Some(format!("`{name}` is kept for a param every {} will have", desc.kind))
        } else {
            None
        }
    }))
}

/// The spec of param `name` of a node of kind `desc` with `params`: the kind's own, else one
/// the node declares.
pub fn spec_in<'a>(
    desc: &'a NodeDesc,
    params: &BTreeMap<Name, Value>,
    name: &str,
) -> Option<Cow<'a, ParamSpec>> {
    if let Some(spec) = desc.param_spec(name) {
        return Some(Cow::Borrowed(spec));
    }
    declared_in(desc, params)?.param(name).cloned().map(Cow::Owned)
}

/// The `@expose` lines of `declared`, with their targets inside the component at `comp`.
pub fn exposed_of(comp: &NodePath, declared: &Declared) -> Vec<Exposed> {
    declared
        .exposed()
        .map(|line| {
            let target = line.target.as_str();
            let target = if target.starts_with('/') || target.split('/').any(|n| n == ".." || n == ".") {
                Err(format!("`{target}` must be a path inside the component, like lfo1.rate"))
            } else {
                let joined = if comp.is_root() { format!("/{target}") } else { format!("{comp}/{target}") };
                ParamPath::parse(&joined).map_err(|e| format!("`{target}`: {e}"))
            };
            Exposed { name: line.name.clone(), target }
        })
        .collect()
}

impl NodeDef {
    /// What this node declares in its [`PARAMS`] text; `None` when its kind declares nothing.
    pub fn declared(&self, desc: &NodeDesc) -> Option<Declared> {
        declared_in(desc, &self.params)
    }

    /// The spec of param `name`: its kind's, else one the node declares ([`PARAMS`]).
    pub fn spec<'a>(&self, desc: &'a NodeDesc, name: &str) -> Option<Cow<'a, ParamSpec>> {
        spec_in(desc, &self.params, name)
    }

    /// The value of `name` as [`NodeDef::param`] gives it, and for a param the node declares
    /// itself, its stored value or the declared default.
    pub fn value(&self, desc: &NodeDesc, name: &str) -> Option<Value> {
        if let Some(v) = self.param(desc, name) {
            return Some(v.clone());
        }
        self.declared(desc)?.param(name).map(|s| s.default.clone())
    }
}

impl Network {
    /// What node `id` declares ([`PARAMS`]); `None` for a node whose kind declares nothing.
    pub fn declared(&self, types: &NodeTypes, id: NodeId) -> Option<Declared> {
        let node = self.node(id)?;
        node.declared(types.desc(&node.kind)?)
    }

    /// The spec of param `name` of node `id`: its kind's, else one the node declares. Exposed
    /// params are not followed here ([`Network::resolve_alias`]).
    pub fn spec_of<'t>(&self, types: &'t NodeTypes, id: NodeId, name: &str) -> Option<Cow<'t, ParamSpec>> {
        let node = self.node(id)?;
        node.spec(types.desc(&node.kind)?, name)
    }

    /// The params component `id` exposes (its `@expose` lines); empty for other nodes.
    pub fn exposed(&self, types: &NodeTypes, id: NodeId) -> Vec<Exposed> {
        self.exposed_with_errors(types, id, &mut Vec::new())
    }

    /// [`Self::exposed`], collecting the lines of the node's [`PARAMS`] text that declare
    /// nothing, of either kind.
    pub fn exposed_with_errors(
        &self,
        types: &NodeTypes,
        id: NodeId,
        errors: &mut Vec<String>,
    ) -> Vec<Exposed> {
        let (Some(declared), Some(path)) = (self.declared(types, id), self.path_of(id)) else {
            return Vec::new();
        };
        errors.extend(declared.errors.iter().cloned());
        exposed_of(&path, &declared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Op;
    use crate::testing::{create, test_types};

    fn pp(s: &str) -> ParamPath {
        ParamPath::parse(s).unwrap()
    }

    fn set(param: &str, value: Value) -> Op {
        Op::Set { param: pp(param), value }
    }

    fn text(t: &str) -> Value {
        Value::Text(t.into())
    }

    const BOX: &str = "@param preset choice(coral|maze|worms) = coral -- the pattern\n\
                       @param feed float = 0.05 0..0.1\n\
                       @group look\n\
                       @param glow float = 1\n\
                       @expose level = k.value";

    fn net() -> Network {
        let types = test_types();
        Network::new()
            .apply(
                &types,
                &[
                    create("/c", "test.comp"),
                    create("/c/k", "test.const"),
                    create("/src", "test.const"),
                    set("/c.params", text(BOX)),
                ],
            )
            .unwrap()
            .network
    }

    fn stored(net: &Network, param: &str) -> Option<Value> {
        let p = pp(param);
        net.node(net.resolve(p.node()).unwrap()).unwrap().params.get(p.param()).cloned()
    }

    #[test]
    fn declared_params_are_known_without_a_cook_and_hold_their_value() {
        let types = test_types();
        let net = net();
        let c = net.resolve(&"/c".parse().unwrap()).unwrap();
        let spec = net.spec_of(&types, c, "preset").unwrap();
        assert_eq!(spec.doc, "the pattern");
        assert_eq!(net.spec_of(&types, c, "glow").unwrap().group, "look");
        assert!(net.spec_of(&types, c, "level").is_none(), "an exposed param's spec stays inside");
        // Set and reset act on the component; setting the default stores nothing.
        let n = net
            .apply(&types, &[set("/c.preset", text("maze")), set("/c.feed", Value::Int(0))])
            .unwrap()
            .network;
        assert_eq!(stored(&n, "/c.preset"), Some(text("maze")));
        assert_eq!(stored(&n, "/c.feed"), Some(Value::Float(0.0)), "coerced to the declared kind");
        let node = n.node(c).unwrap();
        assert_eq!(node.value(types.desc("test.comp").unwrap(), "glow"), Some(Value::Float(1.0)));
        let back = n
            .apply(&types, &[Op::Reset { param: pp("/c.preset") }, set("/c.feed", Value::Float(0.05))])
            .unwrap();
        assert_eq!(stored(&back.network, "/c.preset"), None);
        assert_eq!(stored(&back.network, "/c.feed"), None);
        // The exposed one still sets the inner param.
        let n = net.apply(&types, &[set("/c.level", Value::Float(3.0))]).unwrap().network;
        assert_eq!(stored(&n, "/c/k.value"), Some(Value::Float(3.0)));
        assert_eq!(stored(&n, "/c.level"), None);
    }

    #[test]
    fn wrong_values_are_refused_at_once() {
        let types = test_types();
        let net = net();
        let e = net.apply(&types, &[set("/c.preset", text("spots"))]).unwrap_err().0;
        assert!(e.contains("coral, maze, worms") || e.contains("coral|maze|worms"), "{e}");
        let e = net.apply(&types, &[set("/c.feed", text("fast"))]).unwrap_err().0;
        assert!(e.contains("`feed` expects"), "{e}");
        let e = net.apply(&types, &[set("/c.nope", Value::Float(1.0))]).unwrap_err().0;
        assert!(e.contains("params: params, preset, feed, glow, level"), "{e}");
        let e = net.apply(&types, &[set("/c.expose", text("a = k.value"))]).unwrap_err().0;
        assert!(e.contains("@expose speed = lfo1.rate"), "{e}");
        let e = net.apply(&types, &[Op::Reset { param: pp("/c.nope") }]).unwrap_err().0;
        assert!(e.contains("has no param `nope`"), "{e}");
    }

    #[test]
    fn a_declared_param_binds_from_outside_and_checks_what_it_takes() {
        let types = test_types();
        let net = net();
        let bind = |param: &str, component: Option<u8>| Op::Bind {
            param: pp(param),
            component,
            from: "/src".parse().unwrap(),
            channel: None,
            cell: None,
            format: None,
        };
        let n = net.apply(&types, &[bind("/c.feed", None)]).unwrap().network;
        let c = n.resolve(&"/c".parse().unwrap()).unwrap();
        assert!(n.node(c).unwrap().is_bound("feed"));
        assert!(net.apply(&types, &[bind("/c.feed", Some(1))]).is_err(), "a float has no component 1");
        assert!(net.apply(&types, &[bind("/c.nope", None)]).is_err());
    }

    #[test]
    fn a_changed_declaration_keeps_the_values_that_still_fit() {
        let types = test_types();
        let net = net()
            .apply(
                &types,
                &[
                    set("/c.preset", text("worms")),
                    set("/c.feed", Value::Float(0.08)),
                    set("/c.glow", Value::Float(2.0)),
                ],
            )
            .unwrap()
            .network;
        // `worms` is gone from the list, `glow` is not declared any more, `feed` takes ints now.
        let changed = "@param preset choice(coral|maze)\n@param feed float = 0.08\n@param more int";
        let n = net.apply(&types, &[set("/c.params", text(changed))]).unwrap().network;
        assert_eq!(stored(&n, "/c.preset"), None, "no longer a choice: back to the default");
        assert_eq!(stored(&n, "/c.feed"), None, "the default now");
        assert_eq!(stored(&n, "/c.glow"), None, "nothing declares it");
        // Undo is the network before: every value comes back with the old text.
        assert_eq!(stored(&net, "/c.preset"), Some(text("worms")));
    }

    #[test]
    fn names_that_clash_cost_their_line_only() {
        let types = test_types();
        let clash = "@param params bool\n@param reset bool\n@param level float\n@expose level = k.value\n@param ok bool";
        let net = net().apply(&types, &[set("/c.params", text(clash))]).unwrap().network;
        let c = net.resolve(&"/c".parse().unwrap()).unwrap();
        let declared = net.declared(&types, c).unwrap();
        let names: Vec<&str> = declared.lines.iter().map(|d| d.name()).collect();
        assert_eq!(names, ["level", "ok"]);
        assert_eq!(declared.errors.len(), 3, "{:?}", declared.errors);
        assert!(
            declared.errors[0].starts_with("line 1: `params` is a param of every test.comp"),
            "{:?}",
            declared.errors
        );
        assert!(declared.errors[1].contains("`reset` is kept"), "{:?}", declared.errors);
        assert!(
            declared.errors[2].contains("line 4: `level` is declared on line 3 already"),
            "{:?}",
            declared.errors
        );
        let mut errors = Vec::new();
        assert!(net.exposed_with_errors(&types, c, &mut errors).is_empty());
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn a_new_node_takes_its_declared_values_with_its_text() {
        let types = test_types();
        let mut params = BTreeMap::new();
        params.insert(Name::new(PARAMS).unwrap(), text("@param x int = 2 0..9"));
        params.insert(Name::new("x").unwrap(), Value::Float(4.0));
        let ops =
            [Op::Create { path: "/c".parse().unwrap(), kind: "test.comp".into(), params: params.clone() }];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        assert_eq!(stored(&net, "/c.x"), Some(Value::Int(4)));
        params.insert(Name::new("x").unwrap(), text("four"));
        let ops = [Op::Create { path: "/c".parse().unwrap(), kind: "test.comp".into(), params }];
        assert!(Network::new().apply(&types, &ops).is_err());
    }
}
