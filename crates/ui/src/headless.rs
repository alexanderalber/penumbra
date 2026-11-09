//! Headless UI runs: the real app in an `egui_kittest` harness, rendered offscreen with wgpu,
//! driven by a script of control-channel requests. Works without a window, with a locked
//! screen and in tests; `ui.screenshot` saves what the window would show.
//!
//! The script is JSON lines. Each line is a request (`{"method": ..., "params": {...}}`, any
//! engine or `ui.*` command), `{"wait": seconds}` to let the show run (thumbnails cook at
//! 10 Hz, a shader compiles in a job), `{"wait_for": id, "timeout"?: seconds}` to let it run
//! until the element `id` is on screen (at most `timeout`, 30 s unless given), or
//! `{"resize": [width, height]}` to change the window's size in points, as a maximize would.
//! Empty lines and lines starting with `//` are skipped. `$HERE` in any string of a line
//! stands for the project's folder (forward slashes), so a script kept in a project can name
//! files in it wherever the project lies: `{"method": "node.snapshot", "params": {"path":
//! "/out", "file": "$HERE/snap/out.png"}}`.
//!
//! `wait` takes wall-clock time, so how far the show got in it depends on the machine's load;
//! before acting on an element that appears only once something cooked (the viewer's handles,
//! a helper's list), `wait_for` it.
//!
//! `{"record": {"dir": ..., "fps": 30}}` starts recording numbered PNG frames ([`record`]),
//! `{"record": false}` stops it (the end of the script does too). While recording, the show
//! clock steps by one recorded frame per drawn frame, `wait` and `wait_for` count show time
//! instead of wall-clock time, and every frame a request takes is recorded: a drag of 30
//! steps lasts a second at 30 fps. egui's own time moves by one recorded frame per frame too,
//! so its animations play at their speed in the recording.

mod record;

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use penumbra_engine::{Engine, Registry};
use serde_json::{Value as Json, json};

use crate::app::{AppOptions, PenumbraApp};
use crate::control::{self, Bridge};

/// How long one request may take, synthetic input included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Seconds a `wait_for` line waits unless it says.
const WAIT_FOR_TIMEOUT: f64 = 30.0;

/// One software run at a time in the process. Two WARP devices compiling shaders at once can
/// crash the process inside `d3d10warp.dll`. `penumbra_gpu::Gpu::submit` keeps the engine's
/// submissions apart, but egui renders its frame on the queue directly, so a whole run waits here.
static SOFTWARE_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());

type Harness<'a> = egui_kittest::Harness<'a, PenumbraApp>;

pub struct Options {
    /// Window size in points.
    pub size: [f32; 2],
    /// Pixels per point: 2 draws the same layout with twice the pixels each way.
    pub pixels_per_point: f32,
    /// WARP (or another software adapter) instead of the GPU: slow, but the same everywhere.
    pub software: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { size: [1600.0, 1000.0], pixels_per_point: 1.0, software: false }
    }
}

fn setup(software: bool) -> egui_wgpu::WgpuSetup {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.memory_budget_thresholds = penumbra_gpu::vram::memory_thresholds();
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
    }
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    setup.device_descriptor = Arc::new(penumbra_gpu::device_descriptor);
    let want = if software { wgpu::DeviceType::Cpu } else { wgpu::DeviceType::DiscreteGpu };
    setup.native_adapter_selector = Some(Arc::new(move |adapters: &[wgpu::Adapter], _| {
        let rank = |a: &wgpu::Adapter| {
            let t = a.get_info().device_type;
            if t == want {
                0
            } else if t == wgpu::DeviceType::Cpu {
                2
            } else {
                1
            }
        };
        adapters.iter().min_by_key(|a| rank(a)).cloned().ok_or_else(|| "no GPU adapter".to_string())
    }));
    egui_wgpu::WgpuSetup::CreateNew(setup)
}

/// kittest's wgpu renderer without the cursor kittest adds to every recorded frame: a white
/// triangle at the pointer. The app paints its own arrow when a script asks for one
/// (`ui.pointer`), so a recording with both showed two pointers on top of each other.
struct NoTestCursor(egui_kittest::wgpu::WgpuTestRenderer);

impl NoTestCursor {
    fn new(setup: egui_wgpu::WgpuSetup) -> Self {
        Self(egui_kittest::wgpu::WgpuTestRenderer::from_setup(setup))
    }
}

/// Whether `shape` is the cursor `Harness::render` appends: a closed white triangle with its
/// corners at the pointer, 16 x 8 and 8 x 16 points from it.
fn is_test_cursor(shape: &egui::epaint::ClippedShape) -> bool {
    let egui::Shape::Path(path) = &shape.shape else { return false };
    let [tip, a, b] = path.points.as_slice() else { return false };
    path.closed
        && path.fill == egui::Color32::WHITE
        && *a == *tip + egui::vec2(16.0, 8.0)
        && *b == *tip + egui::vec2(8.0, 16.0)
}

impl egui_kittest::TestRenderer for NoTestCursor {
    fn setup_eframe(&self, cc: &mut eframe::CreationContext<'_>, frame: &mut eframe::Frame) {
        self.0.setup_eframe(cc, frame);
    }

    fn handle_delta(&mut self, delta: &mut egui::TexturesDelta) {
        self.0.handle_delta(delta);
    }

    fn render(&mut self, ctx: &egui::Context, output: &egui::FullOutput) -> Result<image::RgbaImage, String> {
        if output.shapes.last().is_some_and(is_test_cursor) {
            let mut output = output.clone();
            output.shapes.pop();
            return self.0.render(ctx, &output);
        }
        self.0.render(ctx, output)
    }
}

/// A headless app on the software adapter without a script or control channel, for tests that
/// feed it input of their own. The guard keeps other software runs out while it lives.
#[cfg(test)]
pub(crate) fn software_app(engine: Arc<Engine>) -> (std::sync::MutexGuard<'static, ()>, Harness<'static>) {
    let guard = penumbra_core::lock(&SOFTWARE_RUN);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .with_max_steps(4)
        .with_step_dt(1.0 / 60.0)
        .renderer(NoTestCursor::new(setup(true)))
        .build_eframe(|cc| {
            PenumbraApp::new(
                cc,
                engine,
                AppOptions {
                    control: Err("headless".into()),
                    requests: None,
                    bridge: None,
                    spacenav: false,
                    window: None,
                },
            )
        });
    step_for(&mut harness, Duration::from_millis(300));
    (guard, harness)
}

/// Runs `script` against `engine` in a headless app. Returns the method and the reply envelope
/// (`{"ok", "result" | "error"}`) of every request; a failing request does not stop the script.
pub fn run(engine: Arc<Engine>, script: &str, options: &Options) -> Result<Vec<(String, Json)>, String> {
    let mut out = Vec::new();
    run_with(engine, script, options, &mut |method, reply| out.push((method.to_string(), reply.clone())))?;
    Ok(out)
}

/// [`run`], handing each method and reply to `on_reply` as soon as its line is done, so a caller
/// can print the replies while the script runs.
pub fn run_with(
    engine: Arc<Engine>,
    script: &str,
    options: &Options,
    on_reply: &mut dyn FnMut(&str, &Json),
) -> Result<(), String> {
    let _software = options.software.then(|| penumbra_core::lock(&SOFTWARE_RUN));
    let here = engine.session().dir().display().to_string().replace('\\', "/");
    // With their line numbers in the script, for the marks of a recording.
    let lines: Vec<(usize, Json)> = script
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with("//"))
        .map(|(i, l)| {
            let mut v: Json = serde_json::from_str(l).map_err(|e| format!("script line {i} `{l}`: {e}"))?;
            fill_here(&mut v, &here);
            Ok::<_, String>((i, v))
        })
        .collect::<Result<_, _>>()?;
    // egui's time follows the recording's rate, if the script records.
    let mut rates: Vec<u64> = lines.iter().filter_map(|(_, l)| record::fps_of(l)).collect();
    rates.sort_unstable();
    rates.dedup();
    if rates.len() > 1 {
        return Err("every recording of one script must use the same fps".into());
    }
    let step_dt = match rates.first() {
        // `as`: a small whole number, checked against the accepted range.
        Some(fps) if (1..=u64::from(*record::FPS.end())).contains(fps) => 1.0 / *fps as f32,
        _ => 1.0 / 60.0,
    };
    let (bridge, requests) = Bridge::new();
    let mut registry: Registry = penumbra_engine::engine_registry(&engine);
    control::register(&mut registry, &bridge);
    let registry = Arc::new(registry);

    let app_engine = engine.clone();
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(options.size[0], options.size[1]))
        .with_pixels_per_point(options.pixels_per_point)
        // The app always asks for the next frame, so "run until stable" must stop early.
        .with_max_steps(4)
        // Frame time as in the window (kittest's default is 0.25 s), so double-clicks register.
        .with_step_dt(step_dt)
        .renderer(NoTestCursor::new(setup(options.software)))
        .build_eframe(|cc| {
            PenumbraApp::new(
                cc,
                app_engine,
                AppOptions {
                    control: Err("headless".into()),
                    requests: Some(requests),
                    bridge: Some(bridge),
                    spacenav: false,
                    window: None,
                },
            )
        });
    step_for(&mut harness, Duration::from_millis(300));

    let mut out = Vec::new();
    let mut rec: Option<record::Recorder> = None;
    let mut emit = |out: &mut Vec<(String, Json)>| {
        for (method, reply) in out.drain(..) {
            on_reply(&method, &reply);
        }
    };
    for (number, line) in lines {
        let first = rec.as_ref().map_or(0, |r| r.frames);
        let what = line_kind(&line);
        let result = run_line(&engine, &registry, &mut harness, &mut rec, line, &mut out);
        if let Some(r) = rec.as_mut() {
            r.mark(number, &what, first);
        }
        if let Err(e) = result {
            // A recording that cannot go on stops; the frames so far stay.
            let stopped = rec.take().map(record::Recorder::stop);
            out.push(("record".into(), json!({"ok": false, "error": e, "stopped": stopped.map(envelope)})));
        }
        emit(&mut out);
    }
    if let Some(r) = rec.take() {
        out.push(("record".into(), envelope(r.stop())));
    }
    emit(&mut out);
    harness.state_mut().close();
    Ok(())
}

/// What a script line does, for the marks: its method or its kind of line.
fn line_kind(line: &Json) -> String {
    if let Some(m) = line.get("method").and_then(Json::as_str) {
        return m.to_string();
    }
    ["wait", "wait_for", "resize", "record"]
        .into_iter()
        .find(|k| line.get(*k).is_some())
        .unwrap_or("skipped")
        .to_string()
}

fn envelope(r: Result<Json, String>) -> Json {
    match r {
        Ok(v) => json!({"ok": true, "result": v}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// One frame: recorded while recording, else drawn after a short sleep.
fn advance(harness: &mut Harness<'_>, rec: &mut Option<record::Recorder>) -> Result<(), String> {
    match rec {
        Some(r) => r.frame(harness),
        None => {
            step(harness);
            std::thread::sleep(Duration::from_millis(2));
            Ok(())
        }
    }
}

/// `{"record": {dir, fps?}}` or `{"record": false}`.
fn record_line(engine: &Arc<Engine>, rec: &mut Option<record::Recorder>, line: &Json) -> Json {
    let Some(r) = line.get("record") else { return json!({"ok": false, "error": "no `record`"}) };
    if r.as_bool() == Some(false) {
        return match rec.take() {
            Some(r) => envelope(r.stop()),
            None => json!({"ok": false, "error": "nothing is recording"}),
        };
    }
    if rec.is_some() {
        return json!({"ok": false, "error": "already recording; stop it with {\"record\": false} first"});
    }
    let Some(dir) = r.get("dir").and_then(Json::as_str) else {
        return json!({"ok": false, "error": "`record` needs {dir, fps?} or false"});
    };
    let Ok(fps) = u32::try_from(record::fps_of(line).unwrap_or(30)) else {
        return json!({"ok": false, "error": "`fps` is too large"});
    };
    match record::Recorder::start(engine.clone(), Path::new(dir), fps) {
        Ok(r) => {
            *rec = Some(r);
            json!({"ok": true, "result": {"dir": dir, "fps": fps}})
        }
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// Runs one script line. `Err` only when a recording cannot go on.
fn run_line(
    engine: &Arc<Engine>,
    registry: &Arc<Registry>,
    harness: &mut Harness<'_>,
    rec: &mut Option<record::Recorder>,
    line: Json,
    out: &mut Vec<(String, Json)>,
) -> Result<(), String> {
    if line.get("record").is_some() {
        let reply = record_line(engine, rec, &line);
        out.push(("record".into(), reply));
        return Ok(());
    }
    if let Some(s) = line.get("wait").and_then(Json::as_f64) {
        match rec {
            Some(r) => {
                for _ in 0..r.frames_in(s) {
                    r.frame(harness)?;
                }
            }
            None => step_for(harness, Duration::from_secs_f64(s.clamp(0.0, 600.0))),
        }
        return Ok(());
    }
    if let Some(id) = line.get("wait_for").and_then(Json::as_str) {
        let timeout = line.get("timeout").and_then(Json::as_f64).unwrap_or(WAIT_FOR_TIMEOUT);
        let timeout = if timeout.is_finite() { timeout.clamp(0.0, 600.0) } else { WAIT_FOR_TIMEOUT };
        let end = Instant::now() + Duration::from_secs_f64(timeout);
        // Recording, the timeout counts show time as well; the wall-clock one still holds.
        let mut budget = rec.as_ref().map(|r| r.frames_in(timeout));
        while !harness.state().shows(id) && Instant::now() < end && budget != Some(0) {
            advance(harness, rec)?;
            if rec.is_none() {
                std::thread::sleep(Duration::from_millis(6));
            }
            budget = budget.map(|b| b.saturating_sub(1));
        }
        let reply = if harness.state().shows(id) {
            json!({"ok": true, "result": {"id": id}})
        } else {
            json!({"ok": false, "error": format!("no element `{id}` on screen after {timeout} s")})
        };
        out.push(("wait_for".into(), reply));
        return Ok(());
    }
    if let Some(size) = line.get("resize") {
        let [w, h] = [0, 1].map(|i| size.get(i).and_then(Json::as_f64).unwrap_or(0.0) as f32);
        let reply = if !(w.is_finite() && h.is_finite() && w >= 64.0 && h >= 64.0) {
            json!({"ok": false, "error": "`resize` needs [width, height] of 64 or more"})
        } else if rec.is_some() {
            json!({"ok": false, "error": "not while recording: the frames keep one size"})
        } else {
            harness.set_size(egui::vec2(w, h));
            for _ in 0..4 {
                step(harness);
            }
            json!({"ok": true, "result": {"width": w, "height": h}})
        };
        out.push(("resize".into(), reply));
        return Ok(());
    }
    let Some(method) = line.get("method").and_then(Json::as_str).map(str::to_string) else {
        out.push(("skipped".into(), line));
        return Ok(());
    };
    let params = line.get("params").cloned().unwrap_or_else(|| json!({}));
    let (tx, rx) = mpsc::channel();
    let reg = registry.clone();
    let m = method.clone();
    std::thread::Builder::new()
        .name("ui-script".into())
        .spawn(move || {
            tx.send(reg.dispatch(&m, &params)).ok();
        })
        .map_err(|e| format!("cannot start a thread: {e}"))?;
    let started = Instant::now();
    let reply = loop {
        advance(harness, rec)?;
        if let Ok(r) = rx.try_recv() {
            break r;
        }
        if started.elapsed() > REQUEST_TIMEOUT {
            break json!({"ok": false, "error": "timed out"});
        }
    };
    // A few more frames, so the result of the request is drawn.
    for _ in 0..3 {
        advance(harness, rec)?;
    }
    out.push((method, reply));
    Ok(())
}

/// Replaces `$HERE` in every string of `v` (keys stay) by `here`.
pub fn fill_here(v: &mut Json, here: &str) {
    match v {
        Json::String(s) if s.contains("$HERE") => *s = s.replace("$HERE", here),
        Json::Array(items) => items.iter_mut().for_each(|i| fill_here(i, here)),
        Json::Object(map) => map.values_mut().for_each(|i| fill_here(i, here)),
        _ => {}
    }
}

fn step(harness: &mut Harness<'_>) {
    if let Some(e) = harness.state_mut().take_synthetic() {
        harness.event(e);
    }
    harness.step();
}

fn step_for(harness: &mut Harness<'_>, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        step(harness);
        std::thread::sleep(Duration::from_millis(8));
    }
}

#[cfg(test)]
mod cursor_tests {
    use egui::epaint::{ClippedShape, PathShape};
    use egui::{Color32, Rect, Stroke, pos2, vec2};

    fn clipped(shape: egui::Shape) -> ClippedShape {
        ClippedShape { clip_rect: Rect::EVERYTHING, shape }
    }

    #[test]
    fn only_kittests_cursor_triangle_is_dropped() {
        let at = pos2(100.0, 50.0);
        let triangle = |fill| {
            clipped(
                PathShape::convex_polygon(
                    vec![at, at + vec2(16.0, 8.0), at + vec2(8.0, 16.0)],
                    fill,
                    Stroke::new(1.0, Color32::BLACK),
                )
                .into(),
            )
        };
        assert!(super::is_test_cursor(&triangle(Color32::WHITE)));
        assert!(!super::is_test_cursor(&triangle(Color32::RED)));
        let other = PathShape::convex_polygon(
            vec![at, at + vec2(12.0, 8.0), at + vec2(8.0, 16.0)],
            Color32::WHITE,
            Stroke::NONE,
        );
        assert!(!super::is_test_cursor(&clipped(other.into())));
        assert!(!super::is_test_cursor(&clipped(egui::Shape::circle_filled(at, 4.0, Color32::WHITE))));
    }
}

#[cfg(test)]
mod here_tests {
    use serde_json::json;

    #[test]
    fn here_is_filled_in_every_string_but_not_in_keys() {
        let mut line = json!({"method": "node.snapshot", "params": {"file": "$HERE/snap/a.png",
            "list": ["x", "$HERE"], "$HERE": 3, "n": 1}});
        super::fill_here(&mut line, "E:/p");
        assert_eq!(
            line,
            json!({"method": "node.snapshot", "params": {"file": "E:/p/snap/a.png",
                "list": ["x", "E:/p"], "$HERE": 3, "n": 1}})
        );
    }
}
