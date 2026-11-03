//! Headless runs as a tutorial recording drives them: frames on a stepped clock, a drawn
//! pointer that glides, and the add dialog chosen by keys.

use std::sync::Arc;

use penumbra_core::scratch::ScratchDir;
use penumbra_engine::Engine;
use penumbra_project::{Content, Session};
use serde_json::{Value as Json, json};

use crate::headless;

fn engine(dir: &ScratchDir) -> Arc<Engine> {
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let session = Arc::new(Session::open(dir.path(), types, &Content::default()).unwrap());
    Engine::start(session).unwrap()
}

fn run(engine: &Arc<Engine>, script: &[Json]) -> Vec<(String, Json)> {
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [640.0, 400.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }
    replies
}

fn ok<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

#[test]
fn a_recording_steps_the_clock_by_one_frame_per_picture() {
    let dir = ScratchDir::unborn("ui-record");
    let frames = dir.path().join("frames");
    let engine = engine(&dir);
    let replies = run(
        &engine,
        &[
            json!({"method": "ui.pointer", "params": {"show": true, "glide": 6}}),
            json!({"method": "cook.status", "params": {}}),
            json!({"record": {"dir": frames.display().to_string(), "fps": 30}}),
            json!({"wait": 0.2}),
            json!({"method": "ui.hover", "params": {"x": 100, "y": 100}}),
            json!({"method": "ui.hover", "params": {"x": 300, "y": 200}}),
            json!({"method": "cook.status", "params": {}}),
            json!({"record": false}),
            json!({"method": "cook.status", "params": {}}),
        ],
    );
    let done = ok(&replies, "record", 1);
    let n = done["frames"].as_u64().unwrap();
    // 6 for the wait; the first hover jumps (nowhere to glide from), the second glides 6 frames;
    // each request takes at least one frame and three more to show its result.
    assert!(n >= 6 + 4 + 6 + 3 + 4, "{n} frames");
    let pngs = std::fs::read_dir(&frames)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("frame_"))
        .count();
    assert_eq!(pngs as u64, n);
    // 60 fps show, 30 fps recording: two show frames per picture, none in between.
    let (before, during) = (ok(&replies, "cook.status", 0), ok(&replies, "cook.status", 1));
    assert_eq!(during["stepping"], true);
    let moved = during["frame"].as_i64().unwrap() - before["frame"].as_i64().unwrap();
    // The status request itself is answered before its own frame is drawn.
    assert!((2 * n as i64 - 8..=2 * n as i64).contains(&moved), "{moved} show frames for {n} pictures");
    assert_eq!(ok(&replies, "cook.status", 2)["stepping"], false, "back on the timer");
    let marks = std::fs::read_to_string(frames.join("marks.jsonl")).unwrap();
    let marks: Vec<Json> = marks.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(
        marks.first().map(|m| (m["what"].clone(), m["line"].clone(), m["first"].clone())),
        Some((json!("wait"), json!(4), json!(0)))
    );
    assert!(marks.iter().any(|m| m["what"] == "ui.hover"));
    // Recording again into the same folder is refused, the frames stay.
    let again = run(&engine, &[json!({"record": {"dir": frames.display().to_string(), "fps": 30}})]);
    assert!(again[0].1["error"].as_str().unwrap().contains("already holds frames"), "{:?}", again[0]);
    let odd =
        run(&engine, &[json!({"record": {"dir": dir.path().join("odd").display().to_string(), "fps": 25}})]);
    assert!(odd[0].1["error"].as_str().unwrap().contains("divides"), "{:?}", odd[0]);
    engine.shutdown();
}

#[test]
fn tab_to_another_family_and_enter_adds_its_first_match() {
    let dir = ScratchDir::unborn("ui-add-tab");
    let engine = engine(&dir);
    let replies = run(
        &engine,
        &[
            json!({"method": "ui.hover", "params": {"x": 200, "y": 150}}),
            json!({"method": "ui.key", "params": {"key": "Tab"}}),
            json!({"wait_for": "add.filter"}),
            json!({"method": "ui.type", "params": {"text": "noise"}}),
            json!({"method": "ui.key", "params": {"key": "Enter"}}),
            json!({"wait": 0.3}),
            // Away from the noise, so the next node is not wired to it.
            json!({"method": "ui.click", "params": {"x": 200, "y": 330}}),
            json!({"method": "ui.key", "params": {"key": "Tab"}}),
            json!({"wait_for": "add.filter"}),
            json!({"method": "ui.type", "params": {"text": "lfo"}}),
            // The dialog opens on tex again, which has no lfo; Tab goes on to chan.
            json!({"method": "ui.key", "params": {"key": "Tab"}}),
            json!({"method": "ui.key", "params": {"key": "Enter"}}),
            json!({"wait": 0.3}),
        ],
    );
    let _ = ok(&replies, "ui.key", 2);
    let net = engine.session().doc().snapshot();
    let kinds: Vec<String> = net.nodes().map(|n| n.kind.clone()).collect();
    assert!(kinds.contains(&"chan.lfo".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"tex.noise".to_string()), "the exact name, not tex.denoise: {kinds:?}");
    engine.shutdown();
}
