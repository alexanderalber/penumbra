//! `projector.flatfield` against a simulated camera: two projectors light overlapping halves of
//! a wall, each with a hot spot and a colour cast across its picture and a black of its own;
//! the camera (with a steep curve of its own) sees what the output board's flat signals show.
//! Synthetic scans tell the command which camera pixel each projector pixel lands on.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use penumbra_calib::colour::decode;
use penumbra_camera::Key;
use penumbra_core::NodeId;
use serde_json::json;

use super::Fixture;
use super::projector_tests::{H, W, frame};
use crate::Engine;

/// Projector pixels across and down.
const PW: usize = 40;
const PH: usize = 32;
/// The camera columns each projector lights (one camera pixel per projector pixel).
const STARTS: [usize; 2] = [0, 24];
/// Each projector's black, as light.
const BLACKS: [[f64; 3]; 2] = [[0.004, 0.004, 0.004], [0.002, 0.003, 0.008]];

/// The light of projector `p` at its pixel `u, v` (0..1) for channel `c` at full drive,
/// relative: a hot spot in the middle, and towards one side less red.
fn field(p: usize, u: f64, v: f64, c: usize) -> f64 {
    let spot = 1.0 - 0.8 * ((u - 0.5).powi(2) + 0.5 * (v - 0.5).powi(2));
    let side = if p == 0 { 1.0 - u } else { u };
    let cast = if c == 0 { 1.0 - 0.25 * side } else { 1.0 };
    spot * cast
}

fn light(p: usize, signal: [f32; 3], u: f64, v: f64) -> [f64; 3] {
    let white = [0.30, 0.45, 0.40];
    [0, 1, 2].map(|c| white[c] * decode(f64::from(signal[c])) * field(p, u, v, c) + BLACKS[p][c])
}

struct Camera {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Camera {
    fn start(engine: Arc<Engine>, key: Key, projectors: [NodeId; 2]) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut sequence = 0;
            while !s.load(Ordering::Relaxed) {
                let arrived = Instant::now();
                let flat = engine.board().state().flat;
                // No room light: every camera pixel is lit by one projector or both.
                let mut pixels = vec![[0.0; 3]; W * H];
                for (p, id) in projectors.iter().enumerate() {
                    let signal = flat.get(id).copied().unwrap_or([0.0; 3]);
                    for (i, px) in pixels.iter_mut().enumerate() {
                        let (x, y) = (i % W, i / W);
                        let Some(col) = x.checked_sub(STARTS[p]).filter(|c| *c < PW) else { continue };
                        let (u, v) = ((col as f64 + 0.5) / PW as f64, (y as f64 + 0.5) / H as f64);
                        for (a, b) in px.iter_mut().zip(light(p, signal, u, v)) {
                            *a += b;
                        }
                    }
                }
                sequence += 1;
                // A camera steeper than sRGB.
                for px in &mut pixels {
                    *px = px.map(|v| (v * 1.6).min(1.0).powf(1.3));
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

/// The scan of projector `p` as `scan.decode` writes it, into `dir/scan_p`.
fn write_scan(dir: &std::path::Path, p: usize) {
    let folder = dir.join(format!("scan_{p}"));
    std::fs::create_dir_all(&folder).unwrap();
    let mut cam = vec![[-1.0f32, -1.0, 0.0, 1.0]; W * H];
    for (i, px) in cam.iter_mut().enumerate() {
        let (x, y) = (i % W, i / W);
        if let Some(col) = x.checked_sub(STARTS[p]).filter(|c| *c < PW) {
            *px = [(col as f32 + 0.5) / PW as f32, (y as f32 + 0.5) / H as f32, 1.0, 1.0];
        }
    }
    let proj: Vec<[f32; 4]> = (0..PW * PH)
        .map(|i| {
            let (x, y) = (i % PW, i / PW);
            [((STARTS[p] + x) as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32, 1.0, 1.0]
        })
        .collect();
    let enc = penumbra_gpu::image::encode_exr_rgba32f;
    std::fs::write(folder.join("cam_to_proj.exr"), enc(W as u32, H as u32, &cam).unwrap()).unwrap();
    std::fs::write(folder.join("proj_to_cam.exr"), enc(PW as u32, PH as u32, &proj).unwrap()).unwrap();
}

/// A map the command wrote, as linear values.
fn read(file: &std::path::Path) -> Vec<[f64; 3]> {
    let raw = penumbra_gpu::image::decode_file_raw(file, 4096, 1 << 22).unwrap();
    assert_eq!((raw.width as usize, raw.height as usize), (PW, PH));
    raw.rgba.iter().map(|p| [0, 1, 2].map(|c| decode(f64::from(p[c])))).collect()
}

#[test]
fn flatfield_evens_out_a_hot_spot_and_a_cast_and_lifts_the_black() {
    let f = Fixture::new("projector-flatfield");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(&vec![[0.0; 3]; W * H], 0, Instant::now()));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    f.call("node.create", json!({"path": "/proj_a", "kind": "out.projector"}));
    f.call("node.create", json!({"path": "/proj_b", "kind": "out.projector"}));
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let _camera = Camera::start(f.engine.clone(), key, [id("/proj_a"), id("/proj_b")]);
    let dir = f.engine.session().dir().to_path_buf();
    write_scan(&dir, 0);
    write_scan(&dir, 1);

    let args = json!({"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "scans": ["scan_0", "scan_1"],
        "settle": 0.0, "frames": 1, "trim": 0.0, "floor": 0.0});
    let bad = |patch: serde_json::Value| {
        let mut a = args.clone();
        for (k, v) in patch.as_object().unwrap() {
            a[k] = v.clone();
        }
        f.fail("projector.flatfield", a)
    };
    assert!(bad(json!({"scans": ["scan_0"]})).contains("one scan folder per projector"));
    assert!(bad(json!({"scans": ["scan_0", "nope"]})).contains("nope"));
    assert!(bad(json!({"levels": 1})).contains("levels"));
    assert!(bad(json!({"even": "sometimes"})).contains("colour"));
    assert!(f.engine.board().state().flat.is_empty());

    let r = f.call("projector.flatfield", args.clone());
    assert_eq!(r["shared_spot"], true, "{r}");
    assert_eq!(r["applied"], true);
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    assert!(f.engine.board().state().flat.is_empty(), "the flat signals are cleared");
    let scale = r["scale"].as_f64().unwrap();
    assert!(scale > 0.4 && scale < 1.0, "{r}");

    // Through its gain map each projector's light is even: within a few per cent of its mean,
    // but for the outermost pixels, which the camera sees half lit and the fit leaves out (the
    // dimmest corner stays a little dim: the gain cannot exceed 1).
    for (p, name) in ["proj_a", "proj_b"].iter().enumerate() {
        let gain = read(&dir.join("flatfield").join(format!("gain_{name}.png")));
        for c in 0..3 {
            let lit: Vec<f64> = (0..PW * PH)
                .filter(|i| (2..PW - 2).contains(&(i % PW)) && (2..PH - 2).contains(&(i / PW)))
                .map(|i| {
                    let (u, v) = (((i % PW) as f64 + 0.5) / PW as f64, ((i / PW) as f64 + 0.5) / PH as f64);
                    gain[i][c] * field(p, u, v, c)
                })
                .collect();
            let mean = lit.iter().sum::<f64>() / lit.len() as f64;
            let worst = lit.iter().fold(0.0f64, |a, v| a.max((v / mean - 1.0).abs()));
            assert!(worst < 0.05, "{name} channel {c}: {worst} off: {r}");
            assert!(gain.iter().all(|g| g[c] <= 1.0 + 1e-6));
        }
    }

    // The black: A lifted where B does not reach (its first 24 columns), not where B does; bluer,
    // as B's black is. Through this camera's steep curve the black reads low, so the lift is
    // well below B's black over A's white (0.02 in blue), as on real hardware.
    let lift_a = read(&dir.join("flatfield/black_proj_a.png"));
    let at = |m: &[[f64; 3]], x: usize, y: usize| m[y * PW + x];
    let alone = at(&lift_a, 5, 16);
    let shared = at(&lift_a, 35, 16);
    assert!(alone[2] > 0.004 && alone[2] < 0.03 && alone[2] > 2.0 * alone[0], "{alone:?} {r}");
    assert!(shared.iter().all(|v| *v < 0.2 * alone[2]), "{shared:?} {alone:?}");

    // The maps are connected through image nodes.
    let net = f.engine.session().doc().snapshot();
    let a = net.node(id("/proj_a")).unwrap();
    let gain_node = a.inputs[1].unwrap().node;
    assert_eq!(net.path_of(gain_node).unwrap().to_string(), "/proj_a_gain");
    // Again: the same image nodes get the files, no new ones.
    let r = f.call(
        "projector.flatfield",
        json!({"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "scans": ["scan_0", "scan_1"],
            "settle": 0.0, "frames": 1, "levels": 3, "even": "colour"}),
    );
    assert_eq!(r["history"]["created"].as_array().unwrap().len(), 0, "{r}");
}
