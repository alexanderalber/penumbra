//! Renaming a node together with the texts that name it by path.
//!
//! A wire or a binding names its source by id, so it follows a rename by itself. Two kinds of
//! params name nodes by path in text: node reference params ([`penumbra_core::ParamSpec::node`])
//! and the [`EXPOSE`] lists of components. [`rename`] gives the ops that rename a node and
//! rewrite those texts, as one batch, so the whole rename is one undo step. A text is rewritten
//! only where it named the renamed node or a node inside it; every other text stays as written.

use penumbra_core::{Error, Name, NodePath, ParamPath, Value};

use crate::network::{EXPOSE, Network, Op};
use crate::node::NodeTypes;

/// The ops that rename the node at `path` to `name` and rewrite the node references and expose
/// lines that name it or a node inside it. The rename comes first, so the other ops use the
/// new paths.
pub fn rename(net: &Network, types: &NodeTypes, path: &NodePath, name: &Name) -> Result<Vec<Op>, Error> {
    if net.resolve(path).is_none() {
        return Err(Error::new(format!("no node at {path}")));
    }
    let parent = path.parent().ok_or_else(|| Error::new("the top level has no name"))?;
    let new = parent.child(name.clone())?;
    let moved = |p: &NodePath| moved(p, path, &new);
    let mut ops = vec![Op::Rename { path: path.clone(), name: name.clone() }];
    if &new == path {
        return Ok(ops);
    }
    for node in net.nodes() {
        let Some(owner) = net.path_of(node.id) else { continue };
        let owner_now = moved(&owner).unwrap_or_else(|| owner.clone());
        let refs =
            types.desc(&node.kind).map(|d| d.params.iter().filter(|s| s.node_ref)).into_iter().flatten();
        for spec in refs {
            let Ok(param) = Name::new(&spec.name) else { continue };
            let Some(text) = node.params.get(&param).and_then(Value::as_str) else { continue };
            if let Some(fixed) = fix_ref(&owner, &owner_now, text, &moved) {
                ops.push(Op::Set {
                    param: ParamPath::new(owner_now.clone(), param)?,
                    value: Value::Text(fixed),
                });
            }
        }
        let expose = Name::new(EXPOSE)?;
        if let Some(text) = node.params.get(&expose).and_then(Value::as_str) {
            let fixed = rewrite_expose(text, |target| fix_target(&owner, &owner_now, target, &moved));
            if fixed != text {
                ops.push(Op::Set {
                    param: ParamPath::new(owner_now.clone(), expose)?,
                    value: Value::Text(fixed),
                });
            }
        }
    }
    Ok(ops)
}

/// Where `p` is after `from` became `to`: `None` unless `p` is `from` or inside it.
fn moved(p: &NodePath, from: &NodePath, to: &NodePath) -> Option<NodePath> {
    let rest = p.names().strip_prefix(from.names())?;
    NodePath::from_names(to.names().iter().chain(rest).cloned().collect()).ok()
}

/// The node reference `text` of the node `owner` (now at `owner_now`) rewritten for the rename,
/// or `None` when it still names what it named, or names nothing.
fn fix_ref(
    owner: &NodePath,
    owner_now: &NodePath,
    text: &str,
    moved: &impl Fn(&NodePath) -> Option<NodePath>,
) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let target = owner.parent()?.resolve_ref(text).ok()?;
    let target_now = moved(&target)?;
    let level_now = owner_now.parent()?;
    if level_now.resolve_ref(text).ok().as_ref() == Some(&target_now) {
        return None;
    }
    // An absolute reference stays absolute; a relative one is written the shortest way.
    Some(if text.trim().starts_with('/') { target_now.to_string() } else { level_now.write_ref(&target_now) })
}

/// The target `inner/node.param` of an expose line of the component `comp` (now at `comp_now`)
/// rewritten for the rename, or `None` when it does not change.
fn fix_target(
    comp: &NodePath,
    comp_now: &NodePath,
    target: &str,
    moved: &impl Fn(&NodePath) -> Option<NodePath>,
) -> Option<String> {
    if target.starts_with('/') || target.split('/').any(|n| n == ".." || n == ".") {
        return None;
    }
    let joined = if comp.is_root() { format!("/{target}") } else { format!("{comp}/{target}") };
    let param = ParamPath::parse(&joined).ok()?;
    let node_now = moved(param.node())?;
    let inside = node_now.names().strip_prefix(comp_now.names())?;
    let names: Vec<&str> = inside.iter().map(Name::as_str).collect();
    let fixed = format!("{}.{}", names.join("/"), param.param());
    (fixed != target).then_some(fixed)
}

/// `text` (an [`EXPOSE`] list) with the target of each line passed through `fix`; everything
/// else (names, spacing, comments, blank lines) stays as it is.
fn rewrite_expose(text: &str, fix: impl Fn(&str) -> Option<String>) -> String {
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let code_end = line.find('#').unwrap_or(line.len());
            let code = line.get(..code_end).unwrap_or(line);
            let start = code.find('=').map_or(0, |i| i + 1);
            let part = code.get(start..).unwrap_or("");
            let target = part.trim();
            if target.is_empty() {
                return line.to_string();
            }
            let from = start + (part.len() - part.trim_start().len());
            let to = from + target.len();
            match fix(target) {
                Some(new) => {
                    format!("{}{new}{}", line.get(..from).unwrap_or(""), line.get(to..).unwrap_or(""))
                }
                None => line.to_string(),
            }
        })
        .collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{create, test_types};

    fn p(s: &str) -> NodePath {
        s.parse().unwrap()
    }

    fn set(param: &str, text: &str) -> Op {
        Op::Set { param: ParamPath::parse(param).unwrap(), value: Value::Text(text.into()) }
    }

    fn text(net: &Network, param: &str) -> String {
        let param = ParamPath::parse(param).unwrap();
        let id = net.resolve(param.node()).unwrap();
        net.node(id).unwrap().params.get(param.param()).and_then(Value::as_str).unwrap_or("").to_string()
    }

    /// `/outer` holds `/outer/comp`, which holds `lfo` and `inner/deep`; both components expose
    /// params inside, and references from several levels name the nodes.
    fn net() -> Network {
        let types = test_types();
        let ops = vec![
            create("/outer", "test.comp"),
            create("/outer/comp", "test.comp"),
            create("/outer/comp/lfo", "test.const"),
            create("/outer/comp/inner", "test.group"),
            create("/outer/comp/inner/deep", "test.const"),
            create("/outer/comp/near", "test.ref"),
            create("/outer/comp/abs", "test.ref"),
            create("/outer/comp/inner/up", "test.ref"),
            create("/outer/far", "test.ref"),
            create("/other", "test.ref"),
            set(
                "/outer/comp.expose",
                "speed = lfo.value  # the rate\nlfo.value\n\n  deep = inner/deep.value\nbad line",
            ),
            set("/outer.expose", "s = comp/lfo.value\nd=comp/inner/deep.value"),
            set("/outer/comp/near.node", "lfo"),
            set("/outer/comp/abs.node", "/outer/comp/lfo"),
            set("/outer/comp/inner/up.node", "../lfo"),
            set("/outer/far.node", "comp/inner/deep"),
            set("/other.node", "/outer/comp/inner"),
        ];
        Network::new().apply(&types, &ops).unwrap().network
    }

    #[test]
    fn a_rename_rewrites_the_expose_lines_and_references_that_name_the_node() {
        let types = test_types();
        let net = net();
        let ops = rename(&net, &types, &p("/outer/comp/lfo"), &"wave".parse().unwrap()).unwrap();
        let after = net.apply(&types, &ops).unwrap().network;
        assert!(after.resolve(&p("/outer/comp/wave")).is_some());
        assert_eq!(
            text(&after, "/outer/comp.expose"),
            "speed = wave.value  # the rate\nwave.value\n\n  deep = inner/deep.value\nbad line"
        );
        assert_eq!(text(&after, "/outer.expose"), "s = comp/wave.value\nd=comp/inner/deep.value");
        assert_eq!(text(&after, "/outer/comp/near.node"), "wave");
        assert_eq!(text(&after, "/outer/comp/abs.node"), "/outer/comp/wave", "absolute stays absolute");
        assert_eq!(text(&after, "/outer/comp/inner/up.node"), "../wave");
        // Texts that name other nodes stay as written.
        assert_eq!(text(&after, "/outer/far.node"), "comp/inner/deep");
        assert_eq!(ops.len(), 6, "{ops:?}");
    }

    #[test]
    fn renaming_a_component_rewrites_what_names_nodes_inside_it() {
        let types = test_types();
        let net = net();
        let ops = rename(&net, &types, &p("/outer/comp"), &"c2".parse().unwrap()).unwrap();
        let after = net.apply(&types, &ops).unwrap().network;
        assert_eq!(text(&after, "/outer.expose"), "s = c2/lfo.value\nd=c2/inner/deep.value");
        assert_eq!(text(&after, "/outer/far.node"), "c2/inner/deep");
        assert_eq!(text(&after, "/other.node"), "/outer/c2/inner");
        assert_eq!(text(&after, "/outer/c2/abs.node"), "/outer/c2/lfo");
        // Inside the renamed component, relative texts still hold.
        assert_eq!(text(&after, "/outer/c2/near.node"), "lfo");
        assert_eq!(text(&after, "/outer/c2/inner/up.node"), "../lfo");
        assert!(text(&after, "/outer/c2.expose").starts_with("speed = lfo.value"));
    }

    #[test]
    fn a_rename_to_the_same_name_or_of_a_missing_node_changes_no_text() {
        let types = test_types();
        let net = net();
        let ops = rename(&net, &types, &p("/outer/comp/lfo"), &"lfo".parse().unwrap()).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(rename(&net, &types, &p("/nope"), &"x".parse().unwrap()).is_err());
    }
}
