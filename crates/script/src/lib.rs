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

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use penumbra_core::lock;

pub use host::{Bench, Host, ScriptError, Status};
pub use protocol::{Call, CallOutput, InPicture, Load, Loaded, OutMesh, OutPicture, PictureArg, Samples};

static HOST: OnceLock<Host> = OnceLock::new();

/// The Python chosen in the app ([`locate::Env::chosen`]).
static CHOSEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The search with the process environment and the Python chosen in the app.
fn find() -> Result<locate::Install, String> {
    let chosen = lock(&CHOSEN).clone();
    locate::find(&locate::Env { chosen, ..locate::Env::from_process() })
}

/// Names the Python chosen in an earlier run, before the first [`host()`]. It is not checked
/// here: a choice that no longer holds a Python shows as the script nodes' error, which names
/// it, instead of giving way to another Python unnoticed.
pub fn set_chosen(path: Option<PathBuf>) {
    *lock(&CHOSEN) = path;
}

/// The Python chosen in the app, if any.
pub fn chosen() -> Option<PathBuf> {
    lock(&CHOSEN).clone()
}

/// What [`choose`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// The choice is in effect in this run. Otherwise only from the next start: Python has
    /// started (or failed to start) already, and a process holds one interpreter for good.
    pub now: bool,
    /// `PENUMBRA_PYTHON` is set, so its Python is used whatever is chosen.
    pub overridden: bool,
}

/// Chooses the Python for script nodes: a Python directory or a file in one. `None` goes back
/// to the search. A path without a usable Python is refused and changes nothing.
pub fn choose(path: Option<PathBuf>) -> Result<Chosen, String> {
    let env = locate::Env::from_process();
    let overridden = env.explicit.is_some();
    if path.is_some() {
        locate::find(&locate::Env { chosen: path.clone(), explicit: None, ..env })?;
    }
    *lock(&CHOSEN) = path;
    let now = HOST.get().is_none_or(|host| host.set_install(find()));
    Ok(Chosen { now, overridden })
}

/// The process's script host. The first use looks for Python (cheap); Python itself starts
/// with the first script.
pub fn host() -> &'static Host {
    HOST.get_or_init(|| {
        let found = find();
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
