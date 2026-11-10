//! The shipped simulations (library components on the shelf `simulations`): each one loads from
//! the library, runs its default entry on the software adapter at a small size, draws and keeps
//! changing, and an example's copy of its files is the library's own.

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

/// How the picture test runs one simulation with its default entry: params set before the first
/// cook (a size at which that entry draws within the test's frames, a smaller grain or agent
/// count), and the frames between the two looks.
struct Run {
    name: &'static str,
    params: &'static [(&'static str, i64)],
    /// Frames from the first look to the second.
    between: u32,
    /// A word on the input `mask`, as in the example: the default grows from a mask's edges.
    word: bool,
}

/// Frames from the first picture to the first look.
const SETTLE: u32 = 60;

/// Each shipped simulation with what its default entry needs. Small sizes keep the software
/// adapter fast. Lenia's gliders need room: at 96 x 54 and 192 x 108 they die out often, and
/// the field stays empty for a second or more until new ones are dropped in.
const RUNS: &[Run] = &[
    Run {
        name: "chladni",
        params: &[("width", 96), ("height", 54), ("grains", 4)],
        between: 60,
        word: false,
    },
    Run { name: "growth", params: &[("width", 96), ("height", 54)], between: 120, word: true },
    Run { name: "lenia", params: &[("width", 256), ("height", 144)], between: 60, word: false },
    Run {
        name: "physarum",
        params: &[("width", 96), ("height", 54), ("agents", 4)],
        between: 60,
        word: false,
    },
    Run { name: "reaction_diffusion", params: &[("width", 96), ("height", 54)], between: 60, word: false },
    Run { name: "sand", params: &[("width", 96), ("height", 54)], between: 60, word: false },
    Run { name: "waves", params: &[("width", 96), ("height", 54)], between: 60, word: false },
];

/// The paths of the shader nodes inside simulation `name`, loaded at `/<name>`.
fn shaders_of(name: &str) -> Vec<String> {
    let file = repo().join("library").join(name).join(format!("{name}.comp.ron"));
    let text = std::fs::read_to_string(file).unwrap();
    text.lines()
        .filter(|l| l.contains("(kind: \"tex.shader\""))
        .filter_map(|l| l.trim().strip_prefix('"').and_then(|l| l.split('"').next()).map(str::to_string))
        .collect()
}

/// The mean difference of two pictures of one size, over red, green and blue.
fn change(a: &Path, b: &Path) -> f64 {
    let read = |p: &Path| penumbra_gpu::image::decode_file(p, 4096).unwrap().to_rgba32f();
    let (a, b) = (read(a), read(b));
    assert_eq!(a.len(), b.len());
    let sum: f64 =
        a.iter().zip(&b).map(|(x, y)| (0..3).map(|c| f64::from((x[c] - y[c]).abs())).sum::<f64>()).sum();
    sum / (3.0 * a.len().max(1) as f64)
}

/// Every simulation in the library loads by `comp.load` and runs its default entry, at a size
/// of its own, on the software adapter. It must draw (a bright pixel) and be alive: its picture
/// changes between two looks some seconds apart. A static picture, such as waves' tiled floor
/// with dead water on it, fails.
#[test]
fn every_simulation_in_the_library_draws_its_default_alive() {
    let f = Fixture::new("simulations");
    f.engine.set_shipped_library(Some(repo().join("library")));
    let names = simulations(&f);
    let listed: Vec<&str> = RUNS.iter().map(|r| r.name).collect();
    assert_eq!(names, listed, "every shipped simulation needs a row in RUNS");
    let Some(gpu) = warp() else { return };
    f.engine.attach_gpu(gpu).unwrap();
    f.call("cook.stepping", json!({"on": true}));
    let shots = f._dir.join("shots");
    for run in RUNS {
        let name = run.name;
        let path = format!("/{name}");
        let loaded = f.call("comp.load", json!({"file": name, "path": path}));
        assert!(loaded["copied"].as_array().is_some_and(|c| !c.is_empty()), "{name}: {loaded}");
        let shown = shown(&f, &path);
        let mut params = serde_json::Map::new();
        for (param, value) in run.params {
            assert!(shown.iter().any(|n| n == param), "{name} has no param {param}");
            params.insert((*param).into(), json!(value));
        }
        // The output picture at the simulation's size, where the component has one.
        for (param, value) in [("out_width", "width"), ("out_height", "height")] {
            let size = run.params.iter().find(|(p, _)| *p == value).map(|(_, v)| *v);
            if let (true, Some(size)) = (shown.iter().any(|n| n == param), size) {
                params.insert(param.into(), json!(size));
            }
        }
        f.call("node.set", json!({"path": path, "params": Json::Object(params)}));
        if run.word {
            f.call(
                "node.create",
                json!({"path": "/word", "kind": "tex.text", "params": {"text": "penumbra", "size": 360.0}}),
            );
            f.call("node.connect", json!({"from": "/word", "to": path, "input": "mask"}));
        }
        let family = f.call("node.info", json!({"path": path}))["component"]["presets"][0].clone();
        let out = format!("{path}/out");
        f.call("node.watch", json!({"path": out}));
        // The software adapter compiles each shader in the background for up to seconds, and a
        // stepped frame does not wait for it: the picture can exist while a step shader is still
        // compiling. Step until the picture exists and no shader inside has a compile pending
        // for some frames in a row (one compiled shader can bring the next one into the cook).
        let shaders = shaders_of(name);
        let started = Instant::now();
        let mut quiet = 0;
        while quiet < 5 {
            assert!(started.elapsed() < Duration::from_secs(300), "{name}: not ready after 5 minutes");
            f.call("cook.step", json!({"frames": 1}));
            std::thread::sleep(Duration::from_millis(20));
            let drawn =
                f.call("node.info", json!({"path": out}))["status"]["has_output"].as_bool() == Some(true);
            let pending = shaders.iter().any(|s| {
                // A shader nothing pulls has no job (null) and costs no wait.
                let job = f.call("node.info", json!({"path": s}))["status"]["job"].clone();
                assert_ne!(job["state"], "failed", "{s}: {job}");
                job["state"] == "queued" || job["state"] == "running"
            });
            quiet = if drawn && !pending { quiet + 1 } else { 0 };
        }
        let look = |frames: u32, file: &str| {
            f.call("cook.step", json!({"frames": frames}));
            let file = shots.join(format!("{name}_{file}.png"));
            let shot = f.call("node.snapshot", json!({"path": out, "file": file}));
            assert_eq!(shot["error"], Json::Null, "{name}: {shot}");
            let max = shot["stats"]["max"].as_array().cloned().unwrap_or_default();
            (file, max.iter().take(3).filter_map(Json::as_f64).fold(0.0, f64::max))
        };
        let (first, _) = look(SETTLE, "first");
        let (second, brightest) = look(run.between, "second");
        let changed = change(&first, &second);
        println!("{name} ({}): brightest {brightest:.3}, change {changed:.4}", family["chosen"]);
        assert!(brightest > 0.2, "{name}: nothing drawn with its default {}", family["chosen"]);
        assert!(changed > 0.002, "{name}: the picture stood still with its default {}", family["chosen"]);
        let presets = &f.call("node.info", json!({"path": path}))["component"]["presets"];
        for family in presets.as_array().into_iter().flatten() {
            assert_eq!(family["problems"], json!([]), "{name}: {family}");
        }
        f.call("node.delete", json!({"path": path}));
        if run.word {
            f.call("node.delete", json!({"path": "/word"}));
        }
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
