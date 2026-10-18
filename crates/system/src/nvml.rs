//! NVIDIA's management library (NVML), loaded at runtime: power draw, power limit, temperature,
//! graphics clock and why the clocks are held back, per NVIDIA GPU. The library ships with the
//! driver (on Windows `nvml.dll` in System32, older drivers put it in `NVSMI`), so machines
//! without an NVIDIA driver simply have none and [`Nvml::open`] fails.
//!
//! This module and the Windows calls hold the crate's `unsafe`. Every function is looked up
//! once with its documented signature, every call's status is checked, and a value is only used
//! when its call succeeded; one failing value does not hide the others.

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};

/// `nvmlDevice_t`: an opaque handle, valid until `nvmlShutdown`.
type Handle = *mut c_void;
type Status = i32;
type InitFn = unsafe extern "C" fn() -> Status;
type CountFn = unsafe extern "C" fn(*mut u32) -> Status;
type HandleFn = unsafe extern "C" fn(u32, *mut Handle) -> Status;
type NameFn = unsafe extern "C" fn(Handle, *mut c_char, u32) -> Status;
type ValueFn = unsafe extern "C" fn(Handle, *mut u32) -> Status;
type KindValueFn = unsafe extern "C" fn(Handle, u32, *mut u32) -> Status;
type ReasonsFn = unsafe extern "C" fn(Handle, *mut u64) -> Status;
type ErrorStringFn = unsafe extern "C" fn(Status) -> *const c_char;

const SUCCESS: Status = 0;
/// `NVML_TEMPERATURE_GPU`.
const TEMPERATURE_GPU: u32 = 0;
/// `NVML_CLOCK_GRAPHICS`.
const CLOCK_GRAPHICS: u32 = 0;
/// `NVML_DEVICE_NAME_V2_BUFFER_SIZE`.
const NAME_SIZE: usize = 96;

/// What held the clocks back (`nvmlClocksEventReasons`), bit and name.
const REASONS: [(u64, &str); 9] = [
    (0x1, "idle"),
    (0x2, "application clocks"),
    (0x4, "power cap"),
    (0x8, "hardware slowdown"),
    (0x10, "sync boost"),
    (0x20, "thermal (software)"),
    (0x40, "thermal (hardware)"),
    (0x80, "power brake"),
    (0x100, "display clock"),
];

/// One GPU's readings; `None` where its call failed or the GPU does not report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Milliwatts.
    pub power_mw: Option<u32>,
    /// The power limit the driver enforces, milliwatts.
    pub power_limit_mw: Option<u32>,
    /// Degrees Celsius, the GPU die.
    pub temperature_c: Option<u32>,
    /// Graphics clock, MHz.
    pub clock_mhz: Option<u32>,
    /// What holds the clocks back, by name; empty when nothing does.
    pub reasons: Option<Vec<&'static str>>,
}

/// NVML, initialised, with the GPUs it found. Shut down when dropped.
pub struct Nvml {
    shutdown: InitFn,
    power: Option<ValueFn>,
    power_limit: Option<ValueFn>,
    temperature: Option<KindValueFn>,
    clock: Option<KindValueFn>,
    reasons: Option<ReasonsFn>,
    devices: Vec<(Handle, String)>,
    // Last, so the functions above are never called after the library is unloaded.
    _lib: libloading::Library,
}

impl Nvml {
    /// Loads and initialises NVML; fails without an NVIDIA driver or GPU.
    pub fn open() -> Result<Nvml, String> {
        let lib = load()?;
        // SAFETY: each symbol is looked up with the signature NVML's header (nvml.h) gives it;
        // the copied function pointers stay valid because `lib` lives in the returned struct and
        // is dropped last.
        unsafe {
            let init =
                *lib.get::<InitFn>(b"nvmlInit_v2\0").map_err(|e| format!("NVML has no nvmlInit_v2 ({e})"))?;
            let shutdown = *lib
                .get::<InitFn>(b"nvmlShutdown\0")
                .map_err(|e| format!("NVML has no nvmlShutdown ({e})"))?;
            let error_string = lib.get::<ErrorStringFn>(b"nvmlErrorString\0").ok().map(|f| *f);
            let describe = |status: Status| -> String {
                let text = error_string.map(|f| f(status)).filter(|p| !p.is_null());
                match text {
                    Some(p) => format!("{} ({status})", CStr::from_ptr(p).to_string_lossy()),
                    None => format!("status {status}"),
                }
            };
            let status = init();
            if status != SUCCESS {
                return Err(format!("NVML did not start: {}", describe(status)));
            }
            let count = lib.get::<CountFn>(b"nvmlDeviceGetCount_v2\0").ok().map(|f| *f);
            let handle = lib.get::<HandleFn>(b"nvmlDeviceGetHandleByIndex_v2\0").ok().map(|f| *f);
            let name = lib.get::<NameFn>(b"nvmlDeviceGetName\0").ok().map(|f| *f);
            let mut devices = Vec::new();
            if let (Some(count), Some(handle)) = (count, handle) {
                let mut n = 0u32;
                if count(&raw mut n) == SUCCESS {
                    // More than a few dozen would be a broken answer, not a machine.
                    for i in 0..n.min(64) {
                        let mut h: Handle = std::ptr::null_mut();
                        if handle(i, &raw mut h) != SUCCESS || h.is_null() {
                            continue;
                        }
                        let mut buf = [0 as c_char; NAME_SIZE];
                        let text = match name {
                            Some(f) if f(h, buf.as_mut_ptr(), NAME_SIZE as u32) == SUCCESS => {
                                let bytes: Vec<u8> = buf.iter().map(|&c| c as u8).collect();
                                CStr::from_bytes_until_nul(&bytes)
                                    .map(|s| s.to_string_lossy().trim().to_string())
                                    .unwrap_or_default()
                            }
                            _ => String::new(),
                        };
                        devices.push((h, if text.is_empty() { format!("GPU {i}") } else { text }));
                    }
                }
            }
            if devices.is_empty() {
                shutdown();
                return Err("NVML found no GPU".into());
            }
            let reasons = lib
                .get::<ReasonsFn>(b"nvmlDeviceGetCurrentClocksEventReasons\0")
                .or_else(|_| lib.get::<ReasonsFn>(b"nvmlDeviceGetCurrentClocksThrottleReasons\0"))
                .ok()
                .map(|f| *f);
            Ok(Nvml {
                shutdown,
                power: lib.get::<ValueFn>(b"nvmlDeviceGetPowerUsage\0").ok().map(|f| *f),
                power_limit: lib.get::<ValueFn>(b"nvmlDeviceGetEnforcedPowerLimit\0").ok().map(|f| *f),
                temperature: lib.get::<KindValueFn>(b"nvmlDeviceGetTemperature\0").ok().map(|f| *f),
                clock: lib.get::<KindValueFn>(b"nvmlDeviceGetClockInfo\0").ok().map(|f| *f),
                reasons,
                devices,
                _lib: lib,
            })
        }
    }

    /// The GPUs' names, by NVML index.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.devices.iter().map(|(_, name)| name.as_str())
    }

    /// Reads GPU `index` (as in [`Nvml::names`]); `None` for an index NVML did not list.
    pub fn read(&self, index: usize) -> Option<Reading> {
        let (h, _) = *self.devices.get(index)?;
        // SAFETY: `h` came from nvmlDeviceGetHandleByIndex_v2 and NVML is initialised until
        // `self` drops; every out-pointer is a live local of the type the header names.
        unsafe {
            let value = |f: Option<ValueFn>| {
                let mut v = 0u32;
                f.filter(|f| f(h, &raw mut v) == SUCCESS).map(|_| v)
            };
            let kind_value = |f: Option<KindValueFn>, kind: u32| {
                let mut v = 0u32;
                f.filter(|f| f(h, kind, &raw mut v) == SUCCESS).map(|_| v)
            };
            let mut bits = 0u64;
            let reasons = self.reasons.filter(|f| f(h, &raw mut bits) == SUCCESS).map(|_| reason_names(bits));
            Some(Reading {
                power_mw: value(self.power),
                power_limit_mw: value(self.power_limit),
                temperature_c: kind_value(self.temperature, TEMPERATURE_GPU),
                clock_mhz: kind_value(self.clock, CLOCK_GRAPHICS),
                reasons,
            })
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the successful nvmlInit_v2 in `open`; the handles are not used after.
        unsafe {
            (self.shutdown)();
        }
    }
}

/// The names of the bits set in an `nvmlClocksEventReasons` mask; unknown bits are left out.
pub fn reason_names(bits: u64) -> Vec<&'static str> {
    REASONS.iter().filter(|(bit, _)| bits & bit != 0).map(|(_, name)| *name).collect()
}

#[cfg(windows)]
fn load() -> Result<libloading::Library, String> {
    use libloading::os::windows::{LOAD_LIBRARY_SEARCH_SYSTEM32, Library};
    // SAFETY: loading runs the library's initialisers; both places are the NVIDIA driver's own
    // files, and System32 is searched alone, not the working directory.
    let system = unsafe { Library::load_with_flags("nvml.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) };
    match system {
        Ok(lib) => Ok(lib.into()),
        Err(first) => {
            let program_files =
                std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
            let path = std::path::Path::new(&program_files).join("NVIDIA Corporation\\NVSMI\\nvml.dll");
            if !path.is_file() {
                return Err(format!("no NVML ({first})"));
            }
            // SAFETY: as above, by full path.
            unsafe { Library::load_with_flags(&path, 0) }
                .map(Into::into)
                .map_err(|e| format!("cannot load {}: {e}", path.display()))
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn load() -> Result<libloading::Library, String> {
    // SAFETY: loading runs the library's initialisers; it is the NVIDIA driver's own library.
    unsafe { libloading::Library::new("libnvidia-ml.so.1") }.map_err(|e| format!("no NVML ({e})"))
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn load() -> Result<libloading::Library, String> {
    Err("NVML is not available on this system".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reasons_are_named_by_bit() {
        assert_eq!(super::reason_names(0), Vec::<&str>::new());
        assert_eq!(super::reason_names(0x4 | 0x40), vec!["power cap", "thermal (hardware)"]);
        assert_eq!(super::reason_names(1 << 40), Vec::<&str>::new());
    }

    /// Reads the machine's GPUs where an NVIDIA driver is installed; passes elsewhere.
    #[test]
    fn an_nvidia_machine_reports_its_gpu() {
        let Ok(nvml) = super::Nvml::open() else { return };
        assert!(nvml.names().count() > 0);
        let r = nvml.read(0).unwrap_or_default();
        assert!(r.temperature_c.is_some_and(|t| t < 150), "{r:?}");
        assert!(nvml.read(999).is_none());
    }
}
