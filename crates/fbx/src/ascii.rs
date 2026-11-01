//! The ASCII form of FBX 7.x, read into the same node tree as the binary form.
//!
//! An ASCII file is a list of records. A record is `Name:`, then values separated by commas,
//! then optionally its child records in `{ }`. Values are numbers, quoted strings and bare
//! words (`Y`, `T`). An array is written `*N { a: v, v, ... }` with `N` its claimed length.
//! `;` starts a comment that runs to the end of the line. Strings escape `"` as `&quot;`, and
//! line breaks as `&lf;` and `&cr;`. Embedded files (`Content`) are base64 strings, sometimes
//! after a leading comma (`Content: , "iVBOR..."`). Object names read `Class::Name`.
//!
//! The text carries no types, so they are chosen here. A whole number is an `I64`, any other
//! number an `F64`. An array holds `I32` while every element fits, then `I64`, and `F64` as soon
//! as one element has a point or an exponent. `KeyValueFloat` and `KeyAttrDataFloat` are
//! `F32`, parsed straight to 32 bits as the binary form stores them. From version 7200 on,
//! `KeyAttrDataFloat` holds the integers of its floats' bits (it packs weights into them). A bare word of one letter is a `Bool` holding that letter, as
//! the binary `C` value does. `nan`, `inf` and the `1.#INF`, `-1.#IND` forms of old writers are
//! floats. Any other word inside an array counts as the code of its first letter, as ufbx
//! reads it.
//!
//! The version comes from `FBXHeaderExtension` / `FBXVersion`, else from the `; FBX 7.4.0`
//! comment that starts the file. Files below 7000 are refused.
//!
//! The format details follow ufbx's ASCII parser (string escapes, number tokens, the leading
//! comma).

use crate::tree::{Array, Node, Tree, Value};
use crate::{Error, Limits};

/// Arrays whose elements are 32-bit floats in the binary form.
const F32_ARRAYS: &[&str] = &["KeyValueFloat", "KeyAttrDataFloat"];

/// From this version on, `KeyAttrDataFloat` is written as the integers of its floats' bits:
/// some of its elements are packed integers, not floats.
const BITS_SINCE: u32 = 7200;

/// Records the readers take as arrays. A file may write them as a bare list (`Indexes: 1,2,3`)
/// instead of `*N { a: ... }`; such a list becomes one array value.
const ARRAY_RECORDS: &[&str] = &[
    "Vertices",
    "PolygonVertexIndex",
    "Edges",
    "Normals",
    "NormalsW",
    "NormalsIndex",
    "Binormals",
    "BinormalsW",
    "Tangents",
    "TangentsW",
    "UV",
    "UVIndex",
    "Colors",
    "ColorIndex",
    "Materials",
    "Smoothing",
    "KeyTime",
    "KeyValueFloat",
    "KeyAttrFlags",
    "KeyAttrDataFloat",
    "KeyAttrRefCount",
    "Indexes",
    "Weights",
    "BlendWeights",
    "FullWeights",
    "Transform",
    "TransformLink",
    "TransformAssociateModel",
    "Matrix",
];

/// Bytes an array element is charged against [`Limits::max_array_bytes`], whatever its type
/// ends up as.
const ELEMENT_BYTES: u64 = 8;

/// Elements reserved up front for an array, whatever its `*N` claims.
const MAX_RESERVE: usize = 4096;

/// The version a `; FBX 7.4.0 project file` comment at the start names (7400), if any.
pub fn header_version(bytes: &[u8]) -> Option<u32> {
    let s = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let start = s.iter().position(|c| !c.is_ascii_whitespace())?;
    let s = s.get(start..)?.strip_prefix(b";")?;
    let start = s.iter().position(|c| *c != b' ' && *c != b'\t')?;
    let s = s.get(start..)?.strip_prefix(b"FBX")?;
    let start = s.iter().position(|c| *c != b' ' && *c != b'\t')?;
    let s = s.get(start..)?;
    match s {
        [a, b'.', b, b'.', c, ..] if a.is_ascii_digit() && b.is_ascii_digit() && c.is_ascii_digit() => {
            Some(1000 * u32::from(a - b'0') + 100 * u32::from(b - b'0') + 10 * u32::from(c - b'0'))
        }
        _ => None,
    }
}

fn too_old(version: u32) -> Error {
    Error::new(format!(
        "an FBX 6 file (version {version}); only FBX 7 (version 7000 and later) is read. Export it again \
         as FBX 2011 or later"
    ))
}

/// Reads the node tree of an ASCII FBX file.
pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Tree, Error> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(Error::new(format!("larger than {} MiB", limits.max_file_bytes >> 20)));
    }
    let comment = header_version(bytes);
    if let Some(v) = comment.filter(|v| *v < 7000) {
        return Err(too_old(v));
    }
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut p = Parser {
        s: text,
        pos: 0,
        tok: Tok::End,
        at: 0,
        limits,
        nodes: 0,
        values: 0,
        array_bytes: 0,
        version: comment.unwrap_or(0),
    };
    p.advance()?;
    let nodes = p.records(0)?;
    if !matches!(p.tok, Tok::End) {
        return Err(p.error("a `}` closes a record that was never opened"));
    }
    let stated = nodes
        .iter()
        .find(|n| n.name == "FBXHeaderExtension")
        .and_then(|h| h.child("FBXVersion"))
        .and_then(|v| v.value(0))
        .and_then(Value::as_i64);
    let version = match (stated, comment) {
        (Some(v), _) => u32::try_from(v).map_err(|_| Error::new(format!("FBX version {v} is not known")))?,
        (None, Some(v)) => v,
        (None, None) => {
            return Err(Error::new("not an FBX file: no `FBXHeaderExtension` with an `FBXVersion`"));
        }
    };
    if version < 7000 {
        return Err(too_old(version));
    }
    if version >= 10000 {
        return Err(Error::new(format!("FBX version {version} is not known")));
    }
    Ok(Tree { version, nodes })
}

#[derive(Clone, Debug)]
enum Tok<'a> {
    /// `Name:` or `"Name":`.
    Name(String),
    Str(Vec<u8>),
    Number(&'a [u8]),
    Word(&'a [u8]),
    Star,
    Comma,
    Open,
    Close,
    End,
}

struct Parser<'a> {
    s: &'a [u8],
    /// Where lexing goes on, after the current token.
    pos: usize,
    /// The current token and the byte it starts at.
    tok: Tok<'a>,
    at: usize,
    limits: &'a Limits,
    nodes: usize,
    values: usize,
    array_bytes: u64,
    /// The version as far as known: from `FBXVersion` once read, else from the comment.
    version: u32,
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'(' | b')')
}

impl<'a> Parser<'a> {
    fn line(&self, at: usize) -> usize {
        1 + self.s.get(..at).unwrap_or(self.s).iter().filter(|c| **c == b'\n').count()
    }

    /// An error at the current token, with its line.
    fn error(&self, what: impl std::fmt::Display) -> Error {
        Error::new(format!("{what} (line {})", self.line(self.at)))
    }

    fn byte(&self, i: usize) -> Option<u8> {
        self.s.get(i).copied()
    }

    /// Skips white space and comments from `self.pos`.
    fn skip_blank(&mut self) {
        while let Some(c) = self.byte(self.pos) {
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else if c == b';' {
                let rest = self.s.get(self.pos..).unwrap_or(&[]);
                self.pos += rest.iter().position(|c| *c == b'\n').unwrap_or(rest.len());
            } else {
                break;
            }
        }
    }

    /// Moves to the next token.
    fn advance(&mut self) -> Result<(), Error> {
        self.skip_blank();
        self.at = self.pos;
        let Some(c) = self.byte(self.pos) else {
            self.tok = Tok::End;
            return Ok(());
        };
        let start = self.pos;
        self.tok = if is_word_start(c) {
            while self.byte(self.pos).is_some_and(is_word) {
                self.pos += 1;
            }
            let word = self.s.get(start..self.pos).unwrap_or(&[]);
            let after = self.pos;
            self.skip_blank_no_comment();
            if self.byte(self.pos) == Some(b':') {
                self.pos += 1;
                Tok::Name(String::from_utf8_lossy(word).into_owned())
            } else {
                self.pos = after;
                Tok::Word(word)
            }
        } else if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') {
            while self.byte(self.pos).is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(&c)) {
                self.pos += 1;
            }
            // `1.#INF`, `-1.#IND00`, `-nan(ind)`.
            while self.byte(self.pos).is_some_and(|c| c.is_ascii_alphanumeric() || b"#()".contains(&c)) {
                self.pos += 1;
            }
            Tok::Number(self.s.get(start..self.pos).unwrap_or(&[]))
        } else if c == b'"' {
            let s = self.string()?;
            if self.byte(self.pos) == Some(b':') {
                self.pos += 1;
                Tok::Name(String::from_utf8_lossy(&s).into_owned())
            } else {
                Tok::Str(s)
            }
        } else {
            self.pos += 1;
            match c {
                b',' => Tok::Comma,
                b'{' => Tok::Open,
                b'}' => Tok::Close,
                b'*' => Tok::Star,
                other => {
                    return Err(self.error(if other.is_ascii_graphic() {
                        format!("unexpected character `{}`", char::from(other))
                    } else {
                        format!("unexpected byte 0x{other:02x}")
                    }));
                }
            }
        };
        Ok(())
    }

    fn skip_blank_no_comment(&mut self) {
        while self.byte(self.pos).is_some_and(|c| c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    /// A quoted string from `self.pos` (at its `"`), unescaped.
    fn string(&mut self) -> Result<Vec<u8>, Error> {
        let rest = self.s.get(self.pos + 1..).unwrap_or(&[]);
        let Some(len) = rest.iter().position(|c| *c == b'"') else {
            return Err(self.error("a string has no closing quote"));
        };
        let raw = rest.get(..len).unwrap_or(&[]);
        self.pos += len + 2;
        if !raw.contains(&b'&') {
            return Ok(raw.to_vec());
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while let Some(&c) = raw.get(i) {
            let tail = raw.get(i..).unwrap_or(&[]);
            let (with, skip) = if c != b'&' {
                (c, 1)
            } else if tail.starts_with(b"&quot;") {
                (b'"', 6)
            } else if tail.starts_with(b"&lf;") {
                (b'\n', 4)
            } else if tail.starts_with(b"&cr;") {
                (b'\r', 4)
            } else {
                (b'&', 1)
            };
            out.push(with);
            i += skip;
        }
        Ok(out)
    }

    /// Records until a `}` or the end of the file, which the caller checks.
    fn records(&mut self, depth: usize) -> Result<Vec<Node>, Error> {
        let mut out = Vec::new();
        loop {
            match std::mem::replace(&mut self.tok, Tok::End) {
                Tok::Name(name) => {
                    self.advance()?;
                    out.push(self.record(name, depth)?);
                }
                t @ (Tok::Close | Tok::End) => {
                    self.tok = t;
                    return Ok(out);
                }
                other => {
                    self.tok = other;
                    return Err(self.error(format!("expected a record name, found {}", self.describe())));
                }
            }
        }
    }

    fn describe(&self) -> String {
        let short = |b: &[u8]| {
            let s = String::from_utf8_lossy(b.get(..b.len().min(24)).unwrap_or(&[])).into_owned();
            if b.len() > 24 { format!("{s}...") } else { s }
        };
        match &self.tok {
            Tok::Name(n) => format!("`{}:`", short(n.as_bytes())),
            Tok::Str(s) => format!("the string \"{}\"", short(s)),
            Tok::Number(n) => format!("the number `{}`", short(n)),
            Tok::Word(w) => format!("`{}`", short(w)),
            Tok::Star => "`*`".into(),
            Tok::Comma => "`,`".into(),
            Tok::Open => "`{`".into(),
            Tok::Close => "`}`".into(),
            Tok::End => "the end of the file".into(),
        }
    }

    /// One record after its name: values, then children.
    fn record(&mut self, name: String, depth: usize) -> Result<Node, Error> {
        if depth > self.limits.max_depth {
            return Err(self.error(format!("records nested deeper than {}", self.limits.max_depth)));
        }
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            return Err(Error::new(format!("more than {} records", self.limits.max_nodes)));
        }
        let mut values = Vec::new();
        // `Content: , "..."`: an empty first value.
        if matches!(self.tok, Tok::Comma) {
            self.advance()?;
        }
        loop {
            let v = match &self.tok {
                Tok::Str(s) => Value::String(String::from_utf8_lossy(s).into_owned()),
                Tok::Number(t) => scalar(t).ok_or_else(|| self.bad_number(t))?,
                Tok::Word(w) => word(w),
                Tok::Star => {
                    self.advance()?;
                    Value::Array(self.array(&name)?)
                }
                _ => break,
            };
            self.values += 1;
            if self.values > self.limits.max_values {
                return Err(Error::new(format!("more than {} values", self.limits.max_values)));
            }
            if !matches!(v, Value::Array(_)) {
                self.advance()?;
            }
            values.push(v);
            if !matches!(self.tok, Tok::Comma) {
                break;
            }
            self.advance()?;
        }
        if name == "Content" {
            values = vec![content(&values)];
        } else if ARRAY_RECORDS.contains(&name.as_str())
            && !values.is_empty()
            && values.iter().all(|v| v.as_f64().is_some())
        {
            values = vec![Value::Array(self.bare_list(&name, &values)?)];
        }
        let mut children = Vec::new();
        if matches!(self.tok, Tok::Open) {
            self.advance()?;
            children = self.records(depth + 1)?;
            if !matches!(self.tok, Tok::Close) {
                return Err(self.error(format!("record `{name}` is not closed: its `}}` is missing")));
            }
            self.advance()?;
        }
        if name == "FBXVersion"
            && depth == 1
            && let Some(v) = values.first().and_then(Value::as_i64).and_then(|v| u32::try_from(v).ok())
        {
            self.version = v;
        }
        Ok(Node { name, values, children })
    }

    fn bad_number(&self, t: &[u8]) -> Error {
        let s = String::from_utf8_lossy(t.get(..t.len().min(32)).unwrap_or(&[])).into_owned();
        self.error(format!("`{s}` is not a number"))
    }

    fn charge(&mut self, elements: usize) -> Result<(), Error> {
        self.array_bytes = self.array_bytes.saturating_add((elements as u64).saturating_mul(ELEMENT_BYTES));
        if self.array_bytes > self.limits.max_array_bytes {
            return Err(Error::new(format!(
                "the arrays exceed {} MiB together",
                self.limits.max_array_bytes >> 20
            )));
        }
        Ok(())
    }

    /// `N { a: v, v, ... }` after the `*`. The claimed `N` only sizes the first reservation; the
    /// elements present count.
    fn array(&mut self, name: &str) -> Result<Array, Error> {
        let claimed = match &self.tok {
            Tok::Number(t) => std::str::from_utf8(t).ok().and_then(|s| s.parse::<u64>().ok()),
            _ => None,
        };
        let Some(claimed) = claimed else {
            return Err(self.error(format!("the array `{name}` has no length after its `*`")));
        };
        self.advance()?;
        if !matches!(self.tok, Tok::Open) {
            return Err(self.error(format!("the array `{name}` has no `{{` after its length")));
        }
        self.advance()?;
        if !matches!(self.tok, Tok::Name(_)) {
            return Err(self.error(format!("the array `{name}` has no `a:` before its elements")));
        }
        self.advance()?;
        let reserve = usize::try_from(claimed).unwrap_or(MAX_RESERVE).min(MAX_RESERVE);
        let mut out = Building::new(name, reserve, self.version);
        loop {
            match self.tok {
                Tok::Number(t) => {
                    self.charge(1)?;
                    if !out.push(t) {
                        return Err(self.bad_number(t));
                    }
                }
                // `inf`, `nan`; any other word counts as its first letter's code, as ufbx
                // reads it.
                Tok::Word(w) => {
                    self.charge(1)?;
                    match inf_nan(w) {
                        Some(x) => out.push_float(x),
                        None => out.push_int(w.first().map_or(0, |c| i64::from(*c))),
                    }
                }
                _ => break,
            }
            self.advance()?;
            if !matches!(self.tok, Tok::Comma) {
                break;
            }
            self.advance()?;
        }
        if !matches!(self.tok, Tok::Close) {
            return Err(self.error(format!(
                "the array `{name}` holds {} where a number or its `}}` should be",
                self.describe()
            )));
        }
        self.advance()?;
        Ok(out.finish())
    }

    /// A bare list of numbers as one array. The values were counted already; only the array
    /// budget is charged.
    fn bare_list(&mut self, name: &str, values: &[Value]) -> Result<Array, Error> {
        self.charge(values.len())?;
        let mut out = Building::new(name, values.len().min(MAX_RESERVE), self.version);
        for v in values {
            out.push_value(v);
        }
        Ok(out.finish())
    }
}

/// A whole number: optional sign and digits.
fn is_integer(t: &[u8]) -> bool {
    let digits = t.strip_prefix(b"-").or_else(|| t.strip_prefix(b"+")).unwrap_or(t);
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

/// A float in any form a writer produces, `None` when `t` is none.
fn float(t: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(t).ok()?;
    s.parse::<f64>().ok().or_else(|| inf_nan(t))
}

/// Infinity or NaN as C libraries print them: `inf`, `infinity`, `nan`, `nan(ind)`, and the
/// old MSVC forms `1.#INF`, `-1.#IND00`, `1.#QNAN`, each with an optional sign, any case. The
/// whole of `t` must be one of them.
fn inf_nan(t: &[u8]) -> Option<f64> {
    let (negative, rest) = match t {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, t),
    };
    let lower = rest.to_ascii_lowercase();
    let infinite = match lower.as_slice() {
        [d, b'.', b'#', tail @ ..] if d.is_ascii_digit() => {
            let (word, digits) = tail.split_at_checked(3)?;
            if !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            match word {
                b"inf" => true,
                b"nan" | b"ind" => false,
                _ => return None,
            }
        }
        b"inf" | b"infinity" => true,
        [b'n', b'a', b'n', tail @ ..] => match tail {
            [] => false,
            [b'(', inner @ .., b')'] if inner.iter().all(u8::is_ascii_alphanumeric) => false,
            _ => return None,
        },
        _ => return None,
    };
    let v = if infinite { f64::INFINITY } else { f64::NAN };
    Some(if negative { -v } else { v })
}

fn scalar(t: &[u8]) -> Option<Value> {
    if is_integer(t)
        && let Some(v) = std::str::from_utf8(t).ok().and_then(|s| s.parse::<i64>().ok())
    {
        return Some(Value::I64(v));
    }
    float(t).map(Value::F64)
}

/// A bare word: one letter as a `Bool` holding it (`Y`, `T`, `N`), `nan` and `inf` as floats,
/// anything else as a string.
fn word(w: &[u8]) -> Value {
    if let [c] = w {
        return Value::Bool(*c);
    }
    match inf_nan(w) {
        Some(x) => Value::F64(x),
        None => Value::String(String::from_utf8_lossy(w).into_owned()),
    }
}

/// `Content`'s strings, base64 decoded and joined, as one raw value. Text that is not base64
/// gives no bytes, so the reader looks for the file beside the FBX instead.
fn content(values: &[Value]) -> Value {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::String(s) => match base64(s.as_bytes(), &mut out) {
                Some(()) => {}
                None => return Value::Raw(Vec::new()),
            },
            Value::Raw(b) => out.extend_from_slice(b),
            _ => {}
        }
    }
    Value::Raw(out)
}

/// Appends the bytes of standard base64 text to `out`. White space is skipped, `=` ends the
/// data; `None` for any other character.
fn base64(text: &[u8], out: &mut Vec<u8>) -> Option<()> {
    out.reserve(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(())
}

/// An array whose element type widens as elements arrive.
enum Building {
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    /// 32-bit floats written as the integer of their bits.
    Bits(Vec<f32>),
}

impl Building {
    fn new(name: &str, reserve: usize, version: u32) -> Self {
        if name == "KeyAttrDataFloat" && version >= BITS_SINCE {
            Building::Bits(Vec::with_capacity(reserve))
        } else if F32_ARRAYS.contains(&name) {
            Building::F32(Vec::with_capacity(reserve))
        } else {
            Building::I32(Vec::with_capacity(reserve))
        }
    }

    /// Adds the number `t`; false when it is none.
    fn push(&mut self, t: &[u8]) -> bool {
        let int = if is_integer(t) {
            std::str::from_utf8(t).ok().and_then(|s| s.parse::<i64>().ok())
        } else {
            None
        };
        if let Building::F32(v) = self {
            // Straight to 32 bits: a float through 64 bits could round differently.
            let x = match int {
                Some(i) => i as f32,
                None => match std::str::from_utf8(t).ok().and_then(|s| s.parse::<f32>().ok()) {
                    Some(x) => x,
                    None => match float(t) {
                        Some(x) => x as f32,
                        None => return false,
                    },
                },
            };
            v.push(x);
            return true;
        }
        match int {
            Some(i) => self.push_int(i),
            None => match float(t) {
                Some(x) => self.push_float(x),
                None => return false,
            },
        }
        true
    }

    fn push_value(&mut self, v: &Value) {
        match v {
            Value::F32(x) => self.push_float(f64::from(*x)),
            Value::F64(x) => self.push_float(*x),
            other => self.push_int(other.as_i64().unwrap_or(0)),
        }
    }

    fn push_int(&mut self, i: i64) {
        match self {
            Building::I32(v) => match i32::try_from(i) {
                Ok(x) => v.push(x),
                Err(_) => {
                    let mut w: Vec<i64> = Vec::with_capacity(v.capacity().max(v.len() + 1));
                    w.extend(v.iter().map(|x| i64::from(*x)));
                    w.push(i);
                    *self = Building::I64(w);
                }
            },
            Building::I64(v) => v.push(i),
            Building::F32(v) => v.push(i as f32),
            Building::F64(v) => v.push(i as f64),
            Building::Bits(v) => v.push(f32::from_bits(i as u32)),
        }
    }

    fn push_float(&mut self, x: f64) {
        match self {
            Building::I32(v) => {
                let mut w: Vec<f64> = Vec::with_capacity(v.capacity().max(v.len() + 1));
                w.extend(v.iter().map(|x| f64::from(*x)));
                w.push(x);
                *self = Building::F64(w);
            }
            Building::I64(v) => {
                let mut w: Vec<f64> = Vec::with_capacity(v.capacity().max(v.len() + 1));
                w.extend(v.iter().map(|x| *x as f64));
                w.push(x);
                *self = Building::F64(w);
            }
            Building::F32(v) => v.push(x as f32),
            Building::F64(v) => v.push(x),
            Building::Bits(v) => v.push(f32::from_bits(x as i32 as u32)),
        }
    }

    fn finish(self) -> Array {
        match self {
            Building::I32(v) => Array::I32(v),
            Building::I64(v) => Array::I64(v),
            Building::F32(v) | Building::Bits(v) => Array::F32(v),
            Building::F64(v) => Array::F64(v),
        }
    }
}
