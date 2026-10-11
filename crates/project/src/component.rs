//! Components as content: grouping nodes into a component and dissolving one again, and the
//! component file that carries one component from project to project.
//!
//! Each operation is a pure function from one [`Content`] to another; the caller turns the pair
//! into an edit with [`crate::diff`], so it is one undoable history entry and nodes the operation
//! does not touch keep their ids. Moved nodes are created anew (fresh operators), as a rename is.
//!
//! A wire that crosses a component's edge runs through one of its input or output nodes
//! (`tex.in`, `chan.out`, ...; [`PortRole`]). Grouping adds those where wires cross; dissolving
//! takes them out and joins the wires again. Bindings and node references may cross edges freely;
//! their paths are rewritten so they keep naming the same nodes.

use std::collections::{BTreeMap, BTreeSet};

use penumbra_core::{Error, Name, NodePath, Value};
use penumbra_graph::{DataKind, NodeTypes, PortRole, natural_cmp};

use crate::content::{BindRef, Content, NodeContent, PortRef};
use crate::file;

/// What a component file is called: `<name>.comp.ron`.
pub const FILE_SUFFIX: &str = ".comp.ron";

/// What [`collapse`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Collapsed {
    pub content: Content,
    /// Old path to new path of every moved node (descendants included).
    pub moved: BTreeMap<NodePath, NodePath>,
    /// The input and output nodes made for wires that cross the new edge.
    pub ports: Vec<NodePath>,
}

/// What [`expand`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Expanded {
    pub content: Content,
    pub moved: BTreeMap<NodePath, NodePath>,
}

/// `path` with its leading `from` replaced by `to`; `None` when it does not start with `from`.
pub(crate) fn rebase(path: &NodePath, from: &NodePath, to: &NodePath) -> Option<NodePath> {
    let rest = path.names().strip_prefix(from.names())?;
    let mut names = to.names().to_vec();
    names.extend(rest.iter().cloned());
    NodePath::from_names(names).ok()
}

pub(crate) fn is_within(path: &NodePath, ancestor: &NodePath) -> bool {
    path.names().starts_with(ancestor.names())
}

/// The kind registered for a component port of data kind `kind` and role `role`.
fn port_kind(types: &NodeTypes, kind: DataKind, role: PortRole) -> Option<String> {
    types
        .kinds()
        .filter_map(|k| types.desc(k))
        .find(|d| d.port == Some(role) && d.inputs.first().is_some_and(|i| i.kind == kind))
        .map(|d| d.kind.clone())
}

pub(crate) fn role_of(types: &NodeTypes, content: &Content, path: &NodePath) -> Option<PortRole> {
    content.nodes.get(path).and_then(|n| types.desc(&n.kind)).and_then(|d| d.port)
}

/// Data kind of output `port`.
fn kind_of(types: &NodeTypes, content: &Content, port: &PortRef) -> Option<DataKind> {
    let node = content.nodes.get(&port.node)?;
    types.desc(&node.kind)?.outputs.get(port.output as usize).map(|o| o.kind)
}

/// A name under `parent` not in `taken`, from `base`: `base`, else `base1`, `base2`, ...
fn free_name(base: &str, taken: &BTreeSet<Name>) -> Result<Name, Error> {
    let clean: String = base.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(56).collect();
    let clean = if clean.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        clean
    } else {
        format!("n{clean}")
    };
    let first = Name::new(&clean)?;
    if !taken.contains(&first) {
        return Ok(first);
    }
    for i in 1..10_000 {
        let n = Name::new(&format!("{clean}{i}"))?;
        if !taken.contains(&n) {
            return Ok(n);
        }
    }
    Err(Error::new(format!("no free name for `{base}`")))
}

pub(crate) fn children_names(content: &Content, parent: &NodePath) -> BTreeSet<Name> {
    content
        .nodes
        .keys()
        .filter(|p| p.parent().as_ref() == Some(parent))
        .filter_map(|p| p.name().cloned())
        .collect()
}

/// The node a node reference (`tex.select`'s `node`) of the node at `owner` names, by the rule of
/// [`NodePath::resolve_ref`] on `owner`'s level.
fn resolve_ref(owner: &NodePath, text: &str) -> Option<NodePath> {
    if text.trim().is_empty() {
        return None;
    }
    owner.parent()?.resolve_ref(text).ok()
}

/// How `owner` names `target` ([`NodePath::write_ref`] on `owner`'s level).
fn write_ref(owner: &NodePath, target: &NodePath) -> String {
    owner.parent().map_or_else(|| target.to_string(), |level| level.write_ref(target))
}

/// Rewrites every node reference param in `new` (keyed by new paths) that named a node of `old`
/// (keyed by old paths) which moved. `back` maps a new path to its old one.
pub(crate) fn fix_refs(
    types: &NodeTypes,
    new: &mut Content,
    back: &BTreeMap<NodePath, NodePath>,
    moved: &BTreeMap<NodePath, NodePath>,
) {
    for (path, node) in &mut new.nodes {
        let Some(desc) = types.desc(&node.kind) else { continue };
        let old_owner = back.get(path).unwrap_or(path);
        for spec in desc.params.iter().filter(|s| s.node_ref) {
            let Ok(name) = Name::new(&spec.name) else { continue };
            let Some(text) = node.params.get(&name).and_then(Value::as_str) else { continue };
            let Some(target) = resolve_ref(old_owner, text) else { continue };
            let new_target = moved.get(&target).unwrap_or(&target);
            let fixed = write_ref(path, new_target);
            if fixed != text.trim() && (moved.contains_key(&target) || moved.contains_key(old_owner)) {
                node.params.insert(name, Value::Text(fixed));
            }
        }
    }
}

/// Moves `members` (siblings, with everything inside them) into a new component at `at` of kind
/// `comp_kind`, which must sit beside them. Wires between the members and the rest run through
/// new input and output nodes of the component, one per source, named after it: `noise1_in`,
/// `level1_out` (`level1_mask_out` for an output other than the first).
pub fn collapse(
    base: &Content,
    types: &NodeTypes,
    members: &[NodePath],
    at: &NodePath,
    comp_kind: &str,
) -> Result<Collapsed, Error> {
    let Some(first) = members.first() else { return Err(Error::new("nothing to group")) };
    let level = first.parent().ok_or_else(|| Error::new("the root cannot be grouped"))?;
    for m in members {
        if !base.nodes.contains_key(m) {
            return Err(Error::new(format!("no node at {m}")));
        }
        if m.parent().as_ref() != Some(&level) {
            return Err(Error::new(format!(
                "{m} is not beside {first}; only nodes side by side are grouped"
            )));
        }
        if role_of(types, base, m).is_some() && !level.is_root() {
            return Err(Error::new(format!(
                "{m} is an input or output of {level} and stays at its edge; group the nodes between them"
            )));
        }
    }
    if at.parent().as_ref() != Some(&level) {
        return Err(Error::new(format!("the component must sit beside the nodes, in {level}")));
    }
    if base.nodes.contains_key(at) && !members.contains(at) {
        return Err(Error::new(format!("{at} exists already")));
    }
    if !types.desc(comp_kind).is_some_and(|d| d.container) {
        return Err(Error::new(format!("`{comp_kind}` cannot hold nodes")));
    }

    // Old path to new path of every moved node.
    let mut moved: BTreeMap<NodePath, NodePath> = BTreeMap::new();
    for path in base.nodes.keys() {
        for m in members {
            if is_within(path, m)
                && let Some(new) = rebase(path, &level, at)
            {
                moved.insert(path.clone(), new);
            }
        }
    }
    let inside = |p: &NodePath| moved.contains_key(p);
    let remap = |p: &NodePath| moved.get(p).cloned().unwrap_or_else(|| p.clone());

    let mut out = Content::default();
    out.nodes.insert(
        at.clone(),
        NodeContent {
            kind: comp_kind.to_string(),
            params: BTreeMap::new(),
            inputs: Vec::new(),
            binds: BTreeMap::new(),
            locked: false,
        },
    );
    for (path, node) in &base.nodes {
        out.nodes.insert(remap(path), node.clone());
    }

    // Wires across the new edge, one port node per source.
    let mut taken: BTreeSet<Name> = moved
        .values()
        .filter(|p| p.parent().as_ref() == Some(at))
        .filter_map(|p| p.name().cloned())
        .collect();
    let mut in_ports: BTreeMap<(NodePath, u32), NodePath> = BTreeMap::new();
    let mut out_ports: BTreeMap<(NodePath, u32), NodePath> = BTreeMap::new();
    let mut ports: Vec<NodePath> = Vec::new();
    let mut port_nodes: Vec<(NodePath, NodeContent)> = Vec::new();
    for (path, node) in &base.nodes {
        let new_path = remap(path);
        for (i, slot) in node.inputs.iter().enumerate() {
            let Some(src) = slot else { continue };
            let (reader_in, source_in) = (inside(path), inside(&src.node));
            let replacement = match (reader_in, source_in) {
                (true, true) => Some(PortRef { node: remap(&src.node), output: src.output }),
                (false, false) => None,
                (true, false) => {
                    let key = (src.node.clone(), src.output);
                    let port = match in_ports.get(&key) {
                        Some(p) => p.clone(),
                        None => {
                            let kind = kind_of(types, base, src)
                                .and_then(|k| port_kind(types, k, PortRole::In))
                                .ok_or_else(|| {
                                    Error::new(format!("no input node for the kind of {}", src.node))
                                })?;
                            let base_name = format!("{}_in", port_name(types, base, src));
                            let name = free_name(&base_name, &taken)?;
                            taken.insert(name.clone());
                            let p = at.child(name)?;
                            port_nodes.push((p.clone(), port_node(&kind, Some(src.clone()))));
                            in_ports.insert(key, p.clone());
                            ports.push(p.clone());
                            p
                        }
                    };
                    Some(PortRef { node: port, output: 0 })
                }
                (false, true) => {
                    let key = (src.node.clone(), src.output);
                    let port = match out_ports.get(&key) {
                        Some(p) => p.clone(),
                        None => {
                            let kind = kind_of(types, base, src)
                                .and_then(|k| port_kind(types, k, PortRole::Out))
                                .ok_or_else(|| {
                                    Error::new(format!("no output node for the kind of {}", src.node))
                                })?;
                            let base_name = format!("{}_out", port_name(types, base, src));
                            let name = free_name(&base_name, &taken)?;
                            taken.insert(name.clone());
                            let p = at.child(name)?;
                            let inner = PortRef { node: remap(&src.node), output: src.output };
                            port_nodes.push((p.clone(), port_node(&kind, Some(inner))));
                            out_ports.insert(key, p.clone());
                            ports.push(p.clone());
                            p
                        }
                    };
                    Some(PortRef { node: port, output: 0 })
                }
            };
            if let Some(r) = replacement
                && let Some(slot) = out.nodes.get_mut(&new_path).and_then(|n| n.inputs.get_mut(i))
            {
                *slot = Some(r);
            }
        }
        // Bindings keep naming the same nodes.
        if let Some(n) = out.nodes.get_mut(&new_path) {
            for b in n.binds.values_mut() {
                b.node = remap(&b.node);
            }
        }
    }
    for (p, n) in port_nodes {
        out.nodes.insert(p, n);
    }
    let back: BTreeMap<NodePath, NodePath> = moved.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    fix_refs(types, &mut out, &back, &moved);
    Ok(Collapsed { content: out, moved, ports })
}

/// A port's name after its source: the node's name, with the output's name when it is not the
/// first.
fn port_name(types: &NodeTypes, content: &Content, src: &PortRef) -> String {
    let node = src.node.name().map(|n| n.to_string()).unwrap_or_else(|| "in".into());
    if src.output == 0 {
        return node;
    }
    let output = content
        .nodes
        .get(&src.node)
        .and_then(|n| types.desc(&n.kind))
        .and_then(|d| d.outputs.get(src.output as usize))
        .map(|o| o.name.clone())
        .unwrap_or_else(|| src.output.to_string());
    format!("{node}_{output}")
}

fn port_node(kind: &str, source: Option<PortRef>) -> NodeContent {
    NodeContent {
        kind: kind.to_string(),
        params: BTreeMap::new(),
        inputs: vec![source],
        binds: BTreeMap::new(),
        locked: false,
    }
}

/// Dissolves the component at `comp`: its nodes move up beside where it was (renamed when a
/// name is taken there), its input and output nodes go and the wires through them are joined.
pub fn expand(base: &Content, types: &NodeTypes, comp: &NodePath) -> Result<Expanded, Error> {
    let node = base.nodes.get(comp).ok_or_else(|| Error::new(format!("no node at {comp}")))?;
    if !types.desc(&node.kind).is_some_and(|d| d.container) {
        return Err(Error::new(format!("{comp} ({}) is not a component", node.kind)));
    }
    let level = comp.parent().ok_or_else(|| Error::new("the root is not a component"))?;
    let children: Vec<NodePath> =
        base.nodes.keys().filter(|p| p.parent().as_ref() == Some(comp)).cloned().collect();
    let ports: BTreeSet<NodePath> =
        children.iter().filter(|c| role_of(types, base, c).is_some()).cloned().collect();

    // New names for the children that stay, beside the component's siblings.
    let mut taken = children_names(base, &level);
    if let Some(n) = comp.name() {
        taken.remove(n);
    }
    let mut moved: BTreeMap<NodePath, NodePath> = BTreeMap::new();
    let mut stay: Vec<&NodePath> = children.iter().filter(|c| !ports.contains(*c)).collect();
    stay.sort_by(|a, b| natural_cmp(&a.to_string(), &b.to_string()));
    for child in stay {
        let Some(name) = child.name() else { continue };
        let fresh = free_name(name.as_str(), &taken)?;
        taken.insert(fresh.clone());
        let to = level.child(fresh)?;
        for path in base.nodes.keys().filter(|p| is_within(p, child)) {
            if let Some(new) = rebase(path, child, &to) {
                moved.insert(path.clone(), new);
            }
        }
    }

    // Where a read of a port node really comes from: follow the dissolved ports.
    let through = |mut port: PortRef| -> Option<PortRef> {
        for _ in 0..4 {
            if !ports.contains(&port.node) {
                return Some(port);
            }
            port = base.nodes.get(&port.node)?.inputs.first().cloned().flatten()?;
        }
        None
    };
    let remap = |p: &NodePath| moved.get(p).cloned().unwrap_or_else(|| p.clone());

    let mut out = Content::default();
    for (path, node) in &base.nodes {
        if path == comp || ports.contains(path) {
            continue;
        }
        let mut node = node.clone();
        for slot in &mut node.inputs {
            *slot =
                slot.clone().and_then(through).map(|p| PortRef { node: remap(&p.node), output: p.output });
        }
        let mut binds = BTreeMap::new();
        for (part, b) in &node.binds {
            let src = if ports.contains(&b.node) {
                through(PortRef { node: b.node.clone(), output: 0 }).map(|p| p.node)
            } else {
                Some(b.node.clone())
            };
            if let Some(src) = src {
                binds.insert(part.clone(), BindRef { node: remap(&src), channel: b.channel.clone() });
            }
        }
        node.binds = binds;
        out.nodes.insert(remap(path), node);
    }
    let back: BTreeMap<NodePath, NodePath> = moved.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    fix_refs(types, &mut out, &back, &moved);
    Ok(Expanded { content: out, moved })
}

/// What [`extract`] left out: references from inside the component to nodes outside it, which a
/// component file cannot carry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dropped {
    pub lines: Vec<String>,
}

/// The component at `comp` with everything inside it, moved to the top level (`/<its name>`),
/// for a component file. Wires and bindings from outside are left out and listed.
pub fn extract(base: &Content, types: &NodeTypes, comp: &NodePath) -> Result<(Content, Dropped), Error> {
    let node = base.nodes.get(comp).ok_or_else(|| Error::new(format!("no node at {comp}")))?;
    if !types.desc(&node.kind).is_some_and(|d| d.container) {
        return Err(Error::new(format!("{comp} ({}) is not a component", node.kind)));
    }
    let name = comp.name().cloned().ok_or_else(|| Error::new("the root is not a component"))?;
    let top = NodePath::root().child(name)?;
    let mut dropped = Dropped::default();
    let mut out = Content::default();
    for (path, node) in base.nodes.iter().filter(|(p, _)| is_within(p, comp)) {
        let Some(new_path) = rebase(path, comp, &top) else { continue };
        let mut node = node.clone();
        for (i, slot) in node.inputs.iter_mut().enumerate() {
            let Some(src) = slot.clone() else { continue };
            match rebase(&src.node, comp, &top) {
                Some(n) => *slot = Some(PortRef { node: n, output: src.output }),
                None => {
                    dropped.lines.push(format!("{path} input {i} from {}", src.node));
                    *slot = None;
                }
            }
        }
        let mut binds = BTreeMap::new();
        for (part, b) in &node.binds {
            match rebase(&b.node, comp, &top) {
                Some(n) => {
                    binds.insert(part.clone(), BindRef { node: n, channel: b.channel.clone() });
                }
                None => dropped.lines.push(format!("{path}.{part} bound to {}", b.node)),
            }
        }
        node.binds = binds;
        out.nodes.insert(new_path, node);
    }
    let moved: BTreeMap<NodePath, NodePath> =
        base.nodes.keys().filter_map(|p| Some((p.clone(), rebase(p, comp, &top)?))).collect();
    let back: BTreeMap<NodePath, NodePath> = moved.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    fix_refs(types, &mut out, &back, &moved);
    // A reference that still leads outside the component cannot work in another project.
    for (path, node) in &out.nodes {
        let Some(desc) = types.desc(&node.kind) else { continue };
        for spec in desc.params.iter().filter(|s| s.node_ref) {
            let Some(text) = node.params.get(spec.name.as_str()).and_then(Value::as_str) else { continue };
            if let Some(t) = resolve_ref(path, text)
                && !is_within(&t, &top)
            {
                dropped.lines.push(format!("{path}.{} names {t}, outside the component", spec.name));
            }
        }
    }
    Ok((out, dropped))
}

/// The text of a component file for `content` (from [`extract`]).
pub fn print(content: &Content, types: &NodeTypes) -> Result<String, Error> {
    let text = file::print(content, types)?;
    // The project file's header says the app rewrites the file; a component file is written once.
    let body = text.lines().skip_while(|l| l.starts_with("//")).collect::<Vec<_>>().join("\n");
    Ok(format!(
        "// penumbra component. One component at the top level with everything inside it; add it\n\
         // to a project with comp.load or the add dialog.\n{body}\n"
    ))
}

/// Reads a component file: exactly one node at the top level, a container.
pub fn parse(text: &str, types: &NodeTypes) -> Result<Content, Error> {
    let content = file::parse(text, types)?;
    let tops: Vec<&NodePath> = content.nodes.keys().filter(|p| p.names().len() == 1).collect();
    let [top] = tops.as_slice() else {
        return Err(Error::new(format!(
            "a component file holds exactly one node at the top level, this one {}",
            tops.len()
        )));
    };
    let kind = content.nodes.get(*top).map(|n| n.kind.as_str()).unwrap_or_default();
    if !types.desc(kind).is_some_and(|d| d.container) {
        return Err(Error::new(format!("{top} ({kind}) is not a component")));
    }
    Ok(content)
}

/// `component` (from [`parse`]) moved to `at`, added to `base`. `at` must be free and its parent
/// a component or the root.
pub fn instantiate(
    base: &Content,
    types: &NodeTypes,
    component: &Content,
    at: &NodePath,
) -> Result<Content, Error> {
    let top = component
        .nodes
        .keys()
        .find(|p| p.names().len() == 1)
        .cloned()
        .ok_or_else(|| Error::new("the component is empty"))?;
    if base.nodes.contains_key(at) {
        return Err(Error::new(format!("{at} exists already")));
    }
    let mut moved: BTreeMap<NodePath, NodePath> = BTreeMap::new();
    for p in component.nodes.keys() {
        let new = rebase(p, &top, at).ok_or_else(|| Error::new(format!("{p} lies outside {top}")))?;
        moved.insert(p.clone(), new);
    }
    let remap = |p: &NodePath| moved.get(p).cloned();
    let mut added = Content::default();
    for (p, node) in &component.nodes {
        let mut node = node.clone();
        for slot in &mut node.inputs {
            *slot = slot.clone().and_then(|s| Some(PortRef { node: remap(&s.node)?, output: s.output }));
        }
        node.binds = node
            .binds
            .iter()
            .filter_map(|(part, b)| {
                Some((part.clone(), BindRef { node: remap(&b.node)?, channel: b.channel.clone() }))
            })
            .collect();
        if let Some(new) = remap(p) {
            added.nodes.insert(new, node);
        }
    }
    let back: BTreeMap<NodePath, NodePath> = moved.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    fix_refs(types, &mut added, &back, &moved);
    let mut out = base.clone();
    out.nodes.extend(added.nodes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::diff;
    use crate::testing::{content, test_types};
    use penumbra_graph::Network;

    fn p(s: &str) -> NodePath {
        s.parse().unwrap()
    }

    /// Applies `theirs` to a network loaded from `base`, which proves the graph accepts it
    /// (wires through ports only), and reads the content back.
    fn apply(base: &Content, theirs: &Content) -> Content {
        let types = test_types();
        let net = Network::new().apply(&types, &diff(&Content::default(), base)).unwrap().network;
        let next = net.apply(&types, &diff(base, theirs)).unwrap().network;
        Content::from_network(&next).unwrap()
    }

    /// `/src -> /a -> /b -> /sink` with `/b` also reading `/src` and `/a`'s second output.
    fn chain() -> Content {
        content(&[
            ("/src", "test.const", &[("value", 1.0)], &[], false),
            ("/a", "test.add", &[], &[Some(("/src", 0))], false),
            ("/b", "test.add", &[], &[Some(("/a", 0)), Some(("/src", 0))], false),
            ("/sink", "test.add", &[], &[Some(("/b", 0)), Some(("/a", 1))], false),
        ])
    }

    #[test]
    fn grouping_adds_ports_where_wires_cross_and_expanding_undoes_it() {
        let types = test_types();
        let base = chain();
        let c = collapse(&base, &types, &[p("/a"), p("/b")], &p("/fx"), "test.comp").unwrap();
        let got = apply(&base, &c.content);
        assert_eq!(got, c.content);
        let n = |s: &str| got.nodes.get(&p(s)).unwrap_or_else(|| panic!("{s} in {got:#?}"));
        assert_eq!(n("/fx").kind, "test.comp");
        // One input node for /src (read twice), one output per source read outside.
        assert_eq!(n("/fx/src_in").kind, "test.in");
        assert_eq!(n("/fx/src_in").inputs[0], Some(PortRef { node: p("/src"), output: 0 }));
        assert_eq!(n("/fx/a").inputs[0], Some(PortRef { node: p("/fx/src_in"), output: 0 }));
        assert_eq!(n("/fx/b").inputs[1], Some(PortRef { node: p("/fx/src_in"), output: 0 }));
        assert_eq!(n("/fx/b_out").inputs[0], Some(PortRef { node: p("/fx/b"), output: 0 }));
        assert_eq!(n("/fx/a_neg_out").inputs[0], Some(PortRef { node: p("/fx/a"), output: 1 }));
        assert_eq!(n("/sink").inputs[0], Some(PortRef { node: p("/fx/b_out"), output: 0 }));
        assert_eq!(n("/sink").inputs[1], Some(PortRef { node: p("/fx/a_neg_out"), output: 0 }));
        assert_eq!(c.moved.get(&p("/a")), Some(&p("/fx/a")));
        assert_eq!(c.ports.len(), 3);

        let e = expand(&got, &types, &p("/fx")).unwrap();
        assert_eq!(apply(&got, &e.content), base, "expanding gives the original back");
    }

    #[test]
    fn grouping_refuses_what_it_cannot_do() {
        let types = test_types();
        let base = content(&[
            ("/g", "test.comp", &[], &[], false),
            ("/g/x", "test.const", &[], &[], false),
            ("/y", "test.const", &[], &[], false),
        ]);
        let refuse = |members: &[&str], at: &str| {
            let m: Vec<NodePath> = members.iter().map(|s| p(s)).collect();
            collapse(&base, &types, &m, &p(at), "test.comp").unwrap_err().0
        };
        assert!(refuse(&["/g/x", "/y"], "/c").contains("beside"));
        assert!(refuse(&["/y"], "/g/c").contains("beside the nodes"));
        assert!(refuse(&["/y"], "/g").contains("exists"));
        assert!(refuse(&[], "/c").contains("nothing"));
        assert!(refuse(&["/nope"], "/c").contains("no node"));
        let m = [p("/y")];
        assert!(collapse(&base, &types, &m, &p("/c"), "test.const").unwrap_err().0.contains("cannot hold"));
        assert!(expand(&base, &types, &p("/y")).unwrap_err().0.contains("not a component"));
        let with_port =
            content(&[("/g", "test.comp", &[], &[], false), ("/g/i", "test.in", &[], &[], false)]);
        let m = [p("/g/i")];
        let e = collapse(&with_port, &types, &m, &p("/g/c"), "test.comp").unwrap_err().0;
        assert!(e.contains("stays at its edge"), "{e}");
    }

    #[test]
    fn expanding_renames_on_a_clash_and_nested_components_move_whole() {
        let types = test_types();
        let base = content(&[
            ("/x", "test.const", &[], &[], false),
            ("/c", "test.comp", &[], &[], false),
            ("/c/x", "test.const", &[("value", 2.0)], &[], false),
            ("/c/inner", "test.comp", &[], &[], false),
            ("/c/inner/k", "test.const", &[], &[], false),
        ]);
        let e = expand(&base, &types, &p("/c")).unwrap();
        let got = apply(&base, &e.content);
        assert_eq!(got.nodes.get(&p("/x1")).map(|n| n.params.len()), Some(1));
        assert!(got.nodes.contains_key(&p("/inner/k")));
        assert!(!got.nodes.contains_key(&p("/c")));
        assert_eq!(e.moved.get(&p("/c/inner/k")), Some(&p("/inner/k")));
    }

    #[test]
    fn files_name_nodes_relatively_and_survive_moving() {
        let types = test_types();
        let mut base = content(&[
            ("/a", "test.const", &[], &[], false),
            ("/b", "test.add", &[], &[Some(("/a", 0))], false),
            ("/k", "test.const", &[], &[], false),
            ("/r", "test.ref", &[], &[], false),
            ("/g", "test.comp", &[], &[], false),
        ]);
        let k = base.nodes.get_mut(&p("/k")).unwrap();
        k.binds.insert(
            penumbra_core::ParamPart::parse("value").unwrap(),
            BindRef { node: p("/a"), channel: None },
        );
        base.nodes.get_mut(&p("/r")).unwrap().params.insert("node".parse().unwrap(), Value::Text("a".into()));
        let members = [p("/a"), p("/b"), p("/k"), p("/r")];
        let fx = apply(&base, &collapse(&base, &types, &members, &p("/fx"), "test.comp").unwrap().content);
        // Nested once more: everything still names /fx/a, now one level down.
        let nested = collapse(&fx, &types, &[p("/fx/a")], &p("/fx/sub"), "test.comp").unwrap().content;
        let nested = apply(&fx, &nested);
        let text = file::print(&nested, &types).unwrap();
        for want in [
            r#""/fx/b": (kind: "test.add", inputs: {"a": "sub/a_out"}),"#,
            r#""/fx/k": (kind: "test.const", binds: {"value": "sub/a"}),"#,
            r#""/fx/r": (kind: "test.ref", params: {"node": "sub/a"}),"#,
            r#""/fx/sub/a_out": (kind: "test.out", inputs: {"in": "a"}),"#,
        ] {
            assert!(
                text.contains(want),
                "{want} in
{text}"
            );
        }
        assert_eq!(file::parse(&text, &types).unwrap(), nested);
        // The component file carries the same relative paths, so it reads anywhere.
        let (comp, dropped) = extract(&nested, &types, &p("/fx")).unwrap();
        assert!(dropped.lines.is_empty(), "{dropped:?}");
        let comp_text = print(&comp, &types).unwrap();
        assert!(comp_text.contains(r#"binds: {"value": "sub/a"}"#), "{comp_text}");
        let placed = apply(
            &nested,
            &instantiate(&nested, &types, &parse(&comp_text, &types).unwrap(), &p("/g/fx2")).unwrap(),
        );
        let n = |s: &str| placed.nodes.get(&p(s)).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(n("/g/fx2/k").binds.values().next().map(|b| b.node.clone()), Some(p("/g/fx2/sub/a")));
        assert_eq!(n("/g/fx2/r").params.get("node"), Some(&Value::Text("sub/a".into())));
        assert_eq!(n("/g/fx2/b").inputs[0], Some(PortRef { node: p("/g/fx2/sub/a_out"), output: 0 }));
        // Dissolving the inner component brings the plain names back.
        let flat = apply(&placed, &expand(&placed, &types, &p("/g/fx2/sub")).unwrap().content);
        assert_eq!(flat.nodes[&p("/g/fx2/r")].params.get("node"), Some(&Value::Text("a".into())));
        let up = file::print(&flat, &types).unwrap();
        assert!(up.contains(r#""/g/fx2/b": (kind: "test.add", inputs: {"a": "a"}),"#), "{up}");
    }

    #[test]
    fn a_component_file_round_trips_and_drops_outside_references() {
        let types = test_types();
        let base = chain();
        let grouped = apply(
            &base,
            &collapse(&base, &types, &[p("/a"), p("/b")], &p("/fx"), "test.comp").unwrap().content,
        );
        let (comp, dropped) = extract(&grouped, &types, &p("/fx")).unwrap();
        // The input node's wire from /src cannot go along.
        assert_eq!(dropped.lines.len(), 1, "{dropped:?}");
        assert!(comp.nodes.keys().all(|k| is_within(k, &p("/fx"))));
        let text = print(&comp, &types).unwrap();
        assert!(text.starts_with("// penumbra component."), "{text}");
        let read = parse(&text, &types).unwrap();
        assert_eq!(read, comp);
        // Added twice, side by side, under another name.
        let once = instantiate(&grouped, &types, &read, &p("/fx2")).unwrap();
        let got = apply(&grouped, &once);
        let n = got.nodes.get(&p("/fx2/a")).unwrap();
        assert_eq!(n.inputs[0], Some(PortRef { node: p("/fx2/src_in"), output: 0 }));
        assert!(instantiate(&got, &types, &read, &p("/fx2")).unwrap_err().0.contains("exists"));
        // Not a component file.
        let two = file::print(&chain(), &types).unwrap();
        assert!(parse(&two, &types).unwrap_err().0.contains("exactly one"));
    }
}
