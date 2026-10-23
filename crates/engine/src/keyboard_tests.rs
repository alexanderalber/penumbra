//! Keyboard end to end: keys pressed by command (as the windows press them), the node's gates
//! and `last`, a binding of `last`, learn from a key.

use serde_json::json;

use super::{Fixture, eventually};

fn read(f: &Fixture, path: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}))["channels"].clone()
}

#[test]
fn keys_become_gates_and_last_drives_a_bound_param() {
    let f = Fixture::new("keyboard_in");
    assert_eq!(f.call("keyboard.status", json!({}))["wanted"], false);
    f.call("node.create", json!({"path": "/keys", "kind": "chan.keyboard_in"}));
    eventually("the windows to pass keys on", || f.engine.keyboard().wanted());
    let names = |v: &serde_json::Value| {
        v.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
    };
    let start = read(&f, "/keys");
    assert_eq!(names(&start), ["key_1", "key_2", "key_3", "key_4", "last"], "{start}");
    assert_eq!(start[4]["value"], 0.0, "last starts at 0");

    // /piece follows `last`, the way a show switches pieces.
    f.call("node.create", json!({"path": "/piece", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/piece.value0", "from": "/keys", "channel": "last"}));
    f.call("node.watch", json!({"path": "/piece"}));
    f.call("keyboard.press", json!({"key": "3"}));
    eventually("last to be 2", || read(&f, "/keys")[4]["value"] == 2.0);
    eventually("the bound piece to follow", || read(&f, "/piece")[0]["value"] == 2.0);

    // Held: the gate is 1 until released; a key not listed changes nothing.
    f.call("keyboard.press", json!({"key": "Numpad2", "down": true}));
    eventually("key_2 held", || read(&f, "/keys")[1]["value"] == 1.0);
    assert_eq!(read(&f, "/keys")[4]["value"], 1.0);
    let info = f.call("node.info", json!({"path": "/keys"}));
    assert_eq!(info["keyboard"]["held"], json!(["2"]), "{info}");
    assert_eq!(info["keyboard"]["last"], 1, "{info}");
    f.call("keyboard.press", json!({"key": "2", "down": false}));
    eventually("key_2 released", || read(&f, "/keys")[1]["value"] == 0.0);
    f.call("keyboard.press", json!({"key": "q"}));
    assert_eq!(read(&f, "/keys")[4]["value"], 1.0, "last holds");

    // Other keys; a name that is no key warns and is left out.
    f.call("node.set", json!({"path": "/keys", "params": {"keys": "space F1 nope left"}}));
    let other = read(&f, "/keys");
    assert_eq!(names(&other), ["key_space", "key_f1", "key_left", "last"], "{other}");
    let info = f.call("node.info", json!({"path": "/keys"}));
    assert!(info["status"]["warning"].as_str().unwrap_or_default().contains("nope"), "{info}");
    f.call("keyboard.press", json!({"key": "ArrowLeft"}));
    eventually("last to be the arrow", || read(&f, "/keys")[3]["value"] == 2.0);

    assert!(f.fail("keyboard.press", json!({"key": "f13"})).contains("no key"));
    let status = f.call("keyboard.status", json!({}));
    assert_eq!(status["last"], "left", "{status}");
    assert_eq!(status["held"], json!([]), "{status}");

    // Deleting the node: the windows stop passing keys on.
    f.call("node.delete", json!({"path": "/keys"}));
    eventually("no keys passed on", || !f.engine.keyboard().wanted());
}

#[test]
fn learn_binds_a_param_to_a_listed_key() {
    let f = Fixture::new("keyboard_learn");
    f.call("node.create", json!({"path": "/keys", "kind": "chan.keyboard_in", "params": {"keys": "a b"}}));
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    eventually("the windows to pass keys on", || f.engine.keyboard().wanted());
    f.call("osc.learn", json!({"param": "/lfo.amplitude"}));
    // A key no node lists is passed over.
    f.call("keyboard.press", json!({"key": "z"}));
    f.call("keyboard.press", json!({"key": "b"}));
    eventually("learned", || f.call("osc.status", json!({}))["learned"]["from"] == "/keys");
    let info = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(info["binds"], json!([{"param": "amplitude", "from": "/keys:key_b"}]), "{info}");
}
