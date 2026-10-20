//! This process's memory, handles and threads (`system.process`, the soak test), and a memory cap
//! for measurement runs, whether the process owns its console window, and a raised priority for
//! threads that must wake on time.

use penumbra_core::Error;
use serde::Serialize;

/// What the operating system says about this process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProcessStats {
    /// Resident memory (Windows: working set; Linux: VmRSS).
    pub working_set: u64,
    pub peak_working_set: u64,
    /// Memory only this process uses (Windows: private commit; Linux: RssAnon), the number a
    /// leak grows.
    pub private_bytes: u64,
    /// Open handles (Windows) or file descriptors (Linux).
    pub handles: u64,
    pub threads: u64,
}

/// The numbers now.
pub fn stats() -> Result<ProcessStats, Error> {
    #[cfg(windows)]
    {
        crate::windows::process_stats()
    }
    #[cfg(not(windows))]
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

#[cfg(not(windows))]
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

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_has_memory_and_threads() {
        let s = super::stats().unwrap();
        assert!(s.working_set > 1 << 20, "{s:?}");
        assert!(s.private_bytes > 0 && s.handles > 0 && s.threads >= 1, "{s:?}");
    }
}
