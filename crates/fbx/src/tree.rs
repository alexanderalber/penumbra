//! The node tree: records with a name, values and child records, exactly as stored. This module
//! reads the binary form; [`crate::ascii`] reads the ASCII form into the same tree.
//!
//! A record header holds the offset of the record's end, its number of values, the byte length
//! of the values and the length of its name: three 32-bit numbers and a byte below version
//! 7500, three 64-bit numbers and a byte from 7500 on. A header of zeros ends a list of
//! children. Values are typed by one letter: `C` (bool), `B` and `Z` (bytes some writers use
//! though no documentation lists them), `Y` `I` `L` (16, 32, 64-bit integers), `F` `D` (32,
//! 64-bit floats), `S` (string) and `R` (raw bytes), and the arrays `b` `c` `i` `l` `f` `d`,
//! stored plain or zlib-compressed.

use crate::{Error, Limits, MAGIC};

/// One value of a record.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `C`.
    Bool(u8),
    /// `B` and `Z`.
    Byte(u8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    /// `S`. Bytes that are not UTF-8 are replaced, not refused. Object names keep the binary
    /// separator `\0\x01` between name and class.
    String(String),
    /// `R`, e.g. an embedded image.
    Raw(Vec<u8>),
    Array(Array),
}

/// An array value.
#[derive(Clone, Debug, PartialEq)]
pub enum Array {
    /// `b`.
    Bool(Vec<u8>),
    /// `c`.
    Byte(Vec<u8>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl Array {
    pub fn len(&self) -> usize {
        match self {
            Array::Bool(v) | Array::Byte(v) => v.len(),
            Array::I32(v) => v.len(),
            Array::I64(v) => v.len(),
            Array::F32(v) => v.len(),
            Array::F64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element `i` as a float.
    pub fn f64_at(&self, i: usize) -> Option<f64> {
        match self {
            Array::Bool(v) | Array::Byte(v) => v.get(i).map(|x| f64::from(*x)),
            Array::I32(v) => v.get(i).map(|x| f64::from(*x)),
            Array::I64(v) => v.get(i).map(|x| *x as f64),
            Array::F32(v) => v.get(i).map(|x| f64::from(*x)),
            Array::F64(v) => v.get(i).copied(),
        }
    }

    /// Element `i` as an integer (floats truncated).
    pub fn i64_at(&self, i: usize) -> Option<i64> {
        match self {
            Array::Bool(v) | Array::Byte(v) => v.get(i).map(|x| i64::from(*x)),
            Array::I32(v) => v.get(i).map(|x| i64::from(*x)),
            Array::I64(v) => v.get(i).copied(),
            Array::F32(v) => v.get(i).map(|x| *x as i64),
            Array::F64(v) => v.get(i).map(|x| *x as i64),
        }
    }
}

impl Value {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Bool(v) | Value::Byte(v) => Some(i64::from(*v)),
            Value::I16(v) => Some(i64::from(*v)),
            Value::I32(v) => Some(i64::from(*v)),
            Value::I64(v) => Some(*v),
            Value::F32(v) => Some(*v as i64),
            Value::F64(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F32(v) => Some(f64::from(*v)),
            Value::F64(v) => Some(*v),
            Value::Bool(v) | Value::Byte(v) => Some(f64::from(*v)),
            Value::I16(v) => Some(f64::from(*v)),
            Value::I32(v) => Some(f64::from(*v)),
            Value::I64(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Raw(b) => Some(b),
            Value::String(s) => Some(s.as_bytes()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// One record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub name: String,
    pub values: Vec<Value>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(name: &str, values: Vec<Value>, children: Vec<Node>) -> Self {
        Self { name: name.to_string(), values, children }
    }

    /// The first child called `name`.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// All children called `name`, in file order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    pub fn value(&self, i: usize) -> Option<&Value> {
        self.values.get(i)
    }

    /// The first value of child `name` as an array.
    pub fn child_array(&self, name: &str) -> Option<&Array> {
        self.child(name)?.value(0)?.as_array()
    }

    /// The first value of child `name` as a string.
    pub fn child_str(&self, name: &str) -> Option<&str> {
        self.child(name)?.value(0)?.as_str()
    }
}

/// A whole file: its version and its top-level records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub version: u32,
    pub nodes: Vec<Node>,
}

impl Tree {
    /// The first top-level record called `name`.
    pub fn top(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }
}

/// Reads the node tree of an FBX file, binary or ASCII.
pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Tree, Error> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(Error::new(format!("larger than {} MiB", limits.max_file_bytes >> 20)));
    }
    if !bytes.starts_with(MAGIC) {
        return match crate::ascii::parse(bytes, limits) {
            Err(e) if !crate::is_ascii_fbx(bytes) => {
                Err(e.context("not an FBX file: neither binary nor ASCII FBX"))
            }
            other => other,
        };
    }
    let head = bytes.get(..27).ok_or_else(|| Error::new("the FBX header is cut short"))?;
    if head.get(22).copied() != Some(0) {
        return Err(Error::new("a big-endian FBX file; only little-endian files are read"));
    }
    let version = head.get(23..27).and_then(|b| b.try_into().ok()).map_or(0, u32::from_le_bytes);
    if version < 7000 {
        return Err(Error::new(format!("FBX version {version}; only 7000 and later are read")));
    }
    if version >= 10000 {
        return Err(Error::new(format!("FBX version {version} is not known")));
    }
    let mut p =
        Parser { data: bytes, pos: 27, wide: version >= 7500, limits, nodes: 0, values: 0, array_bytes: 0 };
    let mut nodes = Vec::new();
    // The top level ends with a null record, or (in some writers) where the footer begins:
    // fewer bytes than a header left, or a record that does not parse as one.
    while p.pos + p.header_len() <= bytes.len() {
        match p.node(0)? {
            Some(n) => nodes.push(n),
            None => break,
        }
    }
    Ok(Tree { version, nodes })
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    /// 64-bit record headers (7500 and later).
    wide: bool,
    limits: &'a Limits,
    nodes: usize,
    values: usize,
    array_bytes: u64,
}

impl Parser<'_> {
    fn header_len(&self) -> usize {
        if self.wide { 25 } else { 13 }
    }

    fn cut(&self) -> Error {
        Error::new(format!("the file is cut short or damaged at byte {}", self.pos))
    }

    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.cut())?;
        let s = self.data.get(self.pos..end).ok_or_else(|| self.cut())?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let s = self.take(N)?;
        s.try_into().map_err(|_| Error::new("internal: bad slice length"))
    }

    fn u8(&mut self) -> Result<u8, Error> {
        let [b] = self.array::<1>()?;
        Ok(b)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn offset(&self, v: u64) -> Result<usize, Error> {
        usize::try_from(v).map_err(|_| self.cut())
    }

    /// One record and its children; `None` for a null record.
    fn node(&mut self, depth: usize) -> Result<Option<Node>, Error> {
        if depth > self.limits.max_depth {
            return Err(Error::new(format!("records nested deeper than {}", self.limits.max_depth)));
        }
        let start = self.pos;
        let (end, count, values_len) = if self.wide {
            (self.u64()?, self.u64()?, self.u64()?)
        } else {
            (u64::from(self.u32()?), u64::from(self.u32()?), u64::from(self.u32()?))
        };
        let name_len = self.u8()?;
        if end == 0 && name_len == 0 {
            return Ok(None);
        }
        self.nodes += 1;
        if self.nodes > self.limits.max_nodes {
            return Err(Error::new(format!("more than {} records", self.limits.max_nodes)));
        }
        let end = self.offset(end)?;
        // A zero end offset (written by some Maya versions) means: no children, end unchecked.
        if end != 0 && (end <= start || end > self.data.len()) {
            return Err(Error::new(format!("a record at byte {start} ends outside the file")));
        }
        let name = String::from_utf8_lossy(self.take(usize::from(name_len))?).into_owned();
        let values_end = self.offset(values_len)?.checked_add(self.pos).ok_or_else(|| self.cut())?;
        if values_end > self.data.len() || (end != 0 && values_end > end) {
            return Err(Error::new(format!("the values of record `{name}` lie outside it")));
        }
        // Each value takes at least two bytes, so the count cannot exceed half the bytes.
        let count = usize::try_from(count).map_err(|_| self.cut())?;
        if count > values_len as usize / 2 + 1 {
            return Err(Error::new(format!("record `{name}` claims more values than it has bytes")));
        }
        self.values = self.values.saturating_add(count);
        if self.values > self.limits.max_values {
            return Err(Error::new(format!("more than {} values", self.limits.max_values)));
        }
        // Grown as values are read, so a count the bytes do not back costs nothing.
        let mut values = Vec::with_capacity(count.min(16));
        for _ in 0..count {
            values.push(self.value().map_err(|e| e.context(format!("record `{name}`")))?);
        }
        if self.pos > values_end {
            return Err(Error::new(format!("the values of record `{name}` overrun their length")));
        }
        self.pos = values_end;
        let mut children = Vec::new();
        while end != 0 && self.pos < end {
            match self.node(depth + 1)? {
                Some(c) => children.push(c),
                None => break,
            }
        }
        if end != 0 && self.pos != end {
            return Err(Error::new(format!("the children of record `{name}` do not end where it does")));
        }
        Ok(Some(Node { name, values, children }))
    }

    fn value(&mut self) -> Result<Value, Error> {
        let code = self.u8()?;
        Ok(match code {
            b'C' => Value::Bool(self.u8()?),
            b'B' | b'Z' => Value::Byte(self.u8()?),
            b'Y' => Value::I16(i16::from_le_bytes(self.array()?)),
            b'I' => Value::I32(i32::from_le_bytes(self.array()?)),
            b'L' => Value::I64(i64::from_le_bytes(self.array()?)),
            b'F' => Value::F32(f32::from_le_bytes(self.array()?)),
            b'D' => Value::F64(f64::from_le_bytes(self.array()?)),
            b'S' => {
                let n = self.u32()? as usize;
                Value::String(String::from_utf8_lossy(self.take(n)?).into_owned())
            }
            b'R' => {
                let n = self.u32()? as usize;
                Value::Raw(self.take(n)?.to_vec())
            }
            b'b' | b'c' | b'i' | b'l' | b'f' | b'd' => Value::Array(self.array_value(code)?),
            other => {
                return Err(Error::new(format!(
                    "unknown value type {:?} at byte {}",
                    char::from(other),
                    self.pos - 1
                )));
            }
        })
    }

    fn array_value(&mut self, code: u8) -> Result<Array, Error> {
        let count = self.u32()? as usize;
        let encoding = self.u32()?;
        let encoded = self.u32()? as usize;
        let size = match code {
            b'b' | b'c' => 1,
            b'i' | b'f' => 4,
            _ => 8,
        };
        let bytes = count.checked_mul(size).ok_or_else(|| Error::new("an array is too large"))?;
        self.array_bytes = self.array_bytes.saturating_add(bytes as u64);
        if self.array_bytes > self.limits.max_array_bytes {
            return Err(Error::new(format!(
                "the arrays exceed {} MiB together",
                self.limits.max_array_bytes >> 20
            )));
        }
        let at = self.pos;
        let data = self.take(encoded)?;
        let raw: std::borrow::Cow<'_, [u8]> = match encoding {
            0 => {
                if encoded != bytes {
                    return Err(Error::new(format!("an array at byte {at} has the wrong length")));
                }
                std::borrow::Cow::Borrowed(data)
            }
            1 => {
                let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, bytes)
                    .map_err(|_| Error::new(format!("bad zlib data in an array at byte {at}")))?;
                if out.len() != bytes {
                    return Err(Error::new(format!("an array at byte {at} inflates to the wrong length")));
                }
                std::borrow::Cow::Owned(out)
            }
            e => return Err(Error::new(format!("unknown array encoding {e} at byte {at}"))),
        };
        Ok(match code {
            b'b' => Array::Bool(raw.into_owned()),
            b'c' => Array::Byte(raw.into_owned()),
            b'i' => Array::I32(raw.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b)).collect()),
            b'l' => Array::I64(raw.as_chunks::<8>().0.iter().map(|b| i64::from_le_bytes(*b)).collect()),
            b'f' => Array::F32(raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()),
            _ => Array::F64(raw.as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect()),
        })
    }
}
