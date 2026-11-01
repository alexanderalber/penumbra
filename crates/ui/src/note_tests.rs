//! Notes on the canvas and the way into show mode, driven headless as a user would: a note has
//! no ports, its corner handle resizes it in one undo step, arranging leaves it where it is; show
//! mode starts from the project menu or its keys, and the status bar has no lock button.

use std::sync::Arc;

use penumbra_core::scratch::ScratchDir;
use penumbra_engine::Engine;
use penumbra_project::Session;
use serde_json::{Value as Json, json};

use crate::headless;

const PROJECT: &str = r##"(
    version: 1,
    nodes: {
        "/n": (kind: "ui.note", params: {"text": "# Title\n- a bullet\nplain text"}, pos: [0, 0]),
        "/a": (kind: "chan.lfo", pos: [0, 300]),
        "/b": (kind: "chan.null", inputs: {"in": "/a"}, pos: [700, 600]),
    },
)"##;

fn reply<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

fn ids(r: &Json) -> Vec<String> {
    r["elements"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
}

fn run(name: &str, script: &[Json]) -> (Arc<Engine>, Vec<(String, Json)>, ScratchDir) {
    let dir = ScratchDir::unborn(name);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(600).collect::<String>());
    }
    (engine, replies, dir)
}

fn call(method: &str, params: Json) -> Json {
    json!({"method": method, "params": params})
}

#[test]
fn a_note_has_no_ports_resizes_in_one_step_and_stays_put_when_arranged() {
    let script = [
        call("ui.frame", json!({})),
        json!({"wait": 0.5}),
        call("ui.elements", json!({"prefix": "port."})),
        call("ui.elements", json!({"prefix": "resize."})),
        call("ui.drag", json!({"from": {"id": "resize./n"}, "to": {"id": "node./b"}, "steps": 8})),
        call("node.info", json!({"path": "/n"})),
        call("history.list", json!({"limit": 100})),
        // Arranging waits for the user's last edit to be 3 s old.
        json!({"wait": 3.5}),
        call("ui.arrange", json!({})),
        call("node.info", json!({"path": "/n"})),
        call("node.info", json!({"path": "/b"})),
    ];
    let (engine, replies, _dir) = run("ui-note", &script);
    let ports = ids(reply(&replies, "ui.elements", 0));
    assert!(ports.iter().all(|p| !p.contains("/n")), "a note has no ports: {ports:?}");
    assert_eq!(ids(reply(&replies, "ui.elements", 1)), ["resize./n"]);

    let n = reply(&replies, "node.info", 0);
    let param = |name: &str| {
        let params = n["params"].as_array().unwrap();
        params.iter().find(|p| p["name"] == name).and_then(|p| p["value"].as_i64()).unwrap()
    };
    let (w, h) = (param("width"), param("height"));
    assert!(w > 400 && h > 400, "dragged towards /b, the note grew: {n}");
    let entries = reply(&replies, "history.list", 0)["entries"].as_array().unwrap().len();
    assert_eq!(entries, 1, "the whole resize is one undo step");

    let after = reply(&replies, "node.info", 1);
    assert_eq!(after["pos"], json!([0.0, 0.0]), "arranging leaves the note where it was: {after}");
    let b = reply(&replies, "node.info", 2);
    assert_ne!(b["pos"], json!([700.0, 600.0]), "but arranges the other nodes: {b}");
    engine.shutdown();
}

#[test]
fn show_mode_starts_from_the_project_menu_and_its_keys() {
    let script = [
        call("ui.elements", json!({"prefix": "button.show"})),
        call("ui.click", json!({"id": "button.project"})),
        call("ui.click", json!({"id": "project.show_mode"})),
        call("show.status", json!({})),
        call("ui.click", json!({"id": "button.show_unlock"})),
        call("ui.click", json!({"id": "button.show_unlock"})),
        call("show.status", json!({})),
        // Ctrl+L alone does nothing; Ctrl+Shift+L locks.
        call("ui.key", json!({"key": "L", "ctrl": true})),
        call("show.status", json!({})),
        call("ui.key", json!({"key": "L", "ctrl": true, "shift": true})),
        call("show.status", json!({})),
    ];
    let (engine, replies, _dir) = run("ui-show-menu", &script);
    assert_eq!(
        ids(reply(&replies, "ui.elements", 0)),
        Vec::<String>::new(),
        "no lock button in the status bar"
    );
    assert_eq!(reply(&replies, "show.status", 0)["on"], true, "the menu entry locks");
    assert_eq!(reply(&replies, "show.status", 1)["on"], false, "two clicks on Unlock end it");
    assert_eq!(reply(&replies, "show.status", 2)["on"], false, "Ctrl+L is not the shortcut");
    assert_eq!(reply(&replies, "show.status", 3)["on"], true, "Ctrl+Shift+L locks");
    engine.shutdown();
}
