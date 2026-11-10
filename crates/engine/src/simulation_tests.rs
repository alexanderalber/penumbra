//! The shipped simulations (library components on the shelf `simulations`): each one loads from
//! the library, runs on the software adapter at a small size and draws something, and an
//! example's copy of its files is the library's own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
/// that is not the empty ground after a few seconds of its default preset, or of the first
/// entry of its main choice that draws without inputs.
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
        // The simulation and its picture small, so the software adapter keeps up. `grains` is
        // chladni's grain count, in units of 1024.
        let names = shown(&f, &path);
        let mut size = serde_json::Map::new();
        let sizes = [("width", 96), ("height", 54), ("out_width", 192), ("out_height", 108), ("grains", 4)];
        for (param, value) in sizes {
            if names.iter().any(|n| n == param) {
                size.insert(param.into(), json!(value));
            }
        }
        f.call("node.set", json!({"path": path, "params": Json::Object(size)}));
        let out = format!("{path}/out");
        f.call("node.watch", json!({"path": out}));
        // The software adapter compiles a large shader in the background for seconds, and a
        // stepped frame does not wait for it: step until the picture exists.
        let started = Instant::now();
        while !f.call("node.info", json!({"path": out}))["status"]["has_output"].as_bool().unwrap_or(false) {
            assert!(started.elapsed() < Duration::from_secs(300), "{name}: no picture after 5 minutes");
            f.call("cook.step", json!({"frames": 1}));
            std::thread::sleep(Duration::from_millis(20));
        }
        // The default entry first. At this small size and with no inputs wired, some entries
        // draw nothing within seconds (frost that starts on a mask's edges), so the next
        // entries of the main choice follow until one draws.
        let family = f.call("node.info", json!({"path": path}))["component"]["presets"][0].clone();
        let mut entries: Vec<Option<String>> = vec![None];
        for entry in family["entries"].as_array().into_iter().flatten() {
            if entry["name"] != family["chosen"] {
                entries.push(entry["name"].as_str().map(str::to_string));
            }
        }
        let mut drawn = None;
        for entry in &entries {
            if let (Some(entry), Some(choice)) = (entry, family["choice"].as_str()) {
                f.call("node.set", json!({"path": path, "params": {choice: entry}}));
            }
            f.call("cook.step", json!({"frames": 180}));
            let shot = f.call("node.snapshot", json!({"path": out}));
            assert_eq!(shot["error"], Json::Null, "{name} {entry:?}: {shot}");
            let max = shot["stats"]["max"].as_array().cloned().unwrap_or_default();
            let brightest = max.iter().take(3).filter_map(Json::as_f64).fold(0.0, f64::max);
            if brightest > 0.2 {
                drawn = Some(entry.clone().unwrap_or_else(|| "its default".into()));
                break;
            }
        }
        let Some(drawn) = drawn else { panic!("{name}: nothing drawn after 3 s in any entry") };
        println!("{name}: drawn with {drawn}");
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
