//! What a crash leaves for the next start. When the GPU device is lost (a driver reset, the
//! GPU running out of memory) or the editor fails, the app ends; the project file still holds
//! whatever caused it, and the next start would run straight into it again. So the app writes
//! a small marker file at that moment ([`write`]), and the next start takes it ([`take`]),
//! opens the project with the show clock stopped and says why ([`notice`]). In show mode the
//! app starts again by itself instead, with the clock running where it would be now
//! ([`resume_frame`], [`crate::show`]).

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
    /// Milliseconds since 1970 (older markers lack it).
    #[serde(default)]
    pub at_ms: Option<u64>,
    /// The show clock's frame at the time, if it was known.
    #[serde(default)]
    pub frame: Option<i64>,
}

/// `gpu-lost.json` in penumbra's data directory (`%LOCALAPPDATA%\penumbra` on Windows).
pub fn default_file() -> Result<PathBuf, Error> {
    Ok(crate::projects::data_dir()?.join(FILE))
}

/// Writes the marker. Small and synchronous: it runs while the app is going down.
pub fn write(file: &Path, project: &Path, reason: &str, frame: Option<i64>) -> Result<(), Error> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let at_ms = u64::try_from(now.as_millis()).ok();
    let marker = Marker {
        project: project.display().to_string(),
        reason: reason.to_string(),
        at: now.as_secs(),
        at_ms,
        frame,
    };
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
        at_ms: None,
        frame: None,
    }))
}

/// The frame the show clock would be at `now_ms` had it kept running since the crash: the
/// marker's frame plus the time in between at `fps`. `None` without a frame or a time; a clock
/// that would land before the crash (the system clock went back) stays at the crash's frame.
pub fn resume_frame(marker: &Marker, now_ms: u64, fps: f64) -> Option<i64> {
    let frame = marker.frame?;
    let at = marker.at_ms?;
    let gap = now_ms.saturating_sub(at) as f64 / 1000.0;
    // At most a day of frames, so a wild clock cannot overflow.
    let added = (gap.min(86_400.0) * fps).round() as i64;
    Some(frame.saturating_add(added.max(0)))
}

/// The status bar message for a restart in show mode after `marker`.
pub fn show_notice(marker: &Marker) -> String {
    let reason = marker.reason.lines().next().unwrap_or("").trim();
    let reason: String = reason.chars().take(160).collect();
    format!("Restarted by show mode after a crash ({reason}); the clock runs on.")
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
        write(&file, Path::new("C:/shows/x"), "Device lost\nmore", Some(600)).unwrap();
        let m = take(&file).unwrap();
        assert_eq!((m.project.as_str(), m.reason.as_str()), ("C:/shows/x", "Device lost\nmore"));
        assert!(notice(&m).contains("(Device lost)"), "{}", notice(&m));
        assert_eq!(m.frame, Some(600));
        let at = m.at_ms.unwrap();
        assert_eq!(resume_frame(&m, at + 2500, 60.0), Some(750), "2.5 s later at 60 fps");
        assert_eq!(resume_frame(&m, at - 1000, 60.0), Some(600), "a clock that went back");
        assert_eq!(take(&file), None, "deleted when taken");
        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(take(&file).map(|m| m.reason), Some("unknown".into()));
    }
}
