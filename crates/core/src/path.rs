//! Names and paths.
//!
//! A node path is the chain of names from the root of the project to a node, written
//! `/comp1/noise1`; `/` alone is the root. A parameter path appends the parameter after a dot:
//! `/comp1/noise1.scale`. Names cannot contain `/` or `.`, so both forms parse without
//! ambiguity. Paths already allow nesting, so components (M2b) need no new syntax.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

/// A node or parameter name: 1 to [`Name::MAX_LEN`] ASCII letters, digits and underscores, not
/// starting with a digit. Case-sensitive.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);

impl Name {
    pub const MAX_LEN: usize = 64;

    pub fn new(text: &str) -> Result<Self, Error> {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return Err(Error::new("a name cannot be empty"));
        };
        if text.len() > Self::MAX_LEN {
            return Err(Error::new(format!(
                "name `{}...` is longer than {} characters",
                short(text),
                Self::MAX_LEN
            )));
        }
        let ok_first = first.is_ascii_alphabetic() || first == '_';
        if !ok_first || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::new(format!(
                "invalid name `{}`: use letters, digits and `_`, not starting with a digit",
                short(text)
            )));
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// At most 40 characters of hostile input in an error message.
fn short(text: &str) -> String {
    text.chars().take(40).collect()
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Name {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::new(s)
    }
}

impl TryFrom<String> for Name {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        Self::new(&s)
    }
}

impl From<Name> for String {
    fn from(name: Name) -> String {
        name.0
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Path from the project root to a node. The root itself is the empty path, written `/`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodePath(Vec<Name>);

impl NodePath {
    /// Deepest nesting accepted.
    pub const MAX_DEPTH: usize = 32;

    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn from_names(names: Vec<Name>) -> Result<Self, Error> {
        if names.len() > Self::MAX_DEPTH {
            return Err(Error::new(format!("path is deeper than {} levels", Self::MAX_DEPTH)));
        }
        Ok(Self(names))
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let Some(rest) = text.strip_prefix('/') else {
            return Err(Error::new(format!("node path `{}` must start with `/`", short(text))));
        };
        if rest.is_empty() {
            return Ok(Self::root());
        }
        let names = rest
            .split('/')
            .map(Name::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.context(format!("node path `{}`", short(text))))?;
        Self::from_names(names)
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> &[Name] {
        &self.0
    }

    /// The last name; `None` for the root.
    pub fn name(&self) -> Option<&Name> {
        self.0.last()
    }

    /// The path one level up; `None` for the root.
    pub fn parent(&self) -> Option<NodePath> {
        let (_, init) = self.0.split_last()?;
        Some(Self(init.to_vec()))
    }

    pub fn child(&self, name: Name) -> Result<NodePath, Error> {
        let mut names = self.0.clone();
        names.push(name);
        Self::from_names(names)
    }

    /// `/a/b` to `/a/b.param`.
    pub fn param(&self, param: Name) -> Result<ParamPath, Error> {
        ParamPath::new(self.clone(), param)
    }

    /// The node `text` names, read inside the level `self` (the root or a component): the one
    /// rule for wire sources, bindings and node references in project and component files and
    /// in the UI. `/a/b` is absolute; `x` is the node `x` on this level, `sub/x` one below it,
    /// `../x` one level up (`..` may repeat, only at the start).
    pub fn resolve_ref(&self, text: &str) -> Result<NodePath, Error> {
        let text = text.trim();
        if text.starts_with('/') {
            return Self::parse(text);
        }
        if text.is_empty() {
            return Err(Error::new("an empty node reference"));
        }
        let mut names = self.0.clone();
        let mut segments = text.split('/').peekable();
        while segments.peek() == Some(&"..") {
            segments.next();
            if names.pop().is_none() {
                return Err(Error::new(format!("`{}` leads above the top level", short(text))));
            }
        }
        let mut any = false;
        for s in segments {
            names.push(Name::new(s).map_err(|e| e.context(format!("node reference `{}`", short(text))))?);
            any = true;
        }
        if !any {
            return Err(Error::new(format!("`{}` names no node", short(text))));
        }
        Self::from_names(names)
    }

    /// How a reference written inside the level `self` names `target`, the inverse of
    /// [`Self::resolve_ref`]: the shorter of the relative and the absolute form, counting each
    /// name and each `..` as one step, the relative one on a tie. So a node on the same level is
    /// its name, one below `sub/x`, one beside the enclosing component `../x`, and a node at the
    /// top level seen from inside a component `/x`.
    pub fn write_ref(&self, target: &NodePath) -> String {
        let (level, to) = (self.names(), target.names());
        if to.is_empty() {
            return "/".to_string();
        }
        let mut common = level.iter().zip(to).take_while(|(a, b)| a == b).count();
        // A relative path names at least the target's own name.
        common = common.min(to.len().saturating_sub(1));
        let ups = level.len().saturating_sub(common);
        let downs = to.len().saturating_sub(common);
        if ups + downs > to.len() {
            return target.to_string();
        }
        let mut parts: Vec<&str> = vec![".."; ups];
        parts.extend(to.iter().skip(common).map(Name::as_str));
        parts.join("/")
    }
}

impl fmt::Display for NodePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("/");
        }
        for name in &self.0 {
            write!(f, "/{name}")?;
        }
        Ok(())
    }
}

impl FromStr for NodePath {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

impl TryFrom<String> for NodePath {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        Self::parse(&s)
    }
}

impl From<NodePath> for String {
    fn from(path: NodePath) -> String {
        path.to_string()
    }
}

/// A parameter of a node: `/comp1/noise1.scale`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ParamPath {
    node: NodePath,
    param: Name,
}

impl ParamPath {
    pub fn new(node: NodePath, param: Name) -> Result<Self, Error> {
        if node.is_root() {
            return Err(Error::new(format!("parameter `{param}` needs a node; the root has no parameters")));
        }
        Ok(Self { node, param })
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let Some((node, param)) = text.rsplit_once('.') else {
            return Err(Error::new(format!("parameter path `{}` needs a `.param` at the end", short(text))));
        };
        let node = NodePath::parse(node)?;
        let param = Name::new(param).map_err(|e| e.context(format!("parameter path `{}`", short(text))))?;
        Self::new(node, param)
    }

    pub fn node(&self) -> &NodePath {
        &self.node
    }

    pub fn param(&self) -> &Name {
        &self.param
    }
}

impl fmt::Display for ParamPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.param)
    }
}

impl FromStr for ParamPath {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

impl TryFrom<String> for ParamPath {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        Self::parse(&s)
    }
}

impl From<ParamPath> for String {
    fn from(path: ParamPath) -> String {
        path.to_string()
    }
}

/// A parameter or one component of it, as a channel binding names it: `rotate`, `translate.0`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ParamPart {
    pub param: Name,
    pub component: Option<u8>,
}

impl ParamPart {
    pub fn new(param: Name, component: Option<u8>) -> Self {
        Self { param, component }
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let (param, component) = match text.split_once('.') {
            Some((p, c)) => {
                let c = c
                    .parse::<u8>()
                    .ok()
                    .filter(|c| *c < 4)
                    .ok_or_else(|| Error::new(format!("`{}`: a component is 0 to 3", short(text))))?;
                (p, Some(c))
            }
            None => (text, None),
        };
        Ok(Self { param: Name::new(param)?, component })
    }
}

impl fmt::Display for ParamPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.component {
            Some(c) => write!(f, "{}.{c}", self.param),
            None => write!(f, "{}", self.param),
        }
    }
}

impl TryFrom<String> for ParamPart {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        Self::parse(&s)
    }
}

impl From<ParamPart> for String {
    fn from(part: ParamPart) -> String {
        part.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "_x", "noise1", "Big_Name_2"] {
            assert!(Name::new(ok).is_ok(), "{ok}");
        }
        let long = "x".repeat(65);
        for bad in ["", "1a", "a-b", "a.b", "a/b", "\u{e4}", " a", long.as_str()] {
            assert!(Name::new(bad).is_err(), "{bad}");
        }
        assert!(Name::new(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn node_paths_round_trip() {
        for text in ["/", "/a", "/comp1/noise1"] {
            assert_eq!(NodePath::parse(text).unwrap().to_string(), text);
        }
        for bad in ["", "a", "a/b", "//", "/a/", "/a//b", "/a.b", "/1"] {
            assert!(NodePath::parse(bad).is_err(), "{bad}");
        }
        assert!(NodePath::parse(&"/a".repeat(33)).is_err());
        assert!(NodePath::parse(&"/a".repeat(32)).is_ok());
    }

    #[test]
    fn node_path_parts() {
        let p = NodePath::parse("/comp1/noise1").unwrap();
        assert_eq!(p.name().unwrap().as_str(), "noise1");
        assert_eq!(p.parent().unwrap().to_string(), "/comp1");
        assert_eq!(p.parent().unwrap().parent().unwrap(), NodePath::root());
        assert!(NodePath::root().parent().is_none());
        assert_eq!(NodePath::root().child(Name::new("x").unwrap()).unwrap().to_string(), "/x");
    }

    #[test]
    fn references_relative_to_a_level() {
        let p = |s: &str| NodePath::parse(s).unwrap();
        // (level, target, written)
        let cases = [
            ("/", "/a", "a"),
            ("/", "/a/b", "a/b"),
            ("/g", "/g/x", "x"),
            ("/g", "/g/sub/x", "sub/x"),
            ("/g", "/y", "/y"),
            ("/g/h", "/g/y", "../y"),
            ("/g/h", "/g/h", "../h"),
            ("/g/h", "/g", "/g"),
            ("/g/h/i", "/g/y", "/g/y"),
            ("/g/h/i", "/g/h/y", "../y"),
            ("/a/b", "/c/d/e", "/c/d/e"),
            ("/a/b/c", "/a/b/d/e/f", "../d/e/f"),
            ("/a", "/", "/"),
        ];
        for (level, target, want) in cases {
            let got = p(level).write_ref(&p(target));
            assert_eq!(got, want, "{target} from {level}");
            if target != "/" {
                assert_eq!(p(level).resolve_ref(&got).unwrap(), p(target), "{got} from {level}");
            }
        }
        // Absolute paths always read, and `..` may stand anywhere a shorter form would.
        assert_eq!(p("/g/h").resolve_ref("/x").unwrap(), p("/x"));
        assert_eq!(p("/g/h").resolve_ref("../../x").unwrap(), p("/x"));
        assert_eq!(p("/g/h").resolve_ref(" x ").unwrap(), p("/g/h/x"));
        for bad in ["", "..", "../..", "../../../x", "a/../b", "./x", "a//b", "1x", "a/"] {
            assert!(p("/g/h").resolve_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn param_paths() {
        let p = ParamPath::parse("/comp1/noise1.scale").unwrap();
        assert_eq!(p.node().to_string(), "/comp1/noise1");
        assert_eq!(p.param().as_str(), "scale");
        assert_eq!(p.to_string(), "/comp1/noise1.scale");
        for bad in ["/a", "/.x", ".x", "/a.", "/a.b.c", "a.b", "/a.1"] {
            assert!(ParamPath::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn param_parts() {
        for text in ["rotate", "translate.0", "color.3"] {
            assert_eq!(ParamPart::parse(text).unwrap().to_string(), text);
        }
        for bad in ["", "a.4", "a.x", "a.0.1", "1a", "a."] {
            assert!(ParamPart::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn serde_as_strings() {
        let p: ParamPath = serde_json::from_str("\"/a/b.c\"").unwrap();
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"/a/b.c\"");
        assert!(serde_json::from_str::<NodePath>("\"no-slash\"").is_err());
        assert!(serde_json::from_str::<Name>("\"9\"").is_err());
    }
}
