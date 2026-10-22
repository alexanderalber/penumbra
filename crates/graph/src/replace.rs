//! Replacing a node with one of another kind, keeping what fits: the same name and place in the
//! network, the params the new kind has too, the wires of matching data kinds (by port name
//! first, then by position among the ports of that kind) and the bindings that still apply.
//!
//! The result is one batch of ops, so it is one undo step. `Op::Delete` clears the inputs and
//! bindings that pointed at the old node, so the batch reconnects them after creating the new
//! one. Each reconnection is tried on its own first: one that the new kind refuses (a loop
//! through an input that no longer reads the previous frame, a binding onto a param that cannot
//! be bound) is reported as lost instead of failing the whole replacement.

use penumbra_core::{Error, NodePath, ParamPath};

use crate::network::{Network, Op, Port};
use crate::node::{DataKind, NodeTypes};

/// The ops that replace a node, and what could not be kept, each as a short phrase
/// (`input b from /lfo1`, `param scale`).
#[derive(Clone, Debug, PartialEq)]
pub struct Replacement {
    pub ops: Vec<Op>,
    pub lost: Vec<String>,
}

/// A port by name and data kind.
type Spec<'a> = (&'a str, DataKind);

/// For each port of `from` in `used`, the port of `to` it maps to: the one with the same name and
/// kind, else the first free one of that kind (so the rest keep their order). Each port of `to`
/// is taken once.
fn match_ports(from: &[Spec<'_>], to: &[Spec<'_>], used: &[usize]) -> Vec<Option<usize>> {
    let mut map: Vec<Option<usize>> = vec![None; from.len()];
    let mut free: Vec<bool> = vec![true; to.len()];
    for by_name in [true, false] {
        for &i in used {
            let Some((name, kind)) = from.get(i) else { continue };
            if map.get(i).copied().flatten().is_some() {
                continue;
            }
            let fits = |j: usize| {
                free.get(j).copied().unwrap_or(false)
                    && to.get(j).is_some_and(|(n, k)| k == kind && (!by_name || n == name))
            };
            if let Some(j) = (0..to.len()).find(|j| fits(*j))
                && let (Some(m), Some(f)) = (map.get_mut(i), free.get_mut(j))
            {
                *m = Some(j);
                *f = false;
            }
        }
    }
    map
}

/// The ops that replace the node at `path` with a new node of `kind` (see the module docs).
/// Components are refused either way: what they hold would be lost.
pub fn replace(net: &Network, types: &NodeTypes, path: &NodePath, kind: &str) -> Result<Replacement, Error> {
    let id = net.resolve(path).ok_or_else(|| Error::new(format!("no node at {path}")))?;
    let node = net.node(id).ok_or_else(|| Error::new(format!("no node at {path}")))?;
    let old =
        types.desc(&node.kind).ok_or_else(|| Error::new(format!("unknown node kind `{}`", node.kind)))?;
    let new = types.desc(kind).ok_or_else(|| Error::new(format!("unknown node kind `{kind}`")))?;
    if node.kind == kind {
        return Err(Error::new(format!("{path} is a {kind} already")));
    }
    if old.container || new.container {
        return Err(Error::new(
            "components cannot be replaced or replace a node (what they hold would be lost); \
             delete and add instead",
        ));
    }
    let mut lost: Vec<String> = Vec::new();

    // Params: those the new kind has, where the value fits it.
    let mut params = std::collections::BTreeMap::new();
    for (name, value) in &node.params {
        match new.param_spec(name.as_str()).map(|spec| spec.check(value)) {
            Some(Ok(v)) => {
                params.insert(name.clone(), v);
            }
            _ => lost.push(format!("param {name}")),
        }
    }
    let mut base = vec![
        Op::Delete { path: path.clone() },
        Op::Create { path: path.clone(), kind: kind.to_string(), params },
    ];
    if node.pos.is_some() {
        base.push(Op::Place { path: path.clone(), pos: node.pos });
    }
    let mut work = net.apply(types, &base)?.network;
    let mut ops = base;
    // Keeps `op` when the network takes it, else reports `what` as lost.
    let mut attempt =
        |op: Op, what: String, lost: &mut Vec<String>| match work.apply(types, std::slice::from_ref(&op)) {
            Ok(applied) => {
                work = applied.network;
                ops.push(op);
            }
            Err(_) => lost.push(what),
        };

    let old_in: Vec<Spec<'_>> = old.inputs.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    let new_in: Vec<Spec<'_>> = new.inputs.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    let old_out: Vec<Spec<'_>> = old.outputs.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    let new_out: Vec<Spec<'_>> = new.outputs.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    let name_of =
        |ports: &[Spec<'_>], i: usize| ports.get(i).map_or(String::new(), |(n, _)| (*n).to_string());

    // Outputs read by anyone (the node itself too, through an input that sees the previous frame).
    let mut read: Vec<usize> = net
        .nodes()
        .flat_map(|n| n.inputs.iter().flatten().filter(|s| s.node == id).map(|s| s.output as usize))
        .collect();
    read.sort_unstable();
    read.dedup();
    let out_map = match_ports(&old_out, &new_out, &read);
    let map_output = |o: u32| out_map.get(o as usize).copied().flatten().and_then(|m| u32::try_from(m).ok());
    let source_path = |port: Port| -> Option<(NodePath, u32)> {
        if port.node == id {
            Some((path.clone(), map_output(port.output)?))
        } else {
            Some((net.path_of(port.node)?, port.output))
        }
    };
    let connected: Vec<usize> =
        node.inputs.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).collect();
    let in_map = match_ports(&old_in, &new_in, &connected);

    // Inputs of the node itself.
    for (i, slot) in node.inputs.iter().enumerate() {
        let Some(port) = slot else { continue };
        let from_text = net.path_of(port.node).map_or_else(|| "?".to_string(), |p| p.to_string());
        let what = format!("input {} from {from_text}", name_of(&old_in, i));
        let target = in_map.get(i).copied().flatten().and_then(|t| u32::try_from(t).ok());
        match (target, source_path(*port)) {
            (Some(input), Some((from, output))) => {
                attempt(Op::Connect { from, output, to: path.clone(), input }, what, &mut lost);
            }
            _ => lost.push(what),
        }
    }

    // Readers of its outputs.
    for reader in net.nodes().filter(|n| n.id != id) {
        for (slot, src) in reader.inputs.iter().enumerate() {
            let Some(src) = src.filter(|s| s.node == id) else { continue };
            let Some(to) = net.path_of(reader.id) else { continue };
            let reader_desc = types.desc(&reader.kind);
            let input_name =
                reader_desc.and_then(|d| d.inputs.get(slot)).map_or(String::new(), |s| s.name.clone());
            let what = format!("output {} to {to}:{input_name}", name_of(&old_out, src.output as usize));
            match (map_output(src.output), u32::try_from(slot)) {
                (Some(output), Ok(input)) => {
                    attempt(Op::Connect { from: path.clone(), output, to, input }, what, &mut lost);
                }
                _ => lost.push(what),
            }
        }
    }

    // Bindings onto its params, and bindings to it from elsewhere.
    for (part, bind) in &node.binds {
        let Some(from) = (bind.node != id).then(|| net.path_of(bind.node)).flatten() else { continue };
        let what = format!("binding of {} to {from}", part_text(part.param.as_str(), part.component));
        if new.param_spec(part.param.as_str()).is_none() {
            lost.push(what);
            continue;
        }
        let param = ParamPath::new(path.clone(), part.param.clone())?;
        attempt(
            Op::Bind { param, component: part.component, from, channel: bind.channel.clone() },
            what,
            &mut lost,
        );
    }
    for reader in net.nodes().filter(|n| n.id != id) {
        for (part, bind) in reader.binds.iter().filter(|(_, b)| b.node == id) {
            let Some(reader_path) = net.path_of(reader.id) else { continue };
            let param = ParamPath::new(reader_path, part.param.clone())?;
            let what = format!("binding of {}", part_text(&param.to_string(), part.component));
            attempt(
                Op::Bind {
                    param,
                    component: part.component,
                    from: path.clone(),
                    channel: bind.channel.clone(),
                },
                what,
                &mut lost,
            );
        }
    }
    Ok(Replacement { ops, lost })
}

fn part_text(param: &str, component: Option<u8>) -> String {
    match component {
        Some(c) => format!("{param}.{c}"),
        None => param.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use penumbra_core::Value;

    use super::*;
    use crate::testing::{create, test_types};

    fn p(s: &str) -> NodePath {
        NodePath::parse(s).unwrap()
    }

    fn connect(from: &str, output: u32, to: &str, input: u32) -> Op {
        Op::Connect { from: p(from), output, to: p(to), input }
    }

    #[test]
    fn keeps_name_params_wires_and_bindings_that_fit() {
        let types = test_types();
        let ops = [
            create("/c", "test.const"),
            Op::Set { param: ParamPath::parse("/c.value").unwrap(), value: Value::Float(3.0) },
            create("/k", "test.const"),
            create("/add", "test.add"),
            create("/sink", "test.sink"),
            connect("/c", 0, "/add", 1),
            connect("/c", 0, "/sink", 0),
            Op::Bind {
                param: ParamPath::parse("/k.value").unwrap(),
                component: None,
                from: p("/c"),
                channel: None,
            },
            Op::Place { path: p("/c"), pos: Some([120.0, -40.0]) },
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        let r = replace(&net, &types, &p("/c"), "test.fail").unwrap();
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        let after = net.apply(&types, &r.ops).unwrap().network;
        let c = after.resolve(&p("/c")).unwrap();
        let node = after.node(c).unwrap();
        assert_eq!(node.kind, "test.fail");
        assert_eq!(node.params.get("value"), Some(&Value::Float(3.0)));
        assert_eq!(node.pos, Some([120.0, -40.0]), "the new node takes the old one's place");
        let add = after.node(after.resolve(&p("/add")).unwrap()).unwrap();
        assert_eq!(add.inputs, vec![None, Some(Port { node: c, output: 0 })]);
        let sink = after.node(after.resolve(&p("/sink")).unwrap()).unwrap();
        assert_eq!(sink.inputs, vec![Some(Port { node: c, output: 0 })]);
        let k = after.node(after.resolve(&p("/k")).unwrap()).unwrap();
        assert!(k.binds.values().all(|b| b.node == c) && k.binds.len() == 1);
    }

    #[test]
    fn reports_what_does_not_fit() {
        let types = test_types();
        let ops = [
            create("/a", "test.const"),
            create("/b", "test.const"),
            create("/add", "test.add"),
            Op::Set { param: ParamPath::parse("/a.value").unwrap(), value: Value::Float(1.0) },
            connect("/a", 0, "/add", 0),
            connect("/b", 0, "/add", 1),
            create("/sink", "test.sink"),
            connect("/add", 0, "/sink", 0),
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        // A sink has one chan input and no output: `a` stays (by position), `b` and the reader go.
        let r = replace(&net, &types, &p("/add"), "test.sink").unwrap();
        assert_eq!(r.lost, vec!["input b from /b".to_string(), "output out to /sink:in".to_string()]);
        let after = net.apply(&types, &r.ops).unwrap().network;
        let add = after.node(after.resolve(&p("/add")).unwrap()).unwrap();
        let a = after.resolve(&p("/a")).unwrap();
        assert_eq!(add.inputs, vec![Some(Port { node: a, output: 0 })]);
        // A param the new kind lacks is reported.
        let r = replace(&net, &types, &p("/a"), "test.tex").unwrap();
        assert!(r.lost.contains(&"param value".to_string()), "{:?}", r.lost);
        assert!(r.lost.contains(&"output out to /add:a".to_string()), "{:?}", r.lost);
    }

    #[test]
    fn refuses_components_the_same_kind_and_unknown_kinds() {
        let types = test_types();
        let ops = [create("/g", "test.group"), create("/c", "test.const")];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        assert!(replace(&net, &types, &p("/g"), "test.const").is_err());
        assert!(replace(&net, &types, &p("/c"), "test.group").is_err());
        assert!(replace(&net, &types, &p("/c"), "test.const").is_err());
        assert!(replace(&net, &types, &p("/c"), "test.nope").is_err());
        assert!(replace(&net, &types, &p("/nothing"), "test.const").is_err());
    }
}
