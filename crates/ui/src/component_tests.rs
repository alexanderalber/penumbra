//! A component's params through the panel, headless on WARP: put an inner param on the component
//! and follow one from the link menu, go to the component from a follower's row, and the line
//! errors of the `params` box.

use std::sync::Arc;

use penumbra_core::scratch::ScratchDir;
use penumbra_engine::Engine;
use penumbra_project::Session;
use serde_json::{Value as Json, json};

use crate::headless;

const PROJECT: &str = r#"(
    version: 2,
    nodes: {
        "/rd": (kind: "comp.base", params: {"params": "@param feed float = 0.05\n@param grain quaternion"}),
        "/rd/m": (kind: "chan.math", params: {"op": "multiply"}),
        "/rd/n": (kind: "chan.math"),
    },
)"#;

fn reply<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

fn ids(elements: &Json) -> Vec<String> {
    elements["elements"]
        .as_array()
        .map(|list| list.iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[test]
fn put_follow_and_show_from_the_link_menu() {
    let dir = ScratchDir::unborn("ui-comp-params");
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(dir.path(), types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 1.0}),
        // The box shows its bad line, numbered as in the text.
        json!({"method": "ui.select", "params": {"paths": ["/rd"]}}),
        json!({"method": "ui.click", "params": {"id": "param./rd.params.edit"}}),
        json!({"wait": 0.2}),
        json!({"method": "ui.elements", "params": {"prefix": "param./rd."}}),
        // Put /rd/m.op on the component.
        json!({"method": "ui.select", "params": {"paths": ["/rd/m"]}}),
        json!({"method": "ui.click", "params": {"id": "param./rd/m.op.bind"}}),
        json!({"method": "ui.elements", "params": {"prefix": "bind."}}),
        json!({"method": "ui.click", "params": {"id": "bind.put"}}),
        json!({"wait": 0.3}),
        json!({"method": "node.info", "params": {"path": "/rd"}}),
        // /rd/n.op follows it from the menu.
        json!({"method": "ui.select", "params": {"paths": ["/rd/n"]}}),
        json!({"method": "ui.click", "params": {"id": "param./rd/n.op.bind"}}),
        json!({"method": "ui.click", "params": {"id": "bind.parent.op"}}),
        json!({"wait": 0.3}),
        json!({"method": "node.info", "params": {"path": "/rd/n"}}),
        // /rd/n.b is shown on the component.
        json!({"method": "ui.click", "params": {"id": "param./rd/n.b.bind"}}),
        json!({"method": "ui.click", "params": {"id": "bind.show"}}),
        json!({"wait": 0.3}),
        json!({"method": "node.info", "params": {"path": "/rd"}}),
        // The follower's source goes to the component.
        json!({"method": "ui.click", "params": {"id": "param./rd/n.op.from"}}),
        json!({"wait": 0.2}),
        json!({"method": "ui.state", "params": {}}),
        json!({"method": "ui.elements", "params": {"prefix": "param./rd"}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1400.0, 1000.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(600).collect::<String>());
    }
    let panel = reply(&replies, "ui.elements", 0);
    assert!(ids(panel).contains(&"param./rd.params.error.0".to_string()), "{panel}");
    assert!(panel.to_string().contains("line 2: unknown kind `quaternion`"), "{panel}");
    let menu = ids(reply(&replies, "ui.elements", 1));
    for id in ["bind.parent", "bind.show", "bind.put"] {
        assert!(menu.contains(&id.to_string()), "{menu:?}");
    }
    assert!(!menu.contains(&"bind.parent.feed".to_string()), "a float is no choice to follow: {menu:?}");

    let rd = reply(&replies, "node.info", 0);
    let row = |info: &Json, name: &str| {
        info["instance_params"].as_array().and_then(|l| l.iter().find(|r| r["name"] == name)).cloned()
    };
    let op = row(rd, "op").unwrap_or_default();
    assert_eq!(op["value"], "multiply", "{rd}");
    assert_eq!(op["followed_by"], json!(["/rd/m.op"]), "{rd}");
    let n = reply(&replies, "node.info", 1);
    assert_eq!(n["binds"][0]["from"], "parent.op", "{n}");
    let rd = reply(&replies, "node.info", 2);
    let text = rd["params"].as_array().and_then(|l| l.iter().find(|r| r["name"] == "params")).cloned();
    let text = text.map(|r| r["value"].clone()).unwrap_or_default();
    assert!(text.as_str().is_some_and(|t| t.ends_with("@expose n.b")), "{text}");

    let state = reply(&replies, "ui.state", 0);
    assert_eq!(state["level"], "/", "{state}");
    assert_eq!(state["selection"], json!(["/rd"]), "{state}");
    let rows = ids(reply(&replies, "ui.elements", 2));
    for id in ["param./rd.op.used", "param./rd.feed.used", "param./rd/n.b.inside"] {
        assert!(rows.contains(&id.to_string()), "{id}: {rows:?}");
    }
    engine.shutdown();
}
