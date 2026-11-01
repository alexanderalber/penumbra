//! HTTP end to end: loopback TCP on 127.0.0.1, listeners on port 0 (any free port), no outside
//! network.

use std::io::{Read, Write};

use serde_json::{Value as Json, json};

use super::{Fixture, eventually};

/// The port the listener wanted as `wanted` (0 for any) actually bound.
fn http_port(f: &Fixture, wanted: u16) -> u16 {
    let mut port = 0;
    eventually("the HTTP listener to bind", || {
        let bound = f.engine.http().listener(wanted).and_then(|s| s.bound);
        port = bound.and_then(|b| b.rsplit(':').next().and_then(|p| p.parse().ok())).unwrap_or(0);
        port != 0
    });
    port
}

fn http(port: u16, raw: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    s.shutdown(std::net::Shutdown::Write).ok();
    let mut out = String::new();
    s.read_to_string(&mut out).ok();
    out
}

fn post(port: u16, body: &Json) -> String {
    let body = body.to_string();
    http(
        port,
        &format!(
            "POST /set HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn param_value(f: &Fixture, path: &str, name: &str) -> Json {
    let info = f.call("node.info", json!({"path": path}));
    info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).map(|p| p["value"].clone()).unwrap()
}

fn channel(f: &Fixture, path: &str, name: &str) -> Json {
    let read = f.call("chan.read", json!({"path": path}));
    read["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .map_or(Json::Null, |c| c["value"].clone())
}

#[test]
fn a_message_moves_a_channel_and_a_show_control_and_the_page_is_served() {
    let f = Fixture::new("http_in");
    let www = f.engine.session().dir().join("www");
    std::fs::create_dir_all(&www).unwrap();
    std::fs::write(www.join("index.html"), "<h1>control page</h1>").unwrap();
    f.call("node.create", json!({"path": "/http", "kind": "chan.http_in", "params": {"port": 0}}));
    f.call(
        "node.create",
        json!({"path": "/fader", "kind": "chan.slider", "params": {"show": true, "max": 2}}),
    );
    f.call("node.create", json!({"path": "/plain", "kind": "chan.slider"}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 2}}));
    f.call("node.connect", json!({"from": "/k", "to": "/http"}));
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.watch", json!({"path": "/http"}));
    let port = http_port(&f, 0);
    let entries = f.call("history.list", json!({}))["total"].clone();

    // One message: a channel, the value of a control on the show screen, and a param the
    // default `params` (controls) does not let through.
    let r = post(port, &json!({"/speed": 0.5, "/fader/value": 0.25, "/plain/value": 0.9}));
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    eventually("the channel", || channel(&f, "/http", "speed") == 0.5);
    eventually("the control", || param_value(&f, "/fader", "value") == 0.25);
    eventually("the refusal", || f.call("osc.status", json!({}))["params"]["refused"] == 1);
    assert_eq!(param_value(&f, "/plain", "value"), 0.0);
    let status = f.call("osc.status", json!({}));
    assert!(status["params"]["last_refused"].as_str().unwrap().contains("flagged `show`"), "{status}");
    assert_eq!(f.call("history.list", json!({}))["total"], entries, "live values, no history entries");

    // http.send does what a page does.
    let r = f.call("http.send", json!({"message": {"/speed": 0.75, "/fader/value": 1.5}}));
    assert_eq!(r["addresses"], 2, "{r}");
    eventually("the sent channel", || channel(&f, "/http", "speed") == 0.75);
    eventually("the sent control", || param_value(&f, "/fader", "value") == 1.5);
    assert!(f.fail("http.send", json!({"message": [1, 2]})).contains("JSON object"));

    // The state a page gets: what arrived, the input's channels, the controls with the address
    // that sets them.
    eventually("the input's channels in the state", || {
        f.engine.http().state(0).is_some_and(|s| s["chans"]["chan0"] == 2.0)
    });
    let state = f.engine.http().state(0).unwrap();
    assert_eq!(state["in"]["/speed"], 0.75, "{state}");
    assert_eq!(state["controls"].as_array().unwrap().len(), 1, "{state}");
    let fader = &state["controls"][0];
    assert_eq!(fader["path"], "/fader");
    assert_eq!(fader["address"], "/fader/value");
    assert_eq!(
        (fader["value"].clone(), fader["min"].clone(), fader["max"].clone()),
        (json!(1.5), json!(0.0), json!(2.0))
    );

    // The page, from the project's www folder.
    let page = http(port, &format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"));
    assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<h1>control page</h1>"), "{page}");

    // node.info and http.status show the listener.
    let info = f.call("node.info", json!({"path": "/http"}));
    assert!(info["http"]["messages"].as_u64().unwrap() >= 5, "{info}");
    assert!(info["http"]["page"].as_str().unwrap().starts_with("http://localhost:"), "{info}");
    assert_eq!(info["http"]["last_addresses"][0], "/speed", "{info}");
    let status = f.call("http.status", json!({}));
    assert_eq!(status["listeners"][0]["files"], 1, "{status}");

    // Learn hears HTTP too and binds to the chan.http_in's channel.
    f.call("osc.learn", json!({"param": "/lfo.amplitude"}));
    post(port, &json!({"/knob": 0.3}));
    eventually("learned", || f.call("osc.status", json!({}))["learned"]["from"] == "/http");
    let info = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(info["binds"], json!([{"param": "amplitude", "from": "/http:knob"}]), "{info}");

    // The switch closes the port; the node says so.
    f.call("http.config", json!({"enabled": false}));
    assert_eq!(f.call("http.status", json!({}))["listeners"], json!([]));
    eventually("the node to say so", || {
        f.call("node.info", json!({"path": "/http"}))["status"].to_string().contains("switched off")
    });
    f.call("http.config", json!({"enabled": true}));
}

#[test]
fn params_off_sets_none_and_all_any() {
    let f = Fixture::new("http_params");
    f.call(
        "node.create",
        json!({"path": "/http", "kind": "chan.http_in", "params": {"port": 0, "params": "off"}}),
    );
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.watch", json!({"path": "/http"}));
    http_port(&f, 0);
    f.call("http.send", json!({"message": {"/lfo/rate": 4}}));
    eventually("the channel", || channel(&f, "/http", "lfo_rate") == 4.0);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(param_value(&f, "/lfo", "rate"), 1.0, "params off");
    f.call("node.set", json!({"path": "/http", "params": {"params": "all"}}));
    eventually("the want to follow", || {
        f.engine.http().wanted().first().is_some_and(|w| w.params == penumbra_http::Params::All)
    });
    f.call("http.send", json!({"message": {"/lfo/rate": 3}}));
    eventually("the param", || param_value(&f, "/lfo", "rate") == 3.0);
}

/// A WebSocket to `/ws`, handshake read; frames from it are not read (the test watches the
/// engine instead).
fn ws_connect(port: u16) -> std::net::TcpStream {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let req = format!(
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    assert!(head.starts_with(b"HTTP/1.1 101"), "{}", String::from_utf8_lossy(&head));
    s
}

/// One masked text frame (a client must mask), short payloads only.
fn ws_text(s: &mut std::net::TcpStream, text: &str) {
    let mask = [7u8, 1, 9, 3];
    let p = text.as_bytes();
    assert!(p.len() < 126);
    let mut out = vec![0x81, 0x80 | p.len() as u8];
    out.extend_from_slice(&mask);
    out.extend(p.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
    s.write_all(&out).unwrap();
}

#[test]
fn the_built_in_page_presses_momentary_buttons_with_the_show_screens_checks() {
    let f = Fixture::new("http_page");
    f.call("node.create", json!({"path": "/http", "kind": "chan.http_in", "params": {"port": 0}}));
    f.call("node.create", json!({"path": "/go", "kind": "chan.button", "params": {"show": true}}));
    f.call("node.create", json!({"path": "/stop", "kind": "chan.button", "params": {"show": true}}));
    f.call("node.create", json!({"path": "/plain", "kind": "chan.button"}));
    f.call("node.create", json!({"path": "/held", "kind": "chan.button", "params": {"show": true}}));
    f.call(
        "node.create",
        json!({"path": "/flip", "kind": "chan.button", "params": {"show": true, "mode": "toggle"}}),
    );
    f.call("node.lock", json!({"path": "/held", "locked": true}));
    for n in ["/http", "/go", "/stop"] {
        f.call("node.watch", json!({"path": n}));
    }
    let port = http_port(&f, 0);

    // No www folder in the project: / is the built-in page.
    let page = http(port, &format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"));
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("penumbra controls"), "{page}");

    // The state lists locked nodes as such.
    let state = f.engine.http().state(0).unwrap();
    let held = state["controls"].as_array().unwrap().iter().find(|c| c["path"] == "/held").unwrap().clone();
    assert_eq!((held["locked"].clone(), held["mode"].clone()), (json!(true), json!("momentary")), "{state}");

    // A tap over http.send shows for a frame and counts a press.
    let r = f.call("http.send", json!({"message": {"press": {"/go": true}}}));
    assert_eq!(r["addresses"], 0, "{r}");
    eventually("the tap", || f.call("node.info", json!({"path": "/go"}))["button"]["presses"] == 1);
    for (path, why) in
        [("/plain", "not flagged `show`"), ("/held", "locked"), ("/flip", "toggle"), ("/nothing", "no node")]
    {
        let e = f.fail("http.send", json!({"message": {"press": {path: true}}}));
        assert!(e.contains(why), "{path}: {e}");
    }

    // Two buttons held from one page at once; let go when the page goes away.
    let mut ws = ws_connect(port);
    ws_text(&mut ws, r#"{"press": {"/go": true, "/stop": true}}"#);
    eventually("both held", || channel(&f, "/go", "value") == 1.0 && channel(&f, "/stop", "value") == 1.0);
    ws_text(&mut ws, r#"{"press": {"/stop": false}}"#);
    eventually("stop let go", || channel(&f, "/stop", "value") == 0.0);
    assert_eq!(channel(&f, "/go", "value"), 1.0);
    // The editor's hold and the page's are apart: the editor letting go keeps the page's press.
    f.call("chan.press", json!({"path": "/go", "down": false}));
    assert_eq!(f.call("node.info", json!({"path": "/go"}))["button"]["held"], true);
    drop(ws);
    eventually("go let go with the page", || channel(&f, "/go", "value") == 0.0);
    assert_eq!(f.call("node.info", json!({"path": "/go"}))["button"]["presses"], 2);

    // Switched off, / answers 404 again.
    f.call("node.set", json!({"path": "/http", "params": {"page": false}}));
    eventually("the page switched off", || {
        http(port, &format!("GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")).starts_with("HTTP/1.1 404")
    });

    // In show mode a page still presses and switches, as the show screen does.
    f.call("show.lock", json!({}));
    f.call("http.send", json!({"message": {"press": {"/go": true}, "/flip/value": 1}}));
    eventually("the press in show mode", || {
        f.call("node.info", json!({"path": "/go"}))["button"]["presses"] == 3
    });
    eventually("the toggle in show mode", || param_value(&f, "/flip", "value") == true);
    f.call("show.unlock", json!({}));
}
