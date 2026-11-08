//! Channels and table rows as messages to send: lines, JSON or bytes. The delimiter is not part
//! of a message; the link adds it on a stream.

/// A number as text: the shortest form that reads back the same, `e` notation for very large
/// and very small ones. Not finite is 0, which every receiver can read.
pub fn number(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let a = v.abs();
    if a != 0.0 && !(1e-6..1e15).contains(&a) { format!("{v:e}") } else { format!("{v}") }
}

/// Channels as one line: their values joined by `separator`, each `name:value` with `names`.
pub fn chans_line(chans: &[(&str, f64)], separator: &str, names: bool) -> Vec<u8> {
    let parts: Vec<String> =
        chans.iter().map(|(n, v)| if names { format!("{n}:{}", number(*v)) } else { number(*v) }).collect();
    parts.join(separator).into_bytes()
}

/// Channels as one JSON object, in their order.
pub fn chans_json(chans: &[(&str, f64)]) -> Vec<u8> {
    let parts: Vec<String> = chans.iter().map(|(n, v)| format!("{}:{}", quote(n), number(*v))).collect();
    format!("{{{}}}", parts.join(",")).into_bytes()
}

/// Channels as one byte each: rounded and held to 0 to 255.
pub fn chans_bytes(chans: &[(&str, f64)]) -> Vec<u8> {
    chans.iter().map(|(_, v)| byte(*v)).collect()
}

/// A table row as one line: its cells as they are, joined by `separator`.
pub fn row_line(row: &[String], separator: &str) -> Vec<u8> {
    row.join(separator).into_bytes()
}

/// A table row as one JSON array: a cell that is a number as a number, any other as a string.
pub fn row_json(row: &[String]) -> Vec<u8> {
    let parts: Vec<String> = row
        .iter()
        .map(|c| match c.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => number(v),
            _ => quote(c),
        })
        .collect();
    format!("[{}]", parts.join(",")).into_bytes()
}

/// A table row as bytes: a cell that is a number is one byte (rounded, held to 0 to 255), any
/// other cell its text. So a row `AT`, `13`, `10` sends `AT\r\n`.
pub fn row_bytes(row: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in row {
        match c.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => out.push(byte(v)),
            _ => out.extend_from_slice(c.as_bytes()),
        }
    }
    out
}

fn byte(v: f64) -> u8 {
    if v.is_nan() {
        return 0;
    }
    // `as`: held to 0..=255 first.
    v.round().clamp(0.0, 255.0) as u8
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_back() {
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(1.0), "1");
        assert_eq!(number(-0.0), "-0");
        assert_eq!(number(1e-7), "1e-7");
        assert_eq!(number(2.5e20), "2.5e20");
        assert_eq!(number(f64::NAN), "0");
        for v in [0.1, 1.0 / 3.0, 123456.789, 1e-300, -7e300] {
            assert_eq!(number(v).parse::<f64>().unwrap(), v);
        }
    }

    #[test]
    fn chans_and_rows_encode() {
        let c = [("x", 0.5), ("y", 1.0)];
        assert_eq!(chans_line(&c, ",", false), b"0.5,1");
        assert_eq!(chans_line(&c, " ", true), b"x:0.5 y:1");
        assert_eq!(chans_json(&c), br#"{"x":0.5,"y":1}"#);
        assert_eq!(chans_bytes(&[("a", 300.0), ("b", -2.0), ("c", 127.5)]), [255, 0, 128]);
        let row = vec!["go".to_string(), "2".to_string(), "a \"b\"".to_string()];
        assert_eq!(row_line(&row, ";"), b"go;2;a \"b\"");
        assert_eq!(row_json(&row), br#"["go",2,"a \"b\""]"#);
        assert_eq!(row_bytes(&["AT".into(), "13".into(), "10".into()]), b"AT\r\n");
        // What the parser makes of it comes back.
        let d = crate::Decode { format: crate::Format::Json, ..crate::Decode::default() };
        let f = crate::parse::fields(&chans_json(&c), &d).unwrap();
        assert_eq!(f.len(), 2);
    }
}
