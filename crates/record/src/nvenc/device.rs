//! The Direct3D 11 device an NVENC session is opened on. The public items are safe; the device
//! handle stays in this module and the session.

use std::ffi::c_void;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
use windows::core::Interface;

/// NVIDIA's PCI vendor id.
const NVIDIA: u32 = 0x10DE;

/// A Direct3D 11 device on the first NVIDIA adapter. Its own device, not the show's wgpu
/// device: the pictures reach the encoder through system memory.
pub struct Device(ID3D11Device);

impl Device {
    pub const KIND: u32 = super::ffi::NV_ENC_DEVICE_TYPE_DIRECTX;

    pub fn new() -> Result<Self, String> {
        nvidia_device().map(Self)
    }

    pub fn as_raw(&self) -> *mut c_void {
        self.0.as_raw()
    }
}

fn nvidia_device() -> Result<ID3D11Device, String> {
    // SAFETY: plain COM / D3D calls with valid out-pointers; the adapter outlives the call.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("no DXGI: {e}"))?;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.VendorId != NVIDIA {
                continue;
            }
            let mut device = None;
            let adapter: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
            if D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .is_ok()
                && let Some(d) = device
            {
                return Ok(d);
            }
        }
    }
    Err("no NVIDIA graphics card".into())
}
