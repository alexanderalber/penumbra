//! HTTP/1.1 requests, the little the server needs: the request line, the headers, a body of a
//! stated length. Bounded: the head at most [`MAX_HEAD`] bytes and [`MAX_HEADERS`] lines; a
//! chunked body is refused. Every response closes the connection, so there is no keep-alive to
//! track.

use std::io::Write;

/// Bytes of the request line and headers together.
pub const MAX_HEAD: usize = 8192;
/// Header lines.
pub const MAX_HEADERS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// As sent, upper case by convention (`GET`, `POST`).
    pub method: String,
    /// The path, still percent-encoded, without the query.
    pub path: String,
    pub query: Option<String>,
    /// Names in lower case, in the order they came.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The first header named `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Whether header `name` lists `token` (comma-separated, any case): `Connection: keep-alive,
    /// Upgrade` lists `upgrade`.
    pub fn lists(&self, name: &str, token: &str) -> bool {
        self.headers
            .iter()
            .filter(|(n, _)| n == name)
            .any(|(_, v)| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    }

    /// The body's length: 0 without `Content-Length`; an error for a chunked body or a length
    /// that is not a number or stated twice differently.
    pub fn body_length(&self) -> Result<usize, (u16, &'static str)> {
        if self.header("transfer-encoding").is_some() {
            return Err((501, "chunked bodies are not supported; send Content-Length"));
        }
        let mut found: Option<usize> = None;
        for (_, v) in self.headers.iter().filter(|(n, _)| n == "content-length") {
            let v = v.trim();
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err((400, "Content-Length is not a number"));
            }
            let n = v.parse::<usize>().unwrap_or(usize::MAX);
            if found.is_some_and(|f| f != n) {
                return Err((400, "two different Content-Length headers"));
            }
            found = Some(n);
        }
        Ok(found.unwrap_or(0))
    }
}

/// Where the head ends in `buf` (the index after the blank line), if it has.
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parses a request head (up to and including the blank line).
pub fn parse_head(head: &[u8]) -> Result<Request, &'static str> {
    if head.len() > MAX_HEAD {
        return Err("the request head is too long");
    }
    let text = std::str::from_utf8(head).map_err(|_| "the request head is not UTF-8")?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or("no request line")?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("the request line is not `METHOD target HTTP/1.1`");
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err("the method is not a word in capitals");
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err("only HTTP/1.0 and 1.1 are spoken");
    }
    if !target.starts_with('/') || target.bytes().any(|b| b <= b' ' || b == 0x7F) {
        return Err("the target is not a path");
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (target, None),
    };
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err("folded header lines are not allowed");
        }
        let (name, value) = line.split_once(':').ok_or("a header line without a colon")?;
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_!#$%&'*+.^`|~".contains(&b))
        {
            return Err("a header name with characters a name cannot have");
        }
        if headers.len() >= MAX_HEADERS {
            return Err("too many header lines");
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(Request { method: method.to_string(), path: path.to_string(), query, headers })
}

/// Percent-decodes `s` (and `+` as a space when `plus`); an error for a broken escape or bytes
/// that are not UTF-8.
pub fn percent_decode(s: &str, plus: bool) -> Result<String, &'static str> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    while let Some(&b) = bytes.get(i) {
        match b {
            b'%' => {
                let h = bytes.get(i + 1).copied().and_then(hex);
                let l = bytes.get(i + 2).copied().and_then(hex);
                let (Some(h), Some(l)) = (h, l) else { return Err("a broken %-escape") };
                out.push(h * 16 + l);
                i += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "an escape that is not UTF-8")
}

/// The pairs of a query string (`a=1&b=x`), decoded; a key without `=` has an empty value.
pub fn query_pairs(query: &str) -> Result<Vec<(String, String)>, &'static str> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            Ok((percent_decode(k, true)?, percent_decode(v, true)?))
        })
        .collect()
}

/// The reason phrase for the statuses the server sends.
pub fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// A whole response that closes the connection. `body` is left out for `HEAD`.
pub fn response(status: u16, content_type: &str, body: &[u8], head_only: bool) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .into_bytes();
    if !head_only {
        out.extend_from_slice(body);
    }
    out
}

/// A JSON response.
pub fn json_response(status: u16, body: &serde_json::Value) -> Vec<u8> {
    response(status, "application/json", body.to_string().as_bytes(), false)
}

/// A JSON error: `{"error": why}`.
pub fn error_response(status: u16, why: &str) -> Vec<u8> {
    json_response(status, &serde_json::json!({ "error": why }))
}

/// Writes `bytes`, ignoring errors: the client may be gone, and the connection ends anyway.
pub fn send(stream: &mut impl Write, bytes: &[u8]) {
    if stream.write_all(bytes).is_ok() {
        stream.flush().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_parse() {
        let h = b"GET /set?a=1&b=%2Fx HTTP/1.1\r\nHost: localhost:8090\r\nConnection: keep-alive, Upgrade\r\n\r\n";
        assert_eq!(head_end(h), Some(h.len()));
        let r = parse_head(h).unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/set"));
        assert_eq!(r.header("host"), Some("localhost:8090"));
        assert!(r.lists("connection", "upgrade"));
        assert_eq!(
            query_pairs(r.query.as_deref().unwrap()).unwrap(),
            vec![("a".into(), "1".into()), ("b".into(), "/x".into())]
        );
        assert_eq!(r.body_length(), Ok(0));
    }

    #[test]
    fn broken_heads_are_errors() {
        for bad in [
            &b"GET / HTTP/1.1 extra\r\n\r\n"[..],
            b"get / HTTP/1.1\r\n\r\n",
            b"GET / HTTP/2\r\n\r\n",
            b"GET nopath HTTP/1.1\r\n\r\n",
            b"GET / HTTP/1.1\r\nno colon\r\n\r\n",
            b"GET / HTTP/1.1\r\nA: b\r\n folded\r\n\r\n",
            b"GET / HTTP/1.1\r\nBad Name: x\r\n\r\n",
            b"\xFF\xFE / HTTP/1.1\r\n\r\n",
            b"",
        ] {
            assert!(parse_head(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let many = format!("GET / HTTP/1.1\r\n{}\r\n", "A: b\r\n".repeat(MAX_HEADERS + 1));
        assert!(parse_head(many.as_bytes()).is_err());
        let r = |h: &str| parse_head(format!("POST / HTTP/1.1\r\n{h}\r\n\r\n").as_bytes()).unwrap();
        assert_eq!(r("Content-Length: 12").body_length(), Ok(12));
        assert!(r("Content-Length: -1").body_length().is_err());
        assert!(r("Content-Length: 1\r\nContent-Length: 2").body_length().is_err());
        assert_eq!(r("Transfer-Encoding: chunked").body_length().map_err(|e| e.0), Err(501));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c", true).unwrap(), "a b c");
        assert_eq!(percent_decode("a+b", false).unwrap(), "a+b");
        assert_eq!(percent_decode("%C3%A4", false).unwrap(), "ä");
        assert!(percent_decode("%", false).is_err());
        assert!(percent_decode("%zz", false).is_err());
        assert!(percent_decode("%FF", false).is_err());
    }
}
