//! The GPU side of penumbra: one wgpu device shared by the cook thread, the UI and the projector
//! outputs; textures as they flow between `tex` nodes; fullscreen shader passes with declared
//! parameters; readback to the CPU for snapshots; image files; video planes.
//!
//! Conventions every `tex` node follows:
//! - textures are [`FORMAT`] (RGBA, 16-bit float), **linear light, premultiplied alpha**;
//!   values above 1 are allowed (HDR);
//! - texture coordinates have their origin at the **top left**, `y` pointing down (as wgpu and
//!   image files have it);
//! - a node's resolution is fixed (a parameter or its input's), never derived from downstream.
//!
//! Errors never panic: wgpu's default handler for errors outside an error scope panics, so
//! [`Gpu::new`] replaces it with one that logs and counts ([`Gpu::errors`]).

pub mod bc;
pub mod environment;
pub mod geo;
pub mod header;
pub mod image;
pub mod layers;
pub mod pass;
pub mod projection;
pub mod readback;
pub mod render;
pub mod shapes;
pub mod tex;
pub mod video;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use penumbra_core::Error;

pub use geo::{Geo, Mat};
pub use pass::{Batch, Pass, PassInput};
pub use projection::{MeshVertex, Projection};
pub use readback::{PendingRead, Pixels, ReadPool, ReadState, RowsError, encode_png_rgba8};
pub use render::Lights;
pub use shapes::{ShapeMode, ShapePoly, Shapes};
pub use tex::{Tex, TexRing};

/// The format of every texture flowing between nodes.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Errors raised outside an error scope (see [`Gpu::errors`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorLog {
    pub count: u64,
    pub last: Option<String>,
    /// Window surfaces that could not be reconfigured (resized) because another thread
    /// submitted work during wgpu's wait for an idle GPU. The window then stays black until it
    /// is resized again; the UI does that itself when this count grows.
    pub surface_configure_failures: u64,
}

/// Recognises wgpu-core's `ConfigureSurfaceError::GpuWaitTimeout` by its message (the error
/// type is not exported through wgpu).
fn is_surface_wait_failure(message: &str) -> bool {
    message.contains("Surface::configure") && message.contains("come idle")
}

/// GPU memory held through wgpu's allocator ([`Gpu::memory`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemory {
    /// Bytes in live allocations (textures, buffers).
    pub allocated: u64,
    /// Bytes in the memory blocks those come from, unused parts included.
    pub reserved: u64,
    pub allocations: u64,
    pub blocks: u64,
}

/// Which adapter [`Gpu::headless`] asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterChoice {
    /// The fast GPU (high-performance preference).
    Hardware,
    /// A software rasteriser: WARP on Windows (through DX12), lavapipe or similar elsewhere.
    /// Slow but the same everywhere, so golden images are blessed on it.
    Software,
}

struct Inner {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    errors: Arc<Mutex<ErrorLog>>,
    layouts: pass::Layouts,
    passes: Mutex<HashMap<u64, Arc<Pass>>>,
    video: Mutex<Option<Arc<video::Pipeline>>>,
    bc: Mutex<Option<Arc<bc::Pipeline>>>,
    render: Mutex<Option<Arc<render::Pipelines>>>,
    environment: Mutex<Option<Arc<environment::Pipelines>>>,
    /// Submissions and presents share it, surface configuration takes it alone (see
    /// [`Gpu::configure_surface`]).
    submit_lock: RwLock<()>,
    /// Until when submissions and presents wait (see [`Gpu::hold`]).
    held_until: Mutex<Option<Instant>>,
}

/// The longest [`Gpu::hold`] keeps one submission waiting, however often it is renewed.
const MAX_HOLD: Duration = Duration::from_millis(400);

/// The device and queue plus what every pass needs. Cheap to clone; safe to use from any thread
/// (wgpu's device and queue are).
#[derive(Clone)]
pub struct Gpu {
    inner: Arc<Inner>,
}

impl Gpu {
    /// Wraps a device someone else created (eframe, in the app). Installs the error handler.
    pub fn new(adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let errors = Arc::new(Mutex::new(ErrorLog::default()));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            let message = e.to_string();
            log::error!("GPU error: {message}");
            let mut log = sink.lock().unwrap_or_else(PoisonError::into_inner);
            log.count += 1;
            if is_surface_wait_failure(&message) {
                log.surface_configure_failures += 1;
            }
            log.last = Some(message);
        }));
        let info = adapter.get_info();
        let layouts = pass::Layouts::new(&device);
        Self {
            inner: Arc::new(Inner {
                adapter,
                device,
                queue,
                info,
                errors,
                layouts,
                passes: Mutex::new(HashMap::new()),
                video: Mutex::new(None),
                bc: Mutex::new(None),
                render: Mutex::new(None),
                environment: Mutex::new(None),
                submit_lock: RwLock::new(()),
                held_until: Mutex::new(None),
            }),
        }
    }

    /// A device without a window, for tests and headless tools. `backends` defaults to the
    /// platform's choice (DX12 on Windows, as in the app).
    pub fn headless(choice: AdapterChoice) -> Result<Self, Error> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
            desc.backends = wgpu::Backends::DX12;
        }
        let instance = wgpu::Instance::new(desc);
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: choice == AdapterChoice::Software,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .map_err(|e| Error::new(format!("no {choice:?} GPU adapter: {e}")))?;
        let (device, queue) = block_on(adapter.request_device(&device_descriptor(&adapter)))
            .map_err(|e| Error::new(format!("cannot open the GPU device: {e}")))?;
        Ok(Self::new(adapter, device, queue))
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }

    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.inner.adapter
    }

    pub fn info(&self) -> &wgpu::AdapterInfo {
        &self.inner.info
    }

    /// Adapter name and backend, e.g. `NVIDIA GeForce RTX 3090 (Dx12)`.
    pub fn name(&self) -> String {
        format!("{} ({:?})", self.inner.info.name, self.inner.info.backend)
    }

    /// Largest width or height of a texture.
    pub fn max_texture_size(&self) -> u32 {
        self.inner.device.limits().max_texture_dimension_2d
    }

    /// What wgpu's allocator holds on the GPU, where the backend reports it (DX12, Vulkan,
    /// Metal): `(allocated, reserved)` bytes and the number of allocations. Copies the whole
    /// report, so call it every few seconds, not every frame.
    pub fn memory(&self) -> Option<GpuMemory> {
        let r = self.inner.device.generate_allocator_report()?;
        Some(GpuMemory {
            allocated: r.total_allocated_bytes,
            reserved: r.total_reserved_bytes,
            allocations: r.allocations.len() as u64,
            blocks: r.blocks.len() as u64,
        })
    }

    /// Errors wgpu raised outside an error scope since the device was opened.
    pub fn errors(&self) -> ErrorLog {
        self.inner.errors.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Lets wgpu finish callbacks and free resources without waiting. The cook thread calls it
    /// once per frame.
    pub fn poll(&self) {
        if let Err(e) = self.inner.device.poll(wgpu::PollType::Poll) {
            log::warn!("GPU poll: {e}");
        }
    }

    /// A pass compiled once per process and shared (built-in nodes). Compiling takes from a few
    /// to a hundred milliseconds depending on the backend, so the app warms these up off the
    /// cook thread at startup.
    pub fn cached_pass(
        &self,
        label: &str,
        source: &str,
        params: &[penumbra_core::ParamSpec],
    ) -> Result<Arc<Pass>, Error> {
        let key = pass::cache_key(source, params);
        if let Some(p) = self.inner.passes.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(p.clone());
        }
        // Compiled outside the lock; two threads racing compile twice, which is harmless.
        let compiled = Arc::new(Pass::compile(self, label, source, params)?);
        self.inner.passes.lock().unwrap_or_else(PoisonError::into_inner).insert(key, compiled.clone());
        Ok(compiled)
    }

    /// Submits command buffers. Everything penumbra submits goes through here (not through
    /// the queue directly), so [`Gpu::configure_surface`] can keep submissions out while wgpu
    /// waits for an idle GPU.
    pub fn submit(&self, buffers: impl IntoIterator<Item = wgpu::CommandBuffer>) -> wgpu::SubmissionIndex {
        let _shared = self.shared();
        self.inner.queue.submit(buffers)
    }

    /// Holds penumbra's submissions and presents back until `until`, at most [`MAX_HOLD`]
    /// each: for a window being resized. egui configures the editor's and the viewer window's
    /// surfaces itself, outside [`Gpu::configure_surface`]'s lock, and wgpu fails a configure
    /// when another thread submits while it waits for an idle GPU; with a heavy render cooking
    /// every frame that was nearly every resize. Outputs pause for as long.
    pub fn hold(&self, until: Instant) {
        let mut held = self.inner.held_until.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_none_or(|h| h < until) {
            *held = Some(until);
        }
    }

    /// Holds submissions back like [`Gpu::hold`] and waits for those under way to finish, so
    /// that nothing of penumbra's reaches the queue until `until` (or [`MAX_HOLD`]). For a
    /// surface configure we cannot take the lock around, because the code doing it (eframe
    /// handling a resize) may itself run a frame that submits through this `Gpu`.
    pub fn quiesce(&self, until: Instant) {
        self.hold(until);
        drop(self.inner.submit_lock.write().unwrap_or_else(PoisonError::into_inner));
    }

    /// The shared side of the submit lock, taken once no hold is in force. The hold is looked
    /// at again under the lock: a submission that passed the wait just before
    /// [`Gpu::quiesce`] set the hold would otherwise slip through after it.
    fn shared(&self) -> RwLockReadGuard<'_, ()> {
        let start = Instant::now();
        loop {
            self.wait_for_hold();
            let shared = self.inner.submit_lock.read().unwrap_or_else(PoisonError::into_inner);
            if !self.held() || start.elapsed() >= MAX_HOLD {
                return shared;
            }
        }
    }

    fn held(&self) -> bool {
        let until = *self.inner.held_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| u > Instant::now())
    }

    fn wait_for_hold(&self) {
        let start = Instant::now();
        loop {
            let until = *self.inner.held_until.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let Some(until) = until.filter(|u| *u > now) else { return };
            let left = MAX_HOLD.saturating_sub(now.saturating_duration_since(start));
            if left.is_zero() {
                return;
            }
            std::thread::sleep(until.saturating_duration_since(now).min(left).min(Duration::from_millis(10)));
        }
    }

    /// Copies `src` into `dst`, which must have the same size.
    pub fn copy(&self, src: &Tex, dst: &Tex) -> Result<(), Error> {
        if src.size() != dst.size() {
            return Err(Error::new(format!(
                "cannot copy a {}x{} texture into a {}x{} one",
                src.width(),
                src.height(),
                dst.width(),
                dst.height()
            )));
        }
        let mut encoder =
            self.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("copy") });
        encoder.copy_texture_to_texture(
            src.texture().as_image_copy(),
            dst.texture().as_image_copy(),
            wgpu::Extent3d { width: src.width(), height: src.height(), depth_or_array_layers: 1 },
        );
        self.submit([encoder.finish()]);
        Ok(())
    }

    /// Presents a surface texture, under the same lock as [`Gpu::submit`].
    pub fn present(&self, frame: wgpu::SurfaceTexture) {
        let _shared = self.shared();
        self.inner.queue.present(frame);
    }

    /// Configures (resizes) a window surface. wgpu waits for the GPU to become idle first and
    /// fails with a timeout if another thread submits meanwhile (the black window after a
    /// resize). Our own submissions are held off by the lock; the editor's (eframe submits outside
    /// it) can still interfere, so a failure is retried a few times before it is reported.
    pub fn configure_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
    ) -> Result<(), Error> {
        let mut last = None;
        for _ in 0..4 {
            let _alone = self.inner.submit_lock.write().unwrap_or_else(PoisonError::into_inner);
            let ((), error) = self.scoped(|| surface.configure(&self.inner.device, config));
            match error {
                None => return Ok(()),
                Some(e) => last = Some(e),
            }
        }
        Err(Error::new(format!("cannot configure the surface: {}", last.unwrap_or_default())))
    }

    /// Runs `f` while no penumbra submission or present can start, for surface work done
    /// outside [`Gpu::configure_surface`] (eframe configuring a new window's surface). `f` must
    /// not submit through this `Gpu` itself: the lock is not reentrant.
    pub fn alone<T>(&self, f: impl FnOnce() -> T) -> T {
        let _alone = self.inner.submit_lock.write().unwrap_or_else(PoisonError::into_inner);
        f()
    }

    /// A uniform buffer holding `contents`, padded to wgpu's copy alignment. Not
    /// `DeviceExt::create_buffer_init`: that maps the buffer at creation and panics when the
    /// device cannot create it (out of GPU memory, 2026-10-11 with ComfyUI holding the VRAM);
    /// here a failure goes to the error scope like any other.
    pub(crate) fn uniform_buffer(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
        let size = contents.len().next_multiple_of(align).max(align);
        let buffer = self.inner.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut padded = contents.to_vec();
        padded.resize(size, 0);
        self.inner.queue.write_buffer(&buffer, 0, &padded);
        buffer
    }

    pub(crate) fn layouts(&self) -> &pass::Layouts {
        &self.inner.layouts
    }

    /// Runs `f` inside validation and internal-error scopes and returns the first error raised.
    /// Native wgpu reports errors synchronously, so the scopes resolve at once.
    pub fn scoped<T>(&self, f: impl FnOnce() -> T) -> (T, Option<String>) {
        let device = &self.inner.device;
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let out = f();
        let mut error = None;
        // Scopes pop in reverse order.
        for pop in [internal.pop(), oom.pop(), validation.pop()] {
            if let Some(e) = poll_once(pop).flatten() {
                error.get_or_insert(e.to_string());
            }
        }
        (out, error)
    }
}

/// What penumbra asks of a device: the adapter's texture size limit (up to 16384), BC texture
/// compression where the adapter has it (HAP, [`bc`]), otherwise wgpu's defaults. eframe gets
/// the same in the app.
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let mut limits = wgpu::Limits::default();
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.clamp(8192, 16384);
    let required_features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
    wgpu::DeviceDescriptor {
        label: Some("penumbra"),
        required_limits: limits,
        required_features,
        ..Default::default()
    }
}

/// Polls a future once. Enough for wgpu's native futures, which are ready immediately.
fn poll_once<F: std::future::Future>(f: F) -> Option<F::Output> {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match std::pin::pin!(f).poll(&mut cx) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

/// Drives a future to completion on this thread. Only for wgpu's native futures, which resolve
/// immediately or after a short internal wait.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::OnceLock;

    /// One software device per test process. `None` where no software adapter exists; on Windows
    /// WARP always does, so there it is an error.
    pub fn gpu() -> Option<Gpu> {
        static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
        GPU.get_or_init(|| match Gpu::headless(AdapterChoice::Software) {
            Ok(g) => Some(g),
            Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
            Err(e) => {
                eprintln!("skipping GPU tests: {e}");
                None
            }
        })
        .clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_surface_wait_failure_is_recognised() {
        // As wgpu 30 formats it (seen 2026-10-08 after snapping the window with Win+Left).
        let message = "Validation Error\n\nCaused by:\n  In Surface::configure\n    \
                       Failed to wait for GPU to come idle before reconfiguring the Surface\n";
        assert!(super::is_surface_wait_failure(message));
        assert!(!super::is_surface_wait_failure("Validation Error\n  In Queue::submit\n"));
    }
}
