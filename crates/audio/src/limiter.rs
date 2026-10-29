//! A look-ahead peak limiter: the last guard before a speaker.
//!
//! Every sample's peak across channels asks for the gain that brings it down to the ceiling.
//! That wish goes through three steps before it is applied to the sound, which is delayed by the
//! look-ahead:
//!
//! 1. the smallest wish of the last look-ahead plus one samples, so the gain is already down when
//!    a peak leaves the delay;
//! 2. a release: the gain falls at once and rises again with a time constant;
//! 3. a moving average over the look-ahead, which turns the fall into a ramp instead of a step.
//!
//! The average at the moment a peak leaves the delay covers only gains at or below what that
//! peak asked for, so the output never exceeds the ceiling (rounding aside, which the final
//! clip at the ceiling takes). A sound below the ceiling passes unchanged, delayed by the
//! look-ahead. Non-finite samples become silence before anything else.

use std::collections::VecDeque;

/// The loudest a limited sample may be: -1 dBFS.
pub const CEILING_DB: f64 = -1.0;
/// How far ahead the limiter looks, seconds.
pub const LOOKAHEAD_SECONDS: f64 = 0.003;
/// How fast the gain recovers after a peak (time constant), seconds.
pub const RELEASE_SECONDS: f64 = 0.1;

/// Decibels to a factor.
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A factor to decibels (silence is -inf).
pub fn gain_to_db(gain: f64) -> f64 {
    20.0 * gain.max(0.0).log10()
}

/// One limiter for a stream of interleaved or per-channel blocks at one rate.
pub struct Limiter {
    rate: u32,
    channels: usize,
    ceiling: f64,
    lookahead: usize,
    release: f64,
    /// The delayed sound, per channel, `lookahead` samples each.
    delay: Vec<VecDeque<f64>>,
    /// (sample number, wished gain), increasing in gain: the minimum of the window at the front.
    window: VecDeque<(u64, f64)>,
    /// The released gain of the last `lookahead` samples, for the moving average.
    recent: VecDeque<f64>,
    sum: f64,
    envelope: f64,
    n: u64,
    /// The lowest gain applied since [`Limiter::take_reduction`].
    lowest: f64,
}

impl Limiter {
    /// A limiter at `rate` for `channels` channels, with the default ceiling, look-ahead and
    /// release.
    pub fn new(rate: u32, channels: usize) -> Self {
        let lookahead = ((f64::from(rate.max(1)) * LOOKAHEAD_SECONDS).round() as usize).max(1);
        let release = 1.0 - (-1.0 / (f64::from(rate.max(1)) * RELEASE_SECONDS)).exp();
        let mut l = Self {
            rate,
            channels: channels.max(1),
            ceiling: db_to_gain(CEILING_DB),
            lookahead,
            release,
            delay: Vec::new(),
            window: VecDeque::with_capacity(lookahead + 2),
            recent: VecDeque::with_capacity(lookahead + 1),
            sum: 0.0,
            envelope: 1.0,
            n: 0,
            lowest: 1.0,
        };
        l.reset();
        l
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The ceiling as a factor of full scale.
    pub fn ceiling(&self) -> f64 {
        self.ceiling
    }

    /// The delay the look-ahead adds, in samples.
    pub fn lookahead(&self) -> usize {
        self.lookahead
    }

    /// Starts over: silence in the delay, no reduction.
    pub fn reset(&mut self) {
        self.delay = (0..self.channels)
            .map(|_| {
                let mut d = VecDeque::with_capacity(self.lookahead + 1);
                d.extend(std::iter::repeat_n(0.0, self.lookahead));
                d
            })
            .collect();
        self.window.clear();
        self.recent.clear();
        self.recent.extend(std::iter::repeat_n(1.0, self.lookahead));
        // `as`: a small count.
        self.sum = self.lookahead as f64;
        self.envelope = 1.0;
        self.n = 0;
        self.lowest = 1.0;
    }

    /// Limits one block in place: one slice per channel, all of one length (shorter ones are
    /// read as silence and not written beyond their end). The output is the input of
    /// [`Limiter::lookahead`] samples before.
    pub fn process(&mut self, channels: &mut [Vec<f64>]) {
        let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
        // `as`: a small count.
        let la = self.lookahead as f64;
        for i in 0..frames {
            let mut peak = 0.0f64;
            for c in channels.iter_mut() {
                if let Some(v) = c.get_mut(i) {
                    if !v.is_finite() {
                        *v = 0.0;
                    }
                    peak = peak.max(v.abs());
                }
            }
            let wish = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
            // The window's minimum: drop larger wishes behind this one, and what fell out.
            while self.window.back().is_some_and(|&(_, g)| g >= wish) {
                self.window.pop_back();
            }
            self.window.push_back((self.n, wish));
            let oldest = self.n.saturating_sub(self.lookahead as u64);
            while self.window.front().is_some_and(|&(k, _)| k < oldest) {
                self.window.pop_front();
            }
            let least = self.window.front().map_or(1.0, |&(_, g)| g);
            self.envelope = if least < self.envelope {
                least
            } else {
                self.envelope + (least - self.envelope) * self.release
            };
            self.recent.push_back(self.envelope);
            self.sum += self.envelope;
            if let Some(old) = self.recent.pop_front() {
                self.sum -= old;
            }
            if self.n.is_multiple_of(4096) {
                // Rounding in the running sum must not build up over days.
                self.sum = self.recent.iter().sum();
            }
            let gain = (self.sum / la).clamp(0.0, 1.0);
            self.lowest = self.lowest.min(gain);
            for (ci, c) in channels.iter_mut().enumerate() {
                let Some(d) = self.delay.get_mut(ci) else { continue };
                let input = c.get(i).copied().unwrap_or(0.0);
                d.push_back(input);
                let out = d.pop_front().unwrap_or(0.0) * gain;
                if let Some(v) = c.get_mut(i) {
                    *v = out.clamp(-self.ceiling, self.ceiling);
                }
            }
            self.n += 1;
        }
    }

    /// The largest reduction since the last call, in dB (0: none), and starts over.
    pub fn take_reduction(&mut self) -> f64 {
        let db = -gain_to_db(self.lowest);
        self.lowest = 1.0;
        if db.is_finite() { db.max(0.0) } else { 120.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, amp: f64, n: usize, from: usize) -> Vec<f64> {
        (from..from + n)
            .map(|i| amp * (std::f64::consts::TAU * freq * i as f64 / f64::from(rate)).sin())
            .collect()
    }

    #[test]
    fn a_loud_burst_never_exceeds_the_ceiling() {
        let rate = 48_000;
        let mut l = Limiter::new(rate, 2);
        let mut peak = 0.0f64;
        let mut reduction = 0.0f64;
        for b in 0..120 {
            // -20 dB, then a burst of +12 dB for a quarter second, then -20 dB again.
            let amp = if (30..45).contains(&b) { db_to_gain(12.0) } else { db_to_gain(-20.0) };
            let left = sine(rate, 440.0, amp, 800, b * 800);
            let right: Vec<f64> = left.iter().map(|v| -v * 0.7).collect();
            let mut block = vec![left, right];
            l.process(&mut block);
            for c in &block {
                for v in c {
                    assert!(v.is_finite());
                    peak = peak.max(v.abs());
                }
            }
            reduction = reduction.max(l.take_reduction());
        }
        assert!(peak <= db_to_gain(CEILING_DB) + 1e-12, "peak {peak}");
        assert!(reduction > 12.0, "reduction {reduction}");
    }

    #[test]
    fn a_quiet_tone_passes_unchanged_after_the_lookahead() {
        let rate = 48_000;
        let mut l = Limiter::new(rate, 1);
        let la = l.lookahead();
        let input = sine(rate, 1000.0, db_to_gain(-20.0), 48_000, 0);
        let mut out = Vec::new();
        for chunk in input.chunks(800) {
            let mut block = vec![chunk.to_vec()];
            l.process(&mut block);
            out.extend_from_slice(&block[0]);
        }
        for (i, v) in out.iter().enumerate().skip(la) {
            assert!((v - input[i - la]).abs() < 1e-12, "sample {i}");
        }
        assert_eq!(l.take_reduction(), 0.0);
        let rms = |x: &[f64]| (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
        let db = gain_to_db(rms(&out[la..]) / rms(&input[..input.len() - la]));
        assert!(db.abs() < 0.1, "{db} dB");
    }

    #[test]
    fn non_numbers_become_silence_and_a_spike_is_caught() {
        let mut l = Limiter::new(44_100, 2);
        let mut block = vec![vec![f64::NAN, f64::INFINITY, 1e30, -1e30, 0.5], vec![0.1; 5]];
        l.process(&mut block);
        let mut more = vec![vec![0.0; 400], vec![0.0; 400]];
        l.process(&mut more);
        for v in block.iter().chain(more.iter()).flatten() {
            assert!(v.is_finite() && v.abs() <= l.ceiling() + 1e-12, "{v}");
        }
        // The spikes of 1e30 are pulled to the ceiling, the 0.1 beside them far below it.
        assert!(l.take_reduction() > 100.0);
    }

    #[test]
    fn the_gain_comes_back_with_the_release() {
        let rate = 48_000;
        let mut l = Limiter::new(rate, 1);
        let mut block = vec![vec![2.0; 10]];
        l.process(&mut block);
        // A second of a quiet tone afterwards: after five time constants it is untouched.
        let mut tail = vec![sine(rate, 500.0, 0.1, rate as usize, 0)];
        l.process(&mut tail);
        let late = &tail[0][(rate as usize) * 6 / 10..];
        let input = sine(rate, 500.0, 0.1, rate as usize, 0);
        let la = l.lookahead();
        for (k, v) in late.iter().enumerate() {
            let i = (rate as usize) * 6 / 10 + k;
            assert!((v - input[i - la]).abs() < 0.1 * 0.01, "sample {i}: {v}");
        }
    }
}
