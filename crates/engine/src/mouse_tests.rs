//! Mouse and 3D mouse end to end: a fake pointer and the wheel into `chan.mouse_in`, reports of
//! the 3D mouse into `chan.spacemouse_in`, learn from the 3D mouse.

use serde_json::json;

use super::{Fixture, eventually};

fn read(f: &Fixture, path: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}))["channels"].clone()
}

/// Waits until `channel` of `path` is `want` (within 0.01): the node cooks once a frame while
/// watched.
fn until(f: &Fixture, path: &str, channel: &str, want: f64) {
    eventually(&format!("{path}:{channel} to be {want}"), || (value(f, path, channel) - want).abs() < 0.01);
}

fn value(f: &Fixture, path: &str, channel: &str) -> f64 {
    read(f, path)
        .as_array()
        .and_then(|a| a.iter().find(|c| c["name"] == channel))
        .and_then(|c| c["value"].as_f64())
        .unwrap_or(f64::NAN)
}

#[test]
fn a_fake_pointer_and_the_wheel_become_channels() {
    let f = Fixture::new("mouse_in");
    assert_eq!(f.call("mouse.status", json!({}))["wanted"], false);
    f.call("node.create", json!({"path": "/mouse", "kind": "chan.mouse_in"}));
    f.call("node.watch", json!({"path": "/mouse"}));
    eventually("the event loop to pass the wheel on", || f.engine.mouse().wanted());
    let names: Vec<String> = read(&f, "/mouse")
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["x", "y", "inside", "left", "right", "middle", "wheel"]);

    // On the first display the desktop has: the middle of it, left and middle held.
    let monitors = f.call("mouse.status", json!({}))["monitors"].clone();
    if let Some(m) = monitors.as_array().and_then(|a| a.first()) {
        let (l, t, r, b) = (
            m["left"].as_i64().unwrap(),
            m["top"].as_i64().unwrap(),
            m["right"].as_i64().unwrap(),
            m["bottom"].as_i64().unwrap(),
        );
        f.call("mouse.fake", json!({"x": (l + r) / 2, "y": t + (b - t) / 4, "buttons": ["left", "middle"]}));
        until(&f, "/mouse", "x", 0.5);
        until(&f, "/mouse", "y", 0.75);
        until(&f, "/mouse", "inside", 1.0);
        until(&f, "/mouse", "left", 1.0);
        until(&f, "/mouse", "right", 0.0);
        until(&f, "/mouse", "middle", 1.0);
        // Named by its OS name: the same shares.
        let name = m["os_name"].as_str().unwrap();
        f.call("node.set", json!({"path": "/mouse", "params": {"display": name}}));
        until(&f, "/mouse", "x", 0.5);
    }

    f.call("mouse.fake", json!({"wheel": 2}));
    f.call("mouse.fake", json!({"wheel": -0.5}));
    until(&f, "/mouse", "wheel", 1.5);

    // A display that is not there warns; the position holds, `inside` is 0.
    f.call("node.set", json!({"path": "/mouse", "params": {"display": "NOPE [XYZ1234]"}}));
    until(&f, "/mouse", "inside", 0.0);
    let info = f.call("node.info", json!({"path": "/mouse"}));
    assert!(info["status"]["warning"].as_str().unwrap_or_default().contains("NOPE"), "{info}");

    assert!(f.fail("mouse.fake", json!({"x": 3})).contains("both"));
    assert!(f.fail("mouse.fake", json!({"x": 3, "y": 4, "buttons": ["thumb"]})).contains("no button"));
    f.call("mouse.fake", json!({"real": true}));
    assert_eq!(f.call("mouse.status", json!({}))["faked"], false);
    f.call("node.delete", json!({"path": "/mouse"}));
    eventually("the wheel no longer passed on", || !f.engine.mouse().wanted());
}

#[test]
fn the_3d_mouse_becomes_channels_and_learn_takes_an_axis() {
    let f = Fixture::new("spacemouse_in");
    // The device this machine may have stays closed; the reports below stand in for it.
    f.engine.spacenav().detach();
    f.call("node.create", json!({"path": "/cap", "kind": "chan.spacemouse_in"}));
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.watch", json!({"path": "/cap"}));
    let names: Vec<String> = read(&f, "/cap")
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        ["right", "up", "forward", "pitch", "yaw", "roll", "button_left", "button_right", "connected"]
    );
    let nav = f.engine.spacenav();
    nav.feed(&[1, 0, 0, 0x3e, 0xfe, 0, 0]); // ty = -450: pushed away
    nav.feed(&[3, 1, 0, 0, 0, 0, 0]); // left button down
    until(&f, "/cap", "forward", 1.0);
    until(&f, "/cap", "button_left", 1.0);
    nav.feed(&[3, 0, 0, 0, 0, 0, 0]);
    nav.feed(&[1, 0, 0, 0, 0, 0, 0]);
    until(&f, "/cap", "button_left", 0.0);

    // Learn: the axis pushed past half way.
    f.call("osc.learn", json!({"param": "/lfo.amplitude"}));
    nav.feed(&[2, 0, 0, 0, 0, 0x90, 0x01]); // rz = 400: twisted
    eventually("learned", || f.call("osc.status", json!({}))["learned"]["from"] == "/cap");
    let info = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(info["binds"], json!([{"param": "amplitude", "from": "/cap:yaw"}]), "{info}");
}
