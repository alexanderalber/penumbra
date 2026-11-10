//! Params that follow a component's own params, end to end: `node.bind` with `parent.x` and
//! `/rd.x`, values reaching the followers in the same cook (by set, by an outside binding, by
//! undo), a choice by name, the refusals, `node.info` on both sides and the file form.

use serde_json::json;

use super::{Fixture, eventually};

/// The value bound param `param` of `/path` took at its last cook.
fn bound(f: &Fixture, path: &str, param: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}));
    f.call("node.info", json!({"path": path}))["status"]["bound"][param].clone()
}

fn warning(f: &Fixture, path: &str) -> String {
    f.call("node.info", json!({"path": path}))["status"]["warning"].as_str().unwrap_or_default().to_string()
}

#[test]
fn inner_params_follow_the_components_params() {
    let f = Fixture::new("follow_parent");
    f.call("node.create", json!({"path": "/rd", "kind": "comp.base"}));
    let text = "@param preset choice(add|multiply|screen) = multiply -- what the inside does\n\
                @param feed float = 0.05 0..0.1\n\
                @expose shown = c.value0";
    f.call("node.set", json!({"path": "/rd", "params": {"params": text}}));
    f.call("node.create", json!({"path": "/rd/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/rd/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/rd/c", "to": "/rd/m"}));
    // chan.math lists add before multiply; the choice follows by name.
    f.call("node.bind", json!({"param": "/rd/m.op", "from": "parent.preset"}));
    f.call("node.bind", json!({"param": "/rd/c.value0", "from": "/rd.feed"}));
    f.call("node.bind", json!({"param": "/rd/c.name0", "from": "parent.preset"}));
    assert_eq!(bound(&f, "/rd/m", "op"), "multiply");
    assert_eq!(bound(&f, "/rd/c", "value0"), 0.05);
    assert_eq!(bound(&f, "/rd/c", "name0"), "multiply", "a choice into a text gives its name");

    // A set reaches the followers in the next cook, and undo takes it back.
    f.call("node.set", json!({"path": "/rd", "params": {"preset": "add", "feed": 0.07}}));
    assert_eq!(bound(&f, "/rd/m", "op"), "add");
    assert_eq!(bound(&f, "/rd/c", "value0"), 0.07);
    f.call("history.undo", json!({}));
    assert_eq!(bound(&f, "/rd/m", "op"), "multiply");
    assert_eq!(bound(&f, "/rd/c", "value0"), 0.05);

    // The component's param bound from outside hands the bound value on.
    let entries = "screen\nmultiply\nadd";
    f.call("node.create", json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": entries}}));
    f.call("node.bind", json!({"param": "/rd.preset", "from": "/d"}));
    f.call("chan.pick", json!({"path": "/d", "entry": "add"}));
    assert_eq!(bound(&f, "/rd/m", "op"), "add");
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 0.02}}));
    f.call("node.bind", json!({"param": "/rd.feed", "from": "/k"}));
    assert_eq!(bound(&f, "/rd/c", "value0"), 0.02);
    f.call("node.set", json!({"path": "/k", "params": {"value0": 0.03}}));
    assert_eq!(bound(&f, "/rd/c", "value0"), 0.03);
    // A name the follower lacks keeps its last entry, with a warning on the follower.
    f.call("chan.pick", json!({"path": "/d", "entry": "screen"}));
    assert_eq!(bound(&f, "/rd/m", "op"), "add");
    let w = warning(&f, "/rd/m");
    assert!(w.contains("`op` follows parent.preset: it follows \"screen\", which is none of add,"), "{w}");
    // The component tells of it too.
    let info = f.call("node.info", json!({"path": "/rd"}));
    let told = &info["component"]["follower_warnings"];
    assert_eq!(told[0]["node"], "/rd/m", "{info}");
    assert!(
        told[0]["warning"].as_str().is_some_and(|t| t.starts_with("`op` follows parent.preset")),
        "{info}"
    );

    // What is refused.
    let e = f.fail("node.bind", json!({"param": "/rd/m.op", "from": "parent.nope"}));
    assert!(e.contains("/rd has no param `nope`; its params: preset, feed"), "{e}");
    f.call("node.create", json!({"path": "/top", "kind": "chan.constant"}));
    let e = f.fail("node.bind", json!({"param": "/top.value0", "from": "parent.feed"}));
    assert!(e.contains("`parent` is the component a node sits in, and /top sits at the top level"), "{e}");
    let e = f.fail("node.bind", json!({"param": "/rd.feed", "from": "/rd/c"}));
    assert!(e.contains("would make a loop"), "{e}");
    let e = f.fail("node.bind", json!({"param": "/top.value0", "from": "/rd.shown"}));
    assert!(e.contains("an @expose line"), "{e}");
    let e = f.fail("node.bind", json!({"param": "/top.value0", "from": "/rd.preset"}));
    assert!(e.contains("`value0` is a number and cannot follow `preset`, which is a choice"), "{e}");

    // node.info on both sides, and the file.
    let info = f.call("node.info", json!({"path": "/rd/m"}));
    assert_eq!(info["binds"], json!([{"param": "op", "from": "parent.preset", "value": "add"}]), "{info}");
    let info = f.call("node.info", json!({"path": "/rd"}));
    let own = info["instance_params"].as_array().cloned().unwrap_or_default();
    assert_eq!(own[0]["followed_by"], json!(["/rd/c.name0", "/rd/m.op"]), "{info}");
    assert_eq!(own[1]["followed_by"], json!(["/rd/c.value0"]), "{info}");
    eventually("autosave", || {
        f.file()
            .contains(r#""/rd/m": (kind: "chan.math", inputs: {"a": "c"}, binds: {"op": "parent.preset"}"#)
    });
    f.call("node.unbind", json!({"param": "/rd/m.op"}));
    assert!(f.call("node.info", json!({"path": "/rd/m"}))["binds"].as_array().is_some_and(Vec::is_empty));
}

/// The row named `name` in the list `list` of `/path`'s `node.info`.
fn row(f: &Fixture, path: &str, list: &str, name: &str) -> serde_json::Value {
    let info = f.call("node.info", json!({"path": path}));
    let rows = info[list].as_array().cloned().unwrap_or_default();
    rows.into_iter().find(|r| r["name"] == name).unwrap_or_default()
}

#[test]
fn an_inner_param_goes_onto_its_component_by_one_command() {
    let f = Fixture::new("put_param");
    f.call(
        "node.create",
        json!({"path": "/rd", "kind": "comp.base", "params": {"params": "@param feed float = 0.05"}}),
    );
    f.call("node.create", json!({"path": "/rd/m", "kind": "chan.math", "params": {"op": "multiply"}}));
    f.call("node.create", json!({"path": "/rd/n", "kind": "chan.math"}));
    let r = f.call("comp.put_param", json!({"param": "/rd/m.op"}));
    assert_eq!(r["how"], "declared", "{r}");
    assert_eq!(r["message"], "/rd has a new param `op`; this param follows it");
    let text = row(&f, "/rd", "params", "params")["value"].as_str().unwrap_or_default().to_string();
    assert!(text.starts_with("@param feed float = 0.05\n@param op choice("), "{text}");
    assert_eq!(row(&f, "/rd", "instance_params", "op")["value"], "multiply", "the value moved up");
    assert_eq!(bound(&f, "/rd/m", "op"), "multiply");
    // The same name on another node: it follows the param declared now.
    let r = f.call("comp.put_param", json!({"param": "/rd/n.op"}));
    assert_eq!(r["how"], "followed", "{r}");
    assert_eq!(row(&f, "/rd", "instance_params", "op")["followed_by"], json!(["/rd/m.op", "/rd/n.op"]));
    // One undo step each.
    f.call("history.undo", json!({}));
    f.call("history.undo", json!({}));
    assert_eq!(row(&f, "/rd", "params", "params")["value"], "@param feed float = 0.05");
    assert!(f.call("node.info", json!({"path": "/rd/m"}))["binds"].as_array().is_some_and(Vec::is_empty));

    // Shown instead: an @expose line, nothing copied.
    let r = f.call("comp.show_param", json!({"param": "/rd/m.b"}));
    assert_eq!((r["how"].as_str(), r["name"].as_str()), (Some("shown"), Some("b")), "{r}");
    assert_eq!(row(&f, "/rd", "params", "params")["value"], "@param feed float = 0.05\n@expose m.b");
    let e = f.fail("comp.show_param", json!({"param": "/rd/m.b"}));
    assert!(e.contains("/rd shows `b` as `b` already"), "{e}");
    // Refused: the top level, a name of another kind, a param that is bound.
    f.call("node.create", json!({"path": "/top", "kind": "chan.math"}));
    let e = f.fail("comp.put_param", json!({"param": "/top.op"}));
    assert!(e.contains("/top sits at the top level"), "{e}");
    let e = f.fail("comp.put_param", json!({"param": "/rd/m.op", "name": "feed"}));
    assert!(e.contains("/rd has a param `feed` already, and it is a number"), "{e}");
    f.call("node.bind", json!({"param": "/rd/n.op", "from": "/top"}));
    let e = f.fail("comp.put_param", json!({"param": "/rd/n.op"}));
    assert!(e.contains("is bound to /top; unbind it first"), "{e}");
}

#[test]
fn a_table_row_follows_a_components_choice_by_name_in_the_same_cook() {
    let f = Fixture::new("follow_table_key");
    let text = "@param look choice(window|fern|coral|custom) = window -- what grows\n\
                @param speed float = 1 0..4";
    f.call("node.create", json!({"path": "/g", "kind": "comp.base", "params": {"params": text}}));
    // Rows in another order than the choice's entries, and `custom` missing.
    let table = "look,steps,hold\ncoral,16,5\nwindow,12,5\n Fern ,32,20";
    let params = json!({"text": table, "format": "csv"});
    f.call("node.create", json!({"path": "/g/table", "kind": "table.text", "params": params}));
    f.call("node.create", json!({"path": "/g/numbers", "kind": "chan.from_table"}));
    f.call("node.connect", json!({"from": "/g/table", "to": "/g/numbers"}));
    f.call("node.bind", json!({"param": "/g/numbers.key", "from": "parent.look"}));
    f.call("node.create", json!({"path": "/g/steps", "kind": "chan.math", "params": {"op": "multiply"}}));
    f.call("node.connect", json!({"from": "/g/numbers", "to": "/g/steps"}));
    f.call("node.bind", json!({"param": "/g/steps.b", "from": "parent.speed"}));
    f.call("node.create", json!({"path": "/g/hold", "kind": "chan.clock", "params": {"mode": "once"}}));
    f.call("node.bind", json!({"param": "/g/hold.length", "from": "/g/numbers", "channel": "hold"}));
    f.call("node.create", json!({"path": "/g/n", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/g/n.value0", "from": "/g/steps", "channel": "steps"}));

    // Channels named by the header; the column of keys makes none.
    let read = f.call("chan.read", json!({"path": "/g/numbers"}));
    let names: Vec<&str> =
        read["channels"].as_array().unwrap().iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names, ["steps", "hold"], "{read}");
    assert_eq!(bound(&f, "/g/n", "value0"), 12.0);
    assert_eq!(bound(&f, "/g/hold", "length"), 5.0);

    // One set, one cook: the row of the new entry, by name, already scaled.
    f.call("node.set", json!({"path": "/g", "params": {"look": "fern", "speed": 2.0}}));
    assert_eq!(bound(&f, "/g/n", "value0"), 64.0, "the new row in the same cook");
    assert_eq!(bound(&f, "/g/hold", "length"), 20.0, "a key matches case and spaces aside");
    assert_eq!(warning(&f, "/g/numbers"), "");

    // A missing row: the first row and a warning naming the keys there are.
    f.call("node.set", json!({"path": "/g", "params": {"look": "custom", "speed": 1.0}}));
    assert_eq!(bound(&f, "/g/n", "value0"), 16.0, "the first row");
    let w = warning(&f, "/g/numbers");
    assert_eq!(w, "no row has the key `custom`, so the first row applies; its keys: coral, window, Fern");

    // An entry from outside the component: a dropdown bound to the component's choice.
    f.call(
        "node.create",
        json!({"path": "/d", "kind": "chan.dropdown", "params": {"entries": "coral\nwindow"}}),
    );
    f.call("node.bind", json!({"param": "/g.look", "from": "/d"}));
    f.call("chan.pick", json!({"path": "/d", "entry": "window"}));
    assert_eq!(bound(&f, "/g/n", "value0"), 12.0);
    assert_eq!(warning(&f, "/g/numbers"), "");

    // The file keeps the key as an ordinary follower.
    eventually("autosave", || f.file().contains(r#"binds: {"key": "parent.look"}"#));
}

/// `node.info` says what a component's own param does when nothing inside follows it: a choice
/// with presets writes params, a bool decides an `@reset` line, a name on one restarts, and only
/// a param with none of these is unused.
#[test]
fn node_info_says_the_job_of_a_param_nothing_follows() {
    let f = Fixture::new("param_jobs");
    let text = "@param preset choice(a|b)\n\
                @param fresh bool\n\
                @param seed int\n\
                @param spare float\n\
                @param feed float = 0.05\n\
                @preset preset=* feed=0.05\n\
                @preset preset=b feed=0.03\n\
                @reset preset if fresh\n\
                @reset seed";
    f.call("node.create", json!({"path": "/rd", "kind": "comp.base", "params": {"params": text}}));
    f.call("node.create", json!({"path": "/rd/c", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/rd/c.value0", "from": "parent.feed"}));
    let get = |name: &str| row(&f, "/rd", "instance_params", name);
    assert_eq!(get("preset")["writes"], json!(["feed"]));
    assert_eq!(get("fresh")["restart_condition"], true);
    assert_eq!(get("seed")["restarts"], true);
    assert_eq!(get("spare")["unused"], true);
    let feed = get("feed");
    assert_eq!(feed["followed_by"], json!(["/rd/c.value0"]), "{feed}");
    let fields = ["unused", "writes", "restart_condition", "restarts"];
    for (name, own) in
        [("preset", "writes"), ("fresh", "restart_condition"), ("seed", "restarts"), ("feed", "")]
    {
        let r = get(name);
        let has: Vec<&str> = fields.iter().copied().filter(|k| r.get(*k).is_some()).collect();
        let want: Vec<&str> = [own].into_iter().filter(|k| !k.is_empty()).collect();
        assert_eq!(has, want, "{name}: {r}");
    }
}
