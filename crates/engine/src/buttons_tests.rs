//! `chan.button` and `chan.slider` end to end: `chan.press` holds and taps a momentary button
//! without history, flips a toggle as an edit; a slider's value is set by `node.set`, kept in
//! its range and saved; both drive bindings like any chan node.

use serde_json::json;

use super::{Fixture, eventually};

fn value(f: &Fixture, path: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}))["channels"][0]["value"].clone()
}

fn entries(f: &Fixture) -> usize {
    f.call("history.list", json!({"limit": 1000}))["entries"].as_array().map_or(0, Vec::len)
}

#[test]
fn a_momentary_button_is_held_by_command_without_history() {
    let f = Fixture::new("button_momentary");
    f.call("node.create", json!({"path": "/b", "kind": "chan.button"}));
    let channels = f.call("chan.read", json!({"path": "/b"}))["channels"].clone();
    assert_eq!(channels, json!([{"name": "value", "value": 0.0}]), "{channels}");
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/c.value0", "from": "/b"}));
    f.call("node.watch", json!({"path": "/c"}));
    let before = entries(&f);
    assert!(before >= 3, "the creates and the bind are entries");

    let r = f.call("chan.press", json!({"path": "/b", "down": true}));
    assert_eq!(r["held"], true, "{r}");
    eventually("the button to be 1", || value(&f, "/b") == 1.0);
    eventually("the bound constant to follow", || value(&f, "/c") == 1.0);
    let info = f.call("node.info", json!({"path": "/b"}));
    assert_eq!(info["button"], json!({"mode": "momentary", "held": true, "presses": 1}), "{info}");
    f.call("chan.press", json!({"path": "/b", "down": false}));
    eventually("the button to be 0", || value(&f, "/b") == 0.0);
    eventually("the bound constant to follow", || value(&f, "/c") == 0.0);

    // A tap counts as a press; the button is up afterwards.
    let r = f.call("chan.press", json!({"path": "/b"}));
    assert_eq!(r["presses"], 2, "{r}");
    assert_eq!(r["held"], false, "{r}");
    assert_eq!(entries(&f), before, "pressing made no history entry");

    assert!(f.fail("chan.press", json!({"path": "/c"})).contains("not a chan.button"));
    assert!(f.fail("chan.press", json!({"path": "/nope"})).contains("no node"));
    f.fail("chan.press", json!({"path": "/b", "down": 1}));
    f.fail("chan.press", json!({"path": "/b", "extra": true}));
    f.fail("chan.press", json!({}));

    // Deleted while held: the hub forgets it.
    f.call("chan.press", json!({"path": "/b", "down": true}));
    let id = f.engine.session().doc().snapshot().resolve(&"/b".parse().unwrap()).unwrap();
    f.call("node.delete", json!({"path": "/b"}));
    eventually("the hub to forget the button", || !f.engine.buttons().view(id).held);
}

#[test]
fn a_toggle_flips_its_saved_value() {
    let f = Fixture::new("button_toggle");
    f.call("node.create", json!({"path": "/b", "kind": "chan.button", "params": {"mode": "toggle"}}));
    assert_eq!(value(&f, "/b"), 0.0);
    let r = f.call("chan.press", json!({"path": "/b"}));
    assert_eq!((r["value"].clone(), r["history"].clone()), (json!(true), json!("new")), "{r}");
    assert_eq!(value(&f, "/b"), 1.0);
    let r = f.call("chan.press", json!({"path": "/b", "down": false}));
    assert_eq!(r["value"], true, "a release leaves a toggle as it is: {r}");
    eventually("autosave", || f.file().contains(r#""value": true"#));
    f.call("chan.press", json!({"path": "/b", "down": true}));
    assert_eq!(value(&f, "/b"), 0.0);
    f.call("history.undo", json!({}));
    assert_eq!(value(&f, "/b"), 1.0, "a press is undone like any edit");

    // Bound: the channel wins, a press is refused.
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/b.value", "from": "/k"}));
    assert!(f.fail("chan.press", json!({"path": "/b"})).contains("bound"));
}

#[test]
fn a_slider_keeps_its_value_in_range_and_saves_it() {
    let f = Fixture::new("slider");
    f.call(
        "node.create",
        json!({"path": "/s", "kind": "chan.slider", "params": {"max": 10, "value": 5, "name": "speed"}}),
    );
    let channels = f.call("chan.read", json!({"path": "/s"}))["channels"].clone();
    assert_eq!(channels, json!([{"name": "speed", "value": 5.0}]), "{channels}");
    f.call("node.set", json!({"path": "/s", "params": {"value": 20}}));
    assert_eq!(value(&f, "/s"), 10.0, "kept at max");
    f.call("node.set", json!({"path": "/s", "params": {"value": 2.5}}));
    eventually("autosave", || f.file().contains(r#""value": 2.5"#));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/c.value0", "from": "/s"}));
    assert_eq!(value(&f, "/c"), 2.5);
}

#[test]
fn in_show_mode_flagged_controls_change_without_history_and_are_saved_after_unlock() {
    let f = Fixture::new("show_controls");
    let show = json!({"show": true});
    f.call(
        "node.create",
        json!({"path": "/t", "kind": "chan.button", "params": {"mode": "toggle", "show": true}}),
    );
    f.call("node.create", json!({"path": "/m", "kind": "chan.button", "params": show}));
    f.call("node.create", json!({"path": "/s", "kind": "chan.slider", "params": show}));
    f.call("node.create", json!({"path": "/plain", "kind": "chan.slider"}));
    f.call("node.create", json!({"path": "/c", "kind": "comp.base"}));
    f.call(
        "node.create",
        json!({"path": "/c/inner", "kind": "chan.slider", "params": {"show": true, "label": "fade"}}),
    );
    // Top to bottom, then left to right; inside the component at the component's place.
    let at = json!({"/t": [0, 0], "/m": [200, 0], "/s": [-100, 100], "/plain": [0, 300], "/c": [0, 200], "/c/inner": [0, 0]});
    f.call("node.place", json!({"nodes": at}));
    eventually("autosave", || f.file().contains("\"/c/inner\""));
    let before = entries(&f);
    let file = f.file();
    f.call("show.lock", json!({}));

    // The flagged controls change; no entry, nothing written.
    let r = f.call("chan.press", json!({"path": "/t"}));
    assert_eq!((r["value"].clone(), r["history"].clone()), (json!(true), json!("none_in_show_mode")), "{r}");
    let r = f.call("node.set", json!({"path": "/s", "params": {"value": 0.7}}));
    assert_eq!(r["history"], "none_in_show_mode", "{r}");
    f.call("node.set", json!({"path": "/c/inner", "params": {"value": 0.25}}));
    let r = f.call("chan.press", json!({"path": "/m"}));
    assert_eq!(r["presses"], 1, "a momentary press works as always: {r}");
    assert_eq!(value(&f, "/t"), 1.0);
    assert_eq!(value(&f, "/s"), 0.7);

    // Anything else stays refused: an unflagged control, another param, a mix.
    for (path, params) in [
        ("/plain", json!({"value": 0.5})),
        ("/s", json!({"min": -1.0})),
        ("/s", json!({"value": 0.1, "max": 2.0})),
        ("/s", json!({"value": null})),
    ] {
        let e = f.fail("node.set", json!({"path": path, "params": params}));
        assert!(e.contains("show mode is on"), "{path} {params}: {e}");
    }
    f.fail("history.undo", json!({}));
    assert_eq!(entries(&f), before, "no history entry");
    let info = f.call("project.info", json!({}));
    assert_eq!((info["locked"].clone(), info["unsaved"].clone()), (json!(true), json!(true)), "{info}");
    std::thread::sleep(std::time::Duration::from_millis(1600));
    assert_eq!(f.file(), file, "not written while locked");

    // show.status lists them, in order of position, with their values.
    let status = f.call("show.status", json!({}));
    let listed: Vec<(String, String, f64)> = status["controls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().into(),
                c["label"].as_str().unwrap().into(),
                c["value"].as_f64().unwrap(),
            )
        })
        .collect();
    let expected = [("/t", "t", 1.0), ("/m", "m", 0.0), ("/s", "s", 0.7), ("/c/inner", "c/fade", 0.25)];
    let expected: Vec<(String, String, f64)> =
        expected.iter().map(|(p, l, v)| (p.to_string(), l.to_string(), *v)).collect();
    assert_eq!(listed, expected, "{status}");

    // After unlock the values count: autosave writes them; undo leaves them alone.
    f.call("show.unlock", json!({}));
    eventually("autosave after unlock", || f.file().contains(r#""value": 0.7"#));
    assert!(f.file().contains(r#""value": true"#), "{}", f.file());
    assert_eq!(entries(&f), before, "still no entry for them");
    f.call("history.undo", json!({}));
    assert_eq!(value(&f, "/s"), 0.7, "undo takes back the last create, not the show's values");
    // Unlocked, a change is an edit again.
    let r = f.call("node.set", json!({"path": "/s", "params": {"value": 0.2}}));
    assert_eq!(r["history"], "new", "{r}");
}

/// A second engine on the project folder of `f`, as the app after a restart.
fn restarted(f: &Fixture) -> std::sync::Arc<crate::Engine> {
    let types = std::sync::Arc::new(penumbra_ops::builtin_types().unwrap());
    let session =
        penumbra_project::Session::open(f._dir.path(), types, &penumbra_project::content::Content::default())
            .unwrap();
    crate::Engine::start(std::sync::Arc::new(session)).unwrap()
}

fn param(e: &crate::Engine, path: &str) -> penumbra_core::Value {
    let net = e.session().doc().snapshot();
    let node = net.node(net.resolve(&path.parse().unwrap()).unwrap()).unwrap();
    let desc = e.types().desc(&node.kind).unwrap();
    node.param(desc, "value").unwrap().clone()
}

#[test]
fn show_screen_values_survive_a_restart_in_show_mode() {
    use penumbra_core::Value;
    let f = Fixture::new("show_values");
    let data = f._dir.join("data");
    let show_file = data.join(crate::show::FILE);
    let kept = data.join(crate::show_values::FILE);
    f.engine.set_show_file(Some(show_file.clone()));
    f.call(
        "node.create",
        json!({"path": "/t", "kind": "chan.button", "params": {"mode": "toggle", "show": true}}),
    );
    f.call("node.create", json!({"path": "/s", "kind": "chan.slider", "params": {"show": true}}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.button", "params": {"show": true}}));
    f.call("node.create", json!({"path": "/plain", "kind": "chan.slider", "params": {"value": 0.3}}));
    eventually("autosave", || f.file().contains("\"/plain\""));
    let file = f.file();
    f.call("show.lock", json!({}));
    f.call("chan.press", json!({"path": "/t"}));
    f.call("node.set", json!({"path": "/s", "params": {"value": 0.7}}));
    eventually("the values kept", || std::fs::read_to_string(&kept).is_ok_and(|t| t.contains("0.7")));
    let text = std::fs::read_to_string(&kept).unwrap();
    assert!(text.contains(r#""/t": true"#) && !text.contains("/m") && !text.contains("/plain"), "{text}");
    // A crash: the app ends without unlocking, project.ron still holds the old values.
    f.engine.shutdown();
    assert_eq!(f.file(), file, "project.ron untouched");

    let g = restarted(&f);
    assert!(g.set_show_file(Some(show_file.clone())));
    assert_eq!(param(&g, "/t"), Value::Bool(true));
    assert_eq!(param(&g, "/s"), Value::Float(0.7));
    assert_eq!(param(&g, "/plain"), Value::Float(0.3));
    assert!(g.session().status(std::time::Instant::now()).unsaved, "live values, not yet in the file");
    g.shutdown();
    assert!(kept.exists(), "still kept while show mode is on");

    // project.ron edited meanwhile: the kept values do not fit it any more.
    std::fs::write(g.session().file(), file.replace("0.3", "0.4")).unwrap();
    let h = restarted(&f);
    assert!(h.set_show_file(Some(show_file.clone())));
    assert_eq!(param(&h, "/s"), Value::Float(0.0), "ignored after an external edit");
    h.shutdown();

    // A broken file is ignored; unlocking writes the project and deletes the file.
    std::fs::write(&kept, "{ values: nope").unwrap();
    let i = restarted(&f);
    assert!(i.set_show_file(Some(show_file.clone())));
    assert_eq!(param(&i, "/t"), Value::Bool(false));
    let registry = crate::engine_registry(&i);
    registry.dispatch("node.set", &json!({"path": "/s", "params": {"value": 0.9}}));
    eventually("kept again", || std::fs::read_to_string(&kept).is_ok_and(|t| t.contains("0.9")));
    assert_eq!(registry.dispatch("show.unlock", &json!({}))["ok"], true);
    let saved = std::fs::read_to_string(i.session().file()).unwrap();
    assert!(saved.contains(r#""value": 0.9"#), "written at unlock: {saved}");
    eventually("deleted after unlock", || !kept.exists());
    i.shutdown();
}
