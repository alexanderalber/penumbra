//! The whole UI driven headless on WARP, the way Claude drives it over the control channel:
//! add a node with the keyboard, wire it with the mouse, drag a parameter, undo, screenshot.

use std::path::PathBuf;
use std::sync::Arc;

use penumbra_engine::Engine;
use penumbra_project::Session;
use serde_json::{Value as Json, json};

use crate::headless;

const PROJECT: &str = r#"(
    version: 1,
    nodes: {
        "/noise1": (kind: "tex.noise", params: {"width": 160, "height": 90}),
        "/level1": (kind: "tex.level", inputs: {"in": "/noise1"}),
        "/c": (kind: "chan.constant", params: {"value0": 0.5}),
    },
)"#;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn reply<'a>(replies: &'a [(String, Json)], method: &str, nth: usize) -> &'a Json {
    let (_, r) =
        replies.iter().filter(|(m, _)| m == method).nth(nth).unwrap_or_else(|| panic!("no {method} #{nth}"));
    assert_eq!(r["ok"], true, "{method}: {r}");
    &r["result"]
}

#[test]
fn build_wire_tweak_and_undo_through_the_ui() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let shot = dir.0.join("shot.png");

    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.click", "params": {"id": "node./level1"}}),
        json!({"method": "ui.key", "params": {"key": "Tab"}}),
        json!({"method": "ui.type", "params": {"text": "composite"}}),
        json!({"method": "ui.key", "params": {"key": "Enter"}}),
        json!({"method": "ui.drag", "params": {"from": {"id": "port.out./noise1.0"}, "to": {"id": "port.in./composite1.1"}}}),
        json!({"method": "ui.click", "params": {"id": "node./level1"}}),
        json!({"method": "ui.elements", "params": {"prefix": "param./level1.opacity"}}),
        json!({"method": "ui.drag", "params": {"from": {"id": "param./level1.opacity.1"}, "to": {"id": "param./level1.opacity.0"}}}),
        json!({"method": "node.info", "params": {"path": "/level1"}}),
        json!({"method": "ui.click", "params": {"id": "button.undo"}}),
        json!({"method": "node.info", "params": {"path": "/level1"}}),
        json!({"method": "node.info", "params": {"path": "/composite1"}}),
        // The add dialog by double-click on empty canvas; typing marks the family with a
        // match but stays on the tab, Tab moves on to it.
        json!({"method": "ui.click", "params": {"x": 15, "y": 60, "double": true}}),
        json!({"method": "ui.type", "params": {"text": "lfo"}}),
        json!({"method": "ui.key", "params": {"key": "Enter"}}),
        json!({"method": "ui.elements", "params": {"prefix": "add.tab."}}),
        json!({"method": "ui.key", "params": {"key": "Tab"}}),
        json!({"method": "ui.key", "params": {"key": "Enter"}}),
        json!({"method": "node.info", "params": {"path": "/lfo1"}}),
        // The editor's options by right-click on empty canvas, and a collapsed section.
        json!({"method": "ui.click", "params": {"x": 15, "y": 600, "button": "right"}}),
        json!({"method": "ui.click", "params": {"id": "options.chan_bars"}}),
        json!({"method": "ui.click", "params": {"x": 15, "y": 600, "button": "right"}}),
        json!({"method": "ui.click", "params": {"id": "thumbnails.never"}}),
        json!({"method": "ui.options", "params": {}}),
        json!({"method": "cook.status", "params": {}}),
        json!({"method": "ui.click", "params": {"id": "section.changes"}}),
        json!({"wait": 0.2}),
        json!({"method": "ui.elements", "params": {"prefix": "history."}}),
        json!({"method": "ui.elements", "params": {"prefix": "button.undo"}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.screenshot", "params": {"file": shot.display().to_string()}}),
        json!({"method": "ui.state", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }

    // The new node sits after the selected one, fed by it, and the drag wired its second input.
    let info = reply(&replies, "node.info", 2);
    assert_eq!(info["inputs"][0]["from"], "/level1", "auto-connected from the selection");
    assert_eq!(info["inputs"][1]["from"], "/noise1", "wired by dragging");
    let opacity = |i: &Json| {
        i["params"].as_array().unwrap().iter().find(|p| p["name"] == "opacity").unwrap()["value"].clone()
    };
    assert_ne!(opacity(reply(&replies, "node.info", 0)), json!(1.0), "the slider drag changed the opacity");
    // One drag is one undo step.
    assert_eq!(opacity(reply(&replies, "node.info", 1)), json!(1.0));

    let tabs: Vec<&str> = reply(&replies, "ui.elements", 1)["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        tabs,
        ["tex", "chan (match)", "table", "rendering", "system"],
        "Enter without a match in tex did nothing"
    );
    assert_eq!(reply(&replies, "node.info", 3)["kind"], "chan.lfo", "typed into the dialog, Enter");
    assert_eq!(reply(&replies, "cook.status", 0)["thumbnails"]["mode"], "never");
    assert_eq!(reply(&replies, "ui.options", 0)["chan_bars"], false, "switched off in the menu");
    assert_eq!(reply(&replies, "ui.elements", 2)["elements"], json!([]), "changes collapsed");
    assert_eq!(reply(&replies, "ui.elements", 3)["elements"].as_array().unwrap().len(), 1, "undo stays");

    let state = reply(&replies, "ui.state", 0);
    assert_eq!(state["selection"], json!(["/lfo1"]));
    let png = std::fs::metadata(&shot).unwrap();
    assert!(png.len() > 10_000, "a screenshot of the whole window");

    let history = engine
        .session()
        .doc()
        .with_history(|h| h.entries().iter().map(|e| e.label.clone()).collect::<Vec<_>>());
    assert_eq!(history.len(), 3, "create, connect, create (the undone set is gone): {history:?}");
    engine.shutdown();
    let layout = std::fs::read_to_string(dir.0.join(crate::layout::FILE_NAME)).unwrap();
    assert!(layout.contains("/composite1") && layout.contains("/lfo1"), "{layout}");
}

#[test]
fn reset_stateful_nodes_with_the_key_and_the_button() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-reset-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let project = r#"(
        version: 1,
        nodes: {
            "/seed": (kind: "tex.noise", params: {"width": 64, "height": 36}),
            "/loop": (kind: "tex.feedback", inputs: {"initial": "seed", "previous": "keep"}),
            "/keep": (kind: "tex.level", inputs: {"in": "loop"}),
            "/smooth": (kind: "chan.filter"),
        },
    )"#;
    let content = penumbra_project::parse(project, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 0.5}),
        // Nothing selected: R resets every stateful node of the level.
        json!({"method": "ui.click", "params": {"id": "node./keep"}}),
        json!({"method": "ui.select", "params": {"paths": []}}),
        json!({"method": "ui.key", "params": {"key": "R"}}),
        json!({"wait": 0.1}),
        json!({"method": "ui.state", "params": {}}),
        // A selection: only it.
        json!({"method": "ui.select", "params": {"paths": ["/keep"]}}),
        json!({"method": "ui.key", "params": {"key": "R"}}),
        json!({"wait": 0.1}),
        json!({"method": "ui.state", "params": {}}),
        // The button sits in the params of stateful nodes only.
        json!({"method": "ui.elements", "params": {"prefix": "param.reset"}}),
        json!({"method": "ui.select", "params": {"paths": ["/loop"]}}),
        json!({"wait": 0.1}),
        json!({"method": "ui.click", "params": {"id": "param.reset"}}),
        json!({"wait": 0.1}),
        json!({"method": "ui.state", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }
    assert_eq!(reply(&replies, "ui.state", 0)["message"], "reset /loop, /smooth");
    assert_eq!(reply(&replies, "ui.state", 1)["message"], "nothing to reset; no state: /keep");
    assert_eq!(reply(&replies, "ui.elements", 0)["elements"], json!([]), "/keep has no reset button");
    assert_eq!(reply(&replies, "ui.state", 2)["message"], "reset /loop");
    engine.shutdown();
}

#[test]
fn bind_and_unbind_through_the_param_panel() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-bind-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.click", "params": {"id": "node./level1"}}),
        json!({"method": "ui.click", "params": {"id": "param./level1.brightness.bind"}}),
        json!({"method": "ui.click", "params": {"id": "bind.0"}}),
        json!({"wait": 0.2}),
        json!({"method": "node.info", "params": {"path": "/level1"}}),
        json!({"method": "ui.click", "params": {"id": "param./level1.brightness.bind"}}),
        json!({"method": "ui.click", "params": {"id": "bind.unbind.brightness"}}),
        json!({"method": "node.info", "params": {"path": "/level1"}}),
        // Dragging the constant's output wire onto a param binds it.
        json!({"method": "ui.drag", "params": {"from": {"id": "port.out./c.0"}, "to": {"id": "param./level1.gamma"}}}),
        json!({"wait": 0.2}),
        json!({"method": "node.info", "params": {"path": "/level1"}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }
    let bound = reply(&replies, "node.info", 0);
    assert_eq!(bound["binds"], json!([{"param": "brightness", "from": "/c"}]));
    assert_eq!(bound["status"]["bound"], json!({"brightness": 0.5}));
    assert_eq!(reply(&replies, "node.info", 1)["binds"], json!([]));
    assert_eq!(reply(&replies, "node.info", 2)["binds"], json!([{"param": "gamma", "from": "/c"}]));
    engine.shutdown();
}

#[test]
fn moving_nodes_is_undone_in_turn_with_edits() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-move-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        json!({"method": "ui.drag", "params": {"from": {"id": "node./c"}, "to": {"x": 600, "y": 600}}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        // An edit after the move: the first undo takes the delete back (the node returns
        // where it was moved to), the second the move.
        json!({"method": "ui.click", "params": {"id": "node./c"}}),
        json!({"method": "ui.key", "params": {"key": "Delete"}}),
        json!({"method": "node.list", "params": {}}),
        json!({"method": "ui.key", "params": {"key": "Z", "ctrl": true}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        json!({"method": "ui.key", "params": {"key": "Z", "ctrl": true}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        json!({"method": "ui.key", "params": {"key": "Z", "ctrl": true, "shift": true}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }
    let pos = |n: usize| reply(&replies, "ui.state", n)["positions"]["/c"].clone();
    let (start, moved) = (pos(0), pos(1));
    assert_ne!(start, moved, "the drag moved the node");
    assert!(!reply(&replies, "node.list", 0).to_string().contains("\"/c\""), "deleted");
    assert_eq!(pos(2), moved, "the first undo brought the node back where it was");
    assert_eq!(pos(3), start, "the second undo moved the node back");
    assert_eq!(pos(4), moved, "redo moved it again");
    engine.shutdown();
}

#[test]
fn map_by_dragging_handles_zoom_and_open_the_window() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-map-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(
        r#"(
            version: 1,
            nodes: {
                "/noise1": (kind: "tex.noise", params: {"width": 160, "height": 90}),
                "/m": (kind: "tex.mapper", params: {"fit": "stretch", "width": 480, "height": 270}, inputs: {"in": "/noise1"}),
                "/p": (kind: "out.projector", inputs: {"in": "/m"}),
            },
        )"#,
        &types,
    )
    .unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"method": "ui.select", "params": {"paths": ["/m"]}}),
        json!({"wait": 1.0}),
        // A 3 x 3 mesh, its centre dragged onto the top right corner, the top left corner
        // dragged onto the middle of the left edge, then the mesh point nudged one projector
        // pixel right with the arrow key.
        json!({"method": "ui.click", "params": {"id": "viewer.mesh"}}),
        json!({"method": "ui.click", "params": {"id": "viewer.mesh.1"}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.drag", "params": {"from": {"id": "viewer.mesh.1.1"}, "to": {"id": "viewer.corner.1"}, "steps": 4}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.drag", "params": {"from": {"id": "viewer.corner.0"}, "to": {"id": "viewer.mesh.0.1"}, "steps": 4}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.click", "params": {"id": "viewer.mesh.1.1"}}),
        json!({"wait": 0.2}),
        json!({"method": "node.info", "params": {"path": "/m"}}),
        json!({"method": "ui.key", "params": {"key": "ArrowRight"}}),
        json!({"wait": 0.2}),
        json!({"method": "node.info", "params": {"path": "/m"}}),
        // Zooming out with the wheel brings the corners closer together.
        json!({"method": "ui.elements", "params": {"prefix": "viewer.corner."}}),
        json!({"method": "ui.scroll", "params": {"id": "viewer", "dy": -300}}),
        json!({"wait": 0.3}),
        json!({"method": "ui.elements", "params": {"prefix": "viewer.corner."}}),
        // The window: the button goes away while it is open; here (headless) it is embedded.
        json!({"method": "ui.click", "params": {"id": "viewer.window"}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.elements", "params": {"prefix": "viewer.window"}}),
        json!({"method": "ui.screenshot", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    let param = |nth: usize, name: &str| {
        let info = reply(&replies, "node.info", nth);
        info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).unwrap()["value"].clone()
    };
    let mesh = |nth: usize| {
        penumbra_ops::mapping::Mesh::parse(param(nth, "warp").as_str().unwrap()).unwrap().unwrap()
    };
    let corner = param(1, "corner_tl");
    // Dragged half way down the left edge.
    assert!(
        corner[0].as_f64().unwrap().abs() < 0.02 && (corner[1].as_f64().unwrap() - 0.5).abs() < 0.05,
        "{corner}"
    );
    let (before, after) = (mesh(0), mesh(1));
    assert_eq!((after.cols(), after.rows()), (3, 3));
    let [dx, dy] = before.get(1, 1);
    assert!(dx > 0.2 && dy < -0.2, "{dx} {dy}");
    // The nudge is one pixel of the mapper's 480 wide picture.
    assert!((after.get(1, 1)[0] - dx - 1.0 / 480.0).abs() < 2e-5, "{:?}", after.get(1, 1));
    assert_eq!(reply(&replies, "node.info", 1)["status"]["error"], Json::Null);
    let width = |nth: usize| {
        let e = &reply(&replies, "ui.elements", nth)["elements"];
        let x = |i: usize| {
            e.as_array().unwrap().iter().find(|el| el["id"] == format!("viewer.corner.{i}")).unwrap()["rect"]
                [0]
            .as_f64()
            .unwrap()
        };
        x(1) - x(3)
    };
    let (before, after) = (width(0), width(1));
    assert!(after < before * 0.9, "zoom out: {before} -> {after}");
    assert_eq!(reply(&replies, "ui.elements", 2)["elements"], json!([]), "window open");
    engine.shutdown();
}

#[test]
fn draw_move_repeat_and_delete_shapes_in_the_viewer() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-shapes-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(
        r#"(
            version: 1,
            nodes: {
                "/s": (kind: "tex.shapes", params: {"width": 320, "height": 180, "shapes": "window rect 0.1,0.1 0.1,0.1
# keep me
"}),
            },
        )"#,
        &types,
    )
    .unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"method": "ui.select", "params": {"paths": ["/s"]}}),
        json!({"wait": 1.0}),
        // Select the window and drag it by its middle onto its own bottom right corner.
        json!({"method": "ui.click", "params": {"id": "viewer.shape.0"}}),
        json!({"wait": 0.3}),
        json!({"method": "ui.drag", "params": {"from": {"id": "viewer.shape.0"}, "to": {"id": "viewer.handle.2"}, "steps": 4}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/s"}}),
        // The rect tool: a box from the window's top left to its bottom right corner.
        json!({"method": "ui.click", "params": {"id": "viewer.tool.rect"}}),
        json!({"method": "ui.drag", "params": {"from": {"id": "viewer.handle.0"}, "to": {"id": "viewer.handle.2"}, "steps": 4}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/s"}}),
        // Three columns of it, then the spacing diamond dragged onto the original's top right.
        json!({"method": "ui.click", "params": {"id": "viewer.repeat.cols"}}),
        json!({"method": "ui.key", "params": {"key": "Backspace"}}),
        json!({"method": "ui.type", "params": {"text": "3"}}),
        json!({"method": "ui.key", "params": {"key": "Enter"}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/s"}}),
        json!({"method": "ui.drag", "params": {"from": {"id": "viewer.spacing"}, "to": {"id": "viewer.shape.0"}, "steps": 4}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/s"}}),
        json!({"method": "ui.screenshot", "params": {"file": dir.0.join("shapes.png").to_string_lossy()}}),
        // Delete the first window with the key, the pointer over the picture.
        json!({"method": "ui.click", "params": {"id": "viewer.shape.0"}}),
        json!({"method": "ui.key", "params": {"key": "Delete"}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/s"}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(
        engine.clone(),
        &script.join(
            "
",
        ),
        &options,
    )
    .unwrap();
    let text = |nth: usize| {
        let info = reply(&replies, "node.info", nth);
        let p = info["params"].as_array().unwrap().iter().find(|p| p["name"] == "shapes").unwrap();
        p["value"].as_str().unwrap().to_string()
    };
    let shapes = |nth: usize| penumbra_ops::shapes::parse(&text(nth)).unwrap();
    let bounds = |nth: usize, i: usize| shapes(nth)[i].geom.bounds();
    let near = |a: [f64; 4], b: [f64; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
    assert!(near(bounds(0, 0), [0.15, 0.15, 0.1, 0.1]), "moved by half its size: {:?}", bounds(0, 0));
    assert!(text(0).contains("# keep me"), "{}", text(0));
    assert_eq!(shapes(1).len(), 2);
    assert!(near(bounds(1, 1), bounds(1, 0)), "drawn over the first: {:?}", bounds(1, 1));
    assert_eq!(shapes(1)[1].tag, "window", "a new shape takes the tag of the last selected");
    let repeat = |nth: usize| shapes(nth)[1].repeat.unwrap();
    assert_eq!((repeat(2).cols, repeat(2).rows), (3, 1));
    // The last copy's centre now lies on the first's centre: no spacing left.
    assert!(repeat(3).step[0].abs() < 0.01, "{:?}", repeat(3));
    assert_eq!(shapes(4).len(), 1);
    assert!(text(4).contains("# keep me"));
    engine.shutdown();
}

#[test]
fn any_node_opens_in_the_viewer_window() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-window-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"method": "ui.select", "params": {"paths": ["/noise1"]}}),
        json!({"wait": 1.0}),
        json!({"method": "ui.click", "params": {"id": "viewer.window"}}),
        json!({"wait": 0.5}),
        // The window stays on the noise while the selection moves on.
        json!({"method": "ui.select", "params": {"paths": ["/c"]}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.elements", "params": {"prefix": "viewer.window"}}),
        json!({"method": "cook.status", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1400.0, 900.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    assert_eq!(reply(&replies, "ui.elements", 0)["elements"], json!([]), "the button hides while it is open");
    engine.shutdown();
}

#[test]
fn backward_wires_go_around_nodes() {
    use egui::{Rect, pos2};

    use crate::editor::{route_cost, wire_route};
    // A mapper below a projector, its output wired back up into the projector's input: the
    // case that used to swing below and cut through the mapper.
    let projector = Rect::from_min_max(pos2(170.0, 100.0), pos2(465.0, 195.0));
    let mapper = Rect::from_min_max(pos2(150.0, 290.0), pos2(452.0, 500.0));
    let nodes = [projector, mapper];
    let route = wire_route(pos2(452.0, 415.0), pos2(170.0, 167.0), mapper, projector, &nodes, 1.0);
    let clear = route_cost(&route, &[], 1.0);
    assert!(clear > 0.0);
    assert_eq!(route_cost(&route, &nodes, 1.0), clear, "the wire crosses no node");
    // Side by side there is no gap to snake through; it goes around.
    let right = mapper.translate(egui::vec2(400.0, 0.0));
    let left = Rect::from_min_max(pos2(150.0, 290.0), pos2(452.0, 400.0));
    let nodes = [left, right];
    let route = wire_route(pos2(right.max.x, 395.0), pos2(150.0, 330.0), right, left, &nodes, 1.0);
    assert_eq!(route_cost(&route, &nodes, 1.0), route_cost(&route, &[], 1.0));
}

#[test]
fn optimize_resolutions_from_the_helpers_window() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-res-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let project = r#"(
        version: 1,
        nodes: {
            "/noise1": (kind: "tex.noise", params: {"width": 160, "height": 90}),
            "/m": (kind: "tex.mapper", params: {"width": 320, "height": 180}, inputs: {"in": "/noise1"}),
            "/p": (kind: "out.projector", inputs: {"in": "/m"}),
        },
    )"#;
    let content = penumbra_project::parse(project, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.click", "params": {"x": 30, "y": 820, "button": "right"}}),
        json!({"method": "ui.click", "params": {"id": "options.helpers"}}),
        json!({"wait": 0.5}),
        json!({"method": "ui.elements", "params": {"prefix": "helpers.res."}}),
        json!({"method": "ui.click", "params": {"id": "helpers.overlay"}}),
        json!({"wait": 0.3}),
        json!({"method": "ui.click", "params": {"id": "helpers.optimize"}}),
        json!({"wait": 0.5}),
        json!({"method": "node.info", "params": {"path": "/noise1"}}),
        json!({"method": "node.info", "params": {"path": "/m"}}),
        json!({"method": "ui.click", "params": {"id": "helpers.overlay"}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1400.0, 900.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    let listed: Vec<String> = reply(&replies, "ui.elements", 0)["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, ["helpers.res./m", "helpers.res./noise1"], "from the projector upstream");
    let width = |nth: usize| {
        let info = reply(&replies, "node.info", nth);
        info["params"].as_array().unwrap().iter().find(|p| p["name"] == "width").unwrap()["value"].clone()
    };
    // The projector's display is unknown headless: 1920 x 1080 assumed, so both grow to it.
    assert_eq!((width(0), width(1)), (json!(1920), json!(1920)));
    engine.shutdown();
}

#[test]
fn group_enter_and_dissolve_in_the_editor() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-comp-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    engine.set_library(Some(dir.0.join("library")));
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        json!({"method": "ui.click", "params": {"id": "canvas"}}),
        json!({"method": "ui.select", "params": {"paths": ["/noise1"]}}),
        json!({"method": "ui.key", "params": {"key": "G", "ctrl": true}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        // In with I, out with U.
        json!({"method": "ui.key", "params": {"key": "I"}}),
        json!({"method": "ui.state", "params": {}}),
        json!({"method": "ui.elements", "params": {"prefix": "node."}}),
        json!({"method": "ui.key", "params": {"key": "U"}}),
        json!({"method": "ui.state", "params": {}}),
        json!({"method": "ui.elements", "params": {"prefix": "port."}}),
        // The component's output feeds the level: the wire runs from its port.
        json!({"method": "node.info", "params": {"path": "/level1"}}),
        json!({"method": "ui.key", "params": {"key": "Z", "ctrl": true}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(400).collect::<String>());
    }
    let state = |n: usize| reply(&replies, "ui.state", n).clone();
    let before = state(0)["positions"]["/noise1"].clone();
    let grouped = state(1);
    assert_eq!(grouped["selection"], json!(["/group1"]), "{grouped}");
    assert_eq!(grouped["positions"]["/group1/noise1"], before, "the node kept its place inside");
    assert_eq!(state(2)["level"], "/group1");
    let inside = reply(&replies, "ui.elements", 0).to_string();
    assert!(inside.contains("node./group1/noise1") && !inside.contains("node./c\""), "{inside}");
    assert_eq!(state(3)["level"], "/");
    assert_eq!(state(3)["selection"], json!(["/group1"]), "coming out selects the component");
    let ports = reply(&replies, "ui.elements", 1).to_string();
    assert!(ports.contains("port.out./group1.0"), "{ports}");
    let level = reply(&replies, "node.info", 0);
    assert_eq!(level["inputs"][0]["from"], "/group1/noise1_out", "{level}");
    assert_eq!(state(4)["positions"]["/noise1"], before, "undo brings the node back in place");
    engine.shutdown();
}

#[test]
fn copy_and_paste_with_the_keyboard() {
    let dir = TempDir(std::env::temp_dir().join(format!("penumbra-ui-paste-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let content = penumbra_project::parse(PROJECT, &types).unwrap();
    let session = Arc::new(Session::open(&dir.0, types, &content).unwrap());
    let engine = Engine::start(session).unwrap();
    let script = [
        json!({"wait": 1.0}),
        json!({"method": "ui.select", "params": {"paths": ["/noise1", "/level1"]}}),
        // Keys go to the pane under the pointer.
        json!({"method": "ui.scroll", "params": {"x": 200, "y": 500, "dy": 0}}),
        json!({"method": "ui.key", "params": {"key": "C", "ctrl": true}}),
        json!({"method": "ui.scroll", "params": {"x": 300, "y": 600, "dy": 0}}),
        json!({"method": "ui.key", "params": {"key": "V", "ctrl": true}}),
        json!({"method": "node.info", "params": {"path": "/level2"}}),
        json!({"method": "ui.state", "params": {"positions": true}}),
        // Cut takes them away again, one undo step brings them back.
        json!({"method": "ui.key", "params": {"key": "X", "ctrl": true}}),
        json!({"method": "node.list", "params": {}}),
        json!({"method": "ui.key", "params": {"key": "Z", "ctrl": true}}),
        json!({"method": "node.list", "params": {}}),
    ];
    let script: Vec<String> = script.iter().map(Json::to_string).collect();
    let options = headless::Options { size: [1200.0, 800.0], pixels_per_point: 1.0, software: true };
    let replies = headless::run(engine.clone(), &script.join("\n"), &options).unwrap();
    for (m, r) in &replies {
        eprintln!("{m}: {}", r.to_string().chars().take(300).collect::<String>());
    }
    // noise1 becomes noise2, level1 level2, wired to the copy.
    let info = reply(&replies, "node.info", 0);
    assert_eq!(info["inputs"][0]["from"], "/noise2", "{info}");
    let state = reply(&replies, "ui.state", 0);
    let pos = |p: &str| {
        let v = &state["positions"][p];
        (v[0].as_f64().unwrap(), v[1].as_f64().unwrap())
    };
    let (o, c) = ((pos("/noise1"), pos("/level1")), (pos("/noise2"), pos("/level2")));
    // The copies keep the originals' arrangement, moved.
    assert!(((c.1.0 - c.0.0) - (o.1.0 - o.0.0)).abs() < 0.01, "{state}");
    assert!(((c.1.1 - c.0.1) - (o.1.1 - o.0.1)).abs() < 0.01, "{state}");
    assert_ne!(c.0, o.0, "{state}");
    assert_eq!(state["selection"], json!(["/level2", "/noise2"]), "{state}");
    let listed = |n: usize| reply(&replies, "node.list", n).to_string();
    assert!(!listed(0).contains("/level2"), "cut: {}", listed(0));
    assert!(listed(1).contains("/level2"), "undone: {}", listed(1));
    engine.shutdown();
}
