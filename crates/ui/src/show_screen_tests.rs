//! The show screen's controls driven headless as a user would: while show mode is on, a
//! momentary button held, a toggle clicked and a slider dragged on the show screen change their
//! values without a history entry; Unlock still takes its two clicks.

use std::sync::Arc;

use penumbra_core::scratch::ScratchDir;
use penumbra_engine::Engine;
use penumbra_project::Session;
use serde_json::{Value as Json, json};

use crate::headless;

const PROJECT: &str = r#"(
    version: 1,
    nodes: {
        "/b": (kind: "chan.button", params: {"show": true}, pos: [0, 0]),
        "/t": (kind: "chan.button", params: {"mode": "toggle", "label": "lights", "show": true}, pos: [200, 0]),
        "/s": (kind: "chan.slider", params: {"label": "speed", "show": true}, pos: [400, 0]),
        "/hidden": (kind: "chan.slider", pos: [600, 0]),
        "/stage": (kind: "comp.base", pos: [0, 200]),
        "/stage/fade": (kind: "chan.slider", params: {"show": true, "value": 0.5}, pos: [0, 0]),
    },
)"#;

fn reply<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

fn ids(r: &Json) -> Vec<String> {
    r["elements"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn controls_on_the_show_screen_change_values_without_history() {
    let dir = ScratchDir::unborn("ui-show-controls");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let call = |method: &str, params: Json| json!({"method": method, "params": params});
    let drag =
        |from: &str, to: &str| call("ui.drag", json!({"from": {"id": from}, "to": {"id": to}, "steps": 6}));
    let script = [
        call("show.lock", json!({})),
        json!({"wait": 1.0}),
        call("ui.elements", json!({"prefix": "show.control."})),
        call("ui.elements", json!({"prefix": "button.show_unlock"})),
        // Held for a few frames, then let go.
        drag("show.control./b", "show.control./b"),
        call("node.info", json!({"path": "/b"})),
        call("ui.click", json!({"id": "show.control./t"})),
        // Dragged right past the next control: the slider's end.
        drag("show.control./s", "show.control./stage/fade"),
        // Dragged to its own left end, then up onto Unlock: let go there, it does not unlock.
        drag("show.control./stage/fade", "button.show_unlock"),
        call("show.status", json!({})),
        call("history.list", json!({"limit": 100})),
        call("ui.state", json!({})),
        // Unlock: two clicks.
        call("ui.click", json!({"id": "button.show_unlock"})),
        call("ui.click", json!({"id": "button.show_unlock"})),
        call("show.status", json!({})),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(600).collect::<String>());
    }
    assert_eq!(
        ids(reply(&replies, "ui.elements", 0)),
        ["show.control./b", "show.control./t", "show.control./s", "show.control./stage/fade"],
        "the flagged controls, in order of position; the one inside the component too"
    );
    assert_eq!(ids(reply(&replies, "ui.elements", 1)), ["button.show_unlock"]);
    let b = reply(&replies, "node.info", 0);
    assert_eq!(b["button"], json!({"mode": "momentary", "held": false, "presses": 1}), "{b}");

    let status = reply(&replies, "show.status", 0);
    assert_eq!(status["on"], true, "dragging onto Unlock did not unlock: {status}");
    let controls = status["controls"].as_array().unwrap();
    let labels: Vec<&str> = controls.iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["b", "lights", "speed", "stage/fade"], "{status}");
    let values: Vec<f64> = controls.iter().map(|c| c["value"].as_f64().unwrap()).collect();
    assert_eq!(values, [0.0, 1.0, 1.0, 0.0], "{status}");
    assert_eq!(controls[1]["mode"], "toggle");

    assert_eq!(reply(&replies, "history.list", 0)["entries"], json!([]), "no history entry");
    assert_eq!(reply(&replies, "ui.state", 0)["selection"], json!([]), "nothing got selected");
    let after = reply(&replies, "show.status", 1);
    assert_eq!(after["on"], false, "two clicks on Unlock end show mode: {after}");
    assert_eq!(after["controls"][2]["value"], 1.0, "the values set in show mode stay: {after}");
    engine.shutdown();
}

#[test]
fn a_button_with_a_label_has_no_caption_above_its_face() {
    use crate::node_controls::Control;
    let button = |label: &str| Control::Button { toggle: true, on: false, label: label.into(), bound: false };
    let slider = Control::Slider { value: 0.5, min: 0.0, max: 1.0, bound: false };
    assert_eq!(crate::show_screen::caption("/go", "go", &button("")), "go");
    assert_eq!(crate::show_screen::caption("/go", "GO", &button("GO")), "");
    assert_eq!(crate::show_screen::caption("/stage/go", "stage/GO", &button("GO")), "stage");
    assert_eq!(crate::show_screen::caption("/stage/a/go", "stage/a/GO", &button("GO")), "stage/a");
    assert_eq!(crate::show_screen::caption("/s", "speed", &slider), "speed");
}
