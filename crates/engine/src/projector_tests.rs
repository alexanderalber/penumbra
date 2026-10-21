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

const W: usize = 64;
const H: usize = 32;

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
fn frame(pixels: &[[f64; 3]], sequence: u64, arrived: Instant) -> CameraFrame {
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
    fn start(engine: Arc<Engine>, key: Key, projectors: Vec<NodeId>) -> Self {
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
    let _camera = Camera::start(f.engine.clone(), key, vec![a, b]);

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
        let corr = penumbra_ops::out::correct::correction(|p| n.param(desc, p)).unwrap().unwrap();
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
    let before = f.file();
    let r = f.call("projector.match", json!({"camera": "/cam", "levels": 2, "settle": 0.0, "apply": false}));
    assert_eq!(r["applied"], false);
    assert_eq!(f.file(), before);

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
