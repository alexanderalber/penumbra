//! What a binding reads besides a channel, and how a value becomes text or a choice.
//!
//! A binding drives a param from another node. It reads one of two things there:
//!
//! - a channel of the first `chan` output: a number, the last sample;
//! - a cell of the first `table` output ([`Cell`]): a text.
//!
//! The param turns that into its own value. A number param takes a number as it is and reads a
//! cell as a number. A text param takes a cell as it is and writes a number by a
//! [`NumberFormat`]. A choice param picks its entry by a cell's text ([`choice_named`]) or by a
//! number as the entry's place ([`choice_at`]).

use std::fmt::{self, Write};

use serde::{Deserialize, Serialize};

use crate::{Error, Table};

/// Longest header text a [`Cell`] may name, in bytes.
pub const MAX_HEADER: usize = 256;
/// Longest [`NumberFormat`] text, in bytes.
pub const MAX_FORMAT: usize = 256;
/// Digits after the point when the format does not say: at most this many, trailing zeros
/// dropped.
pub const DEFAULT_DIGITS: u8 = 3;
/// Most digits after the point a format may ask for.
pub const MAX_DIGITS: u8 = 9;

/// A row or a column of a table: by its place, counted from 0, or by its header. A row's header
/// is its first cell; a column's header is its cell in the first row. Headers compare with
/// surrounding spaces aside.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Index {
    At(u32),
    Named(String),
}

impl Default for Index {
    fn default() -> Self {
        Index::At(0)
    }
}

impl Index {
    /// A header text, checked: not empty, at most [`MAX_HEADER`] bytes, no control characters.
    pub fn named(text: &str) -> Result<Self, Error> {
        let t = text.trim();
        if t.is_empty() {
            return Err(Error::new("a row or column header cannot be empty"));
        }
        if t.len() > MAX_HEADER || t.chars().any(char::is_control) {
            return Err(Error::new(format!(
                "a row or column header is at most {MAX_HEADER} bytes, without control characters"
            )));
        }
        Ok(Index::Named(t.to_string()))
    }
}

impl fmt::Display for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Index::At(i) => write!(f, "{i}"),
            Index::Named(n) => write!(f, "\"{}\"", n.escape_debug()),
        }
    }
}

impl Serialize for Index {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Index::At(i) => s.serialize_u32(*i),
            Index::Named(n) => s.serialize_str(n),
        }
    }
}

impl<'de> Deserialize<'de> for Index {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Index;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a row or column: a place counted from 0, or a header text")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Index, E> {
                u32::try_from(v)
                    .map(Index::At)
                    .map_err(|_| E::custom(format!("row or column {v} is too large")))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Index, E> {
                u32::try_from(v)
                    .map(Index::At)
                    .map_err(|_| E::custom(format!("row or column {v}: places count from 0")))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Index, E> {
                Index::named(v).map_err(|e| E::custom(e.0))
            }
        }
        d.deserialize_any(V)
    }
}

/// One cell of a table, by row and column; a missing one is 0.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    #[serde(default)]
    pub row: Index,
    #[serde(default)]
    pub col: Index,
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "row {}, column {}", self.row, self.col)
    }
}

impl Cell {
    /// The text of this cell in `table`; why there is none (a sentence fragment for a warning).
    /// Does not allocate when the cell is found.
    pub fn find<'t>(&self, table: &'t Table) -> Result<&'t str, String> {
        let rows = table.rows();
        let row = match &self.row {
            Index::At(r) => usize::try_from(*r)
                .ok()
                .filter(|r| *r < rows.len())
                .ok_or_else(|| format!("the table has no row {r} (it has {} rows)", rows.len()))?,
            Index::Named(name) => rows
                .iter()
                .position(|r| r.first().is_some_and(|c| c.trim() == name))
                .ok_or_else(|| format!("no row starts with \"{}\"", name.escape_debug()))?,
        };
        let col = match &self.col {
            Index::At(c) => usize::try_from(*c).unwrap_or(usize::MAX),
            Index::Named(name) => rows
                .first()
                .and_then(|h| h.iter().position(|c| c.trim() == name))
                .ok_or_else(|| format!("no column is headed \"{}\"", name.escape_debug()))?,
        };
        rows.get(row)
            .and_then(|r| r.get(col))
            .map(String::as_str)
            .ok_or_else(|| format!("row {row} of the table has no column {col}"))
    }
}

/// How a number becomes text: a template with one placeholder for the number. `{}` writes it
/// with up to [`DEFAULT_DIGITS`] digits after the point and drops trailing zeros (`3`, `0.5`,
/// `0.123`); `{.2}` writes exactly two (`0.50`), `{.0}` none. Text around the placeholder is
/// written as it is: `{.1} m`, `Round {}`, `{.0}%`. `{{` and `}}` stand for a brace. A number
/// that rounds to zero is written without a minus sign.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NumberFormat {
    /// As written; empty for `{}`, so the default needs no allocation.
    text: String,
    prefix: String,
    /// `None` for `{}`.
    digits: Option<u8>,
    suffix: String,
}

/// `{}`, what a channel bound into a text param is written by when the binding names no format.
pub static DEFAULT_FORMAT: NumberFormat = NumberFormat::DEFAULT;

impl Default for NumberFormat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for NumberFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl NumberFormat {
    /// `{}`.
    pub const DEFAULT: NumberFormat =
        NumberFormat { text: String::new(), prefix: String::new(), digits: None, suffix: String::new() };

    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_FORMAT || text.chars().any(|c| c.is_control()) {
            return Err(Error::new(format!(
                "a number format is at most {MAX_FORMAT} bytes on one line, without control characters"
            )));
        }
        let wrong = |why: &str| {
            Error::new(format!(
                "number format `{text}`: {why}; write one placeholder, `{{}}` or `{{.2}}` for two \
                 digits after the point, with any text around it (`{{{{` and `}}}}` for a brace)"
            ))
        };
        let mut prefix = String::new();
        let mut suffix = String::new();
        let mut digits = None;
        let mut placed = false;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            let out = if placed { &mut suffix } else { &mut prefix };
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    out.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    out.push('}');
                }
                '}' => return Err(wrong("a `}` without its `{`")),
                '{' if placed => return Err(wrong("more than one placeholder")),
                '{' => {
                    let mut inner = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => inner.push(c),
                            None => return Err(wrong("a `{` without its `}`")),
                        }
                    }
                    digits = match inner.as_str() {
                        "" => None,
                        d => match d
                            .strip_prefix('.')
                            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                            .map(|n| n.parse::<u8>().unwrap_or(u8::MAX))
                        {
                            Some(n) if n <= MAX_DIGITS => Some(n),
                            Some(_) => {
                                return Err(wrong(&format!("at most {MAX_DIGITS} digits after the point")));
                            }
                            None => return Err(wrong(&format!("`{{{d}}}` is no placeholder"))),
                        },
                    };
                    placed = true;
                }
                c => out.push(c),
            }
        }
        if !placed {
            return Err(wrong("no placeholder for the number"));
        }
        let text = if text == "{}" { String::new() } else { text.to_string() };
        Ok(Self { text, prefix, digits, suffix })
    }

    /// The format as written.
    pub fn as_str(&self) -> &str {
        if self.text.is_empty() { "{}" } else { &self.text }
    }

    /// Writes `x` by this format; a value that is not finite is written as 0.
    pub fn write(&self, x: f64, out: &mut impl Write) -> fmt::Result {
        out.write_str(&self.prefix)?;
        let x = if x.is_finite() { x } else { 0.0 };
        match self.digits {
            Some(d) => {
                // `-0.00` reads as a fault on a screen; a number that rounds to zero is zero.
                let x = if (x.abs() * 10f64.powi(i32::from(d))).round() == 0.0 { 0.0 } else { x };
                let d = usize::from(d);
                write!(out, "{x:.d$}")?;
            }
            None => {
                let d = usize::from(DEFAULT_DIGITS);
                let x =
                    if (x.abs() * 10f64.powi(i32::from(DEFAULT_DIGITS))).round() == 0.0 { 0.0 } else { x };
                // The digits go through a buffer on the stack so trailing zeros can be dropped
                // without allocating. An f64 has at most 309 digits before the point.
                let mut buf = Stack::<384>::new();
                write!(buf, "{x:.d$}")?;
                let s = buf.as_str();
                let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s };
                out.write_str(s)?;
            }
        }
        out.write_str(&self.suffix)
    }

    /// Whether writing `x` by this format gives `text`. Does not allocate.
    pub fn writes(&self, x: f64, text: &str) -> bool {
        let mut same = Same { rest: text, same: true };
        self.write(x, &mut same).is_ok() && same.same && same.rest.is_empty()
    }

    /// `x` written by this format.
    pub fn to_text(&self, x: f64) -> String {
        let mut s = String::new();
        // Writing into a String does not fail.
        let _ = self.write(x, &mut s);
        s
    }
}

impl Serialize for NumberFormat {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NumberFormat {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        NumberFormat::parse(&text).map_err(|e| serde::de::Error::custom(e.0))
    }
}

/// Compares what is written with a text, piece by piece.
struct Same<'a> {
    rest: &'a str,
    same: bool,
}

impl Write for Same<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        match self.rest.strip_prefix(s) {
            Some(rest) if self.same => self.rest = rest,
            _ => self.same = false,
        }
        Ok(())
    }
}

/// A fixed buffer on the stack; writing past its end fails.
struct Stack<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Stack<N> {
    fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }

    fn as_str(&self) -> &str {
        // Only whole `str`s are copied in, so the bytes are UTF-8.
        self.buf.get(..self.len).and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("")
    }
}

impl<const N: usize> Write for Stack<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        let slot = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
        slot.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The choice entry a text names: the entry equal to the text with surrounding spaces and ASCII
/// case aside.
pub fn choice_named<'c>(choices: &'c [String], text: &str) -> Option<&'c str> {
    let t = text.trim();
    choices.iter().find(|c| c.eq_ignore_ascii_case(t)).map(String::as_str)
}

/// The choice entry a number picks: its place counted from 0, rounded and clamped to the
/// entries (a value that is not finite picks the first). `None` only without entries.
pub fn choice_at(choices: &[String], x: f64) -> Option<&str> {
    let last = choices.len().checked_sub(1)?;
    let x = if x.is_finite() { x.round() } else { 0.0 };
    // `as` saturates; a negative number picks the first entry.
    let i = (x.max(0.0) as usize).min(last);
    choices.get(i).map(String::as_str)
}

/// A number read from a table cell: the text with surrounding spaces aside.
pub fn cell_number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(t: &str) -> NumberFormat {
        NumberFormat::parse(t).unwrap()
    }

    #[test]
    fn formats() {
        assert_eq!(fmt("{}").to_text(3.0), "3");
        assert_eq!(fmt("{}").to_text(0.5), "0.5");
        assert_eq!(fmt("{}").to_text(0.123456), "0.123");
        assert_eq!(fmt("{}").to_text(-0.0001), "0");
        assert_eq!(fmt("{}").to_text(1200.0), "1200");
        assert_eq!(fmt("{}").to_text(f64::NAN), "0");
        assert_eq!(fmt("{}").to_text(f64::MAX).len(), 309);
        assert_eq!(fmt("{.2}").to_text(0.5), "0.50");
        assert_eq!(fmt("{.2}").to_text(-0.001), "0.00");
        assert_eq!(fmt("{.0}%").to_text(42.6), "43%");
        assert_eq!(fmt("{.1} \u{b0}C").to_text(21.04), "21.0 \u{b0}C");
        assert_eq!(fmt("Round {}").to_text(7.0), "Round 7");
        assert_eq!(fmt("{{{}}}").to_text(1.0), "{1}");
        assert_eq!(NumberFormat::default().to_text(2.25), "2.25");
        assert_eq!((fmt("{}"), NumberFormat::default().as_str()), (DEFAULT_FORMAT.clone(), "{}"));
        assert_eq!(fmt("x = {.9}").to_text(1.0), "x = 1.000000000");
    }

    #[test]
    fn bad_formats_are_refused() {
        for (t, why) in [
            ("no number", "no placeholder"),
            ("{} {}", "more than one"),
            ("{.12}", "at most 9"),
            ("{:3}", "no placeholder"),
            ("{.x}", "no placeholder"),
            ("{.+2}", "no placeholder"),
            ("{", "without its `}`"),
            ("}", "without its `{`"),
            ("a\nb {}", "one line"),
        ] {
            let e = NumberFormat::parse(t).unwrap_err().0;
            assert!(e.contains(why), "{t}: {e}");
        }
        assert!(NumberFormat::parse(&"x".repeat(300)).is_err());
    }

    #[test]
    fn writes_compares_without_writing() {
        let f = fmt("{.1} m");
        assert!(f.writes(2.04, "2.0 m"));
        assert!(!f.writes(2.06, "2.0 m"));
        assert!(!f.writes(2.04, "2.0 m "));
        assert!(!f.writes(2.04, "2.0"));
        assert!(fmt("{}").writes(0.5, "0.5"));
        assert!(!fmt("{}").writes(0.5, "0.50"));
    }

    #[test]
    fn choices_by_text_and_number() {
        let c: Vec<String> = ["over", "add", "multiply"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choice_named(&c, " ADD "), Some("add"));
        assert_eq!(choice_named(&c, "Multiply"), Some("multiply"));
        assert_eq!(choice_named(&c, "screen"), None);
        assert_eq!(choice_named(&c, ""), None);
        assert_eq!(choice_at(&c, 0.0), Some("over"));
        assert_eq!(choice_at(&c, 1.4), Some("add"));
        assert_eq!(choice_at(&c, 1.6), Some("multiply"));
        assert_eq!(choice_at(&c, 99.0), Some("multiply"));
        assert_eq!(choice_at(&c, -3.0), Some("over"));
        assert_eq!(choice_at(&c, f64::NAN), Some("over"));
        assert_eq!(choice_at(&[], 1.0), None);
    }

    #[test]
    fn cells_by_place_and_header() {
        let t = Table::parse("name,score\nada,3\nbob, 5 \n", crate::table::Format::Csv).unwrap();
        let cell = |row: Index, col: Index| Cell { row, col }.find(&t);
        assert_eq!(cell(Index::At(1), Index::At(0)), Ok("ada"));
        assert_eq!(cell(Index::Named("bob".into()), Index::Named("score".into())), Ok(" 5 "));
        assert_eq!(cell(Index::At(0), Index::Named("score".into())), Ok("score"));
        assert!(cell(Index::At(9), Index::At(0)).unwrap_err().contains("no row 9"));
        assert!(cell(Index::At(1), Index::At(7)).unwrap_err().contains("no column 7"));
        assert!(cell(Index::Named("eve".into()), Index::At(0)).unwrap_err().contains("eve"));
        assert!(cell(Index::At(0), Index::Named("age".into())).unwrap_err().contains("age"));
        assert_eq!(cell_number(" 5 "), Some(5.0));
        assert_eq!(cell_number("five"), None);
        assert_eq!(cell_number("inf"), None);
    }

    #[test]
    fn indices_read_as_places_or_headers() {
        let i: Index = serde_json::from_str("3").unwrap();
        assert_eq!(i, Index::At(3));
        let i: Index = serde_json::from_str("\" score \"").unwrap();
        assert_eq!(i, Index::Named("score".into()));
        assert!(serde_json::from_str::<Index>("-1").is_err());
        assert!(serde_json::from_str::<Index>("\"\"").is_err());
        assert!(serde_json::from_str::<Index>("1.5").is_err());
        assert_eq!(serde_json::to_string(&Index::Named("a".into())).unwrap(), "\"a\"");
        let f: NumberFormat = serde_json::from_str("\"{.1} m\"").unwrap();
        assert_eq!(serde_json::to_string(&f).unwrap(), "\"{.1} m\"");
        assert!(serde_json::from_str::<NumberFormat>("\"m\"").is_err());
    }
}
