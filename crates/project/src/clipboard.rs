//! Copy and paste of nodes.
//!
//! Copied nodes are side by side on one level, each with everything inside it. Their text is the
//! project file's format for just those nodes, so it can go through the system clipboard into
//! another project and be read or written by hand; inputs and bindings from nodes that were not
//! copied keep naming them (an input from such a node gives its output by number).
//!
//! Pasting is a pure function from one [`Content`] to another, as grouping is; the caller turns
//! the pair into one undoable edit with [`crate::diff`]. Inside the pasted set, wires, bindings
//! and node references follow the copies. A wire from a node outside the set is kept where the
//! graph allows it at the new place (pasted beside the original, it reads the same source);
//! elsewhere it is dropped and listed. A binding to a node outside the set is kept while that
//! node exists, since bindings may reach anywhere.

use std::collections::{BTreeMap, BTreeSet};

use penumbra_core::{Error, Name, NodePath};
use penumbra_graph::{NodeTypes, PortRole, natural_cmp};

use crate::component::{children_names, fix_refs, is_within, rebase};
use crate::content::{BindRef, Content, PortRef};
use crate::file;

/// The first line of copied text; what tells it apart from other text on the clipboard.
pub const MARK: &str = "// penumbra nodes";
/// Largest copied text read.
pub const MAX_TEXT_BYTES: usize = file::MAX_FILE_BYTES as usize;

/// Copied nodes, by the paths they had.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// The level they were copied from.
    pub from: NodePath,
    /// The copied nodes and everything inside them. Wires and bindings may name nodes outside.
    pub content: Content,
}

impl Clip {
    /// The copied nodes themselves (not what is inside them), in natural order.
    pub fn top(&self) -> Vec<NodePath> {
        let mut top: Vec<NodePath> =
            self.content.nodes.keys().filter(|p| p.parent().as_ref() == Some(&self.from)).cloned().collect();
        top.sort_by(|a, b| natural_cmp(&a.to_string(), &b.to_string()));
        top
    }
}

/// Copies `paths` (side by side) out of `base`, each with everything inside it.
pub fn copy(base: &Content, paths: &[NodePath]) -> Result<Clip, Error> {
    let first = paths.first().ok_or_else(|| Error::new("nothing to copy"))?;
    let from = first.parent().ok_or_else(|| Error::new("the root cannot be copied"))?;
    for p in paths {
        if !base.nodes.contains_key(p) {
            return Err(Error::new(format!("no node at {p}")));
        }
        if p.parent().as_ref() != Some(&from) {
            return Err(Error::new(format!("{p} is not beside {first}; only nodes side by side are copied")));
        }
    }
    let nodes = base
        .nodes
        .iter()
        .filter(|(p, _)| paths.iter().any(|m| is_within(p, m)))
        .map(|(p, n)| (p.clone(), n.clone()))
        .collect();
    Ok(Clip { from, content: Content { nodes, settings: BTreeMap::new() } })
}

/// The text of `clip`, for the clipboard.
pub fn print(clip: &Clip, types: &NodeTypes) -> Result<String, Error> {
    let text = file::print(&clip.content, types)?;
    let body = text.lines().skip_while(|l| l.starts_with("//")).collect::<Vec<_>>().join("\n");
    Ok(format!(
        "{MARK}, copied from {}. Paste them with Ctrl+V over the node editor (or node.paste).\n{body}\n",
        clip.from
    ))
}

/// Whether `text` looks like copied nodes (it may still fail to parse).
pub fn is_clip(text: &str) -> bool {
    text.trim_start().starts_with(MARK)
}

/// Reads copied nodes. Any project file text reads too (its top-level nodes are the copied ones);
/// its settings are ignored.
pub fn parse(text: &str, types: &NodeTypes) -> Result<Clip, Error> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::new(format!("the copied text is larger than {MAX_TEXT_BYTES} bytes")));
    }
    let content = file::parse_as(text, types, "copied nodes", true)?;
    // The copied nodes are the ones whose parent was not copied; they share one level.
    let mut levels: BTreeSet<NodePath> = BTreeSet::new();
    for p in content.nodes.keys() {
        let parent = p.parent().unwrap_or_else(NodePath::root);
        if !content.nodes.contains_key(&parent) {
            levels.insert(parent);
        }
    }
    let mut levels = levels.into_iter();
    let (Some(from), None) = (levels.next(), levels.next()) else {
        return Err(Error::new("copied nodes must be side by side, on one level"));
    };
    Ok(Clip { from, content: Content { nodes: content.nodes, settings: BTreeMap::new() } })
}

/// What [`paste`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Pasted {
    pub content: Content,
    /// Copied path to pasted path of every node (what is inside included).
    pub moved: BTreeMap<NodePath, NodePath>,
    /// The pasted nodes on the level pasted into, in the order of [`Clip::top`].
    pub top: Vec<NodePath>,
    /// Wires and bindings from outside that could not come along.
    pub dropped: Vec<String>,
}

/// A name for a copy of `name` not in `taken`: `name` itself if free, else the next number after
/// its own (`noise1` becomes `noise2`, `level` becomes `level1`).
fn copy_name(name: &Name, taken: &BTreeSet<Name>) -> Result<Name, Error> {
    if !taken.contains(name) {
        return Ok(name.clone());
    }
    let text = name.as_str();
    let stem = text.trim_end_matches(|c: char| c.is_ascii_digit());
    let start = text[stem.len()..].parse::<u64>().map_or(1, |n| n.saturating_add(1));
    for i in start..start.saturating_add(100_000) {
        let n = Name::new(&format!("{stem}{i}"))?;
        if !taken.contains(&n) {
            return Ok(n);
        }
    }
    Err(Error::new(format!("no free name for a copy of `{name}`")))
}

/// Whether a wire from `src` may feed `dst`, by the rule of [`penumbra_graph::Network::wire_allowed`]:
/// from beside the reader (from outside its component for an input node), or from an output node
/// of a component beside it.
fn wire_allowed(types: &NodeTypes, src: &NodePath, src_kind: &str, dst: &NodePath, dst_kind: &str) -> bool {
    let role = |kind: &str| types.desc(kind).and_then(|d| d.port);
    let level = match (role(dst_kind), dst.parent()) {
        (Some(PortRole::In), Some(comp)) if !comp.is_root() => comp.parent(),
        (_, parent) => parent,
    };
    if src.parent() == level {
        return true;
    }
    role(src_kind) == Some(PortRole::Out)
        && src.parent().filter(|c| !c.is_root()).and_then(|c| c.parent()) == level
}

/// `clip` pasted into `into` (the root or a component) of `base`. The copied nodes keep their
/// names where those are free there and are numbered on otherwise.
pub fn paste(base: &Content, types: &NodeTypes, clip: &Clip, into: &NodePath) -> Result<Pasted, Error> {
    if !into.is_root() {
        let node = base.nodes.get(into).ok_or_else(|| Error::new(format!("no node at {into}")))?;
        if !types.desc(&node.kind).is_some_and(|d| d.container) {
            return Err(Error::new(format!("{into} ({}) cannot hold nodes", node.kind)));
        }
    }
    let top = clip.top();
    if top.is_empty() {
        return Err(Error::new("nothing to paste"));
    }
    let mut taken = children_names(base, into);
    let mut moved: BTreeMap<NodePath, NodePath> = BTreeMap::new();
    let mut new_top = Vec::new();
    for t in &top {
        let Some(name) = t.name() else { continue };
        let fresh = copy_name(name, &taken)?;
        taken.insert(fresh.clone());
        let to = into.child(fresh)?;
        for path in clip.content.nodes.keys().filter(|p| is_within(p, t)) {
            let new = rebase(path, t, &to).ok_or_else(|| Error::new(format!("{path} lies outside {t}")))?;
            moved.insert(path.clone(), new);
        }
        new_top.push(to);
    }

    let mut dropped = Vec::new();
    let mut added = Content::default();
    for (path, node) in &clip.content.nodes {
        let Some(new_path) = moved.get(path).cloned() else { continue };
        let mut node = node.clone();
        let input_names: Vec<String> = types
            .desc(&node.kind)
            .map(|d| d.inputs.iter().map(|i| i.name.clone()).collect())
            .unwrap_or_default();
        for (i, slot) in node.inputs.iter_mut().enumerate() {
            let Some(src) = slot.clone() else { continue };
            if let Some(n) = moved.get(&src.node) {
                *slot = Some(PortRef { node: n.clone(), output: src.output });
                continue;
            }
            let keep = base
                .nodes
                .get(&src.node)
                .is_some_and(|s| wire_allowed(types, &src.node, &s.kind, &new_path, &node.kind));
            if !keep {
                let input = input_names.get(i).cloned().unwrap_or_else(|| i.to_string());
                dropped.push(format!("{new_path} input `{input}` from {}", src.node));
                *slot = None;
            }
        }
        let mut binds = BTreeMap::new();
        for (part, b) in &node.binds {
            match moved.get(&b.node) {
                Some(n) => {
                    binds.insert(part.clone(), BindRef { node: n.clone(), channel: b.channel.clone() });
                }
                None if base.nodes.contains_key(&b.node) => {
                    binds.insert(part.clone(), b.clone());
                }
                None => dropped.push(format!("{new_path}.{part} bound to {}, which is not here", b.node)),
            }
        }
        node.binds = binds;
        added.nodes.insert(new_path, node);
    }
    let back: BTreeMap<NodePath, NodePath> = moved.iter().map(|(o, n)| (n.clone(), o.clone())).collect();
    fix_refs(types, &mut added, &back, &moved);
    let mut out = base.clone();
    out.nodes.extend(added.nodes);
    Ok(Pasted { content: out, moved, top: new_top, dropped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::diff;
    use crate::testing::{content, test_types};
    use penumbra_core::{ParamPart, Value};
    use penumbra_graph::Network;

    fn p(s: &str) -> NodePath {
        s.parse().unwrap()
    }

    /// Applies `theirs` to a network loaded from `base` (so the graph accepts it) and reads it back.
    fn apply(base: &Content, theirs: &Content) -> Content {
        let types = test_types();
        let net = Network::new().apply(&types, &diff(&Content::default(), base)).unwrap().network;
        let next = net.apply(&types, &diff(base, theirs)).unwrap().network;
        Content::from_network(&next).unwrap()
    }

    fn bind(c: &mut Content, at: &str, param: &str, to: &str) {
        c.nodes
            .get_mut(&p(at))
            .unwrap()
            .binds
            .insert(ParamPart::parse(param).unwrap(), BindRef { node: p(to), channel: None });
    }

    /// `/src`, `/a <- /src`, `/b <- /a, /a:neg` (locked), `/r` naming `a`, `/k` bound to `/lfo`,
    /// `/m` bound to `/k`; a component `/g` with an input node and a node inside.
    fn scene() -> Content {
        let mut c = content(&[
            ("/src", "test.const", &[], &[], false),
            ("/lfo", "test.const", &[], &[], false),
            ("/a", "test.add", &[], &[Some(("/src", 0))], false),
            ("/b", "test.add", &[], &[Some(("/a", 0)), Some(("/a", 1))], true),
            ("/r", "test.ref", &[], &[], false),
            ("/k", "test.const", &[("value", 3.0)], &[], false),
            ("/m", "test.const", &[], &[], false),
            ("/g", "test.comp", &[], &[], false),
            ("/g/in", "test.in", &[], &[Some(("/src", 0))], false),
            ("/g/x", "test.add", &[], &[Some(("/g/in", 0))], false),
        ]);
        c.nodes.get_mut(&p("/r")).unwrap().params.insert("node".parse().unwrap(), Value::Text("a".into()));
        bind(&mut c, "/k", "value", "/lfo");
        bind(&mut c, "/m", "value", "/k");
        c
    }

    #[test]
    fn pasting_beside_the_originals_numbers_them_and_keeps_outside_sources() {
        let types = test_types();
        let base = apply(&Content::default(), &scene());
        let clip = copy(&base, &[p("/a"), p("/b"), p("/r"), p("/k"), p("/m")]).unwrap();
        let text = print(&clip, &types).unwrap();
        assert!(is_clip(&text), "{text}");
        let read = parse(&text, &types).unwrap();
        assert_eq!(read, clip, "{text}");

        let pasted = paste(&base, &types, &read, &NodePath::root()).unwrap();
        assert!(pasted.dropped.is_empty(), "{:?}", pasted.dropped);
        assert_eq!(pasted.top, vec![p("/a1"), p("/b1"), p("/k1"), p("/m1"), p("/r1")]);
        let got = apply(&base, &pasted.content);
        assert_eq!(got, pasted.content);
        let n = |s: &str| got.nodes.get(&p(s)).unwrap_or_else(|| panic!("{s}"));
        // Inside the set wires, bindings and references follow the copies; /src stays the source.
        assert_eq!(n("/a1").inputs[0], Some(PortRef { node: p("/src"), output: 0 }));
        assert_eq!(n("/b1").inputs[1], Some(PortRef { node: p("/a1"), output: 1 }));
        assert_eq!(n("/m1").binds.values().next().map(|b| b.node.clone()), Some(p("/k1")));
        assert_eq!(n("/k1").binds.values().next().map(|b| b.node.clone()), Some(p("/lfo")));
        assert_eq!(n("/r1").params.get("node"), Some(&Value::Text("a1".into())));
        assert!(n("/b1").locked);
        assert_eq!(n("/k1").params.get("value"), Some(&Value::Float(3.0)));
        // The originals are untouched.
        for (path, node) in &base.nodes {
            assert_eq!(got.nodes.get(path), Some(node), "{path}");
        }
        // Pasting again counts on.
        let again = paste(&got, &types, &read, &NodePath::root()).unwrap();
        assert_eq!(again.top, vec![p("/a2"), p("/b2"), p("/k2"), p("/m2"), p("/r2")]);
    }

    #[test]
    fn pasting_into_a_component_drops_wires_that_cannot_cross_its_edge() {
        let types = test_types();
        let base = scene();
        let clip = copy(&base, &[p("/a"), p("/b"), p("/r"), p("/m")]).unwrap();
        let pasted = paste(&base, &types, &clip, &p("/g")).unwrap();
        // /src is outside /g: the wire into /g/a goes, the bindings to /k (outside) stay.
        assert_eq!(pasted.dropped, vec!["/g/a input `a` from /src".to_string()]);
        let got = apply(&base, &pasted.content);
        let n = |s: &str| got.nodes.get(&p(s)).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(n("/g/a").inputs[0], None);
        assert_eq!(n("/g/b").inputs[0], Some(PortRef { node: p("/g/a"), output: 0 }));
        assert_eq!(n("/g/m").binds.values().next().map(|b| b.node.clone()), Some(p("/k")));
        assert_eq!(n("/g/r").params.get("node"), Some(&Value::Text("a".into())));
        assert_eq!(pasted.moved.get(&p("/a")), Some(&p("/g/a")));
    }

    #[test]
    fn components_copy_whole_and_their_inputs_keep_reading_outside() {
        let types = test_types();
        let base = scene();
        let clip = copy(&base, &[p("/g")]).unwrap();
        let pasted = paste(&base, &types, &clip, &NodePath::root()).unwrap();
        assert!(pasted.dropped.is_empty(), "{:?}", pasted.dropped);
        let got = apply(&base, &pasted.content);
        let n = |s: &str| got.nodes.get(&p(s)).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(n("/g1/in").inputs[0], Some(PortRef { node: p("/src"), output: 0 }));
        assert_eq!(n("/g1/x").inputs[0], Some(PortRef { node: p("/g1/in"), output: 0 }));
        // Copied out of the component, the input node pasted at the top level reads /src too.
        let inner = copy(&got, &[p("/g/in"), p("/g/x")]).unwrap();
        let out = paste(&got, &types, &inner, &NodePath::root()).unwrap();
        assert!(out.dropped.is_empty(), "{:?}", out.dropped);
        let flat = apply(&got, &out.content);
        assert_eq!(flat.nodes[&p("/x")].inputs[0], Some(PortRef { node: p("/in"), output: 0 }));
        assert_eq!(flat.nodes[&p("/in")].inputs[0], Some(PortRef { node: p("/src"), output: 0 }));
        // Into another component: its input node is fed from outside it, where /src is.
        let other = paste(&got, &types, &inner, &p("/g1")).unwrap();
        assert!(other.dropped.is_empty(), "{:?}", other.dropped);
        let both = apply(&got, &other.content);
        assert_eq!(both.nodes[&p("/g1/in1")].inputs[0], Some(PortRef { node: p("/src"), output: 0 }));
        assert_eq!(both.nodes[&p("/g1/x1")].inputs[0], Some(PortRef { node: p("/g1/in1"), output: 0 }));
        // One level deeper it is not: /src is two levels up.
        let mut nested = both.clone();
        nested.nodes.insert(p("/g1/sub"), base.nodes[&p("/g")].clone());
        let deep = paste(&nested, &types, &inner, &p("/g1/sub")).unwrap();
        assert_eq!(deep.dropped, vec!["/g1/sub/in input `in` from /src".to_string()]);
        apply(&nested, &deep.content);
    }

    #[test]
    fn text_travels_to_another_project() {
        let types = test_types();
        let base = scene();
        let text = print(&copy(&base, &[p("/a"), p("/b"), p("/m")]).unwrap(), &types).unwrap();
        // Outside sources are written relative, outputs of nodes not copied by number.
        assert!(text.contains(r#""/a": (kind: "test.add", inputs: {"a": "src"}),"#), "{text}");
        assert!(text.contains(r#""/b": (kind: "test.add", inputs: {"a": "a", "b": "a:neg"}"#), "{text}");
        let elsewhere = content(&[("/other", "test.const", &[], &[], false)]);
        let pasted = paste(&elsewhere, &types, &parse(&text, &types).unwrap(), &NodePath::root()).unwrap();
        assert_eq!(
            pasted.dropped,
            vec!["/a input `a` from /src".to_string(), "/m.value bound to /k, which is not here".to_string()]
        );
        let got = apply(&elsewhere, &pasted.content);
        assert_eq!(got.nodes.len(), 4);
        // A whole project file pastes as well, its top level being the copied nodes.
        let project = file::print(&base, &types).unwrap();
        let clip = parse(&project, &types).unwrap();
        assert_eq!(clip.from, NodePath::root());
        assert_eq!(clip.top().len(), 8);
        let one = parse(
            r#"(version: 1, nodes: {"/q/s": (kind: "test.add", inputs: {"a": "/far:1", "b": "../w"})})"#,
            &types,
        )
        .unwrap();
        assert_eq!(one.from, p("/q"));
        assert_eq!(one.content.nodes[&p("/q/s")].inputs[0], Some(PortRef { node: p("/far"), output: 1 }));
    }

    #[test]
    fn bad_copies_and_pastes_are_refused() {
        let types = test_types();
        let base = scene();
        assert!(copy(&base, &[]).unwrap_err().0.contains("nothing"));
        assert!(copy(&base, &[p("/a"), p("/g/x")]).unwrap_err().0.contains("side by side"));
        assert!(copy(&base, &[p("/nope")]).unwrap_err().0.contains("no node"));
        let clip = copy(&base, &[p("/a")]).unwrap();
        assert!(paste(&base, &types, &clip, &p("/k")).unwrap_err().0.contains("cannot hold"));
        assert!(paste(&base, &types, &clip, &p("/zz")).unwrap_err().0.contains("no node"));
        let two_levels =
            r#"(version: 1, nodes: {"/a": (kind: "test.const"), "/g2/b": (kind: "test.const")})"#;
        assert!(parse(two_levels, &types).unwrap_err().0.contains("one level"));
        let named = r#"(version: 1, nodes: {"/s": (kind: "test.add", inputs: {"a": "/far:neg"})})"#;
        assert!(parse(named, &types).unwrap_err().0.contains("by number"));
        assert!(parse("hello", &types).is_err());
        assert!(!is_clip("hello"));
    }

    #[test]
    fn copy_names_count_on_from_their_number() {
        let taken: BTreeSet<Name> =
            ["noise1", "noise2", "level", "x9"].iter().map(|s| s.parse().unwrap()).collect();
        let name = |s: &str| copy_name(&s.parse().unwrap(), &taken).unwrap().to_string();
        assert_eq!(name("noise1"), "noise3");
        assert_eq!(name("noise2"), "noise3");
        assert_eq!(name("level"), "level1");
        assert_eq!(name("x9"), "x10");
        assert_eq!(name("free"), "free");
    }
}
