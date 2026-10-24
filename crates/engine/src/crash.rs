//! What a crash leaves for the next start. When the GPU device is lost (a driver reset, the
//! GPU running out of memory) or the editor fails, the app ends; the project file still holds
//! whatever caused it, and the next start would run straight into it again. So the app writes
//! a small marker file at that moment ([`write`]), and the next start takes it ([`take`]),
//! opens the project with the show clock stopped and says why ([`notice`]).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use penumbra_core::Error;
use serde::{Deserialize, Serialize};

/// The marker's name in [`crate::projects::data_dir`].
pub const FILE: &str = "gpu-lost.json";

/// What the marker says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The project open at the time.
    pub project: String,
    /// How the run ended, e.g. `GPU device lost: ...`.
    pub reason: String,
    /// Seconds since 1970.
    pub at: u64,
}

/// `gpu-lost.json` in penumbra's data directory (`%LOCALAPPDATA%\penumbra` on Windows).
pub fn default_file() -> Result<PathBuf, Error> {
    Ok(crate::projects::data_dir()?.join(FILE))
}

/// Writes the marker. Small and synchronous: it runs while the app is going down.
pub fn write(file: &Path, project: &Path, reason: &str) -> Result<(), Error> {
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let marker = Marker { project: project.display().to_string(), reason: reason.to_string(), at };
    let text = serde_json::to_string_pretty(&marker).map_err(|e| Error::new(e.to_string()))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    std::fs::write(file, text).map_err(|e| Error::new(format!("{}: {e}", file.display())))
}

/// Reads and deletes the marker; `None` when there is none. A marker that does not parse still
/// counts (the run did end that way), with an unknown reason.
pub fn take(file: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(file).ok()?;
    if let Err(e) = std::fs::remove_file(file) {
        log::warn!("cannot delete {}: {e}", file.display());
    }
    Some(serde_json::from_str(&text).unwrap_or_else(|_| Marker {
        project: String::new(),
        reason: "unknown".into(),
        at: 0,
    }))
}

/// The status bar message for a start after `marker`.
pub fn notice(marker: &Marker) -> String {
    let reason = marker.reason.lines().next().unwrap_or("").trim();
    let reason: String = reason.chars().take(160).collect();
    format!(
        "Last run ended abnormally ({reason}); opened with the clock stopped. Fix the cause (a \
         huge texture or time machine?), then press play."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;

    #[test]
    fn a_marker_is_taken_once() {
        let dir = ScratchDir::unborn("crash");
        let file = dir.join(FILE);
        assert_eq!(take(&file), None);
        write(&file, Path::new("C:/shows/x"), "Device lost\nmore").unwrap();
        let m = take(&file).unwrap();
        assert_eq!((m.project.as_str(), m.reason.as_str()), ("C:/shows/x", "Device lost\nmore"));
        assert!(notice(&m).contains("(Device lost)"), "{}", notice(&m));
        assert_eq!(take(&file), None, "deleted when taken");
        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(take(&file).map(|m| m.reason), Some("unknown".into()));
    }
}
