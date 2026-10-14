//! A 3Dconnexion SpaceNavigator (USB 046d:c626) read as plain HID, without 3DxWare: the
//! driver would claim the device, and it is not needed for six axes and two buttons.
//!
//! - [`report`]: the HID reports (translation, rotation, buttons) into axis updates.
//! - [`Shaper`]: the raw counts into -1..1 per axis: the resting offset taken out (it drifts by
//!   a few counts after use, so it follows the cap while it rests inside the dead zone), a dead
//!   zone, a response curve for precision near the centre.
//! - [`SpaceNav`]: the reader thread and what it last saw. Unplugging and replugging is a
//!   status, never an error for the caller; the thread looks for the device again every second.
//!
//! Measured on the device (Windows, no 3DxWare): report 1 carries tx, ty, tz, report 2 rx, ry,
//! rz (three little-endian int16 each), report 3 the buttons (padded to six bytes on Windows);
//! about 125 reports per second while the cap is deflected, and none at all while it rests (so
//! axes that stop arriving count as zero, [`STALE`]); full scale about ±450 counts; an offset
//! of up to -20 seen at rest on one machine. The single 12-byte report of newer firmware is
//! accepted too, though it never appeared.
//!
//! Backends: hidapi on Windows; other systems report the device as unsupported for now.

// Without a backend nothing reads reports, so the reading half is unused there.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod report;

#[cfg(windows)]
#[path = "hid.rs"]
mod backend;
#[cfg(not(windows))]
#[path = "none.rs"]
mod backend;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

pub use report::{Update, parse};

pub const VENDOR_ID: u16 = 0x046d;
pub const PRODUCT_ID: u16 = 0xc626;

/// Raw counts at full deflection.
pub const FULL_SCALE: f32 = 450.0;
/// Fraction of full scale that counts as zero.
pub const DEAD_ZONE: f32 = 0.05;
/// Response exponent: small deflections move slowly, full ones fast.
pub const CURVE: f32 = 1.7;
/// Axis values older than this count as zero (a device that stopped reporting).
pub const STALE: Duration = Duration::from_millis(250);

/// The cap rests when every axis stays within this many counts...
const REST_SPAN: i32 = 4;
/// ...for this long, inside the dead zone: only an offset that already reads as zero is
/// followed, so a cap held still with a light touch never becomes the new zero (it would read
/// as a deflection once let go).
const REST_TIME: Duration = Duration::from_secs(2);

/// Axis order in reports and in [`Motion::axes`].
pub const AXES: [&str; 6] = ["tx", "ty", "tz", "rx", "ry", "rz"];

/// Raw counts to -1..1 per axis, with the resting offset taken out.
#[derive(Clone, Debug, Default)]
pub struct Shaper {
    /// The resting offset per axis.
    bias: [f32; 6],
    /// The stretch being checked for rest.
    window: Option<Window>,
}

/// Axis values since `since`: per axis the lowest, the highest and the sum, and how many.
#[derive(Clone, Debug)]
struct Window {
    since: Instant,
    lo: [i16; 6],
    hi: [i16; 6],
    sum: [f64; 6],
    count: u32,
}

impl Window {
    fn new(raw: [i16; 6], now: Instant) -> Self {
        Self { since: now, lo: raw, hi: raw, sum: raw.map(f64::from), count: 1 }
    }

    /// Takes `raw` if every axis stays within [`REST_SPAN`]; `false` if one would not.
    fn take(&mut self, raw: [i16; 6]) -> bool {
        let still = raw
            .iter()
            .zip(self.lo.iter().zip(&self.hi))
            .all(|(v, (lo, hi))| i32::from(*hi.max(v)) - i32::from(*lo.min(v)) <= REST_SPAN);
        if !still {
            return false;
        }
        for (((v, lo), hi), sum) in raw.iter().zip(&mut self.lo).zip(&mut self.hi).zip(&mut self.sum) {
            *lo = (*lo).min(*v);
            *hi = (*hi).max(*v);
            *sum += f64::from(*v);
        }
        self.count = self.count.saturating_add(1);
        true
    }
}

impl Shaper {
    pub fn bias(&self) -> [f32; 6] {
        self.bias
    }

    /// Takes the axes as they are at `now`; while they keep still inside the dead zone for
    /// [`REST_TIME`], their mean becomes the new resting offset.
    pub fn observe(&mut self, raw: [i16; 6], now: Instant) {
        let Some(w) = self.window.as_mut() else {
            self.window = Some(Window::new(raw, now));
            return;
        };
        if !w.take(raw) {
            *w = Window::new(raw, now);
            return;
        }
        if now.saturating_duration_since(w.since) < REST_TIME {
            return;
        }
        let mean = w.sum.map(|s| (s / f64::from(w.count.max(1))) as f32);
        let zone = FULL_SCALE * DEAD_ZONE;
        if mean.iter().zip(self.bias).all(|(m, b)| (m - b).abs() <= zone) {
            self.bias = mean;
        }
        self.window = None;
    }

    /// The axes shaped: offset out, dead zone, curve; -1..1.
    pub fn shape(&self, raw: [i16; 6]) -> [f32; 6] {
        let mut out = [0.0; 6];
        for ((o, r), b) in out.iter_mut().zip(raw).zip(self.bias) {
            *o = shape(f32::from(r) - b);
        }
        out
    }
}

/// One axis's counts (offset already out) to -1..1.
pub fn shape(counts: f32) -> f32 {
    let x = (counts / FULL_SCALE).clamp(-1.0, 1.0);
    let a = x.abs();
    if !a.is_finite() || a <= DEAD_ZONE {
        return 0.0;
    }
    x.signum() * ((a - DEAD_ZONE) / (1.0 - DEAD_ZONE)).powf(CURVE)
}

/// What the cap does now: six shaped axes in report order ([`AXES`]) and the buttons held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Motion {
    pub axes: [f32; 6],
    pub buttons: u8,
}

impl Motion {
    /// Whether the cap is deflected at all.
    pub fn moving(&self) -> bool {
        self.axes.iter().any(|a| *a != 0.0)
    }

    /// The axes as a camera flight, each -1..1, in the camera's own frame.
    ///
    /// The device's frame (HID): x right, y towards the user, z down; rotations about those
    /// axes. So pushing the cap away flies forward and pulling it up flies up. Tilting it
    /// pitches the way that felt right on the device (the first guess felt inverted),
    /// twisting it clockwise (seen from above) turns right, tilting it to the right rolls
    /// right.
    pub fn fly(&self) -> Fly {
        let [tx, ty, tz, rx, ry, rz] = self.axes;
        Fly { right: tx, up: -tz, forward: -ty, pitch: rx, yaw: rz, roll: ry }
    }
}

/// [`Motion::fly`]: translation and rotation rates, -1..1 each.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fly {
    pub right: f32,
    pub up: f32,
    pub forward: f32,
    /// Positive: the nose goes up.
    pub pitch: f32,
    /// Positive: turns right.
    pub yaw: f32,
    /// Positive: the picture's top goes right.
    pub roll: f32,
}

/// Where the reader stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not started (nobody asked for the device yet).
    #[default]
    Off,
    /// Looking for the device: not plugged in, or unplugged.
    Searching,
    Connected,
    /// Found but not usable (3DxWare may hold it); tried again after a while.
    Failed,
    /// No backend on this system.
    Unsupported,
}

/// [`SpaceNav::status`]: for `spacenav.status` and the Helpers window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub state: State,
    /// The product name the device reports, while connected.
    pub product: Option<String>,
    /// Why it is not connected, when there is a reason.
    pub message: Option<String>,
    /// Reports read since the start.
    pub reports: u64,
    /// The last raw counts, the resting offset, and the shaped axes ([`AXES`] order).
    pub raw: [i16; 6],
    pub bias: [f32; 6],
    pub motion: Motion,
}

#[derive(Default)]
struct Inner {
    status: Status,
    shaper: Shaper,
    /// When axes last arrived, translation and rotation apart: a group that stops arriving
    /// rests while the other still moves.
    axes_at: [Option<Instant>; 2],
    /// Buttons pressed (rising edges) since [`SpaceNav::take_presses`].
    presses: u8,
    /// Report shapes not understood, warned about once each.
    unknown: Vec<(u8, usize)>,
}

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    stop: AtomicBool,
    started: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    fn set_state(&self, state: State, product: Option<String>, message: Option<String>) {
        let mut inner = self.lock();
        if state != State::Connected {
            inner.axes_at = [None; 2];
            inner.status.motion = Motion::default();
        }
        if inner.status.state != state || inner.status.message != message {
            match (&state, &message) {
                (State::Connected, _) => log::info!("SpaceNavigator connected"),
                (_, Some(m)) => log::info!("SpaceNavigator: {m}"),
                _ => {}
            }
        }
        inner.status.state = state;
        inner.status.product = product;
        inner.status.message = message;
    }

    /// One report as read (id byte first).
    fn report(&self, buf: &[u8], now: Instant) {
        let mut inner = self.lock();
        inner.status.reports += 1;
        match parse(buf) {
            Some(Update::Axes { first, values }) => {
                for (k, v) in values.iter().enumerate() {
                    if let Some(slot) = inner.status.raw.get_mut(first + k) {
                        *slot = *v;
                    }
                }
                let raw = inner.status.raw;
                inner.shaper.observe(raw, now);
                inner.status.bias = inner.shaper.bias();
                for (g, at) in inner.axes_at.iter_mut().enumerate() {
                    if (first..first + values.len()).any(|a| a / 3 == g) {
                        *at = Some(now);
                    }
                }
                inner.status.motion.axes = inner.shaper.shape(raw);
            }
            Some(Update::Buttons(b)) => {
                let pressed = b & !inner.status.motion.buttons;
                inner.presses |= pressed;
                inner.status.motion.buttons = b;
            }
            None => {
                let key = (buf.first().copied().unwrap_or(0), buf.len().saturating_sub(1));
                if !inner.unknown.contains(&key) && inner.unknown.len() < 32 {
                    inner.unknown.push(key);
                    log::warn!("SpaceNavigator: ignoring report {} with {} bytes", key.0, key.1);
                }
            }
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Sleeps for `d` in small steps; `false` when asked to stop meanwhile.
    fn pause(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.stopping() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.stopping()
    }
}

/// The SpaceNavigator: a reader thread once [`SpaceNav::start`]ed, and what it last saw.
/// Cheap to clone.
#[derive(Clone, Default)]
pub struct SpaceNav(Arc<Shared>);

impl SpaceNav {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the reader thread, once; later calls do nothing.
    pub fn start(&self) {
        if self.0.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.0.clone();
        match std::thread::Builder::new().name("spacenav".into()).spawn(move || backend::run(&shared)) {
            Ok(t) => *lock(&self.0.thread) = Some(t),
            Err(e) => self.0.set_state(State::Failed, None, Some(format!("cannot start a thread: {e}"))),
        }
    }

    /// What the cap does now; at rest (all zero) while the device is away or silent.
    pub fn motion(&self) -> Motion {
        let inner = self.0.lock();
        let mut axes = inner.status.motion.axes;
        for (i, a) in axes.iter_mut().enumerate() {
            let fresh = inner.axes_at.get(i / 3).copied().flatten().is_some_and(|t| t.elapsed() < STALE);
            if !fresh {
                *a = 0.0;
            }
        }
        Motion { axes, ..inner.status.motion }
    }

    /// The buttons pressed since the last call, as bits (1: left, 2: right).
    pub fn take_presses(&self) -> u8 {
        std::mem::take(&mut self.0.lock().presses)
    }

    pub fn status(&self) -> Status {
        let mut s = self.0.lock().status.clone();
        s.motion = self.motion();
        s
    }

    /// Stops the reader thread and waits for it (at most one read timeout).
    pub fn shutdown(&self) {
        self.0.stop.store(true, Ordering::Release);
        let thread = lock(&self.0.thread).take();
        if let Some(t) = thread
            && t.join().is_err()
        {
            log::error!("the SpaceNavigator thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaping_has_a_dead_zone_and_a_curve() {
        assert_eq!(shape(0.0), 0.0);
        assert_eq!(shape(FULL_SCALE * DEAD_ZONE * 0.99), 0.0);
        assert_eq!(shape(FULL_SCALE), 1.0);
        assert_eq!(shape(-FULL_SCALE * 3.0), -1.0);
        let half = shape(FULL_SCALE * 0.5);
        assert!(half > 0.0 && half < 0.5, "{half}");
        assert_eq!(shape(f32::NAN), 0.0);
    }

    #[test]
    fn the_resting_offset_follows_a_cap_resting_inside_the_dead_zone() {
        let mut s = Shaper::default();
        let t0 = Instant::now();
        let ms = |k: u64| t0 + Duration::from_millis(k);
        // Resting at -20 on tx, inside the dead zone of 22.5 counts: zero already, and after
        // two seconds the zero itself.
        let rest = [-20, 2, 0, 0, -1, 0];
        assert_eq!(s.shape(rest), [0.0; 6]);
        for k in 0..250 {
            s.observe(rest, ms(10 * k));
        }
        assert!((s.bias()[0] + 20.0).abs() < 1e-3, "{:?}", s.bias());
        // So a drift by ten more counts still reads as zero.
        assert_eq!(s.shape([-30, 2, 0, 0, -1, 0]), [0.0; 6]);
        // A cap held still with a light touch beyond the dead zone is not at rest: the offset
        // stays, and the touch reads as a deflection.
        let held = [40, 0, 0, 0, 0, 0];
        for k in 0..400 {
            s.observe(held, ms(3000 + 10 * k));
        }
        assert!((s.bias()[0] + 20.0).abs() < 1e-3);
        assert!(s.shape(held)[0] > 0.0);
        // Moving resets the window.
        let mut s = Shaper::default();
        for k in 0..400i16 {
            s.observe([k % 3 * 5, 0, 0, 0, 0, 0], ms(10 * k as u64));
        }
        assert_eq!(s.bias(), [0.0; 6]);
    }

    #[test]
    fn reports_move_the_axes_and_buttons_count_presses() {
        let nav = SpaceNav::new();
        let now = Instant::now();
        nav.0.report(&[1, 0xc2, 0x01, 0, 0, 0, 0], now); // tx = 450
        nav.0.report(&[2, 0, 0, 0, 0, 0x3e, 0xfe], now); // rz = -450
        let m = nav.motion();
        assert_eq!(m.axes, [1.0, 0.0, 0.0, 0.0, 0.0, -1.0]);
        let fly = m.fly();
        assert_eq!((fly.right, fly.yaw), (1.0, -1.0));
        nav.0.report(&[3, 1, 0, 0, 0, 0, 0], now);
        nav.0.report(&[3, 3, 0, 0, 0, 0, 0], now);
        nav.0.report(&[3, 0, 0, 0, 0, 0, 0], now);
        assert_eq!(nav.take_presses(), 3);
        assert_eq!(nav.take_presses(), 0);
        // Hostile reports change nothing.
        for bad in [&[][..], &[1], &[1, 2, 3], &[9, 1, 2, 3, 4, 5, 6], &[3]] {
            nav.0.report(bad, now);
        }
        assert_eq!(nav.motion().axes, m.axes);
        // A group gone quiet rests while the other moves; a device gone quiet rests.
        nav.0.lock().axes_at[1] = now.checked_sub(STALE * 2);
        assert_eq!(nav.motion().axes, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        nav.0.lock().axes_at[0] = now.checked_sub(STALE * 2);
        assert!(!nav.motion().moving());
    }
}
