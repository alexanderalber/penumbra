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
