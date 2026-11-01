//! What a message says: a JSON object of addresses and values (`{"/fader": 0.5, "/go": 1}`),
//! or the pairs of a query string (`?fader=0.5&go=1`). Each address becomes an OSC message, so
//! the OSC hub's channels, params by address and learn apply unchanged:
//!
//! - a number, `true`/`false` (1 and 0) or a string: one value;
//! - an array of those: several values (`"/xy": [0.2, 0.8]` makes channels `xy_0` and `xy_1`);
//! - an array of arrays, or an array of objects: a table (rows of cells; for objects the keys
//!   of all objects are the first row, in the order they first came);
//! - `null` or an empty array: the address with no value.
//!
//! An address without a leading `/` gets one. Every count is capped ([`Caps`]).
//!
//! One key is not an address: `"press"` with an object (`{"press": {"/go": true}}`) presses
//! momentary `chan.button`s by node path: `true` or a number of 0.5 or more holds the button,
//! `false` or a smaller number lets go ([`Parsed::presses`]). No address takes an object, so
//! the key stays free for addresses otherwise (`{"press": 1}` is the address `/press`).

use penumbra_osc::codec::{Arg, Message};
use serde_json::Value as Json;

/// Caps on one message.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Addresses in one message.
    pub addresses: usize,
    /// Values of one address.
    pub values: usize,
    /// Bytes of an address.
    pub address_len: usize,
    /// Tables in one message.
    pub tables: usize,
    /// Rows and columns of a table.
    pub rows: usize,
    pub cols: usize,
    /// Characters of one string or cell.
    pub text: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self { addresses: 256, values: 64, address_len: 512, tables: 16, rows: 4096, cols: 256, text: 4096 }
    }
}

/// A message taken apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub messages: Vec<Message>,
    /// Address and rows.
    pub tables: Vec<(String, Vec<Vec<String>>)>,
    /// Momentary buttons to press (`true`) or let go, by node path.
    pub presses: Vec<(String, bool)>,
}

impl Parsed {
    /// Addresses in it, values and tables together (presses are not addresses).
    pub fn len(&self) -> usize {
        self.messages.len() + self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0 && self.presses.is_empty()
    }
}

/// `key` as an address: a leading `/` added, no spaces or control characters, not empty.
pub fn address(key: &str, caps: &Caps) -> Result<String, String> {
    let key = key.trim();
    let a = if key.starts_with('/') { key.to_string() } else { format!("/{key}") };
    if a.len() < 2 {
        return Err("an empty address".into());
    }
    if a.len() > caps.address_len {
        return Err(format!("an address longer than {} bytes", caps.address_len));
    }
    if a.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("`{}` has spaces or control characters", short(&a)));
    }
    Ok(a)
}

fn short(s: &str) -> String {
    let mut t: String = s.chars().take(40).collect();
    if t.len() < s.len() {
        t.push_str("...");
    }
    t
}

fn text(s: &str, caps: &Caps) -> String {
    s.chars().take(caps.text).collect()
}

/// A scalar as an arg; `None` for arrays and objects.
fn scalar(v: &Json, caps: &Caps) -> Option<Arg> {
    Some(match v {
        Json::Null => Arg::Nil,
        Json::Bool(true) => Arg::True,
        Json::Bool(false) => Arg::False,
        Json::Number(n) => Arg::Double(n.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0)),
        Json::String(s) => Arg::Str(text(s, caps)),
        Json::Array(_) | Json::Object(_) => return None,
    })
}

/// A scalar as a table cell.
fn cell(v: &Json, caps: &Caps) -> Result<String, String> {
    Ok(match v {
        Json::Null => String::new(),
        Json::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Json::Number(n) => n.to_string(),
        Json::String(s) => text(s, caps),
        Json::Array(_) | Json::Object(_) => {
            return Err("a table cell is a number, string, boolean or null".into());
        }
    })
}

fn table(items: &[Json], caps: &Caps) -> Result<Vec<Vec<String>>, String> {
    if items.len() > caps.rows {
        return Err(format!("a table of more than {} rows", caps.rows));
    }
    if items.iter().all(Json::is_array) {
        return items
            .iter()
            .map(|row| {
                let cells = row.as_array().map(Vec::as_slice).unwrap_or_default();
                if cells.len() > caps.cols {
                    return Err(format!("a table row of more than {} cells", caps.cols));
                }
                cells.iter().map(|c| cell(c, caps)).collect()
            })
            .collect();
    }
    if items.iter().all(Json::is_object) {
        let mut header: Vec<String> = Vec::new();
        for o in items.iter().filter_map(Json::as_object) {
            for k in o.keys() {
                if !header.contains(k) {
                    if header.len() >= caps.cols {
                        return Err(format!("a table of more than {} columns", caps.cols));
                    }
                    header.push(k.clone());
                }
            }
        }
        let mut rows = vec![header.iter().map(|h| text(h, caps)).collect::<Vec<_>>()];
        for o in items.iter().filter_map(Json::as_object) {
            rows.push(
                header
                    .iter()
                    .map(|h| o.get(h).map_or(Ok(String::new()), |v| cell(v, caps)))
                    .collect::<Result<_, _>>()?,
            );
        }
        return Ok(rows);
    }
    Err("a table is an array of arrays or an array of objects, not a mix".into())
}

/// One address and its value.
fn take(parsed: &mut Parsed, key: &str, v: &Json, caps: &Caps) -> Result<(), String> {
    if parsed.len() >= caps.addresses {
        return Err(format!("more than {} addresses in one message", caps.addresses));
    }
    let a = address(key, caps)?;
    if let Some(arg) = scalar(v, caps) {
        let args = if arg == Arg::Nil { Vec::new() } else { vec![arg] };
        parsed.messages.push(Message::new(a, args));
        return Ok(());
    }
    let Some(items) = v.as_array() else {
        return Err(format!("`{}`: a value is a number, string, boolean, array or null", short(&a)));
    };
    if items.iter().any(|i| i.is_array() || i.is_object()) {
        if parsed.tables.len() >= caps.tables {
            return Err(format!("more than {} tables in one message", caps.tables));
        }
        let rows = table(items, caps).map_err(|e| format!("`{}`: {e}", short(&a)))?;
        parsed.tables.push((a, rows));
        return Ok(());
    }
    if items.len() > caps.values {
        return Err(format!("`{}`: more than {} values", short(&a), caps.values));
    }
    let args: Vec<Arg> = items.iter().filter_map(|i| scalar(i, caps)).filter(|a| *a != Arg::Nil).collect();
    parsed.messages.push(Message::new(a, args));
    Ok(())
}

/// A JSON message: an object of addresses and values.
pub fn parse_json(text: &str, caps: &Caps) -> Result<Parsed, String> {
    let v: Json = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let Json::Object(map) = v else {
        return Err("a message is a JSON object: {\"/fader\": 0.5}".into());
    };
    let mut parsed = Parsed::default();
    for (k, v) in &map {
        if let (Some(presses), "press" | "/press") = (v.as_object(), k.trim()) {
            take_presses(&mut parsed, presses, caps)?;
            continue;
        }
        take(&mut parsed, k, v, caps)?;
    }
    Ok(parsed)
}

/// `{"/go": true, "/stop": false}`: node paths and whether each is held.
fn take_presses(
    parsed: &mut Parsed,
    presses: &serde_json::Map<String, Json>,
    caps: &Caps,
) -> Result<(), String> {
    for (path, v) in presses {
        if parsed.len() + parsed.presses.len() >= caps.addresses {
            return Err(format!("more than {} addresses and presses in one message", caps.addresses));
        }
        let path = address(path, caps)?;
        let down = match v {
            Json::Bool(b) => *b,
            Json::Number(n) => n.as_f64().is_some_and(|x| x >= 0.5),
            _ => return Err(format!("press `{}`: true to hold the button, false to let go", short(&path))),
        };
        parsed.presses.push((path, down));
    }
    Ok(())
}

/// A query string: `fader=0.5&go=1&name=x`. A value that reads as a number is one, `true` and
/// `false` are 1 and 0, numbers separated by commas are several values, anything else is a
/// string; a key without a value has none.
pub fn parse_query(query: &str, caps: &Caps) -> Result<Parsed, String> {
    let pairs = crate::request::query_pairs(query).map_err(str::to_string)?;
    let mut parsed = Parsed::default();
    for (k, v) in pairs {
        let value = query_value(&v);
        take(&mut parsed, &k, &value, caps)?;
    }
    Ok(parsed)
}

fn query_value(v: &str) -> Json {
    let number = |s: &str| s.trim().parse::<f64>().ok().filter(|x| x.is_finite());
    if v.is_empty() {
        return Json::Null;
    }
    match v {
        "true" => return Json::Bool(true),
        "false" => return Json::Bool(false),
        _ => {}
    }
    if let Some(x) = number(v) {
        return serde_json::json!(x);
    }
    if v.contains(',') {
        let parts: Option<Vec<f64>> = v.split(',').map(number).collect();
        if let Some(xs) = parts {
            return serde_json::json!(xs);
        }
    }
    Json::String(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Caps {
        Caps::default()
    }

    #[test]
    fn json_messages() {
        let p = parse_json(
            r#"{"/fader": 0.5, "go": true, "xy": [0.25, 1], "label": "hi", "/bang": null}"#,
            &caps(),
        )
        .unwrap();
        let find = |a: &str| p.messages.iter().find(|m| m.address == a).unwrap().args.clone();
        assert_eq!(find("/fader"), vec![Arg::Double(0.5)]);
        assert_eq!(find("/go"), vec![Arg::True]);
        assert_eq!(find("/xy"), vec![Arg::Double(0.25), Arg::Double(1.0)]);
        assert_eq!(find("/label"), vec![Arg::Str("hi".into())]);
        assert_eq!(find("/bang"), vec![]);
        assert!(p.tables.is_empty());
    }

    #[test]
    fn json_tables() {
        let p = parse_json(r#"{"/cues": [["a", 1], ["b", 2.5, true]]}"#, &caps()).unwrap();
        assert_eq!(
            p.tables,
            vec![(
                "/cues".to_string(),
                vec![vec!["a".into(), "1".into()], vec!["b".into(), "2.5".into(), "1".into()]]
            )]
        );
        let p = parse_json(r#"{"t": [{"name": "a", "x": 1}, {"x": 2, "y": null}]}"#, &caps()).unwrap();
        assert_eq!(
            p.tables[0].1,
            vec![
                vec!["name".to_string(), "x".into(), "y".into()],
                vec!["a".into(), "1".into(), "".into()],
                vec!["".into(), "2".into(), "".into()]
            ]
        );
        assert!(parse_json(r#"{"t": [[1], {"a": 1}]}"#, &caps()).is_err());
        assert!(parse_json(r#"{"t": [[[1]]]}"#, &caps()).is_err());
    }

    #[test]
    fn presses_by_node_path() {
        let p = parse_json(r#"{"press": {"/go": true, "stage/stop": 0, "/x": 0.75}, "/fader": 1}"#, &caps())
            .unwrap();
        assert_eq!(
            p.presses,
            vec![("/go".to_string(), true), ("/x".into(), true), ("/stage/stop".into(), false)]
        );
        assert_eq!(p.len(), 1, "presses are not addresses");
        let p = parse_json(r#"{"press": 1}"#, &caps()).unwrap();
        assert_eq!(p.messages[0].address, "/press", "a number makes the address /press");
        assert!(parse_json(r#"{"press": {"/go": "yes"}}"#, &caps()).is_err());
        assert!(parse_json(r#"{"press": {"": true}}"#, &caps()).is_err());
        let many: String = format!(
            "{{\"press\": {{{}}}}}",
            (0..300).map(|i| format!("\"/b{i}\": true")).collect::<Vec<_>>().join(",")
        );
        assert!(parse_json(&many, &caps()).is_err());
        assert!(!parse_json(r#"{"press": {"/go": true}}"#, &caps()).unwrap().is_empty());
    }

    #[test]
    fn json_garbage_is_an_error() {
        for bad in [
            "",
            "nul",
            "[1, 2]",
            "1",
            "\"x\"",
            r#"{"/a": {"b": 1}}"#,
            r#"{"/a b": 1}"#,
            r#"{"": 1}"#,
            r#"{"/a": 1"#,
            "{\"/a\": 1}}",
        ] {
            assert!(parse_json(bad, &caps()).is_err(), "{bad}");
        }
        let many: String =
            format!("{{{}}}", (0..300).map(|i| format!("\"/a{i}\": {i}")).collect::<Vec<_>>().join(","));
        assert!(parse_json(&many, &caps()).unwrap_err().contains("more than 256"));
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(parse_json(&deep, &caps()).is_err(), "deep nesting stops at serde_json's limit");
        let long = format!("{{\"/{}\": 1}}", "x".repeat(600));
        assert!(parse_json(&long, &caps()).is_err());
    }

    #[test]
    fn query_messages() {
        let p = parse_query("fader=0.5&go=true&xy=0.1,0.9&name=a+b&bang&%2Fs%2Ft=2", &caps()).unwrap();
        let find = |a: &str| p.messages.iter().find(|m| m.address == a).unwrap().args.clone();
        assert_eq!(find("/fader"), vec![Arg::Double(0.5)]);
        assert_eq!(find("/go"), vec![Arg::True]);
        assert_eq!(find("/xy"), vec![Arg::Double(0.1), Arg::Double(0.9)]);
        assert_eq!(find("/name"), vec![Arg::Str("a b".into())]);
        assert_eq!(find("/bang"), vec![]);
        assert_eq!(find("/s/t"), vec![Arg::Double(2.0)]);
        assert!(parse_query("a=%zz", &caps()).is_err());
    }
}
