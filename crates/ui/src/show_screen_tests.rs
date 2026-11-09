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

/// Closing the editor window would end the show: in show mode the close is refused and the
/// status bar says why; out of it the window closes as usual.
#[test]
fn the_editor_window_stays_open_in_show_mode() {
    let dir = ScratchDir::unborn("ui-show-close");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    engine.set_show(true).unwrap();
    let (_guard, mut harness) = headless::software_app(engine.clone());
    let close = |harness: &mut egui_kittest::Harness<'_, crate::app::PenumbraApp>| {
        let root = harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default();
        root.events.push(egui::ViewportEvent::Close);
        harness.step();
        harness
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    };
    assert!(close(&mut harness), "refused in show mode");
    harness.step();
    use egui_kittest::kittest::Queryable;
    assert!(harness.query_by_label_contains("Unlock it before closing").is_some(), "and says why");
    engine.set_show(false).unwrap();
    harness.step();
    assert!(!close(&mut harness), "closes as usual out of show mode");
    drop(harness);
    engine.shutdown();
}

/// A momentary button held down on its node in the editor when show mode begins: the editor is
/// no longer drawn and never sees the release, so the button is let go as the show begins.
#[test]
fn a_button_held_in_the_editor_is_let_go_when_the_show_begins() {
    let dir = ScratchDir::unborn("ui-show-held");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let b = engine.session().doc().snapshot().resolve(&"/b".parse().unwrap()).unwrap();
    let (_guard, mut harness) = headless::software_app(engine.clone());
    let mut at = None;
    for _ in 0..120 {
        harness.step();
        at = harness.state().element("control./b").map(|r| r.center());
        if at.is_some() {
            break;
        }
    }
    let at = at.expect("the button's control in the editor");
    let input = |harness: &mut egui_kittest::Harness<'_, crate::app::PenumbraApp>, e: egui::Event| {
        harness.input_mut().events.push(e);
        harness.step();
    };
    input(&mut harness, egui::Event::PointerMoved(at));
    let press = egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    };
    input(&mut harness, press);
    harness.step();
    assert!(engine.buttons().view(b).held, "held while the pointer is down");
    engine.set_show(true).unwrap();
    for _ in 0..3 {
        harness.step();
    }
    assert!(!engine.buttons().view(b).held, "let go when the show begins");
    drop(harness);
    engine.shutdown();
}

#[test]
fn a_dropdown_on_the_show_screen_picks_without_history() {
    let dir = ScratchDir::unborn("ui-show-dropdown");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let project = r#"(
        version: 1,
        nodes: {
            "/look": (kind: "chan.dropdown", params: {"entries": "ember\nabyss\nbio", "show": true}, pos: [0, 0]),
            "/s": (kind: "chan.slider", params: {"show": true}, pos: [200, 0]),
        },
    )"#;
    let content = penumbra_project::parse(project, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let call = |method: &str, params: Json| json!({"method": method, "params": params});
    let script = [
        call("show.lock", json!({})),
        json!({"wait": 1.0}),
        call("ui.click", json!({"id": "show.control./look"})),
        call("ui.elements", json!({"prefix": "show.control./look.entry."})),
        call("ui.click", json!({"id": "show.control./look.entry.1"})),
        call("show.status", json!({})),
        call("history.list", json!({"limit": 100})),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    assert_eq!(ids(reply(&replies, "ui.elements", 0)).len(), 3, "the menu lists the entries");
    let status = reply(&replies, "show.status", 0);
    assert_eq!(status["controls"][0]["entry"], "abyss", "{status}");
    assert_eq!(status["controls"][0]["value"], 1.0, "{status}");
    assert_eq!(reply(&replies, "history.list", 0)["entries"], json!([]), "no history entry");
    engine.shutdown();
}
