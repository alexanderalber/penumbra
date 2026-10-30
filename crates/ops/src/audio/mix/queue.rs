//! One input of `audio.mix` that does not arrive in step with the mix: converted to the mix's
//! rate, queued, and taken from in the lengths the mix needs.
//!
//! - **Target:** the queue keeps about [`Queue::target`] samples after each take: the swing of
//!   its fill (the most it moved within a second, over the last [`SWING_WINDOWS`] seconds)
//!   plus [`MARGIN_SECONDS`]. A source on the show clock swings by a sample or two; a device
//!   by about one of its buffers. The swing starts at an estimate from the first block and is
//!   measured from then on. The target rises at once and falls slowly
//!   ([`TARGET_FALL_SECONDS`]), so the drift loop sees a steady goal and the pitch stays put.
//! - **Drift:** a device's clock runs a little faster or slower than the show clock. A slow PI
//!   loop steers the smoothed fill to the target by nudging the converter's ratio, at most
//!   [`MAX_CORRECTION`]. Within [`DEADBAND_SECONDS`] of the target it holds the ratio, so a
//!   source on the show clock, whose fill only moves by a sample, plays at exactly its pitch.
//! - **Start and underrun:** the queue starts with the target in silence, so the first block
//!   plays at once. When a take finds too little, what is left fades out over [`FADE_SECONDS`],
//!   the input is silent until the queue holds a block and the target again, and then it fades
//!   in. Counted as an underrun.
//! - **Overrun:** when the fill runs far beyond the target (a stalled mix, a burst from a
//!   device), the oldest samples are dropped down to the target, crossfaded over
//!   [`FADE_SECONDS`]. Counted as an overrun. The queue never holds more than
//!   [`MAX_QUEUE_SECONDS`].

use std::collections::VecDeque;

use penumbra_audio::Resampler;

/// Length of a fade at a start, a gap or a drop.
pub const FADE_SECONDS: f64 = 0.004;
/// Kept on top of the measured swing.
pub const MARGIN_SECONDS: f64 = 0.001;
/// Time constant of a falling target, seconds.
pub const TARGET_FALL_SECONDS: f64 = 20.0;
/// The largest target.
pub const MAX_TARGET_SECONDS: f64 = 0.2;
/// The most a queue holds; beyond it the oldest samples go.
pub const MAX_QUEUE_SECONDS: f64 = 0.5;
/// The swing is measured in windows this long ...
pub const SWING_WINDOW_SECONDS: f64 = 1.0;
/// ... and the largest of the last this many counts.
pub const SWING_WINDOWS: usize = 4;
/// Largest change of the ratio the drift loop may make (2000 ppm).
pub const MAX_CORRECTION: f64 = penumbra_audio::playback::MAX_CORRECTION;
/// Proportional gain of the drift loop: ratio change per second of fill error.
const KP: f64 = 0.1;
/// Integral gain, per second squared (with [`KP`], critically damped).
const KI: f64 = 0.0025;
/// The drift loop leaves a fill this close to the target alone, seconds.
pub const DEADBAND_SECONDS: f64 = 0.0005;
/// Time constant of the fill's smoothing, seconds.
const SMOOTH_SECONDS: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Silent until the queue holds a block and the target.
    Waiting,
    Playing,
}

/// The most the fill moved, per window.
#[derive(Clone, Debug)]
struct Swing {
    window: usize,
    seen: usize,
    range: Option<(f64, f64)>,
    past: VecDeque<f64>,
}

impl Swing {
    fn new(rate: f64, initial: f64) -> Self {
        // `as`: a positive count of samples.
        let window = (SWING_WINDOW_SECONDS * rate).ceil().max(1.0) as usize;
        Self { window, seen: 0, range: None, past: VecDeque::from([initial.max(0.0)]) }
    }

    fn record(&mut self, fill: f64, samples: usize) {
        self.range = Some(match self.range {
            Some((lo, hi)) => (lo.min(fill), hi.max(fill)),
            None => (fill, fill),
        });
        self.seen += samples;
        if self.seen >= self.window {
            if let Some((lo, hi)) = self.range.take() {
                self.past.push_back(hi - lo);
            }
            while self.past.len() > SWING_WINDOWS {
                self.past.pop_front();
            }
            self.seen = 0;
        }
    }

    fn get(&self) -> f64 {
        let now = self.range.map_or(0.0, |(lo, hi)| hi - lo);
        self.past.iter().copied().fold(now, f64::max)
    }
}

pub struct Queue {
    rate: f64,
    resampler: Resampler,
    fifo: Vec<VecDeque<f64>>,
    scratch: Vec<Vec<f64>>,
    state: State,
    /// The fade's position, 0 to 1.
    gain: f64,
    smoothed: Option<f64>,
    integral: f64,
    correction: f64,
    swing: Swing,
    /// The target, in samples, as it rises and falls.
    goal: f64,
    pub underruns: u64,
    pub overruns: u64,
}

impl Queue {
    /// A queue for `channels` channels at `from` samples per second, taken from at `rate`.
    /// `swing` is the first estimate of how far the fill moves, in samples at `rate`.
    pub fn new(from: f64, rate: f64, channels: usize, swing: f64) -> Result<Self, String> {
        let resampler = Resampler::new(from, rate, channels)?;
        let mut q = Self {
            rate,
            resampler,
            fifo: vec![VecDeque::new(); channels],
            scratch: vec![Vec::new(); channels],
            state: State::Playing,
            gain: 0.0,
            smoothed: None,
            integral: 0.0,
            correction: 0.0,
            swing: Swing::new(rate, swing),
            goal: 0.0,
            underruns: 0,
            overruns: 0,
        };
        q.goal = q.wanted();
        let target = q.target();
        for c in &mut q.fifo {
            c.extend(std::iter::repeat_n(0.0, target));
        }
        Ok(q)
    }

    /// Samples the fill is steered to after each take.
    pub fn target(&self) -> usize {
        // `as`: a positive count of samples.
        self.goal.ceil().max(1.0) as usize
    }

    /// The target the swing asks for now.
    fn wanted(&self) -> f64 {
        (self.swing.get() + MARGIN_SECONDS * self.rate).min(MAX_TARGET_SECONDS * self.rate)
    }

    /// Moves the target after `n` samples: up at once, down slowly.
    fn follow(&mut self, n: usize) {
        let wanted = self.wanted();
        if wanted >= self.goal {
            self.goal = wanted;
        } else {
            let dt = n as f64 / self.rate;
            self.goal -= (self.goal - wanted) * (1.0 - (-dt / TARGET_FALL_SECONDS).exp());
        }
    }

    /// The delay this input gets: the target and the converter's own.
    pub fn latency_seconds(&self) -> f64 {
        self.target() as f64 / self.rate + self.resampler.delay_seconds()
    }

    /// Samples queued now.
    pub fn len(&self) -> usize {
        self.fifo.first().map_or(0, VecDeque::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The drift correction, ppm (positive: the input comes faster than the mix takes it).
    pub fn correction_ppm(&self) -> f64 {
        self.correction * 1e6
    }

    /// Converts a block and queues it. Returns the samples queued.
    pub fn push(&mut self, block: &[&[f64]]) -> Result<usize, String> {
        for s in &mut self.scratch {
            s.clear();
        }
        let n = self.resampler.process(block, &mut self.scratch)?;
        // `as`: a positive count of samples.
        let cap = (MAX_QUEUE_SECONDS * self.rate) as usize;
        for (f, s) in self.fifo.iter_mut().zip(&self.scratch) {
            f.extend(s.iter().copied());
            let excess = f.len().saturating_sub(cap);
            f.drain(..excess);
        }
        Ok(n)
    }

    /// The next `n` samples per channel; silence where there are none.
    pub fn take(&mut self, n: usize) -> Vec<Vec<f64>> {
        let mut out = vec![vec![0.0; n]; self.fifo.len()];
        let target = self.target();
        // `as`: a positive count of samples.
        let fade_len = (FADE_SECONDS * self.rate).ceil().max(1.0) as usize;
        let fade = 1.0 / fade_len as f64;
        if self.state == State::Waiting {
            let len = self.len();
            if len < n + target {
                return out;
            }
            // Silent so far: what is beyond the target goes before anything plays.
            let excess = len - n - target;
            for f in &mut self.fifo {
                f.drain(..excess);
            }
            self.state = State::Playing;
            self.gain = 0.0;
            self.smoothed = None;
        }
        let len = self.len();
        if len < n {
            // Too little: what is left fades out, then silence until the queue is full again.
            let tail = len.min(fade_len).max(1);
            for (f, o) in self.fifo.iter_mut().zip(out.iter_mut()) {
                for (k, (d, v)) in o.iter_mut().zip(f.drain(..)).enumerate() {
                    let left = (len - k) as f64 / tail as f64;
                    *d = v * smoothstep(self.gain.min(left));
                }
            }
            self.underruns += 1;
            self.state = State::Waiting;
            self.gain = 0.0;
            self.smoothed = None;
            self.swing.record(len as f64 - n as f64, n);
            self.follow(n);
            return out;
        }
        let after = len - n;
        // `as`: a count of samples.
        let high = target + target.max((0.05 * self.rate) as usize);
        let drop = if after > high { after - target } else { 0 };
        let cross = if drop > 0 { fade_len.min(n) } else { 0 };
        if drop > 0 {
            self.overruns += 1;
            self.smoothed = None;
        }
        // The fade-in's gains, shared by all channels.
        let ramp: Option<Vec<f64>> = (self.gain < 1.0).then(|| {
            let mut g = self.gain;
            (0..n)
                .map(|_| {
                    g = (g + fade).min(1.0);
                    smoothstep(g)
                })
                .collect()
        });
        for (f, o) in self.fifo.iter_mut().zip(out.iter_mut()) {
            for (d, v) in o.iter_mut().zip(f.range(drop..drop + n)) {
                *d = *v;
            }
            for (k, d) in o.iter_mut().enumerate().take(cross) {
                let w = (k as f64 + 0.5) / cross as f64;
                let old = f.get(k).copied().unwrap_or(0.0);
                *d = old + (*d - old) * smoothstep(w);
            }
            if let Some(r) = &ramp {
                for (d, g) in o.iter_mut().zip(r) {
                    *d *= g;
                }
            }
            f.drain(..drop + n);
        }
        if ramp.is_some() {
            self.gain = (self.gain + fade * n as f64).min(1.0);
        }
        let fill = (after - drop) as f64;
        self.swing.record(fill, n);
        self.follow(n);
        self.steer(fill, n, self.goal);
        out
    }

    /// The drift loop, once per take of `n`.
    fn steer(&mut self, fill: f64, n: usize, target: f64) {
        let dt = n as f64 / self.rate;
        let smoothed = match self.smoothed {
            Some(s) => s + (fill - s) * (1.0 - (-dt / SMOOTH_SECONDS).exp()),
            None => fill,
        };
        self.smoothed = Some(smoothed);
        let off = (smoothed - target) / self.rate;
        let error = off.signum() * (off.abs() - DEADBAND_SECONDS).max(0.0);
        self.integral = (self.integral + KI * error * dt).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.correction = (KP * error + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let nominal = self.resampler.nominal_ratio();
        self.resampler.set_ratio(nominal * (1.0 - self.correction));
    }
}

/// A gentle S curve for the fades.
fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n0: usize, n: usize, freq: f64, rate: f64) -> Vec<f64> {
        (n0..n0 + n).map(|i| 0.5 * (std::f64::consts::TAU * freq * i as f64 / rate).sin()).collect()
    }

    /// A 44.1 kHz source on the show clock (735 or 736 samples a frame) into a 48 kHz mix
    /// (800 a frame): no gap ever, and only a millisecond or two of delay.
    #[test]
    fn a_show_clock_source_at_another_rate_queues_a_millisecond() {
        let mut q = Queue::new(44_100.0, 48_000.0, 1, 2.0).unwrap();
        let mut sent = 0;
        for frame in 1..=3600_usize {
            let due = frame * 44_100 / 60;
            q.push(&[&sine(sent, due - sent, 1_000.0, 44_100.0)]).unwrap();
            sent = due;
            q.take(800);
        }
        println!("target {} samples, latency {:.2} ms", q.target(), q.latency_seconds() * 1000.0);
        assert_eq!((q.underruns, q.overruns), (0, 0));
        assert!(q.latency_seconds() < 0.0025, "{}", q.latency_seconds());
        assert!(q.correction_ppm().abs() < 20.0, "{}", q.correction_ppm());
    }

    /// A device's 10 ms buffers arriving with jitter: no gap after the start; the delay is
    /// about one buffer.
    #[test]
    fn device_jitter_is_absorbed() {
        for ppm in [-500.0, 0.0, 500.0] {
            let mut q = Queue::new(96_000.0, 48_000.0, 2, 960.0).unwrap();
            let device = 96_000.0 * (1.0 + ppm * 1e-6);
            let mut sent = 0;
            let mut rng = 99_u64;
            let mut latency = Vec::new();
            for frame in 1..=60 * 120_usize {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let jitter = (rng >> 40) as f64 / (1u64 << 24) as f64 * 0.006;
                let now = frame as f64 / 60.0 - jitter;
                let due = ((now * device / 960.0).floor().max(0.0) as usize) * 960;
                if due > sent {
                    let x = sine(sent, due - sent, 500.0, 96_000.0);
                    q.push(&[&x, &x]).unwrap();
                    sent = due;
                }
                q.take(800);
                if frame % (60 * 20) == 0 {
                    latency.push(format!("{:.1}", q.latency_seconds() * 1000.0));
                }
            }
            println!(
                "{ppm} ppm: latency every 20 s {latency:?} ms, correction {:.0} ppm",
                q.correction_ppm()
            );
            assert_eq!((q.underruns, q.overruns), (0, 0), "{ppm} ppm");
            assert!((q.correction_ppm() - ppm).abs() < 100.0, "{ppm}: {}", q.correction_ppm());
            assert!(q.latency_seconds() < 0.02, "{}", q.latency_seconds());
        }
    }

    /// A source that stops fades out, counts one gap, and fades in when it is back; a burst
    /// beyond the target is dropped with a crossfade and counted.
    #[test]
    fn a_gap_fades_and_a_burst_is_dropped() {
        let mut q = Queue::new(48_000.0, 48_000.0, 1, 2.0).unwrap();
        let mut sent = 0;
        let mut out = Vec::new();
        for frame in 0..200 {
            if !(100..110).contains(&frame) {
                q.push(&[&sine(sent, 800, 600.0, 48_000.0)]).unwrap();
                sent += 800;
            }
            out.extend(q.take(800).remove(0));
        }
        assert_eq!(q.underruns, 1);
        // No step anywhere: the fades.
        let step = std::f64::consts::TAU * 600.0 / 48_000.0 * 0.5;
        let worst = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        assert!(worst < step * 1.6, "{worst} vs {step}");
        assert!(out[150 * 800..].iter().any(|v| v.abs() > 0.49), "back");
        q.push(&[&vec![0.1; 48_000 / 5]]).unwrap();
        q.take(800);
        assert_eq!(q.overruns, 1);
        assert!(q.len() <= q.target() + 1, "{} vs {}", q.len(), q.target());
        // Never more than the most a queue holds.
        q.push(&[&vec![0.1; 48_000 * 3]]).unwrap();
        assert!(q.len() <= (MAX_QUEUE_SECONDS * 48_000.0) as usize);
        assert!(q.take(0).iter().all(Vec::is_empty));
        q.push(&[&[f64::NAN, f64::INFINITY, -1e300]]).unwrap();
        assert!(q.take(5000)[0].iter().all(|v| v.is_finite()));
    }
}
