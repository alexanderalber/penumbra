//! The HTTP hub over real sockets on 127.0.0.1, listeners on port 0 (any free port): requests,
//! a WebSocket round trip, files, and hostile clients (garbage, floods, huge bodies, slow
//! senders, too many connections).

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use penumbra_core::scratch::ScratchDir;
use penumbra_http::hub::{Config, Hub, Params, Want};
use penumbra_osc::hub::Via;
use serde_json::{Value as Json, json};

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn want(params: Params) -> Want {
    Want { port: 0, lan: false, params, interval_ms: 20 }
}

/// A hub listening on any free port; the OSC hub it feeds; the port it got.
fn hub_on_any_port(params: Params, config: Config) -> (Hub, penumbra_osc::Hub, u16) {
    let osc = penumbra_osc::Hub::new();
    let hub = Hub::new(osc.clone());
    hub.set_config(config);
    hub.set_wanted(&[want(params)]);
    let mut port = 0;
    until("the listener to bind", || {
        port = hub
            .listener(0)
            .and_then(|s| s.bound)
            .and_then(|b| b.rsplit(':').next().and_then(|p| p.parse().ok()))
            .unwrap_or(0);
        port != 0
    });
    (hub, osc, port)
}

fn connect(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

/// Sends `raw` and reads the whole response (the server closes every connection).
fn http(port: u16, raw: &[u8]) -> String {
    let mut s = connect(port);
    s.write_all(raw).ok();
    s.shutdown(std::net::Shutdown::Write).ok();
    let mut out = Vec::new();
    s.read_to_end(&mut out).ok();
    String::from_utf8_lossy(&out).into_owned()
}

fn post(port: u16, body: &str) -> String {
    http(
        port,
        format!(
            "POST /set HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn get(port: u16, target: &str) -> String {
    http(port, format!("GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes())
}

fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or("")
}

fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").map_or("", |(_, b)| b)
}

fn values(hub: &Hub, address: &str) -> Option<Vec<f64>> {
    hub.read(0)?.entries.iter().find(|e| e.address == address).map(|e| e.values.clone())
}

/// A WebSocket client: handshake done.
fn ws_connect(port: u16) -> TcpStream {
    let mut s = connect(port);
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let req = format!(
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: http://127.0.0.1:{port}\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    let head = String::from_utf8(head).unwrap();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
    s
}

fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [1u8, 2, 3, 4];
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n <= 65535 => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
    out
}

/// Reads one server frame: opcode and payload; `None` when the connection closed.
fn read_frame(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).ok()?;
    let len = match h[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).ok()?;
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).ok()?;
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut p = vec![0u8; len];
    s.read_exact(&mut p).ok()?;
    Some((h[0] & 0x0F, p))
}

/// Reads text frames until one satisfies `f`.
fn ws_until(s: &mut TcpStream, what: &str, f: impl Fn(&Json) -> bool) -> Json {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        let (op, p) = read_frame(s).unwrap_or_else(|| panic!("closed while waiting for {what}"));
        if op != 1 {
            continue;
        }
        let v: Json = serde_json::from_slice(&p).unwrap();
        if f(&v) {
            return v;
        }
    }
}

#[test]
fn post_get_and_state() {
    let (hub, osc, port) = hub_on_any_port(Params::All, Config::default());
    let r = post(port, r#"{"/fader": 0.5, "go": 1, "/xy": [0.25, 0.75], "/cues": [["a", 1], ["b", 2]]}"#);
    assert!(status_line(&r).contains("200"), "{r}");
    assert_eq!(serde_json::from_str::<Json>(body(&r)).unwrap(), json!({"ok": true, "addresses": 4}));
    assert_eq!(values(&hub, "/fader"), Some(vec![0.5]));
    assert_eq!(values(&hub, "/xy"), Some(vec![0.25, 0.75]));
    assert_eq!(hub.tables(0).unwrap().list[0].1[1], vec!["b".to_string(), "2".into()]);
    let r = get(port, "/set?fader=0.25&name=x%20y");
    assert!(status_line(&r).contains("200"), "{r}");
    assert_eq!(values(&hub, "/fader"), Some(vec![0.25]));
    assert_eq!(
        hub.read(0).unwrap().entries.iter().find(|e| e.address == "/name").unwrap().text.as_deref(),
        Some("x y")
    );
    // The params queue of the OSC hub has them, the latest per address, marked as HTTP.
    let q = osc.take_params();
    let fader = q.iter().find(|m| m.address == "/fader").unwrap();
    assert_eq!((fader.via, fader.controls_only), (Via::Http, false));
    assert_eq!(fader.args, vec![penumbra_osc::Arg::Double(0.25)]);
    // The state once.
    let state: Json = serde_json::from_str(body(&get(port, "/state"))).unwrap();
    assert_eq!(state["in"]["/fader"], 0.25);
    assert_eq!(state["in"]["/xy"], json!([0.25, 0.75]));
    assert_eq!(state["controls"], json!([]));
    // Injecting does the same as a client.
    assert_eq!(hub.inject(0, r#"{"/fader": 1}"#), Ok(1));
    assert_eq!(values(&hub, "/fader"), Some(vec![1.0]));
    assert!(hub.inject(0, "nope").is_err());
    assert!(hub.inject(1, "{}").is_err(), "nothing listens on port 1 by that name");
    let st = hub.listener(0).unwrap();
    assert_eq!(st.messages, 7, "{st:?}");
    assert_eq!(st.last_addresses[0], "/fader");
    hub.shutdown();
}

#[test]
fn a_websocket_round_trip_with_published_channels_and_controls() {
    let (hub, _osc, port) = hub_on_any_port(Params::Controls, Config::default());
    hub.set_controls(std::sync::Arc::new(|| json!([{"path": "/fader", "value": 0.5}])));
    let publisher = hub.publisher(0);
    publisher.set(None, vec![("lfo".into(), 0.3)]);
    let mut ws = ws_connect(port);
    let first = ws_until(&mut ws, "the first state", |_| true);
    assert_eq!(first["chans"]["lfo"], 0.3);
    assert_eq!(first["controls"][0]["path"], "/fader");
    ws.write_all(&client_frame(1, br#"{"/slider": 0.75}"#)).unwrap();
    let v = ws_until(&mut ws, "the echo", |v| v["in"]["/slider"] == 0.75);
    assert_eq!(v["chans"]["lfo"], 0.3);
    publisher.set(None, vec![("lfo".into(), 0.6)]);
    ws_until(&mut ws, "the new channel value", |v| v["chans"]["lfo"] == 0.6);
    // An error comes back as a message; the connection stays.
    ws.write_all(&client_frame(1, b"{not json")).unwrap();
    ws_until(&mut ws, "the error", |v| v["error"].as_str().is_some_and(|e| e.contains("not JSON")));
    // A ping is answered.
    ws.write_all(&client_frame(9, b"hi")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        let (op, p) = read_frame(&mut ws).unwrap();
        if op == 0xA {
            assert_eq!(p, b"hi");
            break;
        }
    }
    let st = hub.listener(0).unwrap();
    assert_eq!(st.websockets.len(), 1);
    assert_eq!(st.websockets[0].messages, 2);
    // Closing is answered and the socket is gone.
    ws.write_all(&client_frame(8, &1000u16.to_be_bytes())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        match read_frame(&mut ws) {
            Some((8, _)) | None => break,
            Some(_) => {}
        }
    }
    until("the socket to be gone", || hub.listener(0).unwrap().websockets.is_empty());
    drop(publisher);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(hub.state(0).unwrap()["chans"], json!({}));
    hub.shutdown();
}

#[test]
fn files_from_the_folder_and_nothing_else() {
    let dir = ScratchDir::new("http_hub_files").unwrap();
    let www = dir.path().join("www");
    std::fs::create_dir_all(&www).unwrap();
    std::fs::write(www.join("index.html"), "<p>page</p>").unwrap();
    std::fs::write(dir.path().join("project.ron"), "secret").unwrap();
    let (hub, _osc, port) = hub_on_any_port(Params::Off, Config::default());
    assert!(get(port, "/").contains("404"), "no folder published yet");
    let publisher = hub.publisher(0);
    publisher.set(Some(www.clone()), Vec::new());
    let r = get(port, "/");
    assert!(status_line(&r).contains("200"), "{r}");
    assert!(r.contains("Content-Type: text/html"), "{r}");
    assert_eq!(body(&r), "<p>page</p>");
    assert!(get(port, "/index.html").ends_with("<p>page</p>"));
    for bad in ["/../project.ron", "/%2e%2e/project.ron", "/..%5cproject.ron", "/%2e%2e%2fproject.ron"] {
        let r = get(port, bad);
        assert!(!r.contains("secret"), "{bad}: {r}");
        assert!(status_line(&r).contains("403") || status_line(&r).contains("404"), "{bad}: {r}");
    }
    assert!(get(port, "/missing.js").contains("404"));
    let r = http(port, format!("DELETE / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes());
    assert!(status_line(&r).contains("405"), "{r}");
    let st = hub.listener(0).unwrap();
    assert_eq!(st.files, 2);
    assert!(st.folder.is_some());
    hub.shutdown();
}

#[test]
fn other_sites_are_refused() {
    let (hub, _osc, port) = hub_on_any_port(Params::All, Config::default());
    let raw = |extra: &str| {
        http(port, format!("GET /set?a=1 HTTP/1.1\r\nHost: localhost:{port}\r\n{extra}\r\n").as_bytes())
    };
    assert!(status_line(&raw("Origin: http://evil.example.com\r\n")).contains("403"));
    assert!(status_line(&raw("Sec-Fetch-Site: cross-site\r\n")).contains("403"));
    let rebinding =
        http(port, format!("GET /set?a=1 HTTP/1.1\r\nHost: evil.example.com:{port}\r\n\r\n").as_bytes());
    assert!(status_line(&rebinding).contains("403"), "{rebinding}");
    assert!(hub.read(0).unwrap().entries.is_empty());
    assert_eq!(hub.listener(0).unwrap().forbidden, 3);
    hub.shutdown();
}

#[test]
fn garbage_is_counted_and_the_server_keeps_answering() {
    let (hub, _osc, port) = hub_on_any_port(Params::Off, Config::default());
    // Random bytes, broken heads, broken bodies.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..40 {
        let len = (next() % 600) as usize + 1;
        let mut bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        if i % 2 == 0 {
            bytes.extend_from_slice(b"\r\n\r\n");
        }
        http(port, &bytes);
    }
    for body in ["", "{", "[1,2]", "{\"/a\": {\"b\": 1}}", "\u{0}\u{1}", "{\"/a b\": 1}"] {
        let r = post(port, body);
        assert!(status_line(&r).contains("400"), "{body}: {r}");
    }
    let r =
        http(port, b"POST /set HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n");
    assert!(status_line(&r).contains("501"), "{r}");
    // A WebSocket that sends an unmasked frame is closed with 1002.
    let mut ws = ws_connect(port);
    ws.write_all(&[0x81, 0x02, b'h', b'i']).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        match read_frame(&mut ws) {
            Some((8, p)) => {
                assert_eq!(u16::from_be_bytes([p[0], p[1]]), 1002);
                break;
            }
            Some(_) => {}
            None => panic!("closed without a close frame"),
        }
    }
    // Random frames after a handshake: closed or ignored, never a crash.
    for _ in 0..20 {
        let mut ws = ws_connect(port);
        let len = (next() % 300) as usize + 1;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        ws.write_all(&bytes).ok();
    }
    let st = hub.listener(0).unwrap();
    assert!(st.malformed >= 27, "{st:?}");
    assert!(status_line(&post(port, r#"{"/ok": 1}"#)).contains("200"));
    assert_eq!(values(&hub, "/ok"), Some(vec![1.0]));
    hub.shutdown();
}

#[test]
fn bodies_and_frames_over_the_cap_are_refused() {
    let config = Config { max_body: 1024, ..Config::default() };
    let (hub, _osc, port) = hub_on_any_port(Params::Off, config);
    let big = format!("{{\"/a\": \"{}\"}}", "x".repeat(2000));
    let r = post(port, &big);
    assert!(status_line(&r).contains("413"), "{r}");
    // Stated but not sent: refused from the header alone.
    let r = http(port, b"POST /set HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000000\r\n\r\n");
    assert!(status_line(&r).contains("413"), "{r}");
    let r = http(port, format!("GET /{} HTTP/1.1\r\nHost: localhost\r\n\r\n", "a".repeat(10_000)).as_bytes());
    assert!(status_line(&r).contains("431"), "{r}");
    let mut ws = ws_connect(port);
    ws.write_all(&client_frame(1, big.as_bytes())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline);
        match read_frame(&mut ws) {
            Some((8, p)) => {
                assert_eq!(u16::from_be_bytes([p[0], p[1]]), 1009);
                break;
            }
            Some(_) => {}
            None => panic!("closed without a close frame"),
        }
    }
    assert_eq!(hub.listener(0).unwrap().oversize, 3);
    assert!(hub.read(0).unwrap().entries.is_empty());
    hub.shutdown();
}

#[test]
fn floods_slow_senders_and_crowds_are_capped() {
    let config = Config {
        rate: 20.0,
        burst: 20.0,
        max_connections: 6,
        max_per_client: 6,
        timeout: 0.3,
        ..Config::default()
    };
    let (hub, _osc, port) = hub_on_any_port(Params::Off, config);
    // A WebSocket flood: the burst gets through, the rest is dropped and counted.
    let mut ws = ws_connect(port);
    for i in 0..500 {
        ws.write_all(&client_frame(1, format!("{{\"/n\": {i}}}").as_bytes())).unwrap();
    }
    until("the flood to be read", || {
        hub.listener(0).unwrap().websockets.first().is_some_and(|w| w.messages == 500)
    });
    let st = hub.listener(0).unwrap();
    assert!(st.rate_dropped >= 450, "{st:?}");
    assert!(st.messages < 50, "{st:?}");
    drop(ws);
    until("the socket to close", || hub.listener(0).unwrap().connections == 0);
    // Slow senders are cut off after the timeout (once the bucket has a token again).
    std::thread::sleep(Duration::from_millis(200));
    let mut slow = connect(port);
    slow.write_all(b"GET / HTTP/1.1\r\nHost: local").unwrap();
    let started = Instant::now();
    let mut out = Vec::new();
    slow.read_to_end(&mut out).ok();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(String::from_utf8_lossy(&out).contains("408"));
    assert_eq!(hub.listener(0).unwrap().timeouts, 1);
    // Wait for the bucket to fill again, then crowd the port: six idle connections, the
    // seventh is turned away.
    std::thread::sleep(Duration::from_millis(1100));
    let idle: Vec<TcpStream> = (0..6).map(|_| connect(port)).collect();
    until("six open", || hub.listener(0).unwrap().connections == 6);
    let r = get(port, "/state");
    assert!(status_line(&r).contains("503"), "{r}");
    assert!(hub.listener(0).unwrap().refused >= 1);
    drop(idle);
    until("the idle ones to time out", || hub.listener(0).unwrap().connections == 0);
    // Connections over the rate are turned away with 429.
    let mut codes = Vec::new();
    for _ in 0..40 {
        codes.push(status_line(&get(port, "/state")).to_string());
    }
    assert!(codes.iter().any(|c| c.contains("429")), "{codes:?}");
    hub.shutdown();
}

#[test]
fn learn_hears_http_and_passes_over_busy_addresses() {
    let (hub, osc, port) = hub_on_any_port(Params::Off, Config::default());
    post(port, r#"{"/stream": 1}"#);
    osc.learn(true);
    post(port, r#"{"/stream": 2, "/label": "x"}"#);
    assert!(osc.take_heard().is_none(), "a busy address and a string");
    post(port, r#"{"/knob": 0.5}"#);
    let heard = osc.take_heard().unwrap();
    assert_eq!((heard.via, heard.address.as_str(), heard.values), (Via::Http, "/knob", 1));
    assert!(osc.take_params().is_empty(), "params off");
    hub.shutdown();
}

#[test]
fn switching_off_closes_and_wants_merge() {
    let osc = penumbra_osc::Hub::new();
    let hub = Hub::new(osc);
    hub.set_wanted(&[
        Want { port: 0, lan: false, params: Params::Off, interval_ms: 50 },
        Want { port: 0, lan: false, params: Params::Controls, interval_ms: 20 },
    ]);
    assert_eq!(hub.wanted(), vec![Want { port: 0, lan: false, params: Params::Controls, interval_ms: 20 }]);
    hub.set_config(Config { enabled: false, ..Config::default() });
    assert!(hub.listener(0).is_none());
    hub.set_config(Config::default());
    assert!(hub.listener(0).is_some());
    hub.set_wanted(&[]);
    assert!(hub.read(0).is_none());
    hub.shutdown();
}
