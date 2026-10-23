//! The network: nodes, their parameters and connections, as an immutable value.
//!
//! An edit never changes a network in place: [`Network::apply`] returns a new one, and nodes
//! are `Arc`s, so the copy shares every unchanged node with the original. Readers (cook thread,
//! UI, autosave) hold snapshots without locks; history entries are snapshots too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use penumbra_core::{Error, Name, NodeId, NodePath, ParamPart, ParamPath, Value};
use serde::{Deserialize, Serialize};

use crate::broken;
use crate::node::{DataKind, NodeDesc, NodeTypes, PortRole};

/// The text param of a container that lists the params it exposes: one per line, `name =
/// inner/path.param` or just `inner/path.param` (exposed under the param's own name); `#` starts a
/// comment. An exposed param is an alias: setting, resetting or binding `/comp.name` acts on the
/// inner param, which holds the value.
pub const EXPOSE: &str = "expose";

/// One line of a container's [`EXPOSE`] list.
#[derive(Clone, Debug, PartialEq)]
pub struct Exposed {
    pub name: Name,
    /// The inner param, or why the line does not name one.
    pub target: Result<ParamPath, String>,
}

/// The lines of an [`EXPOSE`] text of the container at `comp`, with their targets; lines that
/// do not even give a name are left out (`errors` collects them).
pub fn parse_expose(comp: &NodePath, text: &str, errors: &mut Vec<String>) -> Vec<Exposed> {
    let mut out: Vec<Exposed> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (name, target) = match line.split_once('=') {
            Some((n, t)) => (Some(n.trim()), t.trim()),
            None => (None, line),
        };
        let param = target.rsplit_once('.').map(|(_, p)| p).unwrap_or("");
        let name = match Name::new(name.unwrap_or(param)) {
            Ok(n) => n,
            Err(e) => {
                errors.push(format!("line {}: {e}", i + 1));
                continue;
            }
        };
        if out.iter().any(|x| x.name == name) {
            errors.push(format!("line {}: `{name}` is exposed twice", i + 1));
            continue;
        }
        let target = if target.starts_with('/') || target.split('/').any(|n| n == ".." || n == ".") {
            Err(format!("`{target}` must be a path inside the component, like lfo1.rate"))
        } else {
            let joined = if comp.is_root() { format!("/{target}") } else { format!("{comp}/{target}") };
            ParamPath::parse(&joined).map_err(|e| format!("`{target}`: {e}"))
        };
        out.push(Exposed { name, target });
    }
    out
}

/// Compares names with runs of digits as numbers: `in2` before `in10`.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(s: &str) -> Vec<(bool, &str)> {
        let mut parts = Vec::new();
        let mut start = 0;
        let mut digit: Option<bool> = None;
        for (i, c) in s.char_indices() {
            let d = c.is_ascii_digit();
            if let Some(prev) = digit
                && prev != d
            {
                parts.push((prev, s.get(start..i).unwrap_or("")));
                start = i;
            }
            digit = Some(d);
        }
        if let Some(d) = digit {
            parts.push((d, s.get(start..).unwrap_or("")));
        }
        parts
    }
    let (pa, pb) = (split(a), split(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len()).then_with(|| a.cmp(b))
}

/// Most nodes a network may hold. Far above any real project; stops a runaway script or a
/// hostile batch from eating memory.
pub const MAX_NODES: usize = 100_000;

/// One output of one node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Port {
    pub node: NodeId,
    pub output: u32,
}

/// What a bound parameter reads: channel `channel` of the first `chan` output of node `node`, or
/// its first channel when `channel` is `None`. The binding is a dependency like an input: the
/// source cooks first, a loop is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bind {
    pub node: NodeId,
    pub channel: Option<Name>,
}

/// One node as stored in the network.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeDef {
    pub id: NodeId,
    pub name: Name,
    /// `None` at the root.
    pub parent: Option<NodeId>,
    pub kind: String,
    /// Explicitly set values only; a missing parameter has its declared default.
    pub params: BTreeMap<Name, Value>,
    /// One slot per declared input.
    pub inputs: Vec<Option<Port>>,
    /// Per input, whether it reads its source's previous result ([`crate::InputSpec::previous`]);
    /// taken from the type when the node is created.
    pub previous: Vec<bool>,
    /// Parameters (or components of them) driven by a channel. The stored value of a bound
    /// parameter stays as it is: it is what the parameter falls back to while the channel is
    /// missing, and the other components of a vector bound per component.
    pub binds: BTreeMap<ParamPart, Bind>,
    /// Locked: the node keeps its current outputs and does not cook, whatever happens upstream;
    /// its inputs may even be disconnected or deleted (TouchDesigner's lock flag).
    pub locked: bool,
    /// Where the node sits in the editor, in graph units on its component's plane (the top
    /// left corner); `None` until it is placed. Part of the document like any other state, so
    /// a move is undoable and written to the project file; the cooker does not look at it.
    pub pos: Option<[f32; 2]>,
}

impl NodeDef {
    /// The value of `name`: explicit, else the declared default. Bindings are not applied here;
    /// the cooker applies them ([`crate::CookContext::param`]).
    pub fn param<'a>(&'a self, desc: &'a NodeDesc, name: &str) -> Option<&'a Value> {
        self.params.get(name).or_else(|| desc.param_spec(name).map(|s| &s.default))
    }

    /// Every node this one depends on: connected inputs and binding sources. Inputs that read
    /// a previous result are no dependency and not listed.
    pub fn upstream(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.inputs
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.is_previous(*i))
            .filter_map(|(_, p)| p.map(|p| p.node))
            .chain(self.binds.values().map(|b| b.node))
    }

    /// Every node this one reads from, dependency or not.
    pub fn sources(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.inputs.iter().flatten().map(|p| p.node).chain(self.binds.values().map(|b| b.node))
    }

    /// Whether input `i` reads its source's previous result.
    pub fn is_previous(&self, i: usize) -> bool {
        self.previous.get(i).copied().unwrap_or(false)
    }

    /// Whether any part of parameter `name` is bound.
    pub fn is_bound(&self, name: &str) -> bool {
        self.binds.keys().any(|k| k.param.as_str() == name)
    }
}

/// One step of an edit. Edits are batches of these, applied all or nothing. Nodes are named by
/// path, so a batch can create a node and connect it in the next step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Create a node at `path`; its parent must exist and be a container (or the root).
    Create {
        path: NodePath,
        kind: String,
        #[serde(default)]
        params: BTreeMap<Name, Value>,
    },
    /// Delete a node with everything inside it; connections from it are cleared.
    Delete {
        path: NodePath,
    },
    Rename {
        path: NodePath,
        name: Name,
    },
    Set {
        param: ParamPath,
        value: Value,
    },
    /// Back to the declared default.
    Reset {
        param: ParamPath,
    },
    /// Connect output `output` of `from` to input `input` of `to`, replacing what was there.
    Connect {
        from: NodePath,
        #[serde(default)]
        output: u32,
        to: NodePath,
        #[serde(default)]
        input: u32,
    },
    Disconnect {
        to: NodePath,
        input: u32,
    },
    /// Lock or unlock a node's outputs. Only nodes with outputs can be locked.
    Lock {
        path: NodePath,
        locked: bool,
    },
    /// Drive parameter `param` (component `component` of a vector or colour) with channel
    /// `channel` of node `from` (its first channel when `None`), replacing an earlier binding.
    Bind {
        param: ParamPath,
        #[serde(default)]
        component: Option<u8>,
        from: NodePath,
        #[serde(default)]
        channel: Option<Name>,
    },
    /// Back to the stored value. Unbinding something not bound changes nothing (a merged file
    /// edit may unbind what the user unbound already).
    Unbind {
        param: ParamPath,
        #[serde(default)]
        component: Option<u8>,
    },
    /// Move a node in the editor ([`NodeDef::pos`]); `None` leaves it to the editor to place.
    /// Placeholders ([`broken`]) can be moved like any node.
    Place {
        path: NodePath,
        #[serde(default)]
        pos: Option<[f32; 2]>,
    },
    /// Set a project setting ([`SETTINGS`]); `None` (or its default) resets it.
    Setting {
        name: Name,
        #[serde(default)]
        value: Option<Value>,
    },
}

impl Op {
    /// `set /noise1.scale` and the like, for error messages and history labels.
    pub fn label(&self) -> String {
        match self {
            Op::Create { path, kind, .. } => format!("create {path} ({kind})"),
            Op::Delete { path } => format!("delete {path}"),
            Op::Rename { path, name } => format!("rename {path} to {name}"),
            Op::Set { param, .. } => format!("set {param}"),
            Op::Reset { param } => format!("reset {param}"),
            Op::Connect { from, output, to, input } => format!("connect {from}:{output} to {to}:{input}"),
            Op::Disconnect { to, input } => format!("disconnect {to}:{input}"),
            Op::Lock { path, locked: true } => format!("lock {path}"),
            Op::Lock { path, locked: false } => format!("unlock {path}"),
            Op::Bind { param, component, from, channel } => {
                let to = part_label(param, *component);
                match channel {
                    Some(c) => format!("bind {to} to {from}:{c}"),
                    None => format!("bind {to} to {from}"),
                }
            }
            Op::Unbind { param, component } => format!("unbind {}", part_label(param, *component)),
            Op::Place { path, pos: Some(_) } => format!("move {path}"),
            Op::Place { path, pos: None } => format!("unplace {path}"),
            Op::Setting { name, value: Some(v) } => format!("project setting {name} = {v}"),
            Op::Setting { name, value: None } => format!("reset project setting {name}"),
        }
    }
}

fn part_label(param: &ParamPath, component: Option<u8>) -> String {
    match component {
        Some(c) => format!("{param}.{c}"),
        None => param.to_string(),
    }
}

/// Result of [`Network::apply`].
#[derive(Clone, Debug)]
pub struct Applied {
    pub network: Network,
    /// Ids of the nodes created by the batch, in order.
    pub created: Vec<NodeId>,
    /// Every node the batch created, changed or deleted (for the change log).
    pub touched: BTreeSet<NodeId>,
    params: Params,
}

/// How [`Network::apply_with`] treats a parameter the node type does not declare.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Params {
    /// Refuse it: edits from the UI and the control channel, where it is a typo.
    #[default]
    Strict,
    /// Keep it as a stray value: loading the project file, where it is most likely a param a
    /// built-in node lost in a newer version. One stray value must not keep a whole project
    /// from opening; the cooker warns about it and the UI offers to remove it.
    Keep,
}

/// A project-wide setting: stored in the network (so it is undoable, merges with edits of the
/// project file and is written to it) like a param without a node.
#[derive(Clone, Copy, Debug)]
pub struct SettingSpec {
    pub name: &'static str,
    pub default: bool,
    pub doc: &'static str,
}

/// Keep what feedback loops, caches and other operators hold across restarts (the state files).
/// Locked nodes' outputs are kept regardless.
pub const PERSIST_STATE: &str = "persist_state";

/// Every project setting. All are switches so far.
pub const SETTINGS: &[SettingSpec] = &[SettingSpec {
    name: PERSIST_STATE,
    default: false,
    doc: "Keep what feedback loops, caches and other nodes hold across restarts (locked nodes are kept regardless)",
}];

/// The value of setting `name` as stored (`None` for the default), checked against
/// [`SETTINGS`].
pub fn check_setting(name: &Name, value: Option<&Value>) -> Result<Option<Value>, Error> {
    let spec = SETTINGS.iter().find(|s| s.name == name.as_str()).ok_or_else(|| {
        let known: Vec<&str> = SETTINGS.iter().map(|s| s.name).collect();
        Error::new(format!("no project setting `{name}`; settings: {}", known.join(", ")))
    })?;
    match value {
        None => Ok(None),
        Some(v) => {
            let on = v
                .as_bool()
                .ok_or_else(|| Error::new(format!("project setting `{name}` is true or false")))?;
            Ok((on != spec.default).then_some(Value::Bool(on)))
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Network {
    nodes: BTreeMap<NodeId, Arc<NodeDef>>,
    /// `(parent, name)` to node, for path lookups.
    index: BTreeMap<(Option<NodeId>, Name), NodeId>,
    next_id: u64,
    /// Project settings that differ from their default ([`SETTINGS`]).
    settings: BTreeMap<Name, Value>,
}

impl Network {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether two snapshots hold the same network; cheap when they share the allocation.
    pub fn same(a: &Arc<Self>, b: &Arc<Self>) -> bool {
        Arc::ptr_eq(a, b) || a == b
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Settings that differ from their default.
    pub fn settings(&self) -> &BTreeMap<Name, Value> {
        &self.settings
    }

    /// Switch `name` ([`SETTINGS`]); its default when it is not stored, `false` for an unknown
    /// name.
    pub fn switch(&self, name: &str) -> bool {
        match self.settings.get(name).and_then(Value::as_bool) {
            Some(on) => on,
            None => SETTINGS.iter().find(|s| s.name == name).is_some_and(|s| s.default),
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&Arc<NodeDef>> {
        self.nodes.get(&id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Arc<NodeDef>> + '_ {
        self.nodes.values()
    }

    /// Direct children of `parent` (`None` is the root).
    pub fn children(&self, parent: Option<NodeId>) -> impl Iterator<Item = &Arc<NodeDef>> + '_ {
        self.nodes.values().filter(move |n| n.parent == parent)
    }

    /// The params container `id` exposes ([`EXPOSE`]); empty for other nodes.
    pub fn exposed(&self, types: &NodeTypes, id: NodeId) -> Vec<Exposed> {
        self.exposed_with_errors(types, id, &mut Vec::new())
    }

    /// [`Self::exposed`], collecting the lines that name nothing.
    pub fn exposed_with_errors(
        &self,
        types: &NodeTypes,
        id: NodeId,
        errors: &mut Vec<String>,
    ) -> Vec<Exposed> {
        let Some(node) = self.nodes.get(&id) else { return Vec::new() };
        let Some(desc) = types.desc(&node.kind).filter(|d| d.container) else { return Vec::new() };
        let Some(text) = node.param(desc, EXPOSE).and_then(Value::as_str) else { return Vec::new() };
        let Some(path) = self.path_of(id) else { return Vec::new() };
        parse_expose(&path, text, errors)
    }

    /// The input and output nodes of component `id` ([`PortRole`]), each in natural order of
    /// their names (`in2` before `in10`): the component's inputs and outputs.
    pub fn comp_ports(&self, types: &NodeTypes, id: NodeId) -> (Vec<NodeId>, Vec<NodeId>) {
        let mut ins: Vec<&Arc<NodeDef>> = Vec::new();
        let mut outs: Vec<&Arc<NodeDef>> = Vec::new();
        for c in self.children(Some(id)) {
            match types.desc(&c.kind).and_then(|d| d.port) {
                Some(PortRole::In) => ins.push(c),
                Some(PortRole::Out) => outs.push(c),
                None => {}
            }
        }
        for list in [&mut ins, &mut outs] {
            list.sort_by(|a, b| natural_cmp(a.name.as_str(), b.name.as_str()));
        }
        (ins.iter().map(|n| n.id).collect(), outs.iter().map(|n| n.id).collect())
    }

    /// The role of `id` at the edge of its component, if it has one.
    pub fn port_role(&self, types: &NodeTypes, id: NodeId) -> Option<PortRole> {
        self.nodes.get(&id).and_then(|n| types.desc(&n.kind)).and_then(|d| d.port)
    }

    /// Whether an output of `src` may feed an input of `dst`: both sit in the same component,
    /// or the wire crosses a component's edge through its input or output node. A wire straight
    /// into or out of the middle of a component would make it depend on its surroundings in
    /// ways neither its file nor the editor can show.
    pub fn wire_allowed(&self, types: &NodeTypes, src: NodeId, dst: NodeId) -> bool {
        let parent = |id: NodeId| self.nodes.get(&id).and_then(|n| n.parent);
        let Some(src_parent) = self.nodes.get(&src).map(|n| n.parent) else { return false };
        // The level whose nodes may feed `dst`: outside its component for an input node.
        let level = match (self.port_role(types, dst), parent(dst)) {
            (Some(PortRole::In), Some(comp)) => parent(comp),
            _ => parent(dst),
        };
        if src_parent == level {
            return true;
        }
        // An output node is also seen one level up, as an output of its component.
        matches!(
            (self.port_role(types, src), src_parent),
            (Some(PortRole::Out), Some(comp)) if parent(comp) == level
        )
    }

    /// The param an alias on a container stands for ([`EXPOSE`]), followed through nested
    /// components; `param` itself when it is no alias.
    pub fn resolve_alias(&self, types: &NodeTypes, param: &ParamPath) -> Result<ParamPath, Error> {
        let mut current = param.clone();
        for _ in 0..=NodePath::MAX_DEPTH {
            let Some(id) = self.resolve(current.node()) else { return Ok(current) };
            let Some(desc) = self.nodes.get(&id).and_then(|n| types.desc(&n.kind)) else {
                return Ok(current);
            };
            if !desc.container || desc.param_spec(current.param().as_str()).is_some() {
                return Ok(current);
            }
            let exposed = self.exposed(types, id);
            let Some(alias) = exposed.iter().find(|e| &e.name == current.param()) else {
                let known: Vec<&str> = desc
                    .params
                    .iter()
                    .map(|p| p.name.as_str())
                    .chain(exposed.iter().map(|e| e.name.as_str()))
                    .collect();
                return Err(Error::new(format!(
                    "`{}` has no param `{}`; params: {}",
                    desc.kind,
                    current.param(),
                    known.join(", ")
                )));
            };
            current =
                alias.target.clone().map_err(|e| Error::new(format!("exposed `{}`: {e}", alias.name)))?;
        }
        Err(Error::new(format!("{param}: exposed params nest too deep")))
    }

    /// Nodes reading any output of `id`, through an input or a binding.
    pub fn consumers(&self, id: NodeId) -> impl Iterator<Item = &Arc<NodeDef>> + '_ {
        self.nodes.values().filter(move |n| n.sources().any(|u| u == id))
    }

    pub fn resolve(&self, path: &NodePath) -> Option<NodeId> {
        let mut current: Option<NodeId> = None;
        for name in path.names() {
            current = Some(*self.index.get(&(current, name.clone()))?);
        }
        current
    }

    /// The node a node reference param of `from` names ([`penumbra_core::ParamSpec::node_ref`]),
    /// read on `from`'s level by the rule of [`NodePath::resolve_ref`]: absolute, a name beside
    /// it, `sub/x` below, `../x` up.
    pub fn resolve_from(&self, from: NodeId, text: &str) -> Result<NodeId, String> {
        let level = self.path_of(from).and_then(|p| p.parent()).unwrap_or_else(NodePath::root);
        let path = level.resolve_ref(text).map_err(|e| e.0)?;
        self.resolve(&path).ok_or_else(|| format!("no node at {path}"))
    }

    /// The path of a node; `None` if it does not exist.
    pub fn path_of(&self, id: NodeId) -> Option<NodePath> {
        let mut names = Vec::new();
        let mut current = self.nodes.get(&id)?;
        loop {
            names.push(current.name.clone());
            match current.parent {
                // Parent chains are acyclic by construction; the depth limit is a backstop.
                Some(p) if names.len() <= NodePath::MAX_DEPTH => current = self.nodes.get(&p)?,
                Some(_) => return None,
                None => break,
            }
        }
        names.reverse();
        NodePath::from_names(names).ok()
    }

    /// The stored params of the nodes in `ids` that differ between `self` and `after`; `None`
    /// stands for a param that is not stored (its default). Nodes missing on either side are
    /// skipped.
    pub fn param_changes(
        &self,
        after: &Network,
        ids: &BTreeSet<NodeId>,
    ) -> Vec<((NodeId, Name), Option<Value>)> {
        let mut changes = Vec::new();
        for id in ids {
            let (Some(old), Some(new)) = (self.nodes.get(id), after.nodes.get(id)) else { continue };
            let names: BTreeSet<&Name> = old.params.keys().chain(new.params.keys()).collect();
            for name in names {
                let value = new.params.get(name);
                if old.params.get(name) != value {
                    changes.push(((*id, name.clone()), value.cloned()));
                }
            }
        }
        changes
    }

    /// The stored value of param `name` of node `id`: `None` for a missing node, `Some(None)`
    /// for a param at its default.
    pub fn stored_param(&self, id: NodeId, name: &Name) -> Option<Option<&Value>> {
        self.nodes.get(&id).map(|n| n.params.get(name))
    }

    /// A copy with `values` written into the nodes that exist (`None` removes the stored value);
    /// `None` when that changes nothing. Values are taken as they are: they were checked when
    /// they were first set.
    pub fn with_params<'a>(
        &self,
        values: impl IntoIterator<Item = (&'a (NodeId, Name), &'a Option<Value>)>,
    ) -> Option<Network> {
        let mut out: Option<Network> = None;
        for ((id, name), value) in values {
            let net = out.as_ref().unwrap_or(self);
            let Some(current) = net.stored_param(*id, name) else { continue };
            if current == value.as_ref() {
                continue;
            }
            let net = out.get_or_insert_with(|| self.clone());
            let Some(node) = net.nodes.get_mut(id) else { continue };
            let params = &mut Arc::make_mut(node).params;
            match value {
                Some(v) => params.insert(name.clone(), v.clone()),
                None => params.remove(name),
            };
        }
        out
    }

    /// The positions of the nodes in `ids` that differ between `self` and `after`. Nodes missing
    /// on either side are skipped.
    pub fn pos_changes(&self, after: &Network, ids: &BTreeSet<NodeId>) -> Vec<(NodeId, Option<[f32; 2]>)> {
        ids.iter()
            .filter_map(|id| Some((*id, self.nodes.get(id)?.pos, after.nodes.get(id)?.pos)))
            .filter(|(_, old, new)| old != new)
            .map(|(id, _, new)| (id, new))
            .collect()
    }

    /// A copy with `positions` written into the nodes that exist; `None` when that changes
    /// nothing. Like [`Self::with_params`], the values were checked when they were first set.
    pub fn with_positions<'a>(
        &self,
        positions: impl IntoIterator<Item = (&'a NodeId, &'a Option<[f32; 2]>)>,
    ) -> Option<Network> {
        let mut out: Option<Network> = None;
        for (id, pos) in positions {
            let net = out.as_ref().unwrap_or(self);
            if net.nodes.get(id).is_none_or(|n| n.pos == *pos) {
                continue;
            }
            let net = out.get_or_insert_with(|| self.clone());
            if let Some(node) = net.nodes.get_mut(id) {
                Arc::make_mut(node).pos = *pos;
            }
        }
        out
    }

    /// Applies a batch of operations. All or nothing: on error the network is untouched and the
    /// error names the failing step, e.g. `op 2 (set /noise1.scale): ...`.
    pub fn apply(&self, types: &NodeTypes, ops: &[Op]) -> Result<Applied, Error> {
        self.apply_with(types, ops, Params::Strict)
    }

    /// [`Self::apply`], choosing how parameters a node type does not declare are treated.
    pub fn apply_with(&self, types: &NodeTypes, ops: &[Op], params: Params) -> Result<Applied, Error> {
        let mut work =
            Applied { network: self.clone(), created: Vec::new(), touched: BTreeSet::new(), params };
        for (i, op) in ops.iter().enumerate() {
            work.apply_one(types, op).map_err(|e| e.context(format!("op {i} ({})", op.label())))?;
        }
        Ok(work)
    }

    fn require(&self, path: &NodePath) -> Result<NodeId, Error> {
        if path.is_root() {
            return Err(Error::new("the root is not a node"));
        }
        self.resolve(path).ok_or_else(|| Error::new(format!("no node at {path}")))
    }

    /// Whether `start` depends on `target` through any chain of inputs (or is it).
    fn depends_on(&self, start: NodeId, target: NodeId) -> bool {
        let mut stack = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if !seen.insert(id) {
                continue;
            }
            if let Some(n) = self.nodes.get(&id) {
                stack.extend(n.upstream());
            }
        }
        false
    }

    fn descendants(&self, id: NodeId) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::from([id]);
        let mut stack = vec![id];
        while let Some(p) = stack.pop() {
            for c in self.children(Some(p)) {
                if out.insert(c.id) {
                    stack.push(c.id);
                }
            }
        }
        out
    }

    fn name_taken(&self, parent: Option<NodeId>, name: &Name, except: Option<NodeId>) -> bool {
        self.index.get(&(parent, name.clone())).is_some_and(|id| Some(*id) != except)
    }
}

impl Applied {
    fn node_mut(&mut self, id: NodeId) -> Result<&mut NodeDef, Error> {
        self.touched.insert(id);
        let node =
            self.network.nodes.get_mut(&id).ok_or_else(|| Error::new(format!("node {id} vanished")))?;
        Ok(Arc::make_mut(node))
    }

    /// Refuses an edit of placeholder `id` ([`broken`]) unless it comes from the project file.
    fn guard(&self, id: NodeId, path: &NodePath) -> Result<(), Error> {
        match self.network.nodes.get(&id) {
            Some(node) => broken::refuse(node, &path.to_string(), self.params),
            None => Ok(()),
        }
    }

    /// Whether `id` is a placeholder and the edit comes from the project file, which may wire
    /// from it as the file says.
    fn kept_broken(&self, id: NodeId) -> bool {
        self.params == Params::Keep && self.network.nodes.get(&id).is_some_and(|n| broken::is_broken(n))
    }

    fn apply_one(&mut self, types: &NodeTypes, op: &Op) -> Result<(), Error> {
        // An exposed param of a component acts on the inner param it stands for.
        let aliased = match op {
            Op::Set { param, value } => {
                let p = self.network.resolve_alias(types, param)?;
                (&p != param).then(|| Op::Set { param: p, value: value.clone() })
            }
            Op::Reset { param } => {
                let p = self.network.resolve_alias(types, param)?;
                (&p != param).then_some(Op::Reset { param: p })
            }
            Op::Bind { param, component, from, channel } => {
                let p = self.network.resolve_alias(types, param)?;
                (&p != param).then(|| Op::Bind {
                    param: p,
                    component: *component,
                    from: from.clone(),
                    channel: channel.clone(),
                })
            }
            Op::Unbind { param, component } => {
                let p = self.network.resolve_alias(types, param)?;
                (&p != param).then_some(Op::Unbind { param: p, component: *component })
            }
            _ => None,
        };
        let op = aliased.as_ref().unwrap_or(op);
        let net = &self.network;
        match op {
            Op::Create { path, kind, params } => {
                let (Some(parent_path), Some(name)) = (path.parent(), path.name()) else {
                    return Err(Error::new("cannot create the root"));
                };
                let parent = if parent_path.is_root() {
                    None
                } else {
                    let p = net.require(&parent_path)?;
                    let pkind = net.nodes.get(&p).map(|n| n.kind.as_str()).unwrap_or_default();
                    // What sits inside a placeholder in the file loads into it, so a component
                    // whose own entry is broken keeps its nodes.
                    if !types.desc(pkind).is_some_and(|d| d.container) && !self.kept_broken(p) {
                        return Err(Error::new(format!("{parent_path} ({pkind}) cannot hold nodes")));
                    }
                    Some(p)
                };
                if kind == broken::BROKEN_KIND && self.params != Params::Keep {
                    return Err(Error::new(format!(
                        "`{kind}` stands for a node that did not load; only loading a project makes one"
                    )));
                }
                let desc = types.desc(kind).ok_or_else(|| {
                    Error::new(format!(
                        "unknown node kind `{kind}`; known: {}",
                        types.kinds().collect::<Vec<_>>().join(", ")
                    ))
                })?;
                if net.name_taken(parent, name, None) {
                    return Err(Error::new(format!("{path} exists already")));
                }
                if net.nodes.len() >= MAX_NODES {
                    return Err(Error::new(format!("a network holds at most {MAX_NODES} nodes")));
                }
                let mut checked = BTreeMap::new();
                for (pname, value) in params {
                    let value = check_param(desc, pname, value, self.params)?;
                    if desc.param_spec(pname.as_str()).is_none_or(|s| s.default != value) {
                        checked.insert(pname.clone(), value);
                    }
                }
                let id = NodeId(net.next_id);
                let def = NodeDef {
                    id,
                    name: name.clone(),
                    parent,
                    kind: kind.clone(),
                    params: checked,
                    inputs: vec![None; desc.inputs.len()],
                    previous: desc.inputs.iter().map(|i| i.previous).collect(),
                    binds: BTreeMap::new(),
                    locked: false,
                    pos: None,
                };
                self.network.next_id = self.network.next_id.saturating_add(1);
                self.network.index.insert((parent, name.clone()), id);
                self.network.nodes.insert(id, Arc::new(def));
                self.created.push(id);
                self.touched.insert(id);
            }
            Op::Delete { path } => {
                let id = net.require(path)?;
                let gone = net.descendants(id);
                let readers: Vec<NodeId> = net
                    .nodes
                    .values()
                    .filter(|n| !gone.contains(&n.id) && n.sources().any(|u| gone.contains(&u)))
                    .map(|n| n.id)
                    .collect();
                for r in readers {
                    let node = self.node_mut(r)?;
                    for slot in &mut node.inputs {
                        if slot.is_some_and(|p| gone.contains(&p.node)) {
                            *slot = None;
                        }
                    }
                    node.binds.retain(|_, b| !gone.contains(&b.node));
                }
                for g in gone {
                    if let Some(n) = self.network.nodes.remove(&g) {
                        self.network.index.remove(&(n.parent, n.name.clone()));
                    }
                    self.touched.insert(g);
                }
            }
            Op::Rename { path, name } => {
                let id = net.require(path)?;
                let parent = net.nodes.get(&id).and_then(|n| n.parent);
                if net.name_taken(parent, name, Some(id)) {
                    return Err(Error::new(format!("a sibling is already called `{name}`")));
                }
                let node = self.node_mut(id)?;
                let old = std::mem::replace(&mut node.name, name.clone());
                self.network.index.remove(&(parent, old));
                self.network.index.insert((parent, name.clone()), id);
            }
            Op::Set { param, value } => {
                let id = net.require(param.node())?;
                self.guard(id, param.node())?;
                let desc = desc_of(net, types, id)?;
                let value = check_param(desc, param.param(), value, self.params)?;
                let is_default = desc.param_spec(param.param().as_str()).is_some_and(|s| s.default == value);
                let params = &mut self.node_mut(id)?.params;
                // Only values that differ from the default are stored, so the network (and
                // later the project file) has one form per state, and setting the default is
                // the same as resetting.
                if is_default {
                    params.remove(param.param());
                } else {
                    params.insert(param.param().clone(), value);
                }
            }
            Op::Reset { param } => {
                let id = net.require(param.node())?;
                self.guard(id, param.node())?;
                let desc = desc_of(net, types, id)?;
                // A stray value (see [`Params::Keep`]) can always be removed.
                let present = net.nodes.get(&id).is_some_and(|n| n.params.contains_key(param.param()));
                if desc.param_spec(param.param().as_str()).is_none() && !desc.extra_params && !present {
                    return Err(unknown_param(desc, param.param()));
                }
                self.node_mut(id)?.params.remove(param.param());
            }
            Op::Connect { from, output, to, input } => {
                let src = net.require(from)?;
                let dst = net.require(to)?;
                self.guard(src, from)?;
                self.guard(dst, to)?;
                // A placeholder's outputs are unknown: the file's wire from it is kept as it is.
                let out_spec = if self.kept_broken(src) {
                    None
                } else {
                    Some(
                        desc_of(net, types, src)?
                            .outputs
                            .get(*output as usize)
                            .ok_or_else(|| Error::new(format!("{from} has no output {output}")))?,
                    )
                };
                let dst_desc = desc_of(net, types, dst)?;
                let in_spec = dst_desc
                    .inputs
                    .get(*input as usize)
                    .ok_or_else(|| Error::new(format!("{to} has no input {input}")))?;
                if !net.wire_allowed(types, src, dst) {
                    return Err(Error::new(format!(
                        "{from} and {to} are in different components; a wire into or out of a \
                         component goes through one of its input or output nodes (tex.in, tex.out, ...)"
                    )));
                }
                if let Some(out_spec) = out_spec
                    && out_spec.kind != in_spec.kind
                {
                    return Err(Error::new(format!(
                        "{from} output `{}` is {:?}, {to} input `{}` wants {:?}",
                        out_spec.name, out_spec.kind, in_spec.name, in_spec.kind
                    )));
                }
                // A previous-result input closes loops on purpose.
                if !in_spec.previous && net.depends_on(src, dst) {
                    return Err(Error::new(
                        "that connection would make a loop; feedback needs a feedback node",
                    ));
                }
                let port = Port { node: src, output: *output };
                let slot = self.node_mut(dst)?.inputs.get_mut(*input as usize);
                if let Some(slot) = slot {
                    *slot = Some(port);
                }
            }
            Op::Place { path, pos } => {
                let id = net.require(path)?;
                let pos = pos.map(check_pos).transpose()?;
                if net.nodes.get(&id).is_some_and(|n| n.pos != pos) {
                    self.node_mut(id)?.pos = pos;
                }
            }
            Op::Setting { name, value } => match check_setting(name, value.as_ref())? {
                Some(v) => {
                    self.network.settings.insert(name.clone(), v);
                }
                None => {
                    self.network.settings.remove(name);
                }
            },
            Op::Lock { path, locked } => {
                let id = net.require(path)?;
                self.guard(id, path)?;
                if *locked && desc_of(net, types, id)?.outputs.is_empty() {
                    return Err(Error::new(format!("{path} has no outputs to lock")));
                }
                self.node_mut(id)?.locked = *locked;
            }
            Op::Bind { param, component, from, channel } => {
                let dst = net.require(param.node())?;
                let src = net.require(from)?;
                self.guard(dst, param.node())?;
                self.guard(src, from)?;
                let desc = desc_of(net, types, dst)?;
                let name = param.param();
                match desc.param_spec(name.as_str()) {
                    Some(spec) => spec.check_bindable(*component)?,
                    // Checked against the instance's own declaration when it cooks.
                    None if desc.extra_params || self.params == Params::Keep => {}
                    None => return Err(unknown_param(desc, name)),
                }
                if !self.kept_broken(src)
                    && !desc_of(net, types, src)?.outputs.iter().any(|o| o.kind == DataKind::Chan)
                {
                    return Err(Error::new(format!("{from} has no chan output to bind to")));
                }
                if src == dst || net.depends_on(src, dst) {
                    return Err(Error::new(format!(
                        "binding {param} to {from} would make a loop: {from} depends on {}",
                        param.node()
                    )));
                }
                let part = ParamPart::new(name.clone(), *component);
                self.node_mut(dst)?.binds.insert(part, Bind { node: src, channel: channel.clone() });
            }
            Op::Unbind { param, component } => {
                let dst = net.require(param.node())?;
                self.guard(dst, param.node())?;
                let part = ParamPart::new(param.param().clone(), *component);
                if net.nodes.get(&dst).is_some_and(|n| n.binds.contains_key(&part)) {
                    self.node_mut(dst)?.binds.remove(&part);
                }
            }
            Op::Disconnect { to, input } => {
                let dst = net.require(to)?;
                self.guard(dst, to)?;
                let slot = self.node_mut(dst)?.inputs.get_mut(*input as usize);
                match slot {
                    Some(slot) => *slot = None,
                    None => return Err(Error::new(format!("{to} has no input {input}"))),
                }
            }
        }
        Ok(())
    }
}

/// The farthest a node may sit from the origin of its plane, in graph units.
pub const MAX_POS: f32 = 1.0e7;

/// A position as stored: finite, within [`MAX_POS`], in whole graph units (so the project file
/// has one form per place and a drag does not write float noise).
pub fn check_pos(pos: [f32; 2]) -> Result<[f32; 2], Error> {
    if pos.iter().any(|c| !c.is_finite() || c.abs() > MAX_POS) {
        return Err(Error::new(format!(
            "a position is two finite numbers within {MAX_POS}, got [{}, {}]",
            pos[0], pos[1]
        )));
    }
    // `+ 0.0` turns a rounded -0.0 into 0.0.
    Ok(pos.map(|c| c.round() + 0.0))
}

fn desc_of<'t>(net: &Network, types: &'t NodeTypes, id: NodeId) -> Result<&'t NodeDesc, Error> {
    let kind = net.nodes.get(&id).map(|n| n.kind.as_str()).unwrap_or_default();
    types.desc(kind).ok_or_else(|| Error::new(format!("node {id} has unknown kind `{kind}`")))
}

/// The value param `name` of a node of type `desc` stores for `value`, or why it cannot.
pub fn check_param(desc: &NodeDesc, name: &Name, value: &Value, params: Params) -> Result<Value, Error> {
    match desc.param_spec(name.as_str()) {
        Some(spec) => spec.check(value),
        None if desc.extra_params || params == Params::Keep => check_extra(name, value),
        None => Err(unknown_param(desc, name)),
    }
}

/// A parameter the node type does not declare but accepts ([`NodeDesc::extra_params`]): its
/// meaning comes from the node at cook time, so only what holds for every value is checked here.
fn check_extra(name: &Name, value: &Value) -> Result<Value, Error> {
    if value.components().is_some_and(|c| c.iter().any(|x| !x.is_finite())) {
        return Err(Error::new(format!("`{name}` must be finite, got {value}")));
    }
    if value.as_str().is_some_and(|s| s.len() > penumbra_core::param::MAX_TEXT) {
        return Err(Error::new(format!("`{name}` is too long")));
    }
    Ok(value.clone())
}

fn unknown_param(desc: &NodeDesc, name: &Name) -> Error {
    let known: Vec<&str> = desc.params.iter().map(|p| p.name.as_str()).collect();
    Error::new(format!("`{}` has no param `{name}`; params: {}", desc.kind, known.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{create, test_types};

    fn p(s: &str) -> NodePath {
        NodePath::parse(s).unwrap()
    }

    fn pp(s: &str) -> ParamPath {
        ParamPath::parse(s).unwrap()
    }

    #[test]
    fn positions_are_whole_units_and_nonsense_is_refused() {
        let types = test_types();
        let net = Network::new().apply(&types, &[create("/a", "test.const")]).unwrap().network;
        let place = |pos: Option<[f32; 2]>| Op::Place { path: p("/a"), pos };
        let a = net.resolve(&p("/a")).unwrap();
        let applied = net.apply(&types, &[place(Some([10.4, -0.2]))]).unwrap();
        assert_eq!(applied.network.node(a).unwrap().pos, Some([10.0, 0.0]));
        assert!(applied.touched.contains(&a));
        for bad in [[f32::NAN, 0.0], [0.0, f32::INFINITY], [2.0e7, 0.0]] {
            let e = net.apply(&types, &[place(Some(bad))]).unwrap_err();
            assert!(e.0.contains("finite"), "{e}");
        }
        let back = applied.network.apply(&types, &[place(None)]).unwrap().network;
        assert_eq!(back.node(a).unwrap().pos, None);
        assert_eq!(back, net, "unplaced again is the network it was");
        // Placing where it is already touches nothing.
        let again = applied.network.apply(&types, &[place(Some([10.0, 0.0]))]).unwrap();
        assert!(again.touched.is_empty());
        assert!(net.apply(&types, &[Op::Place { path: p("/nope"), pos: None }]).is_err());
    }

    #[test]
    fn create_set_connect() {
        let types = test_types();
        let net = Network::new();
        let ops = [
            create("/a", "test.const"),
            create("/b", "test.const"),
            create("/sum", "test.add"),
            Op::Set { param: pp("/a.value"), value: Value::Int(2) },
            Op::Connect { from: p("/a"), output: 0, to: p("/sum"), input: 0 },
            Op::Connect { from: p("/b"), output: 0, to: p("/sum"), input: 1 },
        ];
        let applied = net.apply(&types, &ops).unwrap();
        assert_eq!(applied.created.len(), 3);
        let n = &applied.network;
        let a = n.resolve(&p("/a")).unwrap();
        let sum = n.node(n.resolve(&p("/sum")).unwrap()).unwrap();
        assert_eq!(sum.inputs[0], Some(Port { node: a, output: 0 }));
        // Coerced to the declared type.
        assert_eq!(n.node(a).unwrap().params.get("value"), Some(&Value::Float(2.0)));
        assert_eq!(n.path_of(a).unwrap(), p("/a"));
        // The original is untouched.
        assert!(net.is_empty());
    }

    #[test]
    fn batches_are_all_or_nothing() {
        let types = test_types();
        let net = Network::new().apply(&types, &[create("/a", "test.const")]).unwrap().network;
        let err = net
            .apply(
                &types,
                &[create("/b", "test.const"), Op::Set { param: pp("/b.nope"), value: Value::Int(1) }],
            )
            .unwrap_err();
        assert!(err.0.starts_with("op 1 (set /b.nope)"), "{err}");
        assert!(err.0.contains("params: value"), "{err}");
        assert_eq!(net.len(), 1);
    }

    #[test]
    fn refusals() {
        let types = test_types();
        let base = Network::new()
            .apply(
                &types,
                &[create("/a", "test.const"), create("/sum", "test.add"), create("/out", "test.sink")],
            )
            .unwrap()
            .network;
        let refuse = |op: Op| {
            let e = base.apply(&types, &[op]).unwrap_err();
            e.0
        };
        assert!(refuse(create("/a", "test.const")).contains("exists"));
        assert!(refuse(create("/x", "tex.nope")).contains("unknown node kind"));
        assert!(refuse(create("/a/x", "test.const")).contains("cannot hold nodes"));
        assert!(refuse(create("/missing/x", "test.const")).contains("no node at /missing"));
        assert!(
            refuse(Op::Set { param: pp("/a.value"), value: Value::Text("x".into()) }).contains("expects")
        );
        assert!(
            refuse(Op::Connect { from: p("/a"), output: 1, to: p("/sum"), input: 0 }).contains("no output 1")
        );
        assert!(
            refuse(Op::Connect { from: p("/a"), output: 0, to: p("/sum"), input: 5 }).contains("no input 5")
        );
        assert!(
            refuse(Op::Connect { from: p("/out"), output: 0, to: p("/sum"), input: 0 }).contains("no output")
        );
        assert!(refuse(Op::Delete { path: NodePath::root() }).contains("root"));
        assert!(refuse(Op::Disconnect { to: p("/sum"), input: 9 }).contains("no input 9"));
    }

    #[test]
    fn binds() {
        let types = test_types();
        let bind = |param: &str, from: &str| Op::Bind {
            param: pp(param),
            component: None,
            from: p(from),
            channel: None,
        };
        let base = Network::new()
            .apply(
                &types,
                &[
                    create("/a", "test.const"),
                    create("/b", "test.const"),
                    create("/sum", "test.add"),
                    create("/t", "test.tex"),
                    Op::Connect { from: p("/a"), output: 0, to: p("/sum"), input: 0 },
                ],
            )
            .unwrap()
            .network;
        let n = base.apply(&types, &[bind("/b.value", "/sum")]).unwrap().network;
        let b = n.node(n.resolve(&p("/b")).unwrap()).unwrap();
        let key = ParamPart::parse("value").unwrap();
        assert_eq!(b.binds.get(&key).map(|x| x.node), n.resolve(&p("/sum")));
        assert!(b.is_bound("value"));
        assert_eq!(n.consumers(n.resolve(&p("/sum")).unwrap()).count(), 1);
        // /sum reads /a; binding /a to /b (which reads /sum) closes a loop, so does a
        // connection from /b into /sum.
        let e = n.apply(&types, &[bind("/a.value", "/b")]).unwrap_err();
        assert!(e.0.contains("loop"), "{e}");
        let e = n.apply(&types, &[Op::Connect { from: p("/b"), output: 0, to: p("/sum"), input: 1 }]);
        assert!(e.unwrap_err().0.contains("loop"));
        assert!(base.apply(&types, &[bind("/a.value", "/a")]).unwrap_err().0.contains("loop"));
        assert!(base.apply(&types, &[bind("/a.nope", "/b")]).unwrap_err().0.contains("no param"));
        assert!(base.apply(&types, &[bind("/a.value", "/t")]).unwrap_err().0.contains("no chan output"));
        let comp = Op::Bind { param: pp("/a.value"), component: Some(0), from: p("/b"), channel: None };
        assert!(base.apply(&types, &[comp]).unwrap_err().0.contains("as a whole"));
        // Deleting the source drops the binding; unbinding something unbound changes nothing.
        let gone = n.apply(&types, &[Op::Delete { path: p("/sum") }]).unwrap().network;
        assert!(gone.node(gone.resolve(&p("/b")).unwrap()).unwrap().binds.is_empty());
        let un = Op::Unbind { param: pp("/b.value"), component: None };
        assert!(
            n.apply(&types, std::slice::from_ref(&un)).unwrap().network.nodes().all(|n| n.binds.is_empty())
        );
        assert!(base.apply(&types, &[un]).unwrap().touched.is_empty());
        // JSON as the control channel sends it.
        let op: Op = serde_json::from_str(
            r#"{"op": "bind", "param": "/t.translate", "component": 1, "from": "/lfo", "channel": "x"}"#,
        )
        .unwrap();
        assert_eq!(op.label(), "bind /t.translate.1 to /lfo:x");
    }

    #[test]
    fn kinds_must_match() {
        let types = test_types();
        let ops = [
            create("/t", "test.tex"),
            create("/sum", "test.add"),
            Op::Connect { from: p("/t"), output: 0, to: p("/sum"), input: 0 },
        ];
        let e = Network::new().apply(&types, &ops).unwrap_err();
        assert!(e.0.contains("wants Chan"), "{e}");
    }

    #[test]
    fn loops_are_refused() {
        let types = test_types();
        let ops = [
            create("/a", "test.add"),
            create("/b", "test.add"),
            Op::Connect { from: p("/a"), output: 0, to: p("/b"), input: 0 },
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        let back = Op::Connect { from: p("/b"), output: 0, to: p("/a"), input: 0 };
        assert!(net.apply(&types, &[back]).unwrap_err().0.contains("loop"));
        let own = Op::Connect { from: p("/a"), output: 0, to: p("/a"), input: 1 };
        assert!(net.apply(&types, &[own]).unwrap_err().0.contains("loop"));
    }

    #[test]
    fn delete_clears_connections_and_children() {
        let types = test_types();
        let ops = [
            create("/g", "test.group"),
            create("/g/a", "test.out"),
            create("/sum", "test.add"),
            Op::Connect { from: p("/g/a"), output: 0, to: p("/sum"), input: 0 },
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        assert_eq!(net.path_of(net.resolve(&p("/g/a")).unwrap()).unwrap(), p("/g/a"));
        let applied = net.apply(&types, &[Op::Delete { path: p("/g") }]).unwrap();
        let n = &applied.network;
        assert_eq!(n.len(), 1);
        assert_eq!(n.node(n.resolve(&p("/sum")).unwrap()).unwrap().inputs[0], None);
        assert_eq!(applied.touched.len(), 3);
    }

    #[test]
    fn only_nodes_with_outputs_lock() {
        let types = test_types();
        let net =
            Network::new().apply(&types, &[create("/a", "test.const"), create("/o", "test.sink")]).unwrap();
        let n = &net.network;
        assert!(
            n.apply(&types, &[Op::Lock { path: p("/o"), locked: true }])
                .unwrap_err()
                .0
                .contains("no outputs")
        );
        let locked = n.apply(&types, &[Op::Lock { path: p("/a"), locked: true }]).unwrap().network;
        assert!(locked.node(locked.resolve(&p("/a")).unwrap()).unwrap().locked);
    }

    #[test]
    fn ids_are_never_reused() {
        let types = test_types();
        let n1 = Network::new().apply(&types, &[create("/a", "test.const")]).unwrap();
        let n2 =
            n1.network.apply(&types, &[Op::Delete { path: p("/a") }, create("/a", "test.const")]).unwrap();
        assert_ne!(n1.created, n2.created);
    }

    #[test]
    fn rename_and_reset() {
        let types = test_types();
        let ops = [
            create("/a", "test.const"),
            create("/b", "test.const"),
            Op::Set { param: pp("/a.value"), value: Value::Float(3.0) },
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        assert!(net.apply(&types, &[Op::Rename { path: p("/a"), name: "b".parse().unwrap() }]).is_err());
        let renamed =
            net.apply(&types, &[Op::Rename { path: p("/a"), name: "c".parse().unwrap() }]).unwrap().network;
        assert!(renamed.resolve(&p("/c")).is_some());
        let reset = renamed.apply(&types, &[Op::Reset { param: pp("/c.value") }]).unwrap().network;
        assert!(reset.node(reset.resolve(&p("/c")).unwrap()).unwrap().params.is_empty());
    }

    #[test]
    fn stray_params_are_kept_only_when_asked_and_can_be_removed() {
        let types = test_types();
        let ops = [Op::Create {
            path: p("/a"),
            kind: "test.const".into(),
            params: [("gone".parse().unwrap(), Value::Float(0.7))].into(),
        }];
        let err = Network::new().apply(&types, &ops).unwrap_err().0;
        assert!(err.contains("has no param `gone`"), "{err}");
        let net = Network::new().apply_with(&types, &ops, Params::Keep).unwrap().network;
        let a = net.resolve(&p("/a")).unwrap();
        assert_eq!(net.node(a).unwrap().params.get("gone"), Some(&Value::Float(0.7)));
        // Setting it stays refused; removing it works.
        assert!(net.apply(&types, &[Op::Set { param: pp("/a.gone"), value: Value::Float(1.0) }]).is_err());
        let clean = net.apply(&types, &[Op::Reset { param: pp("/a.gone") }]).unwrap().network;
        assert!(clean.node(a).unwrap().params.is_empty());
        assert!(clean.apply(&types, &[Op::Reset { param: pp("/a.gone") }]).is_err(), "gone now");
    }

    #[test]
    fn unchanged_nodes_are_shared() {
        let types = test_types();
        let net =
            Network::new().apply(&types, &[create("/a", "test.const"), create("/b", "test.const")]).unwrap();
        let n = &net.network;
        let next = n.apply(&types, &[Op::Set { param: pp("/a.value"), value: Value::Float(1.0) }]).unwrap();
        let b = n.resolve(&p("/b")).unwrap();
        assert!(Arc::ptr_eq(n.node(b).unwrap(), next.network.node(b).unwrap()));
        assert_eq!(next.touched.len(), 1);
    }

    fn connect(from: &str, to: &str) -> Op {
        Op::Connect { from: p(from), output: 0, to: p(to), input: 0 }
    }

    /// `/src`, `/sink`, component `/c` with input `/c/in1`, output `/c/out1`, inner `/c/a`, and
    /// a second component `/d` with input `/d/in1`.
    fn comps() -> Network {
        let types = test_types();
        let ops = [
            create("/src", "test.const"),
            create("/sink", "test.add"),
            create("/c", "test.comp"),
            create("/c/in1", "test.in"),
            create("/c/out1", "test.out"),
            create("/c/a", "test.add"),
            create("/d", "test.comp"),
            create("/d/in1", "test.in"),
            create("/d/b", "test.add"),
        ];
        Network::new().apply(&types, &ops).unwrap().network
    }

    #[test]
    fn wires_cross_component_edges_only_through_ports() {
        let types = test_types();
        let net = comps();
        let ok = |ops: &[Op]| net.apply(&types, ops).map(|_| ()).map_err(|e| e.0);
        // Outside into the input node, inside along, out through the output node.
        ok(&[connect("/src", "/c/in1"), connect("/c/in1", "/c/a"), connect("/c/a", "/c/out1")]).unwrap();
        ok(&[connect("/c/out1", "/sink")]).unwrap();
        // One component's output into another's input; an output read inside too.
        ok(&[connect("/c/out1", "/d/in1")]).unwrap();
        ok(&[connect("/c/out1", "/c/a")]).unwrap();
        for (from, to) in [
            ("/src", "/c/a"),    // into the middle
            ("/c/a", "/sink"),   // out of the middle
            ("/c/a", "/c/in1"),  // an input node is fed from outside only
            ("/c/out1", "/d/b"), // past the other component's input
            ("/d/in1", "/c/a"),  // between two components' insides
        ] {
            let e = ok(&[connect(from, to)]).unwrap_err();
            assert!(e.contains("different components"), "{from} -> {to}: {e}");
        }
        let (ins, outs) = net.comp_ports(&types, net.resolve(&p("/c")).unwrap());
        assert_eq!(ins, vec![net.resolve(&p("/c/in1")).unwrap()]);
        assert_eq!(outs, vec![net.resolve(&p("/c/out1")).unwrap()]);
    }

    #[test]
    fn exposed_params_are_aliases_of_inner_ones() {
        let types = test_types();
        let net = comps()
            .apply(
                &types,
                &[
                    create("/c/k", "test.const"),
                    create("/c/sub", "test.comp"),
                    create("/c/sub/x", "test.const"),
                    Op::Set { param: pp("/c/sub.expose"), value: Value::Text("deep = x.value".into()) },
                    Op::Set {
                        param: pp("/c.expose"),
                        value: Value::Text(
                            "# what the outside sees\nlevel = k.value\nsub.deep\nbad = /src.value\n= nope\n"
                                .into(),
                        ),
                    },
                ],
            )
            .unwrap()
            .network;
        let c = net.resolve(&p("/c")).unwrap();
        let mut errors = Vec::new();
        let ex = net.exposed_with_errors(&types, c, &mut errors);
        let names: Vec<&str> = ex.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["level", "deep", "bad"]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(ex[2].target.is_err());
        // Setting the alias sets the inner value, through two levels.
        let set = |param: &str, v: f64| Op::Set { param: pp(param), value: Value::Float(v) };
        let n = net.apply(&types, &[set("/c.level", 2.0), set("/c.deep", 3.0)]).unwrap();
        let value = |path: &str| {
            let id = n.network.resolve(&p(path)).unwrap();
            n.network.node(id).unwrap().params.get("value").cloned()
        };
        assert_eq!(value("/c/k"), Some(Value::Float(2.0)));
        assert_eq!(value("/c/sub/x"), Some(Value::Float(3.0)));
        assert!(!n.touched.contains(&c), "the component itself holds no value");
        let e = net.apply(&types, &[set("/c.nope", 1.0)]).unwrap_err().0;
        assert!(e.contains("params: expose, level, deep, bad"), "{e}");
        assert!(net.apply(&types, &[set("/c.bad", 1.0)]).unwrap_err().0.contains("inside the component"));
        // Binding and resetting follow the alias too.
        let bound = n
            .network
            .apply(
                &types,
                &[
                    Op::Bind { param: pp("/c.level"), component: None, from: p("/src"), channel: None },
                    Op::Reset { param: pp("/c.deep") },
                ],
            )
            .unwrap()
            .network;
        let k = bound.node(bound.resolve(&p("/c/k")).unwrap()).unwrap();
        assert!(k.is_bound("value"));
        assert!(bound.node(bound.resolve(&p("/c/sub/x")).unwrap()).unwrap().params.is_empty());
    }

    #[test]
    fn names_sort_naturally() {
        let mut v = vec!["in10", "in2", "b", "in1", "a10b", "a9b", "in02"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a9b", "a10b", "b", "in1", "in02", "in2", "in10"]);
    }

    #[test]
    fn ops_from_json() {
        let ops: Vec<Op> = serde_json::from_str(
            r#"[{"op":"create","path":"/n","kind":"test.const","params":{"value":2}},
                {"op":"connect","from":"/n","to":"/s"},
                {"op":"set","param":"/n.value","value":0.5}]"#,
        )
        .unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[1], Op::Connect { output: 0, input: 0, .. }));
        assert!(serde_json::from_str::<Op>(r#"{"op":"set","param":"/n","value":1}"#).is_err());
        assert!(serde_json::from_str::<Op>(r#"{"op":"explode"}"#).is_err());
    }
}
