//! The values bound parameters take, kept up to date in place.
//!
//! The cooker resolves every binding of a node on every frame. A text that did not change must
//! not cost an allocation then, so the values in effect are compared with what the bindings
//! give now and only a changed value is written, into the old text's buffer where there is one.

use std::collections::BTreeMap;

use penumbra_core::{Name, NumberFormat, Value};

/// What a binding reads from its source now.
#[derive(Clone, Copy)]
pub(crate) enum BindRead<'a> {
    /// A channel's value.
    Number(f64),
    /// A table cell.
    Text(&'a str),
}

/// A text from a table for a message: its first 40 characters.
pub(crate) fn short(text: &str) -> String {
    match text.char_indices().nth(40) {
        Some((i, _)) => format!("{}...", text[..i].escape_debug()),
        None => text.escape_debug().to_string(),
    }
}

/// What one bound parameter comes out as now.
pub(crate) enum Resolved<'a> {
    /// A number, vector, colour or bool.
    Value(Value),
    /// A text: a table cell, or a choice's entry.
    Text(&'a str),
    /// A channel's number written as text.
    Formatted(f64, &'a NumberFormat),
    /// The value in effect stays (a choice named by a text it does not have).
    Keep,
    /// The binding gives nothing: the stored value applies.
    Stored,
}

/// Puts `r` into `bound` as the value of `name`; whether the value in effect changed.
pub(crate) fn apply(bound: &mut BTreeMap<Name, Value>, name: &Name, r: Resolved<'_>) -> bool {
    if same(bound.get(name), &r) {
        return false;
    }
    match r {
        Resolved::Keep => false,
        Resolved::Stored => bound.remove(name).is_some(),
        Resolved::Value(v) => {
            match bound.get_mut(name) {
                Some(old) => *old = v,
                None => {
                    bound.insert(name.clone(), v);
                }
            }
            true
        }
        Resolved::Text(t) => {
            match bound.get_mut(name) {
                Some(Value::Text(old)) => {
                    old.clear();
                    old.push_str(t);
                }
                Some(old) => *old = Value::Text(t.to_string()),
                None => {
                    bound.insert(name.clone(), Value::Text(t.to_string()));
                }
            }
            true
        }
        Resolved::Formatted(x, format) => {
            match bound.get_mut(name) {
                Some(Value::Text(old)) => {
                    old.clear();
                    // Writing into a String does not fail.
                    let _ = format.write(x, old);
                }
                Some(old) => *old = Value::Text(format.to_text(x)),
                None => {
                    bound.insert(name.clone(), Value::Text(format.to_text(x)));
                }
            }
            true
        }
    }
}

/// Whether `r` leaves the value in effect, `seen` (`None`: the stored one), as it is.
pub(crate) fn same(seen: Option<&Value>, r: &Resolved<'_>) -> bool {
    match (r, seen) {
        (Resolved::Keep, _) => true,
        (Resolved::Stored, seen) => seen.is_none(),
        (Resolved::Value(v), Some(seen)) => v == seen,
        (Resolved::Text(t), Some(Value::Text(seen))) => t == seen,
        (Resolved::Formatted(x, format), Some(Value::Text(seen))) => format.writes(*x, seen),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_values_stay_and_changed_ones_are_written() {
        let name = Name::new("label").unwrap();
        let mut bound = BTreeMap::new();
        assert!(apply(&mut bound, &name, Resolved::Text("hello")));
        assert!(!apply(&mut bound, &name, Resolved::Text("hello")));
        assert!(!apply(&mut bound, &name, Resolved::Keep));
        let format = NumberFormat::parse("{.1} m").unwrap();
        assert!(apply(&mut bound, &name, Resolved::Formatted(2.04, &format)));
        assert_eq!(bound[&name], Value::Text("2.0 m".into()));
        assert!(!apply(&mut bound, &name, Resolved::Formatted(1.96, &format)));
        assert!(apply(&mut bound, &name, Resolved::Value(Value::Float(1.0))));
        assert!(!apply(&mut bound, &name, Resolved::Value(Value::Float(1.0))));
        assert!(apply(&mut bound, &name, Resolved::Stored));
        assert!(bound.is_empty());
        assert!(!apply(&mut bound, &name, Resolved::Stored));
        // Keeping with nothing in effect leaves the stored value.
        assert!(!apply(&mut bound, &name, Resolved::Keep));
        assert!(bound.is_empty());
    }
}
