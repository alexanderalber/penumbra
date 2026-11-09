//! `chan.dropdown` end to end: `chan.pick` and `node.set` pick an entry by name or by place,
//! a choice bound to the dropdown follows it by name whatever the order of its own entries, a
//! name the choice lacks keeps it at its last entry with a warning, and a pick followed by a
//! reset in the same frame restarts a feedback loop from the new entry.

use serde_json::json;

use super::{Fixture, eventually, warp};

fn channel(f: &Fixture, path: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}))["channels"][0].clone()
}

/// The value bound param `param` of `/path` took at its last cook.
fn bound(f: &Fixture, path: &str, param: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}));
    f.call("node.info", json!({"path": path}))["status"]["bound"][param].clone()
}

fn warning(f: &Fixture, path: &str) -> String {
    f.call("node.info", json!({"path": path}))["status"]["warning"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_dropdown_picks_by_name_and_by_place() {
    let f = Fixture::new("dropdown_pick");
    let entries = "coral\nmaze\n\n worms \n";
    f.call(
        "node.create",
        json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": entries, "name": "preset"}}),
    );
    assert_eq!(channel(&f, "/d"), json!({"name": "preset", "value": 0.0}), "empty value: the first");
    let info = f.call("node.info", json!({"path": "/d"}));
    assert_eq!(info["dropdown"], json!({"entries": ["coral", "maze", "worms"], "value": "", "place": 0}));

    let r = f.call("chan.pick", json!({"path": "/d", "entry": "MAZE"}));
    assert_eq!((r["value"].clone(), r["place"].clone()), (json!("maze"), json!(1)), "{r}");
    assert_eq!(r["history"], "new", "{r}");
    assert_eq!(channel(&f, "/d")["value"], 1.0);
    let r = f.call("chan.pick", json!({"path": "/d", "entry": 2}));
    assert_eq!(r["value"], "worms", "{r}");
    assert_eq!(channel(&f, "/d")["value"], 2.0);
    eventually("autosave", || f.file().contains(r#""value": "worms""#));
    f.call("history.undo", json!({}));
    assert_eq!(channel(&f, "/d")["value"], 1.0, "a pick is undone like any edit");

    // What chan.pick refuses.
    assert!(f.fail("chan.pick", json!({"path": "/d", "entry": "holes"})).contains("no entry \"holes\""));
    assert!(f.fail("chan.pick", json!({"path": "/d", "entry": 3})).contains("entries 0 to 2"));
    f.fail("chan.pick", json!({"path": "/d", "entry": -1}));
    f.fail("chan.pick", json!({"path": "/d", "entry": ""}));
    f.fail("chan.pick", json!({"path": "/d", "entry": true}));
    f.fail("chan.pick", json!({"path": "/d"}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant"}));
    assert!(f.fail("chan.pick", json!({"path": "/k", "entry": 0})).contains("not a chan.dropdown"));

    // node.set picks by name. A name the list lacks keeps the entry given last, with a warning.
    f.call("node.set", json!({"path": "/d", "params": {"value": "coral"}}));
    assert_eq!(channel(&f, "/d")["value"], 0.0);
    f.call("node.set", json!({"path": "/d", "params": {"value": "holes"}}));
    assert_eq!(channel(&f, "/d")["value"], 0.0);
    assert!(warning(&f, "/d").contains("\"holes\" is none of the entries (coral, maze, worms)"));
    assert_eq!(f.call("node.info", json!({"path": "/d"}))["dropdown"]["place"], json!(null));
    // The list edited: the entry follows its name to its new place.
    f.call("node.set", json!({"path": "/d", "params": {"value": "maze", "entries": "worms\nmaze"}}));
    assert_eq!(channel(&f, "/d")["value"], 1.0);
    f.call("node.set", json!({"path": "/d", "params": {"entries": ""}}));
    f.registry.dispatch("chan.read", &json!({"path": "/d"}));
    let status = f.call("node.info", json!({"path": "/d"}))["status"].clone();
    assert!(status["error"].as_str().unwrap_or_default().contains("`entries` is empty"), "{status}");
}

#[test]
fn a_choice_bound_to_a_dropdown_follows_it_by_name() {
    let f = Fixture::new("dropdown_bind");
    // Its own order, unlike chan.math's (add, subtract, multiply, ...).
    let entries = "multiply\nadd\nscreen";
    f.call("node.create", json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": entries}}));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.create", json!({"path": "/p", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m"}));
    f.call("node.connect", json!({"from": "/c", "to": "/p"}));
    f.call("node.bind", json!({"param": "/m.op", "from": "/d"}));
    // By place: the channel named.
    f.call("node.bind", json!({"param": "/p.op", "from": "/d", "channel": "value"}));
    // A text takes the name, a number the place.
    f.call("node.bind", json!({"param": "/c.name0", "from": "/d"}));
    f.call("node.bind", json!({"param": "/c.value0", "from": "/d"}));
    assert_eq!(bound(&f, "/m", "op"), "multiply", "by name");
    assert_eq!(bound(&f, "/p", "op"), "add", "place 0 is chan.math's first entry");
    assert_eq!(channel(&f, "/c"), json!({"name": "multiply", "value": 0.0}));

    f.call("chan.pick", json!({"path": "/d", "entry": "add"}));
    assert_eq!(bound(&f, "/m", "op"), "add");
    assert_eq!(bound(&f, "/p", "op"), "subtract", "place 1 is chan.math's second entry");
    assert_eq!(channel(&f, "/c"), json!({"name": "add", "value": 1.0}));

    // An entry the choice does not have: it keeps its last entry and says so on the node.
    f.call("chan.pick", json!({"path": "/d", "entry": "screen"}));
    assert_eq!(bound(&f, "/m", "op"), "add");
    let w = warning(&f, "/m");
    assert!(w.contains("/d gives \"screen\", which is none of add, subtract"), "{w}");
    assert_eq!(channel(&f, "/c")["name"], "screen");

    // The file keeps the short form: nothing is named.
    eventually("autosave", || f.file().contains(r#"binds: {"op": "d"}"#));
    let info = f.call("node.info", json!({"path": "/m"}));
    assert_eq!(info["binds"], json!([{"param": "op", "from": "/d"}]));
}

#[test]
fn a_dropdown_on_the_show_screen_changes_without_history() {
    let f = Fixture::new("dropdown_show");
    let params = json!({"entries": "a\nb", "show": true, "label": "look"});
    f.call("node.create", json!({"path": "/d", "kind": "chan.dropdown", "params": params}));
    f.call("show.lock", json!({}));
    let r = f.call("chan.pick", json!({"path": "/d", "entry": "b"}));
    assert_eq!(r["history"], "none_in_show_mode", "{r}");
    let r = f.call("node.set", json!({"path": "/d", "params": {"value": "a"}}));
    assert_eq!(r["history"], "none_in_show_mode", "{r}");
    assert!(
        f.fail("node.set", json!({"path": "/d", "params": {"entries": "x"}})).contains("show mode is on")
    );
    f.call("chan.pick", json!({"path": "/d", "entry": 1}));
    let controls = f.call("show.status", json!({}))["controls"].clone();
    assert_eq!(
        controls,
        json!([{"path": "/d", "kind": "chan.dropdown", "label": "look", "value": 1.0,
                "entries": ["a", "b"], "entry": "b", "bound": false}]),
    );
    f.call("show.unlock", json!({}));
    eventually("autosave after unlock", || f.file().contains(r#""value": "b""#));
}

/// The mean red of node `path`'s picture now.
fn mean(f: &Fixture, path: &str, file: &std::path::Path) -> f64 {
    let s = f.call("node.snapshot", json!({"path": path, "file": file}));
    s["stats"]["mean"][0].as_f64().unwrap()
}

#[test]
fn a_pick_and_a_reset_in_one_frame_restart_from_the_new_entry() {
    let Some(gpu) = warp() else { return };
    let f = Fixture::new("dropdown_reset");
    f.engine.attach_gpu(gpu).unwrap();
    let shots = f._dir.join("shots");
    let shot = shots.join("probe.png");
    let size = json!({"width": 64, "height": 64, "size": [0.8, 0.8]});
    f.call(
        "node.create",
        json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": "rectangle\nellipse"}}),
    );
    f.call("node.create", json!({"path": "/shape", "kind": "tex.primitive", "params": size}));
    f.call("node.create", json!({"path": "/ellipse", "kind": "tex.primitive", "params": size}));
    f.call("node.set", json!({"path": "/ellipse", "params": {"shape": "ellipse"}}));
    f.call("node.bind", json!({"param": "/shape.shape", "from": "/d"}));
    f.call("node.create", json!({"path": "/fb", "kind": "tex.feedback"}));
    f.call("node.create", json!({"path": "/hold", "kind": "tex.null"}));
    f.call("node.connect", json!({"from": "/shape", "to": "/fb", "input": "initial"}));
    f.call("node.connect", json!({"from": "/fb", "to": "/hold"}));
    f.call("node.connect", json!({"from": "/hold", "to": "/fb", "input": "previous"}));
    f.call("node.watch", json!({"path": "/hold"}));
    let ellipse = mean(&f, "/ellipse", &shot);
    eventually("the loop to run on the rectangle", || (mean(&f, "/fb", &shot) - 0.64).abs() < 0.02);
    assert!((ellipse - 0.64).abs() > 0.1, "the two shapes differ: {ellipse}");
    f.call("cook.play", json!({"playing": false}));

    // Picked and reset before the next frame: the loop starts from the ellipse.
    f.call("chan.pick", json!({"path": "/d", "entry": "ellipse"}));
    f.call("node.reset", json!({"path": "/fb"}));
    let after = mean(&f, "/fb", &shot);
    assert!((after - ellipse).abs() < 0.01, "restarted from {after}, the ellipse is {ellipse}");

    // A table cell bound the same way does the same.
    f.call("node.create", json!({"path": "/t", "kind": "table.text", "params": {"text": "ellipse"}}));
    f.call("node.bind", json!({"param": "/shape.shape", "from": "/t", "cell": {}}));
    f.call("node.set", json!({"path": "/t", "params": {"text": "rectangle"}}));
    f.call("node.reset", json!({"path": "/fb"}));
    let after = mean(&f, "/fb", &shot);
    assert!((after - 0.64).abs() < 0.02, "restarted from {after}, the rectangle is 0.64");
}
