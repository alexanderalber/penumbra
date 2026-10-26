//! The project file, `project.ron`.
//!
//! ```ron
//! // penumbra project
//! (
//!     version: 2,
//!     settings: {"persist_state": true},
//!     nodes: {
//!         "/lfo1": (kind: "chan.lfo", params: {"rate": 0.5}, pos: [0.0, 0.0]),
//!         "/mix": (kind: "chan.math", inputs: {"a": "lfo1", "b": "split:right"}, locked: true, pos: [200.0, 0.0]),
//!         "/scene": (kind: "comp.base", pos: [400.0, 0.0]),
//!         "/scene/move": (kind: "tex.transform", binds: {"rotate": "/lfo1", "translate.0": "pos:x"}, pos: [0.0, 0.0]),
//!     },
//! )
//! ```
//!
//! Nodes are keyed by path, one per line, sorted. `params` holds only values that differ from the
//! declared default (the app writes it that way; a file may list defaults too). `inputs` maps an
//! input name to the node feeding it: `"path"` for its first output, `"path:name"` (or `"path:1"`)
//! for another. `binds` maps a parameter (or one component, `translate.0`) to the channel driving
//! it: `"path"` for the first channel of the node's first `chan` output, `"path:name"` for the
//! channel called `name` (after the colon comes a channel here, not an output). Node keys are
//! absolute; the paths in `inputs` and `binds` are read on the level of the node they belong to, by
//! the rule node references use ([`NodePath::resolve_ref`]): absolute (`/a/b`), a name beside the
//! node, `sub/x` below, `../x` up. The app writes the shorter form ([`NodePath::write_ref`]).
//! `pos` is where the node sits in the editor, in graph units on the plane of the component it
//! is in (each component's inside has a plane of its own); without it the editor places the node
//! and the next save writes where. A position that is not two finite numbers is dropped when
//! the file is read.
//! `settings` holds the project settings that differ from their default
//! ([`penumbra_graph::SETTINGS`]). Missing `settings`, `params`, `inputs`, `binds`, `locked` and
//! `pos` mean empty, `false` and not placed. Unknown fields are refused, so a typo is an error
//! rather than silently ignored.
//!
//! Version 2 added `pos` (older builds refuse it as an unknown field, so the version tells them
//! instead); files of version 1 read as they are.

use std::collections::BTreeMap;

use penumbra_core::{Error, Name, NodePath, ParamPart, Value};
use penumbra_graph::{
    DataKind, Network, NodeDesc, NodeTypes, Params, broken, check_param, check_pos, check_setting,
};
use serde::{Deserialize, Serialize};

use crate::content::{BindRef, Content, NodeContent, PortRef};

/// File name inside the project directory.
pub const FILE_NAME: &str = "project.ron";
/// Newest format this build reads and the one it writes.
pub const FORMAT_VERSION: u32 = 2;
/// Largest project file read. Far above any real project; a hostile or runaway file is
/// refused before it is parsed.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

const HEADER: &str = "// penumbra project. Written by the app about a second after each change; while the app\n\
// runs, edits to this file are merged into the open project.\n";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRepr {
    version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<Name, Value>,
    #[serde(default)]
    nodes: BTreeMap<NodePath, NodeRepr>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeRepr {
    kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<Name, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inputs: BTreeMap<Name, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    binds: BTreeMap<ParamPart, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    locked: bool,
    /// Written as `[x, y]` (a list, like vector params), not as `Some((x, y))`; `(x, y)` reads
    /// too.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "write_pos",
        deserialize_with = "read_pos"
    )]
    pos: Option<[f32; 2]>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn write_pos<S: serde::Serializer>(pos: &Option<[f32; 2]>, s: S) -> Result<S::Ok, S::Error> {
    match pos {
        Some(p) => s.collect_seq(p.iter()),
        None => s.serialize_none(),
    }
}

fn read_pos<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<[f32; 2]>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Form {
        List(Vec<f32>),
        Pair((f32, f32)),
    }
    match Form::deserialize(d)? {
        Form::Pair((x, y)) => Ok(Some([x, y])),
        Form::List(list) => match list.as_slice() {
            [x, y] => Ok(Some([*x, *y])),
            _ => Err(serde::de::Error::custom(format!("a position is [x, y], got {} numbers", list.len()))),
        },
    }
}

fn desc<'t>(types: &'t NodeTypes, path: &NodePath, kind: &str) -> Result<&'t NodeDesc, Error> {
    types.desc(kind).ok_or_else(|| Error::new(format!("{path}: unknown node kind `{kind}`")))
}

/// Reads the text of a project file, all or nothing: any node that does not load fails the
/// whole text (component files, the demo). Checks the syntax, the version, the node kinds and
/// the connections; param values are checked when the content is applied to a network.
pub fn parse(text: &str, types: &NodeTypes) -> Result<Content, Error> {
    read(text, types, FILE_NAME, false, false)
}

/// Reads the text of a project file as a project opens or merges it: a node that does not load
/// (param values included) becomes a placeholder ([`penumbra_graph::broken`]) holding its
/// entry and the reason, and the rest loads. Only a text that is no project file at all (its syntax, its version, a settings
/// value) fails as a whole; so do the few problems between nodes (a loop, a node inside one
/// that cannot hold nodes), which the network refuses when the content is applied.
pub fn load(text: &str, types: &NodeTypes) -> Result<Content, Error> {
    read(text, types, FILE_NAME, false, true)
}

/// [`parse`] for a text called `what` in errors. With `outside`, inputs and bindings may name
/// nodes that are not in the text (copied nodes: [`crate::clipboard`]); an input from such a
/// node gives its output by index, since its kind is not known here.
pub(crate) fn parse_as(text: &str, types: &NodeTypes, what: &str, outside: bool) -> Result<Content, Error> {
    read(text, types, what, outside, false)
}

fn read(text: &str, types: &NodeTypes, what: &str, outside: bool, tolerant: bool) -> Result<Content, Error> {
    let repr: FileRepr = ron::from_str(text).map_err(|e| Error::new(format!("{what}: {e}")))?;
    if repr.version == 0 || repr.version > FORMAT_VERSION {
        return Err(Error::new(format!(
            "{what}: format version {} is not supported; this build reads version {FORMAT_VERSION}",
            repr.version
        )));
    }
    let mut settings = BTreeMap::new();
    for (name, value) in &repr.settings {
        if let Some(v) =
            check_setting(name, Some(value)).map_err(|e| e.context(format!("{what} settings")))?
        {
            settings.insert(name.clone(), v);
        }
    }
    // Which nodes do not load: first what each node gets wrong by itself, then wires and
    // bindings. A wire or binding from a node that does not load is taken as written, so
    // finding one more such node only lets readers load; the passes end after one or two.
    let mut failed: BTreeMap<&NodePath, Error> = BTreeMap::new();
    for (path, node) in &repr.nodes {
        if let Err(e) = check_own(path, node, types, tolerant) {
            failed.insert(path, e);
        }
    }
    loop {
        let mut more = false;
        for (path, node) in &repr.nodes {
            if failed.contains_key(path) {
                continue;
            }
            let mut outputs = BTreeMap::new();
            if let Err(e) =
                load_node(path, node, &repr.nodes, &failed, &mut outputs, types, outside, tolerant)
            {
                failed.insert(path, e);
                more = true;
            }
        }
        if !more {
            break;
        }
    }
    if !tolerant && let Some(e) = failed.values().next() {
        return Err(e.clone());
    }
    let mut nodes = BTreeMap::new();
    // Per placeholder, the output names its readers use ([`broken::OUTPUTS`]).
    let mut outputs: BTreeMap<NodePath, Vec<String>> = BTreeMap::new();
    for (path, node) in &repr.nodes {
        if failed.contains_key(path) {
            continue;
        }
        let mut content = load_node(path, node, &repr.nodes, &failed, &mut outputs, types, outside, tolerant)
            .map_err(|e| e.context(format!("{what}: {path} loaded once, then not")))?;
        content.pos = position(path, node.pos);
        nodes.insert(path.clone(), content);
    }
    for (path, error) in failed {
        let node = repr.nodes.get(path).ok_or_else(|| Error::new("lost a node"))?;
        // The position lives on the placeholder like on any node, so it can be moved.
        let entry = ron::to_string(&NodeRepr { pos: None, ..node.clone() })
            .map_err(|e| Error::new(format!("{path}: {e}")))?;
        if entry.len() > penumbra_core::param::MAX_TEXT {
            return Err(error.context(format!("{what} (the entry is too long to keep as a placeholder)")));
        }
        log::warn!("{path} did not load and is kept as it is in {what}: {error}");
        let mut params = BTreeMap::new();
        params.insert(Name::new(broken::ENTRY)?, Value::Text(entry));
        // The placeholder is shown with its path already; the error alone goes into it.
        let message = error.0.strip_prefix(&format!("{path}: ")).map(str::to_string).unwrap_or(error.0);
        params.insert(Name::new(broken::ERROR)?, Value::Text(message));
        if let Some(names) = outputs.get(path).filter(|n| !n.is_empty()) {
            params.insert(Name::new(broken::OUTPUTS)?, Value::Text(names.join("\n")));
        }
        nodes.insert(
            path.clone(),
            NodeContent {
                kind: broken::BROKEN_KIND.to_string(),
                params,
                inputs: Vec::new(),
                binds: BTreeMap::new(),
                locked: false,
                pos: position(path, node.pos),
            },
        );
    }
    Ok(Content { nodes, settings })
}

/// A node's position as read, dropped (with a warning) when it is no place: a position is
/// convenience, and the editor places a node without one.
fn position(path: &NodePath, pos: Option<[f32; 2]>) -> Option<[f32; 2]> {
    let pos = pos?;
    match check_pos(pos) {
        Ok(p) => Some(p),
        Err(e) => {
            log::warn!("{path}: {e}; the editor places it anew");
            None
        }
    }
}

/// What can be checked of the node at `path` without looking at other nodes; its declaration.
fn check_own<'t>(
    path: &NodePath,
    node: &NodeRepr,
    types: &'t NodeTypes,
    check_params: bool,
) -> Result<&'t NodeDesc, Error> {
    if node.kind == broken::BROKEN_KIND {
        return Err(Error::new(format!("{path}: `{}` is no kind a file may name", node.kind)));
    }
    let d = desc(types, path, &node.kind)?;
    for (name, value) in node.params.iter().filter(|_| check_params) {
        // Params the kind does not declare are kept ([`Params::Keep`]), as when it is applied.
        check_param(d, name, value, Params::Keep).map_err(|e| e.context(format!("{path} param")))?;
    }
    if node.locked && d.outputs.is_empty() {
        return Err(Error::new(format!("{path} is locked, but `{}` has no outputs to lock", d.kind)));
    }
    for name in node.inputs.keys() {
        if d.input_index(name.as_str()).is_none() {
            return Err(Error::new(format!(
                "{path} input `{name}`: `{}` has no such input; inputs: {}",
                d.kind,
                d.input_names()
            )));
        }
    }
    for part in node.binds.keys() {
        if let Some(spec) = d.param_spec(part.param.as_str()) {
            spec.check_bindable(part.component).map_err(|e| e.context(format!("{path} bind `{part}`")))?;
        }
    }
    Ok(d)
}

/// The content of the node at `path`, or why it does not load. `failed` holds the nodes known
/// not to load; `outputs` collects the output names read from them. Param values are checked
/// only with `check_params`; otherwise the network checks them when the content is applied.
#[allow(clippy::too_many_arguments)]
fn load_node(
    path: &NodePath,
    node: &NodeRepr,
    nodes: &BTreeMap<NodePath, NodeRepr>,
    failed: &BTreeMap<&NodePath, Error>,
    outputs: &mut BTreeMap<NodePath, Vec<String>>,
    types: &NodeTypes,
    outside: bool,
    check_params: bool,
) -> Result<NodeContent, Error> {
    let d = check_own(path, node, types, check_params)?;
    let level = path.parent().unwrap_or_else(NodePath::root);
    let mut inputs = vec![None; d.inputs.len()];
    for (name, source) in &node.inputs {
        let ctx = || format!("{path} input `{name}`");
        let index = d.input_index(name.as_str()).ok_or_else(|| {
            Error::new(format!("{}: `{}` has no such input; inputs: {}", ctx(), d.kind, d.input_names()))
        })?;
        let (port, kind) = parse_source(&level, source, nodes, failed, outputs, types, outside)
            .map_err(|e| e.context(ctx()))?;
        if let (Some(kind), Some(spec)) = (kind, d.inputs.get(index))
            && kind != spec.kind
        {
            return Err(Error::new(format!(
                "{}: {source} gives {kind:?}, the input wants {:?}",
                ctx(),
                spec.kind
            )));
        }
        if let Some(slot) = inputs.get_mut(index) {
            *slot = Some(port);
        }
    }
    let mut binds = BTreeMap::new();
    for (part, source) in &node.binds {
        let ctx = || format!("{path} bind `{part}`");
        let src = parse_bind(&level, source, nodes, outside).map_err(|e| e.context(ctx()))?;
        if let Some(from) = nodes.get(&src.node).filter(|_| !failed.contains_key(&src.node))
            && !types.desc(&from.kind).is_some_and(|f| f.outputs.iter().any(|o| o.kind == DataKind::Chan))
        {
            return Err(Error::new(format!("{}: {} has no chan output to bind to", ctx(), src.node)));
        }
        binds.insert(part.clone(), src);
    }
    Ok(NodeContent {
        kind: node.kind.clone(),
        params: node.params.clone(),
        inputs,
        binds,
        locked: node.locked,
        pos: None,
    })
}

/// `"path"` or `"path:channel"`, read on `level`.
fn parse_bind(
    level: &NodePath,
    text: &str,
    nodes: &BTreeMap<NodePath, NodeRepr>,
    outside: bool,
) -> Result<BindRef, Error> {
    let (path, channel) = match text.split_once(':') {
        Some((p, c)) => (p, Some(Name::new(c).map_err(|e| e.context("channel"))?)),
        None => (text, None),
    };
    let node = level.resolve_ref(path)?;
    if !outside && !nodes.contains_key(&node) {
        return Err(Error::new(format!("{node} is not in the file")));
    }
    Ok(BindRef { node, channel })
}

/// `"path"` or `"path:output"`, where output is a name or an index, read on `level`; with the
/// data kind of that output when it is known.
fn parse_source(
    level: &NodePath,
    text: &str,
    nodes: &BTreeMap<NodePath, NodeRepr>,
    failed: &BTreeMap<&NodePath, Error>,
    outputs: &mut BTreeMap<NodePath, Vec<String>>,
    types: &NodeTypes,
    outside: bool,
) -> Result<(PortRef, Option<DataKind>), Error> {
    let (path, output) = match text.split_once(':') {
        Some((p, o)) => (p, Some(o)),
        None => (text, None),
    };
    let path = level.resolve_ref(path)?;
    if outside && !nodes.contains_key(&path) {
        let output = match output {
            None => 0,
            Some(o) => o.parse::<u32>().map_err(|_| {
                Error::new(format!("{path} is not in the text; give its output by number, not as `{o}`"))
            })?,
        };
        return Ok((PortRef { node: path, output }, None));
    }
    let source = nodes.get(&path).ok_or_else(|| Error::new(format!("{path} is not in the file")))?;
    if failed.contains_key(&path) {
        // Its outputs are unknown: a number stays a number, a name gets a number of its own.
        let output = match output {
            None => 0,
            Some(o) => match o.parse::<u32>() {
                Ok(i) => i,
                Err(_) => {
                    let names = outputs.entry(path.clone()).or_default();
                    let at = match names.iter().position(|n| n == o) {
                        Some(at) => at,
                        None => {
                            names.push(o.to_string());
                            names.len() - 1
                        }
                    };
                    u32::try_from(at)
                        .ok()
                        .and_then(|at| broken::NAMED_OUTPUT.checked_add(at))
                        .ok_or_else(|| Error::new(format!("{path}: too many output names")))?
                }
            },
        };
        return Ok((PortRef { node: path, output }, None));
    }
    let d = desc(types, &path, &source.kind)?;
    let output = match output {
        None => 0,
        Some(o) => match o.parse::<u32>() {
            Ok(i) => i,
            Err(_) => d
                .outputs
                .iter()
                .position(|out| out.name == o)
                .and_then(|i| u32::try_from(i).ok())
                .ok_or_else(|| {
                    let known: Vec<&str> = d.outputs.iter().map(|out| out.name.as_str()).collect();
                    Error::new(format!("{path} has no output `{o}`; outputs: {}", known.join(", ")))
                })?,
        },
    };
    let spec =
        d.outputs.get(output as usize).ok_or_else(|| Error::new(format!("{path} has no output {output}")))?;
    Ok((PortRef { node: path, output }, Some(spec.kind)))
}

/// A node of a project that did not load, as `project.info` lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrokenNode {
    pub path: String,
    /// The kind its entry names.
    pub kind: String,
    pub error: String,
}

/// The placeholders in `net` ([`penumbra_graph::broken`]).
pub fn broken_nodes(net: &Network) -> Vec<BrokenNode> {
    let mut out: Vec<BrokenNode> = net
        .nodes()
        .filter(|n| broken::is_broken(n))
        .map(|n| BrokenNode {
            path: net.path_of(n.id).map(|p| p.to_string()).unwrap_or_default(),
            kind: ron::from_str::<NodeRepr>(broken::text(n, broken::ENTRY))
                .map(|r| r.kind)
                .unwrap_or_default(),
            error: broken::text(n, broken::ERROR).to_string(),
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// How a reader writes output `output` of `from`, which it calls `name`: by the output's name
/// where it has one.
fn output_ref(
    content: &Content,
    types: &NodeTypes,
    from: &NodePath,
    name: String,
    output: u32,
) -> Result<String, Error> {
    let Some(n) = content.nodes.get(from) else {
        // A source outside the content (copied nodes) by number: its kind is not known.
        return Ok(format!("{name}:{output}"));
    };
    if n.kind == broken::BROKEN_KIND {
        let named = output
            .checked_sub(broken::NAMED_OUTPUT)
            .and_then(|i| n.params.get(broken::OUTPUTS)?.as_str()?.lines().nth(i as usize));
        return Ok(match named {
            Some(o) => format!("{name}:{o}"),
            None => format!("{name}:{output}"),
        });
    }
    Ok(match desc(types, from, &n.kind)?.outputs.get(output as usize) {
        Some(out) => format!("{name}:{}", out.name),
        None => format!("{name}:{output}"),
    })
}

/// The text of a project file for `content`, in canonical form: sorted by path, one node per
/// line, outputs other than the first named. A placeholder is written as the entry it holds.
pub fn print(content: &Content, types: &NodeTypes) -> Result<String, Error> {
    let mut nodes = BTreeMap::new();
    for (path, node) in &content.nodes {
        if node.kind == broken::BROKEN_KIND {
            let entry = node.params.get(broken::ENTRY).and_then(Value::as_str).unwrap_or_default();
            let repr: NodeRepr = ron::from_str(entry)
                .map_err(|e| Error::new(format!("{path}: the entry it keeps does not read: {e}")))?;
            nodes.insert(path.clone(), NodeRepr { pos: node.pos, ..repr });
            continue;
        }
        let d = desc(types, path, &node.kind)?;
        let level = path.parent().unwrap_or_else(NodePath::root);
        let mut inputs = BTreeMap::new();
        for (index, slot) in node.inputs.iter().enumerate() {
            let Some(src) = slot else { continue };
            let input =
                d.inputs.get(index).ok_or_else(|| Error::new(format!("{path} has no input {index}")))?;
            let from = level.write_ref(&src.node);
            let source = match src.output {
                0 => from,
                o => output_ref(content, types, &src.node, from, o)?,
            };
            inputs.insert(Name::new(&input.name)?, source);
        }
        let binds = node
            .binds
            .iter()
            .map(|(part, b)| {
                let from = level.write_ref(&b.node);
                let source = match &b.channel {
                    Some(c) => format!("{from}:{c}"),
                    None => from,
                };
                (part.clone(), source)
            })
            .collect();
        nodes.insert(
            path.clone(),
            NodeRepr {
                kind: node.kind.clone(),
                params: node.params.clone(),
                inputs,
                binds,
                locked: node.locked,
                pos: node.pos,
            },
        );
    }
    let repr = FileRepr { version: FORMAT_VERSION, settings: content.settings.clone(), nodes };
    // Depth 2 (the file, the node map) is spread over lines; each node stays on one line.
    let pretty = ron::ser::PrettyConfig::new().depth_limit(2);
    let body = ron::ser::to_string_pretty(&repr, pretty).map_err(|e| Error::new(e.to_string()))?;
    Ok(format!("{HEADER}{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{content, test_types};

    #[test]
    fn print_then_parse_is_identity() {
        let types = test_types();
        let c = content(&[
            ("/g", "test.group", &[], &[], false),
            ("/g/a", "test.const", &[("value", 2.5)], &[], false),
            ("/sum", "test.add", &[], &[Some(("/g/a", 0)), Some(("/sum2", 1))], true),
            ("/sum2", "test.add", &[], &[], false),
        ]);
        let text = print(&c, &types).unwrap();
        assert_eq!(parse(&text, &types).unwrap(), c);
        assert!(
            text.contains(
                r#""/sum": (kind: "test.add", inputs: {"a": "g/a", "b": "sum2:neg"}, locked: true),"#
            ),
            "{text}"
        );
        assert!(text.contains(r#""/g/a": (kind: "test.const", params: {"value": 2.5}),"#), "{text}");
    }

    #[test]
    fn binds_survive_and_bad_ones_are_refused() {
        let types = test_types();
        let text = r#"(version: 1, nodes: {
            "/a": (kind: "test.const"),
            "/b": (kind: "test.const", binds: {"value": "/a:value"}),
        })"#;
        let c = parse(text, &types).unwrap();
        let printed = print(&c, &types).unwrap();
        assert!(printed.contains(r#""/b": (kind: "test.const", binds: {"value": "a:value"}),"#), "{printed}");
        assert_eq!(parse(&printed, &types).unwrap(), c);
        for (bad, want) in [
            (r#"binds: {"value": "/x"}"#, "not in the file"),
            (r#"binds: {"value": "/a:no-name"}"#, "channel"),
            (r#"binds: {"value.7": "/a"}"#, "component"),
        ] {
            let text = format!(
                r#"(version: 1, nodes: {{"/a": (kind: "test.const"), "/b": (kind: "test.const", {bad})}})"#
            );
            let e = parse(&text, &types).unwrap_err();
            assert!(e.0.contains(want), "{bad}: {e}");
        }
    }

    #[test]
    fn paths_are_written_relative_and_read_either_way() {
        let types = test_types();
        let c = content(&[
            ("/top", "test.const", &[], &[], false),
            ("/g", "test.group", &[], &[], false),
            ("/g/a", "test.const", &[], &[], false),
            ("/g/h", "test.group", &[], &[], false),
            ("/g/h/s", "test.add", &[], &[Some(("/g/a", 0)), Some(("/top", 0))], false),
            ("/g/s", "test.add", &[], &[Some(("/g/h/s", 1))], false),
        ]);
        let text = print(&c, &types).unwrap();
        assert!(
            text.contains(r#""/g/h/s": (kind: "test.add", inputs: {"a": "../a", "b": "/top"}),"#),
            "{text}"
        );
        assert!(text.contains(r#""/g/s": (kind: "test.add", inputs: {"a": "h/s:neg"}),"#), "{text}");
        assert_eq!(parse(&text, &types).unwrap(), c);
        // Absolute paths, as files before relative ones were written, read the same.
        let old = text.replace(r#""../a""#, r#""/g/a""#).replace(r#""h/s:neg""#, r#""/g/h/s:neg""#);
        assert_ne!(old, text);
        assert_eq!(parse(&old, &types).unwrap(), c);
        let binds = r#"(version: 1, nodes: {
            "/a": (kind: "test.const"),
            "/g": (kind: "test.group"),
            "/g/b": (kind: "test.const", binds: {"value": "../a"}),
            "/g/c": (kind: "test.const", binds: {"value": "b"}),
        })"#;
        let read = parse(binds, &types).unwrap();
        let bind = |p: &str| {
            read.nodes[&p.parse::<NodePath>().unwrap()].binds.values().next().unwrap().node.to_string()
        };
        assert_eq!(bind("/g/b"), "/a");
        assert_eq!(bind("/g/c"), "/g/b");
        let printed = print(&read, &types).unwrap();
        assert!(printed.contains(r#"binds: {"value": "/a"}"#), "{printed}");
        assert!(printed.contains(r#"binds: {"value": "b"}"#), "{printed}");
        let e = parse(&binds.replace("../a", "../../a"), &types).unwrap_err();
        assert!(e.0.contains("above the top level"), "{e}");
    }

    #[test]
    fn values_of_every_type_survive() {
        let types = test_types();
        let values = [
            Value::Bool(true),
            Value::Int(-3),
            Value::Float(1.0),
            Value::Float(1e-12),
            Value::Vec2([0.5, -1.0]),
            Value::Vec3([1.0, 2.0, 3.0]),
            Value::Vec4([0.0, 0.25, 0.5, 1.0]),
            Value::Text("a \"quoted\" line\nand more".into()),
        ];
        for v in values {
            let mut c = content(&[("/a", "test.const", &[], &[], false)]);
            if let Some(n) = c.nodes.values_mut().next() {
                n.params.insert("value".parse().unwrap(), v.clone());
            }
            let text = print(&c, &types).unwrap();
            assert_eq!(parse(&text, &types).unwrap(), c, "{v:?} in {text}");
            if let Value::Vec2(_) = v {
                assert!(text.contains("[0.5, -1.0]"), "vectors are written as lists: {text}");
            }
        }
    }

    #[test]
    fn handwritten_files_parse() {
        let types = test_types();
        let text = r#"
            // comments are fine
            (
                version: 1,
                nodes: {
                    "/a": (kind: "test.const", params: {"value": 2}),
                    "/s": (kind: "test.add", inputs: {"b": "/a:0", "a": "/s2:neg"}),
                    "/s2": (kind: "test.add"),
                    "/v": (kind: "test.const", params: {"value": [1, 2]}),
                },
            )
        "#;
        let c = parse(text, &types).unwrap();
        let s = &c.nodes[&"/s".parse::<NodePath>().unwrap()];
        assert_eq!(s.inputs[0], Some(PortRef { node: "/s2".parse().unwrap(), output: 1 }));
        assert_eq!(s.inputs[1], Some(PortRef { node: "/a".parse().unwrap(), output: 0 }));
        assert_eq!(c.nodes[&"/a".parse::<NodePath>().unwrap()].params["value"], Value::Int(2));
        assert_eq!(c.nodes[&"/v".parse::<NodePath>().unwrap()].params["value"], Value::Vec2([1.0, 2.0]));
        assert!(parse("(version: 1)", &types).unwrap().nodes.is_empty());
    }

    #[test]
    fn renamed_inputs_load_and_are_written_by_their_new_names() {
        let types = test_types();
        let text = r#"(version: 1, nodes: {
            "/a": (kind: "test.const"),
            "/m": (kind: "test.merge", inputs: {"a": "a", "b": "a", "in40": "a"}),
        })"#;
        let c = parse(text, &types).unwrap();
        let m = &c.nodes[&"/m".parse::<NodePath>().unwrap()];
        let a = Some(PortRef { node: "/a".parse().unwrap(), output: 0 });
        assert_eq!((&m.inputs[0], &m.inputs[1], &m.inputs[40]), (&a, &a, &a));
        let written = print(&c, &types).unwrap();
        assert!(written.contains(r#"inputs: {"in0": "a", "in1": "a", "in40": "a"}"#), "{written}");
    }

    #[test]
    fn bad_files_are_refused_with_a_reason() {
        let types = test_types();
        let cases = [
            ("(version: 3)", "version 3"),
            ("(version: 1, extra: 3)", "extra"),
            ("(version: 1, nodes: {\"a\": (kind: \"test.const\")})", "must start with"),
            ("(version: 1, nodes: {\"/a\": (kind: \"nope.x\")})", "unknown node kind"),
            ("(version: 1, nodes: {\"/a\": (kind: \"test.const\", colour: 1)})", "colour"),
            ("(version: 1, nodes: {\"/s\": (kind: \"test.add\", inputs: {\"c\": \"/s\"})})", "no such input"),
            (
                "(version: 1, nodes: {\"/s\": (kind: \"test.add\", inputs: {\"a\": \"/x\"})})",
                "not in the file",
            ),
            (
                "(version: 1, nodes: {\"/s\": (kind: \"test.add\", inputs: {\"a\": \"/s:wat\"})})",
                "no output `wat`",
            ),
            ("(version: 1, nodes: {\"/s\": (kind: \"test.add\", inputs: {\"a\": \"/s:7\"})})", "no output 7"),
            ("(version: 1, nodes: {\"/a\": (kind: \"test.const\", params: {\"value\": {}})})", ""),
            ("(version: 1", ""),
            ("", ""),
        ];
        for (text, want) in cases {
            let e = parse(text, &types).unwrap_err();
            assert!(e.0.contains(want), "{text}: {e}");
        }
    }

    /// A file with nodes that do not load, in the form the app writes.
    const BROKEN: &str = r#"(version: 1, nodes: {
        "/a": (kind: "test.const", params: {"value": 2.0}),
        "/b": (kind: "test.const", params: {"value": "two"}),
        "/g": (kind: "gone.group", params: {"size": [1, 2]}),
        "/g/in": (kind: "test.const", binds: {"value": "../a"}),
        "/s": (kind: "test.add", inputs: {"a": "x:left", "b": "b"}),
        "/t": (kind: "test.add", inputs: {"a": "x:3", "b": "x"}, binds: {"x": "x:ch"}),
        "/w": (kind: "test.add", inputs: {"c": "a"}),
        "/x": (kind: "gone.kind", params: {"rate": 2}, inputs: {"in": "a"}, binds: {"rate": "a:value"}, locked: true),
    })"#;

    fn line<'a>(text: &'a str, path: &str) -> &'a str {
        text.lines().find(|l| l.trim_start().starts_with(&format!("\"{path}\""))).unwrap_or("").trim()
    }

    #[test]
    fn nodes_that_do_not_load_are_kept_and_written_back_as_they_were() {
        let types = test_types();
        let c = load(BROKEN, &types).unwrap();
        let kind = |p: &str| c.nodes[&p.parse::<NodePath>().unwrap()].kind.as_str();
        for p in ["/b", "/g", "/w", "/x"] {
            assert_eq!(kind(p), broken::BROKEN_KIND, "{p}");
        }
        for (p, k) in [("/a", "test.const"), ("/g/in", "test.const"), ("/s", "test.add"), ("/t", "test.add")]
        {
            assert_eq!(kind(p), k, "{p}");
        }
        let error = |p: &str| {
            c.nodes[&p.parse::<NodePath>().unwrap()].params[broken::ERROR].as_str().unwrap_or("").to_string()
        };
        assert!(error("/b").contains("`value`"), "{}", error("/b"));
        assert!(error("/g").contains("unknown node kind `gone.group`"), "{}", error("/g"));
        assert!(error("/w").contains("no such input"), "{}", error("/w"));
        assert!(error("/x").contains("unknown node kind `gone.kind`"), "{}", error("/x"));
        // Wires from a placeholder are kept: a name gets a number of its own, a number stays.
        let s = &c.nodes[&"/s".parse::<NodePath>().unwrap()];
        assert_eq!(s.inputs[0], Some(PortRef { node: "/x".parse().unwrap(), output: broken::NAMED_OUTPUT }));
        assert_eq!(s.inputs[1], Some(PortRef { node: "/b".parse().unwrap(), output: 0 }));

        let text = print(&c, &types).unwrap();
        for (p, want) in [
            ("/b", r#""/b": (kind: "test.const", params: {"value": "two"}),"#),
            // Values in the canonical form, as for every node: vectors of floats.
            ("/g", r#""/g": (kind: "gone.group", params: {"size": [1.0, 2.0]}),"#),
            ("/s", r#""/s": (kind: "test.add", inputs: {"a": "x:left", "b": "b"}),"#),
            ("/t", r#""/t": (kind: "test.add", inputs: {"a": "x:3", "b": "x"}, binds: {"x": "x:ch"}),"#),
            ("/w", r#""/w": (kind: "test.add", inputs: {"c": "a"}),"#),
            (
                "/x",
                r#""/x": (kind: "gone.kind", params: {"rate": 2}, inputs: {"in": "a"}, binds: {"rate": "a:value"}, locked: true),"#,
            ),
        ] {
            assert_eq!(line(&text, p), want);
        }
        assert_eq!(load(&text, &types).unwrap(), c);
        assert_eq!(print(&load(&text, &types).unwrap(), &types).unwrap(), text);
        // All or nothing where a project is not opened.
        let e = parse(BROKEN, &types).unwrap_err();
        assert!(e.0.contains("/g: unknown node kind"), "{e}");
    }

    #[test]
    fn positions_are_written_plainly_and_read_in_either_form() {
        let types = test_types();
        let text = r#"(version: 1, nodes: {
            "/a": (kind: "test.const", pos: (10, -20.4)),
            "/b": (kind: "test.const", pos: [300.0, 0.0]),
            "/c": (kind: "test.const"),
            "/d": (kind: "test.const", pos: (NaN, 1.0)),
        })"#;
        let c = load(text, &types).unwrap();
        let pos = |c: &Content, p: &str| c.nodes[&p.parse::<NodePath>().unwrap()].pos;
        // Whole units, as the network keeps them; nonsense is dropped, the node stays.
        assert_eq!(pos(&c, "/a"), Some([10.0, -20.0]));
        assert_eq!(pos(&c, "/b"), Some([300.0, 0.0]));
        assert_eq!(pos(&c, "/c"), None);
        assert_eq!(pos(&c, "/d"), None);
        assert_eq!(c.nodes[&"/d".parse::<NodePath>().unwrap()].kind, "test.const");
        let printed = print(&c, &types).unwrap();
        assert!(printed.contains("version: 2,"), "{printed}");
        assert_eq!(line(&printed, "/a"), r#""/a": (kind: "test.const", pos: [10.0, -20.0]),"#);
        assert_eq!(line(&printed, "/c"), r#""/c": (kind: "test.const"),"#);
        assert_eq!(parse(&printed, &types).unwrap(), c);
        for bad in ["Some((1, 2))", "[1, 2, 3]", "\"here\""] {
            let text = format!(r#"(version: 2, nodes: {{"/a": (kind: "test.const", pos: {bad})}})"#);
            assert!(parse(&text, &types).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_placeholder_keeps_its_position_outside_its_entry() {
        let types = test_types();
        let text =
            r#"(version: 2, nodes: {"/x": (kind: "gone.kind", params: {"rate": 2}, pos: (40.0, 50.0))})"#;
        let c = load(text, &types).unwrap();
        let x = &c.nodes[&"/x".parse::<NodePath>().unwrap()];
        assert_eq!(x.kind, broken::BROKEN_KIND);
        assert_eq!(x.pos, Some([40.0, 50.0]));
        let entry = x.params[broken::ENTRY].as_str().unwrap_or_default();
        assert!(!entry.contains("pos"), "{entry}");
        let printed = print(&c, &types).unwrap();
        assert_eq!(
            line(&printed, "/x"),
            r#""/x": (kind: "gone.kind", params: {"rate": 2}, pos: [40.0, 50.0]),"#
        );
        // Moved, it is written where it went.
        let mut moved = c.clone();
        if let Some(n) = moved.nodes.get_mut(&"/x".parse::<NodePath>().unwrap()) {
            n.pos = Some([0.0, 0.0]);
        }
        let printed = print(&moved, &types).unwrap();
        assert_eq!(
            line(&printed, "/x"),
            r#""/x": (kind: "gone.kind", params: {"rate": 2}, pos: [0.0, 0.0]),"#
        );
        assert_eq!(load(&printed, &types).unwrap(), moved);
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_stack_overflow() {
        let types = test_types();
        let text = format!(
            "(version: 1, nodes: {{\"/a\": (kind: \"test.const\", params: {{\"value\": {}1{}}})}})",
            "[".repeat(100_000),
            "]".repeat(100_000)
        );
        assert!(parse(&text, &types).is_err());
    }
}
