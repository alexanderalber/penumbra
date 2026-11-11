//! This process's memory, handles and threads (`system.process`, the soak test), and a memory cap
//! for measurement runs, whether the process owns its console window, and a raised priority for
//! threads that must wake on time.

use penumbra_core::Error;
use serde::Serialize;

/// What the operating system says about this process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProcessStats {
    /// Resident memory (Windows: working set; Linux: VmRSS; macOS: resident size).
    pub working_set: u64,
    pub peak_working_set: u64,
    /// Memory only this process uses (Windows: private commit; Linux: RssAnon), the number a
    /// leak grows. On macOS, the process's physical footprint, including compressed memory.
    pub private_bytes: u64,
    /// Open handles (Windows) or file descriptors (Linux, macOS).
    pub handles: u64,
    pub threads: u64,
}

/// The numbers now.
pub fn stats() -> Result<ProcessStats, Error> {
    #[cfg(windows)]
    {
        crate::windows::process_stats()
    }
    #[cfg(target_os = "macos")]
    {
        macos_stats()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        linux_stats()
    }
}

/// Limits this process's committed memory to `bytes`, so a runaway allocation fails instead of
/// exhausting the machine. Windows only (a job object); elsewhere an error.
pub fn cap_memory(bytes: u64) -> Result<(), Error> {
    #[cfg(windows)]
    {
        crate::windows::cap_memory(bytes)
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err(Error::new("a memory cap is only implemented on Windows (use ulimit -v elsewhere)"))
    }
}

/// Raises the calling thread's scheduling priority above the normal threads' (Windows:
/// `THREAD_PRIORITY_HIGHEST`), for a thread that mostly sleeps and must wake on time even while
/// every core is busy. Returns whether it worked; false outside Windows, where raising a
/// thread's priority needs privileges a show machine should not grant.
pub fn raise_thread_priority() -> bool {
    #[cfg(windows)]
    {
        crate::windows::raise_thread_priority()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Lowers the calling thread's scheduling priority below the normal threads' (Windows:
/// `THREAD_PRIORITY_LOWEST`), for long background work that must not take time from sound,
/// pictures or input while every core is busy. Returns whether it worked; false outside
/// Windows.
pub fn lower_thread_priority() -> bool {
    #[cfg(windows)]
    {
        crate::windows::lower_thread_priority()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Waits until the process `pid` has ended, at most `timeout`. Returns whether it ended (or was
/// gone already). A restarted app waits so for the one it replaces: that one still holds the
/// control channel's port and the output windows until it exits. Outside Windows it only waits
/// out `timeout` (no such restart there yet) and says false.
pub fn wait_for_exit(pid: u32, timeout: std::time::Duration) -> bool {
    #[cfg(windows)]
    {
        crate::windows::wait_for_exit(pid, timeout)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        std::thread::sleep(timeout);
        false
    }
}

/// Whether this process has a console window to itself, which is what a double-click on a
/// console program gives it: the window closes the moment the process exits. False when started
/// from a terminal, with no console at all, and outside Windows.
pub fn own_console() -> bool {
    #[cfg(windows)]
    {
        crate::windows::own_console()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn linux_stats() -> Result<ProcessStats, Error> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| Error::new(format!("/proc/self/status: {e}")))?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let handles = std::fs::read_dir("/proc/self/fd").map(|d| d.count() as u64).unwrap_or(0);
    Ok(ProcessStats {
        working_set: field("VmRSS:") * 1024,
        peak_working_set: field("VmHWM:") * 1024,
        private_bytes: field("RssAnon:") * 1024,
        handles,
        threads: field("Threads:"),
    })
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn macos_stats() -> Result<ProcessStats, Error> {
    let pid = std::process::id() as i32;
    // SAFETY: these C output records contain only integers, for which zero is valid.
    let (mut task, mut usage, mut peak) = unsafe {
        (
            std::mem::zeroed::<libc::proc_taskinfo>(),
            std::mem::zeroed::<libc::rusage_info_v2>(),
            std::mem::zeroed::<libc::rusage>(),
        )
    };
    let size = std::mem::size_of_val(&task) as i32;
    // SAFETY: the buffer has exactly the size and layout required by PROC_PIDTASKINFO.
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut task).cast(), size) } != size {
        return Err(Error::new(format!("proc_pidinfo: {}", std::io::Error::last_os_error())));
    }
    // SAFETY: RUSAGE_INFO_V2 writes one rusage_info_v2 record into this buffer.
    if unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, (&raw mut usage).cast()) } != 0 {
        return Err(Error::new(format!("proc_pid_rusage: {}", std::io::Error::last_os_error())));
    }
    // SAFETY: a valid output record for this process; macOS reports ru_maxrss in bytes.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut peak) } != 0 {
        return Err(Error::new(format!("getrusage: {}", std::io::Error::last_os_error())));
    }
    // SAFETY: a null buffer asks for the descriptor buffer's size without writing anything.
    let size = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) }.max(0);
    let mut fds = vec![0u8; size as usize];
    let bytes = if fds.is_empty() {
        0
    } else {
        // SAFETY: the byte buffer holds `size` writable bytes; only the returned count is used.
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), size) }.max(0)
    };
    Ok(ProcessStats {
        working_set: task.pti_resident_size,
        peak_working_set: peak.ru_maxrss.max(0) as u64,
        private_bytes: usage.ri_phys_footprint,
        handles: bytes as u64 / std::mem::size_of::<libc::proc_fdinfo>() as u64,
        threads: task.pti_threadnum.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_has_memory_and_threads() {
        let s = super::stats().unwrap();
        assert!(s.working_set > 1 << 20, "{s:?}");
        assert!(s.private_bytes > 0 && s.handles > 0 && s.threads >= 1, "{s:?}");
    }

    #[cfg(windows)]
    #[test]
    fn waiting_for_a_process_that_ended() {
        let mut child = std::process::Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(super::wait_for_exit(pid, std::time::Duration::from_secs(5)));
        // This process does not end within the wait.
        assert!(!super::wait_for_exit(std::process::id(), std::time::Duration::from_millis(50)));
    }
}
