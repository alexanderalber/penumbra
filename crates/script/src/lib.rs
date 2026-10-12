//! The script node's Python host.
//!
//! - [`locate`] finds a Python 3.12 or newer on disk (file checks only).
//! - [`ffi`] (the crate's only `unsafe`) loads that Python's library by full path, confirms its
//!   version, then loads `pyhost` (crates/pyhost), the only code that links Python.
//! - [`host`] runs every script on one Python thread, with a watchdog that stops scripts running
//!   past their limit, and calls from the cook thread that never wait for other work.
//! - [`protocol`] is the JSON between here and the bootstrap inside Python.
//!
//! There is one interpreter per process: [`host()`]. No GPU, no UI.

mod ffi;
pub mod host;
pub mod locate;
pub mod protocol;

use std::sync::OnceLock;

pub use host::{Bench, Host, ScriptError, Status};
pub use protocol::{Call, CallOutput, Load, Loaded};

static HOST: OnceLock<Host> = OnceLock::new();

/// The process's script host. The first use looks for Python (cheap); Python itself starts
/// with the first script.
pub fn host() -> &'static Host {
    HOST.get_or_init(|| {
        let found = locate::find(&locate::Env::from_process());
        match &found {
            Ok(i) => log::info!(
                "script node: Python 3.{} found at {} ({})",
                i.minor,
                i.library.display(),
                i.source
            ),
            Err(e) => log::warn!("script node: {e}"),
        }
        Host::new(found)
    })
}
