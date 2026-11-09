//! Presets of a component by command: `node.set` of a choice writes its entry's values in the
//! same undo step, other names in the same call land on top, a reset goes to the chosen
//! entry's value, and `node.info` tells what differs from it.

use serde_json::{Value as Json, json};

use super::Fixture;

const TEXT: &str = "@param preset choice(coral|maze) = coral\n\
                    @param feed float = 0.05\n\
                    @param kill float = 0.06\n\
                    @param speed float = 1\n\
                    @expose level = k.value0\n\
                    @preset preset=* feed=0.0545 kill=0.062 level=2\n\
                    @preset preset=maze feed=0.029 kill=0.057 level=3 -- labyrinth";

/// The value of `name` on `/sim`, or of the inner `/sim/k`'s `value0` for `level`.
fn value(f: &Fixture, name: &str) -> f64 {
    let (path, name) = if name == "level" { ("/sim/k", "value0") } else { ("/sim", name) };
    let info = f.call("node.info", json!({"path": path}));
    let rows = if path == "/sim" { &info["instance_params"] } else { &info["params"] };
    rows.as_array()
        .and_then(|r| r.iter().find(|p| p["name"] == name))
        .and_then(|p| p["value"].as_f64())
        .unwrap_or(f64::NAN)
}

fn values(f: &Fixture) -> [f64; 3] {
    [value(f, "feed"), value(f, "kill"), value(f, "level")]
}

fn presets(f: &Fixture) -> Json {
    f.call("node.info", json!({"path": "/sim"}))["component"]["presets"][0].clone()
}

#[test]
fn a_preset_by_command_writes_its_values_in_one_undo_step() {
    let f = Fixture::new("presets");
    f.call("node.create", json!({"path": "/sim", "kind": "comp.base", "params": {"params": TEXT}}));
    f.call("node.create", json!({"path": "/sim/k", "kind": "chan.constant"}));
    let start = values(&f);

    // The other names land on top of the preset, whatever their order in the call.
    let r = f.call("node.set", json!({"path": "/sim", "params": {"feed": 0.03, "preset": "maze"}}));
    assert!(r.get("left_out").is_none(), "{r}");
    assert_eq!(values(&f), [0.03, 0.057, 3.0]);
    let p = presets(&f);
    assert_eq!(p["chosen"], "maze", "{p}");
    assert_eq!(p["owns"], json!(["feed", "kill", "level"]));
    assert_eq!(p["changed"], json!(["feed"]));
    assert_eq!(p["entries"][1], json!({"name": "maze", "doc": "labyrinth"}));

    // One undo step brings every value back.
    f.call("history.undo", json!({}));
    assert_eq!(values(&f), start);
    f.call("history.redo", json!({}));
    assert_eq!(values(&f), [0.03, 0.057, 3.0]);

    // A reset goes to maze's value; choosing maze again brings all of maze back.
    f.call("node.set", json!({"path": "/sim", "params": {"feed": null, "speed": 2.0}}));
    assert_eq!(value(&f, "feed"), 0.029);
    f.call("node.set", json!({"path": "/sim", "params": {"kill": 0.01}}));
    f.call("node.set", json!({"path": "/sim/k", "params": {"value0": 9.0}}));
    assert_eq!(presets(&f)["changed"], json!(["kill", "level"]));
    f.call("node.set", json!({"path": "/sim", "params": {"preset": "maze"}}));
    assert_eq!(values(&f), [0.029, 0.057, 3.0]);
    assert_eq!(value(&f, "speed"), 2.0, "a param on no `*` line stays");
    assert_eq!(presets(&f)["changed"], json!([]));

    // A value that does not go in is skipped and named in the reply.
    f.call("node.delete", json!({"path": "/sim/k"}));
    let r = f.call("node.set", json!({"path": "/sim", "params": {"preset": "coral"}}));
    let left = r["left_out"].to_string();
    assert!(left.contains("/sim.level") && left.contains("preset=coral"), "{r}");
    assert_eq!(value(&f, "feed"), 0.0545);
    let problems = presets(&f)["problems"].to_string();
    assert!(problems.contains("`level`"), "{problems}");
}

/// The seconds of `/sim/clock` after one more frame, in frames.
fn frame(f: &Fixture) -> i64 {
    f.call("cook.step", json!({}));
    let read = f.call("chan.read", json!({"path": "/sim/clock"}));
    let seconds = read["channels"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == "seconds"))
        .and_then(|c| c["value"].as_f64())
        .unwrap_or(-1.0);
    (seconds * 60.0).round() as i64
}

fn entries(f: &Fixture) -> usize {
    f.call("history.list", json!({"limit": 1000}))["entries"].as_array().map_or(0, Vec::len)
}

#[test]
fn a_bound_preset_writes_its_values_one_frame_later_without_history() {
    let f = Fixture::new("presets_bound");
    f.call("cook.stepping", json!({"on": true}));
    let text = "@param preset choice(coral|maze) = coral\n\
                @param feed float = 0.05\n\
                @param kill float = 0.06\n\
                @param morph bool\n\
                @reset preset unless morph\n\
                @reset feed\n\
                @preset preset=* feed=0.0545 kill=0.062\n\
                @preset preset=maze feed=0.029 kill=0.057";
    f.call("node.create", json!({"path": "/sim", "kind": "comp.base", "params": {"params": text}}));
    f.call("node.create", json!({"path": "/sim/clock", "kind": "chan.clock", "params": {"mode": "free"}}));
    f.call(
        "node.create",
        json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": "coral\nmaze"}}),
    );
    f.call("node.watch", json!({"path": "/sim/clock"}));
    f.call("node.bind", json!({"param": "/sim.preset", "from": "/d"}));
    let first = frame(&f);
    for k in 1..=3 {
        assert_eq!(frame(&f), first + k, "binding the same entry as stored writes nothing");
    }
    assert_eq!(value(&f, "feed"), 0.05, "nothing written yet");

    // The pick: the frame of the pick still runs coral with its values (the cooker test shows
    // that pass); the values are written after it, and the next frame starts over with maze,
    // as `@reset preset` says. (`chan.read` cooks a pass of its own, so it is read only after.)
    f.call("chan.pick", json!({"path": "/d", "entry": "maze"}));
    let before = entries(&f);
    f.call("cook.step", json!({}));
    assert_eq!([value(&f, "feed"), value(&f, "kill")], [0.029, 0.057], "written after that frame");
    assert_eq!(presets(&f)["chosen"], "maze");
    assert_eq!(frame(&f), first, "the next frame starts over");
    assert_eq!(frame(&f), first + 1);
    assert_eq!(entries(&f), before, "the write is no history entry");

    // With morph on, a bound switch writes the values and does not start over, although
    // `feed` restarts when changed by hand.
    f.call("node.set", json!({"path": "/sim", "params": {"morph": true}}));
    f.call("chan.pick", json!({"path": "/d", "entry": "coral"}));
    f.call("cook.step", json!({}));
    assert_eq!(value(&f, "feed"), 0.0545);
    assert_eq!(frame(&f), first + 3, "morph: no restart");
    f.call("node.set", json!({"path": "/sim", "params": {"feed": 0.04}}));
    assert_eq!(frame(&f), first, "feed by hand restarts");

    // A number picks the entry by place, as for any bound choice.
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 0.0}}));
    f.call("node.bind", json!({"param": "/sim.preset", "from": "/k"}));
    frame(&f);
    assert_eq!(value(&f, "feed"), 0.04, "place 0 is coral, as stored: nothing written");
    f.call("node.set", json!({"path": "/k", "params": {"value0": 1.0}}));
    frame(&f);
    assert_eq!(value(&f, "feed"), 0.029);
    assert_eq!(presets(&f)["chosen"], "maze");

    // A preset that writes a bound param writes its stored value; node.info names it.
    f.call("node.bind", json!({"param": "/sim.kill", "from": "/k"}));
    assert_eq!(presets(&f)["bound"], json!(["kill"]));

    // Bound anew by an edit, the binding's first entry is written: the dropdown says coral.
    f.call("node.unbind", json!({"param": "/sim.kill"}));
    f.call("node.bind", json!({"param": "/sim.preset", "from": "/d"}));
    frame(&f);
    assert_eq!(value(&f, "feed"), 0.0545);
    assert_eq!(presets(&f)["chosen"], "coral");

    // In show mode the binding still writes: a dropdown on the show screen.
    f.call("node.set", json!({"path": "/d", "params": {"show": true}}));
    f.call("show.lock", json!({}));
    f.call("chan.pick", json!({"path": "/d", "entry": "maze"}));
    frame(&f);
    assert_eq!(value(&f, "feed"), 0.029, "written while locked");
    f.call("show.unlock", json!({}));
}
