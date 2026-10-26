//! Table files through the commands: a manual `out.table` writes on `table_out.write`.

use serde_json::json;

use super::{Fixture, eventually};

#[test]
fn write_command_writes_the_file() {
    let f = Fixture::new("table_out");
    f.call(
        "node.create",
        json!({"path": "/t", "kind": "table.text", "params": {"text": "a\tb", "format": "tsv"}}),
    );
    f.call(
        "node.create",
        json!({"path": "/w", "kind": "out.table", "params": {"mode": "manual", "file": "w.tsv"}}),
    );
    f.call("node.connect", json!({"from": "/t", "to": "/w"}));
    assert!(f.fail("table_out.write", json!({"path": "/t"})).contains("not an out.table"));
    eventually("the node to cook", || {
        f.call("table_out.status", json!({}))["tables"].as_array().is_some_and(|t| t.len() == 1)
    });
    let file = f.engine.session().dir().join("w.tsv");
    assert!(!file.exists(), "manual: nothing written before the command");
    let s = f.call("table_out.write", json!({"path": "/w"}));
    assert_eq!(
        (s["node"].clone(), s["rows"].clone(), s["cols"].clone()),
        (json!("/w"), json!(1), json!(2)),
        "{s}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\tb\n");
    let s = f.call("table_out.status", json!({"path": "/w"}));
    assert!(s["written_seconds_ago"].as_f64().is_some(), "{s}");
    // A cell TSV cannot hold: the command fails with the node's error.
    f.call(
        "node.create",
        json!({"path": "/c", "kind": "table.text", "params": {"text": "\"two\nlines\"", "format": "csv"}}),
    );
    f.call("node.connect", json!({"from": "/c", "to": "/w"}));
    assert!(f.fail("table_out.write", json!({"path": "/w"})).contains("write it as csv"));
}
