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
use penumbra_core::{Error, Name, NodeId, NodePath, ParamPart, ParamPath, ParamSpec, Value};

use crate::network::{Bind, Network, NodeDef, Op};
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

/// How an inner param got onto its component ([`Network::put_param`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    /// A new `@param` line on the component; the inner param follows it.
    Declared,
    /// The component had a param of that name that fits: the inner param follows it.
    Followed,
    /// The `@expose` line that showed the inner param became an `@param` line in its place.
    Replaced,
    /// A new `@expose` line shows the inner param on the component ([`Network::show_param`]).
    Shown,
}

/// The edit that puts an inner param on its component, as [`Network::put_param`] and
/// [`Network::show_param`] plan it.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub ops: Vec<Op>,
    pub put: Put,
    /// The component.
    pub comp: NodePath,
    /// The param's name on the component.
    pub name: Name,
}

impl Network {
    /// The component node `id` sits in, which must declare params ([`PARAMS`]).
    fn owner(
        &self,
        types: &NodeTypes,
        id: NodeId,
        at: &NodePath,
    ) -> Result<(NodeId, NodePath, Declared), Error> {
        let node = self.node(id).ok_or_else(|| Error::new(format!("no node at {at}")))?;
        let Some(comp) = node.parent else {
            return Err(Error::new(format!("{at} sits at the top level, in no component")));
        };
        let comp_path = self.path_of(comp).ok_or_else(|| Error::new("the component vanished"))?;
        let declared = self
            .declared(types, comp)
            .ok_or_else(|| Error::new(format!("{comp_path} takes no params of its own")))?;
        Ok((comp, comp_path, declared))
    }

    /// Why the component of kind `desc` cannot take `name`, if it cannot: a param of its kind,
    /// or a reserved word.
    fn kind_takes(desc: &NodeDesc, comp: &NodePath, name: &str) -> Option<Error> {
        if desc.param_spec(name).is_some() {
            return Some(Error::new(format!(
                "`{name}` is a param of every {} already; give another name",
                desc.kind
            )));
        }
        RESERVED
            .contains(&name)
            .then(|| Error::new(format!("`{name}` is kept for a param {comp} will have; give another name")))
    }

    /// The edit that makes inner param `param` (of spec `spec`, now `value`) a param of its
    /// component that it follows: an `@param` line copied from the spec (`name`, else the
    /// param's own name), the component set to `value`, and a binding to `parent.<name>`.
    /// A param of that name the component has already is followed when its kind fits, and an
    /// `@expose` line showing this very param is turned into the `@param` line. Refused when
    /// the param is bound, when the name is taken otherwise, and for what a line cannot
    /// declare (a text, a file).
    pub fn put_param(
        &self,
        types: &NodeTypes,
        param: &ParamPath,
        spec: &ParamSpec,
        value: &Value,
        name: Option<&Name>,
    ) -> Result<Planned, Error> {
        let at = param.node();
        let id = self.resolve(at).ok_or_else(|| Error::new(format!("no node at {at}")))?;
        let (comp, comp_path, declared) = self.owner(types, id, at)?;
        let node = self.node(id).ok_or_else(|| Error::new(format!("no node at {at}")))?;
        let own = param.param();
        if let Some((_, b)) = node.binds.iter().find(|(k, _)| k.param == *own) {
            let from = self.path_of(b.node).map_or_else(|| "?".to_string(), |p| p.to_string());
            return Err(Error::new(match &b.param {
                Some(p) if b.node == comp => format!("`{own}` follows parent.{p} already"),
                _ => format!("`{own}` is bound to {from}; unbind it first"),
            }));
        }
        let name = name.unwrap_or(own).clone();
        let desc =
            self.node(comp).and_then(|n| types.desc(&n.kind)).ok_or_else(|| Error::new("unknown kind"))?;
        let follow = Bind::follow(comp, name.clone()).op(param.clone(), None, comp_path.clone());
        let plan = |ops, put| Ok(Planned { ops, put, comp: comp_path.clone(), name: name.clone() });
        if let Some(existing) = declared.param(name.as_str()) {
            if crate::follow::alike(spec, existing) {
                return plan(vec![follow], Put::Followed);
            }
            return Err(Error::new(format!(
                "{comp_path} has a param `{name}` already, and it is {}; `{own}` is {}. Give another name",
                existing.kind_label(),
                spec.kind_label()
            )));
        }
        if let Some(why) = Self::kind_takes(desc, &comp_path, name.as_str()) {
            return Err(why);
        }
        let mut copy = spec.clone();
        copy.name = name.to_string();
        let line = declare::line(&copy)?;
        let text = self.node(comp).and_then(|n| n.params.get(PARAMS)).and_then(Value::as_str).unwrap_or("");
        let shown = exposed_of(&comp_path, &declared).into_iter().find(|x| x.name == name);
        let (text, put) = match shown {
            Some(x) if x.target.as_ref() == Ok(param) => {
                (declare::replace_expose(text, name.as_str(), &line).unwrap_or_default(), Put::Replaced)
            }
            Some(x) => {
                let what = x.target.map_or_else(|e| e, |t| t.to_string());
                return Err(Error::new(format!(
                    "{comp_path} shows `{name}` from {what} already; give another name"
                )));
            }
            None => (declare::add_line(text, &line), Put::Declared),
        };
        let mut ops = vec![Op::Set { param: comp_path.param(PARAMS.parse()?)?, value: Value::Text(text) }];
        let value = spec.check(value).unwrap_or_else(|_| spec.default.clone());
        if value != spec.default {
            ops.push(Op::Set { param: comp_path.param(name.clone())?, value });
        }
        ops.push(follow);
        plan(ops, put)
    }

    /// The edit that shows inner param `param` on its component: an `@expose` line under
    /// `name`, else the param's own name, else `<node>_<param>` when that is taken. The value
    /// and spec stay with the inner node. Refused for a param that is shown or follows already.
    pub fn show_param(
        &self,
        types: &NodeTypes,
        param: &ParamPath,
        name: Option<&Name>,
    ) -> Result<Planned, Error> {
        let at = param.node();
        let id = self.resolve(at).ok_or_else(|| Error::new(format!("no node at {at}")))?;
        let (comp, comp_path, declared) = self.owner(types, id, at)?;
        let node = self.node(id).ok_or_else(|| Error::new(format!("no node at {at}")))?;
        let own = param.param();
        if let Some((_, b)) =
            node.binds.iter().find(|(k, b)| k.param == *own && b.node == comp && b.param.is_some())
        {
            let p = b.param.as_ref().map_or("", Name::as_str);
            return Err(Error::new(format!("`{own}` follows parent.{p}; its row on {comp_path} is `{p}`")));
        }
        let exposed = exposed_of(&comp_path, &declared);
        if let Some(x) = exposed.iter().find(|x| x.target.as_ref() == Ok(param)) {
            return Err(Error::new(format!("{comp_path} shows `{own}` as `{}` already", x.name)));
        }
        let desc =
            self.node(comp).and_then(|n| types.desc(&n.kind)).ok_or_else(|| Error::new("unknown kind"))?;
        let taken = |n: &str| {
            Self::kind_takes(desc, &comp_path, n).is_some() || declared.lines.iter().any(|d| d.name() == n)
        };
        let name = match name {
            Some(n) if taken(n.as_str()) => {
                return Err(Error::new(format!("{comp_path} has a param `{n}` already; give another name")));
            }
            Some(n) => n.clone(),
            None if !taken(own.as_str()) => own.clone(),
            None => {
                Name::new(&format!("{}_{own}", node.name)).ok().filter(|n| !taken(n.as_str())).ok_or_else(
                    || Error::new(format!("{comp_path} has a param `{own}` already; give another name")),
                )?
            }
        };
        let target = format!("{}.{own}", node.name);
        let line = if name == *own {
            format!("{} {target}", declare::EXPOSE)
        } else {
            format!("{} {name} = {target}", declare::EXPOSE)
        };
        let text = self.node(comp).and_then(|n| n.params.get(PARAMS)).and_then(Value::as_str).unwrap_or("");
        let ops = vec![Op::Set {
            param: comp_path.param(PARAMS.parse()?)?,
            value: Value::Text(declare::add_line(text, &line)),
        }];
        Ok(Planned { ops, put: Put::Shown, comp: comp_path, name })
    }

    /// The params that follow component `comp`'s params, by the component's param: each
    /// follower's node and the part of it that follows.
    pub fn followers(&self, comp: NodeId) -> BTreeMap<Name, Vec<(NodeId, ParamPart)>> {
        let mut out: BTreeMap<Name, Vec<(NodeId, ParamPart)>> = BTreeMap::new();
        for n in self.nodes() {
            for (part, b) in n.binds.iter().filter(|(_, b)| b.node == comp) {
                if let Some(name) = &b.param {
                    out.entry(name.clone()).or_default().push((n.id, part.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            from_param: None,
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

    #[test]
    fn an_inner_param_goes_onto_its_component_in_one_edit() {
        let types = test_types();
        let net = net()
            .apply(&types, &[create("/c/m", "test.const"), set("/c/m.value", Value::Float(0.25))])
            .unwrap()
            .network;
        let spec = ParamSpec::float("value", 0.0).range(0.0, 1.0).doc("how much");
        let plan = net.put_param(&types, &pp("/c/m.value"), &spec, &Value::Float(0.25), None).unwrap();
        assert_eq!(plan.put, Put::Declared);
        let n = net.apply(&types, &plan.ops).unwrap().network;
        let c = n.resolve(&"/c".parse().unwrap()).unwrap();
        let box_text = stored(&n, "/c.params").unwrap();
        let box_text = box_text.as_str().unwrap();
        assert!(
            box_text.contains(
                "@param feed float = 0.05 0..0.1\n@param value float = 0 0..1 -- how much\n@group look"
            ),
            "among the main rows: {box_text}"
        );
        assert_eq!(stored(&n, "/c.value"), Some(Value::Float(0.25)), "the value moves to the component");
        let m = n.node(n.resolve(&"/c/m".parse().unwrap()).unwrap()).unwrap();
        assert!(m.binds.values().any(|b| b.node == c && b.param.as_ref().map(Name::as_str) == Some("value")));
        assert_eq!(n.followers(c).get("value").map(Vec::len), Some(1));
        // Again: it follows already. Another node's like param follows the one declared now.
        let e = n.put_param(&types, &pp("/c/m.value"), &spec, &Value::Float(0.25), None).unwrap_err().0;
        assert_eq!(e, "`value` follows parent.value already");
        let plan = n.put_param(&types, &pp("/c/k.value"), &spec, &Value::Float(0.0), None).unwrap();
        assert_eq!((plan.put, plan.ops.len()), (Put::Followed, 1));
        // A taken name of another kind, a kind's own param, the top level.
        let pick = ParamSpec::choice("preset", &["a", "b"]);
        let e = net.put_param(
            &types,
            &pp("/c/m.value"),
            &spec,
            &Value::Float(0.0),
            Some(&Name::new("preset").unwrap()),
        );
        assert!(e.unwrap_err().0.contains("has a param `preset` already, and it is a choice"));
        let e = net.put_param(
            &types,
            &pp("/c/m.value"),
            &spec,
            &Value::Float(0.0),
            Some(&Name::new("params").unwrap()),
        );
        assert!(e.unwrap_err().0.contains("is a param of every test.comp already"));
        let e = net.put_param(&types, &pp("/src.value"), &pick, &text("a"), None).unwrap_err().0;
        assert_eq!(e, "/src sits at the top level, in no component");
    }

    #[test]
    fn a_shown_param_becomes_a_declared_one_in_its_place_and_showing_picks_a_free_name() {
        let types = test_types();
        let net = net();
        let spec = ParamSpec::float("value", 0.0);
        let level = Name::new("level").unwrap();
        let plan = net.put_param(&types, &pp("/c/k.value"), &spec, &Value::Float(0.5), Some(&level)).unwrap();
        assert_eq!(plan.put, Put::Replaced);
        let n = net.apply(&types, &plan.ops).unwrap().network;
        let text = stored(&n, "/c.params").unwrap();
        assert!(text.as_str().unwrap().ends_with("@param glow float = 1\n@param level float = 0"), "{text}");
        assert_eq!(stored(&n, "/c.level"), Some(Value::Float(0.5)));
        // Showing: under its own name, else `<node>_<param>`; never twice.
        let net = net.apply(&types, &[create("/c/glow", "test.const")]).unwrap().network;
        let e = net.show_param(&types, &pp("/c/k.value"), None).unwrap_err().0;
        assert_eq!(e, "/c shows `value` as `level` already");
        let plan = net.show_param(&types, &pp("/c/glow.value"), None).unwrap();
        assert_eq!((plan.put, plan.name.as_str()), (Put::Shown, "value"));
        let n = net.apply(&types, &plan.ops).unwrap().network;
        let text = stored(&n, "/c.params").unwrap();
        assert!(text.as_str().unwrap().contains("@param glow float = 1"), "unchanged lines stay");
        assert!(
            text.as_str()
                .unwrap()
                .contains("@param feed float = 0.05 0..0.1\n@expose glow.value\n@group look")
        );
        let n = n.apply(&types, &[create("/c/x", "test.const")]).unwrap().network;
        let plan = n.show_param(&types, &pp("/c/x.value"), None).unwrap();
        assert_eq!(plan.name.as_str(), "x_value");
    }
}
