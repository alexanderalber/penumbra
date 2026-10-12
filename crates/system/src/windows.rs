//! The Windows calls behind [`crate::display`] and [`crate::checks`]: display configuration
//! (`QueryDisplayConfig`) and the current user's registry. The crate's only `unsafe`; every
//! call checks its status code and every buffer is sized by the API or by its type.
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
