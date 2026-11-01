//! Notes on the canvas and the way into show mode, driven headless as a user would: a note has
//! no ports, its corner handle resizes it in one undo step, a double-click writes in it, arranging
//! takes it along and leaves no node under it; show mode starts from the canvas menu or its keys,
//! and locks the Play/Pause button.

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

fn param_of(info: &Json, name: &str) -> Json {
    let params = info["params"].as_array().unwrap();
    params.iter().find(|p| p["name"] == name).map(|p| p["value"].clone()).unwrap()
}

fn rect_of(info: &Json, size: [f64; 2]) -> [f64; 4] {
    let (x, y) = (info["pos"][0].as_f64().unwrap(), info["pos"][1].as_f64().unwrap());
    [x, y, x + size[0], y + size[1]]
}

fn overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

#[test]
fn a_note_has_no_ports_resizes_in_one_step_and_goes_along_when_arranged() {
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
        call("node.info", json!({"path": "/a"})),
        call("node.info", json!({"path": "/b"})),
        // Arranging the arrangement changes nothing, notes included.
        call("ui.arrange", json!({})),
    ];
    let (engine, replies, _dir) = run("ui-note", &script);
    let ports = ids(reply(&replies, "ui.elements", 0));
    assert!(ports.iter().all(|p| !p.contains("/n")), "a note has no ports: {ports:?}");
    assert_eq!(ids(reply(&replies, "ui.elements", 1)), ["resize./n"]);

    let n = reply(&replies, "node.info", 0);
    let (w, h) = (param_of(n, "width").as_f64().unwrap(), param_of(n, "height").as_f64().unwrap());
    assert!(w > 400.0 && h > 400.0, "dragged towards /b, the note grew: {n}");
    let entries = reply(&replies, "history.list", 0)["entries"].as_array().unwrap().len();
    assert_eq!(entries, 1, "the whole resize is one undo step");

    let note = rect_of(reply(&replies, "node.info", 1), [w, h]);
    let node = crate::layout::NODE_SIZE;
    let (a, b) = (reply(&replies, "node.info", 2), reply(&replies, "node.info", 3));
    assert_ne!(b["pos"], json!([700.0, 600.0]), "arranging moves the nodes: {b}");
    for other in [a, b] {
        let r = rect_of(other, [f64::from(node.x), f64::from(node.y)]);
        assert!(!overlap(note, r), "no node under the note after arranging: {note:?} {other}");
    }
    assert_eq!(reply(&replies, "ui.arrange", 1)["history"], "no_change", "arranging again keeps it all");
    engine.shutdown();
}

#[test]
fn a_double_click_writes_in_a_note_without_editor_keys_in_one_step() {
    let script = [
        call("ui.frame", json!({})),
        json!({"wait": 0.5}),
        call("ui.click", json!({"id": "node./n", "double": true})),
        call("ui.elements", json!({"prefix": "note_text."})),
        // Keys the editor uses (L arranges, F frames, Delete deletes) go into the text.
        call("ui.key", json!({"key": "End", "ctrl": true})),
        call("ui.type", json!({"text": "\nLF "})),
        call("ui.key", json!({"key": "L"})),
        call("ui.key", json!({"key": "Delete"})),
        call("ui.key", json!({"key": "Escape"})),
        call("node.info", json!({"path": "/n"})),
        call("node.info", json!({"path": "/a"})),
        call("history.list", json!({"limit": 100})),
        call("ui.elements", json!({"prefix": "note_text."})),
        // A click outside ends writing too.
        call("ui.click", json!({"id": "node./n", "double": true})),
        call("ui.type", json!({"text": "!"})),
        call("ui.click", json!({"id": "node./b"})),
        call("history.list", json!({"limit": 100})),
        call("ui.elements", json!({"prefix": "note_text."})),
    ];
    let (engine, replies, _dir) = run("ui-note-edit", &script);
    assert_eq!(
        ids(reply(&replies, "ui.elements", 0)),
        ["note_text./n"],
        "the double-click opens the text field"
    );
    let text = param_of(reply(&replies, "node.info", 0), "text");
    assert_eq!(text, "# Title\n- a bullet\nplain text\nLF ", "the typed text, at the end");
    let a = reply(&replies, "node.info", 1);
    assert_eq!(a["pos"], json!([0.0, 300.0]), "L arranged nothing, Delete deleted nothing: {a}");
    let entries = reply(&replies, "history.list", 0)["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 1, "one undo step for the whole text: {entries:?}");
    assert_eq!(ids(reply(&replies, "ui.elements", 1)), Vec::<String>::new(), "Escape ends writing");
    assert_eq!(reply(&replies, "history.list", 1)["entries"].as_array().unwrap().len(), 2);
    assert_eq!(ids(reply(&replies, "ui.elements", 2)), Vec::<String>::new(), "a click outside ends writing");
    engine.shutdown();
}

#[test]
fn show_mode_starts_from_the_canvas_menu_and_its_keys_and_locks_play() {
    let script = [
        call("ui.elements", json!({"prefix": "button.show"})),
        call("ui.click", json!({"id": "button.project"})),
        call("ui.elements", json!({"prefix": "project.show_mode"})),
        call("ui.key", json!({"key": "Escape"})),
        call("ui.click", json!({"x": 15, "y": 600, "button": "right"})),
        call("ui.click", json!({"id": "options.show_mode"})),
        call("show.status", json!({})),
        // Play/Pause is locked: a click does not stop the clock, cook.play still does.
        call("ui.click", json!({"id": "button.play"})),
        call("cook.status", json!({})),
        call("cook.play", json!({"playing": false})),
        call("cook.status", json!({})),
        call("cook.play", json!({"playing": true})),
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
    let none = Vec::<String>::new();
    assert_eq!(ids(reply(&replies, "ui.elements", 0)), none, "no lock button in the status bar");
    assert_eq!(ids(reply(&replies, "ui.elements", 1)), none, "no show mode in the project menu");
    assert_eq!(reply(&replies, "show.status", 0)["on"], true, "the canvas menu entry locks");
    assert_eq!(reply(&replies, "cook.status", 0)["playing"], true, "the locked button does not pause");
    assert_eq!(reply(&replies, "cook.status", 1)["playing"], false, "cook.play pauses in show mode");
    assert_eq!(reply(&replies, "show.status", 1)["on"], false, "two clicks on Unlock end it");
    assert_eq!(reply(&replies, "show.status", 2)["on"], false, "Ctrl+L is not the shortcut");
    assert_eq!(reply(&replies, "show.status", 3)["on"], true, "Ctrl+Shift+L locks");
    engine.shutdown();
}
