//! Parameter metadata.
//!
//! Every node declares its parameters as [`ParamSpec`]s: type, default, range, unit, slider curve,
//! the group the UI shows it in (main rows, or a section folded away), a one-line doc, for a
//! choice what each value does, and for a number the values that stand for something else
//! (`rate` 0: the show rate), which the panel shows by name. The spec is the single source for validation (every edit passes
//! [`ParamSpec::check`]), for the param panel, its help overlay ([`ParamSpec::help`]) and for what
//! `node.kinds` and `node.info` report.

use serde::{Deserialize, Serialize};

use crate::{Error, Name, Value};

/// Longest accepted text value in bytes.
pub const MAX_TEXT: usize = 64 * 1024;
/// Longest accepted file path in bytes.
pub const MAX_FILE: usize = 4096;
/// The group of rarely used params, shown after every other group.
pub const MORE: &str = "more";
/// Longest accepted group name in bytes.
pub const MAX_GROUP: usize = 32;

/// What kind of value a parameter holds and how the UI edits it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Bool,
    Int,
    Float,
    /// 2 to 4 floats, e.g. a position or a size.
    Vec(u8),
    /// Linear RGBA, four floats; values above 1 are allowed (HDR). A three-component value gets
    /// alpha 1.
    Color,
    /// One of a fixed set of words.
    Choice(Vec<String>),
    Text,
    /// A file path, relative to the project directory where possible. `extensions` (lower case,
    /// without the dot) restricts what the UI offers; empty means any.
    File {
        extensions: Vec<String>,
    },
    /// A palette or colormap ([`crate::palette`]): text naming one (or listing hex colours),
    /// empty for none, or an int picking one by its place in the list (what a bound channel
    /// gives).
    Palette,
}

/// The range of a numeric parameter. A `hard` range clamps every value; a soft one is only the
/// slider range, and typed values outside it are kept.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub hard: bool,
}

/// How a slider position maps to a value inside the range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Linear,
    /// Equal slider steps are equal ratios (frequencies, scales). Needs `min > 0`.
    Log,
    /// `value = min + (max - min) * position^gamma`; `gamma > 1` gives fine control near `min`.
    Power(f64),
}

/// Physical unit, shown next to the value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    #[default]
    None,
    Seconds,
    Frames,
    Hertz,
    Beats,
    Pixels,
    Metres,
    Degrees,
    Percent,
    Decibels,
}

impl Unit {
    pub fn symbol(self) -> &'static str {
        match self {
            Unit::None => "",
            Unit::Seconds => "s",
            Unit::Frames => "f",
            Unit::Hertz => "Hz",
            Unit::Beats => "beats",
            Unit::Pixels => "px",
            Unit::Metres => "m",
            Unit::Degrees => "\u{b0}",
            Unit::Percent => "%",
            Unit::Decibels => "dB",
        }
    }
}

/// One declared parameter of a node type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    pub default: Value,
    pub range: Option<Range>,
    pub unit: Unit,
    pub curve: Curve,
    /// The section of the param panel it sits in: empty for the main rows, which are always
    /// shown; any other name is a section folded away until opened ([`MORE`] for the rarely
    /// used ones).
    #[serde(default)]
    pub group: String,
    /// Text edited in a box of several lines (a table typed in by hand).
    #[serde(default)]
    pub multiline: bool,
    /// Lines of a multiline text's box; 0 for the panel's usual size.
    #[serde(default)]
    pub lines: u8,
    /// A text parameter naming another node (`/path`, or the name of a sibling): the named
    /// node cooks first, as if wired to this one, a new result of it recooks this node, and the
    /// operator sees its outputs (TouchDesigner's Select TOP).
    #[serde(default)]
    pub node_ref: bool,
    /// A program the node runs (a script's source). Only the user and the control channel set
    /// it, never a message from the network: whoever can send OSC must not run code here.
    #[serde(default)]
    pub code: bool,
    pub doc: String,
    /// What some or all values of a choice do, in any order; shown after [`ParamSpec::doc`]
    /// ([`ParamSpec::full_doc`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choice_docs: Vec<ChoiceDoc>,
    /// Values of a number that do not mean their number (`0`: automatic); the panel shows the
    /// label instead of the number while the value is one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specials: Vec<Special>,
    /// The value the project file holds is not taken when a project opens: the param opens at
    /// its default. For params that start something (a recording), so opening a project never
    /// starts it by itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opens_at_default: bool,
}

/// A value of a number param that stands for something other than its number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Special {
    pub value: f64,
    /// What the value means, in a word or two (`show rate`, `automatic`).
    pub label: String,
    /// What the panel adds to the label in brackets, when it knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolves: Option<Resolves>,
}

/// What a special value comes out as, which only the running app knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolves {
    /// The show's frame rate, e.g. `60 Hz`.
    ShowRate,
}

/// What one value of a choice param does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceDoc {
    pub value: String,
    pub doc: String,
}

/// Everything the help overlay of a param shows, in words: [`ParamSpec::help`]. The editor
/// draws it, `node.kinds {kind, param}` returns it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamHelp {
    pub name: String,
    /// What the param does; empty when nobody wrote it down yet.
    pub doc: String,
    /// `float`, `vec3`, `colour`, `choice`, `text`, `node`, `file (png, jpg)`, ...
    #[serde(rename = "type")]
    pub type_label: String,
    /// The default as typed, e.g. `0.5`, `[0, 0, 1]`, `over`; empty for empty text.
    pub default: String,
    /// The range with its unit, e.g. `0 to 10 s`; empty without a range.
    pub range: String,
    /// Whether values outside the range are clamped (else the range is only the slider's).
    pub clamped: bool,
    /// The unit's symbol (`s`, `px`, `%`, ...); empty for plain numbers.
    pub unit: String,
    /// How the slider maps to values: `linear`, `log` (equal steps are equal ratios) or `power`
    /// (finer near the minimum).
    pub curve: String,
    /// The section of the param panel; empty for the main rows.
    pub group: String,
    /// Whether a channel can drive it (numbers, vectors, colours).
    pub bindable: bool,
    /// For a choice: every value, with what it does where that is written down.
    pub choices: Vec<ChoiceHelp>,
    /// Values of a number that stand for something else, as `0: show rate`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub specials: Vec<String>,
}

/// One value of a choice in [`ParamHelp`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChoiceHelp {
    pub value: String,
    pub doc: String,
    pub default: bool,
}

impl ParamSpec {
    fn new(name: &str, kind: ParamKind, default: Value) -> Self {
        Self {
            name: name.to_string(),
            kind,
            default,
            range: None,
            unit: Unit::None,
            curve: Curve::Linear,
            group: String::new(),
            multiline: false,
            lines: 0,
            node_ref: false,
            code: false,
            doc: String::new(),
            choice_docs: Vec::new(),
            specials: Vec::new(),
            opens_at_default: false,
        }
    }

    pub fn bool(name: &str, default: bool) -> Self {
        Self::new(name, ParamKind::Bool, Value::Bool(default))
    }

    pub fn int(name: &str, default: i64) -> Self {
        Self::new(name, ParamKind::Int, Value::Int(default))
    }

    pub fn float(name: &str, default: f64) -> Self {
        Self::new(name, ParamKind::Float, Value::Float(default))
    }

    pub fn vec2(name: &str, default: [f64; 2]) -> Self {
        Self::new(name, ParamKind::Vec(2), Value::Vec2(default))
    }

    pub fn vec3(name: &str, default: [f64; 3]) -> Self {
        Self::new(name, ParamKind::Vec(3), Value::Vec3(default))
    }

    pub fn vec4(name: &str, default: [f64; 4]) -> Self {
        Self::new(name, ParamKind::Vec(4), Value::Vec4(default))
    }

    pub fn color(name: &str, default: [f64; 4]) -> Self {
        Self::new(name, ParamKind::Color, Value::Vec4(default))
    }

    /// A choice; the default is the first entry (empty text when `choices` is empty, which
    /// [`ParamSpec::validate_spec`] then refuses).
    pub fn choice(name: &str, choices: &[&str]) -> Self {
        let default = choices.first().map(|c| c.to_string()).unwrap_or_default();
        Self::new(
            name,
            ParamKind::Choice(choices.iter().map(|c| c.to_string()).collect()),
            Value::Text(default),
        )
    }

    pub fn text(name: &str, default: &str) -> Self {
        Self::new(name, ParamKind::Text, Value::Text(default.to_string()))
    }

    /// A palette ([`ParamKind::Palette`]); `default` a name, a hex list or empty.
    pub fn palette(name: &str, default: &str) -> Self {
        Self::new(name, ParamKind::Palette, Value::Text(default.to_string()))
    }

    pub fn file(name: &str, extensions: &[&str]) -> Self {
        Self::new(
            name,
            ParamKind::File { extensions: extensions.iter().map(|e| e.to_lowercase()).collect() },
            Value::Text(String::new()),
        )
    }

    /// Slider range; values outside it can still be typed.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.range = Some(Range { min, max, hard: false });
        self
    }

    /// Range that clamps every value.
    pub fn clamp(mut self, min: f64, max: f64) -> Self {
        self.range = Some(Range { min, max, hard: true });
        self
    }

    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    pub fn curve(mut self, curve: Curve) -> Self {
        self.curve = curve;
        self
    }

    /// In the section `group` ([`ParamSpec::group`]); empty for the main rows.
    pub fn group(mut self, group: &str) -> Self {
        self.group = group.to_string();
        self
    }

    /// In the section of rarely used params ([`MORE`]).
    pub fn more(self) -> Self {
        self.group(MORE)
    }

    pub fn doc(mut self, doc: &str) -> Self {
        self.doc = doc.to_string();
        self
    }

    /// What the choice `value` does ([`ParamSpec::choice_docs`]); a value that is not among the
    /// choices fails [`ParamSpec::validate_spec`].
    pub fn choice_doc(mut self, value: &str, doc: &str) -> Self {
        self.choice_docs.push(ChoiceDoc { value: value.to_string(), doc: doc.to_string() });
        self
    }

    /// Value `value` of a number stands for `label` ([`ParamSpec::specials`]); the param's
    /// doc says what it does. A value outside a hard range or not whole for an int, a label
    /// with digits, a value or label given twice, or a param that is not a number fail
    /// [`ParamSpec::validate_spec`].
    pub fn special(mut self, value: f64, label: &str) -> Self {
        self.specials.push(Special { value, label: label.to_string(), resolves: None });
        self
    }

    /// As [`ParamSpec::special`], and the panel adds what the value comes out as in brackets
    /// (`show rate (60 Hz)`).
    pub fn special_resolved(mut self, value: f64, label: &str, resolves: Resolves) -> Self {
        self.specials.push(Special { value, label: label.to_string(), resolves: Some(resolves) });
        self
    }

    /// The param opens at its default ([`ParamSpec::opens_at_default`]).
    pub fn opens_at_default(mut self) -> Self {
        self.opens_at_default = true;
        self
    }

    /// The special value `value` is, if it is one.
    pub fn special_of(&self, value: f64) -> Option<&Special> {
        self.specials.iter().find(|s| s.value == value)
    }

    /// The special value a typed text names: its label, case and surrounding spaces aside, also
    /// with more after it (`show rate (60 Hz)`).
    pub fn special_named(&self, text: &str) -> Option<&Special> {
        let t = text.trim().to_lowercase();
        self.specials.iter().find(|s| {
            let l = s.label.to_lowercase();
            t.strip_prefix(&l).is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '(']))
        })
    }

    /// The special values as `0: show rate`, in the order declared.
    pub fn special_lines(&self) -> Vec<String> {
        self.specials.iter().map(|s| format!("{}: {}", s.value, s.label)).collect()
    }

    /// What the choice `value` does, when written down.
    pub fn doc_of_choice(&self, value: &str) -> Option<&str> {
        self.choice_docs.iter().find(|c| c.value == value).map(|c| c.doc.as_str())
    }

    /// The doc with what each choice does after it, in the order of the choices, as one line:
    /// `doc; a: ...; b: ...`; special values after that as `0: show rate`.
    pub fn full_doc(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.doc.is_empty() {
            parts.push(self.doc.clone());
        }
        if let ParamKind::Choice(choices) = &self.kind {
            for c in choices {
                if let Some(d) = self.doc_of_choice(c) {
                    parts.push(format!("{c}: {d}"));
                }
            }
        }
        parts.extend(self.special_lines());
        parts.join("; ")
    }

    /// The type in a word or two: `float`, `vec3`, `colour`, `a | b | c` for a choice, `node`
    /// for a node reference, `file (png, jpg)`.
    pub fn type_label(&self) -> String {
        match &self.kind {
            ParamKind::Bool => "bool".into(),
            ParamKind::Int => "int".into(),
            ParamKind::Float => "float".into(),
            ParamKind::Vec(n) => format!("vec{n}"),
            ParamKind::Color => "colour".into(),
            ParamKind::Choice(words) => words.join(" | "),
            ParamKind::Text if self.node_ref => "node".into(),
            ParamKind::Text => "text".into(),
            ParamKind::File { extensions } if extensions.is_empty() => "file".into(),
            ParamKind::File { extensions } => format!("file ({})", extensions.join(", ")),
            ParamKind::Palette => "palette".into(),
        }
    }

    /// The range with its unit, `0 to 10 s` (degrees without the space); just the unit without
    /// a range; empty for neither.
    pub fn range_label(&self) -> String {
        let unit = self.unit.symbol();
        let gap = if self.unit == Unit::Degrees { "" } else { " " };
        match self.range {
            Some(r) if unit.is_empty() => format!("{} to {}", r.min, r.max),
            Some(r) => format!("{} to {}{gap}{unit}", r.min, r.max),
            None => unit.to_string(),
        }
    }

    /// The default as one would type it: `0.5`, `[0, 0, 1]`, `over`, with a special value's
    /// label after it (`0 (show rate)`); `None` for empty text and `several lines` for a text of
    /// several lines (a script's example).
    pub fn default_label(&self) -> Option<String> {
        match &self.default {
            Value::Text(t) if t.is_empty() => None,
            Value::Text(t) if t.contains('\n') => Some("several lines".into()),
            Value::Text(t) => Some(t.clone()),
            v => match v.as_f64().and_then(|x| self.special_of(x)) {
                Some(s) => Some(format!("{v} ({})", s.label)),
                None => Some(v.to_string()),
            },
        }
    }

    /// What the help overlay shows ([`ParamHelp`]).
    pub fn help(&self) -> ParamHelp {
        let default = self.default.as_str().unwrap_or_default();
        let choices = match &self.kind {
            ParamKind::Choice(words) => words
                .iter()
                .map(|w| ChoiceHelp {
                    value: w.clone(),
                    doc: self.doc_of_choice(w).unwrap_or_default().to_string(),
                    default: w == default,
                })
                .collect(),
            _ => Vec::new(),
        };
        let type_label = match &self.kind {
            ParamKind::Choice(_) => "choice".to_string(),
            _ => self.type_label(),
        };
        ParamHelp {
            name: self.name.clone(),
            doc: self.doc.clone(),
            type_label,
            default: self.default_label().unwrap_or_default(),
            range: if self.range.is_some() { self.range_label() } else { String::new() },
            clamped: self.range.is_some_and(|r| r.hard),
            unit: self.unit.symbol().to_string(),
            curve: match self.curve {
                Curve::Linear => "linear",
                Curve::Log => "log",
                Curve::Power(_) => "power",
            }
            .to_string(),
            group: self.group.clone(),
            bindable: self.bind_components() > 0,
            choices,
            specials: self.special_lines(),
        }
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    /// A multiline text whose box shows `lines` lines (a script).
    pub fn lines(mut self, lines: u8) -> Self {
        self.multiline = true;
        self.lines = lines;
        self
    }

    /// A program the node runs ([`ParamSpec::code`]).
    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// A text parameter naming another node ([`ParamSpec::node_ref`]).
    pub fn node(name: &str) -> Self {
        let mut spec = Self::new(name, ParamKind::Text, Value::Text(String::new()));
        spec.node_ref = true;
        spec
    }

    /// Number of components a channel can drive one at a time: 1 for bool, int, float and
    /// palette (bound as a whole; a palette by its place in the list), 2 to 4 for vectors and
    /// colours (bound per component), 0 for choices, text and files, which channels cannot
    /// drive.
    pub fn bind_components(&self) -> u8 {
        match self.kind {
            ParamKind::Bool | ParamKind::Int | ParamKind::Float | ParamKind::Palette => 1,
            ParamKind::Vec(n) => n,
            ParamKind::Color => 4,
            ParamKind::Choice(_) | ParamKind::Text | ParamKind::File { .. } => 0,
        }
    }

    /// Whether a channel may drive this parameter: as a whole (`component` `None`) for scalar
    /// kinds, one component of a vector or colour otherwise.
    pub fn check_bindable(&self, component: Option<u8>) -> Result<(), Error> {
        let n = self.bind_components();
        match (n, component) {
            (0, _) => Err(Error::new(format!(
                "`{}` is {}; channels drive numbers only",
                self.name,
                self.kind_label()
            ))),
            (1, None) => Ok(()),
            (1, Some(_)) => Err(Error::new(format!("`{}` has no components; bind it as a whole", self.name))),
            (_, None) => Err(Error::new(format!(
                "`{}` has {n} components; bind them one at a time (`{}.0` to `{}.{}`)",
                self.name,
                self.name,
                self.name,
                n - 1
            ))),
            (_, Some(c)) if c < n => Ok(()),
            (_, Some(c)) => Err(Error::new(format!("`{}` has no component {c}", self.name))),
        }
    }

    /// `current` with a channel's value `x` put in (the whole value or one component), in
    /// canonical form ([`ParamSpec::check`]: hard ranges clamp). Ints round, bools are true from
    /// 0.5 up.
    pub fn bind_value(&self, current: &Value, component: Option<u8>, x: f64) -> Result<Value, Error> {
        self.check_bindable(component)?;
        let x = if x.is_finite() { x } else { 0.0 };
        let value = match (&self.kind, component) {
            (ParamKind::Bool, _) => Value::Bool(x >= 0.5),
            // `as` saturates.
            (ParamKind::Int | ParamKind::Palette, _) => Value::Int(x.round() as i64),
            (ParamKind::Float, _) => Value::Float(x),
            (_, Some(c)) => {
                let mut v = current.clone();
                let slot = match &mut v {
                    Value::Vec2(a) => a.get_mut(usize::from(c)),
                    Value::Vec3(a) => a.get_mut(usize::from(c)),
                    Value::Vec4(a) => a.get_mut(usize::from(c)),
                    _ => None,
                };
                *slot.ok_or_else(|| Error::new(format!("`{}` has no component {c}", self.name)))? = x;
                v
            }
            (_, None) => return Err(Error::new(format!("`{}` needs a component", self.name))),
        };
        self.check(&value)
    }

    /// Checks the spec itself: a valid name, a sane range and curve, a default that passes
    /// [`ParamSpec::check`] unchanged. Node type registration calls this, so a broken built-in
    /// declaration is an error at startup, not a surprise later.
    pub fn validate_spec(&self) -> Result<(), Error> {
        let ctx = || format!("param `{}`", self.name);
        Name::new(&self.name).map_err(|e| e.context(ctx()))?;
        if let Some(r) = self.range {
            if !(r.min.is_finite() && r.max.is_finite() && r.min < r.max) {
                return Err(Error::new(format!("range {}..{} must be finite with min < max", r.min, r.max))
                    .context(ctx()));
            }
            if self.curve == Curve::Log && r.min <= 0.0 {
                return Err(Error::new("a log curve needs min > 0").context(ctx()));
            }
        }
        if let Curve::Power(g) = self.curve
            && !(g.is_finite() && g > 0.0)
        {
            return Err(Error::new(format!("power curve gamma {g} must be positive")).context(ctx()));
        }
        if self.group.len() > MAX_GROUP
            || self.group.trim() != self.group
            || self.group.chars().any(char::is_control)
        {
            return Err(Error::new(format!(
                "group `{}` must be at most {MAX_GROUP} bytes, without control characters or \
                 surrounding spaces",
                self.group.escape_debug()
            ))
            .context(ctx()));
        }
        match &self.kind {
            ParamKind::Vec(n) if !(2..=4).contains(n) => {
                return Err(Error::new(format!("vec size {n} must be 2 to 4")).context(ctx()));
            }
            ParamKind::Choice(c) if c.is_empty() => {
                return Err(Error::new("a choice needs at least one entry").context(ctx()));
            }
            _ => {}
        }
        for (i, d) in self.choice_docs.iter().enumerate() {
            let known = matches!(&self.kind, ParamKind::Choice(c) if c.contains(&d.value));
            if !known {
                return Err(Error::new(format!("doc for `{}`, which is not one of its choices", d.value))
                    .context(ctx()));
            }
            if self.choice_docs.iter().take(i).any(|e| e.value == d.value) {
                return Err(Error::new(format!("choice `{}` described twice", d.value)).context(ctx()));
            }
        }
        for (i, sp) in self.specials.iter().enumerate() {
            let value = match self.kind {
                // `as` saturates; a whole value that big is outside any range anyway.
                ParamKind::Int if sp.value.fract() == 0.0 => Value::Int(sp.value as i64),
                ParamKind::Float => Value::Float(sp.value),
                _ => {
                    return Err(Error::new(format!(
                        "special value {} on {}; only floats and ints (whole values) have them",
                        sp.value,
                        self.kind_label()
                    ))
                    .context(ctx()));
                }
            };
            if self.check(&value).ok().as_ref() != Some(&value) {
                return Err(
                    Error::new(format!("special value {} is outside the range", sp.value)).context(ctx())
                );
            }
            let label = sp.label.trim();
            if label.is_empty()
                || label != sp.label
                || label.chars().any(|c| c.is_ascii_digit() || c.is_control() || c == '(')
            {
                return Err(Error::new(format!(
                    "special label `{}` must be words, without digits, brackets or surrounding spaces",
                    sp.label.escape_debug()
                ))
                .context(ctx()));
            }
            if self.specials.iter().take(i).any(|o| o.value == sp.value || o.label == sp.label) {
                return Err(Error::new(format!("special value {} or `{}` given twice", sp.value, sp.label))
                    .context(ctx()));
            }
        }
        let checked = self.check(&self.default).map_err(|e| e.context(format!("{}: default", ctx())))?;
        if checked != self.default {
            return Err(Error::new(format!("default {} is not in canonical form ({checked})", self.default))
                .context(ctx()));
        }
        Ok(())
    }

    /// Validates `value` for this parameter and returns it in canonical form: an int where a
    /// float is expected becomes a float, a value outside a hard range is clamped, a colour with
    /// three components gets alpha 1. NaN and infinities are refused.
    pub fn check(&self, value: &Value) -> Result<Value, Error> {
        let wrong = || {
            Error::new(format!(
                "`{}` expects {}, got {} {}",
                self.name,
                self.kind_label(),
                value.type_name(),
                value
            ))
        };
        let clamp = |x: f64| match self.range {
            Some(r) if r.hard => x.clamp(r.min, r.max),
            _ => x,
        };
        let finite = |xs: &[f64]| {
            if xs.iter().all(|x| x.is_finite()) {
                Ok(())
            } else {
                Err(Error::new(format!("`{}` must be finite, got {value}", self.name)))
            }
        };
        match (&self.kind, value) {
            (ParamKind::Bool, Value::Bool(b)) => Ok(Value::Bool(*b)),
            (ParamKind::Int, Value::Int(i)) => Ok(Value::Int(self.clamp_int(*i))),
            (ParamKind::Int, Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
                Ok(Value::Int(self.clamp_int(*f as i64)))
            }
            (ParamKind::Float, Value::Int(_) | Value::Float(_)) => {
                let x = value.as_f64().ok_or_else(wrong)?;
                finite(&[x])?;
                Ok(Value::Float(clamp(x)))
            }
            (ParamKind::Vec(2), Value::Vec2(v)) => {
                finite(v)?;
                Ok(Value::Vec2(v.map(clamp)))
            }
            (ParamKind::Vec(3), Value::Vec3(v)) => {
                finite(v)?;
                Ok(Value::Vec3(v.map(clamp)))
            }
            (ParamKind::Vec(4), Value::Vec4(v)) => {
                finite(v)?;
                Ok(Value::Vec4(v.map(clamp)))
            }
            (ParamKind::Color, Value::Vec4(v)) => {
                finite(v)?;
                Ok(Value::Vec4(*v))
            }
            (ParamKind::Color, Value::Vec3([r, g, b])) => {
                finite(&[*r, *g, *b])?;
                Ok(Value::Vec4([*r, *g, *b, 1.0]))
            }
            (ParamKind::Choice(choices), Value::Text(s)) => {
                if choices.iter().any(|c| c == s) {
                    Ok(Value::Text(s.clone()))
                } else {
                    Err(Error::new(format!(
                        "`{}` must be one of {}, got {value}",
                        self.name,
                        choices.join(", ")
                    )))
                }
            }
            (ParamKind::Text, Value::Text(s)) => {
                if s.len() > MAX_TEXT {
                    return Err(Error::new(format!("`{}` is longer than {MAX_TEXT} bytes", self.name)));
                }
                Ok(Value::Text(s.clone()))
            }
            (ParamKind::Palette, Value::Text(s)) => {
                if s.len() > crate::palette::MAX_VALUE {
                    return Err(Error::new(format!(
                        "`{}` is longer than {} bytes",
                        self.name,
                        crate::palette::MAX_VALUE
                    )));
                }
                crate::palette::wanted(value).map_err(|e| Error::new(format!("`{}`: {e}", self.name)))?;
                Ok(Value::Text(s.clone()))
            }
            (ParamKind::Palette, Value::Int(i)) => Ok(Value::Int(*i)),
            (ParamKind::Palette, Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
                Ok(Value::Int(*f as i64))
            }
            (ParamKind::File { .. }, Value::Text(s)) => {
                if s.len() > MAX_FILE || s.contains('\0') {
                    return Err(Error::new(format!("`{}` is not a usable file path", self.name)));
                }
                Ok(Value::Text(s.clone()))
            }
            _ => Err(wrong()),
        }
    }

    fn clamp_int(&self, i: i64) -> i64 {
        match self.range {
            // `as` saturates; the bounds come from a validated spec.
            Some(r) if r.hard => i.clamp(r.min.ceil() as i64, r.max.floor() as i64),
            _ => i,
        }
    }

    pub fn kind_label(&self) -> String {
        match &self.kind {
            ParamKind::Bool => "a bool".into(),
            ParamKind::Int => "an int".into(),
            ParamKind::Float => "a number".into(),
            ParamKind::Vec(n) => format!("{n} numbers"),
            ParamKind::Color => "a colour [r, g, b, a]".into(),
            ParamKind::Choice(_) => "a choice".into(),
            ParamKind::Text => "text".into(),
            ParamKind::File { .. } => "a file path".into(),
            ParamKind::Palette => "a palette (a name, hex colours or a number)".into(),
        }
    }

    /// Slider position (0 to 1) of a value, following the curve; `None` without a range.
    pub fn to_slider(&self, value: f64) -> Option<f64> {
        let r = self.range?;
        let s = match self.curve {
            Curve::Linear => (value - r.min) / (r.max - r.min),
            Curve::Log => (value.max(r.min) / r.min).ln() / (r.max / r.min).ln(),
            Curve::Power(g) => ((value - r.min) / (r.max - r.min)).clamp(0.0, 1.0).powf(1.0 / g),
        };
        s.is_finite().then_some(s.clamp(0.0, 1.0))
    }

    /// Value at a slider position (0 to 1), following the curve; `None` without a range.
    pub fn from_slider(&self, position: f64) -> Option<f64> {
        let r = self.range?;
        let s = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.0 };
        let v = match self.curve {
            Curve::Linear => r.min + s * (r.max - r.min),
            Curve::Log => r.min * (r.max / r.min).powf(s),
            Curve::Power(g) => r.min + s.powf(g) * (r.max - r.min),
        };
        v.is_finite().then_some(v)
    }
}

/// The named groups of `specs` in the order the panel shows them: as they first appear, with
/// [`MORE`] last. The main rows (empty group) are not among them.
pub fn groups(specs: &[ParamSpec]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for s in specs {
        if !s.group.is_empty() && !out.contains(&s.group.as_str()) {
            out.push(&s.group);
        }
    }
    if let Some(i) = out.iter().position(|g| *g == MORE) {
        let more = out.remove(i);
        out.push(more);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercion_and_clamping() {
        let f = ParamSpec::float("x", 0.5).clamp(0.0, 1.0);
        assert_eq!(f.check(&Value::Int(1)).unwrap(), Value::Float(1.0));
        assert_eq!(f.check(&Value::Float(7.0)).unwrap(), Value::Float(1.0));
        assert!(f.check(&Value::Float(f64::NAN)).is_err());
        assert!(f.check(&Value::Text("1".into())).is_err());

        let soft = ParamSpec::float("x", 0.5).range(0.0, 1.0);
        assert_eq!(soft.check(&Value::Float(7.0)).unwrap(), Value::Float(7.0));

        let i = ParamSpec::int("n", 3).clamp(1.0, 8.0);
        assert_eq!(i.check(&Value::Float(4.0)).unwrap(), Value::Int(4));
        assert_eq!(i.check(&Value::Int(100)).unwrap(), Value::Int(8));
        assert!(i.check(&Value::Float(4.5)).is_err());

        let c = ParamSpec::color("c", [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(c.check(&Value::Vec3([2.0, 0.0, 0.0])).unwrap(), Value::Vec4([2.0, 0.0, 0.0, 1.0]));

        let v = ParamSpec::vec2("p", [0.0, 0.0]).clamp(-1.0, 1.0);
        assert_eq!(v.check(&Value::Vec2([5.0, -5.0])).unwrap(), Value::Vec2([1.0, -1.0]));
        assert!(v.check(&Value::Vec3([0.0, 0.0, 0.0])).is_err());

        let ch = ParamSpec::choice("mode", &["add", "over"]);
        assert_eq!(ch.default, Value::Text("add".into()));
        assert!(ch.check(&Value::Text("over".into())).is_ok());
        let err = ch.check(&Value::Text("nope".into())).unwrap_err();
        assert!(err.0.contains("add, over"), "{err}");

        let t = ParamSpec::text("t", "");
        assert!(t.check(&Value::Text("x".repeat(MAX_TEXT + 1))).is_err());
    }

    #[test]
    fn help_and_choice_docs() {
        let ch = ParamSpec::choice("mode", &["add", "over"])
            .doc("How layers mix")
            .choice_doc("over", "the layer in front")
            .choice_doc("add", "summed");
        ch.validate_spec().unwrap();
        assert_eq!(ch.full_doc(), "How layers mix; add: summed; over: the layer in front");
        let h = ch.help();
        assert_eq!(h.type_label, "choice");
        assert_eq!(h.default, "add");
        assert_eq!(h.choices.len(), 2);
        assert!(h.choices[0].default && h.choices[0].doc == "summed");
        assert!(ParamSpec::choice("m", &["a"]).choice_doc("b", "x").validate_spec().is_err());
        let twice = ParamSpec::choice("m", &["a"]).choice_doc("a", "x").choice_doc("a", "y");
        assert!(twice.validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).choice_doc("a", "x").validate_spec().is_err());

        let f = ParamSpec::float("t", 1.0).range(0.0, 10.0).unit(Unit::Seconds).doc("Fade time");
        let h = f.help();
        assert_eq!(h.range, "0 to 10 s");
        assert_eq!((h.clamped, h.unit.as_str(), h.default.as_str()), (false, "s", "1"));
        assert!(h.bindable && h.choices.is_empty());
        assert_eq!(ParamSpec::text("t", "").help().default, "");
    }

    #[test]
    fn special_values() {
        let rate = ParamSpec::float("rate", 0.0).clamp(0.0, 240.0).doc("Frames per second").special_resolved(
            0.0,
            "show rate",
            Resolves::ShowRate,
        );
        rate.validate_spec().unwrap();
        assert_eq!(rate.special_of(0.0).map(|s| s.label.as_str()), Some("show rate"));
        assert!(rate.special_of(30.0).is_none());
        assert_eq!(rate.full_doc(), "Frames per second; 0: show rate");
        let h = rate.help();
        assert_eq!(
            (h.default.as_str(), h.specials.clone()),
            ("0 (show rate)", vec!["0: show rate".to_string()])
        );
        for typed in ["show rate", " Show Rate ", "show rate (60 Hz)"] {
            assert_eq!(rate.special_named(typed).map(|s| s.value), Some(0.0), "{typed}");
        }
        assert!(rate.special_named("show rates").is_none());
        assert!(rate.special_named("30").is_none());

        let threads = ParamSpec::int("threads", 0).clamp(0.0, 64.0).special(0.0, "automatic");
        threads.validate_spec().unwrap();
        assert!(ParamSpec::int("n", 0).special(0.5, "half").validate_spec().is_err(), "an int's is whole");
        assert!(ParamSpec::float("x", 1.0).clamp(1.0, 2.0).special(0.0, "off").validate_spec().is_err());
        assert!(ParamSpec::bool("b", false).special(0.0, "off").validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).special(0.0, "").validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).special(0.0, "the 2nd").validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).special(0.0, "a").special(0.0, "b").validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).special(0.0, "a").special(1.0, "a").validate_spec().is_err());
        assert!(ParamSpec::float("x", 0.0).special(0.0, "a").special(-1.0, "b").validate_spec().is_ok());
    }

    #[test]
    fn binding() {
        let f = ParamSpec::float("x", 0.5).clamp(0.0, 1.0);
        assert_eq!(f.bind_value(&Value::Float(0.5), None, 3.0).unwrap(), Value::Float(1.0));
        assert!(f.check_bindable(Some(0)).is_err());
        let i = ParamSpec::int("n", 0);
        assert_eq!(i.bind_value(&Value::Int(0), None, 2.6).unwrap(), Value::Int(3));
        let b = ParamSpec::bool("on", false);
        assert_eq!(b.bind_value(&Value::Bool(false), None, 0.5).unwrap(), Value::Bool(true));
        assert_eq!(b.bind_value(&Value::Bool(true), None, 0.49).unwrap(), Value::Bool(false));
        let v = ParamSpec::vec2("p", [1.0, 2.0]);
        assert_eq!(v.bind_value(&Value::Vec2([1.0, 2.0]), Some(1), 7.0).unwrap(), Value::Vec2([1.0, 7.0]));
        assert!(v.check_bindable(None).unwrap_err().0.contains("`p.0` to `p.1`"));
        assert!(v.check_bindable(Some(2)).is_err());
        let c = ParamSpec::color("c", [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            c.bind_value(&Value::Vec4([0.0, 0.0, 0.0, 1.0]), Some(0), f64::NAN).unwrap(),
            Value::Vec4([0.0, 0.0, 0.0, 1.0])
        );
        assert!(ParamSpec::choice("m", &["a"]).check_bindable(None).is_err());
        assert!(ParamSpec::text("t", "").check_bindable(None).is_err());
    }

    #[test]
    fn spec_validation() {
        assert!(ParamSpec::float("ok", 1.0).range(0.0, 2.0).validate_spec().is_ok());
        assert!(ParamSpec::float("bad name", 1.0).validate_spec().is_err());
        assert!(ParamSpec::float("x", 1.0).range(2.0, 1.0).validate_spec().is_err());
        assert!(ParamSpec::float("x", 1.0).range(0.0, 2.0).curve(Curve::Log).validate_spec().is_err());
        assert!(ParamSpec::float("x", 5.0).clamp(0.0, 2.0).validate_spec().is_err());
        assert!(ParamSpec::int("x", 1).validate_spec().is_ok());
        assert!(ParamSpec::choice("x", &[]).validate_spec().is_err());
        assert!(ParamSpec::float("x", 1.0).curve(Curve::Power(0.0)).validate_spec().is_err());
        let mut v = ParamSpec::vec2("x", [0.0, 0.0]);
        v.kind = ParamKind::Vec(5);
        assert!(v.validate_spec().is_err());
    }

    #[test]
    fn slider_curves_round_trip() {
        let specs = [
            ParamSpec::float("lin", 1.0).range(0.0, 10.0),
            ParamSpec::float("log", 1.0).range(0.1, 1000.0).curve(Curve::Log),
            ParamSpec::float("pow", 1.0).range(0.0, 10.0).curve(Curve::Power(2.0)),
        ];
        for spec in &specs {
            for s in [0.0, 0.25, 0.5, 1.0] {
                let v = spec.from_slider(s).unwrap();
                assert!((spec.to_slider(v).unwrap() - s).abs() < 1e-12, "{} {s}", spec.name);
            }
        }
        let log = &specs[1];
        assert!((log.from_slider(0.5).unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(ParamSpec::float("x", 0.0).to_slider(1.0), None);
        assert_eq!(specs[0].to_slider(-5.0), Some(0.0));
        assert_eq!(specs[0].from_slider(f64::NAN), Some(0.0));
    }

    #[test]
    fn groups_in_panel_order() {
        let specs = [
            ParamSpec::float("a", 0.0),
            ParamSpec::float("b", 0.0).more(),
            ParamSpec::float("c", 0.0).group("camera"),
            ParamSpec::float("d", 0.0).group("levels"),
            ParamSpec::float("e", 0.0).group("camera"),
        ];
        assert_eq!(groups(&specs), ["camera", "levels", "more"]);
        assert!(ParamSpec::float("x", 0.0).group("light").validate_spec().is_ok());
        assert!(ParamSpec::float("x", 0.0).group(" light").validate_spec().is_err());
        assert!(
            ParamSpec::float("x", 0.0)
                .group(
                    "a
b"
                )
                .validate_spec()
                .is_err()
        );
        assert!(ParamSpec::float("x", 0.0).group(&"g".repeat(MAX_GROUP + 1)).validate_spec().is_err());
    }

    #[test]
    fn palettes_take_names_hex_and_numbers() {
        let p = ParamSpec::palette("look", "blood");
        p.validate_spec().unwrap();
        assert_eq!(p.type_label(), "palette");
        assert_eq!(p.check(&Value::Text("anything".into())), Ok(Value::Text("anything".into())));
        assert_eq!(p.check(&Value::Text("#31aaa9 f8e0a4".into())), Ok(Value::Text("#31aaa9 f8e0a4".into())));
        assert!(p.check(&Value::Text("#31aaa9 nope".into())).is_err());
        assert!(p.check(&Value::Text("x".repeat(crate::palette::MAX_VALUE + 1))).is_err());
        assert_eq!(p.check(&Value::Float(3.0)), Ok(Value::Int(3)));
        assert!(p.check(&Value::Bool(true)).is_err());
        assert_eq!(p.bind_components(), 1);
        assert_eq!(p.bind_value(&p.default, None, 2.6), Ok(Value::Int(3)));
        assert!(p.help().bindable);
    }
}
