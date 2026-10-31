//! The controls on node bodies driven headless as a user would: a momentary button held, a
//! toggle clicked, a slider dragged, none of it selecting or moving the node.

use std::sync::Arc;

use penumbra_core::scratch::ScratchDir;
use penumbra_engine::Engine;
use penumbra_project::Session;
use serde_json::{Value as Json, json};

use crate::headless;

const PROJECT: &str = r#"(
    version: 1,
    nodes: {
        "/b": (kind: "chan.button", pos: [0, 0]),
        "/t": (kind: "chan.button", params: {"mode": "toggle", "label": "lights"}, pos: [0, 90]),
        "/s": (kind: "chan.slider", pos: [0, 180]),
        "/k": (kind: "chan.constant", params: {"value0": 0.25}, pos: [-300, 270]),
        "/bound": (kind: "chan.slider", binds: {"value": "/k"}, pos: [0, 270]),
        "/right": (kind: "chan.constant", pos: [400, 180]),
        "/left": (kind: "chan.constant", pos: [-300, 180]),
    },
)"#;

fn reply<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

fn param(info: &Json, name: &str) -> Json {
    info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).unwrap()["value"].clone()
}

#[test]
fn press_toggle_and_drag_on_node_bodies() {
    let dir = ScratchDir::unborn("ui-controls");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let drag = |from: &str, to: &str| json!({"method": "ui.drag", "params": {"from": {"id": from}, "to": {"id": to}, "steps": 6}});
    let info = |path: &str| json!({"method": "node.info", "params": {"path": path}});
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.elements", "params": {"prefix": "control."}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        // Held for a few frames, then let go.
        drag("control./b", "control./b"),
        info("/b"),
        json!({"method": "ui.click", "params": {"id": "control./t"}}),
        info("/t"),
        // Past the right end: max; past the left end: min.
        drag("control./s", "node./right"),
        info("/s"),
        drag("control./s", "node./left"),
        info("/s"),
        // A bound slider does not move.
        drag("control./bound", "node./right"),
        info("/bound"),
        json!({"method": "ui.state", "params": {"positions": true}}),
        // The rest of the node behaves as before: its centre selects it.
        json!({"method": "ui.click", "params": {"id": "node./s"}}),
        json!({"method": "ui.state", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(400).collect::<String>());
    }
    let ids: Vec<&str> = reply(&replies, "ui.elements", 0)["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4, "{ids:?}");

    let b = reply(&replies, "node.info", 0);
    assert_eq!(b["button"], json!({"mode": "momentary", "held": false, "presses": 1}), "{b}");
    assert_eq!(param(reply(&replies, "node.info", 1), "value"), json!(true), "the click switched it on");
    assert_eq!(param(reply(&replies, "node.info", 2), "value"), json!(1.0));
    assert_eq!(param(reply(&replies, "node.info", 3), "value"), json!(0.0));
    assert_eq!(param(reply(&replies, "node.info", 4), "value"), json!(0.0), "bound: not dragged");

    let (before, after) = (reply(&replies, "ui.state", 0), reply(&replies, "ui.state", 1));
    assert_eq!(before["positions"], after["positions"], "no node moved");
    assert_eq!(after["selection"], json!([]), "nothing got selected");
    assert_eq!(reply(&replies, "ui.state", 2)["selection"], json!(["/s"]));

    let history = engine.session().doc().with_history(|h| h.entries().len());
    assert_eq!(history, 3, "the toggle and one entry per slider drag; no press, no bound drag");
    engine.shutdown();
}
