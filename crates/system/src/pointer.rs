//! Where the mouse pointer is on the whole desktop, and the displays' rectangles, for
//! `chan.mouse_in`. Polled, not hooked: a global mouse hook would sit in the path of every mouse
//! move of the machine, and a stalled hook makes the whole desktop lag. Polling sees position and
//! buttons from anywhere, inside penumbra or not; the wheel needs a hook and is left to the
//! windows that have it.
//!
//! Coordinates are desktop pixels as the calling process sees them (physical pixels in a
//! process that is aware of per-monitor DPI, as the windowed app is); the pointer and the
//! rectangles come from the same process, so shares of a display come out right either way.

use serde::Serialize;

/// The pointer now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Pointer {
    /// Desktop pixels.
    pub x: i32,
    pub y: i32,
    /// Buttons held, as bits: [`LEFT`], [`RIGHT`], [`MIDDLE`] (the logical buttons, so swapped
    /// buttons for left-handed use count as the user sees them).
    pub buttons: u8,
}

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const MIDDLE: u8 = 4;

/// One display's place on the desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Monitor {
    /// The OS name (`\\.\DISPLAY2`), as in [`crate::DisplayDetails::os_name`].
    pub os_name: String,
    pub left: i32,
    pub top: i32,
    /// Exclusive.
    pub right: i32,
    pub bottom: i32,
}

impl Monitor {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// `(x, y)` as shares of the display, origin top left: 0 at the left (top) edge, 1 at the
    /// right (bottom) one. Outside the display the shares go below 0 or above 1.
    pub fn share(&self, x: i32, y: i32) -> (f64, f64) {
        let along = |v: i32, lo: i32, hi: i32| {
            let span = f64::from(hi) - f64::from(lo);
            if span <= 0.0 { 0.0 } else { (f64::from(v) - f64::from(lo)) / span }
        };
        (along(x, self.left, self.right), along(y, self.top, self.bottom))
    }
}

/// The pointer, `None` where it cannot be read (no desktop, a secure desktop such as the lock
/// screen, or a system without an implementation).
pub fn pointer() -> Option<Pointer> {
    #[cfg(windows)]
    {
        crate::windows::pointer()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The displays of the desktop with their rectangles; empty where unknown.
pub fn monitors() -> Vec<Monitor> {
    #[cfg(windows)]
    {
        crate::windows::monitors()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_run_from_the_top_left() {
        let m = Monitor { os_name: "a".into(), left: 1920, top: -100, right: 3840, bottom: 980 };
        assert!(m.contains(1920, -100));
        assert!(!m.contains(3840, 0));
        assert_eq!(m.share(1920, -100), (0.0, 0.0));
        assert_eq!(m.share(2880, 440), (0.5, 0.5));
        assert!(m.share(0, 0).0 < 0.0);
        let empty = Monitor { os_name: "b".into(), left: 5, top: 5, right: 5, bottom: 5 };
        assert_eq!(empty.share(9, 9), (0.0, 0.0));
    }

    #[test]
    fn reading_the_pointer_never_fails_hard() {
        // On a build machine without a desktop this is None; either way it returns.
        let _ = pointer();
        for m in monitors() {
            assert!(m.right >= m.left && m.bottom >= m.top, "{m:?}");
        }
    }
}
