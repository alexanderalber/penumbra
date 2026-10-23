//! The event loop: forwards everything to eframe (the editor window) and drives the projector
//! output windows next to it (own winit windows, created on the event loop thread, presented from
//! their own threads).
//!
//! It is also where the app ends when the GPU device is lost. Nothing drawn after that is
//! valid, and egui-wgpu panics on the first buffer it cannot write, so every call into eframe
//! is guarded: once the device is lost, or eframe panics, nothing more is forwarded; the
//! engine shuts down (it saves no state from a lost device), a message box says what happened
//! and the process exits with [`EXIT_CRASHED`]. The crash marker makes the next start open
//! paused (`penumbra_engine::crash`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_engine::Engine;
use penumbra_output::Outputs;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents};
use winit::window::WindowId;

/// Filled by the app creator once eframe has its device: the instance that created it, which
/// the output windows need for their surfaces.
pub type InstanceSlot = Arc<Mutex<Option<wgpu::Instance>>>;

/// How long penumbra's submissions wait when an eframe window changes size. eframe configures
/// the surface while it handles the event, and wgpu fails that configure (leaving the window
/// black until the next resize) if another thread submits while it waits for an idle GPU.
const RESIZE_QUIET: Duration = Duration::from_millis(250);

/// The exit code after a lost GPU device or a failed editor window.
pub const EXIT_CRASHED: i32 = 3;

/// How long the shutdown after a crash may take before the process exits without it: a
/// thread stuck in a call on the lost device must not keep a dead app open.
const CRASH_SHUTDOWN: Duration = Duration::from_secs(10);

pub struct Host<'a> {
    pub eframe: eframe::EframeWinitApplication<'a>,
    pub outputs: Outputs,
    pub engine: Arc<Engine>,
    pub instance: InstanceSlot,
    attached: bool,
    /// Set once the app is going down after a crash; nothing is forwarded to eframe any more.
    crashed: bool,
    /// Raw device events are listened to everywhere, not only while focused.
    device_events_always: bool,
}

impl<'a> Host<'a> {
    pub fn new(
        eframe: eframe::EframeWinitApplication<'a>,
        engine: Arc<Engine>,
        instance: InstanceSlot,
    ) -> Self {
        Self {
            eframe,
            outputs: Outputs::new(engine.clone()),
            engine,
            instance,
            attached: false,
            crashed: false,
            device_events_always: false,
        }
    }

    fn attach(&mut self) {
        if self.attached {
            return;
        }
        let instance = lock(&self.instance).clone();
        if let (Some(instance), Some(gpu)) = (instance, self.engine.gpu()) {
            self.outputs.attach_gpu(instance, gpu.clone());
            self.attached = true;
        }
    }

    /// Whether events may still go to eframe; ends the app first if the device was lost.
    fn alive(&mut self) -> bool {
        if self.crashed {
            return false;
        }
        if let Some(why) = self.engine.gpu().and_then(penumbra_gpu::Gpu::lost) {
            self.crash(&format!("The GPU device was lost: {why}"));
            return false;
        }
        true
    }

    /// Runs `f` on eframe unless the app is going down; a panic in it ends the app.
    fn guard(&mut self, f: impl FnOnce(&mut eframe::EframeWinitApplication<'a>)) {
        if !self.alive() {
            return;
        }
        let result = catch_unwind(AssertUnwindSafe(|| f(&mut self.eframe)));
        if let Err(payload) = result {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            let message = match self.engine.gpu().and_then(penumbra_gpu::Gpu::lost) {
                Some(why) => format!("The GPU device was lost: {why}"),
                None => format!("The editor window failed: {what}"),
            };
            self.crash(&message);
        }
    }

    /// Ends the app after a crash: records it for the next start, shuts the engine and the
    /// outputs down, tells the user and exits. Does not return.
    fn crash(&mut self, message: &str) {
        self.crashed = true;
        log::error!("{message}");
        // The lost-device hook has written the marker already; this covers a panic in eframe.
        self.engine.record_crash(message.lines().next().unwrap_or(message));
        let done = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&done);
        let spawned = std::thread::Builder::new().name("crash-watchdog".into()).spawn(move || {
            std::thread::sleep(CRASH_SHUTDOWN);
            if !watched.load(Ordering::Acquire) {
                log::error!("the shutdown after the crash did not finish; exiting without it");
                std::process::exit(EXIT_CRASHED);
            }
        });
        if let Err(e) = spawned {
            log::warn!("no crash watchdog: {e}");
        }
        self.outputs.shutdown();
        self.engine.shutdown();
        done.store(true, Ordering::Release);
        if std::env::var_os("PENUMBRA_NO_CRASH_DIALOG").is_none() {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("penumbra")
                .set_description(format!(
                    "{message}\n\npenumbra has to close. The project file is saved; the next start \
                     opens it with the clock stopped."
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
        // Not through eframe's shutdown: dropping its renderer on a lost device can panic too.
        std::process::exit(EXIT_CRASHED);
    }
}

impl ApplicationHandler<eframe::UserEvent> for Host<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.guard(|e| e.new_events(event_loop, cause));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.guard(|e| e.resumed(event_loop));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.guard(|e| e.user_event(event_loop, event));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        if !self.alive() {
            return;
        }
        if !self.outputs.window_event(window_id, &event) {
            if let WindowEvent::Resized(size) = &event
                && size.width > 0
                && size.height > 0
                && let Some(gpu) = self.engine.gpu()
            {
                gpu.quiesce(Instant::now() + RESIZE_QUIET);
            }
            self.guard(|e| e.window_event(event_loop, window_id, event));
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device_id: DeviceId, event: DeviceEvent) {
        // The wheel for `chan.mouse_in`, turned anywhere (raw input, while one exists).
        if let DeviceEvent::MouseWheel { delta } = &event {
            let mouse = self.engine.mouse();
            if mouse.wanted() {
                mouse.wheel(match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    // Windows reports lines; a pixel delta is taken as 120 to the notch.
                    MouseScrollDelta::PixelDelta(p) => p.y / 120.0,
                });
            }
        }
        self.guard(|e| e.device_event(event_loop, device_id, event));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.guard(|e| e.about_to_wait(event_loop));
        if self.crashed {
            return;
        }
        self.attach();
        // Raw mouse input from everywhere only while a `chan.mouse_in` wants the wheel; otherwise
        // only while a penumbra window has the focus, as winit does by default.
        let wanted = self.engine.mouse().wanted();
        if wanted != self.device_events_always {
            self.device_events_always = wanted;
            event_loop.listen_device_events(if wanted {
                DeviceEvents::Always
            } else {
                DeviceEvents::WhenFocused
            });
        }
        if self.attached {
            self.outputs.tick(event_loop);
        }
        // Wake for the next tick even while the editor is idle or minimised.
        let next = self.outputs.next_tick();
        match event_loop.control_flow() {
            ControlFlow::Wait => event_loop.set_control_flow(ControlFlow::WaitUntil(next)),
            ControlFlow::WaitUntil(t) if t > next => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(next))
            }
            _ => {}
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.guard(|e| e.suspended(event_loop));
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.outputs.shutdown();
        self.guard(|e| e.exiting(event_loop));
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.guard(|e| e.memory_warning(event_loop));
    }
}
