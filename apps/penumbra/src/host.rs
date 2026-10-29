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
//!
//! In show mode (`penumbra_engine::show`) the crash ends differently: no message box, and
//! before it exits the app starts itself again with the same arguments plus
//! `--after-crash-of <pid>`; the new process waits for this one to end, then opens the project
//! with the clock running. The show mode file counts these restarts, so a crash that comes
//! back at once stops after a few.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// The arguments to start again with after a crash in show mode.
    restart_args: Vec<String>,
}

impl<'a> Host<'a> {
    pub fn new(
        eframe: eframe::EframeWinitApplication<'a>,
        engine: Arc<Engine>,
        instance: InstanceSlot,
        restart_args: Vec<String>,
    ) -> Self {
        Self {
            eframe,
            outputs: Outputs::new(engine.clone()),
            engine,
            instance,
            attached: false,
            crashed: false,
            device_events_always: false,
            restart_args,
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
        // Decided (and counted) before the shutdown, which may hang on the lost device.
        let restart = self.engine.show_on()
            && self
                .engine
                .show_file()
                .is_some_and(|f| penumbra_engine::show::take_restart(&f, penumbra_engine::show::now_ms()));
        let relaunch = Arc::new(Relaunch {
            args: restart.then(|| self.restart_args.clone()),
            done: AtomicBool::new(false),
        });
        let done = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&done);
        let watch_relaunch = Arc::clone(&relaunch);
        let spawned = std::thread::Builder::new().name("crash-watchdog".into()).spawn(move || {
            std::thread::sleep(CRASH_SHUTDOWN);
            if !watched.load(Ordering::Acquire) {
                log::error!("the shutdown after the crash did not finish; exiting without it");
                watch_relaunch.start();
                std::process::exit(EXIT_CRASHED);
            }
        });
        if let Err(e) = spawned {
            log::warn!("no crash watchdog: {e}");
        }
        self.outputs.shutdown();
        self.engine.shutdown();
        done.store(true, Ordering::Release);
        if restart {
            // No message box: nobody may be there to click it, and the show goes on.
            relaunch.start();
        } else if std::env::var_os("PENUMBRA_NO_CRASH_DIALOG").is_none() {
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

/// Starting the app again after a crash in show mode; once only, from the crash or from the
/// watchdog, whichever gets there first.
struct Relaunch {
    /// `None`: no restart (show mode off, or its restarts used up).
    args: Option<Vec<String>>,
    done: AtomicBool,
}

impl Relaunch {
    fn start(&self) {
        let Some(args) = &self.args else { return };
        if self.done.swap(true, Ordering::AcqRel) {
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                log::error!("show mode cannot start the app again: {e}");
                return;
            }
        };
        let pid = std::process::id().to_string();
        let command = || {
            let mut c = std::process::Command::new(&exe);
            c.args(args).args(["--after-crash-of", pid.as_str()]);
            c
        };
        // Out of a job that would end it together with this process (some terminals and
        // launchers run programs in one), where the job allows that; else as a plain child.
        #[cfg(windows)]
        let spawned = {
            use std::os::windows::process::CommandExt;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            command().creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| command().spawn())
        };
        #[cfg(not(windows))]
        let spawned = command().spawn();
        match spawned {
            Ok(child) => log::warn!("show mode: started the app again (process {})", child.id()),
            Err(e) => log::error!("show mode cannot start the app again: {e}"),
        }
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
            // eframe configures the surface while it handles a resize (a new window gets a few
            // right after it opens), and wgpu fails that configure, leaving the window black
            // until the next resize, if another thread submits while it waits for an idle GPU.
            // So penumbra's submissions wait while eframe handles the event, and no longer.
            let gpu = match &event {
                WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => self.engine.gpu().cloned(),
                _ => None,
            };
            match gpu {
                Some(gpu) => gpu.quiet(|| self.guard(|e| e.window_event(event_loop, window_id, event))),
                None => self.guard(|e| e.window_event(event_loop, window_id, event)),
            }
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
