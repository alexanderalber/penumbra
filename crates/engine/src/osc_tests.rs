//! OSC end to end: loopback UDP on 127.0.0.1, listeners on port 0 (any free port), no outside
//! network.

use std::time::Duration;

use penumbra_osc::Arg;
use serde_json::{Value as Json, json};

use super::{Fixture, eventually};

/// The port the listener wanted as `wanted` (0 for any) actually bound.
fn osc_port(f: &Fixture, wanted: u16) -> u16 {
    let mut port = 0;
    eventually("the OSC listener to bind", || {
        let bound = f.engine.osc().hub().listener(wanted).and_then(|s| s.bound);
        port = bound.and_then(|b| b.rsplit(':').next().and_then(|p| p.parse().ok())).unwrap_or(0);
        port != 0
    });
    port
}

fn send(port: u16, address: &str, args: Vec<Arg>) {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let packet = penumbra_osc::codec::message_packet(&penumbra_osc::Message::new(address, args));
    s.send_to(&packet, ("127.0.0.1", port)).unwrap();
}

fn param_value(f: &Fixture, path: &str, name: &str) -> Json {
    let info = f.call("node.info", json!({"path": path}));
    info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).map(|p| p["value"].clone()).unwrap()
}

#[test]
fn osc_in_becomes_channels_that_bind_and_osc_out_sends_them() {
    let f = Fixture::new("osc_chan");
    f.call("node.create", json!({"path": "/in", "kind": "chan.oscin", "params": {"port": 0}}));
    let port = osc_port(&f, 0);
    send(port, "/fader1", vec![Arg::Float(0.25)]);
    send(port, "/1/xy", vec![Arg::Float(0.5), Arg::Int(2)]);
    send(port, "/label", vec![Arg::Str("text only".into())]);
    eventually("three addresses", || f.engine.osc().hub().read(0).is_some_and(|s| s.entries.len() == 3));
    let read = f.call("chan.read", json!({"path": "/in"}));
    assert_eq!(
        read["channels"],
        json!([{"name": "fader1", "value": 0.25}, {"name": "_1_xy_0", "value": 0.5}, {"name": "_1_xy_1", "value": 2.0}]),
        "{read}"
    );
    let table = f.call("table.read", json!({"path": "/in", "output": "messages"}));
    assert_eq!(table["rows"][3], json!(["/label", "1", "text only"]), "{table}");

    // Binding is the primary path: the bound param follows, no history entries.
    f.call("node.create", json!({"path": "/m", "kind": "chan.math", "params": {"op": "multiply", "b": 1}}));
    f.call("node.bind", json!({"param": "/m.b", "from": "/in", "channel": "fader1"}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 2}}));
    f.call("node.connect", json!({"from": "/k", "to": "/m"}));
    f.call("node.watch", json!({"path": "/m"}));
    let entries = f.call("history.list", json!({}))["total"].clone();
    assert_eq!(f.call("chan.read", json!({"path": "/m"}))["channels"][0]["value"], 0.5);
    send(port, "/fader1", vec![Arg::Float(0.75)]);
    eventually("the bound value to follow", || {
        f.call("chan.read", json!({"path": "/m"}))["channels"][0]["value"] == 1.5
    });
    assert_eq!(f.call("history.list", json!({}))["total"], entries, "channels do not touch the history");

    // The address filter.
    f.call("node.set", json!({"path": "/in", "params": {"addresses": "/1/* ^/label"}}));
    let read = f.call("chan.read", json!({"path": "/in"}));
    assert_eq!(read["channels"].as_array().unwrap().len(), 2, "{read}");

    // OSC out: the channels of /k to a socket of ours.
    let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let rx_port = rx.local_addr().unwrap().port();
    f.call(
        "node.create",
        json!({"path": "/out", "kind": "chan.oscout", "params": {"port": rx_port, "prefix": "/show"}}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/out"}));
    let mut buf = [0u8; 2048];
    let (n, _) = rx.recv_from(&mut buf).unwrap();
    let p = penumbra_osc::codec::parse(&buf[..n], &penumbra_osc::Limits::default()).unwrap();
    assert_eq!(p.bundles, 1);
    assert_eq!(p.messages[0].1, penumbra_osc::Message::new("/show/chan0", vec![Arg::Float(2.0)]));
    let status = f.call("osc.status", json!({}));
    assert_eq!(status["senders"][0]["label"], "/out", "{status}");
    assert!(status["senders"][0]["packets"].as_u64().unwrap() >= 1);
    assert_eq!(status["listeners"][0]["messages"], 4, "{status}");

    // osc.send reaches the listener too.
    f.call("osc.send", json!({"port": port, "address": "/1/z", "args": [1, 0.5, "x", true]}));
    eventually("osc.send", || f.engine.osc().hub().read(0).is_some_and(|s| s.entries.len() == 4));
    assert!(
        f.fail("osc.send", json!({"host": "example.com", "port": 1, "address": "/a"})).contains("not an IP")
    );
}

#[test]
fn osc_addresses_set_params_live_outside_the_history() {
    let f = Fixture::new("osc_params");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.oscin", "params": {"port": 0, "params": true}}),
    );
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.create", json!({"path": "/lfo2", "kind": "chan.lfo"}));
    f.call("node.create", json!({"path": "/nz", "kind": "tex.noise"}));
    f.call("node.create", json!({"path": "/held", "kind": "chan.lfo"}));
    f.call("node.lock", json!({"path": "/held"}));
    f.call("node.create", json!({"path": "/tied", "kind": "chan.lfo"}));
    f.call("node.bind", json!({"param": "/tied.rate", "from": "/lfo"}));
    f.call("node.create", json!({"path": "/scene", "kind": "comp.base"}));
    f.call("node.create", json!({"path": "/scene/inner", "kind": "chan.lfo"}));
    f.call("node.set", json!({"path": "/scene", "params": {"expose": "speed = inner.rate"}}));
    let port = osc_port(&f, 0);
    let before = f.call("history.list", json!({}))["total"].as_u64().unwrap();

    send(port, "/lfo/rate", vec![Arg::Float(3.0)]);
    eventually("rate 3", || param_value(&f, "/lfo", "rate") == 3.0);
    send(port, "/lfo/rate", vec![Arg::Float(4.0)]);
    send(port, "/lfo/shape", vec![Arg::Str("square".into())]);
    eventually("rate 4 and square", || {
        param_value(&f, "/lfo", "rate") == 4.0 && param_value(&f, "/lfo", "shape") == "square"
    });
    // A component, an exposed alias, a pattern.
    send(port, "/nz/translate/1", vec![Arg::Float(0.5)]);
    eventually("one component", || param_value(&f, "/nz", "translate")[1] == 0.5);
    send(port, "/scene/speed", vec![Arg::Float(2.0)]);
    eventually("the alias", || param_value(&f, "/scene/inner", "rate") == 2.0);
    send(port, "/lfo*/amplitude", vec![Arg::Float(0.5)]);
    eventually("the pattern", || {
        param_value(&f, "/lfo", "amplitude") == 0.5 && param_value(&f, "/lfo2", "amplitude") == 0.5
    });
    // Refused: a locked node, a bound param, a value that does not fit.
    send(port, "/held/rate", vec![Arg::Float(9.0)]);
    send(port, "/tied/rate", vec![Arg::Float(9.0)]);
    send(port, "/lfo/shape", vec![Arg::Int(99)]);
    eventually("three refusals", || f.call("osc.status", json!({}))["params"]["refused"] == 3);
    assert_eq!(param_value(&f, "/held", "rate"), 1.0);
    assert_eq!(param_value(&f, "/tied", "rate"), 1.0);
    // Not a param: counted, nothing else.
    send(port, "/fader9", vec![Arg::Float(1.0)]);
    eventually("unmatched", || f.call("osc.status", json!({}))["params"]["unmatched"].as_u64() >= Some(1));

    // Live control: none of it entered the history.
    let h = f.call("history.list", json!({}));
    assert_eq!(h["total"].as_u64().unwrap(), before, "{h}");
    // OSC counts as the user editing: Claude has to wait.
    assert!(f.fail("node.set", json!({"path": "/lfo", "params": {"rate": 1}})).contains("user is editing"));
    std::thread::sleep(Duration::from_millis(3100));
    // Undoing an older edit (the expose) keeps the OSC values.
    f.call("history.undo", json!({}));
    assert_eq!(param_value(&f, "/lfo", "rate"), 4.0);
    assert_eq!(param_value(&f, "/nz", "translate")[1], 0.5);
    assert_eq!(param_value(&f, "/scene/inner", "rate"), 2.0);
    f.call("history.redo", json!({}));
    assert_eq!(param_value(&f, "/lfo", "shape"), "square");
    // A recorded edit of an OSC-set param takes it back into the history: undo returns to the
    // OSC value, not to the one before it.
    f.call("node.set", json!({"path": "/lfo", "params": {"rate": 7}}));
    assert_eq!(param_value(&f, "/lfo", "rate"), 7.0);
    f.call("history.undo", json!({}));
    assert_eq!(param_value(&f, "/lfo", "rate"), 4.0);
    f.call("history.undo", json!({}));
    assert_eq!(param_value(&f, "/lfo", "rate"), 4.0);
    assert_eq!(param_value(&f, "/lfo", "amplitude"), 0.5);
}

#[test]
fn osc_is_refused_while_claude_edits_and_learn_binds_the_next_address() {
    let f = Fixture::new("osc_learn");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.oscin", "params": {"port": 0, "params": true}}),
    );
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.create", json!({"path": "/nz", "kind": "tex.noise"}));
    let port = osc_port(&f, 0);
    f.call("edit.begin", json!({"label": "tuning"}));
    send(port, "/lfo/rate", vec![Arg::Float(3.0)]);
    eventually("refused", || f.call("osc.status", json!({}))["params"]["refused"] == 1);
    let status = f.call("osc.status", json!({}));
    assert!(status["params"]["last_refused"].as_str().unwrap().contains("Claude is editing"), "{status}");
    assert_eq!(param_value(&f, "/lfo", "rate"), 1.0);
    f.call("edit.end", json!({}));

    // Learn: the next address with a number (text-only messages do not count).
    f.call("osc.learn", json!({"param": "/lfo.amplitude"}));
    assert_eq!(f.call("osc.status", json!({}))["learning"]["param"], "/lfo.amplitude");
    send(port, "/label", vec![Arg::Str("no number".into())]);
    send(port, "/knob", vec![Arg::Float(0.7)]);
    eventually("learned", || f.call("osc.status", json!({}))["learned"]["from"] == "/in");
    let info = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(info["binds"], json!([{"param": "amplitude", "from": "/in:knob"}]), "{info}");
    // A vector learns one channel per component.
    f.call("osc.learn", json!({"param": "/nz.translate"}));
    send(port, "/xy", vec![Arg::Float(0.1), Arg::Float(0.2)]);
    eventually("learned the vector", || f.call("osc.status", json!({}))["learned"]["address"] == "/xy");
    let info = f.call("node.info", json!({"path": "/nz"}));
    assert_eq!(
        info["binds"],
        json!([{"param": "translate.0", "from": "/in:xy_0"}, {"param": "translate.1", "from": "/in:xy_1"}]),
        "{info}"
    );
    // A timeout and a cancel.
    let r = f.registry.dispatch("osc.learn", &json!({"param": "/lfo.rate", "timeout": 0.2, "wait": 2}));
    assert!(r["error"].as_str().unwrap().contains("nothing arrived"), "{r}");
    f.call("osc.learn", json!({"param": "/lfo.rate"}));
    assert_eq!(f.call("osc.learn", json!({"cancel": true}))["cancelled"], true);
    assert!(f.fail("osc.learn", json!({"param": "/lfo.shape"})).contains("numbers only"));

    // Hostile input is counted, not fatal; the switch closes the port.
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    s.send_to(b"\x00garbage", ("127.0.0.1", port)).unwrap();
    eventually("malformed", || f.call("osc.status", json!({}))["listeners"][0]["malformed"] == 1);
    f.call("osc.config", json!({"enabled": false}));
    assert_eq!(f.call("osc.status", json!({}))["listeners"], json!([]));
    f.call("node.watch", json!({"path": "/in"}));
    eventually("the node to say so", || {
        f.call("node.info", json!({"path": "/in"}))["status"].to_string().contains("switched off")
    });
    f.call("osc.config", json!({"enabled": true}));
}

#[test]
fn osc_does_not_set_a_scripts_code() {
    let f = Fixture::new("osc_code");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.oscin", "params": {"port": 0, "params": true}}),
    );
    f.call("node.create", json!({"path": "/py", "kind": "script.python"}));
    let code = param_value(&f, "/py", "code");
    let port = osc_port(&f, 0);

    send(port, "/py/code", vec![Arg::Str("import os".into())]);
    eventually("the refusal", || f.call("osc.status", json!({}))["params"]["refused"] == 1);
    assert_eq!(param_value(&f, "/py", "code"), code);
    let why = f.call("osc.status", json!({}))["params"]["last_refused"].clone();
    assert!(why.as_str().is_some_and(|w| w.contains("is code")), "{why}");
}
