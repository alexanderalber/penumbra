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
//!
//! And `@reset <name> <name> ...`: a change of any of these `@param` values starts the stateful
//! nodes inside over, so a new preset starts from its own seed.
//!
//! And `@preset <choice>=<entry> <name>=<value> ... [-- doc]`: the values that choosing `entry`
//! of the `@param` choice `choice` writes into the component's params ([`Presets`]). The entry
//! `*` is the base: choosing any entry writes the base values, then the entry's own. Only names
//! on a `*` line are ever written by a preset.

use std::collections::BTreeMap;

use crate::{Error, Name, ParamKind, ParamSpec, Value};

/// Declares a param.
pub const PARAM: &str = "@param";
/// Starts a group of params, or ends one.
pub const GROUP: &str = "@group";
/// Shows an inner param on a component.
pub const EXPOSE: &str = "@expose";
/// Names params of a component whose change starts its stateful nodes over.
pub const RESET: &str = "@reset";
/// Gives the values an entry of a choice writes into a component's params.
pub const PRESET: &str = "@preset";
/// The entry of a preset line that every entry of its choice writes first.
pub const BASE: &str = "*";

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
const COMPONENT_WORDS: [&str; 5] = [PARAM, GROUP, EXPOSE, RESET, PRESET];

/// An `@expose` line: the name shown on the component and the inner param it stands for, as
/// written (`lfo1.rate`, `inner/lfo.rate`), relative to the component.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposeLine {
    pub name: Name,
    pub target: String,
    /// The `@group` it sits in, empty for the main rows.
    pub group: String,
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
    /// The params whose change starts the component over (`@reset` lines), each once, in the
    /// order they are first named. Only names of `@param` lines.
    pub resets: Vec<Name>,
    /// The choices with `@preset` lines, in the order of their first line.
    pub presets: Vec<Presets>,
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

    /// The presets of choice `choice`, when it has `@preset` lines.
    pub fn presets_of(&self, choice: &str) -> Option<&Presets> {
        self.presets.iter().find(|p| p.choice.as_str() == choice)
    }

    /// The presets that write param `name` (it is on their `*` line).
    pub fn owner(&self, name: &str) -> Option<&Presets> {
        self.presets.iter().find(|p| p.owns(name))
    }
}

/// The `@preset` lines of one choice: what choosing each of its entries writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Presets {
    /// The `@param` choice whose entries these are.
    pub choice: Name,
    /// The values every entry writes first (the `*` lines), in their order. Their names are the
    /// params these presets own: no other name is ever written by them.
    pub base: Vec<(Name, Value)>,
    /// The entries with lines of their own, in the order of their first line.
    pub entries: Vec<PresetEntry>,
}

/// What one entry of a choice writes beyond the base.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetEntry {
    pub name: String,
    /// Values for names of the base, in line order.
    pub values: Vec<(Name, Value)>,
    /// The docs after `--` of its lines, joined by `; `.
    pub doc: String,
}

impl Presets {
    /// Whether choosing an entry writes param `name`.
    pub fn owns(&self, name: &str) -> bool {
        self.base.iter().any(|(n, _)| n.as_str() == name)
    }

    /// The lines of entry `entry`, when it has any.
    pub fn entry(&self, entry: &str) -> Option<&PresetEntry> {
        self.entries.iter().find(|e| e.name == entry)
    }

    /// What choosing `entry` writes, in the order of the base: the entry's own value of each
    /// owned param, else the base value.
    pub fn values(&self, entry: &str) -> Vec<(Name, Value)> {
        self.base
            .iter()
            .map(|(n, base)| (n.clone(), self.value_in(entry, n.as_str()).unwrap_or(base).clone()))
            .collect()
    }

    /// The value choosing `entry` writes into `name`; `None` when no preset writes it.
    pub fn value(&self, entry: &str, name: &str) -> Option<&Value> {
        let base = self.base.iter().find(|(n, _)| n.as_str() == name).map(|(_, v)| v)?;
        Some(self.value_in(entry, name).unwrap_or(base))
    }

    fn value_in(&self, entry: &str, name: &str) -> Option<&Value> {
        self.entry(entry)?.values.iter().find(|(n, _)| n.as_str() == name).map(|(_, v)| v)
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
    // The `@reset` lines, checked once every `@param` line is known: a name may come first.
    let mut resets: Vec<(usize, Vec<Name>)> = Vec::new();
    // The `@preset` lines, checked once every declaration is known, for the same reason.
    let mut presets: Vec<PresetLine> = Vec::new();
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
                "line {n}: `{}` declares nothing; a line starts with @param, @group, @expose, @reset or @preset, or with # for a comment",
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
            EXPOSE => {
                expose(args).map(|line| Declaration::Expose(ExposeLine { group: group.clone(), ..line }))
            }
            RESET => {
                match reset_names(args) {
                    Ok(names) => resets.push((n, names)),
                    Err(e) => out.errors.push(format!("line {n}: {e}")),
                }
                continue;
            }
            PRESET => {
                match preset_line(n, args) {
                    Ok(line) => presets.push(line),
                    Err(e) => out.errors.push(format!("line {n}: {e}")),
                }
                continue;
            }
            other => Err(Error::new(match misspelt(other, &COMPONENT_WORDS) {
                Some(meant) => format!("`{other}` looks like a misspelt `{meant}`"),
                None => {
                    format!(
                        "`{other}` is no declaration here; a component takes @param, @group, @expose, @reset and @preset"
                    )
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
    for (n, names) in resets {
        let mut bad: Vec<String> = Vec::new();
        for name in names {
            match out.lines.iter().find(|d| d.name() == name.as_str()) {
                Some(Declaration::Param(_)) => {
                    if !out.resets.contains(&name) {
                        out.resets.push(name);
                    }
                }
                Some(Declaration::Expose(_)) => bad.push(format!(
                    "`{name}` is shown from inside by @expose, and only a value the component holds can start it over; make it an @param line that the inner param follows (Put on component)"
                )),
                None => bad.push(format!("`{name}` is declared by no @param line")),
            }
        }
        if !bad.is_empty() {
            insert_error(&mut out.errors, n, bad.join("; "));
        }
    }
    read_presets(&mut out, presets);
    out
}

/// Adds `why` for line `n` to `errors`, in line order among the others.
fn insert_error(errors: &mut Vec<String>, n: usize, why: String) {
    let at = errors.iter().position(|e| error_line(e).is_some_and(|l| l > n)).unwrap_or(errors.len());
    errors.insert(at, format!("line {n}: {why}"));
}

/// An `@preset` line as written, before its names are checked against the declarations.
struct PresetLine {
    n: usize,
    choice: String,
    entry: String,
    values: Vec<(String, String)>,
    doc: String,
}

/// The text after `@preset`: `choice=entry`, then `name=value` pairs, then an optional
/// `-- doc`. A value runs up to the next `name=` word, so `tint=[1, 0.5, 0, 1]` and
/// `colors=31aaa9 f8e0a4` are one value each.
fn preset_line(n: usize, args: &str) -> Result<PresetLine, Error> {
    let usage = "expected `@preset <choice>=<entry> <param>=<value> ...`, like `@preset preset=maze feed=0.029`; the entry `*` gives the values every entry starts from";
    let (body, doc) = match args.split_once("--") {
        Some((b, d)) => (b, d.trim()),
        None => (args, ""),
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for word in body.split_whitespace() {
        let start = word.split_once('=').filter(|(name, _)| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        match (start, pairs.last_mut()) {
            (Some((name, value)), _) => pairs.push((name.to_string(), value.to_string())),
            (None, Some((_, value))) => {
                value.push(' ');
                value.push_str(word);
            }
            (None, None) => {
                return Err(Error::new(format!("`{}` is no `name=value`; {usage}", short(word))));
            }
        }
    }
    let mut pairs = pairs.into_iter().map(|(name, value)| (name, value.trim().to_string()));
    let Some((choice, entry)) = pairs.next() else { return Err(Error::new(usage)) };
    Ok(PresetLine { n, choice, entry, values: pairs.collect(), doc: doc.to_string() })
}

/// A value of a preset line, read like an `@param` default: JSON, else text; for a palette
/// always text. A value for an `@param` line must fit it; one for an `@expose` line is checked
/// when it is written, by the inner node.
fn preset_value(declared: &Declared, name: &Name, text: &str) -> Result<Value, String> {
    if text.is_empty() {
        return Err(format!("`{name}=` has no value"));
    }
    let read = |palette: bool| match serde_json::from_str::<Value>(text) {
        _ if palette => Value::Text(text.to_string()),
        Ok(v) => v,
        Err(_) => Value::Text(text.to_string()),
    };
    match declared.lines.iter().find(|d| d.name() == name.as_str()) {
        Some(Declaration::Param(spec)) => spec.check(&read(spec.kind == ParamKind::Palette)).map_err(|e| e.0),
        Some(Declaration::Expose(_)) => Ok(read(false)),
        None => Err(format!("`{name}` is declared by no @param or @expose line")),
    }
}

/// The choice a preset line starts with, checked: an `@param` choice, and the entry one of
/// its entries or `*`.
fn preset_choice(declared: &Declared, line: &PresetLine) -> Result<Name, String> {
    let name = Name::new(&line.choice).map_err(|e| e.0)?;
    let example = "a preset line starts with a choice and one of its entries, like `preset=maze`";
    match declared.lines.iter().find(|d| d.name() == name.as_str()) {
        Some(Declaration::Param(spec)) => match &spec.kind {
            ParamKind::Choice(entries) => {
                if line.entry == BASE || entries.contains(&line.entry) {
                    Ok(name)
                } else {
                    Err(format!(
                        "`{}` is no entry of `{name}`; entries: {}, and * for the values every entry starts from",
                        short(&line.entry),
                        entries.join(", ")
                    ))
                }
            }
            _ => Err(format!("`{name}` is {}; {example}", spec.kind_label())),
        },
        Some(Declaration::Expose(_)) => Err(format!(
            "`{name}` is shown from inside by @expose; presets need a choice the component holds, an @param line"
        )),
        None => Err(format!("`{name}` is declared by no @param line; {example}")),
    }
}

/// Where each value of the preset lines came from: (choice's place, entry, name, line).
type Given = Vec<(usize, String, Name, usize)>;

/// Reads the `@preset` lines into [`Declared::presets`], once every other line is known. A
/// line whose choice or entry is wrong costs the whole line; a wrong value costs that value.
fn read_presets(out: &mut Declared, lines: Vec<PresetLine>) {
    let mut bad: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut families: Vec<Presets> = Vec::new();
    let mut checked: Vec<(usize, PresetLine)> = Vec::new();
    for line in lines {
        match preset_choice(out, &line) {
            Ok(choice) => {
                let at = match families.iter().position(|f| f.choice == choice) {
                    Some(at) => at,
                    None => {
                        families.push(Presets { choice, base: Vec::new(), entries: Vec::new() });
                        families.len() - 1
                    }
                };
                checked.push((at, line));
            }
            Err(why) => bad.entry(line.n).or_default().push(why),
        }
    }
    let mut given: Given = Vec::new();
    // Names whose `*` value was refused: their entry values go silently, the `*` line says why.
    let mut refused: Vec<(usize, Name)> = Vec::new();
    // The base lines first: they say which params each choice's presets own.
    for (at, line) in checked.iter().filter(|(_, l)| l.entry == BASE) {
        let Some(choice) = families.get(*at).map(|f| f.choice.clone()) else { continue };
        for (raw, text) in &line.values {
            let name = match Name::new(raw) {
                Ok(n) => n,
                Err(e) => {
                    bad.entry(line.n).or_default().push(e.0);
                    continue;
                }
            };
            let first = given.iter().find(|(f, e, n, _)| f == at && e == BASE && *n == name).map(|g| g.3);
            let other = families.iter().find(|f| f.choice != choice && f.owns(name.as_str()));
            let why = if name == choice {
                Some(format!("a preset does not write its own choice `{choice}`"))
            } else if let Some(m) = first {
                Some(format!("`{name}` is given on line {m} already"))
            } else if let Some(other) = other {
                let m = given.iter().find(|(f, _, n, _)| *n == name && f != at).map_or(0, |g| g.3);
                Some(format!(
                    "`{name}` is written by the presets of `{}` already (line {m}); a param belongs to the presets of one choice",
                    other.choice
                ))
            } else {
                None
            };
            if let Some(why) = why {
                bad.entry(line.n).or_default().push(why);
                continue;
            }
            match preset_value(out, &name, text) {
                Ok(value) => {
                    given.push((*at, BASE.to_string(), name.clone(), line.n));
                    if let Some(f) = families.get_mut(*at) {
                        f.base.push((name, value));
                    }
                }
                Err(why) => {
                    bad.entry(line.n).or_default().push(why);
                    refused.push((*at, name));
                }
            }
        }
    }
    // A choice set by another choice's presets may not set a third with its own: presets reach
    // one level, so no loop can form. The earlier line wins.
    let choices: Vec<Name> = families.iter().map(|f| f.choice.clone()).collect();
    let mut links: Vec<(usize, usize, Name)> = Vec::new();
    for (at, family) in families.iter().enumerate() {
        for (name, _) in family.base.iter().filter(|(n, _)| choices.contains(n)) {
            let n = given.iter().find(|(f, e, g, _)| *f == at && e == BASE && g == name).map_or(0, |g| g.3);
            links.push((n, at, name.clone()));
        }
    }
    links.sort_by_key(|l| l.0);
    let mut kept: Vec<(Name, Name)> = Vec::new();
    for (n, at, name) in links {
        let Some(setter) = families.get(at).map(|f| f.choice.clone()) else { continue };
        let above = kept.iter().find(|(_, set)| *set == setter).map(|(by, _)| by.clone());
        let below = kept.iter().find(|(by, _)| *by == name).map(|(_, set)| set.clone());
        let why = match (above, below) {
            (Some(by), _) if by == name => {
                format!("`{name}` and `{setter}` would set each other's choice; presets reach one level")
            }
            (Some(by), _) => format!(
                "the presets of `{by}` set `{setter}` already, so its own presets cannot set `{name}`; presets reach one level"
            ),
            (None, Some(set)) => format!(
                "the presets of `{name}` set `{set}` already, so no other presets can set `{name}`; presets reach one level"
            ),
            (None, None) => {
                kept.push((setter, name));
                continue;
            }
        };
        bad.entry(n).or_default().push(why);
        if let Some(f) = families.get_mut(at) {
            f.base.retain(|(b, _)| *b != name);
        }
        refused.push((at, name));
    }
    // Then the entries, each value for a name of its choice's `*` lines.
    for (at, line) in checked.iter().filter(|(_, l)| l.entry != BASE) {
        let Some(family) = families.get(*at) else { continue };
        let choice = family.choice.clone();
        let mut values: Vec<(Name, Value)> = Vec::new();
        for (raw, text) in &line.values {
            let name = match Name::new(raw) {
                Ok(n) => n,
                Err(e) => {
                    bad.entry(line.n).or_default().push(e.0);
                    continue;
                }
            };
            if refused.iter().any(|(f, n)| f == at && *n == name) {
                continue;
            }
            let first =
                given.iter().find(|(f, e, n, _)| f == at && *e == line.entry && *n == name).map(|g| g.3);
            let why = if name == choice {
                Some(format!("a preset does not write its own choice `{choice}`"))
            } else if !family.owns(name.as_str()) {
                Some(format!(
                    "`{name}` is on no `{choice}=*` line, and a preset writes only the params there; add it there with the value the other entries take"
                ))
            } else {
                first.map(|m| format!("`{name}` is given for `{choice}={}` on line {m} already", line.entry))
            };
            if let Some(why) = why {
                bad.entry(line.n).or_default().push(why);
                continue;
            }
            match preset_value(out, &name, text) {
                Ok(value) => {
                    given.push((*at, line.entry.clone(), name.clone(), line.n));
                    values.push((name, value));
                }
                Err(why) => bad.entry(line.n).or_default().push(why),
            }
        }
        let Some(family) = families.get_mut(*at) else { continue };
        let entry = match family.entries.iter().position(|e| e.name == line.entry) {
            Some(i) => i,
            None => {
                family.entries.push(PresetEntry {
                    name: line.entry.clone(),
                    values: Vec::new(),
                    doc: String::new(),
                });
                family.entries.len() - 1
            }
        };
        if let Some(e) = family.entries.get_mut(entry) {
            e.values.extend(values);
            if !line.doc.is_empty() {
                if !e.doc.is_empty() {
                    e.doc.push_str("; ");
                }
                e.doc.push_str(&line.doc);
            }
        }
    }
    for (n, why) in bad {
        insert_error(&mut out.errors, n, why.join("; "));
    }
    out.presets = families;
}

/// The line number an error of [`component`] starts with.
fn error_line(error: &str) -> Option<usize> {
    error.strip_prefix("line ")?.split(':').next()?.parse().ok()
}

/// The names after `@reset`, separated by spaces or commas; `#` starts a comment.
fn reset_names(args: &str) -> Result<Vec<Name>, Error> {
    let line = args.split('#').next().unwrap_or("");
    let names: Vec<Name> = line
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .map(Name::new)
        .collect::<Result<_, _>>()?;
    if names.is_empty() {
        return Err(Error::new(
            "expected `@reset <param> <param> ...`, the params whose change starts the component over",
        ));
    }
    Ok(names)
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
    Ok(ExposeLine { name, target: target.to_string(), group: String::new() })
}

/// The `@param` line that declares `spec` (its group aside): kind, default, range and the first
/// line of its doc. Refused for what a line cannot say: a text or a file, a choice whose entries
/// hold spaces or `|`, a default or doc that would read back as something else. Unit, curve,
/// special values and a hard range are not carried: a declared range only scales the slider.
pub fn line(spec: &ParamSpec) -> Result<String, Error> {
    let name = &spec.name;
    let kind = match &spec.kind {
        ParamKind::Float => "float".to_string(),
        ParamKind::Int => "int".to_string(),
        ParamKind::Bool => "bool".to_string(),
        ParamKind::Vec(n @ 2..=4) => format!("vec{n}"),
        ParamKind::Color => "color".to_string(),
        ParamKind::Palette => "palette".to_string(),
        ParamKind::Choice(entries) => format!("choice({})", entries.join("|")),
        ParamKind::Vec(_) | ParamKind::Text | ParamKind::File { .. } => {
            return Err(Error::new(format!(
                "`{name}` is {}, which a component cannot declare",
                spec.kind_label()
            )));
        }
    };
    let default = match (&spec.kind, &spec.default) {
        (ParamKind::Choice(_) | ParamKind::Palette, Value::Text(t)) => t.clone(),
        (_, v) => v.to_string(),
    };
    let mut out = format!("{PARAM} {name} {kind}");
    if !default.is_empty() {
        out.push_str(&format!(" = {default}"));
    }
    if let Some(r) = spec.range {
        out.push_str(&format!(" {}..{}", r.min, r.max));
    }
    let doc = spec.doc.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if !doc.is_empty() {
        out.push_str(&format!(" -- {doc}"));
    }
    // Read back: what the line says must be the param.
    let back = split(&out, false).ok_or_else(|| Error::new("the line declares nothing"))?;
    let read = param(back.1, |_| Ok(()))
        .map_err(|e| Error::new(format!("`{name}` cannot be written as a line: {e}")))?;
    let same_range = match (read.range, spec.range) {
        (Some(a), Some(b)) => a.min == b.min && a.max == b.max,
        (None, None) => true,
        _ => false,
    };
    if read.name != *name || read.kind != spec.kind || read.default != spec.default || !same_range {
        return Err(Error::new(format!("`{name}` cannot be written as a line that reads back the same")));
    }
    Ok(out)
}

/// `text` with `line` added among the main rows: before the first `@group` line that opens a
/// section, else at the end.
pub fn add_line(text: &str, line: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let opens =
        |raw: &str| split(raw, false).is_some_and(|(word, args)| word == GROUP && !args.trim().is_empty());
    match lines.iter().position(|l| opens(l)) {
        Some(at) => {
            let mut out = lines;
            out.insert(at, line);
            out.join("\n")
        }
        None if text.trim().is_empty() => line.to_string(),
        None => format!("{}\n{line}", text.trim_end_matches('\n')),
    }
}

/// `text` with the `@expose` line that shows `name` replaced by `line`, in its place; `None`
/// when no such line is there.
pub fn replace_expose(text: &str, name: &str, line: &str) -> Option<String> {
    let mut found = false;
    let out: Vec<&str> = text
        .lines()
        .map(|raw| {
            let hit = !found
                && split(raw, false)
                    .filter(|(word, _)| *word == EXPOSE)
                    .and_then(|(_, args)| expose(args).ok())
                    .is_some_and(|x| x.name.as_str() == name);
            found |= hit;
            if hit { line } else { raw }
        })
        .collect();
    found.then(|| out.join("\n"))
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
    fn reset_lines_name_params_the_component_holds() {
        let d = component(
            "@reset preset, seed  # restarts
             @param preset choice(a|b)
             @param seed int
             @expose glow = show.glow
             @reset seed size glow
             @reset
             @reste preset",
            none,
        );
        let resets: Vec<&str> = d.resets.iter().map(Name::as_str).collect();
        assert_eq!(resets, ["preset", "seed"]);
        assert_eq!(d.errors.len(), 3, "{:?}", d.errors);
        assert!(
            d.errors[1].starts_with("line 6:") && d.errors[1].contains("expected `@reset"),
            "{:?}",
            d.errors
        );
        assert!(
            d.errors[2].starts_with("line 7:") && d.errors[2].contains("misspelt `@reset`"),
            "{:?}",
            d.errors
        );
        let line5 = &d.errors[0];
        assert!(line5.starts_with("line 5:"), "{line5}");
        assert!(line5.contains("`size` is declared by no @param line"), "{line5}");
        assert!(line5.contains("`glow` is shown from inside by @expose"), "{line5}");
        assert!(!line5.contains("`seed`"), "{line5}");
        assert_eq!(d.lines.len(), 3);
    }

    #[test]
    fn a_spec_becomes_a_line_that_reads_back_the_same() {
        let spec = ParamSpec::float("feed", 0.05).range(0.005, 0.11).doc("how fast U is fed in\nmore");
        assert_eq!(line(&spec).unwrap(), "@param feed float = 0.05 0.005..0.11 -- how fast U is fed in");
        let pick = ParamSpec::choice("preset", &["coral", "maze"]);
        assert_eq!(line(&pick).unwrap(), "@param preset choice(coral|maze) = coral");
        let colour = ParamSpec::color("tint", [1.0, 0.5, 0.0, 1.0]);
        assert_eq!(line(&colour).unwrap(), "@param tint color = [1, 0.5, 0, 1]");
        assert_eq!(line(&ParamSpec::int("n", -3).range(-8.0, 8.0)).unwrap(), "@param n int = -3 -8..8");
        assert_eq!(
            line(&ParamSpec::palette("colors", "31aaa9 f8e0a4")).unwrap(),
            "@param colors palette = 31aaa9 f8e0a4"
        );
        for spec in [spec, pick, colour, ParamSpec::bool("on", true), ParamSpec::vec2("at", [0.25, -1.0])] {
            let text = line(&spec).unwrap();
            let d = component(&text, none);
            assert_eq!(
                d.param(&spec.name).map(|s| (&s.kind, &s.default)),
                Some((&spec.kind, &spec.default)),
                "{text}"
            );
        }
        assert!(line(&ParamSpec::text("label", "")).unwrap_err().0.contains("cannot declare"));
        assert!(line(&ParamSpec::choice("font", &["Sans Bold", "Mono"])).is_err());
    }

    #[test]
    fn a_line_goes_among_the_main_rows_or_in_place_of_an_expose_line() {
        assert_eq!(add_line("", "@param a bool"), "@param a bool");
        assert_eq!(add_line("@param x bool\n", "@param a bool"), "@param x bool\n@param a bool");
        assert_eq!(
            add_line("@param x bool\n@group look\n@param y bool", "@param a bool"),
            "@param x bool\n@param a bool\n@group look\n@param y bool"
        );
        let d = component(&add_line("@group look\n@param y bool\n@expose z = n.z", "@param a bool"), none);
        assert_eq!(d.param("a").unwrap().group, "");
        assert_eq!(d.exposed().next().unwrap().group, "look");
        assert_eq!(
            replace_expose(
                "@param x bool\n@expose feed = step.feed\n@expose f2 = s.f",
                "feed",
                "@param feed float"
            ),
            Some("@param x bool\n@param feed float\n@expose f2 = s.f".to_string())
        );
        assert_eq!(replace_expose("@param feed float", "feed", "x"), None);
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

    const PRESETS: &str = "@preset preset=* feed=0.05 kill=0.06 pattern=blobs outside=off tint=[1, 0.5, 0, 1]\n\
                           @param preset choice(coral|maze|letters) = coral -- the pattern\n\
                           @param pattern choice(blobs|noise)\n\
                           @param tint color\n\
                           @param colors palette\n\
                           @param outside choice(off|coral|maze)\n\
                           @expose feed = step.feed\n\
                           @expose kill = step.kill\n\
                           @expose outside_feed = step.outside_feed\n\
                           @preset preset=* colors=31aaa9 f8e0a4\n\
                           @preset preset=maze feed=0.029 kill=0.057 -- labyrinth\n\
                           @preset preset=maze pattern=noise -- from noise\n\
                           @preset preset=letters outside=coral\n\
                           @preset outside=* outside_feed=0\n\
                           @preset outside=coral outside_feed=0.0545";

    #[test]
    fn preset_lines_give_a_base_and_the_values_of_entries() {
        let d = component(PRESETS, none);
        assert!(d.errors.is_empty(), "{:?}", d.errors);
        let names: Vec<&str> = d.presets.iter().map(|p| p.choice.as_str()).collect();
        assert_eq!(names, ["preset", "outside"]);
        let p = d.presets_of("preset").unwrap();
        let owned: Vec<&str> = p.base.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(owned, ["feed", "kill", "pattern", "outside", "tint", "colors"]);
        assert_eq!(p.value("coral", "tint"), Some(&Value::Vec4([1.0, 0.5, 0.0, 1.0])), "checked by the spec");
        assert_eq!(p.value("coral", "colors"), Some(&Value::Text("31aaa9 f8e0a4".into())));
        let maze = p.values("maze");
        assert_eq!(maze[0], (Name::new("feed").unwrap(), Value::Float(0.029)));
        assert_eq!(maze[2], (Name::new("pattern").unwrap(), Value::Text("noise".into())));
        assert_eq!(maze[3].1, Value::Text("off".into()), "the base where the entry says nothing");
        assert_eq!(p.entry("maze").unwrap().doc, "labyrinth; from noise", "lines of one entry add up");
        assert!(p.entry("coral").is_none(), "an entry without lines writes the base");
        assert_eq!(p.value("letters", "outside"), Some(&Value::Text("coral".into())));
        assert_eq!(d.owner("outside_feed").map(|p| p.choice.as_str()), Some("outside"));
        assert!(d.owner("preset").is_none() && d.owner("speed").is_none());
    }

    #[test]
    fn a_wrong_preset_line_or_value_costs_only_itself() {
        let text = format!(
            "{PRESETS}\n\
             @preset preset=spots feed=1\n\
             @preset preset=maze speed=2 kill=0.05 feed=0.03\n\
             @preset preset=letters pattern=rings tint=red\n\
             @preset feed=* kill=0\n\
             @preset pattern=* preset=maze\n\
             @preset nonsense\n\
             @preset preset=* kill=0.07 preset=maze glow=1\n\
             @preset outside=* feed=0.1\n\
             @param speed float"
        );
        let d = component(&text, none);
        let expect = [
            ("line 16:", &["`spots` is no entry of `preset`"][..]),
            (
                "line 17:",
                &["`speed` is on no `preset=*` line", "`kill` is given for `preset=maze` on line 11"],
            ),
            ("line 18:", &["`pattern` must be one of", "`tint` expects"]),
            ("line 19:", &["`feed` is shown from inside by @expose"]),
            ("line 20:", &["`preset` and `pattern` would set each other's choice"]),
            ("line 21:", &["is no `name=value`"]),
            (
                "line 22:",
                &["`kill` is given on line 1", "does not write its own choice", "`glow` is declared by no"],
            ),
            ("line 23:", &["`feed` is written by the presets of `preset` already (line 1)"]),
        ];
        assert_eq!(d.errors.len(), expect.len(), "{:#?}", d.errors);
        for (e, (line, needles)) in d.errors.iter().zip(expect) {
            assert!(e.starts_with(line), "{e}: wants {line}");
            for needle in needles {
                assert!(e.contains(needle), "{e}: wants {needle}");
            }
        }
        // What is fine on those lines holds.
        let p = d.presets_of("preset").unwrap();
        assert_eq!(p.value("maze", "feed"), Some(&Value::Float(0.029)), "the first value given wins");
        assert!(p.owns("pattern"), "presets of `pattern` naming `preset` cost that link, not `pattern`");
        assert!(d.presets_of("pattern").is_some_and(|f| f.base.is_empty()));
        assert!(!d.presets_of("outside").unwrap().owns("feed"));
    }

    #[test]
    fn presets_set_a_choice_with_presets_one_level_deep() {
        let d = component(
            "@param a choice(x|y)\n\
             @param b choice(x|y)\n\
             @param c choice(x|y)\n\
             @param v float\n\
             @param w float\n\
             @preset a=* b=x\n\
             @preset b=* c=y v=1\n\
             @preset c=* w=2\n\
             @preset a=y b=y",
            none,
        );
        assert_eq!(d.errors.len(), 1, "{:?}", d.errors);
        assert!(
            d.errors[0]
                .starts_with("line 7: the presets of `a` set `b` already, so its own presets cannot set `c`"),
            "{:?}",
            d.errors
        );
        let b: Vec<&str> = d.presets_of("b").unwrap().base.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(b, ["v"], "the link to `c` goes, `v` stays");
        assert_eq!(d.presets_of("a").unwrap().value("y", "b"), Some(&Value::Text("y".into())));
    }
}
