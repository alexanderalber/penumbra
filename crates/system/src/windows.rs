//! The Windows calls behind [`crate::display`] and [`crate::checks`]: display configuration
//! (`QueryDisplayConfig`) and the current user's registry. With NVML's loader the crate's only
//! `unsafe`; every call checks its status code and every buffer is sized by the API or by its
//! type.
#![allow(unsafe_code)]

use std::ffi::c_void;

use penumbra_core::Error;
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use crate::display::RawDisplay;

/// Every active display path: source (the desktop's `\\.\DISPLAYn`) and target (the monitor on
/// a connector).
pub fn displays() -> Result<Vec<RawDisplay>, Error> {
    Ok(query(QDC_ONLY_ACTIVE_PATHS)?.iter().map(raw_display).collect())
}

/// The connectors of every adapter, connected or not: `(adapter, target id)` of every path the
/// adapters could drive, deduplicated and sorted.
pub fn connectors() -> Result<Vec<(i64, u32)>, Error> {
    let mut list: Vec<(i64, u32)> =
        query(QDC_ALL_PATHS)?.iter().map(|p| (luid(p.targetInfo.adapterId), p.targetInfo.id)).collect();
    list.sort_unstable();
    list.dedup();
    Ok(list)
}

fn luid(l: LUID) -> i64 {
    (i64::from(l.HighPart) << 32) | i64::from(l.LowPart)
}

fn query(flags: u32) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>, Error> {
    // The configuration can change between sizing and querying; retry a few times.
    for _ in 0..4 {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: two valid out pointers.
        let status = unsafe { GetDisplayConfigBufferSizes(flags, &mut n_paths, &mut n_modes) };
        if status != ERROR_SUCCESS {
            return Err(Error::new(format!("GetDisplayConfigBufferSizes failed ({status})")));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        // SAFETY: the arrays hold exactly the counts passed; the topology pointer may be null
        // without QDC_DATABASE_CURRENT.
        let status = unsafe {
            QueryDisplayConfig(
                flags,
                &mut n_paths,
                paths.as_mut_ptr(),
                &mut n_modes,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(Error::new(format!("QueryDisplayConfig failed ({status})")));
        }
        paths.truncate(n_paths as usize);
        return Ok(paths);
    }
    Err(Error::new("the display configuration kept changing while being read"))
}

fn raw_display(path: &DISPLAYCONFIG_PATH_INFO) -> RawDisplay {
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the header names the struct's own size and type.
    let source_ok = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } == 0;
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: as above.
    let target_ok = unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0;
    let rate = path.targetInfo.refreshRate;
    RawDisplay {
        os_name: if source_ok { wide(&source.viewGdiDeviceName) } else { String::new() },
        monitor: if target_ok { wide(&target.monitorFriendlyDeviceName) } else { String::new() },
        technology: path.targetInfo.outputTechnology,
        adapter: luid(path.targetInfo.adapterId),
        target: path.targetInfo.id,
        edid: target_ok.then_some((target.edidManufactureId, target.edidProductCodeId)),
        device_path: if target_ok { wide(&target.monitorDevicePath) } else { String::new() },
        refresh: (rate.Denominator != 0).then_some((rate.Numerator, rate.Denominator)),
    }
}

/// A NUL-terminated UTF-16 buffer as a string.
fn wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or_default())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A DWORD under `HKEY_CURRENT_USER`; `None` when the key or value does not exist.
pub fn get_dword(key: &str, name: &str) -> Result<Option<u32>, Error> {
    let (k, n) = (to_wide(key), to_wide(name));
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: NUL-terminated strings, an out buffer of `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut value).cast::<c_void>(),
            &mut size,
        )
    };
    match status {
        ERROR_SUCCESS => Ok(Some(value)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        other => Err(Error::new(format!("cannot read HKCU\\{key}\\{name} (error {other})"))),
    }
}

/// Writes a DWORD under `HKEY_CURRENT_USER`, creating the key if needed.
pub fn set_dword(key: &str, name: &str, value: u32) -> Result<(), Error> {
    let (k, n) = (to_wide(key), to_wide(name));
    // SAFETY: NUL-terminated strings, a 4-byte value.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            REG_DWORD,
            (&raw const value).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::new(format!("cannot write HKCU\\{key}\\{name} (error {status})")))
    }
}

/// Deletes a value under `HKEY_CURRENT_USER`; a missing one is fine.
pub fn delete_value(key: &str, name: &str) -> Result<(), Error> {
    let (k, n) = (to_wide(key), to_wide(name));
    // SAFETY: NUL-terminated strings.
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr()) };
    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        other => Err(Error::new(format!("cannot delete HKCU\\{key}\\{name} (error {other})"))),
    }
}

/// Memory, handles and threads of this process (`system.process`, the soak test).
pub fn process_stats() -> Result<crate::process::ProcessStats, Error> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    // SAFETY: the pseudo handle of this process; needs no closing.
    let process = unsafe { GetCurrentProcess() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: size, ..Default::default() };
    // SAFETY: the EX struct starts with the plain one, and `size` says how large it is.
    let ok =
        unsafe { GetProcessMemoryInfo(process, (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(), size) };
    if ok == 0 {
        return Err(Error::new(format!("GetProcessMemoryInfo failed: {}", std::io::Error::last_os_error())));
    }
    let mut handles = 0u32;
    // SAFETY: a valid out pointer.
    if unsafe { GetProcessHandleCount(process, &mut handles) } == 0 {
        return Err(Error::new(format!("GetProcessHandleCount failed: {}", std::io::Error::last_os_error())));
    }
    Ok(crate::process::ProcessStats {
        working_set: counters.WorkingSetSize as u64,
        peak_working_set: counters.PeakWorkingSetSize as u64,
        private_bytes: counters.PrivateUsage as u64,
        handles: u64::from(handles),
        threads: thread_count().unwrap_or(0),
    })
}

/// Threads of this process, from a ToolHelp snapshot of all threads.
fn thread_count() -> Result<u64, Error> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    // SAFETY: plain calls; the snapshot handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Err(Error::new("CreateToolhelp32Snapshot failed"));
    }
    // SAFETY: no arguments.
    let me = unsafe { GetCurrentProcessId() };
    let mut entry =
        THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
    let mut n = 0u64;
    // SAFETY: a valid snapshot and an entry whose size is set.
    let mut more = unsafe { Thread32First(snap, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == me {
            n += 1;
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snap, &mut entry) } != 0;
    }
    // SAFETY: the handle came from CreateToolhelp32Snapshot and is closed once.
    unsafe { CloseHandle(snap) };
    Ok(n)
}

/// Raises the calling thread to `THREAD_PRIORITY_HIGHEST`; false when Windows refuses.
pub fn raise_thread_priority() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };

    // SAFETY: `GetCurrentThread` returns a pseudo handle that is always valid for the calling
    // thread and needs no closing; `SetThreadPriority` only reads it.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) != 0 }
}

/// Whether this process is the only one attached to its console. A terminal attaches itself (or
/// its shell) too, so a count of one means the console was created for this process alone.
pub fn own_console() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;

    let mut ids = [0u32; 2];
    // SAFETY: a buffer of `ids.len()` process ids; the call writes at most that many and returns
    // the total count (0 on failure or without a console).
    let n = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    n == 1
}

/// Puts this process into a new job object that limits its committed memory to `bytes`: an
/// allocation beyond fails instead of taking the machine down (for measurement runs).
pub fn cap_memory(bytes: u64) -> Result<(), Error> {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: no security attributes, no name. The handle stays open for the life of the
    // process (closing it would not lift the limit; keeping it is simpler).
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(Error::new(format!("CreateJobObject failed: {}", std::io::Error::last_os_error())));
    }
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
    let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    // SAFETY: the struct and its size match the information class.
    let ok = unsafe {
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const info).cast(), size)
    };
    if ok == 0 {
        return Err(Error::new(format!(
            "SetInformationJobObject failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: a job handle and the pseudo handle of this process.
    if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
        return Err(Error::new(format!(
            "AssignProcessToJobObject failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Performance counters for [`crate::load`]: CPU over all cores and the GPU engines. Counter
/// paths are added by their English names, which work on every display language.
pub struct Pdh {
    query: windows_sys::Win32::System::Performance::PDH_HQUERY,
    cpu: windows_sys::Win32::System::Performance::PDH_HCOUNTER,
    /// `None` where the system has no GPU counters (older Windows, some drivers).
    gpu: Option<windows_sys::Win32::System::Performance::PDH_HCOUNTER>,
}

impl Pdh {
    pub fn open() -> Result<Pdh, Error> {
        use windows_sys::Win32::System::Performance::{
            PdhAddEnglishCounterW, PdhCollectQueryData, PdhOpenQueryW,
        };

        let mut query = std::ptr::null_mut();
        // SAFETY: no data source (live counters), a valid out pointer.
        let status = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) };
        if status != 0 {
            return Err(Error::new(format!("PdhOpenQuery failed (0x{status:08X})")));
        }
        // From here the query is closed by Drop, also on the error paths.
        let mut pdh = Pdh { query, cpu: std::ptr::null_mut(), gpu: None };
        let add = |path: &str| {
            let path = to_wide(path);
            let mut counter = std::ptr::null_mut();
            // SAFETY: an open query, a NUL-terminated path, a valid out pointer.
            let status = unsafe { PdhAddEnglishCounterW(pdh.query, path.as_ptr(), 0, &mut counter) };
            (status == 0).then_some(counter).ok_or(status)
        };
        pdh.cpu = add(r"\Processor(_Total)\% Processor Time")
            .map_err(|s| Error::new(format!("no CPU counter (0x{s:08X})")))?;
        pdh.gpu = add(r"\GPU Engine(*)\Utilization Percentage").ok();
        // SAFETY: an open query. The first collection is only the baseline for the rates.
        unsafe { PdhCollectQueryData(pdh.query) };
        Ok(pdh)
    }

    /// CPU and GPU in percent since the previous call (or since [`Pdh::open`]).
    pub fn sample(&mut self) -> (Result<f64, Error>, Result<f64, Error>) {
        use windows_sys::Win32::System::Performance::{
            PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PdhCollectQueryData, PdhGetFormattedCounterValue,
        };

        // SAFETY: an open query.
        let status = unsafe { PdhCollectQueryData(self.query) };
        if status != 0 {
            let e = || Err(Error::new(format!("PdhCollectQueryData failed (0x{status:08X})")));
            return (e(), e());
        }
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: a counter of this query and a valid out pointer; the type is optional.
        let status = unsafe {
            PdhGetFormattedCounterValue(self.cpu, PDH_FMT_DOUBLE, std::ptr::null_mut(), &mut value)
        };
        let cpu = if status == 0 && value.CStatus <= 1 {
            // SAFETY: PDH_FMT_DOUBLE fills the double.
            Ok(unsafe { value.Anonymous.doubleValue }.clamp(0.0, 100.0))
        } else {
            Err(Error::new(format!("no CPU value (0x{status:08X}, status {})", value.CStatus)))
        };
        let gpu = match self.gpu {
            Some(counter) => gpu_engines(counter).and_then(|items| {
                crate::load::busiest_engine(items.iter().map(|(n, v)| (n.as_str(), *v)))
                    .ok_or_else(|| Error::new("no GPU engine reported"))
            }),
            None => Err(Error::new("no GPU counters on this system")),
        };
        (cpu, gpu)
    }
}

/// Every instance of a wildcard counter with a valid value. Instances come and go with the
/// processes, so the buffer can be too small again on the second call; then it is retried.
fn gpu_engines(
    counter: windows_sys::Win32::System::Performance::PDH_HCOUNTER,
) -> Result<Vec<(String, f64)>, Error> {
    use windows_sys::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_MORE_DATA, PdhGetFormattedCounterArrayW,
    };

    let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let mut buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::new();
    for _ in 0..4 {
        let mut bytes = u32::try_from(buffer.len() * item).unwrap_or(u32::MAX);
        let mut count = 0u32;
        let ptr = if buffer.is_empty() { std::ptr::null_mut() } else { buffer.as_mut_ptr() };
        // SAFETY: `ptr` is null with a size of 0, or a buffer of `bytes` bytes aligned for the
        // items; PDH writes the items first and the names behind them, within `bytes`.
        let status =
            unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, ptr) };
        if status == PDH_MORE_DATA {
            // Items and their names; one item more so the names fit after rounding up.
            buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (bytes as usize).div_ceil(item) + 1];
            continue;
        }
        if status != 0 {
            return Err(Error::new(format!("no GPU values (0x{status:08X})")));
        }
        let items = buffer.get(..count as usize).unwrap_or_default();
        let mut out = Vec::with_capacity(items.len());
        for i in items {
            if i.FmtValue.CStatus > 1 || i.szName.is_null() {
                continue;
            }
            // SAFETY: PDH points the name into the buffer, NUL-terminated.
            let name = unsafe {
                let mut len = 0usize;
                while *i.szName.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(i.szName, len))
            };
            // SAFETY: PDH_FMT_DOUBLE fills the double.
            out.push((name, unsafe { i.FmtValue.Anonymous.doubleValue }));
        }
        return Ok(out);
    }
    Err(Error::new("the GPU engines kept changing while being read"))
}

impl Drop for Pdh {
    fn drop(&mut self) {
        // SAFETY: the query from PdhOpenQueryW, closed once; it closes its counters too.
        unsafe { windows_sys::Win32::System::Performance::PdhCloseQuery(self.query) };
    }
}
