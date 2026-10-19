//! Direct3D and the Windows objects Spout is made of. The FFI module of this crate: every
//! `unsafe` lives here, behind small owning types that close what they open.
//!
//! - [`Interop`]: a D3D11 device layered on wgpu's own D3D12 device and queue (D3D11On12), so
//!   D3D11 copies run on the same queue as wgpu's work, in submission order, with no fence of
//!   our own. Spout's textures are D3D11 shared textures; wgpu's textures take part through
//!   [`Wrapped`] resources.
//! - [`Shared`]: a D3D11 texture with a legacy shared handle, the one every Spout program opens.
//! - [`Map`], [`NamedMutex`], [`Semaphore`]: the named shared memory, mutexes and the frame
//!   counter that make up Spout's directory of senders.
//!
//! State handover with wgpu: a wrapped texture is created with the same D3D12 state on both
//! sides of the D3D11 work (copy source for a sender's staging texture, copy destination for a
//! receiver's), and the caller puts the texture into that state on the wgpu side first
//! (`CommandEncoder::transition_resources`), so wgpu's own state tracking stays right.

#![allow(unsafe_code)]

use std::ffi::{CString, c_void};
use std::marker::PhantomData;

use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D11on12::{
    D3D11_RESOURCE_FLAGS, D3D11On12CreateDevice, ID3D11On12Device,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATES,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIKeyedMutex, IDXGIResource};
use windows::Win32::System::Memory::{
    CreateFileMappingA, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingA, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueA};
use windows::Win32::System::Threading::{
    CreateMutexA, CreateSemaphoreA, OpenSemaphoreW, ReleaseMutex, ReleaseSemaphore, SEMAPHORE_MODIFY_STATE,
    SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
};
use windows::core::{HSTRING, IUnknown, Interface, PCSTR};

/// How long the lock of a shared memory map is waited for, as Spout waits (4 frames at 60 Hz).
const MAP_WAIT_MS: u32 = 67;

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {}", e.message().trim())
}

fn c_name(name: &str) -> Result<CString, String> {
    CString::new(name).map_err(|_| format!("the name {name:?} contains a null character"))
}

/// Which side of a copy a wrapped wgpu texture is on; also the D3D12 state it must be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Source,
    Dest,
}

impl Side {
    fn state(self) -> D3D12_RESOURCE_STATES {
        match self {
            Side::Source => D3D12_RESOURCE_STATE_COPY_SOURCE,
            Side::Dest => D3D12_RESOURCE_STATE_COPY_DEST,
        }
    }
}

/// A D3D11 device on wgpu's D3D12 device and queue.
pub struct Interop {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    on12: ID3D11On12Device,
}

// SAFETY: the D3D11 device is free-threaded; the immediate context is not, and is only used
// through `&self` methods of an `Interop` that its owner (one sender or receiver, cooked on one
// thread at a time) never shares.
unsafe impl Send for Interop {}

impl Interop {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, String> {
        // SAFETY: `as_hal` hands out wgpu-hal's objects, which must not be used to break wgpu's
        // invariants. Only the COM pointers are cloned (a reference each), the guards drop at
        // the end of the statement. The D3D11On12 device keeps its own references to both.
        let device12 = unsafe { device.as_hal::<Dx12>() }
            .map(|d| d.raw_device().clone())
            .ok_or("Spout needs the DirectX 12 backend, and the GPU runs on another")?;
        // SAFETY: as above.
        let queue12 = unsafe { queue.as_hal::<Dx12>() }
            .map(|q| q.as_raw().clone())
            .ok_or("Spout needs the DirectX 12 backend, and the GPU runs on another")?;
        let queue: IUnknown = queue12.cast().map_err(|e| err("cannot use the GPU queue", e))?;
        let mut device11 = None;
        let mut context = None;
        // SAFETY: valid device and queue; the out pointers live on this stack frame.
        unsafe {
            D3D11On12CreateDevice(
                &device12,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
                None,
                Some(&[Some(queue)]),
                0,
                Some(&mut device11),
                Some(&mut context),
                None,
            )
        }
        .map_err(|e| err("cannot open Direct3D 11 on the GPU", e))?;
        let device = device11.ok_or("Direct3D 11 on 12 returned no device")?;
        let context = context.ok_or("Direct3D 11 on 12 returned no context")?;
        let on12 = device.cast::<ID3D11On12Device>().map_err(|e| err("not a Direct3D 11 on 12 device", e))?;
        Ok(Self { device, context, on12 })
    }

    /// `texture` as a D3D11 resource, to be copied from or into while it is in `side`'s state.
    pub fn wrap(&self, texture: &wgpu::Texture, side: Side) -> Result<Wrapped, String> {
        // SAFETY: as in `new`; the resource is cloned, the guard dropped at once. `Wrapped`
        // holds the wgpu texture too, so its memory outlives the wrapper.
        let resource12 = unsafe { texture.as_hal::<Dx12>() }
            .map(|t| unsafe { t.raw_resource() }.clone())
            .ok_or("the texture is not a DirectX 12 texture")?;
        let flags = D3D11_RESOURCE_FLAGS::default();
        let mut wrapped: Option<ID3D11Resource> = None;
        // SAFETY: a live D3D12 resource of this device; `flags` and `wrapped` outlive the call.
        unsafe {
            self.on12.CreateWrappedResource(&resource12, &flags, side.state(), side.state(), &mut wrapped)
        }
        .map_err(|e| err("cannot share a texture with Direct3D 11", e))?;
        let resource = wrapped.ok_or("Direct3D 11 on 12 returned no texture")?;
        Ok(Wrapped { resource, _texture: texture.clone() })
    }

    /// Copies the top level of `src` into `dst`, one of them `wrapped`, and submits the copy
    /// to the GPU queue (after everything wgpu submitted before). Sizes and formats must
    /// match; D3D11 skips a copy that does not fit rather than fail.
    pub fn copy(&self, dst: &ID3D11Resource, src: &ID3D11Resource, wrapped: &Wrapped) {
        let list = [Some(wrapped.resource.clone())];
        // SAFETY: live resources of this device; the wrapped one is acquired around its use as
        // D3D11On12 requires, and the flush hands the copy to the shared queue.
        unsafe {
            self.on12.AcquireWrappedResources(&list);
            self.context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, None);
            self.on12.ReleaseWrappedResources(&list);
            self.context.Flush();
        }
    }

    /// A new shared texture to send from: the legacy shared handle other programs open.
    pub fn create_shared(&self, width: u32, height: u32, format: u32) -> Result<Shared, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT(i32::try_from(format).unwrap_or(0)),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };
        let mut texture = None;
        // SAFETY: `desc` and `texture` outlive the call.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(|e| err(&format!("cannot create a {width}x{height} shared texture"), e))?;
        let texture: ID3D11Texture2D = texture.ok_or("Direct3D 11 returned no texture")?;
        let dxgi = texture.cast::<IDXGIResource>().map_err(|e| err("the texture cannot be shared", e))?;
        // SAFETY: a live DXGI resource created shareable above.
        let handle =
            unsafe { dxgi.GetSharedHandle() }.map_err(|e| err("the texture has no shared handle", e))?;
        // Legacy shared handles are 32-bit values (Spout stores them in 32 bits).
        let handle = handle.0 as usize as u32;
        Shared::new(texture, handle)
    }

    /// The shared texture of another program's sender. A handle stored as 32 bits is tried
    /// zero-extended (as Spout writes it now) and sign-extended (as it used to).
    pub fn open_shared(&self, handle: u32) -> Result<Shared, String> {
        if handle == 0 {
            return Err("the sender has no shared texture".into());
        }
        let candidates = [handle as usize, handle as i32 as isize as usize];
        let mut last = String::new();
        for raw in candidates {
            let mut texture: Option<ID3D11Texture2D> = None;
            // SAFETY: OpenSharedResource validates the handle and fails on one it does not
            // know; `texture` outlives the call.
            match unsafe { self.device.OpenSharedResource(HANDLE(raw as *mut c_void), &mut texture) } {
                Ok(()) => {
                    if let Some(t) = texture {
                        return Shared::new(t, handle);
                    }
                }
                Err(e) => last = err("cannot open the sender's texture", e),
            }
            if candidates.first() == candidates.last() {
                break;
            }
        }
        Err(last)
    }
}

/// A wgpu texture as a D3D11 resource. Holds the wgpu texture, so the memory stays while the
/// wrapper does (the wrapper is dropped first: fields drop in order).
pub struct Wrapped {
    resource: ID3D11Resource,
    _texture: wgpu::Texture,
}

// SAFETY: a COM reference to a resource of a free-threaded device, used only through the
// `Interop` that made it.
unsafe impl Send for Wrapped {}

impl Wrapped {
    pub fn resource(&self) -> &ID3D11Resource {
        &self.resource
    }
}

/// A D3D11 shared texture: one this process sends from, or another program's it receives from.
pub struct Shared {
    resource: ID3D11Resource,
    keyed: Option<IDXGIKeyedMutex>,
    pub handle: u32,
    pub width: u32,
    pub height: u32,
    /// `DXGI_FORMAT`.
    pub format: u32,
}

// SAFETY: as for `Wrapped`.
unsafe impl Send for Shared {}

impl Shared {
    fn new(texture: ID3D11Texture2D, handle: u32) -> Result<Self, String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture; `desc` outlives the call.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.SampleDesc.Count != 1 {
            return Err(format!("the sender's texture is multisampled ({}x)", desc.SampleDesc.Count));
        }
        let keyed = if desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32 != 0 {
            Some(texture.cast::<IDXGIKeyedMutex>().map_err(|e| err("the texture's keyed mutex", e))?)
        } else {
            None
        };
        let resource = texture.cast::<ID3D11Resource>().map_err(|e| err("not a resource", e))?;
        Ok(Self {
            resource,
            keyed,
            handle,
            width: desc.Width,
            height: desc.Height,
            format: u32::try_from(desc.Format.0).unwrap_or(0),
        })
    }

    pub fn resource(&self) -> &ID3D11Resource {
        &self.resource
    }

    /// Takes the texture's keyed mutex if it has one (`Some(false)`: busy for `ms`); `None`
    /// when it has none and the sender's named mutex guards it instead.
    pub fn lock_keyed(&self, ms: u32) -> Option<Option<KeyedGuard<'_>>> {
        let keyed = self.keyed.as_ref()?;
        // SAFETY: a live keyed mutex. Called through the vtable for the raw HRESULT: a timeout
        // is a success code (WAIT_TIMEOUT), which the wrapper would report as success.
        let hr = unsafe { (Interface::vtable(keyed).AcquireSync)(Interface::as_raw(keyed), 0, ms) };
        Some((hr.0 == 0).then_some(KeyedGuard { keyed, _thread: PhantomData }))
    }
}

/// Holds a keyed mutex until dropped.
pub struct KeyedGuard<'a> {
    keyed: &'a IDXGIKeyedMutex,
    _thread: PhantomData<*const ()>,
}

impl Drop for KeyedGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: acquired with key 0 by this guard.
        unsafe { self.keyed.ReleaseSync(0) }.ok();
    }
}

/// An owned kernel handle, closed when dropped.
struct Handle(HANDLE);

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owned and not closed before.
        unsafe { CloseHandle(self.0) }.ok();
    }
}

/// A named mutex, created or opened (`CreateMutex` does both).
pub struct NamedMutex(Handle);

impl NamedMutex {
    pub fn open_or_create(name: &str) -> Result<Self, String> {
        let c = c_name(name)?;
        // SAFETY: `c` outlives the call.
        let h = unsafe { CreateMutexA(None, false, PCSTR(c.as_ptr().cast())) }
            .map_err(|e| err(&format!("cannot open the mutex {name}"), e))?;
        Ok(Self(Handle(h)))
    }

    /// The mutex for at most `ms`, `None` if another holds it that long. A mutex its last
    /// owner died holding counts as taken: the data it guards is only a texture or a list.
    pub fn lock(&self, ms: u32) -> Option<MutexGuard<'_>> {
        // SAFETY: a live mutex handle.
        let r = unsafe { WaitForSingleObject(self.0.0, ms) };
        (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED)
            .then_some(MutexGuard { mutex: self, _thread: PhantomData })
    }
}

/// Holds a named mutex until dropped, on the thread that took it (mutexes belong to threads).
pub struct MutexGuard<'a> {
    mutex: &'a NamedMutex,
    _thread: PhantomData<*const ()>,
}

impl Drop for MutexGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: owned by this thread through this guard.
        unsafe { ReleaseMutex(self.mutex.0.0) }.ok();
    }
}

/// A sender's frame counter: a semaphore whose count rises by one per frame sent.
pub struct Semaphore(Handle);

impl Semaphore {
    /// Creates the counter or opens it if it exists (a receiver may have made it first).
    pub fn open_or_create(name: &str) -> Result<Self, String> {
        let c = c_name(name)?;
        // SAFETY: `c` outlives the call.
        let h = unsafe { CreateSemaphoreA(None, 1, i32::MAX, PCSTR(c.as_ptr().cast())) }
            .map_err(|e| err(&format!("cannot create the semaphore {name}"), e))?;
        Ok(Self(Handle(h)))
    }

    /// An existing counter, `None` when the sender keeps none.
    pub fn open(name: &str) -> Option<Self> {
        // SAFETY: the HSTRING outlives the call.
        let h = unsafe {
            OpenSemaphoreW(SYNCHRONIZATION_SYNCHRONIZE | SEMAPHORE_MODIFY_STATE, false, &HSTRING::from(name))
        }
        .ok()?;
        Some(Self(Handle(h)))
    }

    /// Counts a frame: one down (always possible, the count never falls below one) and two up.
    pub fn increment(&self) {
        // SAFETY: a live semaphore handle.
        unsafe {
            if WaitForSingleObject(self.0.0, 0) == WAIT_OBJECT_0 {
                ReleaseSemaphore(self.0.0, 2, None).ok();
            }
        }
    }

    /// The count, left as it was (one down, one up); `None` if it cannot be read.
    pub fn count(&self) -> Option<i32> {
        let mut previous = 0i32;
        // SAFETY: a live semaphore handle; `previous` outlives the call.
        unsafe {
            if WaitForSingleObject(self.0.0, 0) != WAIT_OBJECT_0 {
                return None;
            }
            ReleaseSemaphore(self.0.0, 1, Some(&mut previous)).ok()?;
        }
        Some(previous)
    }
}

/// Named shared memory with its `<name>_mutex`, as Spout keeps its directory.
pub struct Map {
    // Closed after the view is unmapped (fields drop after `drop`).
    _handle: Handle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    mutex: NamedMutex,
}

// SAFETY: the view is plain memory, only touched under the named mutex through `with`.
unsafe impl Send for Map {}

impl Map {
    /// Creates the map with `len` bytes (zeroed), or opens it if it exists (then with the size
    /// its creator gave it). The flag says whether it existed.
    pub fn open_or_create(name: &str, len: usize) -> Result<(Self, bool), String> {
        let c = c_name(name)?;
        let size = u32::try_from(len).map_err(|_| format!("{len} bytes are too many for {name}"))?;
        // SAFETY: an anonymous mapping backed by the paging file; `c` outlives the call. The
        // last error is read right after, before anything else can change it.
        let (h, existed) = unsafe {
            let h = CreateFileMappingA(
                windows::Win32::Foundation::INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size,
                PCSTR(c.as_ptr().cast()),
            )
            .map_err(|e| err(&format!("cannot create the shared memory {name}"), e))?;
            (h, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        Ok((Self::view(name, Handle(h))?, existed))
    }

    /// An existing map, `None` if there is none of that name.
    pub fn open(name: &str) -> Option<Self> {
        let c = c_name(name).ok()?;
        // SAFETY: `c` outlives the call.
        let h = unsafe { OpenFileMappingA(FILE_MAP_ALL_ACCESS.0, false, PCSTR(c.as_ptr().cast())) }.ok()?;
        Self::view(name, Handle(h)).ok()
    }

    fn view(name: &str, handle: Handle) -> Result<Self, String> {
        // SAFETY: a live mapping handle; the whole mapping is viewed.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            return Err(format!("cannot map the shared memory {name}"));
        }
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `view` is mapped; `info` outlives the call. The region is the view, rounded
        // up to whole pages, all of it readable and writable.
        let got = unsafe { VirtualQuery(Some(view.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        let len = if got == 0 { 0 } else { info.RegionSize };
        let mutex = match NamedMutex::open_or_create(&format!("{name}_mutex")) {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: mapped above, not unmapped before.
                unsafe { UnmapViewOfFile(view) }.ok();
                return Err(e);
            }
        };
        Ok(Self { _handle: handle, view, len, mutex })
    }

    /// Runs `f` on the bytes while holding the map's mutex; `None` if another program held it
    /// past [`MAP_WAIT_MS`].
    pub fn with<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        let _guard = self.mutex.lock(MAP_WAIT_MS)?;
        // SAFETY: `len` bytes are mapped for the lifetime of `self`; other processes write
        // them only under the same mutex, which is held for the slice's lifetime.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.view.Value.cast::<u8>(), self.len) };
        Some(f(bytes))
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: mapped in `view`, not unmapped before; the handle closes after (field drop).
        unsafe { UnmapViewOfFile(self.view) }.ok();
    }
}

/// A DWORD under `HKEY_CURRENT_USER\Software\Leading Edge\Spout`, where Spout keeps its
/// settings; `None` if it is not set.
pub fn setting(value: &str) -> Option<u32> {
    let c = c_name(value).ok()?;
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` and `size` outlive the call and describe a DWORD.
    let r = unsafe {
        RegGetValueA(
            HKEY_CURRENT_USER,
            PCSTR(c"Software\\Leading Edge\\Spout".as_ptr().cast()),
            PCSTR(c.as_ptr().cast()),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

/// A plain D3D11 device and textures with known pixels, standing in for another Spout program.
#[cfg(test)]
pub mod testing {
    use super::*;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
        D3D11_USAGE_STAGING, D3D11CreateDevice,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIResource;

    pub struct Other {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
    }

    impl Other {
        pub fn new(software: bool) -> Self {
            let mut device = None;
            let mut context = None;
            let kind = if software { D3D_DRIVER_TYPE_WARP } else { D3D_DRIVER_TYPE_HARDWARE };
            unsafe {
                D3D11CreateDevice(
                    None,
                    kind,
                    Default::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .unwrap();
            Self { device: device.unwrap(), context: context.unwrap() }
        }

        /// A shared BGRA texture filled with `pixels` (BGRA bytes), and its legacy handle.
        pub fn shared_bgra(&self, width: u32, height: u32, pixels: &[u8]) -> (ID3D11Texture2D, u32) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT(87),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
            };
            let init = windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA {
                pSysMem: pixels.as_ptr().cast(),
                SysMemPitch: width * 4,
                SysMemSlicePitch: 0,
            };
            let mut t = None;
            unsafe { self.device.CreateTexture2D(&desc, Some(&init), Some(&mut t)) }.unwrap();
            let t: ID3D11Texture2D = t.unwrap();
            let handle = unsafe { t.cast::<IDXGIResource>().unwrap().GetSharedHandle() }.unwrap();
            unsafe { self.context.Flush() };
            (t, handle.0 as usize as u32)
        }

        /// The pixels of the shared texture behind `handle` as bytes in its format (4 bytes per
        /// pixel formats only), rows packed.
        pub fn read(&self, handle: u32) -> (D3D11_TEXTURE2D_DESC, Vec<u8>) {
            let mut src: Option<ID3D11Texture2D> = None;
            unsafe { self.device.OpenSharedResource(HANDLE(handle as usize as *mut c_void), &mut src) }
                .unwrap();
            let src = src.unwrap();
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { src.GetDesc(&mut desc) };
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }.unwrap();
            let staging: ID3D11Texture2D = staging.unwrap();
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let bytes_per = if desc.Format.0 == 10 { 8 } else { 4 };
            let mut out = Vec::new();
            unsafe {
                self.context.CopyResource(&staging, &src);
                self.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
                for y in 0..desc.Height as usize {
                    let row = std::slice::from_raw_parts(
                        mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                        desc.Width as usize * bytes_per,
                    );
                    out.extend_from_slice(row);
                }
                self.context.Unmap(&staging, 0);
            }
            (desc, out)
        }
    }
}
