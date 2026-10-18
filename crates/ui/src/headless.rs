//! Headless UI runs: the real app in an `egui_kittest` harness, rendered offscreen with wgpu,
//! driven by a script of control-channel requests. Works without a window, with a locked
//! screen and in tests; `ui.screenshot` saves what the window would show.
//!
//! The script is JSON lines. Each line is a request (`{"method": ..., "params": {...}}`, any
//! engine or `ui.*` command), or `{"wait": seconds}` to let the show run (thumbnails cook at
//! 10 Hz, a shader compiles in a job). Empty lines and lines starting with `//` are skipped.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use penumbra_engine::{Engine, Registry};
use serde_json::{Value as Json, json};

use crate::app::{AppOptions, PenumbraApp};
use crate::control::{self, Bridge};

/// How long one request may take, synthetic input included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Options {
    /// Window size in points.
    pub size: [f32; 2],
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

/// Runs `script` against `engine` in a headless app. Returns the method and the reply envelope
/// (`{"ok", "result" | "error"}`) of every request; a failing request does not stop the script.
pub fn run(engine: Arc<Engine>, script: &str, options: &Options) -> Result<Vec<(String, Json)>, String> {
    let lines: Vec<Json> = script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| serde_json::from_str(l).map_err(|e| format!("script line `{l}`: {e}")))
        .collect::<Result<_, _>>()?;
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
        .with_step_dt(1.0 / 60.0)
        .wgpu_setup(setup(options.software))
        .build_eframe(|cc| {
            PenumbraApp::new(
                cc,
                app_engine,
                AppOptions {
                    control: Err("headless".into()),
                    requests: Some(requests),
                    bridge: Some(bridge),
                    spacenav: false,
                },
            )
        });
    step_for(&mut harness, Duration::from_millis(300));

    let mut out = Vec::new();
    for line in lines {
        if let Some(s) = line.get("wait").and_then(Json::as_f64) {
            step_for(&mut harness, Duration::from_secs_f64(s.clamp(0.0, 600.0)));
            continue;
        }
        let Some(method) = line.get("method").and_then(Json::as_str).map(str::to_string) else {
            out.push(("skipped".into(), line));
            continue;
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
            step(&mut harness);
            if let Ok(r) = rx.try_recv() {
                break r;
            }
            if started.elapsed() > REQUEST_TIMEOUT {
                break json!({"ok": false, "error": "timed out"});
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        // A few more frames, so the result of the request is drawn.
        for _ in 0..3 {
            step(&mut harness);
        }
        out.push((method, reply));
    }
    harness.state_mut().close();
    Ok(out)
}

fn step(harness: &mut egui_kittest::Harness<'_, PenumbraApp>) {
    if let Some(e) = harness.state_mut().take_synthetic() {
        harness.event(e);
    }
    harness.step();
}

fn step_for(harness: &mut egui_kittest::Harness<'_, PenumbraApp>, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        step(harness);
        std::thread::sleep(Duration::from_millis(8));
    }
}
