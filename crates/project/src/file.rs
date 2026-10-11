//! The project file, `project.ron`.
//!
//! ```ron
//! // penumbra project
//! (
//!     version: 1,
//!     settings: {"persist_state": true},
//!     nodes: {
//!         "/lfo1": (kind: "chan.lfo", params: {"rate": 0.5}),
//!         "/mix": (kind: "chan.math", inputs: {"a": "lfo1", "b": "split:right"}, locked: true),
//!         "/scene": (kind: "comp.base"),
//!         "/scene/move": (kind: "tex.transform", binds: {"rotate": "/lfo1", "translate.0": "pos:x"}),
//!     },
//! )
//! ```
//!
//! Nodes are keyed by path, one per line, sorted. `params` holds only values that differ from
//! the declared default (the app writes it that way; a file may list defaults too). `inputs`
//! maps an input name to the node feeding it: `"path"` for its first output, `"path:name"`
//! (or `"path:1"`) for another. `binds` maps a parameter (or one component, `translate.0`) to
//! the channel driving it: `"path"` for the first channel of the node's first `chan` output,
//! `"path:name"` for the channel called `name` (after the colon comes a channel here, not an
//! output). Node keys are absolute; the paths in `inputs` and `binds` are read on the level of
//! the node they belong to, by the rule node references use ([`NodePath::resolve_ref`]):
//! absolute (`/a/b`), a name beside the node, `sub/x` below, `../x` up. The app writes the
//! shorter form ([`NodePath::write_ref`]). `settings` holds the project settings that differ
//! from their default ([`penumbra_graph::SETTINGS`]). Missing `settings`, `params`, `inputs`, `binds` and `locked` mean empty and `false`. Unknown fields are refused, so a typo is an error rather than silently ignored.

use std::collections::BTreeMap;

use penumbra_core::{Error, Name, NodePath, ParamPart, Value};
use penumbra_graph::{NodeDesc, NodeTypes, check_setting};
use serde::{Deserialize, Serialize};

use crate::content::{BindRef, Content, NodeContent, PortRef};

/// File name inside the project directory.
pub const FILE_NAME: &str = "project.ron";
/// Newest format this build reads and the one it writes.
pub const FORMAT_VERSION: u32 = 1;
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

#[derive(Serialize, Deserialize)]
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
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn desc<'t>(types: &'t NodeTypes, path: &NodePath, kind: &str) -> Result<&'t NodeDesc, Error> {
    types.desc(kind).ok_or_else(|| {
        Error::new(format!(
            "{path}: unknown node kind `{kind}`; known: {}",
            types.kinds().collect::<Vec<_>>().join(", ")
        ))
    })
}

/// Reads the text of a project file. Checks the syntax, the version, the node kinds and the
/// connections; parameter values are checked when the content is applied to a network.
pub fn parse(text: &str, types: &NodeTypes) -> Result<Content, Error> {
    parse_as(text, types, FILE_NAME, false)
}

/// [`parse`] for a text called `what` in errors. With `outside`, inputs and bindings may name
/// nodes that are not in the text (copied nodes: [`crate::clipboard`]); an input from such a
/// node gives its output by index, since its kind is not known here.
pub(crate) fn parse_as(text: &str, types: &NodeTypes, what: &str, outside: bool) -> Result<Content, Error> {
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
    let mut nodes = BTreeMap::new();
    for (path, node) in &repr.nodes {
        let d = desc(types, path, &node.kind)?;
        let level = path.parent().unwrap_or_else(NodePath::root);
        let mut inputs = vec![None; d.inputs.len()];
        for (name, source) in &node.inputs {
            let ctx = || format!("{path} input `{name}`");
            let index = d.inputs.iter().position(|i| i.name == name.as_str()).ok_or_else(|| {
                let known: Vec<&str> = d.inputs.iter().map(|i| i.name.as_str()).collect();
                Error::new(format!("{}: `{}` has no such input; inputs: {}", ctx(), d.kind, known.join(", ")))
            })?;
            let port =
                parse_source(&level, source, &repr.nodes, types, outside).map_err(|e| e.context(ctx()))?;
            if let Some(slot) = inputs.get_mut(index) {
                *slot = Some(port);
            }
        }
        let mut binds = BTreeMap::new();
        for (part, source) in &node.binds {
            let src = parse_bind(&level, source, &repr.nodes, outside)
                .map_err(|e| e.context(format!("{path} bind `{part}`")))?;
            binds.insert(part.clone(), src);
        }
        nodes.insert(
            path.clone(),
            NodeContent {
                kind: node.kind.clone(),
                params: node.params.clone(),
                inputs,
                binds,
                locked: node.locked,
            },
        );
    }
    Ok(Content { nodes, settings })
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

/// `"path"` or `"path:output"`, where output is a name or an index, read on `level`.
fn parse_source(
    level: &NodePath,
    text: &str,
    nodes: &BTreeMap<NodePath, NodeRepr>,
    types: &NodeTypes,
    outside: bool,
) -> Result<PortRef, Error> {
    let (path, output) = match text.split_once(':') {
        Some((p, o)) => (p, Some(o)),
        None => (text, None),
    };
    let path = level.resolve_ref(path)?;
    if outside && !nodes.contains_key(&path) {
        let output = match output {
            None => 0,
            Some(o) => o.parse::<u32>().map_err(|_| {
                Error::new(format!("{path} is not in the text, so its output is given by number, not `{o}`"))
            })?,
        };
        return Ok(PortRef { node: path, output });
    }
    let source = nodes.get(&path).ok_or_else(|| Error::new(format!("{path} is not in the file")))?;
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
    if output as usize >= d.outputs.len() {
        return Err(Error::new(format!("{path} has no output {output}")));
    }
    Ok(PortRef { node: path, output })
}

/// The text of a project file for `content`, in canonical form: sorted by path, one node per
/// line, outputs other than the first named.
pub fn print(content: &Content, types: &NodeTypes) -> Result<String, Error> {
    let mut nodes = BTreeMap::new();
    for (path, node) in &content.nodes {
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
                // A source outside the content (copied nodes) by number: its kind is not known.
                o => match content.nodes.get(&src.node) {
                    Some(n) => match desc(types, &src.node, &n.kind)?.outputs.get(o as usize) {
                        Some(out) => format!("{from}:{}", out.name),
                        None => format!("{from}:{o}"),
                    },
                    None => format!("{from}:{o}"),
                },
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
    fn bad_files_are_refused_with_a_reason() {
        let types = test_types();
        let cases = [
            ("(version: 2)", "version 2"),
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
