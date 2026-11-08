//! A message as named values, and what a reader keeps of them.
//!
//! - **Lines:** a line is split into fields, by default at commas, semicolons or tabs, and at
//!   spaces when it has none of those. A field `name:value` or `name=value` (a name starts with a
//!   letter or `_`) is named; `name: value` with a space works too. Every other field is counted
//!   from 0 and named by the reader's `names` (`-` leaves one out), else `chan0`, `chan1`, ...
//!   A field that is a number sets its channel; any other is text, which only the table shows.
//!   Whitespace around a line (a `\r` before the `\n`) is ignored, empty lines are skipped.
//! - **JSON:** one value per message. An object's keys are the names, nested objects and arrays
//!   are flattened with `_` (`{"acc": {"x": 1}, "xy": [1, 2]}` is `acc_x`, `xy_0`, `xy_1`); a top
//!   level array or a bare value is counted as positional fields. Booleans are 1 and 0, `null`
//!   is left out, strings are text. Deeper than [`MAX_DEPTH`] is left out.
//! - **Bytes:** each byte a positional field, 0 to 255.
//!
//! Everything is capped: [`MAX_FIELDS`] per message, [`MAX_CHANNELS`] and [`MAX_COLUMNS`] per
//! reader, [`MAX_TEXT`] characters per cell. What does not fit is counted, never kept.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use crate::frame::Framer;

/// Fields taken from one message; the rest is left out and counted.
pub const MAX_FIELDS: usize = 1024;
/// Channels one reader keeps; names beyond are left out and counted.
pub const MAX_CHANNELS: usize = 1024;
/// Table columns one reader keeps.
pub const MAX_COLUMNS: usize = 256;
/// Table rows one reader keeps at most.
pub const MAX_ROWS: usize = 1024;
/// Characters of one text cell; longer text is cut.
pub const MAX_TEXT: usize = 4096;
/// How deep a JSON message is flattened.
pub const MAX_DEPTH: usize = 8;
/// Largest message cap a reader may ask for.
pub const MAX_MESSAGE: usize = 1 << 20;
/// Characters of a message kept for the status.
const PREVIEW: usize = 200;

/// What a message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Lines,
    Json,
    Bytes,
}

/// Where a line is split into fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    /// At commas, semicolons and tabs; at spaces when the line has none of those.
    Auto,
    Comma,
    Semicolon,
    Tab,
    Space,
    /// Not at all: the line is one field.
    None,
}

impl Split {
    pub const NAMES: [&'static str; 6] = ["auto", "comma", "semicolon", "tab", "space", "none"];

    pub fn parse(s: &str) -> Option<Split> {
        Some(match s {
            "auto" => Split::Auto,
            "comma" => Split::Comma,
            "semicolon" => Split::Semicolon,
            "tab" => Split::Tab,
            "space" => Split::Space,
            "none" => Split::None,
            _ => return None,
        })
    }
}

/// How a reader cuts and reads its messages.
#[derive(Clone, Debug, PartialEq)]
pub struct Decode {
    pub format: Format,
    pub split: Split,
    /// Names of the positional fields, in order; `-` leaves one out.
    pub names: Vec<String>,
    /// Where a message ends on a stream; on UDP, lines and JSON in one datagram are split at it
    /// too. Empty: no delimiter.
    pub delimiter: Vec<u8>,
    /// Bytes of one message on a stream when there is no delimiter; 0 is each read.
    pub length: usize,
    /// Longest message; longer ones are dropped and counted.
    pub max: usize,
    /// Table rows kept, the newest last.
    pub rows: usize,
}

impl Default for Decode {
    fn default() -> Self {
        Decode {
            format: Format::Lines,
            split: Split::Auto,
            names: Vec::new(),
            delimiter: b"\n".to_vec(),
            length: 0,
            max: 64 * 1024,
            rows: 16,
        }
    }
}

impl Decode {
    /// The framer for one connection.
    pub fn framer(&self) -> Framer {
        Framer::new(&self.delimiter, self.length, self.max.min(MAX_MESSAGE))
    }

    fn positional(&self, index: usize) -> Option<String> {
        match self.names.get(index).map(String::as_str) {
            Some("-") => None,
            Some(n) => Some(n.to_string()),
            None => Some(format!("chan{index}")),
        }
    }
}

/// One value of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Num(f64),
    Text(String),
}

impl Cell {
    fn of(text: &str) -> Cell {
        let t = text.trim();
        match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Cell::Num(v),
            _ => Cell::Text(cut(t)),
        }
    }

    /// As a table cell.
    pub fn text(&self) -> String {
        match self {
            Cell::Num(v) => crate::encode::number(*v),
            Cell::Text(t) => t.clone(),
        }
    }
}

/// A named value of a message.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// A valid channel name: letters, digits and `_`, not starting with a digit, at most 64.
    pub name: String,
    pub cell: Cell,
}

fn cut(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

/// `text` as a channel name: other characters become `_`, a leading digit gets a `_` before it,
/// at most 64 characters. `None` when nothing is left.
pub fn channel_name(text: &str) -> Option<String> {
    let mut s: String =
        text.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if s.is_empty() {
        return None;
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s.truncate(64);
    Some(s)
}

/// The fields of one message. `Err` for a message that is not what the format says (JSON that
/// does not parse); the reason is short and safe to show.
pub fn fields(message: &[u8], d: &Decode) -> Result<Vec<Field>, String> {
    let mut out = Vec::new();
    match d.format {
        Format::Lines => {
            let text = String::from_utf8_lossy(message);
            line_fields(text.trim(), d, &mut out);
        }
        Format::Json => {
            let value: serde_json::Value = serde_json::from_slice(message).map_err(|e| {
                let mut s = format!("not JSON: {e}");
                s.truncate(120);
                s
            })?;
            match value {
                serde_json::Value::Object(_) => flatten(&value, None, 0, &mut out),
                serde_json::Value::Array(items) => {
                    for (i, v) in items.iter().enumerate() {
                        if out.len() >= MAX_FIELDS {
                            break;
                        }
                        if let Some(name) = d.positional(i) {
                            flatten(v, Some(&name), 1, &mut out);
                        }
                    }
                }
                other => {
                    if let Some(name) = d.positional(0) {
                        flatten(&other, Some(&name), 1, &mut out);
                    }
                }
            }
        }
        Format::Bytes => {
            for (i, b) in message.iter().enumerate().take(MAX_FIELDS) {
                if let Some(name) = d.positional(i).and_then(|n| channel_name(&n)) {
                    out.push(Field { name, cell: Cell::Num(f64::from(*b)) });
                }
            }
        }
    }
    out.truncate(MAX_FIELDS);
    Ok(out)
}

/// A field's name when it is `name:value` or `name=value`, with the rest.
fn named(token: &str) -> Option<(&str, &str)> {
    let at = token.find([':', '='])?;
    let (name, rest) = (token.get(..at)?.trim(), token.get(at + 1..)?);
    let first = name.chars().next()?;
    let ok = (first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'));
    ok.then_some((name, rest))
}

fn line_fields(text: &str, d: &Decode, out: &mut Vec<Field>) {
    if text.is_empty() {
        return;
    }
    let tokens: Vec<&str> = match d.split {
        Split::Auto if text.contains([',', ';', '\t']) => text.split([',', ';', '\t']).collect(),
        Split::Auto | Split::Space => text.split_whitespace().collect(),
        Split::Comma => text.split(',').collect(),
        Split::Semicolon => text.split(';').collect(),
        Split::Tab => text.split('\t').collect(),
        Split::None => vec![text],
    };
    let mut position = 0;
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        if out.len() >= MAX_FIELDS {
            return;
        }
        let token = token.trim();
        if d.split != Split::None
            && let Some((name, rest)) = named(token)
        {
            let value = if rest.trim().is_empty() { tokens.next().unwrap_or_default() } else { rest };
            if let Some(name) = channel_name(name) {
                out.push(Field { name, cell: Cell::of(value) });
            }
            continue;
        }
        let name = d.positional(position).and_then(|n| channel_name(&n));
        position += 1;
        if let Some(name) = name {
            out.push(Field { name, cell: Cell::of(token) });
        }
    }
}

/// Characters of a channel name.
const MAX_NAME: usize = 64;

/// At most the first [`MAX_NAME`] characters of `key`.
fn short(key: &str) -> &str {
    key.char_indices().nth(MAX_NAME).and_then(|(i, _)| key.get(..i)).unwrap_or(key)
}

fn flatten(value: &serde_json::Value, prefix: Option<&str>, depth: usize, out: &mut Vec<Field>) {
    use serde_json::Value;
    if depth > MAX_DEPTH || out.len() >= MAX_FIELDS {
        return;
    }
    // A name is at most 64 characters, so a longer prefix is never copied on: a huge key with
    // many items under it costs a short string per item.
    let join = |key: &str| {
        let mut name = match prefix {
            Some(p) => format!("{p}_{}", short(key)),
            None => short(key).to_string(),
        };
        name.truncate(short(&name).len());
        name
    };
    let leaf = |cell: Cell, out: &mut Vec<Field>| {
        if let Some(name) = prefix.and_then(channel_name) {
            out.push(Field { name, cell });
        }
    };
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if out.len() >= MAX_FIELDS {
                    break;
                }
                flatten(v, Some(&join(k)), depth + 1, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                if out.len() >= MAX_FIELDS {
                    break;
                }
                flatten(v, Some(&join(&i.to_string())), depth + 1, out);
            }
        }
        Value::Number(n) => {
            if let Some(v) = n.as_f64().filter(|v| v.is_finite()) {
                leaf(Cell::Num(v), out);
            }
        }
        Value::Bool(b) => leaf(Cell::Num(if *b { 1.0 } else { 0.0 }), out),
        Value::String(s) => leaf(Cell::Text(cut(s)), out),
        Value::Null => {}
    }
}

/// What went wrong with messages, counted since the reader started.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Counts {
    /// Messages taken in.
    pub messages: u64,
    /// Messages that were not what the format says (JSON that does not parse).
    pub malformed: u64,
    /// Messages longer than the cap.
    pub too_long: u64,
    /// Names left out because the reader holds as many channels or columns as it may.
    pub over_cap: u64,
    /// Why the last message was dropped.
    pub last_problem: Option<String>,
    /// The start of the last message taken in.
    pub last: Option<String>,
}

/// What a reader keeps: the latest number per name, in the order the names first came, and the
/// last messages as table rows under a header of every name seen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    /// Counts every change.
    pub version: u64,
    pub values: Vec<(String, f64)>,
    pub columns: Vec<String>,
    /// One row per message, cells in the order of `columns` (shorter when names came later).
    pub rows: VecDeque<Vec<String>>,
    pub counts: Counts,
    index: BTreeMap<String, usize>,
    column_index: BTreeMap<String, usize>,
}

/// One message, parsed: what a [`Store`] takes in. Parsing needs no store, so a link parses
/// before it locks its readers.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    Fields {
        fields: Vec<Field>,
        preview: String,
    },
    Malformed(String),
    TooLong,
    /// Only whitespace: skipped.
    Blank,
}

/// One message from a stream.
pub fn parse(message: &[u8], d: &Decode) -> Parsed {
    if message.len() > d.max {
        return Parsed::TooLong;
    }
    if d.format != Format::Bytes && message.iter().all(u8::is_ascii_whitespace) {
        return Parsed::Blank;
    }
    match fields(message, d) {
        Ok(fields) => {
            let preview = if d.format == Format::Bytes {
                message.iter().take(PREVIEW / 3).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            } else {
                String::from_utf8_lossy(message.get(..PREVIEW * 4).unwrap_or(message))
                    .chars()
                    .take(PREVIEW)
                    .collect()
            };
            Parsed::Fields { fields, preview }
        }
        Err(e) => Parsed::Malformed(e),
    }
}

/// One UDP datagram: one message for bytes. JSON is one message when the whole datagram parses
/// (pretty-printed JSON spans lines), else it is split at the delimiter like lines.
pub fn parse_datagram(datagram: &[u8], d: &Decode) -> Vec<Parsed> {
    if datagram.len() > d.max {
        return vec![Parsed::TooLong];
    }
    if d.format == Format::Bytes || d.delimiter.is_empty() {
        return vec![parse(datagram, d)];
    }
    if d.format == Format::Json {
        let whole = parse(datagram, d);
        if matches!(whole, Parsed::Fields { .. } | Parsed::Blank) {
            return vec![whole];
        }
    }
    let n = d.delimiter.len();
    let mut out = Vec::new();
    let mut rest = datagram;
    while !rest.is_empty() && out.len() < MAX_ROWS {
        let at = rest.windows(n).position(|w| w == d.delimiter.as_slice());
        let (message, next) = match at {
            Some(at) => (rest.get(..at).unwrap_or_default(), rest.get(at + n..).unwrap_or_default()),
            None => (rest, &[][..]),
        };
        if !message.iter().all(u8::is_ascii_whitespace) {
            out.push(parse(message, d));
        }
        rest = next;
    }
    out
}

impl Store {
    /// Takes in a parsed message.
    pub fn apply_parsed(&mut self, parsed: Parsed, d: &Decode) {
        self.version += 1;
        match parsed {
            Parsed::Fields { fields, preview } => {
                self.counts.messages += 1;
                self.counts.last = Some(preview);
                self.apply(&fields, d.rows.clamp(1, MAX_ROWS));
            }
            Parsed::Malformed(e) => {
                self.counts.malformed += 1;
                self.counts.last_problem = Some(e);
            }
            Parsed::TooLong => self.too_long(1),
            Parsed::Blank => {}
        }
    }

    /// One message from a stream, parsed and taken in.
    pub fn take(&mut self, message: &[u8], d: &Decode) {
        self.apply_parsed(parse(message, d), d);
    }

    /// One UDP datagram, parsed and taken in.
    pub fn datagram(&mut self, datagram: &[u8], d: &Decode) {
        for p in parse_datagram(datagram, d) {
            self.apply_parsed(p, d);
        }
    }

    /// Counts messages dropped for their length.
    pub fn too_long(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        self.version += 1;
        self.counts.too_long += n;
        self.counts.last_problem = Some("a message was longer than `max` bytes".to_string());
    }

    fn apply(&mut self, fields: &[Field], rows: usize) {
        let mut row: Vec<String> = Vec::new();
        for f in fields {
            if let Cell::Num(v) = f.cell {
                match self.index.get(&f.name) {
                    Some(&i) => {
                        if let Some(slot) = self.values.get_mut(i) {
                            slot.1 = v;
                        }
                    }
                    None if self.values.len() < MAX_CHANNELS => {
                        self.index.insert(f.name.clone(), self.values.len());
                        self.values.push((f.name.clone(), v));
                    }
                    None => self.counts.over_cap += 1,
                }
            }
            let column = match self.column_index.get(&f.name) {
                Some(&c) => c,
                None if self.columns.len() < MAX_COLUMNS => {
                    let c = self.columns.len();
                    self.column_index.insert(f.name.clone(), c);
                    self.columns.push(f.name.clone());
                    c
                }
                None => {
                    self.counts.over_cap += 1;
                    continue;
                }
            };
            if row.len() <= column {
                row.resize(column + 1, String::new());
            }
            if let Some(cell) = row.get_mut(column) {
                *cell = f.cell.text();
            }
        }
        self.rows.push_back(row);
        while self.rows.len() > rows {
            self.rows.pop_front();
        }
    }

    /// The table: the names as the first row, then one row per message, the newest last.
    pub fn table(&self) -> Vec<Vec<String>> {
        let width = self.columns.len();
        let mut out = Vec::with_capacity(self.rows.len() + 1);
        out.push(self.columns.clone());
        for r in &self.rows {
            let mut row = r.clone();
            row.resize(width, String::new());
            out.push(row);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(format: Format) -> Decode {
        Decode { format, ..Decode::default() }
    }

    fn values(s: &Store) -> Vec<(&str, f64)> {
        s.values.iter().map(|(n, v)| (n.as_str(), *v)).collect()
    }

    fn line(text: &str, d: &Decode) -> Vec<(String, Cell)> {
        fields(text.as_bytes(), d).unwrap().into_iter().map(|f| (f.name, f.cell)).collect()
    }

    #[test]
    fn lines_split_into_positional_and_named_fields() {
        let l = d(Format::Lines);
        let n = |s: &str, v: f64| (s.to_string(), Cell::Num(v));
        assert_eq!(line("1,2.5,-3", &l), vec![n("chan0", 1.0), n("chan1", 2.5), n("chan2", -3.0)]);
        assert_eq!(line("1  2 3", &l), vec![n("chan0", 1.0), n("chan1", 2.0), n("chan2", 3.0)]);
        assert_eq!(
            line("1\t2 x", &l),
            vec![n("chan0", 1.0), ("chan1".to_string(), Cell::Text("2 x".into()))]
        );
        assert_eq!(line("x=1 y:2", &l), vec![n("x", 1.0), n("y", 2.0)]);
        assert_eq!(line("temp: 21.5 hum: 40", &l), vec![n("temp", 21.5), n("hum", 40.0)]);
        assert_eq!(line("temp: 21.5, hum: 40\r", &l), vec![n("temp", 21.5), n("hum", 40.0)]);
        assert_eq!(line("acc.x=1", &l), vec![n("acc_x", 1.0)]);
        // A time is not a name; text fields keep their position.
        assert_eq!(
            line("ACC 12:30 7", &l),
            vec![
                ("chan0".to_string(), Cell::Text("ACC".into())),
                ("chan1".to_string(), Cell::Text("12:30".into())),
                n("chan2", 7.0)
            ]
        );
        assert_eq!(
            line("1,,3", &l),
            vec![n("chan0", 1.0), ("chan1".to_string(), Cell::Text(String::new())), n("chan2", 3.0)]
        );
        assert_eq!(line("nan inf 1e400", &l).iter().filter(|(_, c)| matches!(c, Cell::Num(_))).count(), 0);
        let names = Decode { names: vec!["x".into(), "-".into(), "9z".into()], ..l.clone() };
        assert_eq!(line("1 2 3 4", &names), vec![n("x", 1.0), n("_9z", 3.0), n("chan3", 4.0)]);
        let none = Decode { split: Split::None, ..l.clone() };
        assert_eq!(
            line(" hello, world ", &none),
            vec![("chan0".to_string(), Cell::Text("hello, world".into()))]
        );
        let comma = Decode { split: Split::Comma, ..l };
        assert_eq!(
            line("1 2,3", &comma),
            vec![("chan0".to_string(), Cell::Text("1 2".into())), n("chan1", 3.0)]
        );
    }

    #[test]
    fn json_flattens_objects_and_arrays() {
        let j = d(Format::Json);
        let got =
            line(r#"{"acc": {"x": 1, "y": -2}, "on": true, "xy": [0.5, 0.25], "n": null, "s": "hi"}"#, &j);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["acc_x", "acc_y", "on", "s", "xy_0", "xy_1"]);
        assert_eq!(got[2].1, Cell::Num(1.0));
        assert_eq!(
            line("[1, 2]", &j),
            vec![("chan0".into(), Cell::Num(1.0)), ("chan1".into(), Cell::Num(2.0))]
        );
        assert_eq!(line("7", &j), vec![("chan0".into(), Cell::Num(7.0))]);
        assert_eq!(line(r#"{"1 a": 3}"#, &j), vec![("_1_a".into(), Cell::Num(3.0))]);
        assert!(fields(b"{\"a\": ", &j).unwrap_err().starts_with("not JSON"));
        // Too deep is left out, not followed.
        let deep = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        assert!(line(&deep, &j).is_empty());
        let very_deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(fields(very_deep.as_bytes(), &j).is_err());
        // Many keys are capped.
        let many =
            format!("{{{}}}", (0..5000).map(|i| format!("\"k{i}\": {i}")).collect::<Vec<_>>().join(","));
        assert_eq!(line(&many, &j).len(), MAX_FIELDS);
    }

    #[test]
    fn a_huge_key_over_many_items_stays_cheap() {
        let j = d(Format::Json);
        let key = "k".repeat(32 * 1024);
        let items = vec!["0"; 15_000].join(",");
        let msg = format!("{{\"{key}\": [{items}]}}");
        let start = std::time::Instant::now();
        let got = fields(msg.as_bytes(), &j).unwrap();
        assert_eq!(got.len(), MAX_FIELDS);
        assert!(got.iter().all(|f| f.name.len() <= 64));
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[test]
    fn pretty_json_in_one_datagram_is_one_message() {
        let j = d(Format::Json);
        let mut s = Store::default();
        s.datagram(b"{\n  \"a\": 1,\n  \"b\": 2\n}\n", &j);
        assert_eq!(values(&s), [("a", 1.0), ("b", 2.0)]);
        assert_eq!(s.counts.malformed, 0);
        // Several lines of JSON still split.
        s.datagram(b"{\"a\": 3}\n{\"b\": 4}", &j);
        assert_eq!(values(&s), [("a", 3.0), ("b", 4.0)]);
    }

    #[test]
    fn bytes_are_positional_numbers() {
        let b = Decode { names: vec!["cmd".into()], ..d(Format::Bytes) };
        assert_eq!(
            line("\x01\x7f", &b),
            vec![("cmd".into(), Cell::Num(1.0)), ("chan1".into(), Cell::Num(127.0))]
        );
    }

    #[test]
    fn a_store_keeps_the_latest_values_and_the_last_rows() {
        let l = Decode { rows: 2, ..d(Format::Lines) };
        let mut s = Store::default();
        s.take(b"1,2", &l);
        s.take(b"x=5", &l);
        s.take(b"3,hello", &l);
        assert_eq!(values(&s), [("chan0", 3.0), ("chan1", 2.0), ("x", 5.0)]);
        assert_eq!(
            s.table(),
            vec![
                vec!["chan0".to_string(), "chan1".into(), "x".into()],
                vec![String::new(), String::new(), "5".into()],
                vec!["3".into(), "hello".into(), String::new()],
            ]
        );
        assert_eq!(s.counts.messages, 3);
        // Malformed JSON is counted and changes nothing.
        let j = d(Format::Json);
        let before = s.values.clone();
        s.take(b"{oops", &j);
        assert_eq!(s.counts.malformed, 1);
        assert_eq!(s.values, before);
        // Blank messages are skipped; one over the cap is counted.
        s.take(b"  \r", &l);
        assert_eq!(s.counts.messages, 3);
        s.take(&vec![b'1'; l.max + 1], &l);
        assert_eq!(s.counts.too_long, 1);
    }

    #[test]
    fn a_store_caps_its_channels_and_columns() {
        let j = d(Format::Json);
        let mut s = Store::default();
        for k in 0..3 {
            let msg = format!(
                "{{{}}}",
                (0..1000).map(|i| format!("\"k{k}_{i}\": {i}")).collect::<Vec<_>>().join(",")
            );
            s.take(msg.as_bytes(), &j);
        }
        assert_eq!(s.values.len(), MAX_CHANNELS);
        assert_eq!(s.columns.len(), MAX_COLUMNS);
        assert!(s.counts.over_cap > 0);
        assert!(s.table().iter().all(|r| r.len() == MAX_COLUMNS));
    }

    #[test]
    fn datagrams_split_lines_but_not_bytes() {
        let l = d(Format::Lines);
        let mut s = Store::default();
        s.datagram(b"a=1\r\nb=2\n\n", &l);
        assert_eq!(values(&s), [("a", 1.0), ("b", 2.0)]);
        let b = d(Format::Bytes);
        let mut s = Store::default();
        s.datagram(b"\n\n", &b);
        assert_eq!(values(&s), [("chan0", 10.0), ("chan1", 10.0)]);
    }

    #[test]
    fn random_messages_never_panic() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = b"0123456789.,;:= \t\r\n{}[]\"-eE_abxyz\\\xff\x00";
        let formats = [d(Format::Lines), d(Format::Json), d(Format::Bytes)];
        let mut stores = [Store::default(), Store::default(), Store::default()];
        for _ in 0..20_000 {
            let n = usize::try_from(next() % 80).unwrap();
            let msg: Vec<u8> =
                (0..n).map(|_| alphabet[usize::try_from(next()).unwrap() % alphabet.len()]).collect();
            for (s, f) in stores.iter_mut().zip(&formats) {
                s.take(&msg, f);
                s.datagram(&msg, f);
            }
        }
        for s in &stores {
            assert!(s.values.len() <= MAX_CHANNELS && s.rows.len() <= 16);
            assert!(s.values.iter().all(|(_, v)| v.is_finite()));
        }
    }
}
