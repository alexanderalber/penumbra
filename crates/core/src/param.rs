//! Parameter metadata.
//!
//! Every node declares its parameters as [`ParamSpec`]s: type, default, range, unit, slider curve,
//! a "rarely used" flag that collapses the parameter in the UI, and a one-line doc. The spec is the
//! single source for validation (every edit passes [`ParamSpec::check`]), for the param panel and
//! for what `node.info` reports.

use serde::{Deserialize, Serialize};

use crate::{Error, Name, Value};

/// Longest accepted text value in bytes.
pub const MAX_TEXT: usize = 64 * 1024;
/// Longest accepted file path in bytes.
pub const MAX_FILE: usize = 4096;

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
    /// Collapsed in the param panel until expanded.
    pub rarely_used: bool,
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
    pub doc: String,
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
            rarely_used: false,
            multiline: false,
            lines: 0,
            node_ref: false,
            doc: String::new(),
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

    pub fn rarely_used(mut self) -> Self {
        self.rarely_used = true;
        self
    }

    pub fn doc(mut self, doc: &str) -> Self {
        self.doc = doc.to_string();
        self
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

    /// A text parameter naming another node ([`ParamSpec::node_ref`]).
    pub fn node(name: &str) -> Self {
        let mut spec = Self::new(name, ParamKind::Text, Value::Text(String::new()));
        spec.node_ref = true;
        spec
    }

    /// Number of components a channel can drive one at a time: 1 for bool, int and float
    /// (bound as a whole), 2 to 4 for vectors and colours (bound per component), 0 for choices,
    /// text and files, which channels cannot drive.
    pub fn bind_components(&self) -> u8 {
        match self.kind {
            ParamKind::Bool | ParamKind::Int | ParamKind::Float => 1,
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
            (ParamKind::Int, _) => Value::Int(x.round() as i64),
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
        match &self.kind {
            ParamKind::Vec(n) if !(2..=4).contains(n) => {
                return Err(Error::new(format!("vec size {n} must be 2 to 4")).context(ctx()));
            }
            ParamKind::Choice(c) if c.is_empty() => {
                return Err(Error::new("a choice needs at least one entry").context(ctx()));
            }
            _ => {}
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
}
