//! Parameter values.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The value of one parameter. Serialised untagged, so JSON and RON read naturally:
/// `true`, `3`, `0.5`, `[1, 0, 0, 1]`, `"text"`. What a value means (a colour, a file, a choice)
/// is said by the parameter's [`crate::ParamSpec`], which also coerces (an integer where a float
/// is expected) and clamps.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Vec4([f64; 4]),
    Text(String),
}

impl Value {
    /// Short type name for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Vec2(_) => "vec2",
            Value::Vec3(_) => "vec3",
            Value::Vec4(_) => "vec4",
            Value::Text(_) => "text",
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// A float or an int as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The components of a vector value, or a single number.
    pub fn components(&self) -> Option<&[f64]> {
        match self {
            Value::Float(f) => Some(std::slice::from_ref(f)),
            Value::Vec2(v) => Some(v),
            Value::Vec3(v) => Some(v),
            Value::Vec4(v) => Some(v),
            _ => None,
        }
    }
}

/// Written by hand rather than derived so vectors are sequences in every format: a derived
/// fixed-size array is a tuple, which RON writes as `(1.0, 2.0)` instead of `[1.0, 2.0]`.
impl Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i64(*i),
            Value::Float(f) => s.serialize_f64(*f),
            Value::Vec2(v) => s.collect_seq(v),
            Value::Vec3(v) => s.collect_seq(v),
            Value::Vec4(v) => s.collect_seq(v),
            Value::Text(t) => s.serialize_str(t),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Vec2([a, b]) => write!(f, "[{a}, {b}]"),
            Value::Vec3([a, b, c]) => write!(f, "[{a}, {b}, {c}]"),
            Value::Vec4([a, b, c, d]) => write!(f, "[{a}, {b}, {c}, {d}]"),
            Value::Text(s) => write!(f, "{s:?}"),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_untagged() {
        let cases = [
            ("true", Value::Bool(true)),
            ("3", Value::Int(3)),
            ("0.5", Value::Float(0.5)),
            ("[1, 2]", Value::Vec2([1.0, 2.0])),
            ("[1, 0, 0]", Value::Vec3([1.0, 0.0, 0.0])),
            ("[1, 0, 0, 1]", Value::Vec4([1.0, 0.0, 0.0, 1.0])),
            ("\"hi\"", Value::Text("hi".into())),
        ];
        for (text, value) in cases {
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), value, "{text}");
            let back = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&back).unwrap(), value, "{back}");
        }
        assert!(serde_json::from_str::<Value>("[1, 2, 3, 4, 5]").is_err());
        assert!(serde_json::from_str::<Value>("{}").is_err());
        assert!(serde_json::from_str::<Value>("null").is_err());
    }

    #[test]
    fn accessors() {
        assert_eq!(Value::Int(2).as_f64(), Some(2.0));
        assert_eq!(Value::Float(2.0).as_i64(), None);
        assert_eq!(Value::Vec2([1.0, 2.0]).components(), Some(&[1.0, 2.0][..]));
        assert_eq!(Value::Text("x".into()).to_string(), "\"x\"");
    }
}
