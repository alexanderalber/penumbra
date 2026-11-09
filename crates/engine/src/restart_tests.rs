//! A component starting its stateful nodes over: a key bound to its `reset` (also while the
//! show is locked), a change of a param its `@reset` line names, and a measurement inside it
//! fed back to its `reset`, which restarts it whenever it has run its course.

use serde_json::json;

use super::Fixture;

/// The seconds of clock `path` after the last step.
fn seconds(f: &Fixture, path: &str) -> f64 {
    let read = f.call("chan.read", json!({"path": path}));
    read["channels"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == "seconds"))
        .map_or(-1.0, |c| c["value"].as_f64().unwrap_or(-1.0))
}

/// Steps one frame and gives the clock's frames since it started over.
fn step(f: &Fixture, path: &str) -> i64 {
    f.call("cook.step", json!({}));
    (seconds(f, path) * 60.0).round() as i64
}

#[test]
fn a_key_and_a_preset_start_a_component_over_in_the_same_frame() {
    let f = Fixture::new("restart_key");
    f.call("cook.stepping", json!({"on": true}));
    f.call("node.create", json!({"path": "/keys", "kind": "chan.keyboard_in", "params": {"keys": "0"}}));
    f.call(
        "node.create",
        json!({"path": "/sim", "kind": "comp.base", "params": {
            "params": "@param preset choice(a|b|c) = a\n@param look float\n@reset preset"}}),
    );
    f.call("node.create", json!({"path": "/sim/clock", "kind": "chan.clock", "params": {"mode": "free"}}));
    f.call("node.create", json!({"path": "/sim/plain", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/sim.reset", "from": "/keys", "channel": "key_0"}));
    let first = step(&f, "/sim/clock");
    for k in 1..=5 {
        assert_eq!(step(&f, "/sim/clock"), first + k, "it runs");
    }

    // A preset picked: the very next frame is the clock's first.
    f.call("node.set", json!({"path": "/sim", "params": {"preset": "b"}}));
    assert_eq!(step(&f, "/sim/clock"), first, "the new preset starts over");
    assert_eq!(step(&f, "/sim/clock"), first + 1);
    // A param not named by @reset, and a new text with the same values, leave it running.
    f.call("node.set", json!({"path": "/sim", "params": {"look": 2.0}}));
    assert_eq!(step(&f, "/sim/clock"), first + 2);
    f.call(
        "node.set",
        json!({"path": "/sim", "params": {"params": "@param preset choice(a|b|c) = a\n@param look float\n# more\n@reset preset"}}),
    );
    assert_eq!(step(&f, "/sim/clock"), first + 3);
    // Undo of the pick starts over too: preset is back at a.
    f.call("history.undo", json!({}));
    f.call("history.undo", json!({}));
    f.call("history.undo", json!({}));
    assert_eq!(step(&f, "/sim/clock"), first, "undo is a change of the value");

    // In a locked show: a tap of 0 starts it over, and it is no edit.
    f.call("show.lock", json!({}));
    let edits = |f: &Fixture| {
        let h = f.call("history.list", json!({}));
        (h["total"].clone(), h["applied"].clone())
    };
    let history = edits(&f);
    for _ in 0..3 {
        step(&f, "/sim/clock");
    }
    f.call("keyboard.press", json!({"key": "0"}));
    assert_eq!(step(&f, "/sim/clock"), first, "the key starts it over");
    assert_eq!(step(&f, "/sim/clock"), first + 1);
    // Held: it stays at the start until the key comes up.
    f.call("keyboard.press", json!({"key": "0", "down": true}));
    for _ in 0..4 {
        assert_eq!(step(&f, "/sim/clock"), first, "held at the start");
    }
    f.call("keyboard.press", json!({"key": "0", "down": false}));
    assert_eq!(step(&f, "/sim/clock"), first + 1, "runs again on release");
    assert_eq!(edits(&f), history, "keys are no edits");
}

#[test]
fn a_measurement_fed_back_to_reset_restarts_a_component_when_it_has_run_its_course() {
    let f = Fixture::new("restart_fed_back");
    f.call("cook.stepping", json!({"on": true}));
    f.call("node.create", json!({"path": "/sim", "kind": "comp.base"}));
    f.call("node.create", json!({"path": "/sim/clock", "kind": "chan.clock", "params": {"mode": "free"}}));
    // `done` is 1 once the clock has passed 0.1 s (6 frames).
    f.call(
        "node.create",
        json!({"path": "/sim/done", "kind": "chan.math", "params": {"op": "above", "b": 0.09}}),
    );
    f.call("node.connect", json!({"from": "/sim/clock", "to": "/sim/done"}));
    f.call("node.create", json!({"path": "/sim/late", "kind": "chan.feedback"}));
    f.call("node.connect", json!({"from": "/sim/done", "to": "/sim/late", "input": "previous"}));
    f.call("node.bind", json!({"param": "/sim.reset", "from": "/sim/late", "channel": "seconds"}));
    f.call("node.watch", json!({"path": "/sim/done"}));
    let frames: Vec<i64> = (0..24).map(|_| step(&f, "/sim/clock")).collect();
    // The mark is passed in frame 6; chan.feedback hands that on a frame later, and the clock
    // starts over in that frame.
    assert_eq!(frames[..16], [0, 1, 2, 3, 4, 5, 6, 0, 1, 2, 3, 4, 5, 6, 0, 1], "{frames:?}");
}
