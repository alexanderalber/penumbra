//! `chan.button` and `chan.slider` end to end: `chan.press` holds and taps a momentary button
//! without history, flips a toggle as an edit; a slider's value is set by `node.set`, kept in
//! its range and saved; both drive bindings like any chan node.

use serde_json::json;

use super::{Fixture, eventually};

fn value(f: &Fixture, path: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}))["channels"][0]["value"].clone()
}

fn entries(f: &Fixture) -> usize {
    f.call("history.list", json!({"limit": 1000}))["entries"].as_array().map_or(0, Vec::len)
}

#[test]
fn a_momentary_button_is_held_by_command_without_history() {
    let f = Fixture::new("button_momentary");
    f.call("node.create", json!({"path": "/b", "kind": "chan.button"}));
    let channels = f.call("chan.read", json!({"path": "/b"}))["channels"].clone();
    assert_eq!(channels, json!([{"name": "value", "value": 0.0}]), "{channels}");
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/c.value0", "from": "/b"}));
    f.call("node.watch", json!({"path": "/c"}));
    let before = entries(&f);
    assert!(before >= 3, "the creates and the bind are entries");

    let r = f.call("chan.press", json!({"path": "/b", "down": true}));
    assert_eq!(r["held"], true, "{r}");
    eventually("the button to be 1", || value(&f, "/b") == 1.0);
    eventually("the bound constant to follow", || value(&f, "/c") == 1.0);
    let info = f.call("node.info", json!({"path": "/b"}));
    assert_eq!(info["button"], json!({"mode": "momentary", "held": true, "presses": 1}), "{info}");
    f.call("chan.press", json!({"path": "/b", "down": false}));
    eventually("the button to be 0", || value(&f, "/b") == 0.0);
    eventually("the bound constant to follow", || value(&f, "/c") == 0.0);

    // A tap counts as a press; the button is up afterwards.
    let r = f.call("chan.press", json!({"path": "/b"}));
    assert_eq!(r["presses"], 2, "{r}");
    assert_eq!(r["held"], false, "{r}");
    assert_eq!(entries(&f), before, "pressing made no history entry");

    assert!(f.fail("chan.press", json!({"path": "/c"})).contains("not a chan.button"));
    assert!(f.fail("chan.press", json!({"path": "/nope"})).contains("no node"));
    f.fail("chan.press", json!({"path": "/b", "down": 1}));
    f.fail("chan.press", json!({"path": "/b", "extra": true}));
    f.fail("chan.press", json!({}));

    // Deleted while held: the hub forgets it.
    f.call("chan.press", json!({"path": "/b", "down": true}));
    let id = f.engine.session().doc().snapshot().resolve(&"/b".parse().unwrap()).unwrap();
    f.call("node.delete", json!({"path": "/b"}));
    eventually("the hub to forget the button", || !f.engine.buttons().view(id).held);
}

#[test]
fn a_toggle_flips_its_saved_value() {
    let f = Fixture::new("button_toggle");
    f.call("node.create", json!({"path": "/b", "kind": "chan.button", "params": {"mode": "toggle"}}));
    assert_eq!(value(&f, "/b"), 0.0);
    let r = f.call("chan.press", json!({"path": "/b"}));
    assert_eq!((r["value"].clone(), r["history"].clone()), (json!(true), json!("new")), "{r}");
    assert_eq!(value(&f, "/b"), 1.0);
    let r = f.call("chan.press", json!({"path": "/b", "down": false}));
    assert_eq!(r["value"], true, "a release leaves a toggle as it is: {r}");
    eventually("autosave", || f.file().contains(r#""value": true"#));
    f.call("chan.press", json!({"path": "/b", "down": true}));
    assert_eq!(value(&f, "/b"), 0.0);
    f.call("history.undo", json!({}));
    assert_eq!(value(&f, "/b"), 1.0, "a press is undone like any edit");

    // Bound: the channel wins, a press is refused.
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/b.value", "from": "/k"}));
    assert!(f.fail("chan.press", json!({"path": "/b"})).contains("bound"));
}

#[test]
fn a_slider_keeps_its_value_in_range_and_saves_it() {
    let f = Fixture::new("slider");
    f.call(
        "node.create",
        json!({"path": "/s", "kind": "chan.slider", "params": {"max": 10, "value": 5, "name": "speed"}}),
    );
    let channels = f.call("chan.read", json!({"path": "/s"}))["channels"].clone();
    assert_eq!(channels, json!([{"name": "speed", "value": 5.0}]), "{channels}");
    f.call("node.set", json!({"path": "/s", "params": {"value": 20}}));
    assert_eq!(value(&f, "/s"), 10.0, "kept at max");
    f.call("node.set", json!({"path": "/s", "params": {"value": 2.5}}));
    eventually("autosave", || f.file().contains(r#""value": 2.5"#));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/c.value0", "from": "/s"}));
    assert_eq!(value(&f, "/c"), 2.5);
}
