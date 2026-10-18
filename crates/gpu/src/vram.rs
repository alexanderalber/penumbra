//! How much GPU memory the system grants this process, and how much of it the process uses.
//! On Windows (DX12) DXGI reports both; elsewhere nothing is known and callers fall back to
//! fixed sizes.
//!
//! The DXGI budget is what Windows lets this process keep in dedicated video memory right now.
//! It is smaller than the card's memory and shrinks while other programs hold VRAM. Past it,
//! allocations fail or get evicted, and drivers have been seen to suspend the whole device
//! after a run of failed allocations; [`CREATE_PERCENT`] keeps wgpu from getting that far.

/// Percent of the DXGI budget past which wgpu refuses to create textures and buffers with a
/// clean out-of-memory error, instead of passing the request on to the driver (see
/// [`memory_thresholds`]). The last tenth is left for what the check does not cover: the
/// driver's own allocations, swap chains, descriptor heaps, staging memory.
pub const CREATE_PERCENT: u8 = 90;

/// The thresholds every wgpu instance of penumbra's is created with: resource creation stops
/// at [`CREATE_PERCENT`] of the budget. No threshold for deliberately losing the device: a
/// lost device ends the app, a refused allocation only fails one node.
pub fn memory_thresholds() -> wgpu::MemoryBudgetThresholds {
    wgpu::MemoryBudgetThresholds { for_resource_creation: Some(CREATE_PERCENT), for_device_loss: None }
}

/// Dedicated video memory as DXGI reports it for this process, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vram {
    /// What the system grants the process now.
    pub budget: u64,
    /// What the process holds now.
    pub usage: u64,
}

impl Vram {
    /// Bytes still creatable before wgpu refuses ([`CREATE_PERCENT`] of the budget, less the
    /// usage), minus `reserve`.
    pub fn creatable(&self, reserve: u64) -> u64 {
        (self.budget / 100 * u64::from(CREATE_PERCENT)).saturating_sub(self.usage).saturating_sub(reserve)
    }
}

/// The adapter's figures, `None` off Windows, on another backend, or when DXGI does not answer.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn query(adapter: &wgpu::Adapter) -> Option<Vram> {
    use windows::Win32::Graphics::Dxgi::DXGI_MEMORY_SEGMENT_GROUP_LOCAL;
    // SAFETY: `as_hal` hands out wgpu-hal's adapter, which must not be used to break wgpu's
    // invariants. Only a read-only DXGI query is made on it, and the guard is dropped before
    // returning.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }?;
    let info = hal.raw_adapter().query_video_memory_info(DXGI_MEMORY_SEGMENT_GROUP_LOCAL).ok()?;
    Some(Vram { budget: info.Budget, usage: info.CurrentUsage })
}

#[cfg(not(windows))]
pub(crate) fn query(_adapter: &wgpu::Adapter) -> Option<Vram> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creatable_leaves_the_last_tenth_and_the_reserve() {
        let gib = 1u64 << 30;
        let v = Vram { budget: 10 * gib, usage: 2 * gib };
        assert_eq!(v.creatable(0), 10 * gib / 100 * 90 - 2 * gib);
        assert_eq!(v.creatable(gib), v.creatable(0) - gib);
        assert_eq!(Vram { budget: gib, usage: 2 * gib }.creatable(0), 0, "over budget is zero, not a wrap");
    }

    /// Fills the real GPU up to the creation threshold, in chunks of 256 MiB, and checks that
    /// wgpu then refuses cleanly instead of losing the device. Heavy (most of the VRAM for a
    /// moment), so only by hand: `cargo test --release -p penumbra-gpu -- --ignored threshold`.
    #[test]
    #[ignore]
    fn the_threshold_refuses_cleanly_on_the_hardware() {
        let gpu = crate::Gpu::headless(crate::AdapterChoice::Hardware).unwrap();
        let Some(start) = gpu.vram() else { return };
        let mut held = Vec::new();
        let mut refused = None;
        for _ in 0..(start.budget / (256 << 20) + 8) {
            let (t, error) = gpu.scoped(|| {
                gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("fill"),
                    size: wgpu::Extent3d { width: 4096, height: 4096, depth_or_array_layers: 2 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: crate::FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
            });
            if let Some(e) = error {
                refused = Some(e);
                break;
            }
            held.push(t);
        }
        let at = gpu.vram().unwrap();
        eprintln!(
            "start {start:?}; {} chunks of 256 MiB held; refused at {at:?} ({:.1} % of the budget): {refused:?}",
            held.len(),
            at.usage as f64 * 100.0 / at.budget as f64
        );
        assert!(refused.is_some_and(|e| e.to_lowercase().contains("memory")));
        assert!(!gpu.is_lost());
        drop(held);
        gpu.settle(std::time::Duration::from_secs(5));
        let after = gpu.vram().unwrap();
        eprintln!("after dropping: {after:?}");
        let (_t, error) = gpu.scoped(|| crate::Tex::new(&gpu, 64, 64, "after"));
        assert_eq!(error, None);
        assert!(!gpu.is_lost());
    }

    #[test]
    fn the_hardware_answers_where_there_is_dx12() {
        let Some(gpu) = crate::testing::gpu() else { return };
        // WARP is a DXGI adapter too; its figures only have to be plausible.
        if let Some(v) = gpu.vram() {
            assert!(v.budget > 0, "{v:?}");
        }
    }
}
