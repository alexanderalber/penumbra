//! End-to-end tests of the engine through its command registry, on a project in a temporary
//! directory with the built-in node types.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_graph::Op;
use penumbra_project::{Actor, Content, Session};
use serde_json::{Value as Json, json};

use crate::{Engine, Registry};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("penumbra-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    engine: Arc<Engine>,
    registry: Registry,
    _dir: TempDir,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        let types = Arc::new(penumbra_ops::builtin_types().unwrap());
        let session = Arc::new(Session::open(&dir.0, types, &Content::default()).unwrap());
        let engine = Engine::start(session).unwrap();
        let registry = crate::engine_registry(&engine);
        Self { engine, registry, _dir: dir }
    }

    fn call(&self, method: &str, params: Json) -> Json {
        let r = self.registry.dispatch(method, &params);
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }

    fn fail(&self, method: &str, params: Json) -> String {
        let r = self.registry.dispatch(method, &params);
        assert_eq!(r["ok"], false, "{method} {params} should fail: {r}");
        r["error"].as_str().unwrap().to_string()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.engine.session().file()).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}

/// Polls `check` for up to 5 s.
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn build_a_network_and_watch_it_cook() {
    let f = Fixture::new("build");
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo", "params": {"rate": 2}}));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant"}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/lfo", "to": "/m", "input": "a"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m", "input": 1}));
    f.call("node.set", json!({"path": "/c", "params": {"value0": 0.5}}));
    f.call("node.set", json!({"path": "/m", "params": {"op": "multiply"}}));

    let info = f.call("node.info", json!({"path": "/m"}));
    assert_eq!(info["inputs"][0]["from"], "/lfo");
    assert_eq!(info["inputs"][1]["from"], "/c");
    assert_eq!(info["params"][0], json!({"name": "op", "value": "multiply", "default": "add"}));
    // A frame in between gives /m a runtime (and so a status), but nothing cooks it unasked.
    assert!(info["status"].is_null() || info["status"]["cooks"] == 0, "nothing asked for /m yet");
    let lfo = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(lfo["outputs"][0]["to"][0], json!({"node": "/m", "input": "a"}));

    f.call("node.watch", json!({"path": "/m"}));
    eventually("/m to cook", || {
        let s = f.call("node.info", json!({"path": "/m"}))["status"].clone();
        s["version"].as_u64().unwrap_or(0) > 0 && s["cooks"].as_u64().unwrap_or(0) > 2
    });
    let s = f.call("cook.status", json!({}));
    assert_eq!(s["watched"], json!(["/m"]));
    assert!(s["frame"].as_i64().unwrap() > 0);

    let list = f.call("node.list", json!({}));
    assert_eq!(list["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(list["nodes"][0], json!({"path": "/c", "kind": "chan.constant"}));
    let kinds = f.call("node.kinds", json!({"kind": "chan.lfo"}));
    assert_eq!(kinds["kinds"][0]["params"][0]["name"], "rate");
    let help = f.call("node.kinds", json!({"kind": "chan.lfo", "param": "shape"}))["param"].clone();
    assert_eq!(help["type"], "choice");
    assert_eq!(help["default"], "sine");
    assert_eq!(
        help["choices"][0],
        json!({"value": "sine", "doc": help["choices"][0]["doc"], "default": true})
    );
    assert!(!help["doc"].as_str().unwrap_or_default().is_empty());
    let rate = f.call("node.kinds", json!({"kind": "chan.lfo", "param": "rate"}))["param"].clone();
    assert_eq!((rate["unit"].as_str(), rate["bindable"].as_bool()), (Some("Hz"), Some(true)));
    assert!(f.fail("node.kinds", json!({"param": "rate"})).contains("needs `kind`"));
    assert!(f.fail("node.kinds", json!({"kind": "chan.lfo", "param": "nope"})).contains("no param `nope`"));
}

#[test]
fn replace_a_node_keeping_what_fits() {
    let f = Fixture::new("replace");
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo", "params": {"rate": 2}}));
    f.call("node.create", json!({"path": "/m", "kind": "chan.math"}));
    f.call("node.connect", json!({"from": "/lfo", "to": "/m", "input": "a"}));
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise"}));
    f.call("node.bind", json!({"param": "/n.speed", "from": "/lfo"}));
    let r = f.call("node.replace", json!({"path": "/lfo", "kind": "chan.noise"}));
    assert_eq!(r["history"], "new");
    let info = f.call("node.info", json!({"path": "/lfo"}));
    assert_eq!(info["kind"], "chan.noise");
    let m = f.call("node.info", json!({"path": "/m"}));
    assert_eq!(m["inputs"][0]["from"], "/lfo");
    let n = f.call("node.info", json!({"path": "/n"}));
    assert!(n.to_string().contains("/lfo"), "the binding stays: {n}");
    // chan.noise has no `rate`.
    assert!(r["lost"].as_array().unwrap().iter().any(|l| l == "param rate"), "{r}");
    f.call("history.undo", json!({}));
    assert_eq!(f.call("node.info", json!({"path": "/lfo"}))["kind"], "chan.lfo");
    assert!(f.fail("node.replace", json!({"path": "/m", "kind": "comp.base"})).contains("component"));
}

#[test]
fn errors_say_what_is_wrong() {
    let f = Fixture::new("errors");
    assert!(f.fail("node.create", json!({"path": "/x", "kind": "chan.nope"})).contains("chan.lfo"));
    assert!(f.fail("node.create", json!({"path": "x", "kind": "chan.lfo"})).contains("must start with"));
    assert!(f.fail("node.create", json!({"path": "/x", "kind": "chan.lfo", "colour": 1})).contains("colour"));
    f.call("node.create", json!({"path": "/x", "kind": "chan.lfo"}));
    assert!(f.fail("node.set", json!({"path": "/x", "params": {"rate": "fast"}})).contains("rate"));
    assert!(f.fail("node.connect", json!({"from": "/x", "to": "/x", "input": "q"})).contains("no input"));
    assert!(f.fail("node.info", json!({"path": "/y"})).contains("no node at /y"));
    assert!(f.fail("node.batch", json!({"ops": []})).contains("empty"));
    assert!(f.fail("nope", json!({})).contains("commands"));
    let all = f.call("commands", json!({}));
    assert!(all["commands"].as_array().unwrap().iter().any(|c| c["id"] == "node.batch"));
}

#[test]
fn a_claim_groups_edits_and_blocks_the_user() {
    let f = Fixture::new("claim");
    f.call("edit.begin", json!({"label": "demo"}));
    let s = f.engine.session();
    let op =
        Op::Create { path: "/u".parse().unwrap(), kind: "chan.constant".into(), params: Default::default() };
    assert!(s.edit(Actor::User, &[op], None, "", Instant::now()).is_err());
    f.call("node.create", json!({"path": "/a", "kind": "chan.constant"}));
    let r = f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/b", "kind": "chan.constant"},
            {"op": "set", "param": "/b.value0", "value": 0.25},
        ]}),
    );
    assert_eq!(r["history"], "merged");
    assert_eq!(r["created"], json!(["/b"]));
    let h = f.call("history.list", json!({}));
    assert_eq!(h["total"], 1);
    assert_eq!(f.call("edit.end", json!({}))["was_claimed"], true);
    assert_eq!(f.call("edit.status", json!({}))["mode"], "user");

    f.call("history.undo", json!({}));
    assert_eq!(f.call("node.list", json!({}))["nodes"], json!([]));
    f.call("history.redo", json!({}));
    assert_eq!(f.call("node.list", json!({}))["nodes"].as_array().unwrap().len(), 2);
}

#[test]
fn autosave_and_external_edits_run_on_the_project_thread() {
    let f = Fixture::new("files");
    f.call("node.create", json!({"path": "/a", "kind": "chan.constant", "params": {"value0": 0.75}}));
    eventually("autosave", || {
        f.file().contains(r#""/a": (kind: "chan.constant", params: {"value0": 0.75}),"#)
    });
    let edited = f.file().replace("0.75", "0.5");
    std::fs::write(f.engine.session().file(), edited).unwrap();
    eventually("the merge", || {
        f.call("node.info", json!({"path": "/a"}))["params"][0]["value"] == json!(0.5)
    });
    let h = f.call("history.list", json!({}));
    assert_eq!(h["entries"][0]["source"], "project_file");
    assert_eq!(f.call("project.info", json!({}))["file_error"], Json::Null);
}

#[test]
fn shutdown_writes_what_autosave_has_not_yet() {
    let f = Fixture::new("shutdown");
    f.call("node.create", json!({"path": "/late", "kind": "chan.constant"}));
    f.engine.shutdown();
    assert!(f.file().contains("/late"), "{}", f.file());
    // The cook thread is gone; commands that need it fail instead of hanging.
    assert!(f.fail("cook.status", json!({})).contains("stopped"));
}

#[test]
fn thumbnail_switch() {
    let f = Fixture::new("thumbs");
    assert_eq!(f.call("cook.status", json!({}))["thumbnails"]["mode"], "always");
    f.call("cook.thumbnails", json!({"mode": "when_free"}));
    assert_eq!(f.call("cook.status", json!({}))["thumbnails"]["mode"], "when_free");
    assert!(f.fail("cook.thumbnails", json!({"enabled": false})).contains("mode"));
    assert_eq!(f.call("cook.status", json!({}))["thumbnails"]["min_interval"], 1);
    let r = f.call("cook.thumbnails", json!({"every": 6}));
    assert_eq!((r["mode"].as_str(), r["every"].as_u64()), (Some("when_free"), Some(6)));
    assert_eq!(f.call("cook.status", json!({}))["thumbnails"]["min_interval"], 6);
    for bad in [json!({}), json!({"every": 0}), json!({"every": 61})] {
        assert!(!f.fail("cook.thumbnails", bad).is_empty());
    }
}

#[test]
fn a_lost_gpu_stops_the_passes_records_the_crash_and_saves_no_state() {
    let f = Fixture::new("lost");
    let Ok(gpu) = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) else { return };
    let marker = f.engine.session().dir().join("marker").join(crate::crash::FILE);
    f.engine.set_crash_marker(Some(marker.clone()));
    f.engine.attach_gpu(gpu).unwrap();
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise", "params": {"width": 32, "height": 16}}));
    f.call("node.watch", json!({"path": "/n"}));
    eventually("a pass", || f.call("cook.status", json!({}))["pass"].as_u64() > Some(2));
    assert!(f.fail("gpu.simulate_loss", json!({})).contains("confirm"));
    f.call("gpu.simulate_loss", json!({"confirm": true}));
    let m = crate::crash::take(&marker).unwrap();
    assert!(m.reason.starts_with("GPU device lost: simulated"), "{m:?}");
    assert_eq!(std::path::Path::new(&m.project), f.engine.session().dir());
    assert!(f.call("gpu.info", json!({}))["lost"].as_str().is_some());
    // No further passes, and no state written from a lost device.
    let pass = f.call("cook.status", json!({}))["pass"].as_u64();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(f.call("cook.status", json!({}))["pass"].as_u64(), pass);
    assert!(f.engine.save_state(Duration::from_secs(1)).unwrap_err().0.contains("lost"));
    // The notice goes when the clock runs again.
    f.engine.set_notice(Some("paused".into()));
    assert_eq!(f.call("cook.status", json!({}))["notice"], "paused");
    f.call("cook.play", json!({"playing": true}));
    assert_eq!(f.call("cook.status", json!({}))["notice"], Json::Null);
}

#[test]
fn snapshot_of_a_texture_node() {
    let f = Fixture::new("snapshot");
    assert!(f.fail("node.snapshot", json!({"path": "/n"})).contains("no GPU"));
    assert_eq!(f.call("gpu.info", json!({}))["attached"], false);
    let gpu = match penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        Ok(g) => g,
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(_) => return,
    };
    f.engine.attach_gpu(gpu).unwrap();
    assert!(
        f.engine
            .attach_gpu(penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software).unwrap())
            .is_err()
    );
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise", "params": {"width": 32, "height": 16}}));
    f.call("node.create", json!({"path": "/l", "kind": "chan.lfo"}));
    let out = f.engine.session().dir().join("shots").join("n.png");
    let r = f.call("node.snapshot", json!({"path": "/n", "file": out}));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(32), Some(16)));
    let png = std::fs::read(&out).unwrap();
    assert_eq!(png.get(1..4), Some(&b"PNG"[..]));
    let mean = r["stats"]["mean"][0].as_f64().unwrap();
    assert!(mean > 0.1 && mean < 0.9, "{r}");
    assert!(f.fail("node.snapshot", json!({"path": "/l"})).contains("not a texture"));
    assert!(f.fail("node.snapshot", json!({"path": "/n", "output": "nope"})).contains("nope"));

    // A shader node's params from its header show up in node.info.
    std::fs::write(
        f.engine.session().dir().join("s.wgsl"),
        "// @param gain float = 0.5\n@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(params.gain); }",
    )
    .unwrap();
    f.call(
        "node.create",
        json!({"path": "/s", "kind": "tex.shader", "params": {"file": "s.wgsl", "resolution": "custom", "width": 4, "height": 4, "typo": 1}}),
    );
    let r = f.call("node.snapshot", json!({"path": "/s"}));
    assert!((r["stats"]["mean"][0].as_f64().unwrap() - 0.5).abs() < 1e-3, "{r}");
    assert!(r["warning"].as_str().unwrap().contains("typo"));
    let info = f.call("node.info", json!({"path": "/s"}));
    assert_eq!(info["instance_params"][0]["name"], "gain");
    assert_eq!(info["instance_params"][1], json!({"name": "typo", "value": 1, "declared": false}));

    // A mapper: the right half of a white input, its top left corner pulled a quarter in.
    f.call(
        "node.create",
        json!({"path": "/c", "kind": "tex.constant", "params": {"color": [1, 1, 1, 1], "width": 64, "height": 36}}),
    );
    f.call(
        "node.create",
        json!({"path": "/m", "kind": "tex.mapper", "params": {"region": [0.5, 0, 0.5, 1], "fit": "stretch", "corner_tl": [0.25, 0.25], "width": 192, "height": 108}}),
    );
    assert!(f.fail("node.snapshot", json!({"path": "/m"})).contains("connect an input"));
    f.call("node.connect", json!({"from": "/c", "to": "/m", "input": 0}));
    let r = f.call("node.snapshot", json!({"path": "/m"}));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(192), Some(108)), "{r}");
    // The quadrilateral (0.25, 0.25), (1, 0), (1, 1), (0, 1) covers 3/4 of the picture.
    let mean = r["stats"]["mean"][0].as_f64().unwrap();
    assert!((mean - 0.75).abs() < 0.02, "{r}");
    // Crossed corners are an error on the node, which keeps its last good picture.
    f.call("node.set", json!({"path": "/m", "params": {"corner_tl": [2, 2]}}));
    let r = f.call("node.snapshot", json!({"path": "/m"}));
    assert!(r["error"].as_str().unwrap_or_default().contains("corners"), "{r}");
    assert!((r["stats"]["mean"][0].as_f64().unwrap() - 0.75).abs() < 0.02, "{r}");
    assert_eq!(f.call("gpu.info", json!({}))["errors"], 0);
}

#[test]
fn bind_a_param_and_read_channels() {
    let f = Fixture::new("bind");
    f.call(
        "node.create",
        json!({"path": "/c", "kind": "chan.constant", "params": {"value0": 0.5, "name0": "x"}}),
    );
    f.call("node.create", json!({"path": "/m", "kind": "chan.math", "params": {"op": "multiply", "b": 4}}));
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise"}));
    f.call("node.bind", json!({"param": "/m.b", "from": "/c", "channel": "x"}));
    f.call("node.bind", json!({"param": "/n.translate.2", "from": "/c"}));
    f.call("node.connect", json!({"from": "/c", "to": "/m"}));
    let read = f.call("chan.read", json!({"path": "/m", "samples": true}));
    assert_eq!(read["channels"], json!([{"name": "x", "value": 0.25, "samples": [0.25]}]));
    assert_eq!(read["rate"], 60.0);
    let info = f.call("node.info", json!({"path": "/m"}));
    assert_eq!(info["binds"], json!([{"param": "b", "from": "/c:x"}]));
    assert_eq!(info["status"]["bound"], json!({"b": 0.5}));
    eventually("autosave", || f.file().contains(r#"binds: {"translate.2": "c"}"#));
    assert!(f.fail("node.bind", json!({"param": "/n.translate", "from": "/c"})).contains("one at a time"));
    assert!(f.fail("node.bind", json!({"param": "/c.value0", "from": "/m"})).contains("loop"));
    assert!(f.fail("chan.read", json!({"path": "/n"})).contains("no Chan output"));
    f.call("node.unbind", json!({"param": "/m.b"}));
    assert_eq!(f.call("chan.read", json!({"path": "/m"}))["channels"][0]["value"], 2.0);
}

#[test]
fn the_view_cooks_viewers_and_thumbnails_and_publishes_them() {
    let f = Fixture::new("view");
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    f.call("node.create", json!({"path": "/c", "kind": "chan.constant", "params": {"value0": 0.25}}));
    let net = f.engine.session().doc().snapshot();
    let lfo = net.resolve(&"/lfo".parse().unwrap()).unwrap();
    let c = net.resolve(&"/c".parse().unwrap()).unwrap();
    f.engine.set_view(crate::ViewRequest {
        viewers: vec![lfo],
        thumbnails: vec![c],
        passive: Vec::new(),
        shown: Vec::new(),
        detail: Some(lfo),
        resolution: false,
        free: Vec::new(),
    });
    eventually("both outputs in the view", || {
        let v = f.engine.view();
        v.outputs.contains_key(&lfo) && v.outputs.contains_key(&c) && v.status.len() == 2
    });
    let v = f.engine.view();
    assert_eq!(v.outputs[&c][0].get::<penumbra_core::Chans>().and_then(|c| c.value(None)), Some(0.25));
    assert!(v.thumbnails == penumbra_graph::ThumbnailMode::Always && v.playing);
    // The LFO is a viewer, so it cooks every frame.
    let before = v.status[&lfo].stats.cooks;
    eventually("the viewer to keep cooking", || f.engine.view().status[&lfo].stats.cooks > before + 3);

    f.call("cook.play", json!({"playing": false}));
    eventually("the clock to stop", || !f.engine.view().playing);
    let frame = f.engine.view().time.unwrap().frame;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(f.engine.view().time.unwrap().frame, frame);
    f.call("cook.play", json!({}));
    eventually("the clock to run", || f.engine.view().playing);

    // Pictures nobody cooks are shown only while current: the constant stays, a free-running
    // LFO that only the viewer cooked stops being shown once the viewer moves on.
    f.call("node.create", json!({"path": "/lfo2", "kind": "chan.lfo"}));
    let net = f.engine.session().doc().snapshot();
    let lfo2 = net.resolve(&"/lfo2".parse().unwrap()).unwrap();
    f.call("cook.thumbnails", json!({"mode": "when_free"}));
    f.engine.set_view(crate::ViewRequest {
        viewers: vec![lfo, lfo2],
        thumbnails: vec![c, lfo, lfo2],
        passive: Vec::new(),
        shown: Vec::new(),
        detail: None,
        resolution: false,
        free: Vec::new(),
    });
    eventually("all current", || f.engine.view().pictures.len() == 3);
    f.engine.set_view(crate::ViewRequest {
        viewers: vec![lfo],
        thumbnails: vec![c, lfo, lfo2],
        passive: Vec::new(),
        shown: Vec::new(),
        detail: None,
        resolution: false,
        free: Vec::new(),
    });
    eventually("the uncooked LFO hidden", || {
        let v = f.engine.view();
        v.pictures == [c, lfo].into() && !v.outputs.contains_key(&lfo2)
    });
    f.call("cook.thumbnails", json!({"mode": "never"}));
    eventually("no pictures", || {
        let v = f.engine.view();
        v.pictures.is_empty() && v.outputs.contains_key(&lfo)
    });
}

#[test]
fn resolution_analysis_and_optimisation_through_commands() {
    let f = Fixture::new("res");
    let gpu = match penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        Ok(g) => g,
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(_) => return,
    };
    f.engine.attach_gpu(gpu).unwrap();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/c", "kind": "tex.constant", "params": {"width": 480, "height": 270}},
            {"op": "create", "path": "/m", "kind": "tex.mapper"},
            {"op": "create", "path": "/p", "kind": "out.projector"},
            {"op": "connect", "from": "/c", "to": "/m"},
            {"op": "connect", "from": "/m", "to": "/p"},
        ]}),
    );
    let width = || {
        let info = f.call("node.info", json!({"path": "/c"}));
        info["params"].as_array().unwrap().iter().find(|p| p["name"] == "width").unwrap()["value"].clone()
    };
    let node =
        |r: &Json, path: &str| r["nodes"].as_array().unwrap().iter().find(|n| n["path"] == path).cloned();
    let mut report = Json::Null;
    eventually("the chain to cook", || {
        report = f.call("res.analyze", json!({}));
        node(&report, "/c").is_some_and(|n| !n["size"].is_null())
    });
    let c = node(&report, "/c").unwrap();
    assert_eq!((c["verdict"].as_str(), &c["suggested"]), (Some("coarse"), &json!([1920, 1080])), "{report}");
    assert_eq!(node(&report, "/m").unwrap()["verdict"], "ok");
    assert_eq!(report["projectors"][0]["assumed"], true);

    let dry = f.call("res.optimize", json!({"dry_run": true}));
    assert_eq!(dry["changes"], json!([{"path": "/c", "from": [480, 270], "to": [1920, 1080]}]), "{dry}");
    assert_eq!(width(), 480);
    let done = f.call("res.optimize", json!({}));
    assert_eq!(done["history"], "new", "{done}");
    assert_eq!(width(), 1920);
    f.call("history.undo", json!({}));
    assert_eq!(width(), 480);
    assert!(f.fail("res.optimize", json!({"paths": ["/nope"]})).contains("/nope"));

    assert_eq!(f.call("res.overlay", json!({}))["mappers"], 1);
    eventually("the overlay in the view", || f.engine.view().overlay && f.engine.view().resolution.is_some());
    f.call("res.overlay", json!({"on": false}));
    eventually("the overlay gone", || !f.engine.view().overlay && f.engine.view().resolution.is_none());
}

#[test]
fn components_group_save_load_and_dissolve() {
    let f = Fixture::new("comps");
    let lib = TempDir::new("comps-lib");
    f.engine.set_library(Some(lib.0.clone()));
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/lfo", "kind": "chan.lfo"},
            {"op": "create", "path": "/gain", "kind": "chan.math"},
            {"op": "create", "path": "/k", "kind": "chan.constant", "params": {"value0": 2.0}},
            {"op": "create", "path": "/sum", "kind": "chan.math"},
            {"op": "connect", "from": "/lfo", "to": "/gain"},
            {"op": "connect", "from": "/gain", "to": "/sum"},
        ]}),
    );
    let r = f.call("comp.collapse", json!({"paths": ["/gain", "/k"], "name": "fx"}));
    assert_eq!(r["path"], "/fx");
    assert_eq!(r["moved"]["/gain"], "/fx/gain");
    // A wire straight into the middle is refused; through the component's input it works.
    let e = f.fail("node.connect", json!({"from": "/lfo", "to": "/fx/gain", "input": "b"}));
    assert!(e.contains("different components"), "{e}");
    let info = f.call("node.info", json!({"path": "/fx"}));
    assert_eq!(info["component"]["inputs"][0]["name"], "lfo_in", "{info}");
    assert_eq!(info["component"]["inputs"][0]["from"], "/lfo");
    assert_eq!(info["component"]["outputs"][0]["name"], "gain_out");
    f.call("node.disconnect", json!({"to": "/fx", "input": "lfo_in"}));
    f.call("node.connect", json!({"from": "/lfo", "to": "/fx", "input": 0}));
    f.call("node.connect", json!({"from": "/fx", "output": "gain_out", "to": "/sum", "input": "b"}));
    // Exposed params act on the inner ones.
    f.call("node.set", json!({"path": "/fx", "params": {"expose": "level = k.value0"}}));
    f.call("node.set", json!({"path": "/fx", "params": {"level": 5.0}}));
    let k = f.call("node.info", json!({"path": "/fx/k"}));
    assert_eq!(k["params"][0]["value"], 5.0, "{k}");
    let info = f.call("node.info", json!({"path": "/fx"}));
    assert_eq!(info["component"]["exposed"][0]["value"], 5.0, "{info}");
    let v = f.call("chan.read", json!({"path": "/sum"}));
    assert!(v.to_string().contains("value"), "{v}");

    // Into the library and back, twice.
    let saved = f.call("comp.save", json!({"path": "/fx"}));
    assert_eq!(saved["dropped"].as_array().map(Vec::len), Some(1), "{saved}");
    assert!(lib.0.join("fx.comp.ron").is_file());
    let listed = f.call("comp.library", json!({}));
    assert_eq!(listed["components"][0]["name"], "fx", "{listed}");
    let loaded = f.call("comp.load", json!({"file": "fx"}));
    assert_eq!(loaded["path"], "/fx1");
    let k1 = f.call("node.info", json!({"path": "/fx1/k"}));
    assert_eq!(k1["params"][0]["value"], 5.0);
    assert!(f.fail("comp.load", json!({"file": "nope"})).contains("no component file"));
    // Out of the library again: moved aside, no longer offered.
    let removed = f.call("comp.remove", json!({"name": "fx"}));
    assert!(lib.0.join("removed").join("fx.comp.ron").is_file(), "{removed}");
    assert!(!lib.0.join("fx.comp.ron").exists());
    assert_eq!(f.call("comp.library", json!({}))["components"], json!([]));
    assert!(f.fail("comp.remove", json!({"name": "fx"})).contains("no component file"));

    // Dissolving joins the wires again.
    let r = f.call("comp.expand", json!({"path": "/fx"}));
    assert_eq!(r["moved"]["/fx/gain"], "/gain", "{r}");
    let sum = f.call("node.info", json!({"path": "/sum"}));
    assert_eq!(sum["inputs"][1]["from"], "/gain", "{sum}");
    let gain = f.call("node.info", json!({"path": "/gain"}));
    assert_eq!(gain["inputs"][0]["from"], "/lfo", "{gain}");
    // One undo brings the component back.
    f.call("history.undo", json!({}));
    f.call("node.info", json!({"path": "/fx/gain"}));
}

#[test]
fn geometry_through_commands() {
    let f = Fixture::new("geo");
    let gpu = match penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        Ok(g) => g,
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(_) => return,
    };
    f.engine.attach_gpu(gpu).unwrap();
    f.call("node.create", json!({"path": "/box", "kind": "geo.primitive", "params": {"size": [2, 1, 1]}}));
    f.call("node.create", json!({"path": "/ball", "kind": "geo.primitive", "params": {"shape": "sphere"}}));
    f.call(
        "node.create",
        json!({"path": "/up", "kind": "geo.transform", "params": {"translate": [0, 0, 1]}}),
    );
    f.call("node.create", json!({"path": "/both", "kind": "geo.merge"}));
    f.call("node.connect", json!({"from": "/ball", "to": "/up"}));
    f.call("node.connect", json!({"from": "/box", "to": "/both", "input": 0}));
    f.call("node.connect", json!({"from": "/up", "to": "/both", "input": 1}));
    let info = f.call("geo.info", json!({"path": "/both"}));
    assert_eq!(info["parts"][0], json!({"id": 0, "name": "box", "triangles": 12}));
    assert_eq!(info["parts"][1]["name"], "sphere");
    assert_eq!(info["bounds"]["min"], json!([-1.0, -0.5, -0.5]));
    assert_eq!(info["bounds"]["max"][2], 1.5);
    // A geo node's snapshot is its picture.
    let r = f.call("node.snapshot", json!({"path": "/both"}));
    assert_eq!(r["width"], 512);
    // A render needs a camera node: render.frame and the render say so without one.
    f.call("node.create", json!({"path": "/r", "kind": "tex.render", "params": {"width": 64, "height": 36}}));
    f.call("node.connect", json!({"from": "/both", "to": "/r"}));
    assert!(f.fail("render.frame", json!({"path": "/r"})).contains("view.camera"));
    // render.frame points the wired camera at the middle of the geometry, keeping its direction.
    f.call(
        "node.create",
        json!({"path": "/cam", "kind": "view.camera", "params": {"position": [0, -50, 0]}}),
    );
    f.call("node.connect", json!({"from": "/cam", "to": "/r", "input": "camera"}));
    let framed = f.call("render.frame", json!({"path": "/r"}));
    assert_eq!(framed["camera"], "/cam", "{framed}");
    assert_eq!(framed["target"], json!([0.0, 0.0, 0.5]));
    let y = framed["position"][1].as_f64().unwrap();
    assert!(y < -2.0 && y > -6.0 && framed["position"][0] == 0.0, "{framed}");
    let r = f.call("node.snapshot", json!({"path": "/r"}));
    assert!(r["stats"]["max"][3].as_f64().unwrap() > 0.99, "{r}");
    assert!(f.fail("render.frame", json!({"path": "/box"})).contains("not a tex.render"));
    // The free view: a render's starts at its camera, and draws that camera as a frustum; a
    // geometry's starts framed. Any camera can be given; the viewer asks through the view.
    let free = f.call("view.free", json!({"path": "/r", "width": 96, "height": 54}));
    let eye = free["home"]["eye"].as_array().unwrap();
    assert!((eye[1].as_f64().unwrap() - y).abs() < 1e-6, "{free}");
    assert!((free["home"]["target"][2].as_f64().unwrap() - 0.5).abs() < 1e-6, "{free}");
    assert_eq!((free["width"].clone(), free["lines"].clone()), (json!(96), json!(10)));
    let free = f.call(
        "view.free",
        json!({"path": "/both", "camera": {"eye": [0, 0, 9], "target": [0, 0, 0], "roll": 0, "fov": 30}}),
    );
    assert_eq!((free["camera"]["eye"][2].clone(), free["lines"].clone()), (json!(9.0), json!(0)));
    assert!(
        f.fail(
            "view.free",
            json!({"path": "/box", "camera": {"eye": [0, 0, 0], "target": [0, 0, 0], "roll": 0, "fov": 30}})
        )
        .contains("apart")
    );
    f.call("node.create", json!({"path": "/k", "kind": "tex.constant"}));
    assert!(f.fail("view.free", json!({"path": "/k"})).contains("no 3D scene"));
    let net = f.engine.session().doc().snapshot();
    let r = net.resolve(&"/r".parse().unwrap()).unwrap();
    let request = penumbra_ops::freeview::Request { node: r, output: 0, camera: None, size: (80, 40) };
    f.engine.set_view(crate::ViewRequest { viewers: vec![r], free: vec![request], ..Default::default() });
    eventually("the free view in the view", || {
        f.engine.view().free.first().is_some_and(|(_, p)| p.as_ref().is_ok_and(|p| p.tex.size() == (80, 40)))
    });
    f.engine.set_view(crate::ViewRequest::default());
    f.call("node.delete", json!({"path": "/k"}));
    // Orthographic: framing sets the height seen, from the same direction and distance.
    f.call("node.set", json!({"path": "/cam", "params": {"projection": "orthographic"}}));
    let framed = f.call("render.frame", json!({"path": "/r"}));
    let size = framed["size"].as_f64().unwrap();
    // The bounds' sphere (radius 1.5) across the picture's height, a little to spare.
    assert!(size > 2.0 && size < 4.0, "{framed}");
    assert!((framed["position"][1].as_f64().unwrap() - y).abs() < 1e-6, "{framed}");
    let r = f.call("node.snapshot", json!({"path": "/r"}));
    assert!(r["stats"]["max"][3].as_f64().unwrap() > 0.99, "{r}");
    // The free view of an orthographic render starts orthographic and draws its camera as a box.
    let free = f.call("view.free", json!({"path": "/r", "width": 96, "height": 54}));
    assert_eq!((free["home"]["ortho"].as_f64(), free["lines"].clone()), (Some(size), json!(14)), "{free}");
    f.call("node.set", json!({"path": "/cam", "params": {"projection": "perspective"}}));
    // res.analyze waits for the render node to measure what its camera sees of its texture.
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/c", "kind": "tex.constant", "params": {"width": 64, "height": 64}},
            {"op": "create", "path": "/p", "kind": "out.projector"},
            {"op": "connect", "from": "/c", "to": "/r", "input": 1},
            {"op": "connect", "from": "/r", "to": "/p"},
        ]}),
    );
    let node =
        |r: &Json, path: &str| r["nodes"].as_array().unwrap().iter().find(|n| n["path"] == path).cloned();
    let mut report = Json::Null;
    eventually("the texture's need", || {
        report = f.call("res.analyze", json!({}));
        node(&report, "/c").is_some()
    });
    assert_eq!(report["pending"], json!([]), "{report}");
    // A 64 x 36 render on an assumed 1920 x 1080 projector: everything is far too coarse.
    let c = node(&report, "/c").unwrap();
    assert!(c["needed"][0].as_u64().unwrap() > 200 && c["verdict"] == "coarse", "{report}");
    let overlay = f.call("res.overlay", json!({}));
    assert_eq!((overlay["mappers"].as_u64(), overlay["renders"].as_u64()), (Some(0), Some(1)));
    f.call("res.overlay", json!({"on": false}));
    assert!(f.fail("geo.info", json!({"path": "/r"})).contains("Geo"));
}

/// An engine on `dir` (kept when the engine goes), as the app would open it again.
fn open(dir: &std::path::Path) -> (Arc<Engine>, Registry) {
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let session = Arc::new(Session::open(dir, types, &Content::default()).unwrap());
    let engine = Engine::start(session).unwrap();
    let registry = crate::engine_registry(&engine);
    (engine, registry)
}

fn ok(registry: &Registry, method: &str, params: Json) -> Json {
    let r = registry.dispatch(method, &params);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    r["result"].clone()
}

fn warp() -> Option<penumbra_gpu::Gpu> {
    match penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        Ok(g) => Some(g),
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(_) => None,
    }
}

#[test]
fn feedback_and_locked_textures_survive_a_restart() {
    let dir = TempDir::new("state");
    let Some(gpu) = warp() else { return };
    let shots = dir.0.join("shots");
    let (before_fb, before_hold);
    {
        let (engine, r) = open(&dir.0);
        engine.attach_gpu(gpu).unwrap();
        // Operators' state is kept only where the project asks for it.
        let settings = ok(&r, "project.settings", json!({"set": {"persist_state": true}}));
        assert_eq!(settings["settings"][0]["value"], true, "{settings}");
        // A loop that adds 1/16 per frame: black, then brighter until the clock stops.
        let size = json!({"width": 40, "height": 24});
        ok(&r, "node.create", json!({"path": "/black", "kind": "tex.constant", "params": size}));
        let step = json!({"width": 40, "height": 24, "color": [0.0625, 0.03125, 0.0, 1.0]});
        ok(&r, "node.create", json!({"path": "/step", "kind": "tex.constant", "params": step}));
        ok(&r, "node.create", json!({"path": "/fb", "kind": "tex.feedback"}));
        ok(&r, "node.create", json!({"path": "/add", "kind": "tex.composite", "params": {"op": "add"}}));
        ok(&r, "node.connect", json!({"from": "/black", "to": "/fb", "input": "initial"}));
        ok(&r, "node.connect", json!({"from": "/fb", "to": "/add", "input": "layer1"}));
        ok(&r, "node.connect", json!({"from": "/step", "to": "/add", "input": "layer2"}));
        ok(&r, "node.connect", json!({"from": "/add", "to": "/fb", "input": "previous"}));
        ok(&r, "node.watch", json!({"path": "/add"}));
        // The startup restore found nothing; saving is allowed from then on.
        eventually("the startup restore", || engine.state().status().restore.is_some());
        assert_eq!(engine.state().status().restore.unwrap().note.as_deref(), Some("no saved state"));
        eventually("a few frames of the loop", || {
            let s = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("probe.png")}));
            s["stats"]["mean"][0].as_f64().unwrap() > 0.3
        });
        ok(&r, "cook.play", json!({"playing": false}));
        // A locked node that lost its input: only the state file can bring its picture back.
        ok(&r, "node.create", json!({"path": "/hold", "kind": "tex.null"}));
        ok(&r, "node.connect", json!({"from": "/step", "to": "/hold", "input": 0}));
        ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("hold_probe.png")}));
        ok(&r, "node.lock", json!({"path": "/hold"}));
        ok(&r, "node.disconnect", json!({"to": "/hold", "input": 0}));
        before_fb = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("fb_before.png")}));
        before_hold =
            ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("hold_before.png")}));

        let info = ok(&r, "state.info", json!({}));
        let kept: Vec<&str> =
            info["kept_now"].as_array().unwrap().iter().map(|k| k["path"].as_str().unwrap()).collect();
        assert_eq!(kept, ["/fb", "/hold"], "{info}");
        let saved = ok(&r, "state.save", json!({}));
        assert_eq!(saved["nodes"], json!(["/fb", "/hold"]), "{saved}");
        assert!(saved["bytes"].as_u64().unwrap() > 2 * 40 * 24 * 8, "{saved}");
        engine.shutdown();
        // Closing saved once more: the first file became the previous one.
        assert!(dir.0.join("state").join("previous.pnstate").exists());
        assert!(std::fs::read_to_string(dir.0.join("state").join(".gitignore")).unwrap().contains('*'));
    }
    let mean = |s: &Json| s["stats"]["mean"].clone();
    {
        let (engine, r) = open(&dir.0);
        ok(&r, "cook.play", json!({"playing": false}));
        engine.attach_gpu(warp().unwrap()).unwrap();
        eventually("the startup restore", || engine.state().status().restore.is_some());
        let report = engine.state().status().restore.unwrap();
        assert_eq!(report.restored, ["/fb", "/hold"], "{report:?}");
        let fb = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("fb_after.png")}));
        let hold = ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("hold_after.png")}));
        assert_eq!(mean(&fb), mean(&before_fb), "{fb} vs {before_fb}");
        assert_eq!(fb["stats"], before_fb["stats"]);
        assert_eq!(hold["stats"], before_hold["stats"]);
        assert_eq!(
            std::fs::read(shots.join("fb_after.png")).unwrap(),
            std::fs::read(shots.join("fb_before.png")).unwrap()
        );
        // The loop carries on from the restored frame, not from black.
        ok(&r, "node.watch", json!({"path": "/add"}));
        ok(&r, "cook.play", json!({"playing": true}));
        eventually("the loop to move on", || {
            let s = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("probe.png")}));
            s["stats"]["mean"][0].as_f64().unwrap() > before_fb["stats"]["mean"][0].as_f64().unwrap()
        });

        // A damaged latest file: moved aside, the previous one is used.
        ok(&r, "cook.play", json!({"playing": false}));
        let state = dir.0.join("state");
        let latest = std::fs::read(state.join("latest.pnstate")).unwrap();
        std::fs::write(state.join("latest.pnstate"), &latest[..latest.len() / 2]).unwrap();
        let report = ok(&r, "state.restore", json!({}));
        assert!(report["file"].as_str().unwrap().ends_with("previous.pnstate"), "{report}");
        assert!(report["broken"][0][1].as_str().unwrap().contains("truncated"), "{report}");
        assert!(state.join("broken.pnstate").exists());
        // A file that is not one at all, named explicitly: an error, nothing touched.
        std::fs::write(state.join("junk.pnstate"), b"definitely not a state file, just some text").unwrap();
        let reply = r.dispatch("state.restore", &json!({"file": state.join("junk.pnstate")}));
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("not a penumbra state file"), "{reply}");
        assert_eq!(ok(&r, "gpu.info", json!({}))["errors"], 0);

        // Discarding: the loop starts over, the files go, the locked picture stays.
        ok(&r, "state.save", json!({}));
        ok(&r, "state.save", json!({}));
        let done = ok(&r, "state.discard", json!({}));
        assert!(done["done"].as_str().unwrap().contains("started fresh: 1 nodes"), "{done}");
        assert!(state.join("discarded.pnstate").exists());
        assert!(!state.join("previous.pnstate").exists());
        eventually("the loop to start over", || {
            let s = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("probe.png")}));
            s["stats"]["mean"][0].as_f64().unwrap() < before_fb["stats"]["mean"][0].as_f64().unwrap()
        });
        let hold = ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("probe.png")}));
        assert_eq!(hold["stats"], before_hold["stats"]);
        // Off again: closing keeps the locked node only.
        ok(&r, "project.settings", json!({"set": {"persist_state": null}}));
        engine.shutdown();
    }
    {
        let (engine, r) = open(&dir.0);
        engine.attach_gpu(warp().unwrap()).unwrap();
        eventually("the startup restore", || engine.state().status().restore.is_some());
        let report = engine.state().status().restore.unwrap();
        assert_eq!(report.restored, ["/hold"], "{report:?}");
        assert!(report.skipped.is_empty(), "/fb was not written: {report:?}");
        // On, and the loop is kept again.
        ok(&r, "project.settings", json!({"set": {"persist_state": true}}));
        ok(&r, "node.watch", json!({"path": "/add"}));
        eventually("the loop in a snapshot", || {
            ok(&r, "state.save", json!({}))["nodes"] == json!(["/fb", "/hold"])
        });
        engine.shutdown();
    }
    {
        // A fresh start (`--fresh`): the loop's state is left out, the locked node is not.
        let (engine, _r) = open(&dir.0);
        engine.state().start_fresh();
        engine.attach_gpu(warp().unwrap()).unwrap();
        eventually("the startup restore", || engine.state().status().restore.is_some());
        let report = engine.state().status().restore.unwrap();
        assert_eq!(report.restored, ["/hold"], "{report:?}");
        assert_eq!(report.skipped, [("/fb".to_string(), "left out: a fresh start".to_string())]);
        engine.shutdown();
    }
}

#[test]
fn node_reset_starts_a_loop_over_and_leaves_locked_nodes_alone() {
    let dir = TempDir::new("reset");
    let Some(gpu) = warp() else { return };
    let shots = dir.0.join("shots");
    let (engine, r) = open(&dir.0);
    engine.attach_gpu(gpu).unwrap();
    let size = json!({"width": 40, "height": 24});
    ok(&r, "node.create", json!({"path": "/black", "kind": "tex.constant", "params": size}));
    let step = json!({"width": 40, "height": 24, "color": [0.0625, 0.03125, 0.0, 1.0]});
    ok(&r, "node.create", json!({"path": "/step", "kind": "tex.constant", "params": step}));
    ok(&r, "node.create", json!({"path": "/fb", "kind": "tex.feedback"}));
    ok(&r, "node.create", json!({"path": "/add", "kind": "tex.composite", "params": {"op": "add"}}));
    ok(&r, "node.connect", json!({"from": "/black", "to": "/fb", "input": "initial"}));
    ok(&r, "node.connect", json!({"from": "/fb", "to": "/add", "input": "layer1"}));
    ok(&r, "node.connect", json!({"from": "/step", "to": "/add", "input": "layer2"}));
    ok(&r, "node.connect", json!({"from": "/add", "to": "/fb", "input": "previous"}));
    ok(&r, "node.watch", json!({"path": "/add"}));
    eventually("a few frames of the loop", || {
        let s = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("probe.png")}));
        s["stats"]["mean"][0].as_f64().unwrap() > 0.3
    });
    ok(&r, "cook.play", json!({"playing": false}));
    // A locked node holding a picture its input no longer gives.
    ok(&r, "node.create", json!({"path": "/hold", "kind": "tex.null"}));
    ok(&r, "node.connect", json!({"from": "/step", "to": "/hold", "input": 0}));
    ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("probe.png")}));
    ok(&r, "node.lock", json!({"path": "/hold"}));
    ok(&r, "node.disconnect", json!({"to": "/hold", "input": 0}));
    let hold_before = ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("hold.png")}));
    // A component stands for the stateful nodes inside it.
    ok(&r, "node.create", json!({"path": "/c", "kind": "comp.base"}));
    ok(&r, "node.create", json!({"path": "/c/cache", "kind": "tex.cache"}));
    ok(&r, "node.create", json!({"path": "/c/plain", "kind": "tex.null"}));

    let done = ok(&r, "node.reset", json!({"paths": ["/fb", "/hold", "/step", "/c"]}));
    assert_eq!(done["reset"], json!(["/fb", "/c/cache"]), "{done}");
    assert_eq!(done["locked"], json!(["/hold"]), "{done}");
    assert_eq!(done["stateless"], json!(["/step"]), "{done}");
    assert!(done["message"].as_str().unwrap().starts_with("reset /fb, /c/cache"), "{done}");
    // The loop shows its initial input again, though the clock stands still.
    let fb = ok(&r, "node.snapshot", json!({"path": "/fb", "file": shots.join("fb.png")}));
    assert!(fb["stats"]["mean"][0].as_f64().unwrap() < 0.1, "{fb}");
    let hold = ok(&r, "node.snapshot", json!({"path": "/hold", "file": shots.join("hold.png")}));
    assert_eq!(hold["stats"], hold_before["stats"]);
    // One path works too; nothing stateful is not an error, a missing node is.
    let one = ok(&r, "node.reset", json!({"path": "/black"}));
    assert_eq!(one["message"], "nothing to reset; no state: /black", "{one}");
    assert!(
        r.dispatch("node.reset", &json!({"path": "/nope"}))["error"].as_str().unwrap().contains("no node")
    );
    assert_eq!(r.dispatch("node.reset", &json!({}))["ok"], false);
    engine.shutdown();
}

#[test]
fn nothing_is_saved_before_the_saved_state_was_read() {
    let f = Fixture::new("state-hold");
    // No GPU, so the startup restore has not happened: saving would overwrite the file.
    assert!(f.fail("state.save", json!({})).contains("not been read back"));
    assert_eq!(f.call("state.info", json!({}))["status"]["holding"], true);
    // Saying not to restore lifts that; a locked channel node is kept without any GPU.
    f.call("state.config", json!({"restore_on_start": false, "interval": 5}));
    f.call("node.create", json!({"path": "/k", "kind": "chan.constant", "params": {"value0": 2.5}}));
    f.call("node.watch", json!({"path": "/k"}));
    eventually("a cook", || f.call("node.info", json!({"path": "/k"}))["status"]["has_output"] == true);
    f.call("node.lock", json!({"path": "/k"}));
    let saved = f.call("state.save", json!({}));
    assert_eq!(saved["nodes"], json!(["/k"]), "{saved}");
    assert_eq!(f.call("state.info", json!({}))["persist"], false, "off by default");
    assert!(f.fail("project.settings", json!({"set": {"persist": true}})).contains("persist_state"));
    assert!(f.fail("project.settings", json!({"set": {"persist_state": 1}})).contains("true or false"));
    assert!(f.fail("state.config", json!({"interval": 0})).contains("seconds"));
    // Restoring without a GPU is refused by the command (textures could not be uploaded).
    assert!(f.fail("state.restore", json!({})).contains("no GPU"));
    let stats = f.call("cook.stats", json!({"reset": true}));
    assert!(stats["passes"].as_u64().unwrap() > 0, "{stats}");
    let p = f.call("system.process", json!({}));
    assert!(p["process"]["private_bytes"].as_u64().unwrap() > 0, "{p}");
    let m = f.call("cook.meter", json!({"samples": true}));
    assert!(m["passes"].as_u64().unwrap() > 0 && m["smoothed_ms"].as_f64().unwrap() > 0.0, "{m}");
    assert_eq!(m["passes"].as_u64(), m["samples"].as_array().map(|s| s.len() as u64), "{m}");
}

#[path = "osc_tests.rs"]
mod osc_tests;

#[path = "dmx_tests.rs"]
mod dmx_tests;

#[path = "audio_tests.rs"]
mod audio_tests;

#[path = "video_out_tests.rs"]
mod video_out_tests;

#[path = "projector_tests.rs"]
mod projector_tests;

#[path = "scan_capture_tests.rs"]
mod scan_capture_tests;

#[test]
fn copy_and_paste_nodes() {
    let f = Fixture::new("clipboard");
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/lfo", "kind": "chan.lfo"},
            {"op": "create", "path": "/gain", "kind": "chan.math"},
            {"op": "create", "path": "/k", "kind": "chan.constant", "params": {"value0": 2.0}},
            {"op": "connect", "from": "/lfo", "to": "/gain"},
            {"op": "connect", "from": "/k", "to": "/gain", "input": 1},
            {"op": "create", "path": "/scene", "kind": "comp.base"},
        ]}),
    );
    // A locked node's copy holds what the original held, not what its params would make now.
    assert_eq!(f.call("chan.read", json!({"path": "/k"}))["channels"][0]["value"], 2.0);
    f.call("node.lock", json!({"path": "/k"}));
    f.call("node.set", json!({"path": "/k", "params": {"value0": 5.0}}));
    let c = f.call("node.copy", json!({"paths": ["/gain", "/k"]}));
    assert_eq!(c["nodes"], json!(["/gain", "/k"]), "{c}");
    assert_eq!(c["held"], 1, "{c}");
    assert!(c["text"].as_str().unwrap().starts_with("// penumbra nodes"), "{c}");

    let r = f.call("node.paste", json!({}));
    assert_eq!(r["pasted"], json!(["/gain1", "/k1"]), "{r}");
    assert_eq!(r["dropped"], json!([]));
    let gain1 = f.call("node.info", json!({"path": "/gain1"}));
    assert_eq!(gain1["inputs"][0]["from"], "/lfo", "{gain1}");
    assert_eq!(gain1["inputs"][1]["from"], "/k1", "{gain1}");
    assert_eq!(f.call("chan.read", json!({"path": "/k1"}))["channels"][0]["value"], 2.0);

    // Into a component: the wire from /lfo cannot cross its edge and is listed.
    let r = f.call("node.paste", json!({"into": "/scene"}));
    assert_eq!(r["pasted"], json!(["/scene/gain", "/scene/k"]), "{r}");
    assert_eq!(r["dropped"], json!(["/scene/gain input `a` from /lfo"]), "{r}");
    // One undo step each.
    f.call("history.undo", json!({}));
    assert!(f.fail("node.info", json!({"path": "/scene/gain"})).contains("no node"));

    // Cut is copy plus delete, one undo step; the text pastes as given, too.
    let cut = f.call("node.cut", json!({"paths": ["/gain1", "/k1"]}));
    assert!(f.fail("node.info", json!({"path": "/gain1"})).contains("no node"));
    let text = cut["text"].as_str().unwrap().to_string();
    let r = f.call("node.paste", json!({"text": text, "into": "/scene"}));
    assert_eq!(r["pasted"], json!(["/scene/gain1", "/scene/k1"]), "{r}");
    f.call("history.undo", json!({}));
    f.call("history.undo", json!({}));
    f.call("node.info", json!({"path": "/gain1"}));
    assert!(f.fail("node.paste", json!({"text": "hello"})).contains("copied nodes"));
    assert!(f.fail("node.paste", json!({"into": "/k"})).contains("cannot hold"));
    assert!(f.fail("node.copy", json!({"paths": ["/k", "/scene/k"]})).contains("no node"));
}

#[test]
fn another_project_opens_while_the_engine_keeps_running() {
    let f = Fixture::new("switch-a");
    let other = TempDir::new("switch-b");
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    let first = f.engine.session().file().to_path_buf();
    let before = f.engine.view().pass;
    eventually("a few passes", || f.engine.view().pass > before + 2);

    assert!(f.fail("project.open", json!({"dir": other.0})).contains("project.new"));
    let r = f.call("project.new", json!({"dir": other.0}));
    assert_eq!(r["switched"], true);
    assert_eq!(f.engine.generation(), 1);
    // The old project's change was written on the way out, the new one is empty.
    assert!(std::fs::read_to_string(&first).unwrap().contains("/lfo"));
    assert_eq!(f.call("node.list", json!({}))["nodes"], json!([]));
    assert!(f.fail("project.new", json!({"dir": other.0})).contains("project.open"));

    // The cook thread runs for the new project, and pass numbers keep growing.
    let at_switch = f.engine.view().pass;
    assert!(at_switch >= before);
    f.call("node.create", json!({"path": "/n", "kind": "chan.noise"}));
    f.call("node.watch", json!({"path": "/n"}));
    eventually("/n to cook", || {
        f.call("node.info", json!({"path": "/n"}))["status"]["cooks"].as_u64().unwrap_or(0) > 2
    });
    assert!(f.engine.view().pass > at_switch);

    // Back to the first one: its node is there again; opening the open project is a no-op.
    let dir = first.parent().unwrap().to_path_buf();
    assert_eq!(f.call("project.open", json!({"dir": dir}))["switched"], true);
    assert_eq!(f.call("node.list", json!({}))["nodes"][0]["path"], "/lfo");
    assert_eq!(f.call("project.open", json!({"dir": dir}))["switched"], false);
    assert_eq!(f.engine.generation(), 2);
    assert!(std::fs::read_to_string(other.0.join(penumbra_project::FILE_NAME)).unwrap().contains("/n"));
    // No recent file is set in tests, so nothing is listed.
    assert_eq!(f.call("project.recent", json!({}))["recent"], json!([]));
}

#[test]
fn vsyncs_that_pile_up_during_a_slow_pass_cook_once() {
    let f = Fixture::new("vsync-backlog");
    let before = f.engine.on_cook(|st| st.last.pass).unwrap();
    // The cook thread is busy for a while (a slow pass) while an output keeps presenting.
    f.engine.post_cook(|_| std::thread::sleep(Duration::from_millis(300)));
    let t0 = Instant::now();
    for k in 0..100 {
        f.engine.vsync(t0 + Duration::from_secs_f64(f64::from(k) / 60.0));
    }
    // A task sent after them is answered after one pass for all of them, not a hundred.
    let after = f.engine.on_cook(|st| st.last.pass).unwrap();
    assert!(after - before < 5, "{} passes for one backlog", after - before);
}

#[test]
fn the_ticker_rests_while_real_vsyncs_arrive() {
    let f = Fixture::new("ticker-park");
    assert!(f.engine.ticking(), "no output: the ticker paces");
    for _ in 0..5 {
        f.engine.vsync(Instant::now());
        std::thread::sleep(Duration::from_millis(16));
    }
    // A task after the vsyncs runs after the loop has seen them.
    f.engine.on_cook(|_| ()).unwrap();
    assert!(!f.engine.ticking(), "real vsyncs pace: the ticker rests");
    // Three periods without one and the ticker takes over again.
    let until = Instant::now() + Duration::from_secs(2);
    while !f.engine.ticking() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(f.engine.ticking(), "the vsyncs stopped: the ticker paces again");
}

#[test]
fn save_a_copy_and_reopen_a_project_afresh() {
    let f = Fixture::new("copy-a");
    let target = TempDir::new("copy-b");
    let elsewhere = TempDir::new("copy-c");
    let dir = f.engine.session().dir().to_path_buf();
    f.call("node.create", json!({"path": "/lfo", "kind": "chan.lfo"}));
    std::fs::write(dir.join("look.wgsl"), "// a shader").unwrap();
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::fs::write(dir.join("state").join("latest.pnstate"), "latest").unwrap();
    std::fs::write(dir.join("state").join("previous.pnstate"), "previous").unwrap();

    // A folder that holds something, and one inside the project, are refused.
    assert!(f.fail("project.save_as", json!({"dir": dir})).contains("not empty"));
    assert!(f.fail("project.save_as", json!({"dir": dir.join("sub")})).contains("inside"));
    assert_eq!(f.engine.generation(), 0);

    let r = f.call("project.save_as", json!({"dir": target.0}));
    assert_eq!(r["switched"], true);
    assert_eq!(f.engine.generation(), 1);
    assert!(crate::projects::same_dir(f.engine.session().dir(), &target.0));
    assert!(std::fs::read_to_string(target.0.join(penumbra_project::FILE_NAME)).unwrap().contains("/lfo"));
    assert!(target.0.join("look.wgsl").is_file());
    assert_eq!(std::fs::read_to_string(target.0.join("state").join("latest.pnstate")).unwrap(), "latest");
    assert!(!target.0.join("state").join("previous.pnstate").exists());
    assert_eq!(f.call("node.list", json!({}))["nodes"][0]["path"], "/lfo");

    // Reopening the open project afresh: the folder is rewritten after the project closed, so
    // what was written on the way out does not come back.
    f.call("node.create", json!({"path": "/n", "kind": "chan.noise"}));
    let opened = crate::projects::open_fresh(&f.engine, &target.0, |d| {
        assert!(std::fs::read_to_string(d.join(penumbra_project::FILE_NAME)).unwrap().contains("/n"));
        std::fs::write(d.join(penumbra_project::FILE_NAME), "(version: 1)")
            .map_err(|e| penumbra_core::Error::new(e.to_string()))?;
        Ok(Content::default())
    })
    .unwrap();
    assert!(opened.switched);
    assert_eq!(f.engine.generation(), 2);
    assert_eq!(f.call("node.list", json!({}))["nodes"], json!([]));

    // When making the new one fails, the old project runs on.
    f.call("node.create", json!({"path": "/m", "kind": "chan.noise"}));
    let failed =
        crate::projects::open_fresh(&f.engine, &elsewhere.0, |_| Err(penumbra_core::Error::new("no")));
    assert!(failed.is_err());
    assert!(crate::projects::same_dir(f.engine.session().dir(), &target.0));
    assert_eq!(f.engine.generation(), 2);
    f.call("node.watch", json!({"path": "/m"}));
    eventually("/m to cook", || {
        f.call("node.info", json!({"path": "/m"}))["status"]["cooks"].as_u64().unwrap_or(0) > 2
    });
}

#[test]
fn trace_a_mask_into_shapes() {
    let f = Fixture::new("trace");
    let gpu = match penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        Ok(g) => g,
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(_) => return,
    };
    f.engine.attach_gpu(gpu).unwrap();
    // The mask of a rectangle and an ellipse, traced into a second node of the same size.
    f.call(
        "node.create",
        json!({"path": "/src", "kind": "tex.shapes", "params": {"width": 64, "height": 48,
            "shapes": "a rect 0.25,0.25 0.5,0.25\nb ellipse 0.1,0.6 0.3,0.3\n"}}),
    );
    f.call(
        "node.create",
        json!({"path": "/t", "kind": "tex.shapes", "params": {"width": 64, "height": 48,
            "shapes": "# drawn by hand\nkeep rect 0,0 0.1,0.1\n"}}),
    );
    let r = f.call("shapes.trace", json!({"path": "/t", "from": "src", "tag": "book"}));
    assert_eq!(r["shapes"], 2, "{r}");
    // The mask is transparent outside the shapes.
    assert_eq!(r["channel"], "alpha", "{r}");
    let text = |f: &Fixture| {
        let info = f.call("node.info", json!({"path": "/t"}));
        let param =
            info["params"].as_array().unwrap().iter().find(|p| p["name"] == "shapes").unwrap().clone();
        param["value"].as_str().unwrap().to_string()
    };
    let traced = penumbra_ops::shapes::parse(&text(&f)).unwrap();
    assert_eq!(traced.len(), 3);
    assert!(text(&f).starts_with("# drawn by hand\nkeep rect"));
    // The rectangle comes back where it was, in the same surface coordinates.
    let b = traced[1].geom.bounds();
    let want = [0.25, 0.25, 0.5, 0.25];
    assert!(b.iter().zip(want).all(|(a, e)| (a - e).abs() < 0.02), "{b:?}");
    assert_eq!(traced[1].geom.kind(), "poly");
    let e = traced[2].geom.bounds();
    assert!(e.iter().zip([0.1, 0.6, 0.3, 0.3]).all(|(a, w)| (a - w).abs() < 0.03), "{e:?}");

    // Again: the lines with the tag are replaced, not added to; with append they are added.
    f.call("shapes.trace", json!({"path": "/t", "from": "/src", "tag": "book"}));
    assert_eq!(penumbra_ops::shapes::parse(&text(&f)).unwrap().len(), 3);
    let r = f.call("shapes.trace", json!({"path": "/t", "from": "/src", "tag": "book", "append": true}));
    assert_eq!(r["replaced"], 0);
    assert_eq!(penumbra_ops::shapes::parse(&text(&f)).unwrap().len(), 5);
    // One undo step each.
    f.call("history.undo", json!({}));
    assert_eq!(penumbra_ops::shapes::parse(&text(&f)).unwrap().len(), 3);

    // The id output as a label picture: one shape per id, even where shapes touch.
    f.call(
        "node.set",
        json!({"path": "/src", "params": {"shapes": "a rect 0,0 0.5,1\nb rect 0.5,0 0.5,1\n"}}),
    );
    let r = f.call("shapes.trace", json!({"path": "/t", "from": "/src", "output": "mask", "tag": "half"}));
    assert_eq!(r["shapes"], 1, "{r}");
    let r = f.call(
        "shapes.trace",
        json!({"path": "/t", "from": "/src", "output": "id", "channel": "red", "labels": true, "tag": "half"}),
    );
    // Id 0 is the background of a label picture, so only the second half.
    assert_eq!(r["shapes"], 1, "{r}");
    assert_eq!(r["replaced"], 1, "{r}");

    assert!(f.fail("shapes.trace", json!({"path": "/src", "from": "/t", "tag": "2x"})).contains("tag"));
    assert!(f.fail("shapes.trace", json!({"path": "/t", "from": "/nope"})).contains("no node"));
    assert!(f.fail("shapes.trace", json!({"path": "/t", "from": "/t", "channel": "hue"})).contains("one of"));
    f.call("node.create", json!({"path": "/l", "kind": "chan.lfo"}));
    assert!(f.fail("shapes.trace", json!({"path": "/l", "from": "/t"})).contains("not a tex.shapes"));
}

#[test]
fn decode_a_scan_from_pictures_in_a_folder() {
    let f = Fixture::new("scan");
    let r = f.call("scan.patterns", json!({}));
    assert_eq!(r["count"], 46);
    assert_eq!(r["patterns"][3], "column 0 inverse");
    // A camera seeing a 40 x 24 projector at twice its size, shifted by (6, 4) camera pixels, in
    // a 96 x 64 picture: grey 0.1 where the projector does not reach.
    let (pw, ph) = (40u32, 24u32);
    let patterns = penumbra_calib::scan::Patterns::new(pw, ph).unwrap();
    let frames = f.engine.session().dir().join("captures");
    std::fs::create_dir_all(&frames).unwrap();
    for k in 0..patterns.count() {
        let p = patterns.get(k).unwrap();
        let mut rgba = Vec::new();
        for y in 0..64u32 {
            for x in 0..96u32 {
                let (u, v) = ((x as i64 - 6) / 2, (y as i64 - 4) / 2);
                let inside = x >= 6 && y >= 4 && u < pw as i64 && v < ph as i64;
                let lit = inside && patterns.lit(p, u as u32, v as u32);
                let c = if lit { 200 } else { 60 };
                rgba.extend([c, c, c, 255]);
            }
        }
        let png = penumbra_gpu::encode_png_rgba8(96, 64, rgba).unwrap();
        // Numbered without padding: the sort must take 10 after 9.
        std::fs::write(frames.join(format!("shot{k}.png")), png).unwrap();
    }
    let r = f.call("scan.decode", json!({"frames": "captures", "projector": [pw, ph], "fill": 4}));
    assert_eq!(r["camera"], json!([96, 64]), "{r}");
    assert_eq!(r["camera_pixels"]["valid"], 80 * 48, "{r}");
    assert_eq!(r["camera_pixels"]["dark"], 96 * 64 - 80 * 48, "{r}");
    assert_eq!(r["projector_pixels"]["measured"], pw * ph, "{r}");
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    // The maps read back as tex.image would: camera pixel (6, 4) sees projector pixel 0, 0.
    let map = penumbra_gpu::image::decode_file(&frames.join("decoded/cam_to_proj.exr"), 8192).unwrap();
    let at = |x: usize, y: usize, c: usize| map.rgba[(y * 96 + x) * 4 + c].to_f32();
    assert!((at(6, 4, 0) - 0.5 / pw as f32).abs() < 1e-3 && (at(6, 4, 1) - 0.5 / ph as f32).abs() < 1e-3);
    assert_eq!(at(0, 0, 0), -1.0);
    let inv = penumbra_gpu::image::decode_file(&frames.join("decoded/proj_to_cam.exr"), 8192).unwrap();
    assert_eq!((inv.width, inv.height), (pw, ph));
    // Projector pixel 0, 0 is camera pixels 6..8 x 4..6: centre (7, 5).
    assert!((inv.rgba[0].to_f32() - 7.0 / 96.0).abs() < 1e-3, "{}", inv.rgba[0]);
    assert!(frames.join("decoded/scene.png").exists() && frames.join("decoded/quality.png").exists());

    // A picture missing, one of another size, hostile arguments.
    std::fs::remove_file(frames.join("shot23.png")).unwrap();
    assert!(
        f.fail("scan.decode", json!({"frames": "captures", "projector": [pw, ph]})).contains("23 picture")
    );
    let small = penumbra_gpu::encode_png_rgba8(4, 4, vec![0; 64]).unwrap();
    std::fs::write(frames.join("shot23.png"), small).unwrap();
    assert!(f.fail("scan.decode", json!({"frames": "captures", "projector": [pw, ph]})).contains("4 x 4"));
    assert!(f.fail("scan.decode", json!({"frames": "nowhere"})).contains("nowhere"));
    assert!(f.fail("scan.decode", json!({"frames": "captures", "projector": [0, 5]})).contains("projector"));
    assert!(f.fail("scan.decode", json!({"frames": "captures", "fill": 100000})).contains("fill"));
    let e = f.fail("scan.decode", json!({"frames": "captures", "options": {"min_contrast": 7}}));
    assert!(e.contains("min_contrast"), "{e}");
}

#[test]
fn objects_and_blend_from_decoded_scans() {
    let f = Fixture::new("scan_objects");
    let dir = f.engine.session().dir().to_path_buf();
    // One 64 x 48 projector seen 1:1 by a 160 x 48 camera; with the objects, a 24 x 20 box
    // shows 40 camera pixels further right.
    let (pw, ph, cw, ch) = (64usize, 48usize, 160usize, 48usize);
    let in_box = |x: usize, y: usize| (20..44).contains(&x) && (10..30).contains(&y);
    let write_scan = |name: &str, objects: bool| {
        let folder = dir.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let mut p2c = Vec::new();
        for y in 0..ph {
            for x in 0..pw {
                let shift = if objects && in_box(x, y) { 40.0 } else { 0.0 };
                p2c.push([(x as f32 + 0.5 + shift) / cw as f32, (y as f32 + 0.5) / ch as f32, 1.0, 1.0]);
            }
        }
        let mut c2p = Vec::new();
        for y in 0..ch {
            for x in 0..cw {
                c2p.push(if x < pw {
                    [(x as f32 + 0.5) / pw as f32, (y as f32 + 0.5) / ph as f32, 1.0, 1.0]
                } else {
                    [-1.0, -1.0, 0.0, 1.0]
                });
            }
        }
        let exr = |w: usize, h: usize, d: &[[f32; 4]]| {
            penumbra_gpu::image::encode_exr_rgba32f(w as u32, h as u32, d).unwrap()
        };
        std::fs::write(folder.join("proj_to_cam.exr"), exr(pw, ph, &p2c)).unwrap();
        std::fs::write(folder.join("cam_to_proj.exr"), exr(cw, ch, &c2p)).unwrap();
    };
    write_scan("wall", false);
    write_scan("with", true);
    let r =
        f.call("scan.objects", json!({"projectors": [{"reference": "wall", "scan": "with"}], "out": "obj"}));
    assert_eq!(r["objects"].as_array().unwrap().len(), 1, "{r}");
    let px = r["projectors"][0]["pixels"][0].as_u64().unwrap();
    assert!((400..=480).contains(&px), "{r}");
    let labels = penumbra_gpu::image::decode_file_raw(&dir.join("obj/labels_0.png"), 8192, 1 << 24).unwrap();
    assert_eq!(labels.rgba[20 * pw + 32][0] * labels.scale, 1.0);
    assert_eq!(labels.rgba[5 * pw + 5][0], 0.0);
    let cam =
        penumbra_gpu::image::decode_file_raw(&dir.join("obj/objects_camera.png"), 8192, 1 << 24).unwrap();
    assert_eq!((cam.width, cam.height), (cw as u32, ch as u32));
    assert_eq!(cam.rgba[20 * cw + 72][0] * cam.scale, 1.0, "the box in camera space");
    assert!(dir.join("obj/parts_0.png").exists() && dir.join("obj/objects_camera_view.png").exists());

    let r = f.call("scan.blend", json!({"scans": ["with"], "masks": ["obj/labels_0.png"], "out": "obj"}));
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    // Read as tex.image reads it: the weight itself, 1 on the box, 0 outside.
    let w = penumbra_gpu::image::decode_file(&dir.join("obj/weight_0.png"), 8192).unwrap();
    assert!((w.rgba[(20 * pw + 32) * 4].to_f32() - 1.0).abs() < 1e-3);
    assert_eq!(w.rgba[(5 * pw + 5) * 4].to_f32(), 0.0);
    let m = penumbra_gpu::image::decode_file_raw(&dir.join("obj/proj_to_cam_smooth_0.exr"), 8192, 1 << 24)
        .unwrap();
    // Within half a camera pixel (near its edge, a part pulls its positions inwards a little).
    assert!(
        (m.rgba[20 * pw + 32][0] - 72.5 / cw as f32).abs() < 0.5 / cw as f32,
        "{:?}",
        m.rgba[20 * pw + 32]
    );

    assert!(f.fail("scan.blend", json!({"scans": ["with"], "masks": []})).contains("masks"));
    assert!(f.fail("scan.blend", json!({"scans": ["nowhere"]})).contains("nowhere"));
    assert!(f.fail("scan.blend", json!({"scans": ["with"], "ramp": -1})).contains("ramp"));
    assert!(f.fail("scan.objects", json!({"projectors": []})).contains("projectors"));
    let e = f.fail(
        "scan.objects",
        json!({"projectors": [{"reference": "wall", "scan": "with"}], "threshold": "x"}),
    );
    assert!(e.contains("f32"), "{e}");
    let e = f.fail(
        "scan.objects",
        json!({"projectors": [{"reference": "wall", "scan": "with"}], "threshold": -1}),
    );
    assert!(e.contains("threshold"), "{e}");
}
