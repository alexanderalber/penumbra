//! `projector.match` against a simulated camera: two synthetic projectors of different
//! brightness, primaries and tone curves light overlapping halves of a wall; the camera thread
//! of the test renders what they show (the output board's flat signals) into Y'CbCr frames.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use penumbra_calib::colour::{self, Mat3, apply, decode, encode, lookup, luminance};
use penumbra_camera::{CameraFrame, Key, Matrix, PixelFormat};
use penumbra_core::NodeId;
use serde_json::json;

use super::Fixture;
use crate::Engine;

pub(super) const W: usize = 64;
pub(super) const H: usize = 32;

struct Synth {
    p: Mat3,
    gamma: [f64; 3],
    /// Camera columns it lights.
    columns: std::ops::Range<usize>,
}

impl Synth {
    fn light(&self, s: [f32; 3]) -> [f64; 3] {
        let d = [0, 1, 2].map(|c| f64::from(s[c]).clamp(0.0, 1.0).powf(self.gamma[c]));
        apply(&self.p, d)
    }
}

fn synths() -> [Synth; 2] {
    let p = [[0.30, 0.10, 0.05], [0.12, 0.35, 0.04], [0.01, 0.04, 0.32]];
    let narrow = [[0.60, 0.05, 0.02], [0.06, 0.55, 0.05], [0.02, 0.04, 0.62]];
    [
        Synth { p, gamma: [2.2; 3], columns: 0..40 },
        Synth { p: colour::mul(&p, &narrow), gamma: [2.4, 2.0, 2.6], columns: 24..64 },
    ]
}

/// A full-range BT.709 4:4:4 frame of linear RGB per pixel.
pub(super) fn frame(pixels: &[[f64; 3]], sequence: u64, arrived: Instant) -> CameraFrame {
    let (kr, kb) = (0.2126, 0.0722);
    let q = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let (mut y, mut cb, mut cr) = (Vec::new(), Vec::new(), Vec::new());
    for p in pixels {
        let [r, g, b] = p.map(encode);
        let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
        y.push(q(luma));
        cb.push(q((b - luma) / (2.0 * (1.0 - kb)) + 128.0 / 255.0));
        cr.push(q((r - luma) / (2.0 * (1.0 - kr)) + 128.0 / 255.0));
    }
    CameraFrame {
        width: W as u32,
        height: H as u32,
        format: PixelFormat::I420,
        y,
        cb,
        cr,
        shift: (0, 0),
        matrix: Matrix::Bt709,
        full_range: true,
        sequence,
        arrived,
        delay_ms: None,
    }
}

/// Feeds the camera `key` every few milliseconds with what the projectors show, until dropped.
struct Camera {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Camera {
    /// `curve`: the camera's own exponent per channel on the light (1: linear).
    fn start(engine: Arc<Engine>, key: Key, projectors: Vec<NodeId>, curve: [f64; 3]) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let synths = synths();
            let mut sequence = 0;
            while !s.load(Ordering::Relaxed) {
                let arrived = Instant::now();
                let flat = engine.board().state().flat;
                let mut pixels = vec![[0.02; 3]; W * H];
                for (synth, id) in synths.iter().zip(&projectors) {
                    let signal = flat.get(id).copied().unwrap_or([0.0; 3]);
                    let light = synth.light(signal);
                    for (i, p) in pixels.iter_mut().enumerate() {
                        if synth.columns.contains(&(i % W)) {
                            for (a, b) in p.iter_mut().zip(light) {
                                *a += b;
                            }
                        }
                    }
                }
                sequence += 1;
                for p in &mut pixels {
                    *p = [0, 1, 2].map(|c| p[c].powf(curve[c]));
                }
                engine.camera().feed(&key, frame(&pixels, sequence, arrived));
                std::thread::sleep(Duration::from_millis(4));
            }
        });
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn one_measurement_at_a_time_per_engine() {
    use crate::projector::Claim;
    let f = Fixture::new("one-measurement");
    let other = Fixture::new("other-measurement");
    let first = Claim::new(&f.engine, Vec::new()).unwrap();
    let err = Claim::new(&f.engine, Vec::new()).err().unwrap();
    assert!(err.0.contains("already running"), "{}", err.0);
    // Another engine measures on its own.
    assert!(Claim::new(&other.engine, Vec::new()).is_ok());
    drop(first);
    assert!(Claim::new(&f.engine, Vec::new()).is_ok());
}

#[test]
fn projector_match_with_a_simulated_camera() {
    let f = Fixture::new("projector-match");
    let key = Key::new("sim", "");
    // The stream exists (fed) before the node wants it, so no real camera is opened.
    f.engine.camera().feed(&key, frame(&vec![[0.0; 3]; W * H], 0, Instant::now()));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    f.call("node.create", json!({"path": "/proj_a", "kind": "out.projector"}));
    f.call("node.create", json!({"path": "/proj_b", "kind": "out.projector"}));
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let (a, b) = (id("/proj_a"), id("/proj_b"));
    let _camera = Camera::start(f.engine.clone(), key, vec![a, b], [1.0; 3]);

    assert!(f.fail("projector.match", json!({"camera": "/proj_a"})).contains("not a tex.video_in"));
    assert!(f.fail("projector.match", json!({"camera": "/cam", "levels": 1})).contains("levels"));
    assert!(
        f.fail("projector.match", json!({"camera": "/cam", "projectors": ["/cam"]}))
            .contains("not an out.projector")
    );

    let r = f.call(
        "projector.match",
        json!({"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "levels": 6, "settle": 0.0}),
    );
    assert_eq!(r["shared_spot"], true, "{r}");
    assert_eq!(r["reference"], "/proj_b", "{r}");
    assert_eq!(r["applied"], true);
    assert_eq!(r["target"], "reference");
    let straight = (0..17).map(|i| format!("{}", f64::from(i) / 16.0)).collect::<Vec<_>>().join(" ");
    assert!(r["projectors"][1]["curves"].as_str().unwrap().starts_with(&format!("r: {straight}")), "{r}");
    assert!(f.fail("projector.match", json!({"camera": "/cam", "target": "nope"})).contains("srgb"));
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    let peak = &r["projectors"][0]["peak"];
    assert!(peak["strength_1"].as_f64().unwrap() < peak["strength_0_5"].as_f64().unwrap(), "{r}");
    assert!(f.engine.board().state().flat.is_empty(), "the flat signals are cleared");

    // The params written make the two synthetic projectors show the same light.
    let net = f.engine.session().doc().snapshot();
    let desc = f.engine.types().desc(penumbra_ops::out::PROJECTOR).unwrap();
    let synths = synths();
    let corrected = |node: NodeId, synth: &Synth, c: [f64; 3]| {
        let n = net.node(node).unwrap();
        // None: the identity (the reference with target reference).
        let corr =
            penumbra_ops::out::correct::correction(|p| n.param(desc, p)).unwrap().unwrap_or_else(|| {
                Arc::new(penumbra_gpu::projection::Correction {
                    matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    curves: [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]],
                })
            });
        let m = corr.matrix.map(|r| r.map(f64::from));
        let d = apply(&m, c).map(|v| v.clamp(0.0, 1.0));
        let s = [0, 1, 2].map(|i| {
            let table: Vec<f64> = corr.curves[i].iter().map(|v| f64::from(*v)).collect();
            lookup(&table, encode(d[i])) as f32
        });
        synth.light(s)
    };
    let white = luminance(corrected(a, &synths[0], [1.0; 3]));
    let mut worst: f64 = 0.0;
    for c in [
        [1.0, 1.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.2, 0.2, 0.2],
        [1.0, 0.0, 0.0],
        [0.0, 0.7, 0.0],
        [0.1, 0.3, 0.9],
        [decode(0.25); 3],
    ] {
        let la = corrected(a, &synths[0], c);
        let lb = corrected(b, &synths[1], c);
        for (x, y) in la.iter().zip(lb) {
            worst = worst.max((x - y).abs() / white);
        }
    }
    assert!(worst < 0.04, "worst difference {worst} of white: {r}");

    // Not applied: the params stay.
    let before = f.engine.session().doc().snapshot();
    let r = f.call("projector.match", json!({"camera": "/cam", "levels": 2, "settle": 0.0, "apply": false}));
    assert_eq!(r["applied"], false);
    let after = f.engine.session().doc().snapshot();
    let text =
        |net: &penumbra_graph::Network, id: NodeId| net.node(id).unwrap().param(desc, "matrix").cloned();
    assert_eq!(text(&before, a), text(&after, a));
    assert_eq!(text(&before, b), text(&after, b));

    // A projector the camera does not see is named.
    f.call("node.create", json!({"path": "/proj_c", "kind": "out.projector"}));
    let e = f.fail("projector.match", json!({"camera": "/cam", "levels": 2, "settle": 0.0}));
    assert!(e.contains("/proj_c lights no camera pixel"), "{e}");
    assert!(f.engine.board().state().flat.is_empty(), "cleared after an error too");

    // A camera that stalls times out.
    f.engine.camera().feed(&Key::new("dead", ""), frame(&vec![[0.0; 3]; W * H], 1, Instant::now()));
    f.call("node.create", json!({"path": "/dead", "kind": "tex.video_in", "params": {"device": "dead"}}));
    let e = f.fail(
        "projector.match",
        json!({"camera": "/dead", "projectors": ["/proj_a"], "settle": 0.0, "timeout": 0.3}),
    );
    assert!(e.contains("sent no frame"), "{e}");
    assert!(f.engine.board().state().flat.is_empty());
}

#[test]
fn projector_match_refines_through_a_nonlinear_camera() {
    let f = Fixture::new("projector-refine");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(&vec![[0.0; 3]; W * H], 0, Instant::now()));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    f.call("node.create", json!({"path": "/proj_a", "kind": "out.projector"}));
    f.call("node.create", json!({"path": "/proj_b", "kind": "out.projector"}));
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let (a, b) = (id("/proj_a"), id("/proj_b"));
    // A camera much steeper than linear, differently per channel.
    let curve = [1.5, 2.0, 2.5];
    let _camera = Camera::start(f.engine.clone(), key, vec![a, b], curve);

    let r = f.call("projector.match", json!({"camera": "/cam", "levels": 6, "settle": 0.0}));
    let reference = r["reference"].as_str().unwrap().to_string();
    let other = if reference == "/proj_a" { 1 } else { 0 };
    let rounds =
        r["projectors"][other]["refine"]["rounds"].as_array().cloned().unwrap_or_else(|| panic!("{r}"));
    assert!(rounds.len() >= 2, "{r}");
    let off = |round: &serde_json::Value| {
        (0..3).map(|c| (round["ratio"][c].as_f64().unwrap() - 1.0).abs()).fold(0.0, f64::max)
    };
    let first = off(&rounds[0]);
    let best = rounds.iter().map(off).fold(f64::MAX, f64::min);
    assert!(first > 0.05, "the camera's curve should mislead the first estimate: {r}");
    assert!(best < 0.02, "refined to {best}: {r}");

    // Check against the simulation: the camera reads full red, green, blue alike through both.
    let net = f.engine.session().doc().snapshot();
    let desc = f.engine.types().desc(penumbra_ops::out::PROJECTOR).unwrap();
    let synths = synths();
    let read = |node: NodeId, synth: &Synth, content: [f64; 3]| {
        let n = net.node(node).unwrap();
        let text = |p: &str| n.param(desc, p).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let m = penumbra_ops::out::correct::parse_matrix(&text("matrix")).unwrap();
        let curves = penumbra_ops::out::correct::parse_curves(&text("curves"))
            .unwrap()
            .unwrap_or_else(|| [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]]);
        let s = colour::signal(&m, &curves, content).map(|v| v as f32);
        let light = synth.light(s);
        [0, 1, 2].map(|c| (0.02 + light[c]).powf(curve[c]) - 0.02_f64.powf(curve[c]))
    };
    let (wa, wb) = (read(a, &synths[0], [1.0; 3]), read(b, &synths[1], [1.0; 3]));
    for c in 0..3 {
        // 8-bit camera codes: a code is several per cent of a dark blue.
        assert!((wa[c] / wb[c] - 1.0).abs() < 0.04, "white, channel {c}: {wa:?} vs {wb:?}: {r}");
    }
    let check = &r["projectors"][other]["refine"]["check"];
    for name in ["red", "green", "blue"] {
        assert!(check[name]["ratio"].is_array(), "{r}");
    }
    // The greys refined read alike; one between them nearly so.
    let off = |name: &str| {
        (0..3).map(|c| (check[name]["ratio"][c].as_f64().unwrap() - 1.0).abs()).fold(0.0, f64::max)
    };
    for name in ["grey 0.5", "grey 0.75"] {
        assert!(off(name) < 0.03, "{name}: {}: {r}", off(name));
    }
    // Dark greys through a steep camera: a code is many per cent, so judged in codes.
    let codes = |name: &str| {
        (0..3).map(|c| check[name]["residual_codes"][c].as_f64().unwrap().abs()).fold(0.0, f64::max)
    };
    for name in ["grey 0.25", "grey 0.375"] {
        assert!(codes(name) <= 2.5, "{name}: {} codes: {r}", codes(name));
    }
    assert!(r["projectors"][other]["refine"]["grey_rounds"].as_array().unwrap().len() >= 2, "{r}");
    assert!(f.engine.board().state().flat.is_empty());
    assert!(f.fail("projector.match", json!({"camera": "/cam", "refine": 11})).contains("refine"));
}

#[test]
fn projector_match_saves_measurements_and_matches_from_them() {
    let f = Fixture::new("projector-stored");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(&vec![[0.0; 3]; W * H], 0, Instant::now()));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    f.call("node.create", json!({"path": "/proj_a", "kind": "out.projector"}));
    f.call("node.create", json!({"path": "/proj_b", "kind": "out.projector"}));
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let (a, b) = (id("/proj_a"), id("/proj_b"));
    let curve = [1.5, 2.0, 2.5];
    let _camera = Camera::start(f.engine.clone(), key, vec![a, b], curve);

    let r = f.call(
        "projector.match",
        json!({"camera": "/cam", "levels": 6, "settle": 0.0, "save": "measured", "note": "Cinema, Eco"}),
    );
    let files = r["saved"]["files"].as_array().unwrap_or_else(|| panic!("{r}"));
    assert_eq!(files.len(), 2, "{r}");
    let dir = f.engine.session().dir().join("measured");
    let stored = crate::projector_stored::read(&dir.join("proj_a.json")).unwrap();
    assert_eq!(stored.projector, "/proj_a");
    assert_eq!(stored.note, "Cinema, Eco");
    assert_eq!(stored.camera.device, "sim");
    assert_eq!(stored.greys.as_ref().map(Vec::len), Some(6));
    assert!(stored.shared_spot);
    let check = r["saved"]["stored_match_against_this"].as_array().unwrap_or_else(|| panic!("{r}"));
    assert_eq!(check.len(), 2, "{r}");

    // Without the camera: the files alone.
    f.call("node.create", json!({"path": "/later", "kind": "comp.base"}));
    f.call("node.create", json!({"path": "/later/proj_a", "kind": "out.projector"}));
    let r = f.call(
        "projector.match",
        json!({"from": {"/proj_a": "measured/proj_a.json", "/proj_b": "measured/proj_b.json"}}),
    );
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(r["from"], "stored");
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    // Through the simulated camera both read the same greys.
    let net = f.engine.session().doc().snapshot();
    let desc = f.engine.types().desc(penumbra_ops::out::PROJECTOR).unwrap();
    let synths = synths();
    let read = |node: NodeId, synth: &Synth, content: [f64; 3]| {
        let n = net.node(node).unwrap();
        let text = |p: &str| n.param(desc, p).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let m = penumbra_ops::out::correct::parse_matrix(&text("matrix")).unwrap();
        let curves = penumbra_ops::out::correct::parse_curves(&text("curves"))
            .unwrap()
            .unwrap_or_else(|| [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]]);
        let s = colour::signal(&m, &curves, content).map(|v| v as f32);
        let light = synth.light(s);
        [0, 1, 2].map(|c| (0.02 + light[c]).powf(curve[c]) - 0.02_f64.powf(curve[c]))
    };
    for g in [0.25, 0.5, 1.0] {
        let (ga, gb) = (read(a, &synths[0], [g; 3]), read(b, &synths[1], [g; 3]));
        let white = read(b, &synths[1], [1.0; 3]);
        for c in 0..3 {
            // Within 8 %, or a per cent of white where the camera sees hardly anything.
            let off = (ga[c] - gb[c]).abs();
            assert!(
                off < 0.08 * gb[c] || off < 0.01 * white[c],
                "grey {g}, channel {c}: {ga:?} vs {gb:?}: {r}"
            );
        }
    }

    // A folder: `<node name>.json` per projector, here for a node elsewhere in the network.
    let r = f.call(
        "projector.match",
        json!({"from": "measured", "projectors": ["/later/proj_a", "/proj_b"], "apply": false}),
    );
    assert_eq!(r["applied"], false, "{r}");

    // Refused: both sources, camera-only settings, missing and broken files.
    assert!(f.fail("projector.match", json!({})).contains("`camera`"));
    assert!(f.fail("projector.match", json!({"camera": "/cam", "from": "measured"})).contains("not both"));
    assert!(f.fail("projector.match", json!({"from": "measured", "levels": 4})).contains("`levels`"));
    assert!(f.fail("projector.match", json!({"from": {"/proj_a": "nowhere.json"}})).contains("cannot read"));
    assert!(
        f.fail(
            "projector.match",
            json!({"from": {"/proj_a": "measured/proj_a.json"}, "projectors": ["/proj_b"]})
        )
        .contains("names no file for /proj_b")
    );
    std::fs::write(dir.join("broken.json"), "{\"format\": \"something else\"}").unwrap();
    assert!(
        f.fail("projector.match", json!({"from": {"/proj_a": "measured/broken.json"}}))
            .contains("not a projector measurement")
    );
    // Files of different runs, one without greys: combined, with warnings.
    let mut old = stored.clone();
    old.run = "older".into();
    old.greys = None;
    std::fs::write(dir.join("old.json"), serde_json::to_string(&old).unwrap()).unwrap();
    let r = f.call(
        "projector.match",
        json!({"from": {"/proj_a": "measured/old.json", "/proj_b": "measured/proj_b.json"}, "apply": false}),
    );
    let warnings = r["warnings"].to_string();
    assert!(warnings.contains("different runs") && warnings.contains("without greys"), "{r}");
}

#[test]
fn projector_test_switches_every_projector() {
    let f = Fixture::new("projector-test");
    assert!(f.fail("projector.test", json!({"on": true})).contains("no out.projector"));
    f.call("node.create", json!({"path": "/proj_a", "kind": "out.projector"}));
    f.call("node.create", json!({"path": "/proj_b", "kind": "out.projector", "params": {"active": false}}));
    f.call("node.create", json!({"path": "/noise", "kind": "tex.noise"}));
    let read = |path: &str| {
        let net = f.engine.session().doc().snapshot();
        let desc = f.engine.types().desc(penumbra_ops::out::PROJECTOR).unwrap();
        let n = net.node(net.resolve(&path.parse().unwrap()).unwrap()).unwrap().clone();
        let test = n.param(desc, "test").and_then(|v| v.as_bool()).unwrap();
        let pattern = n.param(desc, "test_pattern").and_then(|v| v.as_str()).unwrap().to_string();
        (test, pattern)
    };
    assert_eq!(read("/proj_a"), (false, "align".to_string()));

    let r = f.call("projector.test", json!({"on": true, "pattern": "focus"}));
    assert_eq!(r["projectors"], json!(["/proj_a", "/proj_b"]), "{r}");
    assert_eq!(r["history"]["history"], "new", "one undo step: {r}");
    assert_eq!(read("/proj_a"), (true, "focus".to_string()));
    assert_eq!(read("/proj_b"), (true, "focus".to_string()));

    // Off keeps the pattern chosen; a list switches only those.
    f.call("projector.test", json!({"on": false}));
    assert_eq!(read("/proj_b"), (false, "focus".to_string()));
    f.call("projector.test", json!({"on": true, "pattern": "align", "projectors": ["/proj_b"]}));
    assert_eq!(read("/proj_a"), (false, "focus".to_string()));
    assert_eq!(read("/proj_b"), (true, "align".to_string()));

    assert!(f.fail("projector.test", json!({"on": true, "pattern": "grid"})).contains("align or focus"));
    assert!(
        f.fail("projector.test", json!({"on": true, "projectors": ["/noise"]}))
            .contains("not an out.projector")
    );
    assert!(f.fail("projector.test", json!({"pattern": "focus"})).contains("on"));
}
