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
