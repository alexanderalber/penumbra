//! The cook pass times of the last seconds (`cook.meter`, the cook time in the status bar): every
//! pass with its duration and a first-order lag of it, and what they add up to.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How far back the meter remembers.
pub const WINDOW: Duration = Duration::from_secs(8);
/// Time constant of the smoothed value.
pub const TAU: Duration = Duration::from_secs(1);

/// One cook pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// When the pass started.
    pub at: Instant,
    /// How long it took, in milliseconds.
    pub ms: f64,
    /// The smoothed value after it.
    pub smoothed: f64,
}

/// The passes of the last [`WINDOW`].
#[derive(Clone, Debug, Default)]
pub struct CookMeter {
    samples: VecDeque<Sample>,
}

/// What the passes in the window add up to, in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub passes: usize,
    /// The newest pass.
    pub last_ms: f64,
    /// First-order lag with a time constant of [`TAU`].
    pub smoothed_ms: f64,
    pub median_ms: f64,
    /// The 99th percentile: one pass in a hundred was slower ("1 % high").
    pub high_ms: f64,
    pub max_ms: f64,
    /// Share of the wall time the cook thread spent in passes (1 = no time left over).
    pub busy: f64,
}

impl CookMeter {
    /// Adds a pass that started `at` and took `took`; forgets passes older than the window.
    pub fn add(&mut self, at: Instant, took: Duration) {
        let ms = took.as_secs_f64() * 1000.0;
        let smoothed = match self.samples.back() {
            Some(prev) => {
                let dt = at.saturating_duration_since(prev.at).as_secs_f64();
                let k = 1.0 - (-dt / TAU.as_secs_f64()).exp();
                prev.smoothed + k * (ms - prev.smoothed)
            }
            None => ms,
        };
        self.samples.push_back(Sample { at, ms, smoothed });
        while self.samples.front().is_some_and(|s| at.saturating_duration_since(s.at) > WINDOW) {
            self.samples.pop_front();
        }
    }

    /// The smoothed value after the newest pass.
    pub fn smoothed(&self) -> Option<f64> {
        self.samples.back().map(|s| s.smoothed)
    }

    /// The passes that started within the window before `now`, oldest first.
    pub fn samples(&self, now: Instant) -> impl Iterator<Item = &Sample> {
        self.samples.iter().filter(move |s| now.saturating_duration_since(s.at) <= WINDOW)
    }

    /// The passes within the window before `now`; all zero without any.
    pub fn summary(&self, now: Instant) -> Summary {
        let mut ms: Vec<f64> = self.samples(now).map(|s| s.ms).collect();
        let (Some(first), Some(last)) = (self.samples(now).next(), self.samples.back()) else {
            return Summary::default();
        };
        ms.sort_by(f64::total_cmp);
        // Nearest rank.
        let rank = |p: f64| ms.get(((p * ms.len() as f64).ceil() as usize).saturating_sub(1)).copied();
        let span = now.saturating_duration_since(first.at).as_secs_f64();
        let total: f64 = ms.iter().sum();
        let busy = if span > 0.0 { (total / 1000.0 / span).min(1.0) } else { 0.0 };
        Summary {
            passes: ms.len(),
            last_ms: last.ms,
            smoothed_ms: last.smoothed,
            median_ms: rank(0.5).unwrap_or(0.0),
            high_ms: rank(0.99).unwrap_or(0.0),
            max_ms: ms.last().copied().unwrap_or(0.0),
            busy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: f64) -> Duration {
        Duration::from_secs_f64(v / 1000.0)
    }

    #[test]
    fn the_smoothed_value_follows_a_step_with_the_time_constant() {
        let t0 = Instant::now();
        let mut m = CookMeter::default();
        m.add(t0, ms(2.0));
        assert_eq!(m.smoothed(), Some(2.0));
        // A step to 12 ms at 60 Hz: after one time constant 63 % of the way.
        for i in 1..=60 {
            m.add(t0 + Duration::from_secs_f64(i as f64 / 60.0), ms(12.0));
        }
        let s = m.smoothed().unwrap();
        assert!((s - (2.0 + 10.0 * (1.0 - (-1.0f64).exp()))).abs() < 0.01, "{s}");
    }

    #[test]
    fn the_summary_covers_the_window() {
        let t0 = Instant::now();
        let mut m = CookMeter::default();
        // Ten seconds at 60 Hz: 4 ms, every fiftieth pass 30 ms, a slow pass before the window.
        m.add(t0, ms(100.0));
        for i in 1..600 {
            let v = if i % 50 == 0 { 30.0 } else { 4.0 };
            m.add(t0 + Duration::from_secs_f64(i as f64 / 60.0), ms(v));
        }
        let now = t0 + Duration::from_secs(10);
        let s = m.summary(now);
        assert!((475..=482).contains(&s.passes), "{s:?}");
        assert_eq!(s.median_ms, 4.0);
        assert!((s.high_ms - 30.0).abs() < 1e-9, "{s:?}");
        assert!((s.max_ms - 30.0).abs() < 1e-9, "{s:?}");
        assert!((0.26..0.28).contains(&s.busy), "{s:?}");
        assert_eq!(m.summary(now + Duration::from_secs(20)), Summary::default());
    }
}
