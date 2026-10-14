//! Show time.
//!
//! Time is an integer number of [`Tick`]s at [`TICKS_PER_SECOND`], the same rate as FilmCraft
//! and EffectCraft use, so video timestamps from FilmCraft need no conversion. The rate divides
//! evenly into every common frame duration (23.976 to 240 fps, including the 1001 family) and
//! every common audio sample duration (8 to 192 kHz, including 44.1 kHz), so frames and audio
//! cannot drift apart. An `i64` of ticks covers about 400 days.
//!
//! The [`ShowClock`] counts frames at the project's [`Rate`]; time is derived from the frame
//! number, never from wall-clock deltas, so animation is deterministic. What drives the clock
//! (projector vsync, or a [`TimerPacer`] when no output is open) lives elsewhere and only tells
//! the clock how many frame periods are due.

use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Error;

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 254_016_000_000;

/// A point or a duration in show time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    /// Seconds to ticks, rounded and saturating (NaN is zero), so times from files, users and
    /// Claude cannot overflow later arithmetic.
    pub fn from_seconds(seconds: f64) -> Tick {
        // `as` saturates and maps NaN to 0.
        Tick((seconds * TICKS_PER_SECOND as f64).round() as i64)
    }

    pub fn seconds(self) -> f64 {
        self.0 as f64 / TICKS_PER_SECOND as f64
    }

    /// Exact conversion from `units` at `per_second`, e.g. samples at 48 000 (floored when the
    /// rate does not divide [`TICKS_PER_SECOND`]; a zero rate counts as 1).
    pub fn from_units(units: i64, per_second: i64) -> Tick {
        Tick(saturate(i128::from(units) * i128::from(TICKS_PER_SECOND) / nonzero(per_second)))
    }

    /// This time as a count of units at `per_second`, floored.
    pub fn to_units_floor(self, per_second: i64) -> i64 {
        saturate((i128::from(self.0) * i128::from(per_second)).div_euclid(i128::from(TICKS_PER_SECOND)))
    }
}

fn nonzero(v: i64) -> i128 {
    if v == 0 { 1 } else { i128::from(v) }
}

fn saturate(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// A frame rate `num / den` frames per second whose frame duration is a whole number of ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RateRepr", into = "RateRepr")]
pub struct Rate {
    num: u32,
    den: u32,
}

/// Serialised form of [`Rate`], validated on the way in.
#[derive(Serialize, Deserialize)]
struct RateRepr {
    num: u32,
    den: u32,
}

impl TryFrom<RateRepr> for Rate {
    type Error = Error;
    fn try_from(r: RateRepr) -> Result<Self, Error> {
        Rate::new(r.num, r.den)
    }
}

impl From<Rate> for RateRepr {
    fn from(r: Rate) -> Self {
        RateRepr { num: r.num, den: r.den }
    }
}

impl Default for Rate {
    fn default() -> Self {
        Self::FPS_60
    }
}

impl Rate {
    pub const FPS_60: Rate = Rate { num: 60, den: 1 };

    pub fn new(num: u32, den: u32) -> Result<Rate, Error> {
        if num == 0 || den == 0 {
            return Err(Error::new(format!("frame rate {num}/{den} must be positive")));
        }
        if num / den > 1000 {
            return Err(Error::new(format!("frame rate {num}/{den} is above 1000 fps")));
        }
        let ticks = i128::from(TICKS_PER_SECOND) * i128::from(den);
        if ticks % i128::from(num) != 0 {
            return Err(Error::new(format!(
                "frame rate {num}/{den} has no whole-tick frame duration; use a common rate such as 60, 50, 30000/1001"
            )));
        }
        Ok(Rate { num, den })
    }

    /// Whole frames per second, e.g. `Rate::fps(60)`.
    pub fn fps(fps: u32) -> Result<Rate, Error> {
        Self::new(fps, 1)
    }

    pub fn num(self) -> u32 {
        self.num
    }

    pub fn den(self) -> u32 {
        self.den
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Duration of one frame in ticks (exact by construction).
    pub fn frame_ticks(self) -> i64 {
        saturate(i128::from(TICKS_PER_SECOND) * i128::from(self.den) / i128::from(self.num.max(1)))
    }

    /// Duration of one frame in wall-clock time.
    pub fn period(self) -> Duration {
        let nanos = 1_000_000_000u128 * u128::from(self.den) / u128::from(self.num.max(1));
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Start of frame `frame`, saturating.
    pub fn tick_of(self, frame: i64) -> Tick {
        Tick(saturate(i128::from(frame) * i128::from(self.frame_ticks())))
    }

    /// The frame that contains `t`.
    pub fn frame_at(self, t: Tick) -> i64 {
        t.0.div_euclid(self.frame_ticks().max(1))
    }

    /// Whole frame periods in `elapsed`, floored.
    pub fn frames_in(self, elapsed: Duration) -> i64 {
        let frames = elapsed.as_nanos().saturating_mul(u128::from(self.num))
            / (1_000_000_000u128 * u128::from(self.den.max(1)));
        i64::try_from(frames).unwrap_or(i64::MAX)
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 { write!(f, "{} fps", self.num) } else { write!(f, "{}/{} fps", self.num, self.den) }
    }
}

/// What the clock does when frames were missed (a cook took too long).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatePolicy {
    /// Skip ahead to the frame that is due; the default for shows.
    #[default]
    Realtime,
    /// Never skip a frame; time slows down instead. For offline renders.
    EveryFrame,
}

/// Where the show is, as handed to every cook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameTime {
    pub frame: i64,
    pub tick: Tick,
    pub rate: Rate,
}

impl FrameTime {
    pub fn seconds(&self) -> f64 {
        self.tick.seconds()
    }
}

/// Result of [`ShowClock::advance`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Advance {
    /// Frames the clock moved forward.
    pub advanced: i64,
    /// Frames that were due but never shown (only with [`LatePolicy::Realtime`]).
    pub skipped: i64,
}

/// The show clock: a frame counter at the project rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShowClock {
    rate: Rate,
    policy: LatePolicy,
    frame: i64,
    playing: bool,
}

impl ShowClock {
    pub fn new(rate: Rate, policy: LatePolicy) -> Self {
        Self { rate, policy, frame: 0, playing: true }
    }

    pub fn now(&self) -> FrameTime {
        FrameTime { frame: self.frame, tick: self.rate.tick_of(self.frame), rate: self.rate }
    }

    pub fn rate(&self) -> Rate {
        self.rate
    }

    pub fn policy(&self) -> LatePolicy {
        self.policy
    }

    pub fn set_policy(&mut self, policy: LatePolicy) {
        self.policy = policy;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Jump to `frame` (clamped to 0 and later).
    pub fn seek(&mut self, frame: i64) {
        self.frame = frame.max(0);
    }

    /// Change the rate, keeping the current time (the frame containing it).
    pub fn set_rate(&mut self, rate: Rate) {
        let now = self.rate.tick_of(self.frame);
        self.rate = rate;
        self.frame = rate.frame_at(now);
    }

    /// Called once per pacing event with the number of frame periods that elapsed since the
    /// previous call (1 normally, more after a stall, 0 when the pacer fired early).
    pub fn advance(&mut self, due: u64) -> Advance {
        if !self.playing || due == 0 {
            return Advance::default();
        }
        let due = i64::try_from(due).unwrap_or(i64::MAX);
        let advanced = match self.policy {
            LatePolicy::Realtime => due,
            LatePolicy::EveryFrame => 1,
        };
        self.frame = self.frame.saturating_add(advanced);
        Advance { advanced, skipped: advanced - 1 }
    }
}

/// Timer fallback for pacing when no projector output is open: counts the frame periods of
/// `rate` that have elapsed since the start.
#[derive(Clone, Debug)]
pub struct TimerPacer {
    start: Instant,
    rate: Rate,
    counted: i64,
}

impl TimerPacer {
    pub fn new(rate: Rate, start: Instant) -> Self {
        Self { start, rate, counted: 0 }
    }

    /// Frame periods elapsed between the previous call and `now`, for [`ShowClock::advance`].
    pub fn due(&mut self, now: Instant) -> u64 {
        let total = self.rate.frames_in(now.saturating_duration_since(self.start));
        let due = total.saturating_sub(self.counted).max(0);
        self.counted = self.counted.max(total);
        u64::try_from(due).unwrap_or(0)
    }

    /// When the next frame period starts, for sleeping until then.
    pub fn next_deadline(&self) -> Instant {
        let next = self.counted.saturating_add(1);
        let nanos =
            i128::from(next) * 1_000_000_000 * i128::from(self.rate.den) / i128::from(self.rate.num.max(1));
        let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
        self.start.checked_add(Duration::from_nanos(nanos)).unwrap_or(self.start)
    }
}

/// What drives the show clock at the moment ([`Pacer::source`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceSource {
    /// A projector output's vsync.
    Vsync,
    /// A timer: no output paces, or its vsync stopped arriving.
    Timer,
}

/// Paces the show clock from a projector output's vsync, falling back to a [`TimerPacer`] when no
/// vsync has arrived for [`Pacer::FALLBACK_PERIODS`] frame periods (no output open, window hidden,
/// display unplugged).
///
/// A precise timer thread may stand in for the output, feeding [`Pacer::vsync`] at the show
/// rate; the [`TimerPacer`] then only takes over if its ticks stop too.
///
/// One vsync is one frame; a gap of several periods (a stall) counts as that many, so the
/// `realtime` policy still skips ahead. A display whose refresh differs slightly from the show
/// rate (59.94 Hz for 60 fps) therefore runs show time at the display's pace, as in
/// TouchDesigner; only outputs within a percent of the show rate may pace (the output crate
/// decides).
#[derive(Clone, Debug)]
pub struct Pacer {
    rate: Rate,
    timer: TimerPacer,
    last_vsync: Option<Instant>,
}

impl Pacer {
    pub const FALLBACK_PERIODS: u32 = 3;

    pub fn new(rate: Rate, start: Instant) -> Self {
        Self { rate, timer: TimerPacer::new(rate, start), last_vsync: None }
    }

    fn fallback(&self) -> Duration {
        self.rate.period().saturating_mul(Self::FALLBACK_PERIODS)
    }

    fn vsync_active(&self, now: Instant) -> bool {
        self.last_vsync.is_some_and(|v| now.saturating_duration_since(v) < self.fallback())
    }

    pub fn source(&self, now: Instant) -> PaceSource {
        if self.vsync_active(now) { PaceSource::Vsync } else { PaceSource::Timer }
    }

    /// The pacing output presented at `at`: frame periods due, at least 1.
    pub fn vsync(&mut self, at: Instant) -> u64 {
        let due = match self.last_vsync {
            Some(prev) if at > prev => {
                let period = self.rate.period().as_secs_f64().max(1e-6);
                let periods = at.saturating_duration_since(prev).as_secs_f64() / period;
                // `as` saturates; a gap of 1.4 periods is one frame, 1.6 are two.
                (periods.round() as u64).max(1)
            }
            _ => 1,
        };
        self.last_vsync = Some(at.max(self.last_vsync.unwrap_or(at)));
        due
    }

    /// Frame periods due by the timer; 0 while vsync paces. When vsync stops, the timer counts
    /// from the last vsync, so the frames missed in between are due at once (and skipped under
    /// `realtime`).
    pub fn due(&mut self, now: Instant) -> u64 {
        if self.vsync_active(now) {
            return 0;
        }
        if let Some(last) = self.last_vsync.take() {
            self.timer = TimerPacer::new(self.rate, last);
        }
        self.timer.due(now)
    }

    /// When to look again: the timer's next period, or the moment vsync would count as stopped.
    pub fn next_deadline(&self) -> Instant {
        match self.last_vsync {
            Some(v) => v.checked_add(self.fallback()).unwrap_or(v),
            None => self.timer.next_deadline(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_rates_are_exact() {
        for (num, den) in [(24, 1), (25, 1), (30, 1), (50, 1), (60, 1), (120, 1), (144, 1), (240, 1)] {
            let r = Rate::new(num, den).unwrap();
            assert_eq!(r.frame_ticks() * i64::from(num), TICKS_PER_SECOND * i64::from(den));
        }
        for (num, den) in [(24000, 1001), (30000, 1001), (60000, 1001)] {
            let r = Rate::new(num, den).unwrap();
            assert_eq!(
                i128::from(r.frame_ticks()) * i128::from(num),
                i128::from(TICKS_PER_SECOND) * i128::from(den)
            );
        }
        for sr in [8000, 22050, 44100, 48000, 96000, 192000] {
            assert_eq!(TICKS_PER_SECOND % sr, 0, "{sr}");
        }
    }

    #[test]
    fn odd_rates_are_refused() {
        assert!(Rate::new(165, 1).is_err());
        assert!(Rate::new(0, 1).is_err());
        assert!(Rate::new(1, 0).is_err());
        assert!(Rate::new(100_000, 1).is_err());
    }

    #[test]
    fn frames_and_ticks() {
        let r = Rate::FPS_60;
        assert_eq!(r.tick_of(60), Tick(TICKS_PER_SECOND));
        assert_eq!(r.frame_at(Tick(TICKS_PER_SECOND - 1)), 59);
        assert_eq!(r.frame_at(Tick(-1)), -1);
        assert_eq!(r.tick_of(i64::MAX), Tick(i64::MAX));
        assert_eq!(Tick::from_units(48_000, 48_000), Tick(TICKS_PER_SECOND));
        assert_eq!(Tick(TICKS_PER_SECOND).to_units_floor(44_100), 44_100);
        assert_eq!(Tick::from_seconds(f64::NAN), Tick::ZERO);
        assert_eq!(Tick::from_seconds(f64::INFINITY), Tick(i64::MAX));
        assert_eq!(Tick::from_units(5, 0), Tick(5 * TICKS_PER_SECOND));
    }

    #[test]
    fn frames_in_wall_time() {
        let r = Rate::FPS_60;
        assert_eq!(r.frames_in(Duration::from_millis(999)), 59);
        assert_eq!(r.frames_in(Duration::from_secs(1)), 60);
        let ntsc = Rate::new(30000, 1001).unwrap();
        assert_eq!(ntsc.frames_in(Duration::from_secs(1001)), 30000);
        assert_eq!(r.frames_in(Duration::MAX), i64::MAX);
    }

    #[test]
    fn realtime_skips_every_frame_does_not() {
        let mut c = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        assert_eq!(c.advance(1), Advance { advanced: 1, skipped: 0 });
        assert_eq!(c.advance(3), Advance { advanced: 3, skipped: 2 });
        assert_eq!(c.now().frame, 4);
        c.set_policy(LatePolicy::EveryFrame);
        assert_eq!(c.advance(3), Advance { advanced: 1, skipped: 0 });
        assert_eq!(c.now().frame, 5);
        assert_eq!(c.advance(0), Advance::default());
    }

    #[test]
    fn paused_clock_stands_still() {
        let mut c = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        c.pause();
        assert_eq!(c.advance(5).advanced, 0);
        assert_eq!(c.now().frame, 0);
        c.play();
        c.advance(2);
        assert_eq!(c.now().tick, Rate::FPS_60.tick_of(2));
        c.seek(-5);
        assert_eq!(c.now().frame, 0);
    }

    #[test]
    fn rate_change_keeps_time() {
        let mut c = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        c.seek(120);
        c.set_rate(Rate::fps(30).unwrap());
        assert_eq!(c.now().frame, 60);
        assert_eq!(c.now().seconds(), 2.0);
    }

    #[test]
    fn timer_pacer_counts_periods() {
        let start = Instant::now();
        let mut p = TimerPacer::new(Rate::FPS_60, start);
        assert_eq!(p.due(start), 0);
        assert_eq!(p.due(start + Duration::from_millis(17)), 1);
        assert_eq!(p.due(start + Duration::from_millis(20)), 0);
        assert_eq!(p.due(start + Duration::from_millis(100)), 5);
        // Time going backwards (a clock glitch) is not negative.
        assert_eq!(p.due(start), 0);
        assert!(p.next_deadline() > start + Duration::from_millis(100));
    }

    #[test]
    fn vsync_paces_and_the_timer_takes_over() {
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let mut p = Pacer::new(Rate::FPS_60, start);
        assert_eq!(p.source(start), PaceSource::Timer);
        assert_eq!(p.due(ms(17)), 1);
        assert_eq!(p.vsync(ms(20)), 1);
        assert_eq!(p.source(ms(21)), PaceSource::Vsync);
        // The timer is silent while vsync paces.
        assert_eq!(p.due(ms(36)), 0);
        assert_eq!(p.vsync(ms(37)), 1);
        // A 59.94 Hz display is one frame per vsync.
        assert_eq!(p.vsync(ms(54)), 1);
        // A stall of about three periods counts as three.
        assert_eq!(p.vsync(ms(104)), 3);
        assert_eq!(p.next_deadline(), ms(104) + Rate::FPS_60.period() * 3);
        // Vsync stops: after three periods the timer counts from the last vsync.
        assert_eq!(p.due(ms(140)), 0);
        assert_eq!(p.due(ms(160)), 3);
        assert_eq!(p.source(ms(160)), PaceSource::Timer);
        assert_eq!(p.due(ms(177)), 1);
        // A vsync from the past (threads racing) is still one frame and does not go back.
        assert_eq!(p.vsync(ms(180)), 1);
        assert_eq!(p.vsync(ms(179)), 1);
        assert_eq!(p.source(ms(200)), PaceSource::Vsync);
    }

    #[test]
    fn rate_serde_validates() {
        let r: Rate = serde_json::from_str(r#"{"num":30000,"den":1001}"#).unwrap();
        assert_eq!(r.to_string(), "30000/1001 fps");
        assert!(serde_json::from_str::<Rate>(r#"{"num":165,"den":1}"#).is_err());
    }
}
