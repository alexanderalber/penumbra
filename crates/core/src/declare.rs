//! Params declared in text, one line each. A shader file declares its params in comment lines
//! of its header, and a component declares its own in its `params` text. Both use one grammar:
//!
//! ```text
//! @param scale float = 4 1..32 -- cells per image height
//! @param tint color = [1, 0.5, 0.2, 1]
//! @param octaves int = 3 1..8
//! @param invert bool
//! @param mode choice(add|multiply|screen) = multiply
//! @group detail
//! @param warp float = 0 0..1
//! ```
//!
//! `@param <name> <kind> [= <default>] [<min>..<max>] [-- <doc>]`. Kinds: `float`, `int`, `bool`,
//! `vec2`, `vec3`, `vec4`, `color`, `palette`, `choice(a|b|c)`. The default is JSON (`1.5`,
//! `[1, 0, 0, 1]`, `true`) or, for a choice, a bare word, for a palette a name or hex colours
//! (`blood`, `31aaa9 f8e0a4`); without one it is zero, false, the first choice or no palette.
//! A range is a soft range (slider only). `@group <name>` puts the params declared after it in a
//! section of the panel that is folded away until opened; `@group` alone goes back to the main
//! rows.
//!
//! A component's text has one more kind of line, `@expose <name> = <node>.<param>` (or
//! `@expose <node>.<param>`, shown under the param's own name): one inner param shown on the
//! component, its value and spec kept inside. [`component`] reads such a text.

use crate::{Error, Name, ParamKind, ParamSpec, Value};

/// Declares a param.
pub const PARAM: &str = "@param";
/// Starts a group of params, or ends one.
pub const GROUP: &str = "@group";
/// Shows an inner param on a component.
pub const EXPOSE: &str = "@expose";

/// The `@word` of a declaration line and the text after it, when `line` is one. The line may
/// start with `//`, so header lines of a shader can be pasted; with `comment_only` it must (a
/// line of a shader file that is no comment is code).
pub fn split(line: &str, comment_only: bool) -> Option<(&str, &str)> {
    let rest = line.trim_start();
    let rest = match rest.strip_prefix("//") {
        Some(r) => r,
        None if comment_only => return None,
        None => rest,
    };
    let rest = rest.trim();
    if !rest.starts_with('@') {
        return None;
    }
    Some(rest.split_once(char::is_whitespace).unwrap_or((rest, "")))
}

/// The text after `@param` as a spec, without its group. `name_ok` may refuse a name (a shader
/// refuses its language's reserved words), before the default is read.
pub fn param(decl: &str, name_ok: impl Fn(&str) -> Result<(), Error>) -> Result<ParamSpec, Error> {
    let (decl, doc) = match decl.split_once("--") {
        Some((d, doc)) => (d, doc.trim()),
        None => (decl, ""),
    };
    let decl = decl.trim();
    let (head, default) = match decl.split_once('=') {
        Some((h, d)) => (h.trim(), Some(d.trim())),
        None => (decl, None),
    };
    // A range is the last token of whatever follows the kind (or the default).
    let (head, default, range) = {
        let tail = default.unwrap_or(head);
        let (before, last) = tail.rsplit_once(char::is_whitespace).unwrap_or(("", tail));
        match parse_range(last) {
            Some(r) if default.is_some() => (head, Some(before.trim()), Some(r)),
            Some(r) => (before.trim(), None, Some(r)),
            None => (head, default, None),
        }
    };
    let mut words = head.split_whitespace();
    let (Some(name), Some(kind), None) = (words.next(), words.next(), words.next()) else {
        return Err(Error::new("expected `@param <name> <kind> [= default] [min..max] [-- doc]`"));
    };
    name_ok(name)?;
    let default = default.filter(|d| !d.is_empty());
    let mut spec = match kind {
        "float" => ParamSpec::float(name, 0.0),
        "int" => ParamSpec::int(name, 0),
        "bool" => ParamSpec::bool(name, false),
        "vec2" => ParamSpec::vec2(name, [0.0; 2]),
        "vec3" => ParamSpec::vec3(name, [0.0; 3]),
        "vec4" => ParamSpec::vec4(name, [0.0; 4]),
        "color" => ParamSpec::color(name, [0.0, 0.0, 0.0, 1.0]),
        "palette" => ParamSpec::palette(name, ""),
        k => match k.strip_prefix("choice(").and_then(|c| c.strip_suffix(')')) {
            Some(list) => {
                let choices: Vec<&str> = list.split('|').map(str::trim).collect();
                ParamSpec::choice(name, &choices)
            }
            None => {
                return Err(Error::new(format!(
                    "unknown kind `{k}`; kinds: float, int, bool, vec2, vec3, vec4, color, palette, choice(a|b)"
                )));
            }
        },
    };
    if let Some(d) = default {
        let value = match serde_json::from_str::<Value>(d) {
            // A palette's default is always text: `123456` is a colour, not a place in the list.
            _ if spec.kind == ParamKind::Palette => Value::Text(d.to_string()),
            Ok(v) => v,
            Err(_) => Value::Text(d.to_string()),
        };
        spec.default = spec.check(&value)?;
    }
    if let Some((min, max)) = range {
        spec = spec.range(min, max);
    }
    spec = spec.doc(doc);
    spec.validate_spec()?;
    Ok(spec)
}

fn parse_range(token: &str) -> Option<(f64, f64)> {
    let (a, b) = token.split_once("..")?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// The word among `headers` (each with its `@`) that `word` seems to mean, when it is none of
/// them but close to one: in other letter case, with a letter or two more, fewer or different,
/// or with letters after it (`@parm`, `@Param`, `@params`).
pub fn misspelt(word: &str, headers: &[&'static str]) -> Option<&'static str> {
    let lower = word.to_ascii_lowercase();
    let letters = lower.strip_prefix('@')?;
    if letters.len() < 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    headers.iter().copied().find(|h| {
        let name = h.get(1..).unwrap_or("");
        // Three letters are too few for two edits: `@arm` is no `@param`.
        let edits = if letters.len() > 3 { 2 } else { 1 };
        letters.starts_with(name) || distance(letters, name) <= edits
    })
}

/// Edits (insert, delete, replace one letter) from `a` to `b`, for ASCII words.
fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(usize::MAX);
            let left = row.get(j).copied().unwrap_or(usize::MAX);
            let here =
                (diagonal + usize::from(ca != cb)).min(above.saturating_add(1)).min(left.saturating_add(1));
            diagonal = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = here;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

/// The words a component's text takes.
const COMPONENT_WORDS: [&str; 3] = [PARAM, GROUP, EXPOSE];

/// An `@expose` line: the name shown on the component and the inner param it stands for, as
/// written (`lfo1.rate`, `inner/lfo.rate`), relative to the component.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposeLine {
    pub name: Name,
    pub target: String,
}

/// One declaration of a component's text.
#[derive(Clone, Debug, PartialEq)]
pub enum Declaration {
    /// A param the component owns: it holds the value.
    Param(Box<ParamSpec>),
    /// An inner param shown on the component.
    Expose(ExposeLine),
}

impl Declaration {
    pub fn name(&self) -> &str {
        match self {
            Self::Param(spec) => &spec.name,
            Self::Expose(line) => line.name.as_str(),
        }
    }
}

/// What a component's text declares, in its order, and the lines that declare nothing, each as
/// `line <n>: <why>`. A bad line costs only itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declared {
    pub lines: Vec<Declaration>,
    pub errors: Vec<String>,
}

impl Declared {
    /// The params the component owns.
    pub fn params(&self) -> impl Iterator<Item = &ParamSpec> + '_ {
        self.lines.iter().filter_map(|d| match d {
            Declaration::Param(spec) => Some(spec.as_ref()),
            Declaration::Expose(_) => None,
        })
    }

    /// The inner params shown on the component.
    pub fn exposed(&self) -> impl Iterator<Item = &ExposeLine> + '_ {
        self.lines.iter().filter_map(|d| match d {
            Declaration::Expose(line) => Some(line),
            Declaration::Param(_) => None,
        })
    }

    /// The param the component owns under `name`.
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params().find(|s| s.name == name)
    }
}

/// Reads a component's text. Blank lines and lines starting with `#` or with `//` and no `@`
/// are comments; in an `@expose` line, `#` starts a comment. `taken` says why a name may not be
/// declared (the component's own params, reserved words), or `None` when it may.
pub fn component(text: &str, taken: impl Fn(&str) -> Option<String>) -> Declared {
    let mut out = Declared::default();
    let mut group = String::new();
    // The line each declared name came from, to name it in a clash.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((word, args)) = split(raw, false) else {
            if trimmed.starts_with("//") {
                continue;
            }
            out.errors.push(format!(
                "line {n}: `{}` declares nothing; a line starts with @param, @group or @expose, or with # for a comment",
                short(trimmed)
            ));
            continue;
        };
        let declaration = match word {
            PARAM => param(args, |_| Ok(())).map(|spec| spec.group(&group)).and_then(|spec| {
                spec.validate_spec()?;
                Ok(Declaration::Param(Box::new(spec)))
            }),
            GROUP => {
                let name = args.trim();
                match ParamSpec::bool("x", false).group(name).validate_spec() {
                    Ok(()) => group = name.to_string(),
                    Err(e) => {
                        let why = e.0.strip_prefix("param `x`: ").unwrap_or(&e.0).to_string();
                        out.errors.push(format!("line {n}: {why}"));
                    }
                }
                continue;
            }
            EXPOSE => expose(args).map(Declaration::Expose),
            other => Err(Error::new(match misspelt(other, &COMPONENT_WORDS) {
                Some(meant) => format!("`{other}` looks like a misspelt `{meant}`"),
                None => {
                    format!("`{other}` is no declaration here; a component takes @param, @group and @expose")
                }
            })),
        };
        let declaration = match declaration {
            Ok(d) => d,
            Err(e) => {
                out.errors.push(format!("line {n}: {e}"));
                continue;
            }
        };
        let name = declaration.name().to_string();
        if let Some(why) = taken(&name) {
            out.errors.push(format!("line {n}: {why}; give this one another name"));
            continue;
        }
        if let Some((_, first)) = seen.iter().find(|(s, _)| *s == name) {
            out.errors
                .push(format!("line {n}: `{name}` is declared on line {first} already; rename one of them"));
            continue;
        }
        seen.push((name, n));
        out.lines.push(declaration);
    }
    out
}

/// The text after `@expose`.
fn expose(args: &str) -> Result<ExposeLine, Error> {
    let line = args.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return Err(Error::new("expected `@expose <name> = <node>.<param>` or `@expose <node>.<param>`"));
    }
    let (name, target) = match line.split_once('=') {
        Some((n, t)) => (Some(n.trim()), t.trim()),
        None => (None, line),
    };
    let param = target.rsplit_once('.').map(|(_, p)| p).unwrap_or("");
    let name = Name::new(name.unwrap_or(param))?;
    Ok(ExposeLine { name, target: target.to_string() })
}

/// At most 40 characters of a line in an error message.
fn short(text: &str) -> String {
    text.chars().take(40).collect()
}

/// The text of an old `expose` list as `@expose` lines: `name = node.param` becomes
/// `@expose name = node.param`; blank lines and `#` comments stay as they are.
pub fn expose_lines(old: &str) -> String {
    old.split('\n')
        .map(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') { t.to_string() } else { format!("{EXPOSE} {t}") }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_component_text_declares_params_groups_and_exposed_ones() {
        let d = component(
            "// @param preset choice(coral|maze) = maze -- the pattern\n\
             @param feed float = 0.05 0..0.1\n\
             \n\
             # a comment\n\
             // a pasted comment\n\
             @group look\n\
             @param glow float = 1\n\
             @expose speed = lfo1.rate  # main\n\
             @expose inner/n.seed",
            none,
        );
        assert!(d.errors.is_empty(), "{:?}", d.errors);
        let names: Vec<&str> = d.lines.iter().map(Declaration::name).collect();
        assert_eq!(names, ["preset", "feed", "glow", "speed", "seed"]);
        let preset = d.param("preset").unwrap();
        assert_eq!(preset.kind, ParamKind::Choice(vec!["coral".into(), "maze".into()]));
        assert_eq!(preset.default, Value::Text("maze".into()));
        assert_eq!(preset.doc, "the pattern");
        assert_eq!(d.param("glow").unwrap().group, "look");
        assert_eq!(d.param("feed").unwrap().group, "");
        let exposed: Vec<(&str, &str)> = d.exposed().map(|e| (e.name.as_str(), e.target.as_str())).collect();
        assert_eq!(exposed, [("speed", "lfo1.rate"), ("seed", "inner/n.seed")]);
    }

    #[test]
    fn a_bad_line_costs_only_itself_and_says_why() {
        let taken = |n: &str| (n == "shelf").then(|| "`shelf` is a param of every component".to_string());
        let d = component(
            "@param a float\n\
             @param a int\n\
             @expose a = x.value\n\
             @param shelf bool\n\
             @param b quaternion\n\
             @parm c float\n\
             @density 2\n\
             just words\n\
             @param mode choice(x|y) = z\n\
             @group a group name far longer than it may ever be\n\
             @expose\n\
             @expose 9 = x.value\n\
             @param last bool",
            taken,
        );
        let names: Vec<&str> = d.lines.iter().map(Declaration::name).collect();
        assert_eq!(names, ["a", "last"]);
        let expect = [
            ("line 2:", "declared on line 1 already"),
            ("line 3:", "declared on line 1 already"),
            ("line 4:", "`shelf` is a param of every component; give this one another name"),
            ("line 5:", "unknown kind `quaternion`"),
            ("line 6:", "misspelt `@param`"),
            ("line 7:", "`@density` is no declaration here"),
            ("line 8:", "declares nothing"),
            ("line 9:", "must be one of"),
            ("line 10:", "at most 32 bytes"),
            ("line 11:", "expected `@expose"),
            ("line 12:", "invalid name"),
        ];
        assert_eq!(d.errors.len(), expect.len(), "{:?}", d.errors);
        for (e, (line, needle)) in d.errors.iter().zip(expect) {
            assert!(e.starts_with(line) && e.contains(needle), "{e}: wants {line} {needle}");
        }
    }

    #[test]
    fn old_expose_lists_become_expose_lines() {
        assert_eq!(
            expose_lines("level = k.value0  # main\n\n  # note\nn.seed"),
            "@expose level = k.value0  # main\n\n# note\n@expose n.seed"
        );
        let d = component(&expose_lines("level = k.value0  # main\nn.seed"), none);
        let exposed: Vec<&str> = d.exposed().map(|e| e.name.as_str()).collect();
        assert_eq!(exposed, ["level", "seed"]);
    }
}
