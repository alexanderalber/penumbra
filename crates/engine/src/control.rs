//! The control channel: JSON lines over TCP on `127.0.0.1`, one request per line, one reply per
//! line.
//!
//! Request: `{"id": 1, "method": "ping", "params": {}}` (`id` and `params` optional).
//! Reply: `{"id": 1, "ok": true, "result": ...}` or `{"id": 1, "ok": false, "error": "..."}`.
//!
//! Hardening: loopback only; a line that is not a JSON object with a string `method` (an HTTP
//! request line from a web page's `fetch`, invalid UTF-8, binary junk) or that is longer than
//! [`MAX_LINE`] gets one error reply and the connection is closed, so nothing sent after it runs;
//! at most [`MAX_CONNECTIONS`] connections at once.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

/// Port used when neither `--port` nor [`PORT_ENV`] says otherwise.
pub const DEFAULT_PORT: u16 = 9880;
/// Environment variable that overrides [`DEFAULT_PORT`] for both the app and `penumbra-cli`.
pub const PORT_ENV: &str = "PENUMBRA_PORT";
/// Longest accepted request line in bytes, without the newline.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;
/// Longest reply the client accepts.
pub const MAX_REPLY: usize = 64 * 1024 * 1024;

/// Answers one request: `(method, params)` to a reply envelope without `id`.
pub type Handler = dyn Fn(&str, &Value) -> Value + Send + Sync;

/// The port from [`PORT_ENV`], or [`DEFAULT_PORT`] when it is unset.
pub fn port_from_env() -> Result<u16, String> {
    match std::env::var(PORT_ENV) {
        Ok(text) => parse_port(&text).map_err(|e| format!("{PORT_ENV}: {e}")),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_PORT),
        Err(e) => Err(format!("{PORT_ENV}: {e}")),
    }
}

pub fn parse_port(text: &str) -> Result<u16, String> {
    text.trim().parse::<u16>().map_err(|_| format!("`{text}` is not a port number (0-65535)"))
}

/// Bind `127.0.0.1:port` (0 picks a free port) and serve requests on background threads.
/// Returns the bound address; the server lives as long as the process.
pub fn serve(port: u16, handler: Arc<Handler>) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let addr = listener.local_addr()?;
    std::thread::Builder::new()
        .name("control-accept".into())
        .spawn(move || accept_loop(listener, handler))?;
    Ok(addr)
}

/// Gives the connection slot back when the connection ends, however it ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: TcpListener, handler: Arc<Handler>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("control channel: accept failed: {e}");
                continue;
            }
        };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let slot = Slot(Arc::clone(&open));
        let handler = Arc::clone(&handler);
        let spawned = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
            let _slot = slot;
            serve_stream(stream, handler.as_ref());
        });
        if let Err(e) = spawned {
            log::warn!("control channel: cannot start a connection thread: {e}");
        }
    }
}

fn serve_stream(stream: TcpStream, handler: &Handler) {
    let _ = stream.set_nodelay(true);
    let Ok(read) = stream.try_clone() else { return };
    serve_lines(read, stream, handler);
}

/// One parsed request line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let value = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => {
            return Line::Reject(format!(
                "not a control request (bad JSON: {e}); expected one JSON object per line; \
                 closing the connection"
            ));
        }
    };
    let Value::Object(mut obj) = value else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(Value::String(method)) = obj.remove("method") else {
        return Line::Reject(
            "not a control request (missing string `method`); closing the connection".into(),
        );
    };
    Line::Request {
        id: obj.remove("id").unwrap_or(Value::Null),
        method,
        params: obj.remove("params").unwrap_or_else(|| json!({})),
    }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf`, newline stripped.
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds what one line can make us buffer.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("line longer than {max} bytes; closing the connection"));
    }
    // A last line without newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes or sends something that
/// is not a request. `handle` gets `(method, params)` and returns the reply; the request's `id`
/// is added here.
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, mut handle: impl FnMut(&str, &Value) -> Value) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(e) => e,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params } => {
                    let mut reply = handle(&method, &params);
                    if let Some(obj) = reply.as_object_mut() {
                        obj.insert("id".into(), id);
                    }
                    if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
        return;
    }
}

/// Send one request to the control channel on `127.0.0.1:port` and return the reply envelope.
pub fn call(port: u16, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("cannot reach the control channel on {addr} (is penumbra running?): {e}"))?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let request = json!({"id": 1, "method": method, "params": params});
    // A server that refuses the connection (too many open) writes its error and closes before
    // reading, so a failed send may still leave a reply to read.
    let sent = writeln!(stream, "{request}").and_then(|()| stream.flush());
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    match (read_bounded_line(&mut reader, &mut buf, MAX_REPLY), sent) {
        (Ok(true), _) => serde_json::from_slice(&buf).map_err(|e| format!("reply is not JSON: {e}")),
        (_, Err(e)) => Err(format!("send failed: {e}")),
        (Ok(false), Ok(())) => Err("the control channel closed the connection without a reply".into()),
        (Err(e), Ok(())) => Err(format!("reading the reply failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, |m, _p| {
            calls.push(m.to_string());
            json!({"ok": true, "result": null})
        });
        let replies = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect();
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) =
            run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":\"x\",\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}");
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], "x");
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn http_request_is_rejected_before_its_body_runs() {
        // What a web page's cross-origin `fetch` to the port sends: the request line is rejected
        // and the connection closed, so the JSON body never runs.
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:9880\r\nOrigin: https://evil.example\r\n\
            Content-Type: text/plain\r\n\r\n{\"id\":7,\"method\":\"ping\"}\n";
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
    }

    #[test]
    fn non_request_lines_close_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"\"ping\"\n{\"method\":\"a\"}\n",
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
            b"{\"method\":\"a\"\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn requests_before_junk_still_run() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused() {
        let mut input = vec![b'x'; MAX_LINE + 1];
        input.extend_from_slice(b"\n{\"method\":\"a\"}\n");
        let (r, calls) = run(&input);
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let mut input = vec![b'a'; 10];
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, vec![b'a'; 10]);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    #[test]
    fn ports_parse() {
        assert_eq!(parse_port(" 9880 "), Ok(9880));
        assert!(parse_port("70000").is_err());
        assert!(parse_port("abc").is_err());
    }

    fn start_registry() -> SocketAddr {
        let registry = Arc::new(crate::builtin_registry());
        serve(0, Arc::new(move |m: &str, p: &Value| registry.dispatch(m, p))).unwrap()
    }

    #[test]
    fn ping_over_tcp() {
        let addr = start_registry();
        let r = call(addr.port(), "ping", json!({}), Duration::from_secs(5)).unwrap();
        assert_eq!(r["ok"], true);
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["pong"], true);
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        let addr = start_registry();
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"ping\"}\nhello\n{\"method\":\"ping\"}\n").unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap();
        let replies: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 2, "{text}");
        assert_eq!(replies[0]["id"], 7);
        assert_eq!(replies[0]["ok"], true);
        assert_eq!(replies[1]["ok"], false);
    }

    #[test]
    fn connections_beyond_the_limit_are_refused_and_slots_come_back() {
        let addr = start_registry();
        let held: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| TcpStream::connect(addr).unwrap()).collect();
        // The accept thread counts connections in order, so once the last held connection
        // answers a ping, all held ones are counted.
        let mut last = held.last().unwrap().try_clone().unwrap();
        last.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        writeln!(last, "{{\"method\":\"ping\"}}").unwrap();
        let mut line = String::new();
        BufReader::new(last.try_clone().unwrap()).read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"), "{line}");

        // Refused: either the error line arrives, or (Windows) the early close resets the
        // connection before the client reads it. Never a successful ping.
        match call(addr.port(), "ping", json!({}), Duration::from_secs(5)) {
            Ok(r) => {
                assert_eq!(r["ok"], false);
                assert!(r["error"].as_str().unwrap().contains("too many"));
            }
            Err(e) => assert!(e.contains("failed"), "{e}"),
        }

        drop(last);
        drop(held);
        // Slots come back once the server notices the closed connections.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let r = call(addr.port(), "ping", json!({}), Duration::from_secs(5));
            if r.is_ok_and(|r| r["ok"] == true) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "slots never came back");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
