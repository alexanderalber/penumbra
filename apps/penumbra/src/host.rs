//! The event loop: forwards everything to eframe (the editor window) and drives the projector
//! output windows next to it (own winit windows, created on the event loop thread, presented from
//! their own threads).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use penumbra_engine::Engine;
use penumbra_output::Outputs;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

/// Filled by the app creator once eframe has its device: the instance that created it, which
/// the output windows need for their surfaces.
pub type InstanceSlot = Arc<Mutex<Option<wgpu::Instance>>>;

/// How long penumbra's submissions wait when an eframe window changes size. eframe configures
/// the surface while it handles the event, and wgpu fails that configure (leaving the window
/// black until the next resize) if another thread submits while it waits for an idle GPU.
const RESIZE_QUIET: Duration = Duration::from_millis(250);

pub struct Host<'a> {
    pub eframe: eframe::EframeWinitApplication<'a>,
    pub outputs: Outputs,
    pub engine: Arc<Engine>,
    pub instance: InstanceSlot,
    attached: bool,
}

impl<'a> Host<'a> {
    pub fn new(
        eframe: eframe::EframeWinitApplication<'a>,
        engine: Arc<Engine>,
        instance: InstanceSlot,
    ) -> Self {
        Self { eframe, outputs: Outputs::new(engine.clone()), engine, instance, attached: false }
    }

    fn attach(&mut self) {
        if self.attached {
            return;
        }
        let instance = self.instance.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let (Some(instance), Some(gpu)) = (instance, self.engine.gpu()) {
            self.outputs.attach_gpu(instance, gpu.clone());
            self.attached = true;
        }
    }
}

impl ApplicationHandler<eframe::UserEvent> for Host<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.eframe.new_events(event_loop, cause);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.eframe.user_event(event_loop, event);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        if !self.outputs.window_event(window_id, &event) {
            if let WindowEvent::Resized(size) = &event
                && size.width > 0
                && size.height > 0
                && let Some(gpu) = self.engine.gpu()
            {
                gpu.quiesce(Instant::now() + RESIZE_QUIET);
            }
            self.eframe.window_event(event_loop, window_id, event);
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device_id: DeviceId, event: DeviceEvent) {
        self.eframe.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.about_to_wait(event_loop);
        self.attach();
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
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.outputs.shutdown();
        self.eframe.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.memory_warning(event_loop);
    }
}
