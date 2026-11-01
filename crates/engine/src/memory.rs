//! The machine's memory numbers for the UI: dedicated video memory against the budget Windows
//! grants (as `gpu.info`), what wgpu's allocator holds and the process's working set (as
//! `system.process`).
//!
//! The allocator report copies one entry per allocation, too much for every frame. So a thread
//! of its own reads all three every [`INTERVAL`], only while someone asks: it starts with the
//! first [`MemoryMeter::latest`] and ends a few seconds after the last, letting go of the GPU.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_gpu::{Gpu, GpuMemory, Vram};
use penumbra_system::process::ProcessStats;

/// Time between two readings.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// The thread ends this long after the last [`MemoryMeter::latest`].
const IDLE_AFTER: Duration = Duration::from_secs(10);

/// One reading. A number is `None` where it is not known: no GPU yet, a backend without the
/// report, a system without DXGI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Memory {
    /// Dedicated video memory of this process and its budget (DXGI).
    pub vram: Option<Vram>,
    /// What wgpu's allocator holds.
    pub wgpu: Option<GpuMemory>,
    /// The process as the operating system sees it.
    pub process: Option<ProcessStats>,
    /// Why the process numbers are missing.
    pub error: Option<String>,
    /// When it was read; `None` before the first reading.
    pub at: Option<Instant>,
}

impl Memory {
    /// Video memory in use, in percent of the budget.
    pub fn vram_percent(&self) -> Option<f64> {
        let v = self.vram?;
        (v.budget > 0).then(|| v.usage as f64 * 100.0 / v.budget as f64)
    }

    /// Reads all numbers now; `gpu` is the device penumbra renders on.
    pub fn read(gpu: Option<&Gpu>) -> Memory {
        let (process, error) = match penumbra_system::process::stats() {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.0)),
        };
        Memory {
            vram: gpu.and_then(Gpu::vram),
            wgpu: gpu.and_then(Gpu::memory),
            process,
            error,
            at: Some(Instant::now()),
        }
    }
}

/// Reads [`Memory`] on a thread of its own while asked for.
pub struct MemoryMeter {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    latest: Memory,
    /// The device to read; taken from the first [`MemoryMeter::latest`] that has one.
    gpu: Option<Gpu>,
    wanted: Option<Instant>,
    running: bool,
    stop: bool,
}

impl Default for MemoryMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMeter {
    pub fn new() -> Self {
        MemoryMeter { shared: Arc::new(Shared::default()) }
    }

    /// The latest reading (empty at first); keeps the thread reading for a few seconds more.
    /// Cheap enough for every frame: a lock and a copy.
    pub fn latest(&self, gpu: Option<&Gpu>) -> Memory {
        let mut s = lock(&self.shared.state);
        if s.stop {
            return s.latest.clone();
        }
        s.wanted = Some(Instant::now());
        if s.gpu.is_none()
            && let Some(gpu) = gpu
        {
            s.gpu = Some(gpu.clone());
            // Read again at once, so the GPU's numbers do not wait for the next interval.
            self.shared.wake.notify_all();
        }
        if !s.running {
            let shared = self.shared.clone();
            match std::thread::Builder::new().name("memory".into()).spawn(move || run(&shared)) {
                Ok(_) => s.running = true,
                Err(e) => s.latest.error = Some(format!("cannot start the memory thread: {e}")),
            }
        }
        s.latest.clone()
    }

    /// Ends the thread and lets go of the GPU.
    pub fn shutdown(&self) {
        let mut s = lock(&self.shared.state);
        s.stop = true;
        s.gpu = None;
        self.shared.wake.notify_all();
    }
}

impl Drop for MemoryMeter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &Shared) {
    let mut s = lock(&shared.state);
    loop {
        let wanted = s.wanted.is_some_and(|w| w.elapsed() < IDLE_AFTER);
        if s.stop || !wanted {
            s.running = false;
            // The GPU is not kept alive by a meter nobody looks at.
            s.gpu = None;
            return;
        }
        let gpu = s.gpu.clone();
        drop(s);
        let memory = Memory::read(gpu.as_ref());
        drop(gpu);
        s = lock(&shared.state);
        s.latest = memory;
        let had_gpu = s.gpu.is_some();
        let next = Instant::now() + INTERVAL;
        while !s.stop && Instant::now() < next && s.gpu.is_some() == had_gpu {
            s = shared
                .wake
                .wait_timeout(s, next.saturating_duration_since(Instant::now()))
                .map(|(s, _)| s)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_percent_is_of_the_budget() {
        let m = Memory { vram: Some(Vram { budget: 200, usage: 50 }), ..Memory::default() };
        assert_eq!(m.vram_percent(), Some(25.0));
        assert_eq!(Memory::default().vram_percent(), None);
        let empty = Memory { vram: Some(Vram { budget: 0, usage: 50 }), ..Memory::default() };
        assert_eq!(empty.vram_percent(), None, "no budget is unknown, not a division by zero");
    }

    #[test]
    fn the_thread_reads_soon_and_stops_on_shutdown() {
        let meter = MemoryMeter::new();
        assert_eq!(meter.latest(None).at, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut m = meter.latest(None);
        while m.at.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            m = meter.latest(None);
        }
        assert!(m.at.is_some(), "a first reading within seconds");
        assert!(m.process.is_some() || m.error.is_some());
        assert_eq!(m.vram, None, "no GPU, no video memory");
        meter.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&meter.shared.state).running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!lock(&meter.shared.state).running);
    }
}
