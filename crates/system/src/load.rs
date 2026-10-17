//! How busy the machine is: CPU over all cores and the busiest GPU engine, the numbers Task
//! Manager shows (`system.load`, the cook time details in the status bar). On NVIDIA GPUs also
//! the GPU's power draw, temperature, clock and what holds the clock back ([`GpuSensors`]).
//!
//! Reading the GPU counters can take tens of milliseconds with many processes running, so a
//! thread of its own samples them, once a second and only while someone asks: it starts with
//! the first [`LoadMeter::latest`] and goes to sleep a few seconds after the last.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

/// Time between two samples.
pub const INTERVAL: Duration = Duration::from_secs(1);
/// The sampler stops this long after the last [`LoadMeter::latest`].
const IDLE_AFTER: Duration = Duration::from_secs(5);

/// One sample.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Load {
    /// All cores, in percent.
    pub cpu: Option<f64>,
    /// The busiest GPU engine (3D, copy, video, ...) summed over all processes, in percent, as
    /// Task Manager's GPU column.
    pub gpu: Option<f64>,
    /// Why a number is missing.
    pub error: Option<String>,
    /// The NVIDIA GPU's sensors; `None` without an NVIDIA driver.
    pub nvidia: Option<GpuSensors>,
    /// When it was taken; `None` before the first sample.
    #[serde(skip)]
    pub at: Option<Instant>,
}

/// One NVIDIA GPU's sensors, read through NVML. A value is `None` when the GPU did not report it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GpuSensors {
    /// NVML's name for the GPU, e.g. `NVIDIA GeForce RTX 3090`.
    pub name: String,
    /// NVML's index of the GPU.
    pub index: usize,
    /// Whether this is the adapter penumbra renders on (found by name). `false` means the first
    /// NVIDIA GPU was taken because none matched, or the adapter is not known yet.
    pub matched: bool,
    /// Power draw, watts.
    pub power_w: Option<f64>,
    /// The power limit the driver enforces, watts.
    pub power_limit_w: Option<f64>,
    /// The GPU die, degrees Celsius.
    pub temperature_c: Option<u32>,
    /// Graphics clock, MHz.
    pub clock_mhz: Option<u32>,
    /// What holds the clock back right now (`power cap`, `thermal (hardware)`, `idle`, ...);
    /// empty when nothing does.
    pub limited_by: Option<Vec<String>>,
}

/// Samples [`Load`] on a thread of its own while asked for.
pub struct LoadMeter {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    latest: Load,
    /// The name of the adapter penumbra renders on, to pick the matching NVIDIA GPU.
    adapter: Option<String>,
    wanted: Option<Instant>,
    started: bool,
    stop: bool,
}

impl Default for LoadMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadMeter {
    pub fn new() -> Self {
        LoadMeter { shared: Arc::new(Shared::default()) }
    }

    /// The latest sample (empty at first); keeps the sampler running for a few seconds more.
    pub fn latest(&self) -> Load {
        let mut s = lock(&self.shared.state);
        if s.stop {
            return s.latest.clone();
        }
        s.wanted = Some(Instant::now());
        if !s.started {
            let shared = self.shared.clone();
            match std::thread::Builder::new().name("load".into()).spawn(move || run(&shared)) {
                Ok(_) => s.started = true,
                Err(e) => s.latest.error = Some(format!("cannot start the load thread: {e}")),
            }
        }
        self.shared.wake.notify_all();
        s.latest.clone()
    }

    /// Names the adapter penumbra renders on (as the graphics API reports it), so the sensors
    /// come from that GPU when there are several.
    pub fn set_adapter(&self, name: &str) {
        lock(&self.shared.state).adapter = Some(name.to_string());
    }

    /// Waits up to `timeout` for a sample newer than `after` (asking for it).
    pub fn wait_newer(&self, after: Option<Instant>, timeout: Duration) -> Load {
        let deadline = Instant::now() + timeout;
        let mut load = self.latest();
        while load.at.is_none_or(|at| after.is_some_and(|a| at <= a)) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let s = lock(&self.shared.state);
            let (s, _) = self
                .shared
                .wake
                .wait_timeout(s, deadline.saturating_duration_since(now))
                .unwrap_or_else(PoisonError::into_inner);
            load = s.latest.clone();
        }
        load
    }

    /// Ends the sampler thread (it finishes a sample under way first).
    pub fn shutdown(&self) {
        lock(&self.shared.state).stop = true;
        self.shared.wake.notify_all();
    }
}

impl Drop for LoadMeter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &Shared) {
    let mut sampler: Option<Sampler> = None;
    // Kept for the thread's life once loaded: starting NVML costs more than keeping it. A
    // failure is final too, so a machine without NVIDIA says so once in the log.
    let mut nvml: Option<Option<crate::nvml::Nvml>> = None;
    let mut s = lock(&shared.state);
    loop {
        if s.stop {
            return;
        }
        let wanted = s.wanted.is_some_and(|w| w.elapsed() < IDLE_AFTER);
        if !wanted {
            // Closing the counters costs nothing worth keeping them for.
            sampler = None;
            s = shared.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let adapter = s.adapter.clone();
        drop(s);
        let nvml = nvml.get_or_insert_with(|| match crate::nvml::Nvml::open() {
            Ok(n) => Some(n),
            Err(e) => {
                log::info!("no NVIDIA GPU sensors: {e}");
                None
            }
        });
        let load = match &mut sampler {
            Some(sm) => sm.sample(),
            None => match Sampler::open() {
                // Rates need two readings; the first one comes after an interval.
                Ok(sm) => {
                    sampler = Some(sm);
                    None
                }
                Err(e) => Some(Load { error: Some(e.0), at: Some(Instant::now()), ..Load::default() }),
            },
        };
        let load = load.map(|mut l| {
            l.nvidia = nvml.as_ref().and_then(|n| sensors(n, adapter.as_deref()));
            l
        });
        s = lock(&shared.state);
        if let Some(load) = load {
            s.latest = load;
            shared.wake.notify_all();
        }
        let next = Instant::now() + INTERVAL;
        while !s.stop && Instant::now() < next {
            s = shared
                .wake
                .wait_timeout(s, next.saturating_duration_since(Instant::now()))
                .map(|(s, _)| s)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

#[cfg(windows)]
struct Sampler(crate::windows::Pdh);

#[cfg(windows)]
impl Sampler {
    fn open() -> Result<Self, penumbra_core::Error> {
        crate::windows::Pdh::open().map(Sampler)
    }

    fn sample(&mut self) -> Option<Load> {
        let (cpu, gpu) = self.0.sample();
        let error = [cpu.as_ref().err(), gpu.as_ref().err()]
            .into_iter()
            .flatten()
            .map(|e| e.0.clone())
            .reduce(|a, b| format!("{a}; {b}"));
        Some(Load { cpu: cpu.ok(), gpu: gpu.ok(), error, at: Some(Instant::now()), nvidia: None })
    }
}

#[cfg(not(windows))]
struct Sampler;

#[cfg(not(windows))]
impl Sampler {
    fn open() -> Result<Self, penumbra_core::Error> {
        Err(penumbra_core::Error::new("CPU and GPU load are only measured on Windows so far"))
    }

    fn sample(&mut self) -> Option<Load> {
        None
    }
}

/// The sensors of the NVIDIA GPU named `adapter`, else of the first one.
fn sensors(nvml: &crate::nvml::Nvml, adapter: Option<&str>) -> Option<GpuSensors> {
    let names: Vec<&str> = nvml.names().collect();
    let found = adapter.and_then(|a| pick(&names, a));
    let index = found.unwrap_or(0);
    let name = names.get(index)?.to_string();
    let r = nvml.read(index)?;
    let watts = |mw: Option<u32>| mw.map(|mw| f64::from(mw) / 1000.0);
    Some(GpuSensors {
        name,
        index,
        matched: found.is_some(),
        power_w: watts(r.power_mw),
        power_limit_w: watts(r.power_limit_mw),
        temperature_c: r.temperature_c,
        clock_mhz: r.clock_mhz,
        limited_by: r.reasons.map(|v| v.into_iter().map(str::to_string).collect()),
    })
}

/// The index of the GPU in `names` that is `adapter`. Names are compared without case and
/// without a leading `NVIDIA` (older drivers leave it out on one side).
pub fn pick(names: &[&str], adapter: &str) -> Option<usize> {
    let norm = |s: &str| {
        let s = s.trim().to_lowercase();
        s.strip_prefix("nvidia ").map(str::to_string).unwrap_or(s)
    };
    let adapter = norm(adapter);
    names.iter().position(|n| norm(n) == adapter)
}

/// Task Manager's number for one GPU: per engine (an adapter's 3D, copy or video engine) the
/// sum over the processes using it, then the busiest engine. `items` are the instances of
/// `\GPU Engine(*)\Utilization Percentage`, named like
/// `pid_1234_luid_0x0_0xD1A2_phys_0_eng_0_engtype_3D`.
pub fn busiest_engine<'a>(items: impl IntoIterator<Item = (&'a str, f64)>) -> Option<f64> {
    let mut engines: std::collections::BTreeMap<&str, f64> = std::collections::BTreeMap::new();
    for (name, value) in items {
        let engine = name.find("luid_").and_then(|i| name.get(i..)).unwrap_or(name);
        *engines.entry(engine).or_default() += value;
    }
    engines.into_values().reduce(f64::max).map(|v| v.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_sum_over_processes_and_the_busiest_counts() {
        let items = [
            ("pid_1_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 30.0),
            ("pid_2_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 25.0),
            ("pid_1_luid_0x0_0xA_phys_0_eng_1_engtype_Copy", 40.0),
            ("pid_3_luid_0x0_0xA_phys_0_eng_1_engtype_Copy", 70.0),
        ];
        assert_eq!(busiest_engine(items), Some(100.0));
        assert_eq!(busiest_engine(items.into_iter().take(3)), Some(55.0));
        assert_eq!(busiest_engine([]), None);
    }

    #[test]
    fn the_rendering_adapter_is_found_by_name() {
        let names = ["NVIDIA RTX A4000", "NVIDIA GeForce RTX 3090"];
        assert_eq!(pick(&names, "NVIDIA GeForce RTX 3090"), Some(1));
        assert_eq!(pick(&names, "GeForce RTX 3090"), Some(1));
        assert_eq!(pick(&names, "nvidia rtx a4000 "), Some(0));
        assert_eq!(pick(&names, "Intel(R) UHD Graphics 770"), None);
    }

    /// Takes two seconds and reads the machine's counters.
    #[cfg(windows)]
    #[test]
    fn the_machine_has_a_load() {
        let meter = LoadMeter::new();
        let load = meter.wait_newer(None, Duration::from_secs(5));
        assert!(load.at.is_some(), "{load:?}");
        let cpu = load.cpu.unwrap_or(-1.0);
        assert!((0.0..=100.0).contains(&cpu), "{load:?}");
        meter.shutdown();
    }
}
