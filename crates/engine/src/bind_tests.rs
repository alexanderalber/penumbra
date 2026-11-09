//! Bindings that carry text and choices end to end: a table cell and a script drive a text param
//! and a choice live, a channel is written as text by a format, a choice keeps its entry when a
//! text names none, and the project file holds the structured form.

use serde_json::json;

use super::{Fixture, eventually};

/// The channel names `/path` makes, after cooking it.
fn names(f: &Fixture, path: &str) -> Vec<String> {
    let read = f.call("chan.read", json!({"path": path}));
    read["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
}

/// The value bound param `param` of `/path` took at its last cook.
fn bound(f: &Fixture, path: &str, param: &str) -> serde_json::Value {
    f.call("chan.read", json!({"path": path}));
    f.call("node.info", json!({"path": path}))["status"]["bound"][param].clone()
}

#[test]
fn a_table_cell_drives_a_text_and_a_choice_live() {
    let f = Fixture::new("bind_text");
    let table = "name,op\nspeed,multiply\nfast,add\n";
    f.call(
        "node.create",
        json!({"path": "/t", "kind": "table.text", "params": {"text": table, "format": "csv"}}),
    );
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m"}));
    f.call("node.bind", json!({"param": "/c.name0", "from": "/t", "cell": {"row": 1, "col": "name"}}));
    f.call("node.bind", json!({"param": "/m.op", "from": "/t", "cell": {"row": "fast", "col": "op"}}));
    assert_eq!(names(&f, "/c"), ["speed"]);
    assert_eq!(bound(&f, "/m", "op"), "add");
    let info = f.call("node.info", json!({"path": "/c"}));
    assert_eq!(info["binds"], json!([{"param": "name0", "from": "/t", "cell": {"row": 1, "col": "name"}}]));

    // Live: an edit of the table reaches both params.
    f.call("node.set", json!({"path": "/t", "params": {"text": "name,op\nslow,x\nfast, Multiply \n"}}));
    assert_eq!(names(&f, "/c"), ["slow"]);
    assert_eq!(bound(&f, "/m", "op"), "multiply", "a choice by name, case and spaces aside");

    // A text the choice does not have: it keeps its last entry and says why.
    f.call("node.set", json!({"path": "/t", "params": {"text": "name,op\nslow,x\nfast,screen\n"}}));
    assert_eq!(bound(&f, "/m", "op"), "multiply");
    let warning = f.call("node.info", json!({"path": "/m"}))["status"]["warning"].clone();
    assert!(warning.as_str().unwrap().contains("\"screen\", which is none of add"), "{warning}");

    // A cell that is not there: the stored value, with a warning.
    f.call("node.bind", json!({"param": "/c.name0", "from": "/t", "cell": {"row": 9}}));
    assert_eq!(names(&f, "/c"), ["chan0"]);
    let warning = f.call("node.info", json!({"path": "/c"}))["status"]["warning"].clone();
    assert!(warning.as_str().unwrap().contains("no row 9"), "{warning}");

    // The file holds the structured form; a plain channel binding keeps the short one.
    eventually("autosave", || {
        f.file().contains(r#"binds: {"op": (node: "t", cell: (row: "fast", col: "op"))}"#)
            && f.file().contains(r#"binds: {"name0": (node: "t", cell: (row: 9, col: 0))}"#)
    });

    // What cannot be bound, and what does not fit.
    let fail = |args: serde_json::Value| f.fail("node.bind", args);
    assert!(fail(json!({"param": "/t.text", "from": "/c", "format": "{} m"})).contains("loop"));
    assert!(fail(json!({"param": "/m.op", "from": "/c", "format": "{}"})).contains("text param"));
    assert!(fail(json!({"param": "/c.name0", "from": "/c", "cell": {}})).contains("no table output"));
    assert!(
        fail(json!({"param": "/c.name0", "from": "/t", "cell": {}, "channel": "x"})).contains("not both")
    );
    assert!(fail(json!({"param": "/c.name0", "from": "/m", "format": "m"})).contains("no placeholder"));
}

#[test]
fn a_channel_is_written_as_text_and_picks_a_choice_by_place() {
    let f = Fixture::new("bind_format");
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 2.4}}));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m"}));
    f.call("node.bind", json!({"param": "/c.name0", "from": "/k", "format": "n{.0}"}));
    f.call("node.bind", json!({"param": "/m.op", "from": "/k"}));
    assert_eq!(names(&f, "/c"), ["n2"]);
    assert_eq!(bound(&f, "/m", "op"), "multiply", "2.4 rounds to the third entry");
    f.call("node.set", json!({"path": "/k", "params": {"value0": 0.6}}));
    assert_eq!(names(&f, "/c"), ["n1"]);
    assert_eq!(bound(&f, "/m", "op"), "subtract");
    f.call("node.set", json!({"path": "/k", "params": {"value0": 99}}));
    assert_eq!(bound(&f, "/m", "op"), "below", "held at the last entry");
    let info = f.call("node.info", json!({"path": "/c"}));
    assert_eq!(info["binds"], json!([{"param": "name0", "from": "/k", "format": "n{.0}"}]));
    eventually("autosave", || f.file().contains(r#"binds: {"name0": (node: "k", format: "n{.0}")}"#));
    eventually("autosave", || f.file().contains(r#"binds: {"op": "k"}"#));
}

#[test]
fn a_script_drives_a_text_and_a_choice_live() {
    if penumbra_script::host().install().is_err() {
        eprintln!("no Python, skipped");
        return;
    }
    let f = Fixture::new("bind_script");
    let code = "PARAMS = {'n': (1.0, 0.0, 9.0)}
def cook(cx):\n    n = int(cx.params['n'])\n    cx.out['v'] = n\n    cx.table.append(['label' + str(n), 'divide' if n else 'add'])\n";
    f.call("node.create", json!({"path": "/s", "kind": "script.python", "params": {"code": code, "n": 1}}));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m"}));
    f.call("node.bind", json!({"param": "/c.name0", "from": "/s", "cell": {"row": 0, "col": 0}}));
    f.call("node.bind", json!({"param": "/m.op", "from": "/s", "cell": {"row": 0, "col": 1}}));
    // The script cooks in a job: its results arrive a few frames later.
    eventually("the script's cell as the name", || names(&f, "/c") == ["label1"]);
    eventually("the script's cell as the op", || bound(&f, "/m", "op") == "divide");
    f.call("node.set", json!({"path": "/s", "params": {"n": 0}}));
    eventually("the new name", || names(&f, "/c") == ["label0"]);
    eventually("the new op", || bound(&f, "/m", "op") == "add");
    // The script's channel picks the choice by place.
    f.call("node.bind", json!({"param": "/m.op", "from": "/s", "channel": "v"}));
    f.call("node.set", json!({"path": "/s", "params": {"n": 3}}));
    eventually("the op by place", || bound(&f, "/m", "op") == "divide");
}
