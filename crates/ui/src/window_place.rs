//! Where the editor window sits, kept across restarts: a small JSON file in the app's data
//! directory, written a second after the window last moved, changed size or was maximised. The
//! next start puts the window back there, if enough of its title bar still lies on a connected
//! display to grab it (a display unplugged since must not leave the editor out of reach); else
//! it stays where the OS put it. A setup that is filmed or shown keeps the editor beside, not
//! over, the windows it shares the screen with, however often the app restarts.
//!
//! Positions and sizes are physical pixels of the desktop, as `output.list` gives the displays.
//! The window is moved after it opened, in two steps (position, then size at the scale of the
//! display it landed on), so displays with different scaling come out right.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The editor window's place, physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The outer top left corner, title bar included.
    pub x: i32,
    pub y: i32,
    /// The inner size, the part the app draws.
    pub width: u32,
    pub height: u32,
    /// Maximised; position and size are then the window's before it was.
    pub maximized: bool,
}

/// The smallest and largest inner size taken from a file.
const SIZE: std::ops::RangeInclusive<u32> = 200..=16384;
/// How much of the title bar must lie on one display: a strip along the window's top this high,
/// at least `GRAB` of it.
const TITLE: i32 = 32;
const GRAB: [i32; 2] = [120, 24];
/// How long the place must stand still before it is written.
const SETTLE: Duration = Duration::from_secs(1);

impl Place {
    fn valid(&self) -> bool {
        SIZE.contains(&self.width)
            && SIZE.contains(&self.height)
            && self.x.abs() < 100_000
            && self.y.abs() < 100_000
    }

    /// Whether enough of the title bar lies on one of `displays` to grab the window there.
    pub fn reachable(&self, displays: &[penumbra_system::pointer::Monitor]) -> bool {
        let right = self.x.saturating_add(i32::try_from(self.width).unwrap_or(i32::MAX));
        displays.iter().any(|d| {
            let w = right.min(d.right) - self.x.max(d.left);
            let h = (self.y + TITLE).min(d.bottom) - self.y.max(d.top);
            w >= GRAB[0] && h >= GRAB[1]
        })
    }
}

/// `window.json` in the app's data directory.
pub fn default_file() -> Result<PathBuf, String> {
    Ok(penumbra_engine::projects::data_dir().map_err(|e| e.0)?.join("window.json"))
}

/// The place in `file`; `None` when there is none or it does not hold a sensible one.
pub fn load(file: &Path) -> Option<Place> {
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str::<Place>(&text) {
        Ok(place) if place.valid() => Some(place),
        Ok(place) => {
            log::warn!("{}: ignoring an implausible window place {place:?}", file.display());
            None
        }
        Err(e) => {
            log::warn!("{}: {e}", file.display());
            None
        }
    }
}

/// Writes `place` to `file` (through a temporary file, so a crash never leaves half of it).
pub fn save(file: &Path, place: &Place) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(place).map_err(|e| e.to_string())?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, file).map_err(fail)
}

/// Puts the window back at the start, then writes its place whenever it settles.
pub struct Keeper {
    file: PathBuf,
    /// Still to do at the start: the place to move to, and whether the position was sent.
    restore: Option<(Place, bool)>,
    saved: Option<Place>,
    /// Position and size while not maximised, kept for a maximised window's place.
    normal: Option<Place>,
    pending: Option<(Place, Instant)>,
    failed: bool,
}

impl Keeper {
    /// Reads `file`; the place there is restored if it is `reachable` on `displays`.
    pub fn new(file: PathBuf, displays: &[penumbra_system::pointer::Monitor]) -> Self {
        let saved = load(&file);
        let restore = saved.filter(|p| {
            let ok = p.reachable(displays);
            if !ok {
                log::info!(
                    "the editor window's last place {p:?} is off every display; leaving it where it opened"
                );
            }
            ok
        });
        Self { file, restore: restore.map(|p| (p, false)), saved, normal: None, pending: None, failed: false }
    }

    /// Whether it will move the window to a place kept from last time.
    pub fn restores(&self) -> bool {
        self.restore.is_some()
    }

    /// Once per frame. `seen` is the window's place now (`None` while minimised or not known
    /// yet); `ppp` the pixels per point where it is. Returns the viewport commands to send.
    pub fn frame(&mut self, now: Instant, seen: Option<Place>, ppp: f32) -> Vec<egui::ViewportCommand> {
        let Some(seen) = seen else { return Vec::new() };
        if let Some((target, moved)) = self.restore {
            if !moved {
                self.restore = Some((target, true));
                return vec![egui::ViewportCommand::OuterPosition(points([target.x, target.y], ppp))];
            }
            // The size at the scale of the display the window landed on, then maximised.
            self.restore = None;
            self.normal = Some(Place { maximized: false, ..target });
            let size = points([target.width as i32, target.height as i32], ppp).to_vec2();
            let mut cmds = vec![egui::ViewportCommand::InnerSize(size)];
            if target.maximized {
                cmds.push(egui::ViewportCommand::Maximized(true));
            }
            return cmds;
        }
        self.observe(now, seen);
        Vec::new()
    }

    fn observe(&mut self, now: Instant, seen: Place) {
        let place = if seen.maximized {
            match self.normal.or(self.saved) {
                Some(n) => Place { maximized: true, ..n },
                None => seen,
            }
        } else {
            self.normal = Some(seen);
            seen
        };
        if Some(place) == self.saved {
            self.pending = None;
            return;
        }
        match self.pending {
            Some((p, since)) if p == place => {
                if now.saturating_duration_since(since) >= SETTLE {
                    self.pending = None;
                    match save(&self.file, &place) {
                        Ok(()) => {
                            self.saved = Some(place);
                            self.failed = false;
                        }
                        Err(e) => {
                            if !self.failed {
                                log::warn!("cannot keep the editor window's place: {e}");
                            }
                            self.failed = true;
                            // Not again every frame: as if written, until it moves again.
                            self.saved = Some(place);
                        }
                    }
                }
            }
            _ => self.pending = Some((place, now)),
        }
    }
}

/// Physical pixels to points.
fn points(px: [i32; 2], ppp: f32) -> egui::Pos2 {
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    egui::pos2(px[0] as f32 / ppp, px[1] as f32 / ppp)
}

/// The window's place as the viewport reports it; `None` while minimised or not known.
pub fn seen(v: &egui::ViewportInfo) -> Option<(Place, f32)> {
    if v.minimized == Some(true) {
        return None;
    }
    let (outer, inner, ppp) = (v.outer_rect?, v.inner_rect?, v.native_pixels_per_point?);
    let px = |x: f32| (x * ppp).round();
    let size = |x: f32| u32::try_from(px(x) as i64).ok();
    let place = Place {
        x: px(outer.min.x) as i32,
        y: px(outer.min.y) as i32,
        width: size(inner.width())?,
        height: size(inner.height())?,
        maximized: v.maximized == Some(true),
    };
    Some((place, ppp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;
    use penumbra_system::pointer::Monitor;

    fn display(left: i32, top: i32, right: i32, bottom: i32) -> Monitor {
        Monitor { os_name: String::new(), left, top, right, bottom }
    }

    fn place(x: i32, y: i32) -> Place {
        Place { x, y, width: 1600, height: 1000, maximized: false }
    }

    #[test]
    fn a_place_is_reachable_only_with_its_title_bar_on_a_display() {
        let desk = [display(0, 0, 3840, 2160), display(3840, 1087, 5760, 2167)];
        assert!(place(100, 100).reachable(&desk));
        // On the second display, and hanging over the first's left edge with most of it.
        assert!(place(4000, 1200).reachable(&desk));
        assert!(place(-1400, 10).reachable(&desk));
        // Title bar above the desktop, or the display it was on unplugged.
        assert!(!place(100, -40).reachable(&desk));
        assert!(!place(6000, 100).reachable(&desk));
        assert!(!place(100, 100).reachable(&[]));
        // Only a sliver of the title bar left on the display.
        assert!(!place(3780, 100).reachable(&desk[..1]));
    }

    #[test]
    fn files_round_trip_and_nonsense_is_ignored() {
        let dir = ScratchDir::new("window-place").unwrap();
        let file = dir.join("window.json");
        assert_eq!(load(&file), None);
        let p = Place { x: 1920, y: 0, width: 1900, height: 2100, maximized: true };
        save(&file, &p).unwrap();
        assert_eq!(load(&file), Some(p));
        std::fs::write(&file, r#"{"x": 0, "y": 0, "width": 5, "height": 900, "maximized": false}"#).unwrap();
        assert_eq!(load(&file), None);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(load(&file), None);
    }

    #[test]
    fn the_keeper_restores_then_writes_once_the_window_settles() {
        let dir = ScratchDir::new("window-keeper").unwrap();
        let file = dir.join("window.json");
        let desk = [display(0, 0, 3840, 2160)];
        let target = Place { x: 1920, y: 0, width: 1800, height: 2000, maximized: false };
        save(&file, &target).unwrap();
        let mut k = Keeper::new(file.clone(), &desk);
        let t0 = Instant::now();
        let opened = place(400, 300);
        // First the position (at 1.5 pixels per point), then the size.
        let c = k.frame(t0, Some(opened), 1.5);
        assert_eq!(c, vec![egui::ViewportCommand::OuterPosition(egui::pos2(1280.0, 0.0))]);
        let c = k.frame(t0, Some(Place { x: 1920, y: 0, ..opened }), 1.5);
        assert_eq!(c, vec![egui::ViewportCommand::InnerSize(egui::vec2(1200.0, 2000.0 / 1.5))]);
        // The window where it was: nothing to write.
        assert!(k.frame(t0, Some(target), 1.5).is_empty());
        // Moved: written a second after it stopped moving, not before.
        let moved = Place { x: 0, ..target };
        k.frame(t0, Some(moved), 1.5);
        k.frame(t0 + Duration::from_millis(500), Some(moved), 1.5);
        assert_eq!(load(&file), Some(target));
        k.frame(t0 + Duration::from_millis(1100), Some(moved), 1.5);
        assert_eq!(load(&file), Some(moved));
        // Maximised: the place before it, marked maximised.
        let full = Place { x: -8, y: -8, width: 3840, height: 2100, maximized: true };
        k.frame(t0 + Duration::from_secs(2), Some(full), 1.5);
        k.frame(t0 + Duration::from_secs(4), Some(full), 1.5);
        assert_eq!(load(&file), Some(Place { maximized: true, ..moved }));
        // Minimised: nothing changes.
        assert!(k.frame(t0 + Duration::from_secs(9), None, 1.5).is_empty());
        assert_eq!(load(&file), Some(Place { maximized: true, ..moved }));
    }

    #[test]
    fn a_place_off_every_display_is_not_restored() {
        let dir = ScratchDir::new("window-off").unwrap();
        let file = dir.join("window.json");
        save(&file, &place(6000, 100)).unwrap();
        let mut k = Keeper::new(file, &[display(0, 0, 3840, 2160)]);
        assert!(k.frame(Instant::now(), Some(place(400, 300)), 1.0).is_empty());
    }
}
