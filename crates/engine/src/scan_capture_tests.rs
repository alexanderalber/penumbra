//! `scan.capture` and `scan.align` against a simulated camera: two synthetic 40 x 24 projectors
//! light overlapping parts of a 96 x 64 camera picture at two camera pixels per projector pixel;
//! the camera thread of the test renders what the published output frame shows them (stripes,
//! flat black) a little late, as a real camera sees the light.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use penumbra_calib::scan::Patterns;
use penumbra_camera::{CameraFrame, Key, Matrix, PixelFormat};
use penumbra_core::NodeId;
use penumbra_gpu::projection::Stripes;
use serde_json::json;

use super::Fixture;
use crate::Engine;

const W: usize = 96;
const H: usize = 64;
const PW: u32 = 40;
const PH: u32 = 24;
/// Where each projector's pixel 0, 0 lands in the camera.
const OFFSETS: [(usize, usize); 2] = [(6, 4), (14, 8)];
/// How late the camera sees a change.
const LATENCY: Duration = Duration::from_millis(30);

/// A full-range BT.709 4:2:0 grey frame.
fn frame(luma: Vec<u8>) -> CameraFrame {
    let chroma = W.div_ceil(2) * H.div_ceil(2);
    CameraFrame {
        width: W as u32,
        height: H as u32,
        format: PixelFormat::I420,
        y: luma,
        cb: vec![128; chroma],
        cr: vec![128; chroma],
        shift: (1, 1),
        matrix: Matrix::Bt709,
        full_range: true,
        sequence: 0,
        arrived: Instant::now(),
        delay_ms: None,
    }
}

/// What the camera sees of the projectors showing `stripes` (and nothing else).
/// `fine`: a projector pixel is half a camera pixel (each camera pixel averages 2 x 2), so the
/// finest stripes of each axis are a flat grey to the camera. Also returns where a camera pixel
/// sees lit and dark projector pixels at once (where a real camera flickers).
/// `glare`: the camera's left 40 columns see the light twice as bright, so a full level clips there.
fn picture(stripes: &[Option<Stripes>], fine: bool, glare: bool) -> (Vec<u8>, Vec<bool>) {
    let mut luma = vec![40u8; W * H];
    let mut mixed = vec![false; W * H];
    for (s, (ox, oy)) in stripes.iter().zip(OFFSETS) {
        let Some(s) = s else { continue };
        let patterns = Patterns::new(s.size[0], s.size[1]).unwrap();
        let Some(pattern) = patterns.get(s.pattern as usize) else { continue };
        for (i, (v, m)) in luma.iter_mut().zip(mixed.iter_mut()).enumerate() {
            let (x, y) = (i % W, i / W);
            if x < ox || y < oy {
                continue;
            }
            if fine {
                let (u, w) = (((x - ox) * 2) as u32, ((y - oy) * 2) as u32);
                let mut lit = 0;
                for (du, dw) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (pu, pw) = (u + du, w + dw);
                    if pu < s.size[0] && pw < s.size[1] && patterns.lit(pattern, pu, pw) {
                        lit += 1;
                    }
                }
                *v = v.saturating_add((40.0 * f64::from(lit) * f64::from(s.level)) as u8);
                *m |= (1..4).contains(&lit);
                // Moire: the grey of stripes too fine differs a little between pattern and
                // inverse, too little to decode, enough to look like a change.
                if (1..4).contains(&lit) && s.pattern % 2 == 1 {
                    *v = v.saturating_add(20);
                }
                continue;
            }
            let (u, w) = (((x - ox) / 2) as u32, ((y - oy) / 2) as u32);
            if u < s.size[0] && w < s.size[1] && patterns.lit(pattern, u, w) {
                let gain = if glare && x < 40 { 320.0 } else { 160.0 };
                *v = v.saturating_add((gain * s.level) as u8);
            }
        }
    }
    (luma, mixed)
}

/// How the simulated camera misbehaves.
#[derive(Clone, Copy, Default)]
struct Flaws {
    /// Instead of [`LATENCY`].
    latency: Option<Duration>,
    /// Up to this much later than the latency, at random per frame.
    jitter: Duration,
    /// Two frames in every four a noise burst over a random half of the camera pixels that see
    /// stripes finer than themselves (the same in both), as such stripes flicker in a real
    /// camera.
    flicker: bool,
    /// Projector pixels half a camera pixel.
    fine: bool,
    /// Between frames.
    interval: Option<Duration>,
    /// See [`picture`].
    glare: bool,
}

/// Feeds the camera `key` every few milliseconds with what the projectors showed [`LATENCY`]
/// ago, until dropped.
struct Camera {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Camera {
    fn start(engine: Arc<Engine>, key: Key, projectors: Vec<NodeId>) -> Self {
        Self::with(engine, key, projectors, Flaws::default())
    }

    fn with(engine: Arc<Engine>, key: Key, projectors: Vec<NodeId>, flaws: Flaws) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut history: VecDeque<(Instant, Vec<Option<Stripes>>)> = VecDeque::new();
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            let mut random = move || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (seed >> 11) as f64 / (1u64 << 53) as f64
            };
            let mut count = 0u64;
            let mut burst: Vec<bool> = Vec::new();
            while !s.load(Ordering::Relaxed) {
                let now = Instant::now();
                // What the outputs draw: the frame the cook thread published last.
                let out = engine.output();
                let shown: Vec<Option<Stripes>> = projectors
                    .iter()
                    .map(|id| {
                        let p = out.projectors.iter().find(|p| p.node == *id)?;
                        // Without an override a projector shows its content: all lit here.
                        let content = Stripes { pattern: 0, level: 1.0, size: [PW, PH] };
                        if p.flat.is_some() { None } else { Some(p.stripes.unwrap_or(content)) }
                    })
                    .collect();
                history.push_back((now, shown));
                let latency = flaws.latency.unwrap_or(LATENCY);
                let keep = latency + flaws.jitter + Duration::from_millis(50);
                while history.len() > 1 && history.front().is_some_and(|(t, _)| now.duration_since(*t) > keep)
                {
                    history.pop_front();
                }
                let delay = latency + flaws.jitter.mul_f64(random());
                let seen = history
                    .iter()
                    .rev()
                    .find(|(t, _)| now.duration_since(*t) >= delay)
                    .or(history.front())
                    .map(|(_, s)| s.clone())
                    .unwrap_or_default();
                let (mut luma, mixed) = picture(&seen, flaws.fine, flaws.glare);
                count += 1;
                if flaws.flicker && count % 4 == 2 {
                    burst = (0..W * H).map(|_| random() < 0.5).collect();
                }
                if flaws.flicker && count % 4 >= 2 {
                    for ((v, b), m) in luma.iter_mut().zip(&burst).zip(&mixed) {
                        if *b && *m {
                            *v = if *v > 128 { *v - 40 } else { *v + 40 };
                        }
                    }
                }
                engine.camera().feed(&key, frame(luma));
                std::thread::sleep(flaws.interval.unwrap_or(Duration::from_millis(4)));
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
fn scan_capture_and_align_with_a_simulated_camera() {
    let f = Fixture::new("scan-capture");
    let key = Key::new("sim", "");
    // The stream exists (fed) before the node wants it, so no real camera is opened.
    f.engine.camera().feed(&key, frame(vec![40; W * H]));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    // 40 x 24 pixels of the 1920 x 1080 a projector is without a display.
    let viewport = json!([0.0, 0.0, f64::from(PW) / 1920.0, f64::from(PH) / 1080.0]);
    for p in ["/proj_a", "/proj_b"] {
        f.call("node.create", json!({"path": p, "kind": "out.projector", "params": {"viewport": viewport}}));
    }
    // Dimmed by its colour correction: the scan must not be.
    f.call("node.set", json!({"path": "/proj_b", "params": {"matrix": "0.3 0 0\n0 0.3 0\n0 0 0.3"}}));
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let _camera = Camera::start(f.engine.clone(), key, vec![id("/proj_a"), id("/proj_b")]);

    assert!(f.fail("scan.capture", json!({"camera": "/proj_a"})).contains("not a tex.video_in"));
    assert!(f.fail("scan.capture", json!({"camera": "/cam", "level": 2})).contains("level"));
    assert!(
        f.fail("scan.capture", json!({"camera": "/cam", "projectors": ["/cam"]}))
            .contains("not an out.projector")
    );

    let r = f.call(
        "scan.capture",
        json!({"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "frames": true, "settle": 0.0}),
    );
    assert_eq!(r["scans"], json!(["scan/proj_a", "scan/proj_b"]), "{r}");
    let pictures = Patterns::new(PW, PH).unwrap().count();
    for p in r["projectors"].as_array().unwrap() {
        assert_eq!(p["projector"], json!([PW, PH]), "{r}");
        assert_eq!(p["camera"], json!([W, H]), "{r}");
        assert_eq!(p["pictures"], pictures, "{r}");
        // 80 x 48 camera pixels see each projector.
        let valid = p["camera_pixels"]["valid"].as_u64().unwrap();
        assert!((3700..=3840).contains(&valid), "{valid}: {r}");
        assert!(p["projector_pixels"]["measured"].as_u64().unwrap() >= u64::from(PW * PH) * 95 / 100, "{r}");
        assert!(
            p["latency_ms"]["rise"].as_f64().is_some() && p["latency_ms"]["fall"].as_f64().is_some(),
            "{r}"
        );
        assert!(p["margins"]["columns"][0].as_f64().unwrap() > 0.9, "{r}");
        assert!(p["warnings"].as_array().unwrap().is_empty(), "{r}");
    }
    assert!(f.engine.board().state().stripes.is_empty() && f.engine.board().state().flat.is_empty());
    let dir = f.engine.session().dir().to_path_buf();
    for file in ["cam_to_proj.exr", "proj_to_cam.exr", "scene.png", "quality.png"] {
        assert!(dir.join("scan/proj_b").join(file).exists(), "{file}");
    }
    // The frames kept decode alike.
    let frames = std::fs::read_dir(dir.join("scan/proj_a/frames")).unwrap().count();
    assert_eq!(frames, pictures);
    let d = f.call("scan.decode", json!({"frames": "scan/proj_a/frames", "projector": [PW, PH]}));
    assert_eq!(d["camera_pixels"]["valid"], r["projectors"][0]["camera_pixels"]["valid"], "{d}");

    // A scan of B alone: A, overlapping it, shows black meanwhile instead of its content.
    let r = f.call("scan.capture", json!({"camera": "/cam", "projectors": ["/proj_b"], "out": "alone"}));
    let valid = r["projectors"][0]["camera_pixels"]["valid"].as_u64().unwrap();
    assert!((3700..=3840).contains(&valid), "{valid}: {r}");

    // Alignment: A covers camera x 6..86, y 4..52; B x 14..94, y 8..56. Content: x 6..94, y 8..52.
    for m in ["/map_a", "/map_b"] {
        f.call("node.create", json!({"path": m, "kind": "tex.mapper"}));
    }
    let scans = json!(["scan/proj_a", "scan/proj_b"]);
    let report =
        f.call("scan.align", json!({"scans": scans, "mappers": ["/map_a", "/map_b"], "apply": false}));
    assert_eq!(report["applied"], false);
    let r = f.call("scan.align", json!({"scans": scans, "mappers": ["/map_a", "/map_b"]}));
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(r["content"]["camera_turned"], "none", "{r}");
    assert!((r["content"]["aspect"].as_f64().unwrap() - 88.0 / 44.0).abs() < 0.05, "{r}");
    let close = |v: &serde_json::Value, want: &[f64]| {
        want.iter().enumerate().all(|(i, w)| (v[i].as_f64().unwrap() - w).abs() < 0.02)
    };
    let params = |path: &str| {
        let info = f.call("node.info", json!({"path": path}));
        let map: serde_json::Map<String, serde_json::Value> = info["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p["name"].as_str().unwrap().to_string(), p["value"].clone()))
            .collect();
        serde_json::Value::Object(map)
    };
    let a = params("/map_a");
    let b = params("/map_b");
    assert!(close(&a["region"], &[0.0, 0.0, 80.0 / 88.0, 1.0]), "{a}");
    assert!(close(&b["region"], &[8.0 / 88.0, 0.0, 80.0 / 88.0, 1.0]), "{b}");
    assert!(close(&a["corner_tl"], &[0.0, 4.0 / 48.0]) && close(&a["corner_tr"], &[0.0, 4.0 / 48.0]), "{a}");
    assert!(close(&a["corner_br"], &[0.0, 0.0]) && close(&a["corner_bl"], &[0.0, 0.0]), "{a}");
    assert!(close(&b["corner_tl"], &[0.0, 0.0]) && close(&b["corner_tr"], &[0.0, 0.0]), "{b}");
    assert!(
        close(&b["corner_br"], &[0.0, -4.0 / 48.0]) && close(&b["corner_bl"], &[0.0, -4.0 / 48.0]),
        "{b}"
    );
    assert_eq!(a["fit"], "stretch");
    assert_eq!(r["projectors"][0]["place"], 0);
    assert!(r["projectors"][0]["error_px"]["median"].as_f64().unwrap() < 1.0, "{r}");
    // One undo step takes it all back.
    f.call("history.undo", json!({}));
    let a = params("/map_a");
    assert!(close(&a["region"], &[0.0, 0.0, 1.0, 1.0]) && close(&a["corner_tl"], &[0.0, 0.0]), "{a}");

    assert!(
        f.fail("scan.align", json!({"scans": scans, "mappers": ["/map_a"]}))
            .contains("one tex.mapper per scan")
    );
    assert!(
        f.fail("scan.align", json!({"scans": scans, "mappers": ["/map_a", "/cam"]}))
            .contains("not a tex.mapper")
    );
    assert!(f.fail("scan.align", json!({"scans": ["nowhere"]})).contains("nowhere"));
    assert!(f.fail("scan.align", json!({"scans": []})).contains("scans"));

    // A projector the camera does not see is named, and the board is cleared after it.
    f.call(
        "node.create",
        json!({"path": "/proj_c", "kind": "out.projector", "params": {"viewport": viewport}}),
    );
    let e = f.fail("scan.capture", json!({"camera": "/cam", "projectors": ["/proj_c"], "timeout": 0.3}));
    assert!(e.contains("/proj_c, white") && e.contains("did not change"), "{e}");
    assert!(f.engine.board().state().stripes.is_empty() && f.engine.board().state().flat.is_empty());
}

#[test]
fn scan_align_with_a_camera_turned_a_quarter() {
    let f = Fixture::new("scan-align-turned");
    // The two projectors of the test above, seen by a camera on its side: camera pixel (x, y) of
    // the 64 x 96 picture is (y, 63 - x) of the upright one.
    let dir = f.engine.session().dir().to_path_buf();
    for (k, (ox, oy)) in OFFSETS.iter().enumerate() {
        let mut cam = Vec::new();
        for y in 0..W {
            for x in 0..H {
                let (ux, uy) = (y, H - 1 - x);
                let inside =
                    ux >= *ox && uy >= *oy && (ux - ox) / 2 < PW as usize && (uy - oy) / 2 < PH as usize;
                cam.push(if inside {
                    let u = (((ux - ox) / 2) as f32 + 0.5) / PW as f32;
                    let v = (((uy - oy) / 2) as f32 + 0.5) / PH as f32;
                    [u, v, 1.0, 1.0]
                } else {
                    [-1.0, -1.0, 0.0, 1.0]
                });
            }
        }
        let folder = dir.join(format!("turned/{k}"));
        std::fs::create_dir_all(&folder).unwrap();
        let exr = penumbra_gpu::image::encode_exr_rgba32f(H as u32, W as u32, &cam).unwrap();
        std::fs::write(folder.join("cam_to_proj.exr"), exr).unwrap();
        let proj = vec![[0.0f32, 0.0, 1.0, 1.0]; (PW * PH) as usize];
        let exr = penumbra_gpu::image::encode_exr_rgba32f(PW, PH, &proj).unwrap();
        std::fs::write(folder.join("proj_to_cam.exr"), exr).unwrap();
    }
    let r = f.call("scan.align", json!({"scans": ["turned/0", "turned/1"]}));
    assert_eq!(r["applied"], false, "{r}");
    assert_ne!(r["content"]["camera_turned"], "none", "{r}");
    let close = |v: &serde_json::Value, want: &[f64]| {
        want.iter().enumerate().all(|(i, w)| (v[i].as_f64().unwrap() - w).abs() < 0.02)
    };
    let (a, b) = (&r["projectors"][0], &r["projectors"][1]);
    assert!(close(&a["region"], &[0.0, 0.0, 80.0 / 88.0, 1.0]), "{r}");
    assert!(close(&b["region"], &[8.0 / 88.0, 0.0, 80.0 / 88.0, 1.0]), "{r}");
    assert!(close(&a["corners"]["corner_tl"], &[0.0, 4.0 / 48.0]), "{r}");
    assert!(close(&b["corners"]["corner_bl"], &[0.0, -4.0 / 48.0]), "{r}");
}

#[test]
fn scan_capture_keeps_in_step_through_a_flickering_late_camera() {
    let f = Fixture::new("scan-capture-flicker");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(vec![40; W * H]));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    // 80 x 48 projector pixels on 40 x 24 camera pixels: the finest stripes are grey.
    let viewport = json!([0.0, 0.0, f64::from(2 * PW) / 1920.0, f64::from(2 * PH) / 1080.0]);
    f.call(
        "node.create",
        json!({"path": "/proj_a", "kind": "out.projector", "params": {"viewport": viewport}}),
    );
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let a = net.resolve(&"/proj_a".parse().unwrap()).unwrap();
    // Noise bursts that look like changes, a latency that varies by more than a frame.
    let flaws = Flaws {
        latency: Some(Duration::from_millis(120)),
        jitter: Duration::from_millis(20),
        flicker: true,
        fine: true,
        interval: Some(Duration::from_millis(16)),
        ..Flaws::default()
    };
    let _camera = Camera::with(f.engine.clone(), key, vec![a], flaws);
    let r = f.call("scan.capture", json!({"camera": "/cam", "settle": 0.05, "retries": 6}));
    let p = &r["projectors"][0];
    // Every pair of pattern and inverse in step: one picture of the pattern before would shift
    // all after it, and the codes would no longer fit together.
    for axis in ["columns", "rows"] {
        let margins = p["margins"][axis].as_array().unwrap();
        for (bit, m) in margins.iter().enumerate().take(margins.len() - 1) {
            assert!(m.as_f64().unwrap() > 0.8, "{axis} bit {bit}: {r}");
        }
    }
    let valid = p["camera_pixels"]["valid"].as_u64().unwrap();
    assert!(valid >= 800, "{valid} of 960: {r}");
}

#[test]
fn scan_capture_at_two_levels_decodes_where_the_bright_one_clips() {
    let f = Fixture::new("scan-capture-levels");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(vec![40; W * H]));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    let viewport = json!([0.0, 0.0, f64::from(PW) / 1920.0, f64::from(PH) / 1080.0]);
    f.call(
        "node.create",
        json!({"path": "/proj_a", "kind": "out.projector", "params": {"viewport": viewport}}),
    );
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let a = net.resolve(&"/proj_a".parse().unwrap()).unwrap();
    let _camera = Camera::with(f.engine.clone(), key, vec![a], Flaws { glare: true, ..Flaws::default() });
    let valid = |r: &serde_json::Value| r["projectors"][0]["camera_pixels"]["valid"].as_u64().unwrap();

    // The full level alone clips on the camera's left 34 of A's 80 columns.
    let bright = f.call("scan.capture", json!({"camera": "/cam", "out": "bright", "settle": 0.0}));
    let clipped = bright["projectors"][0]["camera_pixels"]["clipped"].as_u64().unwrap();
    assert!(clipped > 1500, "{bright}");
    assert!(valid(&bright) < 2300, "{bright}");

    let r = f
        .call("scan.capture", json!({"camera": "/cam", "levels": [1.0, 0.4], "frames": true, "settle": 0.0}));
    let p = &r["projectors"][0];
    assert!((3700..=3840).contains(&valid(&r)), "{r}");
    assert!(p["camera_pixels"]["clipped"].as_u64().unwrap() < 50, "{r}");
    let levels = p["levels"].as_array().unwrap();
    assert_eq!(levels.len(), 2, "{r}");
    assert_eq!(levels[1]["level"], 0.4);
    // The dim pass decodes everything, but at the lower margin loses the ties to the bright one.
    assert!(levels[1]["only"].as_u64().unwrap() > 1500, "{r}");
    assert!(levels[0]["taken"].as_u64().unwrap() > 1500, "{r}");
    assert!(levels[1]["margins"]["columns"][0].as_f64().unwrap() > 0.9, "{r}");
    assert!(p["apart_px"]["median"].as_f64().unwrap() < 0.5, "{r}");
    assert!(p["warnings"].as_array().unwrap().is_empty(), "{r}");
    assert!(f.engine.board().state().stripes.is_empty() && f.engine.board().state().flat.is_empty());

    // Both passes' frames are kept and decode alike, merged.
    let d = f.call(
        "scan.decode",
        json!({"frames": ["scan/proj_a/frames", "scan/proj_a/frames_1"], "projector": [PW, PH], "out": "redecoded"}),
    );
    assert_eq!(d["camera_pixels"]["valid"], p["camera_pixels"]["valid"], "{d}");
    assert_eq!(d["levels"][1]["only"], levels[1]["only"], "{d}");
    let one = f.call("scan.decode", json!({"frames": ["scan/proj_a/frames_1"], "projector": [PW, PH]}));
    assert!(one.get("levels").is_none(), "{one}");
    assert!(f.fail("scan.decode", json!({"frames": []})).contains("1 to 4"));
}

#[test]
fn scan_align_with_projectors_one_above_the_other() {
    let f = Fixture::new("scan-align-column");
    // A 64 x 96 camera, one camera pixel per projector pixel: A at (10, 6), B at (14, 26), so they
    // overlap by 4 rows and the column runs from row 6 to 50 between columns 14 and 50.
    let (cw, ch) = (64usize, 96usize);
    let dir = f.engine.session().dir().to_path_buf();
    for (k, (ox, oy)) in [(10usize, 6usize), (14, 26)].into_iter().enumerate() {
        let mut cam = Vec::new();
        for y in 0..ch {
            for x in 0..cw {
                let inside = x >= ox && y >= oy && x - ox < PW as usize && y - oy < PH as usize;
                cam.push(if inside {
                    [((x - ox) as f32 + 0.5) / PW as f32, ((y - oy) as f32 + 0.5) / PH as f32, 1.0, 1.0]
                } else {
                    [-1.0, -1.0, 0.0, 1.0]
                });
            }
        }
        let folder = dir.join(format!("column/{k}"));
        std::fs::create_dir_all(&folder).unwrap();
        let exr = penumbra_gpu::image::encode_exr_rgba32f(cw as u32, ch as u32, &cam).unwrap();
        std::fs::write(folder.join("cam_to_proj.exr"), exr).unwrap();
        let proj = vec![[0.0f32, 0.0, 1.0, 1.0]; (PW * PH) as usize];
        let exr = penumbra_gpu::image::encode_exr_rgba32f(PW, PH, &proj).unwrap();
        std::fs::write(folder.join("proj_to_cam.exr"), exr).unwrap();
    }
    let r = f.call("scan.align", json!({"scans": ["column/0", "column/1"]}));
    assert_eq!(r["content"]["layout"], "column", "{r}");
    assert_eq!(r["content"]["camera_turned"], "none", "{r}");
    let close = |v: &serde_json::Value, want: &[f64]| {
        want.iter().enumerate().all(|(i, w)| (v[i].as_f64().unwrap() - w).abs() < 0.02)
    };
    let (a, b) = (&r["projectors"][0], &r["projectors"][1]);
    assert!(close(&a["region"], &[0.0, 0.0, 1.0, 24.0 / 44.0]), "{r}");
    assert!(close(&b["region"], &[0.0, 20.0 / 44.0, 1.0, 24.0 / 44.0]), "{r}");
    // A's share starts 4 columns into it, B's ends 4 columns before its right edge.
    assert!(
        close(&a["corners"]["corner_tl"], &[0.1, 0.0]) && close(&a["corners"]["corner_tr"], &[0.0, 0.0]),
        "{r}"
    );
    assert!(
        close(&b["corners"]["corner_tr"], &[-0.1, 0.0]) && close(&b["corners"]["corner_bl"], &[0.0, 0.0]),
        "{r}"
    );
    assert!((r["content"]["aspect"].as_f64().unwrap() - 36.0 / 44.0).abs() < 0.02, "{r}");
}

/// `scan.run`, the scan button of `tex.scan` nodes: both projectors scanned into the nodes'
/// folders, lined up through the mappers the nodes name, blended into the folders; the nodes then
/// hold the maps. A run can be cancelled and only one runs at a time.
#[test]
fn scan_run_from_tex_scan_nodes_with_a_simulated_camera() {
    let f = Fixture::new("scan-run");
    let key = Key::new("sim", "");
    f.engine.camera().feed(&key, frame(vec![40; W * H]));
    f.call("node.create", json!({"path": "/cam", "kind": "tex.video_in", "params": {"device": "sim"}}));
    let viewport = json!([0.0, 0.0, f64::from(PW) / 1920.0, f64::from(PH) / 1080.0]);
    for (p, m, s) in [("/proj_a", "/map_a", "/scan_a"), ("/proj_b", "/map_b", "/scan_b")] {
        f.call("node.create", json!({"path": p, "kind": "out.projector", "params": {"viewport": viewport}}));
        f.call("node.create", json!({"path": m, "kind": "tex.mapper"}));
        f.call(
            "node.create",
            json!({"path": s, "kind": "tex.scan", "params": {"projector": p, "camera": "cam", "mapper": m, "settle": 0.0}}),
        );
    }
    f.call("cook.play", json!({"playing": true}));
    let net = f.engine.session().doc().snapshot();
    let id = |p: &str| net.resolve(&p.parse().unwrap()).unwrap();
    let _camera = Camera::start(f.engine.clone(), key, vec![id("/proj_a"), id("/proj_b")]);

    let status = f.call("scan.status", json!({}));
    assert_eq!(status["running"], false, "{status}");
    assert_eq!(status["nodes"].as_array().unwrap().len(), 2, "{status}");
    assert!(f.fail("scan.run", json!({"nodes": ["/cam"]})).contains("not a tex.scan"));
    f.call("node.create", json!({"path": "/scan_c", "kind": "tex.scan", "params": {"projector": "/proj_a", "camera": "/proj_b"}}));
    assert!(f.fail("scan.run", json!({})).contains("not a tex.video_in"));
    f.call("node.set", json!({"path": "/scan_c", "params": {"camera": "/cam"}}));
    assert!(f.fail("scan.run", json!({})).contains("both scan /proj_a"));
    f.call("node.delete", json!({"path": "/scan_c"}));

    let r = f.call("scan.run", json!({"wait": true}));
    assert_eq!(r["scans"].as_array().unwrap().len(), 2, "{r}");
    assert!(r["scans"][0]["projector_pixels"].as_u64().unwrap() >= u64::from(PW * PH) * 95 / 100, "{r}");
    assert!((r["align"]["aspect"].as_f64().unwrap() - 88.0 / 44.0).abs() < 0.05, "{r}");
    assert!(r["blend"]["camera_pixels"].as_u64().unwrap() > 0, "{r}");
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    let dir = f.engine.session().dir().to_path_buf();
    for p in ["proj_a", "proj_b"] {
        for file in
            ["proj_to_cam.exr", "cam_to_proj.exr", "scene.png", "weight.png", "proj_to_cam_smooth.exr"]
        {
            assert!(dir.join("scan").join(p).join(file).exists(), "{p}/{file}");
        }
    }
    let region = f.call("node.info", json!({"path": "/map_b"}))["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "region")
        .unwrap()["value"]
        .clone();
    assert!((region[0].as_f64().unwrap() - 8.0 / 88.0).abs() < 0.02, "{region}");
    assert!(f.engine.board().state().stripes.is_empty() && f.engine.board().state().flat.is_empty());
    let status = f.call("scan.status", json!({}));
    for n in status["nodes"].as_array().unwrap() {
        let st = &n["state"];
        assert_eq!(st["running"], false, "{status}");
        assert_eq!(st["finished"], 1, "{status}");
        assert!(st["step"].as_str().unwrap().starts_with("scanned"), "{status}");
        assert!(st["error"].is_null(), "{status}");
    }

    // The node holds the maps: projector size, the weight between 0 and 1.
    if let Ok(gpu) = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
        f.engine.attach_gpu(gpu).unwrap();
        let mut shot = serde_json::Value::Null;
        super::eventually("the scan node's weight", || {
            let r = f.registry.dispatch("node.snapshot", &json!({"path": "/scan_a", "output": "weight"}));
            shot = r["result"].clone();
            r["ok"] == true
        });
        assert_eq!(
            (shot["width"].as_u64(), shot["height"].as_u64()),
            (Some(u64::from(PW)), Some(u64::from(PH)))
        );
        let mean = shot["stats"]["mean"][0].as_f64().unwrap();
        assert!(mean > 0.4 && mean < 1.0, "{shot}");
        let map = f.call("node.snapshot", json!({"path": "/scan_a", "output": "proj_to_cam"}));
        assert_eq!(map["width"].as_u64(), Some(u64::from(PW)), "{map}");
    }

    // Cancelled: stops soon, the projectors show their pictures again, the nodes say so.
    let r = f.call("scan.run", json!({"nodes": ["/scan_a"]}));
    assert_eq!(r["started"], true, "{r}");
    assert!(f.fail("scan.run", json!({"nodes": ["/scan_b"]})).contains("already running"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(f.call("scan.cancel", json!({}))["cancelled"], true);
    super::eventually("the scan to stop", || f.call("scan.status", json!({}))["running"] == false);
    let status = f.call("scan.status", json!({}));
    let a = status["nodes"].as_array().unwrap().iter().find(|n| n["path"] == "/scan_a").unwrap();
    assert_eq!(a["state"]["error"], "cancelled", "{status}");
    assert_eq!(a["state"]["finished"], 2, "{status}");
    assert!(f.engine.board().state().stripes.is_empty() && f.engine.board().state().flat.is_empty());
    assert_eq!(f.call("scan.cancel", json!({}))["cancelled"], false);
}
