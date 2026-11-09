//! Which tone is sounding: the pitch of a voice, a whistle or an instrument, block by block.
//!
//! **Method:** the McLeod pitch method. Every hop (10 ms) the newest window of sound is
//! compared with itself shifted by every lag up to the longest period in range: the normalised
//! square difference function (NSDF), 1 where the shifted sound matches exactly, 0 where it is
//! unrelated, -1 where it is inverted. A tone repeats every period, so the NSDF peaks there. Of
//! the peaks between its zero crossings, the first one within [`PICK`] of the highest is the
//! period. Its height is the **clarity**: about 1 for a clean tone, lower for a breathy one,
//! and lower still for chords and noise. A tone whose fundamental is weak or missing still
//! repeats at the fundamental's period, so it reads at the fundamental. A peak at twice the
//! period (an octave down) is as high as the first one, and the "first within" rule keeps the
//! octave right. The autocorrelation inside the NSDF goes through the FFT.
//!
//! **Range:** `min_hz` sets the window: two periods of the lowest tone, 40 ms at 50 Hz. Before
//! the analysis the sound passes a highpass at half of `min_hz` (rumble) and a lowpass at
//! `max_hz` (24 dB per octave), so the tones in range dominate what is measured: with a range
//! for the bass, a louder melody above it counts less. A tone above `max_hz` is still found
//! through the lowpass; it reads as heard but with clarity 0. A tone below `min_hz` has no
//! peak within the window and may read an octave or more higher.
//!
//! **Sure and held:** an estimate is sure when its clarity reaches the threshold, it lies in the
//! range, and the estimate a hop before was sure and within a semitone of it. A sure estimate
//! moves the held note, smoothed over `smooth` seconds in note numbers (linear in pitch).
//! Silence, noise and an unclear sound leave the held note where it was.
//!
//! **Sample rates:** above 48 kHz the filtered sound is decimated by a whole factor to 24 to 48
//! kHz, so a 96 kHz input costs as much as a 48 kHz one. The highest `max_hz` used is a fifth of
//! that rate (9.6 kHz at 48 kHz), so the lowpass also guards against aliasing.

use crate::fft::{Complex, Fft};

/// Lowest and highest frequency of the range, Hz.
pub const MIN_HZ: f64 = 20.0;
pub const MAX_HZ: f64 = 10_000.0;
/// The first NSDF peak at least this share of the highest one is the period.
pub const PICK: f64 = 0.9;
/// Seconds between two estimates.
pub const HOP_SECONDS: f64 = 0.01;
/// The highest analysis rate: faster input is decimated below it.
const MAX_WORK_RATE: f64 = 48_000.0;
/// The highest `max_hz` used, as a share of the analysis rate.
const TOP_SHARE: f64 = 0.2;
/// Semitones two estimates in a row may differ by and still back each other.
const AGREE: f64 = 1.0;
/// Clear estimates in a row, each within [`AGREE`] of the one before, before they count as
/// sure: a single clear hop in dense music is often a chance alignment.
const AGREE_HOPS: u32 = 3;
/// Highest sample rate accepted.
pub const MAX_RATE: f64 = 384_000.0;
/// Lowest sample rate accepted.
pub const MIN_RATE: f64 = 8_000.0;
/// Samples beyond this are clipped to it; non-finite ones count as 0.
const CLIP: f32 = 16.0;

/// What the tracker can be told, per call.
#[derive(Clone, Debug, PartialEq)]
pub struct PitchSettings {
    /// The lowest tone looked for, Hz. Sets the window: two of its periods.
    pub min_hz: f64,
    /// The highest tone looked for, Hz.
    pub max_hz: f64,
    /// 0 to 1: the clarity an estimate needs to move the held note.
    pub threshold: f64,
    /// Seconds the held note takes to get about two thirds of the way to a new tone; 0 jumps.
    pub smooth: f64,
    /// A window quieter than this (dBFS, after the filters) is silence.
    pub floor_db: f64,
}

impl Default for PitchSettings {
    fn default() -> Self {
        Self { min_hz: 50.0, max_hz: 4000.0, threshold: 0.8, smooth: 0.05, floor_db: -60.0 }
    }
}

/// What the tracker found at the end of a block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pitch {
    /// The held note in Hz: the last sure tone, smoothed. 0 until the first sure tone.
    pub frequency: f64,
    /// The held note as a MIDI note number with fraction (69 is A4 at 440 Hz). 0 until the
    /// first sure tone.
    pub note: f64,
    /// 0 to 1: how clearly the newest estimate repeats at its period. 0 in silence, without
    /// a period, and for a tone outside the range.
    pub clarity: f64,
    /// The newest estimate moved the held note.
    pub sure: bool,
    /// The newest estimate in Hz, sure or not, also outside the range; 0 in silence or without
    /// a period.
    pub heard_frequency: f64,
    /// The same as a note number; 0 when `heard_frequency` is.
    pub heard_note: f64,
}

/// The note number of `hz` in equal temperament, A4 (69) at 440 Hz; 0 for no frequency.
pub fn note_of(hz: f64) -> f64 {
    if hz.is_finite() && hz > 0.0 { 69.0 + 12.0 * (hz / 440.0).log2() } else { 0.0 }
}

/// The frequency of note number `n`, A4 (69) at 440 Hz.
pub fn hertz_of(n: f64) -> f64 {
    if n.is_finite() { 440.0 * ((n.clamp(-200.0, 300.0) - 69.0) / 12.0).exp2() } else { 0.0 }
}

/// A second-order filter section, transposed direct form II.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// Sets a lowpass (`high` false) or highpass at `hz` with quality `q`, keeping the state.
    fn set(&mut self, high: bool, hz: f64, q: f64, rate: f64) {
        let w = std::f64::consts::TAU * (hz / rate).clamp(1e-6, 0.49);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = if high {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        } else {
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
        };
        self.b = b.map(|v| v / a0);
        self.a = [-2.0 * cos / a0, (1.0 - alpha) / a0];
    }

    fn run(&mut self, x: f64) -> f64 {
        let [b0, b1, b2] = self.b;
        let [a1, a2] = self.a;
        let y = b0 * x + self.z[0];
        self.z[0] = b1 * x - a1 * y + self.z[1];
        self.z[1] = b2 * x - a2 * y;
        if !y.is_finite() {
            self.z = [0.0; 2];
            return 0.0;
        }
        y
    }
}

/// Everything a block changes: the filters, the newest samples and the held note. A copy taken
/// before a block lets the same block be analysed again from the same start.
#[derive(Clone, Debug)]
pub struct PitchState {
    /// The input's sample rate, so a snapshot of another tracker is refused.
    rate: f64,
    /// Highpass, then two lowpass sections.
    filters: [Biquad; 3],
    /// Input samples since the last one kept for the analysis.
    skip: usize,
    /// The newest analysis samples, a ring written at `pos`.
    ring: Vec<f32>,
    pos: usize,
    /// Analysis samples in the ring so far, up to its length.
    filled: usize,
    since_hop: usize,
    /// The note of the last hop's estimate, if it was clear and in range, and how many hops in
    /// a row before it were too, each within [`AGREE`] of the one before.
    prev: Option<(f64, u32)>,
    /// The held note, once a tone was sure.
    held: Option<f64>,
    out: Pitch,
}

/// Buffers for one estimate, kept between calls so a hop allocates nothing.
#[derive(Default)]
struct Work {
    fft: Option<Fft>,
    buf: Vec<Complex>,
    x: Vec<f32>,
    nsdf: Vec<f64>,
    keys: Vec<(f64, f64)>,
}

/// The pitch tracker for one signal at one sample rate.
pub struct PitchTracker {
    rate: f64,
    /// Input samples per analysis sample.
    decimate: usize,
    work_rate: f64,
    /// Analysis samples per hop.
    hop: usize,
    state: PitchState,
    work: Work,
}

impl PitchTracker {
    /// A tracker for a signal at `rate` Hz.
    pub fn new(rate: f64) -> Result<PitchTracker, String> {
        if !(MIN_RATE..=MAX_RATE).contains(&rate) {
            return Err(format!("a sample rate of {rate} Hz is not audio ({MIN_RATE} to {MAX_RATE} Hz)"));
        }
        // `as`: a small positive ratio, rounded up.
        let decimate = ((rate / MAX_WORK_RATE).ceil() as usize).max(1);
        let work_rate = rate / decimate as f64;
        let hop = ((work_rate * HOP_SECONDS).round() as usize).max(1);
        // Two periods of the lowest tone of any range, and the sample after the last lag.
        let ring = 2 * (work_rate / MIN_HZ).ceil() as usize + 4;
        Ok(PitchTracker {
            rate,
            decimate,
            work_rate,
            hop,
            state: PitchState {
                rate,
                filters: [Biquad::default(); 3],
                skip: 0,
                ring: vec![0.0; ring],
                pos: 0,
                filled: 0,
                since_hop: 0,
                prev: None,
                held: None,
                out: Pitch::default(),
            },
            work: Work::default(),
        })
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The rate the sound is analysed at, after decimation.
    pub fn work_rate(&self) -> f64 {
        self.work_rate
    }

    /// Input samples between two estimates.
    pub fn hop(&self) -> usize {
        self.hop * self.decimate
    }

    /// Input samples in the window for `settings`: two periods of `min_hz`.
    pub fn window(&self, settings: &PitchSettings) -> usize {
        2 * self.range(settings).2 * self.decimate
    }

    /// A copy of everything a block changes.
    pub fn snapshot(&self) -> PitchState {
        self.state.clone()
    }

    /// Goes back to a [`PitchTracker::snapshot`] of this tracker.
    pub fn restore(&mut self, state: &PitchState) {
        if state.rate == self.state.rate && state.ring.len() == self.state.ring.len() {
            self.state.clone_from(state);
        }
    }

    /// The range made sane: lowest and highest frequency used, and the longest lag in
    /// analysis samples.
    fn range(&self, s: &PitchSettings) -> (f64, f64, usize) {
        let top = TOP_SHARE * self.work_rate;
        let clean = |v: f64, default: f64| if v.is_finite() { v } else { default };
        let a = clean(s.min_hz, 50.0).clamp(MIN_HZ, MAX_HZ);
        let b = clean(s.max_hz, 4000.0).clamp(MIN_HZ, MAX_HZ);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let hi = hi.min(top).max(MIN_HZ * 1.5);
        let lo = lo.min(hi / 1.5);
        // `as`: the longest period in samples, rounded up.
        let lag = ((self.work_rate / lo).ceil() as usize).max(4);
        (lo, hi, lag)
    }

    /// Analyses the next block of samples and gives what is heard at its end.
    pub fn process(&mut self, samples: &[f32], s: &PitchSettings) -> Pitch {
        let (lo, hi, _) = self.range(s);
        let rate = self.rate;
        let [hp, lp1, lp2] = &mut self.state.filters;
        hp.set(true, lo / 2.0, std::f64::consts::FRAC_1_SQRT_2, rate);
        // A fourth-order Butterworth lowpass in two sections.
        lp1.set(false, hi, 0.541_196_1, rate);
        lp2.set(false, hi, 1.306_563, rate);
        for &x in samples {
            let x = if x.is_finite() { x.clamp(-CLIP, CLIP) } else { 0.0 };
            let [hp, lp1, lp2] = &mut self.state.filters;
            let y = lp2.run(lp1.run(hp.run(f64::from(x))));
            self.state.skip += 1;
            if self.state.skip < self.decimate {
                continue;
            }
            self.state.skip = 0;
            let len = self.state.ring.len().max(1);
            if let Some(slot) = self.state.ring.get_mut(self.state.pos) {
                // `as`: a sample to the ring's precision.
                *slot = y as f32;
            }
            self.state.pos = (self.state.pos + 1) % len;
            self.state.filled = (self.state.filled + 1).min(len);
            self.state.since_hop += 1;
            if self.state.since_hop >= self.hop {
                self.state.since_hop = 0;
                self.step(s);
            }
        }
        self.state.out
    }

    /// One estimate from the newest window, and the held note after it.
    fn step(&mut self, s: &PitchSettings) {
        let (lo, hi, lag) = self.range(s);
        let window = 2 * lag;
        let (heard, clarity) =
            if self.state.filled < window + 1 { (0.0, 0.0) } else { self.estimate(window, lag, s) };
        let in_range = heard > 0.0 && heard <= hi * 1.0001 && heard >= lo / 1.0001;
        let clarity = if in_range { clarity } else { 0.0 };
        let note = note_of(heard);
        let clear = in_range && clarity >= s.threshold.clamp(0.0, 1.0);
        let streak = match self.state.prev {
            Some((p, n)) if clear && (note - p).abs() < AGREE => n.saturating_add(1),
            _ => 1,
        };
        let sure = clear && streak >= AGREE_HOPS;
        self.state.prev = clear.then_some((note, streak));
        if sure {
            let smooth = if s.smooth.is_finite() { s.smooth.clamp(0.0, 60.0) } else { 0.0 };
            // `as`: samples to seconds.
            let dt = self.hop as f64 / self.work_rate;
            let alpha = if smooth <= 0.0 { 1.0 } else { 1.0 - (-dt / smooth).exp() };
            self.state.held = Some(match self.state.held {
                Some(h) => h + alpha * (note - h),
                None => note,
            });
        }
        let held = self.state.held.unwrap_or(0.0);
        self.state.out = Pitch {
            frequency: if self.state.held.is_some() { hertz_of(held) } else { 0.0 },
            note: held,
            clarity,
            sure,
            heard_frequency: heard,
            heard_note: note,
        };
    }

    /// The frequency and clarity of the newest `window` analysis samples, from lags up to
    /// `lag`; (0, 0) in silence or without a period.
    fn estimate(&mut self, window: usize, lag: usize, s: &PitchSettings) -> (f64, f64) {
        let w = &mut self.work;
        // Oldest first.
        let ring = &self.state.ring;
        let len = ring.len();
        let start = (self.state.pos + len - window.min(len)) % len.max(1);
        w.x.clear();
        w.x.extend(ring.iter().cycle().skip(start).take(window));
        let energy: f64 = w.x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
        // `as`: a count to a divisor.
        let rms = (energy / window.max(1) as f64).sqrt();
        let floor = if s.floor_db.is_finite() { s.floor_db.clamp(-200.0, 0.0) } else { -60.0 };
        if !(rms.is_finite() && rms > 10f64.powf(floor / 20.0)) {
            return (0.0, 0.0);
        }
        // A linear correlation up to `lag + 1` needs that much room after the window.
        let size = (window + lag + 2).next_power_of_two();
        if w.fft.as_ref().is_none_or(|f| f.size() != size) {
            match Fft::new(size) {
                Ok(f) => w.fft = Some(f),
                Err(_) => return (0.0, 0.0),
            }
        }
        let Some(fft) = &w.fft else { return (0.0, 0.0) };
        w.buf.clear();
        w.buf.extend(w.x.iter().map(|&re| Complex { re, im: 0.0 }));
        w.buf.resize(size, Complex::default());
        fft.forward(&mut w.buf);
        for c in &mut w.buf {
            *c = Complex { re: c.re * c.re + c.im * c.im, im: 0.0 };
        }
        // The transform of a real, even spectrum is the inverse transform times the size.
        fft.forward(&mut w.buf);
        // `as`: a size to a divisor.
        let scale = 1.0 / size as f64;
        // NSDF: 2 r(t) / m(t), where m(t) sums the squares of both overlapping parts.
        w.nsdf.clear();
        let mut m = 2.0 * energy;
        let top = lag + 1;
        for t in 0..=top {
            if t > 0 {
                let a = w.x.get(t - 1).map_or(0.0, |v| f64::from(*v));
                let b = w.x.get(window - t).map_or(0.0, |v| f64::from(*v));
                m -= a * a + b * b;
            }
            let r = w.buf.get(t).map_or(0.0, |c| f64::from(c.re) * scale);
            w.nsdf.push(if m > energy * 1e-9 { (2.0 * r / m).clamp(-1.0, 1.0) } else { 0.0 });
        }
        // Key maxima: the highest point of each positive lobe after the first zero crossing.
        w.keys.clear();
        let mut t = 1;
        while w.nsdf.get(t).is_some_and(|v| *v > 0.0) {
            t += 1;
        }
        let mut lobe: Option<usize> = None;
        while t < top {
            let v = w.nsdf.get(t).copied().unwrap_or(0.0);
            if v > 0.0 {
                let best = lobe.and_then(|i| w.nsdf.get(i).copied()).unwrap_or(f64::MIN);
                if v > best {
                    lobe = Some(t);
                }
            } else if let Some(i) = lobe.take() {
                w.keys.push(refine(&w.nsdf, i));
            }
            t += 1;
        }
        // A lobe still open at the last lag counts if its top lies inside.
        if let Some(i) = lobe
            && i + 1 < top
        {
            w.keys.push(refine(&w.nsdf, i));
        }
        let highest = w.keys.iter().map(|k| k.1).fold(0.0, f64::max);
        let Some(&(period, value)) = w.keys.iter().find(|k| k.1 >= PICK * highest) else {
            return (0.0, 0.0);
        };
        if !(period.is_finite() && period > 0.0) {
            return (0.0, 0.0);
        }
        (self.work_rate / period, value.clamp(0.0, 1.0))
    }
}

/// The lag and height of the NSDF peak at `i`, refined by a parabola through its neighbours.
fn refine(nsdf: &[f64], i: usize) -> (f64, f64) {
    let at = |j: usize| nsdf.get(j).copied().unwrap_or(0.0);
    let (a, b, c) = (at(i.saturating_sub(1)), at(i), at(i + 1));
    let d = a - 2.0 * b + c;
    // `as`: a lag to a position.
    let i = i as f64;
    if d.abs() < 1e-12 || !d.is_finite() {
        return (i, b);
    }
    let x = (0.5 * (a - c) / d).clamp(-0.5, 0.5);
    (i + x, b - 0.25 * (a - c) * x)
}
