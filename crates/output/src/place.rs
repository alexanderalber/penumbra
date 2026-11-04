//! Where a projector's desktop window sits (`out.projector` `window_place`), kept in the
//! projector node so the window opens there again.
//!
//! The param and the window are kept in line in both directions. A new value of the param (typed
//! by the user, set over the control channel, an undo) moves the window there. A window the user
//! moved or resized writes its place to the param once it stood still for [`SETTLE`]. Only a
//! change on either side does anything: a window that opened a few pixels off its place (a
//! display's scaling rounds) does not rewrite the project.
//!
//! A place is taken only when enough of the window's top lies on a connected display to grab it
//! ([`penumbra_system::pointer::grabbable`]). Otherwise the window opens where the system puts a
//! new window, and the param keeps the place: plugging the display back in and opening the
//! project again brings the window back there.

use std::time::{Duration, Instant};

use penumbra_system::pointer::Monitor;
use winit::window::ResizeDirection;

/// A window's place, desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// The outer top left corner, title bar included.
    pub x: i32,
    pub y: i32,
    /// The inner size, the picture's.
    pub width: u32,
    pub height: u32,
}

/// The smallest and largest inner size taken from the param.
const SIZE: std::ops::RangeInclusive<u32> = 64..=16384;
/// How far from the desktop's origin a place may lie.
const FAR: f64 = 100_000.0;
/// How long a moved window must stand still before its place is written.
pub const SETTLE: Duration = Duration::from_secs(1);

impl Place {
    /// The place in a `window_place` value. `Ok(None)` for none (width or height 0); `Err` for
    /// a value that is no place (a size out of [`SIZE`], a corner too far out, not a number).
    pub fn from_param(v: [f64; 4]) -> Result<Option<Place>, String> {
        let [x, y, w, h] = v;
        if w == 0.0 || h == 0.0 {
            return Ok(None);
        }
        let size = |s: f64| {
            let r = s.round();
            (r.is_finite() && r >= f64::from(*SIZE.start()) && r <= f64::from(*SIZE.end()))
                .then_some(r as u32)
        };
        let corner = |c: f64| {
            let r = c.round();
            (r.is_finite() && r.abs() < FAR).then_some(r as i32)
        };
        match (corner(x), corner(y), size(w), size(h)) {
            (Some(x), Some(y), Some(width), Some(height)) => Ok(Some(Place { x, y, width, height })),
            _ => Err(format!(
                "`window_place` {} is no window place: width and height go from {} to {} pixels",
                format_args!("[{x}, {y}, {w}, {h}]"),
                SIZE.start(),
                SIZE.end()
            )),
        }
    }

    pub fn to_param(self) -> [f64; 4] {
        [f64::from(self.x), f64::from(self.y), f64::from(self.width), f64::from(self.height)]
    }

    /// Whether enough of the window's top lies on one of `displays` to grab it there.
    pub fn reachable(&self, displays: &[Monitor]) -> bool {
        penumbra_system::pointer::grabbable(self.x, self.y, self.width, displays)
    }
}

/// What the window is to do after a [`Keeper::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Move the window there (the param changed).
    Move(Place),
    /// Write this place to the param (the user moved the window).
    Write(Place),
}

/// Keeps one projector window and its `window_place` in line.
#[derive(Debug)]
pub struct Keeper {
    /// The param value last acted on: moved to, or written.
    applied: [f64; 4],
    /// The value before the last write, which the frames may still show for a moment.
    stale: Option<[f64; 4]>,
    /// `settled` before the last write, for trying it again.
    before_write: Option<Place>,
    /// The window's place after penumbra last moved it or wrote its place; `None` until seen.
    settled: Option<Place>,
    /// A place seen that differs from `settled`, and since when.
    pending: Option<(Place, Instant)>,
    /// Why the param's place is not taken, for the node; `None` while it is.
    problem: Option<String>,
}

impl Keeper {
    /// For a window about to open with `param`: where to open it (`None`: where the system
    /// puts new windows), and the keeper.
    pub fn open(param: [f64; 4], displays: &[Monitor]) -> (Option<Place>, Self) {
        let mut k = Keeper {
            applied: param,
            stale: None,
            before_write: None,
            settled: None,
            pending: None,
            problem: None,
        };
        let place = k.take(param, displays);
        (place, k)
    }

    /// The param's place if it can be taken; records why not.
    fn take(&mut self, param: [f64; 4], displays: &[Monitor]) -> Option<Place> {
        self.problem = None;
        match Place::from_param(param) {
            Ok(Some(p)) if p.reachable(displays) => Some(p),
            Ok(Some(p)) => {
                self.problem = Some(format!(
                    "`window_place` {}, {} lies off every connected display, so the window is not \
                     put there; moving the window by hand keeps a new place",
                    p.x, p.y
                ));
                None
            }
            Ok(None) => None,
            Err(e) => {
                self.problem = Some(e);
                None
            }
        }
    }

    /// Why the param's place is not taken, as a warning for the node.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Once per tick. `param` is the node's `window_place` now, `seen` the window's place
    /// (`None` while minimised or maximised).
    pub fn tick(
        &mut self,
        now: Instant,
        param: [f64; 4],
        seen: Option<Place>,
        displays: &[Monitor],
    ) -> Action {
        if param == self.applied {
            self.stale = None;
        } else if Some(param) != self.stale {
            // Set from elsewhere: the window follows, and where it lands is the new start.
            self.applied = param;
            self.stale = None;
            self.pending = None;
            let place = self.take(param, displays);
            if place.is_some() {
                self.settled = None;
            }
            return place.map_or(Action::Nothing, Action::Move);
        }
        let Some(seen) = seen else {
            self.pending = None;
            return Action::Nothing;
        };
        let Some(settled) = self.settled else {
            self.settled = Some(seen);
            return Action::Nothing;
        };
        if seen == settled {
            self.pending = None;
            return Action::Nothing;
        }
        match self.pending {
            Some((p, since)) if p == seen => {
                if now.saturating_duration_since(since) < SETTLE {
                    return Action::Nothing;
                }
                self.pending = None;
                self.before_write = self.settled.replace(seen);
                self.stale = Some(self.applied);
                self.applied = seen.to_param();
                self.problem = None;
                Action::Write(seen)
            }
            _ => {
                self.pending = Some((seen, now));
                Action::Nothing
            }
        }
    }

    /// The write of the last [`Action::Write`] failed: it is tried again once the window stood
    /// still for another [`SETTLE`].
    pub fn write_failed(&mut self) {
        if let Some(before) = self.stale.take() {
            self.applied = before;
        }
        self.settled = self.before_write.take();
        self.pending = None;
    }
}

/// How close to its edge a borderless window is resized rather than moved, in pixels.
pub const EDGE: f64 = 8.0;

/// Where a press at `(x, y)` in a borderless window of `size` grabs it: an edge or corner within
/// [`EDGE`] resizes, anywhere else moves the window (`None`).
pub fn grip(x: f64, y: f64, size: [u32; 2]) -> Option<ResizeDirection> {
    let (w, h) = (f64::from(size[0]), f64::from(size[1]));
    // A window too small for two edges is only moved.
    if w < 3.0 * EDGE || h < 3.0 * EDGE {
        return None;
    }
    let (west, east) = (x < EDGE, x >= w - EDGE);
    let (north, south) = (y < EDGE, y >= h - EDGE);
    Some(match (north, south, west, east) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (true, _, _, true) => ResizeDirection::NorthEast,
        (_, true, true, _) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::North,
        (_, true, ..) => ResizeDirection::South,
        (_, _, true, _) => ResizeDirection::West,
        (_, _, _, true) => ResizeDirection::East,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(left: i32, top: i32, right: i32, bottom: i32) -> Monitor {
        Monitor { os_name: String::new(), left, top, right, bottom }
    }

    fn desk() -> Vec<Monitor> {
        vec![display(0, 0, 3840, 2160), display(3840, 0, 5760, 1080)]
    }

    #[test]
    fn params_are_read_as_places() {
        assert_eq!(Place::from_param([0.0; 4]), Ok(None));
        assert_eq!(Place::from_param([10.0, 20.0, 0.0, 450.0]), Ok(None));
        assert_eq!(
            Place::from_param([-1919.6, 20.2, 800.0, 450.0]),
            Ok(Some(Place { x: -1920, y: 20, width: 800, height: 450 }))
        );
        assert!(Place::from_param([0.0, 0.0, 10.0, 450.0]).is_err());
        assert!(Place::from_param([0.0, 0.0, -800.0, 450.0]).is_err());
        assert!(Place::from_param([f64::NAN, 0.0, 800.0, 450.0]).is_err());
        assert!(Place::from_param([1e9, 0.0, 800.0, 450.0]).is_err());
        assert!(Place::from_param([0.0, 0.0, f64::INFINITY, 450.0]).is_err());
        let p = Place { x: -5, y: 7, width: 640, height: 360 };
        assert_eq!(Place::from_param(p.to_param()), Ok(Some(p)));
    }

    #[test]
    fn a_window_opens_at_its_place_only_on_a_connected_display() {
        let (at, k) = Keeper::open([100.0, 100.0, 800.0, 450.0], &desk());
        assert_eq!(at, Some(Place { x: 100, y: 100, width: 800, height: 450 }));
        assert!(k.problem().is_none());
        // On a display unplugged since: where the system puts it, with a warning.
        let (at, k) = Keeper::open([6000.0, 100.0, 800.0, 450.0], &desk());
        assert_eq!(at, None);
        assert!(k.problem().is_some_and(|p| p.contains("off every connected display")), "{k:?}");
        let (at, k) = Keeper::open([100.0, 100.0, 800.0, 450.0], &[]);
        assert_eq!(at, None);
        assert!(k.problem().is_some());
        // No place: no warning.
        let (at, k) = Keeper::open([0.0; 4], &desk());
        assert_eq!(at, None);
        assert!(k.problem().is_none());
        let (at, k) = Keeper::open([0.0, 0.0, 3.0, 3.0], &desk());
        assert_eq!(at, None);
        assert!(k.problem().is_some());
    }

    #[test]
    fn a_moved_window_writes_its_place_once_it_stands_still() {
        let d = desk();
        let t0 = Instant::now();
        let (at, mut k) = Keeper::open([0.0; 4], &d);
        assert_eq!(at, None);
        let opened = Place { x: 300, y: 200, width: 960, height: 540 };
        // Where it opened is no move of the user's: nothing written.
        assert_eq!(k.tick(t0, [0.0; 4], Some(opened), &d), Action::Nothing);
        assert_eq!(k.tick(t0 + SETTLE * 3, [0.0; 4], Some(opened), &d), Action::Nothing);
        // Dragged: written a second after it stopped, not before.
        let moved = Place { x: 1000, ..opened };
        let t1 = t0 + SETTLE * 4;
        assert_eq!(k.tick(t1, [0.0; 4], Some(Place { x: 600, ..opened }), &d), Action::Nothing);
        assert_eq!(k.tick(t1 + SETTLE / 4, [0.0; 4], Some(moved), &d), Action::Nothing);
        assert_eq!(k.tick(t1 + SETTLE, [0.0; 4], Some(moved), &d), Action::Nothing);
        assert_eq!(k.tick(t1 + SETTLE / 4 + SETTLE, [0.0; 4], Some(moved), &d), Action::Write(moved));
        // The frames still show the old value for a moment: the window is not moved back.
        let t2 = t1 + SETTLE * 3;
        assert_eq!(k.tick(t2, [0.0; 4], Some(moved), &d), Action::Nothing);
        assert_eq!(k.tick(t2, moved.to_param(), Some(moved), &d), Action::Nothing);
        // Minimised: nothing.
        assert_eq!(k.tick(t2 + SETTLE * 5, moved.to_param(), None, &d), Action::Nothing);
        assert_eq!(k.tick(t2 + SETTLE * 6, moved.to_param(), Some(moved), &d), Action::Nothing);
        // After the write is through, the old value is a new one again (an undo of a typed value).
        assert_eq!(k.tick(t2 + SETTLE * 7, [0.0; 4], Some(moved), &d), Action::Nothing);
    }

    #[test]
    fn a_new_param_value_moves_the_window() {
        let d = desk();
        let t0 = Instant::now();
        let start = Place { x: 100, y: 100, width: 800, height: 450 };
        let (_, mut k) = Keeper::open(start.to_param(), &d);
        let seen = Place { x: 101, ..start };
        assert_eq!(k.tick(t0, start.to_param(), Some(seen), &d), Action::Nothing);
        let typed = Place { x: 4000, y: 0, width: 1920, height: 1080 };
        assert_eq!(k.tick(t0, typed.to_param(), Some(seen), &d), Action::Move(typed));
        // Where it landed is the new start; a rounding difference writes nothing.
        let landed = Place { y: 1, ..typed };
        assert_eq!(k.tick(t0 + SETTLE, typed.to_param(), Some(landed), &d), Action::Nothing);
        assert_eq!(k.tick(t0 + SETTLE * 5, typed.to_param(), Some(landed), &d), Action::Nothing);
        // A place off every display is not taken: the window stays, the node is told.
        let off = [9000.0, 0.0, 800.0, 450.0];
        assert_eq!(k.tick(t0 + SETTLE * 6, off, Some(landed), &d), Action::Nothing);
        assert!(k.problem().is_some());
        // Moving the window by hand then writes a good place and clears the warning.
        let by_hand = Place { x: 200, ..landed };
        k.tick(t0 + SETTLE * 7, off, Some(by_hand), &d);
        k.tick(t0 + SETTLE * 7, off, Some(by_hand), &d);
        assert_eq!(k.tick(t0 + SETTLE * 9, off, Some(by_hand), &d), Action::Write(by_hand));
        assert!(k.problem().is_none());
    }

    #[test]
    fn a_failed_write_is_tried_again() {
        let d = desk();
        let t0 = Instant::now();
        let (_, mut k) = Keeper::open([0.0; 4], &d);
        let opened = Place { x: 0, y: 0, width: 960, height: 540 };
        k.tick(t0, [0.0; 4], Some(opened), &d);
        let moved = Place { x: 50, ..opened };
        k.tick(t0, [0.0; 4], Some(moved), &d);
        assert_eq!(k.tick(t0 + SETTLE, [0.0; 4], Some(moved), &d), Action::Write(moved));
        k.write_failed();
        // The param still says 0: no move back, the place is written again a second later.
        let t1 = t0 + SETTLE * 2;
        assert_eq!(k.tick(t1, [0.0; 4], Some(moved), &d), Action::Nothing);
        assert_eq!(k.tick(t1 + SETTLE, [0.0; 4], Some(moved), &d), Action::Write(moved));
    }

    #[test]
    fn edges_resize_and_the_rest_moves() {
        let size = [800, 450];
        assert_eq!(grip(400.0, 200.0, size), None);
        assert_eq!(grip(2.0, 2.0, size), Some(ResizeDirection::NorthWest));
        assert_eq!(grip(799.0, 449.0, size), Some(ResizeDirection::SouthEast));
        assert_eq!(grip(795.0, 3.0, size), Some(ResizeDirection::NorthEast));
        assert_eq!(grip(3.0, 445.0, size), Some(ResizeDirection::SouthWest));
        assert_eq!(grip(400.0, 1.0, size), Some(ResizeDirection::North));
        assert_eq!(grip(400.0, 448.0, size), Some(ResizeDirection::South));
        assert_eq!(grip(1.0, 200.0, size), Some(ResizeDirection::West));
        assert_eq!(grip(793.0, 200.0, size), Some(ResizeDirection::East));
        assert_eq!(grip(1.0, 1.0, [16, 16]), None);
    }
}
