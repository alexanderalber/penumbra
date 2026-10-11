//! One output window: borderless fullscreen on its display, presented from its own thread on
//! the shared device (decided by measurement), so vsync on one display blocks neither the editor
//! nor another output.
//!
//! Every frame the thread waits for the display (acquiring the next surface texture blocks
//! until a buffer is free, right after a vsync), then draws whatever the cook thread published
//! last. The window that paces the show clock first tells the engine ([`Engine::vsync`]) and
//! waits up to three quarters of a refresh period for the frame that cooks in response, so its
//! picture is one frame old; the others take the latest frame without waiting.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::NodeId;
use penumbra_engine::Engine;
use penumbra_gpu::{Gpu, Projection};
use penumbra_ops::out::{WindowInfo, item};
use winit::window::Window;

/// Frame statistics, reset every [`REPORT`].
const REPORT: Duration = Duration::from_secs(1);
/// Missed frames are not counted this long after the window opens (the display mode settles).
const WARMUP: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Stats {
    first: Option<Instant>,
    last_acquire: Option<Instant>,
    period_start: Option<Instant>,
    period_frames: u64,
    period_max: Duration,
    fps: f64,
    max_ms: f64,
    missed: u64,
    error: Option<String>,
}

impl Stats {
    fn frame(&mut self, now: Instant, refresh: Duration) {
        let first = *self.first.get_or_insert(now);
        if let Some(last) = self.last_acquire {
            let gap = now.saturating_duration_since(last);
            if gap > refresh.mul_f64(1.5) && now.saturating_duration_since(first) > WARMUP {
                self.missed += 1;
            }
            self.period_max = self.period_max.max(gap);
        }
        self.last_acquire = Some(now);
        self.period_frames += 1;
        let start = *self.period_start.get_or_insert(now);
        let span = now.saturating_duration_since(start);
        if span >= REPORT {
            self.fps = (self.period_frames.saturating_sub(1)) as f64 / span.as_secs_f64().max(1e-3);
            self.max_ms = self.period_max.as_secs_f64() * 1000.0;
            self.period_start = Some(now);
            self.period_frames = 1;
            self.period_max = Duration::ZERO;
        }
    }
}

/// What the event loop thread and the presentation thread share.
struct Shared {
    stop: AtomicBool,
    paces: AtomicBool,
    /// Physical size as `w << 32 | h`, from `Resized`.
    size: AtomicU64,
    /// The projector nodes shown in this window.
    nodes: Mutex<Vec<NodeId>>,
    stats: Mutex<Stats>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pack(w: u32, h: u32) -> u64 {
    (u64::from(w) << 32) | u64::from(h)
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, (v & 0xffff_ffff) as u32)
}

pub struct OutputWindow {
    pub window: Arc<Window>,
    /// The display id it was opened for, and the OS name of that display.
    pub display: String,
    pub os_name: String,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl OutputWindow {
    /// Starts presenting into `window` (already created fullscreen on its display).
    pub fn start(
        window: Arc<Window>,
        display: String,
        os_name: String,
        refresh_hz: f64,
        instance: &wgpu::Instance,
        gpu: Gpu,
        engine: Arc<Engine>,
    ) -> Result<Self, String> {
        let surface = instance.create_surface(Arc::clone(&window)).map_err(|e| format!("no surface: {e}"))?;
        let s = window.inner_size();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            paces: AtomicBool::new(false),
            size: AtomicU64::new(pack(s.width, s.height)),
            nodes: Mutex::new(Vec::new()),
            stats: Mutex::new(Stats::default()),
        });
        let refresh = Duration::from_secs_f64(1.0 / refresh_hz.clamp(1.0, 1000.0));
        let thread_shared = Arc::clone(&shared);
        let label = display.clone();
        let thread = std::thread::Builder::new()
            .name(format!("output {display}"))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    present_loop(&surface, &gpu, &engine, &thread_shared, refresh)
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some("the output thread panicked".to_string()),
                };
                if let Some(e) = error {
                    log::error!("output {label}: {e}");
                    lock(&thread_shared.stats).error = Some(e);
                }
            })
            .map_err(|e| format!("cannot start the output thread: {e}"))?;
        Ok(Self { window, display, os_name, shared, thread: Some(thread) })
    }

    pub fn resized(&self, width: u32, height: u32) {
        self.shared.size.store(pack(width, height), Ordering::Relaxed);
    }

    pub fn set_nodes(&self, nodes: Vec<NodeId>) {
        let mut current = lock(&self.shared.nodes);
        if *current != nodes {
            *current = nodes;
        }
    }

    pub fn set_paces(&self, paces: bool) {
        self.shared.paces.store(paces, Ordering::Relaxed);
    }

    pub fn paces(&self) -> bool {
        self.shared.paces.load(Ordering::Relaxed)
    }

    /// Whether the thread has stopped (an error it reported, or a panic).
    pub fn failed(&self) -> Option<String> {
        lock(&self.shared.stats).error.clone()
    }

    pub fn info(&self) -> WindowInfo {
        let (w, h) = unpack(self.shared.size.load(Ordering::Relaxed));
        let stats = lock(&self.shared.stats);
        WindowInfo {
            display: self.display.clone(),
            size: [w, h],
            fps: stats.fps,
            missed: stats.missed,
            max_ms: stats.max_ms,
            paces: self.paces(),
            error: stats.error.clone(),
        }
    }

    /// Stops the thread and waits for it; the window closes when `self` is dropped.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            log::error!("output {}: the thread panicked", self.display);
        }
    }
}

impl Drop for OutputWindow {
    fn drop(&mut self) {
        self.stop();
    }
}

fn present_loop(
    surface: &wgpu::Surface<'static>,
    gpu: &Gpu,
    engine: &Engine,
    shared: &Shared,
    refresh: Duration,
) -> Result<(), String> {
    let caps = surface.get_capabilities(gpu.adapter());
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .or_else(|| caps.formats.first().copied())
        .ok_or("the display's surface does not work with this GPU")?;
    let (mut w, mut h) = unpack(shared.size.load(Ordering::Relaxed));
    let mut config = surface
        .get_default_config(gpu.adapter(), w.max(1), h.max(1))
        .ok_or("the display's surface does not work with this GPU")?;
    config.format = format;
    config.present_mode = wgpu::PresentMode::Fifo;
    config.desired_maximum_frame_latency = 1;
    gpu.configure_surface(surface, &config).map_err(|e| e.0)?;
    let mut drawer = Projection::new(gpu);
    // What a projector showing its test pattern binds as its (unread) texture.
    let blank = penumbra_gpu::Tex::new(gpu, 1, 1, "blank").map_err(|e| e.to_string())?;
    log::info!("output: {}x{} {format:?}, Fifo", config.width, config.height);
    let mut last_pass = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        let (nw, nh) = unpack(shared.size.load(Ordering::Relaxed));
        if (nw, nh) != (w, h) && nw > 0 && nh > 0 {
            (w, h) = (nw, nh);
            config.width = w;
            config.height = h;
            gpu.configure_surface(surface, &config).map_err(|e| e.0)?;
        }
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.configure_surface(surface, &config).map_err(|e| e.0)?;
                continue;
            }
            other => {
                // Occluded (minimised) or a timeout: try again shortly.
                log::debug!("output: no surface texture ({other:?})");
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let now = Instant::now();
        lock(&shared.stats).frame(now, refresh);
        let out = if shared.paces.load(Ordering::Relaxed) {
            engine.vsync(now);
            engine.wait_output(last_pass, refresh.mul_f64(0.75))
        } else {
            engine.output()
        };
        last_pass = out.pass;
        let nodes = lock(&shared.nodes).clone();
        let (fw, fh) = (config.width as f32, config.height as f32);
        let items: Vec<_> = out
            .projectors
            .iter()
            .filter(|p| p.active && nodes.contains(&p.node))
            .filter_map(|p| {
                let [x, y, w, h] = p.viewport.map(|v| v.clamp(0.0, 1.0) as f32);
                item(p, &blank, [x * fw, y * fh, w * fw, h * fh])
            })
            .collect();
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        if let Err(e) = drawer.draw(gpu, &view, format, (config.width, config.height), false, &items) {
            // The pipeline did not build: nothing will ever draw, so stop and report.
            return Err(e.to_string());
        }
        drop(view);
        gpu.present(frame);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_count_missed_frames() {
        let start = Instant::now();
        let refresh = Duration::from_micros(16_667);
        let mut s = Stats::default();
        for i in 0..121u64 {
            // Frame 5 (warm-up, not counted) and frame 90 come two periods late.
            let extra = if i >= 90 {
                4
            } else if i >= 5 {
                2
            } else {
                0
            };
            s.frame(start + refresh * (i + extra) as u32, refresh);
        }
        assert_eq!(s.missed, 1);
        assert!(s.fps > 55.0 && s.fps < 61.0, "{}", s.fps);
        assert!(s.max_ms > 49.0, "{}", s.max_ms);
    }
}
