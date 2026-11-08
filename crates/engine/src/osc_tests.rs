//! OSC end to end: loopback UDP on 127.0.0.1, listeners on port 0 (any free port), no outside
//! network.

use std::time::Duration;

use penumbra_osc::{Arg, Message};
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
    f.call("node.create", json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0}}));
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
        json!({"path": "/out", "kind": "chan.osc_out", "params": {"port": rx_port, "prefix": "/show"}}),
    );
    f.call("node.connect", json!({"from": "/k", "to": "/out"}));
    let mut buf = [0u8; 2048];
    let (n, _) = rx.recv_from(&mut buf).unwrap();
    let p = penumbra_osc::codec::parse(&buf[..n], &penumbra_osc::Limits::default()).unwrap();
    assert_eq!(p.bundles, 1);
    assert_eq!(p.messages[0].1, penumbra_osc::Message::new("/show/chan0", vec![Arg::Float(2.0)]));
    // The sender counts a packet after sending it, so the socket can have it first.
    eventually("the sender to count the packet", || {
        f.call("osc.status", json!({}))["senders"][0]["packets"].as_u64().unwrap_or(0) >= 1
    });
    let status = f.call("osc.status", json!({}));
    assert_eq!(status["senders"][0]["label"], "/out", "{status}");
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
        json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0, "params": true}}),
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
        json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0, "params": true}}),
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
        json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0, "params": true}}),
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

/// Channel `name` of the node at `path`, as `chan.read` reads it.
fn channel(registry: &crate::Registry, path: &str, name: &str) -> Json {
    let r = registry.dispatch("chan.read", &json!({"path": path}));
    let channels = r["result"]["channels"].as_array().cloned().unwrap_or_default();
    channels.into_iter().find(|c| c["name"] == name).map(|c| c["value"].clone()).unwrap_or(Json::Null)
}

/// The UDP port the engine's listener for `chan.osc_in` port 0 got.
fn bound_port(e: &crate::Engine) -> u16 {
    let mut port = 0;
    eventually("the OSC listener to bind", || {
        let bound = e.osc().hub().listener(0).and_then(|s| s.bound);
        port = bound.and_then(|b| b.rsplit(':').next().and_then(|p| p.parse().ok())).unwrap_or(0);
        port != 0
    });
    port
}

/// A cue list of holding steps, its `go` bound to the trigger `/go` of `/in`.
fn cue_list(f: &Fixture, params: bool) {
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0, "triggers": "/go", "params": params}}),
    );
    f.call(
        "node.create",
        json!({"path": "/s", "kind": "chan.sequence", "params": {
            "steps": "a hold\nb hold\nc hold\nd hold\ne hold\nf hold", "lead_in": 0}}),
    );
    f.call("node.bind", json!({"param": "/s.go", "from": "/in", "channel": "go"}));
}

fn param_of(registry: &crate::Registry, path: &str, name: &str) -> Option<Json> {
    let info = registry.dispatch("node.info", &json!({"path": path}));
    info["result"]["params"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["name"] == name).map(|p| p["value"].clone()))
}

#[test]
fn a_trigger_address_counts_every_press() {
    let f = Fixture::new("osc_trigger");
    cue_list(&f, false);
    let port = osc_port(&f, 0);
    let step = || channel(&f.registry, "/s", "step");
    eventually("the first cue", || step() == 0.0);
    assert_eq!(channel(&f.registry, "/in", "go"), 0.0, "a trigger written out has its channel at once");
    // A desk that sends only `/go 1` per press moves the show on each time.
    send(port, "/go", vec![Arg::Float(1.0)]);
    eventually("the second cue", || step() == 1.0);
    send(port, "/go", vec![Arg::Float(1.0)]);
    eventually("the third cue", || step() == 2.0);
    // A release is no press; nor is a level below a half. A GO without a number is one.
    send(port, "/go", vec![Arg::Float(0.0)]);
    send(port, "/go", vec![Arg::Float(0.25)]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(step(), 2.0);
    send(port, "/go", vec![]);
    eventually("the fourth cue", || step() == 3.0);
    // Two presses in one packet arrive within one frame: two GOs, one after the other.
    let twice = vec![Message::new("/go", vec![Arg::Float(1.0)]), Message::new("/go", vec![Arg::Int(1)])];
    let packets = penumbra_osc::codec::packets(&twice, 1400, true);
    assert_eq!(packets.len(), 1);
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    s.send_to(&packets[0], ("127.0.0.1", port)).unwrap();
    eventually("two cues on", || step() == 5.0);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(step(), 5.0, "no more than two");
    assert_eq!(channel(&f.registry, "/in", "go"), 0.0, "the pulse fell again");
}

#[test]
fn a_go_button_held_over_a_resend_is_one_press() {
    // As the example `show`: a button sent by `chan.osc_out`, which resends everything once a
    // second, into a trigger. Held for over two seconds, it moves the show on once.
    let f = Fixture::new("osc_trigger_held");
    cue_list(&f, false);
    let port = osc_port(&f, 0);
    f.call("node.create", json!({"path": "/b", "kind": "chan.button", "params": {"name": "go"}}));
    f.call(
        "node.create",
        json!({"path": "/desk", "kind": "chan.osc_out", "params": {"port": port, "rate": 60.0}}),
    );
    f.call("node.connect", json!({"from": "/b", "to": "/desk", "input": "chans"}));
    let step = || channel(&f.registry, "/s", "step");
    let presses = || {
        f.engine
            .osc()
            .hub()
            .read(0)
            .and_then(|s| s.entries.iter().find(|e| e.address == "/go").map(|e| (e.count, e.presses)))
    };
    eventually("the released button sent", || presses().is_some());
    f.call("chan.press", json!({"path": "/b", "down": true}));
    eventually("the second cue", || step() == 1.0);
    let before = presses().map_or(0, |p| p.0);
    // Held until two resends arrived. The resends go by show time a second apart, and a busy
    // machine can push one past a fixed wait.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let held = loop {
        let p = presses().unwrap();
        if p.0 >= before + 2 {
            break p;
        }
        assert!(std::time::Instant::now() < deadline, "resent while held: {p:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(held.1, 1, "one press: {held:?}");
    assert_eq!(step(), 1.0, "no cue skipped");
    f.call("chan.press", json!({"path": "/b", "down": false}));
    eventually("the release sent", || channel(&f.registry, "/b", "go") == 0.0);
    std::thread::sleep(Duration::from_millis(200));
    f.call("chan.press", json!({"path": "/b", "down": true}));
    eventually("the third cue", || step() == 2.0);
    f.call("chan.press", json!({"path": "/b", "down": false}));
}

#[test]
fn a_cue_list_and_values_set_by_address_survive_a_restart_in_show_mode() {
    let f = Fixture::new("osc_show_restart");
    let data = f._dir.join("data");
    let show_file = data.join(crate::show::FILE);
    let kept = data.join(crate::show_values::FILE);
    f.engine.set_show_file(Some(show_file.clone()));
    cue_list(&f, true);
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    eventually("autosave", || f.file().contains("\"/lfo\""));
    let file = f.file();
    f.call("show.lock", json!({}));
    let port = osc_port(&f, 0);
    let step = |r: &crate::Registry| channel(r, "/s", "step");
    send(port, "/go", vec![Arg::Float(1.0)]);
    eventually("the second cue", || step(&f.registry) == 1.0);
    send(port, "/go", vec![Arg::Float(1.0)]);
    eventually("the third cue", || step(&f.registry) == 2.0);
    send(port, "/lfo/rate", vec![Arg::Float(3.0)]);
    eventually("the rate set by address", || param_value(&f, "/lfo", "rate") == 3.0);
    eventually("the cue and the rate kept", || {
        std::fs::read_to_string(&kept).is_ok_and(|t| {
            let k: Json = serde_json::from_str(&t).unwrap_or_default();
            k["live"]["/lfo.rate"] == 3.0 && k["places"]["/s"]["place"]["free"] == 2
        })
    });
    let text = std::fs::read_to_string(&kept).unwrap();
    assert!(text.contains("chan.sequence"), "{text}");
    // A crash: the app ends without unlocking, project.ron still holds the old values.
    f.engine.shutdown();
    assert_eq!(f.file(), file, "project.ron untouched");

    let restart = || {
        let types = std::sync::Arc::new(penumbra_ops::builtin_types().unwrap());
        let session = penumbra_project::Session::open(
            f._dir.path(),
            types,
            &penumbra_project::content::Content::default(),
        )
        .unwrap();
        let e = crate::Engine::start(std::sync::Arc::new(session)).unwrap();
        assert!(e.set_show_file(Some(show_file.clone())));
        let r = crate::engine_registry(&e);
        (e, r)
    };
    let (g, r) = restart();
    assert_eq!(param_of(&r, "/lfo", "rate"), Some(json!(3.0)), "back before the desk sends again");
    eventually("the cue list at its third cue", || step(&r) == 2.0);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(step(&r), 2.0, "it holds there, and nothing counted as a GO");
    // It goes on from there.
    send(bound_port(&g), "/go", vec![Arg::Float(1.0)]);
    eventually("the fourth cue", || step(&r) == 3.0);
    eventually("the fourth cue kept", || {
        std::fs::read_to_string(&kept).is_ok_and(|t| t.contains("\"free\": 3"))
    });
    g.shutdown();

    // A hostile place (seconds that are no number), a place for a node of another kind, values
    // that do not fit: all left out, the cue list starts over, the rate still comes back.
    let mut k: Json = serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
    k["places"]["/s"]["place"]["seconds"] = json!("soon");
    k["places"]["/lfo"] = json!({"kind": "chan.sequence", "place": {"seconds": 1}});
    k["live"]["/lfo.shape"] = json!(42);
    k["live"]["/nobody.rate"] = json!(1);
    std::fs::write(&kept, serde_json::to_string(&k).unwrap()).unwrap();
    let (h, r) = restart();
    eventually("the first cue", || step(&r) == 0.0);
    assert_eq!(param_of(&r, "/lfo", "rate"), Some(json!(3.0)));
    assert_eq!(param_of(&r, "/lfo", "shape"), Some(json!("sine")), "a value that does not fit is skipped");
    h.shutdown();

    // A place kept for another show (its steps edited) does not move this one either.
    let mut k: Json = serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
    k["places"]["/s"]["place"] =
        json!({"show": "0000000000000000", "seconds": 0.5, "free": 3, "round": false});
    std::fs::write(&kept, serde_json::to_string(&k).unwrap()).unwrap();
    let (i, r) = restart();
    eventually("the first cue", || step(&r) == 0.0);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(step(&r), 0.0);
    i.shutdown();
}

#[test]
fn osc_in_keeps_the_first_channels_when_addresses_flood_in() {
    let f = Fixture::new("osc_flood");
    f.call("node.create", json!({"path": "/in", "kind": "chan.osc_in", "params": {"port": 0}}));
    let port = osc_port(&f, 0);
    // 70 addresses of 64 values: 4480 channels, more than a node holds.
    for a in 0..70 {
        send(port, &format!("/a{a:02}"), (0..64).map(|v| Arg::Float(v as f32)).collect());
    }
    eventually("70 addresses", || f.engine.osc().hub().read(0).is_some_and(|s| s.entries.len() == 70));
    f.call("node.watch", json!({"path": "/in"}));
    eventually("the channels", || {
        f.call("chan.read", json!({"path": "/in"}))["channels"].as_array().is_some_and(|c| !c.is_empty())
    });
    let read = f.call("chan.read", json!({"path": "/in"}));
    assert_eq!(read["channels"].as_array().map(Vec::len), Some(penumbra_core::chan::MAX_CHANNELS));
    let status = f.call("node.info", json!({"path": "/in"}))["status"].clone();
    assert_eq!(status["error"], Json::Null, "{status}");
    let warning = status["warning"].as_str().unwrap_or("").to_string();
    assert!(warning.contains("384 more left out"), "{warning}");
}
