//! A project's content as plain data keyed by path, and the diff between two contents.
//!
//! The network refers to nodes by id; the project file refers to them by path. [`Content`] is
//! the path-keyed form both sides convert to. Loading, hot reload and merging an external edit
//! are all the same operation: [`diff`] two contents into a batch of [`Op`]s and apply the batch
//! to the running network, so every node the batch does not touch keeps its id, and with it its
//! operator state in the cooker (caches, streams, tracking ids).

use std::collections::{BTreeMap, BTreeSet};

use penumbra_core::{Error, Name, NodePath, ParamPart, Value};
use penumbra_graph::{Network, Op};

/// One output of one node, by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortRef {
    pub node: NodePath,
    pub output: u32,
}

/// What a bound parameter reads, by path: a channel of the first `chan` output of `node`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindRef {
    pub node: NodePath,
    pub channel: Option<Name>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeContent {
    pub kind: String,
    /// Explicitly set values; may include defaults when read from a file.
    pub params: BTreeMap<Name, Value>,
    /// One slot per declared input of the kind.
    pub inputs: Vec<Option<PortRef>>,
    pub binds: BTreeMap<ParamPart, BindRef>,
    pub locked: bool,
    /// Where the node sits in the editor ([`penumbra_graph::NodeDef::pos`]).
    pub pos: Option<[f32; 2]>,
}

/// All nodes of a project by path. Paths sort parents before their children (a path is a
/// prefix of its descendants' paths), which is the order nodes must be created in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Content {
    pub nodes: BTreeMap<NodePath, NodeContent>,
    /// Project settings that differ from their default ([`penumbra_graph::SETTINGS`]).
    pub settings: BTreeMap<Name, Value>,
}

impl Content {
    pub fn from_network(net: &Network) -> Result<Content, Error> {
        let path = |id| net.path_of(id).ok_or_else(|| Error::new(format!("node {id} has no path")));
        let mut nodes = BTreeMap::new();
        for def in net.nodes() {
            let inputs = def
                .inputs
                .iter()
                .map(|slot| match slot {
                    Some(p) => Ok(Some(PortRef { node: path(p.node)?, output: p.output })),
                    None => Ok(None),
                })
                .collect::<Result<Vec<_>, Error>>()?;
            let binds = def
                .binds
                .iter()
                .map(|(part, b)| {
                    Ok((part.clone(), BindRef { node: path(b.node)?, channel: b.channel.clone() }))
                })
                .collect::<Result<BTreeMap<_, _>, Error>>()?;
            nodes.insert(
                path(def.id)?,
                NodeContent {
                    kind: def.kind.clone(),
                    params: def.params.clone(),
                    inputs,
                    binds,
                    locked: def.locked,
                    pos: def.pos,
                },
            );
        }
        Ok(Content { nodes, settings: net.settings().clone() })
    }
}

/// Whether `path` lies strictly inside `ancestor`.
fn is_inside(path: &NodePath, ancestor: &NodePath) -> bool {
    path.names().len() > ancestor.names().len() && path.names().starts_with(ancestor.names())
}

/// The ops that turn `base` into `theirs`. Bindings are dependencies like connections and follow
/// the same order: changed ones are dropped before any is made. Applied to a network whose content
/// is `base`, the result has the content `theirs`; applied to a network that has moved on from
/// `base` (the user's edits since the file was last written), it carries only the changes between
/// `base` and `theirs` over, which is the three-way merge of an external file edit. A conflicting
/// change (the user deleted the node the file edit sets a parameter on) makes the batch fail as a
/// whole.
///
/// Renames cannot be told apart from a delete plus a create, so a renamed node starts with a
/// fresh operator. A node whose kind changed is deleted and created again, with everything
/// inside it.
pub fn diff(base: &Content, theirs: &Content) -> Vec<Op> {
    // Base nodes that do not survive: gone from `theirs`, kind changed, or inside one of those.
    let mut gone: BTreeSet<&NodePath> = BTreeSet::new();
    for (path, b) in &base.nodes {
        let same_kind = theirs.nodes.get(path).is_some_and(|t| t.kind == b.kind);
        if !same_kind || gone.iter().any(|g| is_inside(path, g)) {
            gone.insert(path);
        }
    }
    let kept = |path: &NodePath| base.nodes.contains_key(path) && !gone.contains(path);
    // Does a connection from `src` need to be made again because `src` is (re)created?
    let fresh = |src: &PortRef| !kept(&src.node);

    let mut deletes = Vec::new();
    for path in &gone {
        let parent_gone = path.parent().is_some_and(|p| gone.contains(&p));
        if !parent_gone {
            deletes.push(Op::Delete { path: (*path).clone() });
        }
    }

    let mut disconnects = Vec::new();
    let mut unbinds = Vec::new();
    let mut binds = Vec::new();
    let mut creates = Vec::new();
    let mut sets = Vec::new();
    let mut connects = Vec::new();
    let mut locks = Vec::new();
    let mut places = Vec::new();
    for (path, t) in &theirs.nodes {
        let base_node = base.nodes.get(path).filter(|_| kept(path));
        let Some(b) = base_node else {
            creates.push(Op::Create { path: path.clone(), kind: t.kind.clone(), params: t.params.clone() });
            if t.pos.is_some() {
                places.push(Op::Place { path: path.clone(), pos: t.pos });
            }
            for (input, slot) in t.inputs.iter().enumerate() {
                if let Some(src) = slot {
                    connects.push(connect(src, path, input));
                }
            }
            for (part, src) in &t.binds {
                binds.extend(bind(path, part, src));
            }
            if t.locked {
                locks.push(Op::Lock { path: path.clone(), locked: true });
            }
            continue;
        };
        for (name, value) in &t.params {
            if b.params.get(name) != Some(value)
                && let Ok(param) = path.param(name.clone())
            {
                sets.push(Op::Set { param, value: value.clone() });
            }
        }
        for name in b.params.keys().filter(|n| !t.params.contains_key(*n)) {
            if let Ok(param) = path.param(name.clone()) {
                sets.push(Op::Reset { param });
            }
        }
        for (input, slot) in t.inputs.iter().enumerate() {
            let before = b.inputs.get(input).cloned().flatten();
            let changed = before.as_ref() != slot.as_ref() || slot.as_ref().is_some_and(fresh);
            if !changed {
                continue;
            }
            // Every changed slot is cleared before any is connected: the edges left then are a
            // subset of the final ones, so no intermediate state can form a loop.
            disconnects
                .push(Op::Disconnect { to: path.clone(), input: u32::try_from(input).unwrap_or(u32::MAX) });
            if let Some(src) = slot {
                connects.push(connect(src, path, input));
            }
        }
        for (part, src) in &t.binds {
            let changed = b.binds.get(part) != Some(src) || !kept(&src.node);
            if changed {
                unbinds.extend(unbind(path, part));
                binds.extend(bind(path, part, src));
            }
        }
        for part in b.binds.keys().filter(|p| !t.binds.contains_key(*p)) {
            unbinds.extend(unbind(path, part));
        }
        if t.locked != b.locked {
            locks.push(Op::Lock { path: path.clone(), locked: t.locked });
        }
        if t.pos != b.pos {
            places.push(Op::Place { path: path.clone(), pos: t.pos });
        }
    }

    let mut settings = Vec::new();
    for (name, value) in &theirs.settings {
        if base.settings.get(name) != Some(value) {
            settings.push(Op::Setting { name: name.clone(), value: Some(value.clone()) });
        }
    }
    for name in base.settings.keys().filter(|n| !theirs.settings.contains_key(*n)) {
        settings.push(Op::Setting { name: name.clone(), value: None });
    }

    let mut ops = settings;
    ops.extend(deletes);
    ops.extend(disconnects);
    ops.extend(unbinds);
    ops.extend(creates);
    ops.extend(sets);
    ops.extend(connects);
    ops.extend(binds);
    ops.extend(locks);
    ops.extend(places);
    ops
}

fn bind(path: &NodePath, part: &ParamPart, src: &BindRef) -> Option<Op> {
    Some(Op::Bind {
        param: path.param(part.param.clone()).ok()?,
        component: part.component,
        from: src.node.clone(),
        channel: src.channel.clone(),
    })
}

fn unbind(path: &NodePath, part: &ParamPart) -> Option<Op> {
    Some(Op::Unbind { param: path.param(part.param.clone()).ok()?, component: part.component })
}

fn connect(src: &PortRef, to: &NodePath, input: usize) -> Op {
    Op::Connect {
        from: src.node.clone(),
        output: src.output,
        to: to.clone(),
        input: u32::try_from(input).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{content, test_types};

    fn apply(net: &Network, ops: &[Op]) -> Network {
        net.apply(&test_types(), ops).unwrap().network
    }

    fn load(c: &Content) -> Network {
        apply(&Network::new(), &diff(&Content::default(), c))
    }

    fn roundtrip(c: &Content) {
        let net = load(c);
        assert_eq!(&Content::from_network(&net).unwrap(), c);
    }

    #[test]
    fn load_creates_parents_first_and_connects_after() {
        let c = content(&[
            ("/g", "test.group", &[], &[], false),
            ("/g/a", "test.const", &[("value", 2.0)], &[], false),
            ("/g/o", "test.out", &[], &[Some(("/g/a", 0))], false),
            ("/sum", "test.add", &[], &[Some(("/g/o", 0)), None], true),
        ]);
        roundtrip(&c);
    }

    #[test]
    fn diff_of_equal_contents_is_empty() {
        let c = content(&[("/a", "test.const", &[("value", 1.0)], &[], false)]);
        assert!(diff(&c, &c).is_empty());
    }

    #[test]
    fn unchanged_nodes_keep_their_ids() {
        let base = content(&[
            ("/a", "test.const", &[], &[], false),
            ("/b", "test.const", &[], &[], false),
            ("/sum", "test.add", &[], &[Some(("/a", 0)), Some(("/b", 0))], false),
        ]);
        let theirs = content(&[
            ("/a", "test.const", &[("value", 3.0)], &[], false),
            ("/c", "test.const", &[], &[], false),
            ("/sum", "test.add", &[], &[Some(("/c", 0)), Some(("/a", 0))], true),
        ]);
        let net = load(&base);
        let next = apply(&net, &diff(&base, &theirs));
        assert_eq!(Content::from_network(&next).unwrap(), theirs);
        for path in ["/a", "/sum"] {
            let p: NodePath = path.parse().unwrap();
            assert_eq!(net.resolve(&p), next.resolve(&p), "{path} kept its id");
        }
    }

    #[test]
    fn swapping_a_connection_does_not_trip_the_loop_check() {
        let base = content(&[
            ("/a", "test.add", &[], &[None, None], false),
            ("/b", "test.add", &[], &[Some(("/a", 0)), None], false),
        ]);
        let theirs = content(&[
            ("/a", "test.add", &[], &[Some(("/b", 0)), None], false),
            ("/b", "test.add", &[], &[None, None], false),
        ]);
        let next = apply(&load(&base), &diff(&base, &theirs));
        assert_eq!(Content::from_network(&next).unwrap(), theirs);
    }

    #[test]
    fn kind_change_recreates_the_node_and_reconnects_readers() {
        let base = content(&[
            ("/g", "test.group", &[], &[], false),
            ("/g/a", "test.const", &[], &[], false),
            ("/src", "test.const", &[], &[], false),
            ("/sum", "test.add", &[], &[Some(("/src", 0)), None], false),
        ]);
        // `/g` becomes a constant (its child must go), `/src` becomes an adder; `/sum` still
        // reads `/src`, which needs a fresh connection because the delete cleared it.
        let theirs = content(&[
            ("/g", "test.const", &[], &[], false),
            ("/src", "test.add", &[], &[None, None], false),
            ("/sum", "test.add", &[], &[Some(("/src", 0)), None], false),
        ]);
        let net = load(&base);
        let next = apply(&net, &diff(&base, &theirs));
        assert_eq!(Content::from_network(&next).unwrap(), theirs);
        let src: NodePath = "/src".parse().unwrap();
        assert_ne!(net.resolve(&src), next.resolve(&src));
    }

    #[test]
    fn three_way_keeps_the_users_unsaved_changes() {
        let base = content(&[("/a", "test.const", &[], &[], false), ("/b", "test.const", &[], &[], false)]);
        let net = load(&base);
        // The user changes /b; the file edit changes /a.
        let ours = apply(&net, &[Op::Set { param: "/b.value".parse().unwrap(), value: Value::Float(7.0) }]);
        let theirs = content(&[
            ("/a", "test.const", &[("value", 1.0)], &[], false),
            ("/b", "test.const", &[], &[], false),
        ]);
        let merged = apply(&ours, &diff(&base, &theirs));
        let want = content(&[
            ("/a", "test.const", &[("value", 1.0)], &[], false),
            ("/b", "test.const", &[("value", 7.0)], &[], false),
        ]);
        assert_eq!(Content::from_network(&merged).unwrap(), want);
    }

    #[test]
    fn binds_load_change_and_go() {
        let bound = |src: &str, channel: Option<&str>| {
            let mut c = content(&[
                ("/a", "test.const", &[], &[], false),
                ("/b", "test.const", &[], &[], false),
                ("/c", "test.const", &[], &[], false),
            ]);
            if let Some(n) = c.nodes.get_mut(&"/c".parse::<NodePath>().unwrap()) {
                let src =
                    BindRef { node: src.parse().unwrap(), channel: channel.map(|c| c.parse().unwrap()) };
                n.binds.insert(ParamPart::parse("value").unwrap(), src);
            }
            c
        };
        let base = bound("/a", None);
        roundtrip(&base);
        let net = load(&base);
        let theirs = bound("/b", Some("value"));
        let next = apply(&net, &diff(&base, &theirs));
        assert_eq!(Content::from_network(&next).unwrap(), theirs);
        let none = content(&[
            ("/a", "test.const", &[], &[], false),
            ("/b", "test.const", &[], &[], false),
            ("/c", "test.const", &[], &[], false),
        ]);
        let gone = apply(&next, &diff(&theirs, &none));
        assert_eq!(Content::from_network(&gone).unwrap(), none);
        // Swapping the direction of a binding between two nodes passes the loop check.
        let mut ab = none.clone();
        let mut ba = none.clone();
        let part = ParamPart::parse("value").unwrap();
        let src = |p: &str| BindRef { node: p.parse().unwrap(), channel: None };
        ab.nodes.get_mut(&"/b".parse::<NodePath>().unwrap()).unwrap().binds.insert(part.clone(), src("/a"));
        ba.nodes.get_mut(&"/a".parse::<NodePath>().unwrap()).unwrap().binds.insert(part, src("/b"));
        let swapped = apply(&load(&ab), &diff(&ab, &ba));
        assert_eq!(Content::from_network(&swapped).unwrap(), ba);
    }

    #[test]
    fn positions_load_merge_and_go() {
        let at = |c: &mut Content, path: &str, pos: Option<[f32; 2]>| {
            if let Some(n) = c.nodes.get_mut(&path.parse::<NodePath>().unwrap()) {
                n.pos = pos;
            }
        };
        let mut base =
            content(&[("/a", "test.const", &[], &[], false), ("/b", "test.const", &[], &[], false)]);
        at(&mut base, "/a", Some([10.0, 20.0]));
        roundtrip(&base);
        let net = load(&base);
        // The user moves /b; the file edit moves /a and nothing else.
        let ours = apply(&net, &[Op::Place { path: "/b".parse().unwrap(), pos: Some([0.0, 300.0]) }]);
        let mut theirs = base.clone();
        at(&mut theirs, "/a", Some([50.0, 20.0]));
        assert_eq!(
            diff(&base, &theirs),
            vec![Op::Place { path: "/a".parse().unwrap(), pos: Some([50.0, 20.0]) }]
        );
        let merged = Content::from_network(&apply(&ours, &diff(&base, &theirs))).unwrap();
        let pos = |c: &Content, p: &str| c.nodes[&p.parse::<NodePath>().unwrap()].pos;
        assert_eq!((pos(&merged, "/a"), pos(&merged, "/b")), (Some([50.0, 20.0]), Some([0.0, 300.0])));
        // A position taken out of the file leaves the node to the editor to place.
        let mut gone = base.clone();
        at(&mut gone, "/a", None);
        let after = Content::from_network(&apply(&net, &diff(&base, &gone))).unwrap();
        assert_eq!(after, gone);
    }

    #[test]
    fn removing_a_param_resets_it() {
        let base = content(&[("/a", "test.const", &[("value", 1.0)], &[], false)]);
        let theirs = content(&[("/a", "test.const", &[], &[], false)]);
        let next = apply(&load(&base), &diff(&base, &theirs));
        assert_eq!(Content::from_network(&next).unwrap(), theirs);
    }
}
