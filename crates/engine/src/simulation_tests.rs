//! The shipped simulations (library components on the shelf `simulations`): each one loads from
//! the library, runs on the software adapter at a small size and draws something, and an
//! example's copy of its files is the library's own.

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

use super::{Fixture, warp};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The names of the library's components on the shelf `simulations`.
fn simulations(f: &Fixture) -> Vec<String> {
    let listed = f.call("comp.library", json!({}));
    let mut names: Vec<String> = listed["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["place"] == "penumbra")
        .filter(|c| c["shelf"].as_str().is_some_and(|s| s == "simulations" || s.starts_with("simulations/")))
        .filter_map(|c| c["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// The names a component shows: its own params and the exposed ones.
fn shown(f: &Fixture, path: &str) -> Vec<String> {
    let info = f.call("node.info", json!({"path": path}));
    let own = info["instance_params"].as_array().into_iter().flatten();
    let exposed = info["component"]["exposed"].as_array().into_iter().flatten();
    own.chain(exposed).filter_map(|p| p["name"].as_str().map(str::to_string)).collect()
}

/// Every simulation in the library loads by `comp.load` and, at a small size, draws a picture
/// that is not the empty ground after a few seconds of its default preset.
#[test]
fn every_simulation_in_the_library_draws_a_picture() {
    let f = Fixture::new("simulations");
    f.engine.set_shipped_library(Some(repo().join("library")));
    let names = simulations(&f);
    assert!(names.iter().any(|n| n == "sand"), "{names:?}");
    let Some(gpu) = warp() else { return };
    f.engine.attach_gpu(gpu).unwrap();
    f.call("cook.stepping", json!({"on": true}));
    for name in &names {
        let path = format!("/{name}");
        let loaded = f.call("comp.load", json!({"file": name, "path": path}));
        assert!(loaded["copied"].as_array().is_some_and(|c| !c.is_empty()), "{name}: {loaded}");
        // The simulation and its picture small, so the software adapter keeps up.
        let names = shown(&f, &path);
        let mut size = serde_json::Map::new();
        for (param, value) in [("width", 96), ("height", 54), ("out_width", 192), ("out_height", 108)] {
            if names.iter().any(|n| n == param) {
                size.insert(param.into(), json!(value));
            }
        }
        f.call("node.set", json!({"path": path, "params": Json::Object(size)}));
        let out = format!("{path}/out");
        f.call("node.watch", json!({"path": out}));
        f.call("cook.step", json!({"frames": 180}));
        let shot = f.call("node.snapshot", json!({"path": out}));
        assert_eq!(shot["error"], Json::Null, "{name}: {shot}");
        let max = shot["stats"]["max"].as_array().cloned().unwrap_or_default();
        let brightest = max.iter().take(3).filter_map(Json::as_f64).fold(0.0, f64::max);
        assert!(brightest > 0.2, "{name}: nothing drawn after 3 s: {shot}");
        let presets = &f.call("node.info", json!({"path": path}))["component"]["presets"];
        for family in presets.as_array().into_iter().flatten() {
            assert_eq!(family["problems"], json!([]), "{name}: {family}");
        }
        f.call("node.delete", json!({"path": path}));
    }
}

/// An example that carries a simulation's files (`library/<name>/` beside its project, as
/// `comp.load` copies them) carries the library's own: a fix in the library reaches the example.
#[test]
fn an_example_copy_of_a_simulation_is_the_library_one() {
    let f = Fixture::new("simulation_copies");
    let repo = repo();
    f.engine.set_shipped_library(Some(repo.join("library")));
    let names = simulations(&f);
    let mut examples: Vec<PathBuf> =
        std::fs::read_dir(repo.join("examples")).unwrap().flatten().map(|e| e.path()).collect();
    examples.sort();
    let mut compared = 0;
    for name in &names {
        for example in &examples {
            let copy = example.join("library").join(name);
            let Ok(files) = std::fs::read_dir(&copy) else { continue };
            for file in files.flatten() {
                let original = repo.join("library").join(name).join(file.file_name());
                let (a, b) =
                    (std::fs::read(file.path()).unwrap(), std::fs::read(&original).unwrap_or_default());
                assert!(a == b, "{} differs from {}", file.path().display(), original.display());
                compared += 1;
            }
        }
    }
    assert!(compared > 0, "no example carries a simulation");
}
