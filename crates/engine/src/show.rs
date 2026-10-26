//! Show mode: one switch that keeps the app from touching a project during a performance.
//!
//! While it is on:
//!
//! - the project is locked ([`penumbra_project::Session::set_locked`]): every edit, undo and
//!   redo is refused, from the editor and the control channel alike, and `project.ron` is
//!   neither written nor reloaded when it changes on disk;
//! - commands that would write into the project folder or change the project are refused
//!   ([`refuses`]): saving, switching projects, palettes, components, scans, OSC learn;
//! - the show itself runs on: OSC and DMX values, the control channel's other commands, and the
//!   inputs the project reads (keys, mouse, audio, cameras);
//! - after a crash that ends the app with exit code 3 (a lost GPU device, a failed editor
//!   window) the app starts again by itself with the clock running, at most
//!   [`MAX_RESTARTS`] times within [`RESTART_WINDOW`]; a crash beyond that ends as without
//!   show mode (a message, the next start paused).
//!
//! The switch is kept in [`FILE`] in penumbra's data directory, so it survives a restart and a
//! crash. Nothing is written into the project folder for it. The node state the project keeps
//! (its `state` folder) is still saved, so a restart picks feedback and caches up again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use penumbra_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

/// The show mode file's name in [`crate::projects::data_dir`].
pub const FILE: &str = "show-mode.json";

/// How many automatic restarts show mode allows within [`RESTART_WINDOW`].
pub const MAX_RESTARTS: usize = 3;

/// The window [`MAX_RESTARTS`] counts in. A show that crashes more often than that has a cause
/// a restart does not fix; starting it again and again would only flicker the projectors.
pub const RESTART_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Why a project switch is refused in show mode.
pub const SWITCH_REFUSED: &str = "show mode is on: the open project stays open; show.unlock ends show mode";

/// What the show mode file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kept {
    /// Show mode is on.
    pub on: bool,
    /// The project open when it was last switched.
    pub project: String,
    /// When it was last switched on or off, milliseconds since 1970.
    pub since: u64,
    /// When show mode last restarted the app, milliseconds since 1970 (only those within
    /// [`RESTART_WINDOW`] are kept).
    pub restarts: Vec<u64>,
}

/// `show-mode.json` in penumbra's data directory (`%LOCALAPPDATA%\penumbra` on Windows).
pub fn default_file() -> Result<PathBuf, Error> {
    Ok(crate::projects::data_dir()?.join(FILE))
}

/// Milliseconds since 1970.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

/// The file's content; off when it is missing or does not parse (a broken file must not lock
/// anybody out of editing).
pub fn read(file: &Path) -> Kept {
    let Ok(text) = std::fs::read_to_string(file) else { return Kept::default() };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log::warn!("{} does not parse ({e}); show mode is off", file.display());
        Kept::default()
    })
}

pub fn write(file: &Path, kept: &Kept) -> Result<(), Error> {
    let text = serde_json::to_string_pretty(kept).map_err(|e| Error::new(e.to_string()))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| Error::new(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, file).map_err(|e| Error::new(format!("{}: {e}", file.display())))
}

/// Restarts within [`RESTART_WINDOW`] before `now`.
fn recent(kept: &Kept, now: u64) -> Vec<u64> {
    let window = u64::try_from(RESTART_WINDOW.as_millis()).unwrap_or(u64::MAX);
    kept.restarts.iter().copied().filter(|&t| t <= now && now - t < window).collect()
}

/// Whether show mode may restart the app after a crash at `now` (ms since 1970): it is on and
/// fewer than [`MAX_RESTARTS`] restarts fall within [`RESTART_WINDOW`]. If so, the restart is
/// counted in the file before it happens, so a crash that comes back at once runs out.
pub fn take_restart(file: &Path, now: u64) -> bool {
    let mut kept = read(file);
    if !kept.on {
        return false;
    }
    let mut restarts = recent(&kept, now);
    if restarts.len() >= MAX_RESTARTS {
        log::error!(
            "show mode restarted the app {} times within {} minutes; not again",
            restarts.len(),
            RESTART_WINDOW.as_secs() / 60
        );
        return false;
    }
    restarts.push(now);
    kept.restarts = restarts;
    match write(file, &kept) {
        Ok(()) => true,
        Err(e) => {
            // Without the count there is no limit; better no restart than an endless loop.
            log::error!("cannot count the restart, so none: {e}");
            false
        }
    }
}

/// The commands show mode refuses: those that write into the project folder, switch the
/// project or start editing. Edits themselves (`node.*`, `history.*`, ...) are refused by the
/// locked project, so they need no entry here.
pub fn refuses(method: &str) -> bool {
    const REFUSED: &[&str] = &[
        "project.save",
        "project.save_as",
        "project.open",
        "project.new",
        "project.demo",
        "project.example",
        "edit.begin",
        "palette.set",
        "palette.remove",
        "comp.save",
        "comp.remove",
        "comp.load",
        "comp.collapse",
        "osc.learn",
        "projector.match",
        "projector.flatfield",
    ];
    if REFUSED.contains(&method) {
        return true;
    }
    // Scans write pictures into the project folder and take over the projectors.
    method.starts_with("scan.") && !matches!(method, "scan.status" | "scan.cancel" | "scan.folders")
}

/// The error for a command show mode refuses.
pub fn refusal(method: &str) -> String {
    format!("show mode is on: {method} would change the project; show.unlock ends show mode")
}

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let guarded = Arc::downgrade(engine);
    registry.set_guard(move |method| {
        let engine = guarded.upgrade()?;
        (engine.show_on() && refuses(method)).then(|| refusal(method))
    });
    let mut add = engine_commands(registry, engine);
    add(
        "show.lock",
        "Show mode on: the project is locked against edits and moves, not written and not reloaded from disk; \
         OSC, DMX and the control channel stay live; after a crash (exit code 3) the app starts again with the clock running. \
         Kept across restarts",
        show_lock,
    );
    add("show.unlock", "Show mode off: editing, saving and reloading as usual", show_unlock);
    add(
        "show.status",
        "Whether show mode is on, since when, the project it locks, restarts after crashes in the last 10 minutes",
        show_status,
    );
}

fn show_lock(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    e.set_show(true)?;
    show_status(e, p)
}

fn show_unlock(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    e.set_show(false)?;
    show_status(e, p)
}

fn show_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    let file = e.show_file();
    let kept = file.as_deref().map(read).unwrap_or_default();
    let now = now_ms();
    let restarts = recent(&kept, now).len();
    let since = (kept.since > 0).then(|| (now.saturating_sub(kept.since)) as f64 / 1000.0);
    Ok(json!({
        "on": e.show_on(),
        "project": e.session().dir().display().to_string(),
        "seconds_since_switched": if file.is_some() { json!(since) } else { Json::Null },
        "restarts_recent": restarts,
        "restarts_left": MAX_RESTARTS.saturating_sub(restarts),
        "kept_in": file.map(|f| f.display().to_string()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;

    #[test]
    fn restarts_run_out_within_the_window() {
        let dir = ScratchDir::unborn("show-restarts");
        let file = dir.join(FILE);
        assert!(!take_restart(&file, 1_000_000), "off: no restart");
        write(&file, &Kept { on: true, ..Kept::default() }).unwrap();
        let t0 = 10_000_000;
        for i in 0..MAX_RESTARTS as u64 {
            assert!(take_restart(&file, t0 + i * 1000), "restart {i}");
        }
        assert!(!take_restart(&file, t0 + 5000), "the fourth within the window");
        let window = RESTART_WINDOW.as_millis() as u64;
        assert!(take_restart(&file, t0 + window + 500), "the first has left the window");
        assert_eq!(read(&file).restarts.len(), MAX_RESTARTS);
    }

    #[test]
    fn a_broken_file_means_off() {
        let dir = ScratchDir::unborn("show-broken");
        let file = dir.join(FILE);
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(&file, "{ on: yes").unwrap();
        assert_eq!(read(&file), Kept::default());
        assert!(!take_restart(&file, 1));
    }

    #[test]
    fn what_is_refused() {
        for m in ["project.save", "project.open", "palette.set", "scan.capture", "scan.run", "osc.learn"] {
            assert!(refuses(m), "{m}");
        }
        for m in
            ["scan.status", "scan.cancel", "cook.play", "node.info", "osc.send", "dmx.status", "show.unlock"]
        {
            assert!(!refuses(m), "{m}");
        }
    }
}
