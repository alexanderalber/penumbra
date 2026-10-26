//! Mesh files through the commands: a manual `out.mesh` writes on `mesh_out.write`.

use serde_json::json;

use super::{Fixture, eventually, warp};

#[test]
fn write_command_writes_the_file() {
    let Some(gpu) = warp() else { return };
    let f = Fixture::new("mesh_out");
    f.engine.attach_gpu(gpu).unwrap();
    f.call("node.create", json!({"path": "/box", "kind": "geo.primitive"}));
    f.call(
        "node.create",
        json!({"path": "/w", "kind": "out.mesh", "params": {"mode": "manual", "file": "w.stl"}}),
    );
    f.call("node.connect", json!({"from": "/box", "to": "/w"}));
    assert!(f.fail("mesh_out.write", json!({"path": "/box"})).contains("not an out.mesh"));
    eventually("the node to cook", || {
        f.call("mesh_out.status", json!({}))["meshes"].as_array().is_some_and(|t| t.len() == 1)
    });
    let file = f.engine.session().dir().join("w.stl");
    assert!(!file.exists(), "manual: nothing written before the command");
    let s = f.call("mesh_out.write", json!({"path": "/w"}));
    assert_eq!(
        (s["node"].clone(), s["format"].clone(), s["triangles"].clone(), s["parts"].clone()),
        (json!("/w"), json!("stl"), json!(12), json!(1)),
        "{s}"
    );
    // Binary STL: an 80 byte header, a count, 50 bytes per triangle.
    assert_eq!(std::fs::metadata(&file).unwrap().len(), 84 + 12 * 50);
    let s = f.call("mesh_out.status", json!({"path": "/w"}));
    assert!(s["written_seconds_ago"].as_f64().is_some(), "{s}");
    // A folder that does not exist: the command fails with the node's error.
    f.call("node.set", json!({"path": "/w", "params": {"file": "missing/w.stl"}}));
    assert!(f.fail("mesh_out.write", json!({"path": "/w"})).contains("w.stl"));
}
