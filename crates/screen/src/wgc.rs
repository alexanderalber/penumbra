//! Windows.Graphics.Capture and Direct3D. The FFI module of this crate: every `unsafe` lives
//! here, behind small owning types.
//!
//! - [`Device`]: two Direct3D 11 devices on wgpu's adapter. Windows.Graphics.Capture delivers
//!   its frames as textures of a plain one. The other is layered on wgpu's own DirectX 12 device
//!   and queue (Direct3D 11 on 12), so its copy into a wgpu texture runs on wgpu's queue, in
//!   submission order, with no fence of our own. A frame pool on the layered device alone
//!   delivered two frames and then stopped (tried on an RTX 3090 and on WARP).
//! - [`Bridge`]: a shared texture with a keyed mutex, opened on both devices. Per frame: the
//!   plain device copies the frame into it, the layered device copies it into the wrapped wgpu
//!   texture, and a pass converts that to a `tex` texture. Two GPU copies, no CPU copy.
//! - A worker thread per [`Capture`] joins the multithreaded COM apartment, finds the monitor or
//!   window, starts a capture session on a free-threaded frame pool and watches it. Frame and
//!   close events arrive on Windows' thread pool and only wake the node. While the worker runs,
//!   the apartment exists, so the cook thread may use the pool and its frames (all agile
//!   objects) without joining it.
//! - The cook takes the newest frame and drops older ones. A frame whose content size differs
//!   from the pool's buffers recreates the pool at the new size, as Windows asks for a resized
//!   window.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use penumbra_gpu::{Alpha, Gpu, Tex, TexRing};
use penumbra_spout::convert::{Converter, Direction};
use wgpu::hal::api::Dx12;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{CloseHandle, HMODULE, HWND, LPARAM, LUID, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D11on12::{
    D3D11_RESOURCE_FLAGS, D3D11On12CreateDevice, ID3D11On12Device,
};
use windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_COPY_DEST;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory4, IDXGIKeyedMutex, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC,
    HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, Interface, PCWSTR, PWSTR};

use crate::pick::{self, Monitor};
use crate::{MonitorListing, Options, Sources, Status, Target, WindowListing};

/// Frames the pool keeps: one being copied, one being written.
const BUFFERS: i32 = 2;
/// The pixel format asked for: what the desktop is composed in (sRGB-encoded, 8 bits).
const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// `DXGI_FORMAT_B8G8R8A8_UNORM`.
const DXGI_BGRA8: i32 = 87;
/// How often the worker checks that the window or monitor is still there.
const POLL: Duration = Duration::from_millis(100);
/// How long a keyed mutex of the shared texture is waited for (both sides are the cook's).
const KEYED_WAIT_MS: u32 = 100;

const UNSUPPORTED: &str = "Windows cannot capture the screen here: Windows.Graphics.Capture needs Windows 10 \
                           1903 or later and a desktop session (some remote desktops have none)";

fn err(what: &str, e: windows::core::Error) -> String {
    let message = e.message();
    let message = message.trim();
    if message.is_empty() { format!("{what} ({})", e.code()) } else { format!("{what}: {message}") }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Two Direct3D 11 devices on wgpu's adapter: one layered on wgpu's DirectX 12 device and queue,
/// one plain, for Windows' capture.
struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    on12: ID3D11On12Device,
    native: ID3D11Device,
    native_context: ID3D11DeviceContext,
}

// SAFETY: D3D11 devices are free-threaded; their immediate contexts are used only by the cook
// through `&mut Capture` (the plain one is multithread-protected besides, see `native_device`).
unsafe impl Send for Device {}

impl Device {
    fn new(gpu: &Gpu) -> Result<Self, String> {
        const NOT_DX12: &str = "screen capture needs the DirectX 12 backend, and the GPU runs on another";
        // SAFETY: `as_hal` hands out wgpu-hal's objects, which must not be used to break wgpu's
        // invariants. Only the COM pointers are cloned (a reference each), the guards drop at
        // the end of the statement. The D3D11On12 device keeps its own references to both.
        let device12 =
            unsafe { gpu.device().as_hal::<Dx12>() }.map(|d| d.raw_device().clone()).ok_or(NOT_DX12)?;
        // SAFETY: as above.
        let queue12 = unsafe { gpu.queue().as_hal::<Dx12>() }.map(|q| q.as_raw().clone()).ok_or(NOT_DX12)?;
        let queue: windows::core::IUnknown =
            queue12.cast().map_err(|e| err("cannot use the GPU queue", e))?;
        let mut device = None;
        let mut context = None;
        // SAFETY: valid device and queue; the out pointers live on this stack frame.
        unsafe {
            D3D11On12CreateDevice(
                &device12,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
                None,
                Some(&[Some(queue)]),
                0,
                Some(&mut device),
                Some(&mut context),
                None,
            )
        }
        .map_err(|e| err("cannot open Direct3D 11 on the GPU", e))?;
        let device: ID3D11Device = device.ok_or("Direct3D 11 on 12 returned no device")?;
        let context: ID3D11DeviceContext = context.ok_or("Direct3D 11 on 12 returned no context")?;
        let on12 = device.cast::<ID3D11On12Device>().map_err(|e| err("not a Direct3D 11 on 12 device", e))?;
        // SAFETY: a live device.
        let luid = unsafe { device12.GetAdapterLuid() };
        let (native, native_context) = native_device(luid)?;
        Ok(Self { device, context, on12, native, native_context })
    }
}

/// A plain Direct3D 11 device on the adapter with this LUID (wgpu's), for the capture.
fn native_device(luid: LUID) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    // SAFETY: plain factory calls; the out pointers live on this stack frame.
    unsafe {
        let factory: IDXGIFactory4 =
            CreateDXGIFactory1().map_err(|e| err("cannot list the graphics adapters", e))?;
        let adapter: IDXGIAdapter =
            factory.EnumAdapterByLuid(luid).map_err(|e| err("cannot find wgpu's graphics adapter", e))?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| err("cannot open Direct3D 11 for the capture", e))?;
        let device: ID3D11Device = device.ok_or("Direct3D 11 returned no device")?;
        let context: ID3D11DeviceContext = context.ok_or("Direct3D 11 returned no context")?;
        // Windows' capture threads use this device while the cook copies from it.
        if let Ok(m) = context.cast::<ID3D11Multithread>() {
            let _ = m.SetMultithreadProtected(true);
        }
        Ok((device, context))
    }
}

/// Takes a keyed mutex (key 0) for at most `ms`; `false` when it stayed taken. Called through
/// the vtable for the raw HRESULT: a timeout is a success code the wrapper would hide.
fn acquire(keyed: &IDXGIKeyedMutex, ms: u32) -> bool {
    // SAFETY: a live keyed mutex.
    let hr = unsafe { (Interface::vtable(keyed).AcquireSync)(Interface::as_raw(keyed), 0, ms) };
    hr.0 == 0
}

fn release(keyed: &IDXGIKeyedMutex) {
    // SAFETY: acquired with key 0 by `acquire`.
    unsafe { keyed.ReleaseSync(0) }.ok();
}

/// The texture a frame travels through, of one size: a shared texture with a keyed mutex,
/// opened on both devices, and the wrapped wgpu texture it ends in.
struct Bridge {
    native: ID3D11Resource,
    native_keyed: IDXGIKeyedMutex,
    on12: ID3D11Resource,
    on12_keyed: IDXGIKeyedMutex,
    target: Wrapped,
}

// SAFETY: COM references to resources of free-threaded devices, used only through the
// `Device` that made them.
unsafe impl Send for Bridge {}

/// The device as the Windows Runtime takes it. Called on the worker, inside the apartment.
fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, String> {
    let dxgi = device.cast::<IDXGIDevice>().map_err(|e| err("the Direct3D device has no DXGI device", e))?;
    // SAFETY: a live DXGI device.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|e| err("cannot hand the Direct3D device to Windows' capture", e))?;
    inspectable.cast::<IDirect3DDevice>().map_err(|e| err("not a Direct3D device", e))
}

/// The DXGI device, handed to the worker to make the Windows Runtime device there.
struct DxgiDevice(ID3D11Device);

// SAFETY: a COM reference to a free-threaded device.
unsafe impl Send for DxgiDevice {}

/// A wgpu texture as a D3D11 resource in the copy-destination state. Holds the wgpu texture, so
/// its memory stays while the wrapper does (fields drop in order).
struct Wrapped {
    resource: ID3D11Resource,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

// SAFETY: a COM reference to a resource of a free-threaded device, used only through the
// `Device` that made it.
unsafe impl Send for Wrapped {}

/// A running capture session. Closing it ends the capture (and Windows' border).
struct Live {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    winrt: IDirect3DDevice,
    /// The size of the pool's buffers.
    size: Mutex<SizeInt32>,
    /// Set by the item's `Closed` event: the window closed or the monitor went.
    closed: Arc<AtomicBool>,
    window: Option<isize>,
    monitor: Option<isize>,
    name: String,
}

// SAFETY: the pool, session, item and device are agile Windows Runtime objects (the frame pool
// is free-threaded); they are used from the worker and the cook while the worker keeps the
// multithreaded apartment alive.
unsafe impl Send for Live {}
// SAFETY: as above.
unsafe impl Sync for Live {}

impl Live {
    fn alive(&self) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        if let Some(w) = self.window {
            // SAFETY: IsWindow takes any value and says whether it names a window.
            return unsafe { IsWindow(Some(HWND(w as *mut c_void))) }.as_bool();
        }
        if let Some(m) = self.monitor {
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            // SAFETY: GetMonitorInfoW fails on a handle that names no monitor; `info` outlives it.
            return unsafe { GetMonitorInfoW(HMONITOR(m as *mut c_void), &mut info) }.as_bool();
        }
        true
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.session.Close().ok();
        self.pool.Close().ok();
    }
}

#[derive(Default)]
struct State {
    live: Option<Arc<Live>>,
    error: Option<String>,
    /// Counts sessions started.
    generation: u64,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// One node's capture: a worker thread that keeps a session running, and the textures the cook
/// copies frames into.
pub struct Capture {
    device: Device,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    bridge: Option<Bridge>,
    converter: Converter,
    ring: TexRing,
    frames: u64,
    size: Option<(u32, u32)>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Capture {
    /// Starts capturing `target`. `wake` is called when a frame arrives or the capture changes.
    pub fn new(
        gpu: &Gpu,
        target: Target,
        options: Options,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let device = Device::new(gpu)?;
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let wake: Wake = Arc::new(wake);
        let dxgi = DxgiDevice(device.native.clone());
        let (s, flag) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("screen capture".into())
            .spawn(move || run(&target, options, dxgi, &s, &flag, &wake))
            .map_err(|e| format!("cannot start the capture thread: {e}"))?;
        Ok(Self {
            device,
            state,
            stop,
            bridge: None,
            converter: Converter::new(),
            ring: TexRing::new(),
            frames: 0,
            size: None,
        })
    }

    pub fn status(&self) -> Status {
        let s = lock(&self.state);
        Status { capturing: s.live.as_ref().map(|l| l.name.clone()), error: s.error.clone(), size: self.size }
    }

    /// Frames taken.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes on the GPU of the textures the capture keeps.
    pub fn bytes(&self) -> u64 {
        // The shared texture and the wrapped one, 4 bytes a pixel each.
        let bridge = self.bridge.as_ref().map_or(0, |b| {
            let s = b.target.texture.size();
            u64::from(s.width) * u64::from(s.height) * 8
        });
        self.ring.bytes() + bridge
    }

    /// The newest frame as a texture, `None` when none arrived since the last call. An error
    /// is about this frame; why nothing is captured at all is in [`Capture::status`].
    pub fn take(&mut self, gpu: &Gpu) -> Result<Option<Tex>, String> {
        let Some(live) = lock(&self.state).live.clone() else { return Ok(None) };
        let mut newest: Option<Direct3D11CaptureFrame> = None;
        // An empty pool answers with an error (a null frame).
        while let Ok(frame) = live.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(frame) {
                old.Close().ok();
            }
        }
        let Some(frame) = newest else { return Ok(None) };
        let result = self.copy(gpu, &live, &frame);
        frame.Close().ok();
        result
    }

    fn copy(
        &mut self,
        gpu: &Gpu,
        live: &Live,
        frame: &Direct3D11CaptureFrame,
    ) -> Result<Option<Tex>, String> {
        let content = frame.ContentSize().map_err(|e| err("the frame has no size", e))?;
        if content.Width > 0 && content.Height > 0 {
            let mut size = lock(&live.size);
            if *size != content {
                live.pool
                    .Recreate(&live.winrt, FORMAT, BUFFERS, content)
                    .map_err(|e| err("cannot follow the new size", e))?;
                *size = content;
            }
        }
        let surface = frame.Surface().map_err(|e| err("the frame has no picture", e))?;
        let access = surface
            .cast::<IDirect3DDxgiInterfaceAccess>()
            .map_err(|e| err("the frame is not a Direct3D texture", e))?;
        // SAFETY: a live surface; asks for an interface it implements.
        let texture: ID3D11Texture2D =
            unsafe { access.GetInterface() }.map_err(|e| err("the frame is not a Direct3D 11 texture", e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture; `desc` outlives the call.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format.0 != DXGI_BGRA8 || desc.SampleDesc.Count != 1 {
            return Err(format!("the frame came as DXGI format {}, not BGRA", desc.Format.0));
        }
        // The buffers may be larger than the content until the pool is recreated.
        let w = u32::try_from(content.Width).unwrap_or(0).min(desc.Width);
        let h = u32::try_from(content.Height).unwrap_or(0).min(desc.Height);
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let max = gpu.max_texture_size();
        if w > max || h > max {
            return Err(format!("the capture is {w}x{h}, more than {max} on a side"));
        }
        if self
            .bridge
            .as_ref()
            .is_none_or(|b| (b.target.texture.width(), b.target.texture.height()) != (w, h))
        {
            self.bridge = None;
            self.bridge = Some(self.bridge(gpu, w, h)?);
        }
        let Some(bridge) = self.bridge.as_ref() else { return Err("no texture to capture into".into()) };
        let src = texture.cast::<ID3D11Resource>().map_err(|e| err("the frame is not a resource", e))?;
        // The frame into the shared texture, on the capture's device. Both sides of the keyed
        // mutex are this thread's, one after the other, so it is free at once.
        if !acquire(&bridge.native_keyed, KEYED_WAIT_MS) {
            return Err("the capture's shared texture stayed locked".into());
        }
        let area = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
        // SAFETY: live resources of the plain device; the box lies inside both textures.
        unsafe {
            self.device.native_context.CopySubresourceRegion(&bridge.native, 0, 0, 0, 0, &src, 0, Some(&area))
        };
        // Releasing the keyed mutex submits the copy and orders it before the other side's use.
        release(&bridge.native_keyed);
        // The target into the copy-destination state on wgpu's side before Direct3D 11 copies.
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("screen capture") });
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &bridge.target.texture,
                selector: None,
                state: wgpu::TextureUses::COPY_DST,
            }),
        );
        gpu.submit([encoder.finish()]);
        if !acquire(&bridge.on12_keyed, KEYED_WAIT_MS) {
            return Err("the capture's shared texture stayed locked".into());
        }
        let list = [Some(bridge.target.resource.clone())];
        // SAFETY: live resources of the layered device; the wrapped one is acquired around its
        // use as Direct3D 11 on 12 requires, and the flush hands the copy to wgpu's queue.
        unsafe {
            self.device.on12.AcquireWrappedResources(&list);
            self.device.context.CopySubresourceRegion(
                &bridge.target.resource,
                0,
                0,
                0,
                0,
                &bridge.on12,
                0,
                None,
            );
            self.device.on12.ReleaseWrappedResources(&list);
        }
        release(&bridge.on12_keyed);
        // SAFETY: the layered device's context, used on this thread only.
        unsafe { self.device.context.Flush() };
        let out = self.ring.next(gpu, w, h, "screen capture").map_err(|e| e.0)?;
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("screen capture") });
        let view = &bridge.target.view;
        self.converter.encode(gpu, &mut encoder, view, out.texture(), Direction::FromSpout, Alpha::Opaque)?;
        let ((), error) = gpu.scoped(|| {
            gpu.submit([encoder.finish()]);
        });
        if let Some(e) = error {
            return Err(format!("cannot convert the captured picture: {e}"));
        }
        self.frames += 1;
        self.size = Some((w, h));
        Ok(Some(out))
    }

    /// The textures a `w` x `h` frame travels through.
    fn bridge(&self, gpu: &Gpu, w: u32, h: u32) -> Result<Bridge, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT(DXGI_BGRA8),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        };
        let mut shared: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` and `shared` outlive the call.
        unsafe { self.device.native.CreateTexture2D(&desc, None, Some(&mut shared)) }
            .map_err(|e| err(&format!("cannot create a {w}x{h} shared texture"), e))?;
        let shared = shared.ok_or("Direct3D 11 returned no texture")?;
        let native = shared.cast::<ID3D11Resource>().map_err(|e| err("not a resource", e))?;
        let native_keyed =
            shared.cast::<IDXGIKeyedMutex>().map_err(|e| err("the texture has no keyed mutex", e))?;
        let dxgi = shared.cast::<IDXGIResource>().map_err(|e| err("the texture cannot be shared", e))?;
        // SAFETY: a live DXGI resource created shareable above.
        let handle =
            unsafe { dxgi.GetSharedHandle() }.map_err(|e| err("the texture has no shared handle", e))?;
        let mut opened: Option<ID3D11Texture2D> = None;
        // SAFETY: a handle of a live shared texture on the same adapter; `opened` outlives the call.
        unsafe { self.device.device.OpenSharedResource(handle, &mut opened) }
            .map_err(|e| err("cannot open the shared texture", e))?;
        let opened = opened.ok_or("Direct3D 11 on 12 opened no texture")?;
        let on12 = opened.cast::<ID3D11Resource>().map_err(|e| err("not a resource", e))?;
        let on12_keyed =
            opened.cast::<IDXGIKeyedMutex>().map_err(|e| err("the texture has no keyed mutex", e))?;
        let target = self.wrap(gpu, w, h)?;
        Ok(Bridge { native, native_keyed, on12, on12_keyed, target })
    }

    fn wrap(&self, gpu: &Gpu, w: u32, h: u32) -> Result<Wrapped, String> {
        let (texture, error) = gpu.scoped(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("screen capture"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        });
        if let Some(e) = error {
            return Err(format!("cannot create a {w}x{h} texture to capture into: {e}"));
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // wgpu clears a texture it never wrote before its first read, which would wipe the
        // first copy Direct3D 11 makes into it; cleared once here, it counts as written.
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("screen capture") });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("screen capture clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        gpu.submit([encoder.finish()]);
        // SAFETY: as in `Device::new`; the resource is cloned, the guard dropped at once.
        // `Wrapped` holds the wgpu texture too, so its memory outlives the wrapper.
        let resource12 = unsafe { texture.as_hal::<Dx12>() }
            .map(|t| unsafe { t.raw_resource() }.clone())
            .ok_or("the texture is not a DirectX 12 texture")?;
        let flags = D3D11_RESOURCE_FLAGS::default();
        let state = D3D12_RESOURCE_STATE_COPY_DEST;
        let mut wrapped: Option<ID3D11Resource> = None;
        // SAFETY: a live D3D12 resource of this device; `flags` and `wrapped` outlive the call.
        unsafe { self.device.on12.CreateWrappedResource(&resource12, &flags, state, state, &mut wrapped) }
            .map_err(|e| err("cannot share a texture with Direct3D 11", e))?;
        let resource = wrapped.ok_or("Direct3D 11 on 12 returned no texture")?;
        Ok(Wrapped { resource, texture, view })
    }
}

/// The worker: keeps a session running for `target` until `stop`, retrying with a growing pause.
fn run(
    target: &Target,
    options: Options,
    dxgi: DxgiDevice,
    state: &Mutex<State>,
    stop: &AtomicBool,
    wake: &Wake,
) {
    // SAFETY: joins this thread to the multithreaded apartment; undone below if it succeeded.
    let apartment = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
    watch(target, options, &dxgi, state, stop, wake);
    lock(state).live = None;
    drop(dxgi);
    if apartment {
        // SAFETY: balances the RoInitialize above, on the same thread, after every object this
        // thread made in the apartment is gone.
        unsafe { RoUninitialize() };
    }
}

fn watch(
    target: &Target,
    options: Options,
    dxgi: &DxgiDevice,
    state: &Mutex<State>,
    stop: &AtomicBool,
    wake: &Wake,
) {
    let winrt = winrt_device(&dxgi.0);
    let mut pause = Duration::ZERO;
    let mut next = Instant::now();
    let report = |e: Option<String>| {
        let mut s = lock(state);
        if s.error != e {
            s.error = e;
            drop(s);
            wake();
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let alive = lock(state).live.as_ref().map(|l| l.alive());
        match alive {
            Some(true) => {
                std::thread::sleep(POLL);
                continue;
            }
            Some(false) => {
                lock(state).live = None;
                report(Some(match target {
                    Target::Window(_) => "the window closed; waiting for it to come back".into(),
                    Target::Monitor(_) => "the monitor went away; waiting for it to come back".into(),
                }));
                pause = Duration::ZERO;
                next = Instant::now() + pick::RETRY_MIN;
            }
            None => {}
        }
        let now = Instant::now();
        if now < next {
            std::thread::sleep((next - now).min(POLL));
            continue;
        }
        let started = winrt.clone().and_then(|w| start(target, options, w, wake));
        match started {
            Ok(live) => {
                let mut s = lock(state);
                s.live = Some(Arc::new(live));
                s.error = None;
                s.generation += 1;
                drop(s);
                pause = Duration::ZERO;
                wake();
            }
            Err(e) => {
                report(Some(e));
                pause = pick::backoff(pause);
                next = Instant::now() + pause;
            }
        }
    }
}

/// Starts a session on what `target` names now.
fn start(target: &Target, options: Options, winrt: IDirect3DDevice, wake: &Wake) -> Result<Live, String> {
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return Err(UNSUPPORTED.into());
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|e| err("cannot reach Windows' capture", e))?;
    let (item, window, monitor, name) = match target {
        Target::Monitor(spec) => {
            let monitors = monitors();
            let displays = penumbra_system::displays().unwrap_or_default();
            let list: Vec<Monitor> = monitors.iter().map(|(_, m)| m.clone()).collect();
            let i = pick::choose_monitor(spec, &list, &displays)?;
            let Some((handle, m)) = monitors.get(i) else { return Err("the monitor went away".into()) };
            // SAFETY: a monitor handle just listed; a stale one makes the call fail.
            let item: GraphicsCaptureItem =
                unsafe { interop.CreateForMonitor(HMONITOR(*handle as *mut c_void)) }
                    .map_err(|e| err(&format!("cannot capture {}", m.os_name), e))?;
            (item, None, Some(*handle), pick::monitor_id(m, &displays))
        }
        Target::Window(part) => {
            if part.trim().is_empty() {
                return Err("`window` is empty: name a part of the window's title".into());
            }
            let list = windows();
            let i =
                pick::first_window(list.iter().map(|(_, w)| w.title.as_str()), part).ok_or_else(|| {
                    format!("no window's title contains `{}` (screen.sources lists them)", part.trim())
                })?;
            let Some((handle, w)) = list.get(i) else { return Err("the window closed".into()) };
            // SAFETY: a window handle just listed; a stale one makes the call fail.
            let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(HWND(*handle as *mut c_void)) }
                .map_err(|e| err(&format!("cannot capture the window `{}`", w.title), e))?;
            (item, Some(*handle), None, w.title.clone())
        }
    };
    let size = item.Size().map_err(|e| err("the capture has no size", e))?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(format!("`{name}` has no area to capture ({}x{})", size.Width, size.Height));
    }
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt, FORMAT, BUFFERS, size)
        .map_err(|e| err("cannot make the capture's frame pool", e))?;
    let session = pool.CreateCaptureSession(&item).map_err(|e| err("cannot start a capture session", e))?;
    // Both are missing on older Windows: the pointer is then always drawn, the border too.
    session.SetIsCursorCaptureEnabled(options.cursor).ok();
    if !options.border {
        session.SetIsBorderRequired(false).ok();
    }
    let closed = Arc::new(AtomicBool::new(false));
    let (flag, w) = (closed.clone(), wake.clone());
    item.Closed(&TypedEventHandler::new(move |_, _| {
        flag.store(true, Ordering::Relaxed);
        w();
        Ok(())
    }))
    .map_err(|e| err("cannot watch the capture", e))?;
    let w = wake.clone();
    pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
        w();
        Ok(())
    }))
    .map_err(|e| err("cannot watch the capture's frames", e))?;
    session.StartCapture().map_err(|e| err(&format!("cannot capture `{name}`"), e))?;
    Ok(Live { pool, session, _item: item, winrt, size: Mutex::new(size), closed, window, monitor, name })
}

/// The monitors showing the desktop, with their handles, in `display`'s order.
fn monitors() -> Vec<(isize, Monitor)> {
    unsafe extern "system" fn each(handle: HMONITOR, _dc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Vec` handed to EnumDisplayMonitors below, alive for the call.
        let list = unsafe { &mut *(data.0 as *mut Vec<(isize, Monitor)>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: a monitor being enumerated; `info` is a MONITORINFOEXW with its size set.
        if unsafe { GetMonitorInfoW(handle, (&raw mut info).cast()) }.as_bool() {
            let r = info.monitorInfo.rcMonitor;
            let (mut width, mut height) =
                (u32::try_from(r.right - r.left).unwrap_or(0), u32::try_from(r.bottom - r.top).unwrap_or(0));
            // The rectangle is scaled for a process that is not DPI aware; the mode is not.
            let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
            // SAFETY: `szDevice` is null-terminated (GetMonitorInfoW wrote it); `mode` has its size set.
            if unsafe {
                EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
            }
            .as_bool()
                && mode.dmPelsWidth > 0
                && mode.dmPelsHeight > 0
            {
                (width, height) = (mode.dmPelsWidth, mode.dmPelsHeight);
            }
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            list.push((
                handle.0 as isize,
                Monitor {
                    os_name: String::from_utf16_lossy(info.szDevice.get(..len).unwrap_or_default()),
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                    width,
                    height,
                },
            ));
        }
        BOOL(1)
    }
    let mut list: Vec<(isize, Monitor)> = Vec::new();
    // SAFETY: the callback only touches `list`, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM((&raw mut list) as isize)) };
    list.sort_by(|a, b| pick::monitor_order(&a.1, &b.1));
    list
}

/// The top-level windows that can be captured, front to back: visible, titled, not hidden by
/// the desktop (another virtual desktop, a suspended app), not a tool window.
fn windows() -> Vec<(isize, WindowListing)> {
    unsafe extern "system" fn each(handle: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Vec` handed to EnumWindows below, alive for the call.
        let list = unsafe { &mut *(data.0 as *mut Vec<(isize, WindowListing)>) };
        if let Some(w) = window(handle) {
            list.push((handle.0 as isize, w));
        }
        BOOL(1)
    }
    let mut list: Vec<(isize, WindowListing)> = Vec::new();
    // SAFETY: the callback only touches `list`, which outlives the call.
    unsafe { EnumWindows(Some(each), LPARAM((&raw mut list) as isize)) }.ok();
    list
}

fn window(handle: HWND) -> Option<WindowListing> {
    // SAFETY: plain queries on a window handle being enumerated; buffers outlive the calls.
    unsafe {
        if !IsWindowVisible(handle).as_bool() {
            return None;
        }
        if GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        let cloaked_ok =
            DwmGetWindowAttribute(handle, DWMWA_CLOAKED, (&raw mut cloaked).cast(), size_of::<u32>() as u32)
                .is_ok();
        if cloaked_ok && cloaked != 0 {
            return None;
        }
        let mut text = [0u16; 512];
        let len = usize::try_from(GetWindowTextW(handle, &mut text)).unwrap_or(0).min(text.len());
        let title = String::from_utf16_lossy(text.get(..len).unwrap_or_default());
        if title.trim().is_empty() {
            return None;
        }
        let mut r = RECT::default();
        DwmGetWindowAttribute(
            handle,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast(),
            size_of::<RECT>() as u32,
        )
        .ok()?;
        let width = u32::try_from(r.right - r.left).unwrap_or(0);
        let height = u32::try_from(r.bottom - r.top).unwrap_or(0);
        if width == 0 || height == 0 {
            return None;
        }
        Some(WindowListing { title, program: program(handle), width, height })
    }
}

/// The file name of the program that owns a window, empty when Windows does not say.
fn program(handle: HWND) -> String {
    let mut pid = 0u32;
    // SAFETY: a window handle; `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(handle, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    // SAFETY: opens the process for a query only; closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return String::new();
    };
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` and `len` describe writable memory that outlives the call.
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len)
    }
    .is_ok();
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(process) }.ok();
    if !ok {
        return String::new();
    }
    let path = String::from_utf16_lossy(buffer.get(..len as usize).unwrap_or_default());
    path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

/// What can be captured now. Lists windows and monitors only; whether Windows allows capture at
/// all shows when a capture starts.
pub fn sources() -> Sources {
    let displays = penumbra_system::displays().unwrap_or_default();
    let monitors = monitors()
        .into_iter()
        .enumerate()
        .map(|(i, (_, m))| MonitorListing {
            index: i + 1,
            id: pick::monitor_id(&m, &displays),
            os_name: m.os_name,
            width: m.width,
            height: m.height,
            primary: m.primary,
        })
        .collect();
    Sources { monitors, windows: windows().into_iter().map(|(_, w)| w).collect(), error: None }
}
