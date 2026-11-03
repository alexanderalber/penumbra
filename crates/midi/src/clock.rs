//! MIDI clock as a tempo: 24 clocks per quarter note, start, continue, stop and song position.
//!
//! The tempo is the mean of the last [`WINDOW`] intervals between clocks, measured by the
//! driver's time stamps (when the bytes arrived), not by when the app looked: a frame's jitter
//! would otherwise end up in the tempo. The position counts clocks from a start; a song
//! position moves it. While a device that has sent start or stop is stopped, its clocks keep
//! the tempo but do not move the position, as the MIDI spec says. A device that sends clocks
//! and never a start runs from its first clock.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Clocks per quarter note.
pub const PER_BEAT: u32 = 24;
/// Intervals the tempo is the mean of: two beats.
pub const WINDOW: usize = 48;
/// A pause longer than this starts the tempo measurement over (and the clock counts as gone).
pub const GAP: Duration = Duration::from_millis(500);

/// One device's clock.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    /// Clocks since the start, -1 before the first one after it.
    position: i64,
    /// The driver's time stamp of the last clock, in microseconds.
    last_stamp: Option<u64>,
    /// When the last clock arrived on this machine.
    last_at: Option<Instant>,
    intervals: VecDeque<u64>,
    /// Start, continue or stop was heard.
    transport: bool,
    playing: bool,
    /// Clocks received, all told.
    pub clocks: u64,
}

/// A clock as a reader sees it at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ClockState {
    /// Beats per minute, 0 until two clocks came.
    pub bpm: f64,
    /// Beats since the start, with the fraction since the last clock.
    pub beats: f64,
    /// Clocks are arriving.
    pub ticking: bool,
    /// Started (or never told to stop) and ticking.
    pub playing: bool,
    /// Clocks received, all told.
    pub clocks: u64,
}

impl Clock {
    pub fn new() -> Self {
        Self { position: -1, ..Self::default() }
    }

    /// A clock with the driver's time stamp `stamp` (microseconds) arrived at `now`.
    pub fn clock(&mut self, stamp: u64, now: Instant) {
        self.clocks += 1;
        let gone = self.last_at.is_none_or(|at| now.saturating_duration_since(at) > GAP);
        match self.last_stamp {
            Some(last) if !gone && stamp > last => {
                if self.intervals.len() >= WINDOW {
                    self.intervals.pop_front();
                }
                self.intervals.push_back(stamp - last);
            }
            // A stamp that went backwards (a reconnect) or a pause: measure anew.
            _ => self.intervals.clear(),
        }
        self.last_stamp = Some(stamp);
        self.last_at = Some(now);
        if self.playing || !self.transport {
            self.position = self.position.saturating_add(1);
        }
    }

    pub fn start(&mut self) {
        self.transport = true;
        self.playing = true;
        self.position = -1;
    }

    pub fn resume(&mut self) {
        self.transport = true;
        self.playing = true;
    }

    pub fn stop(&mut self) {
        self.transport = true;
        self.playing = false;
    }

    /// Song position in sixteenth notes: the next clock plays there.
    pub fn song_position(&mut self, sixteenths: u16) {
        self.position = i64::from(sixteenths) * 6 - 1;
    }

    /// Seconds between clocks, `None` before two came.
    fn interval(&self) -> Option<f64> {
        if self.intervals.is_empty() {
            return None;
        }
        let sum: u64 = self.intervals.iter().sum();
        // `as`: a count of at most WINDOW and microseconds far below 2^53.
        let mean = sum as f64 / self.intervals.len() as f64 / 1e6;
        (mean > 0.0).then_some(mean)
    }

    /// The clock at `now`.
    pub fn state(&self, now: Instant) -> ClockState {
        let since = self.last_at.map(|at| now.saturating_duration_since(at));
        let ticking = since.is_some_and(|s| s <= GAP);
        let interval = self.interval();
        let advancing = ticking && (self.playing || !self.transport);
        // Between two clocks the position glides on, up to the next one.
        let fraction = match (advancing, interval, since) {
            (true, Some(i), Some(s)) => (s.as_secs_f64() / i).clamp(0.0, 0.999),
            _ => 0.0,
        };
        // `as`: clock counts far below 2^53.
        let ticks = self.position.max(0) as f64 + if self.position >= 0 { fraction } else { 0.0 };
        ClockState {
            bpm: interval.map_or(0.0, |i| 60.0 / (i * f64::from(PER_BEAT))),
            beats: ticks / f64::from(PER_BEAT),
            ticking,
            playing: advancing,
            clocks: self.clocks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` clocks at `bpm` from `t0` (and the stamp `s0`), returns the time and stamp after.
    fn run(c: &mut Clock, n: u32, bpm: f64, t0: Instant, s0: u64) -> (Instant, u64) {
        // `as`: microseconds of one clock at a sane tempo.
        let step = (60e6 / (bpm * 24.0)) as u64;
        let (mut t, mut s) = (t0, s0);
        for _ in 0..n {
            c.clock(s, t);
            t += Duration::from_micros(step);
            s += step;
        }
        (t, s)
    }

    #[test]
    fn tempo_and_position_from_clocks() {
        let mut c = Clock::new();
        let t0 = Instant::now();
        let (t, _) = run(&mut c, 49, 120.0, t0, 1000);
        let st = c.state(t - Duration::from_micros(20_833));
        assert!((st.bpm - 120.0).abs() < 0.01, "{}", st.bpm);
        // 49 clocks from the first: the last one is at clock 48, two beats in.
        assert!((st.beats - 2.0).abs() < 1e-6, "{}", st.beats);
        assert!(st.ticking && st.playing);
        // Half way to the next clock, the position has glided on by half a clock.
        let half = c.state(t - Duration::from_micros(10_416));
        assert!((half.beats - (2.0 + 0.5 / 24.0)).abs() < 1e-3, "{}", half.beats);
        // Silent for a second: no longer ticking, the tempo stays.
        let later = c.state(t + Duration::from_secs(1));
        assert!(!later.ticking && !later.playing);
        assert!((later.bpm - 120.0).abs() < 0.01);
    }

    #[test]
    fn start_stop_and_song_position() {
        let mut c = Clock::new();
        let t0 = Instant::now();
        let (t, s) = run(&mut c, 30, 100.0, t0, 0);
        c.start();
        let (t, s) = run(&mut c, 25, 100.0, t, s);
        // The first clock after start is beat 0; 25 clocks reach clock 24, beat 1.
        assert!((c.state(t - Duration::from_micros(25_000)).beats - 1.0).abs() < 1e-6);
        c.stop();
        let (t, s) = run(&mut c, 10, 100.0, t, s);
        let st = c.state(t - Duration::from_micros(25_000));
        assert!(!st.playing && st.ticking);
        assert!((st.beats - 1.0).abs() < 1e-6, "stopped clocks do not move it: {}", st.beats);
        // Song position 8 (two beats), then continue.
        c.song_position(8);
        c.resume();
        let (t, _) = run(&mut c, 1, 100.0, t, s);
        assert!((c.state(t - Duration::from_micros(25_000)).beats - 2.0).abs() < 1e-6);
    }
}
