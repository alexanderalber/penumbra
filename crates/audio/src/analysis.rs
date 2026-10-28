//! Analysis of a live audio signal, block by block: levels, three bands, a log-spaced spectrum,
//! onsets, tempo, and beat and bar phase.
//!
//! The signal is cut into frames of about 43 ms that overlap by three quarters (a hop of about
//! 10.7 ms at any sample rate: 512 samples at 44.1 and 48 kHz, 1024 at 96 kHz), each windowed
//! and transformed. From each frame come the band levels, the spectrum, and an **onset
//! strength**: the spectral flux of log-compressed magnitudes, how much louder the spectrum got
//! since the last frame. Its peaks above an adaptive threshold are the onsets (and in the low
//! band, the kicks).
//!
//! **Tempo:** every 0.2 s the onset strength of the last eight seconds (local mean removed,
//! negative parts dropped) is autocorrelated; every period in the allowed range scores its
//! autocorrelation plus that of its multiples up to four, so the beat period wins over a bar or
//! a half beat. The period is refined from the fourth multiple's peak, which gives about 0.1
//! BPM at 120. A new tempo replaces the current one only after it has won for 1.2 s; close
//! estimates are blended in.
//!
//! **Beat phase:** every hop, a comb with the beat period is laid over the recent onset
//! strength at every offset; the best offset says where the last beat was. An oscillator
//! running at the tempo is pulled towards it a little each hop (more when the estimate is
//! confident), so the phase moves smoothly and keeps running through a breakdown with no beat
//! (the confidence drops, the tempo stays). The phase is given at the newest sample: the delay
//! of the frame (half its length) is taken out.
//!
//! **Bar phase:** beats are counted. Bass lines and chords tend to change on the downbeat, so
//! the spectrum below 1 kHz is averaged over each beat and compared with the beat before; that
//! change is averaged per position in the bar, and the position with the most change is the
//! downbeat (a new one must be 15 % ahead to take over). A guess where nothing changes per bar.

use std::collections::VecDeque;

use crate::fft::{Complex, Fft};

/// Lowest and highest sample rates analysed.
pub const MIN_RATE: f64 = 8000.0;
pub const MAX_RATE: f64 = 384_000.0;
/// Seconds of onset strength the tempo is estimated from.
const TEMPO_SECONDS: f64 = 8.0;
/// Seconds between two tempo estimates.
const TEMPO_EVERY: f64 = 0.2;
/// Estimates a different tempo must win in a row before it replaces the current one.
const TEMPO_SWITCH: u32 = 6;
/// Relative difference under which a tempo estimate counts as the same tempo.
const TEMPO_SAME: f64 = 0.04;
/// Periods laid over the onset strength to find the beat.
const COMB_BEATS: usize = 6;
/// Lowest spectrum frequency, Hz.
pub const SPECTRUM_LOW: f64 = 30.0;
/// Highest spectrum frequency, Hz (or the Nyquist frequency, if lower).
pub const SPECTRUM_HIGH: f64 = 16_000.0;
/// The span of the spectrum's values: 0 is this far below the reference.
const SPECTRUM_DB: f64 = 60.0;
/// How fast the normalising peak falls, dB per second.
const FALL_DB_PER_S: f64 = 1.5;
/// Equivalent noise bandwidth of the Hann window, in bins.
const HANN_ENBW: f32 = 1.5;
/// Largest number of spectrum bands.
pub const MAX_BANDS: usize = 512;

/// What the analysis can be told, per call.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Upper edge of the low band, Hz.
    pub low_hz: f64,
    /// Lower edge of the high band, Hz.
    pub high_hz: f64,
    /// 0 to 1: how readily an onset counts.
    pub sensitivity: f64,
    pub min_bpm: f64,
    pub max_bpm: f64,
    pub beats_per_bar: u32,
    /// Spectrum bands, log-spaced from 30 Hz to 16 kHz.
    pub bands: usize,
    /// Divide levels and spectrum by the loudest of the last seconds, so a quiet input still
    /// spans 0 to 1.
    pub normalize: bool,
    /// Levels below this (dBFS) are not raised by `normalize`: silence stays silence.
    pub floor_db: f64,
    /// Seconds the beat and bar phase are moved ahead (positive: earlier), to make up for
    /// what comes after the analysis: the projector, the graph.
    pub offset: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            low_hz: 150.0,
            high_hz: 2500.0,
            sensitivity: 0.5,
            min_bpm: 70.0,
            max_bpm: 180.0,
            beats_per_bar: 4,
            bands: 32,
            normalize: true,
            floor_db: -60.0,
            offset: 0.0,
        }
    }
}

/// What one call found (levels over the block, events within it, phases at its end).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Features {
    /// RMS of the block, linear (full scale is 1).
    pub rms: f64,
    /// Largest absolute sample of the block.
    pub peak: f64,
    /// RMS, normalised when asked.
    pub level: f64,
    pub low: f64,
    pub mid: f64,
    pub high: f64,
    /// An onset happened in the block.
    pub onset: bool,
    /// An onset in the low band happened in the block.
    pub kick: bool,
    /// 0 until a tempo was found.
    pub bpm: f64,
    /// 0 to 1: how clearly the onsets repeat at that tempo.
    pub confidence: f64,
    /// 0 at a beat, rising to 1 at the next.
    pub beat: f64,
    /// A beat started in the block.
    pub tick: bool,
    /// 0 at a downbeat, rising to 1 at the next.
    pub bar: f64,
    /// The beat within the bar, from 0.
    pub bar_beat: u32,
    /// Beats counted since the tempo was found.
    pub beats: u64,
    /// Spectrum bands, low to high, 0 to 1 (dB over [`SPECTRUM_DB`]).
    pub spectrum: Vec<f64>,
}

/// The last values of a series, newest last.
#[derive(Clone, Debug)]
struct History {
    values: VecDeque<f32>,
    cap: usize,
}

impl History {
    fn new(cap: usize) -> Self {
        Self { values: VecDeque::with_capacity(cap.max(1)), cap: cap.max(1) }
    }

    fn push(&mut self, v: f32) {
        if self.values.len() == self.cap {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    /// The value `back` steps before the newest (0 is the newest).
    fn back(&self, back: usize) -> Option<f32> {
        self.values.len().checked_sub(back + 1).and_then(|i| self.values.get(i).copied())
    }

    /// Like [`History::back`] at a fractional position, linearly interpolated.
    fn back_at(&self, back: f64) -> Option<f32> {
        if back.is_nan() || back < 0.0 {
            return None;
        }
        // `as`: a non-negative position, floored.
        let i = back.floor() as usize;
        let t = (back - back.floor()) as f32;
        let a = self.back(i)?;
        let b = self.back(i + 1).unwrap_or(a);
        Some(a + (b - a) * t)
    }

    fn mean(&self) -> f32 {
        if self.values.is_empty() {
            return 0.0;
        }
        // `as`: a count to a divisor.
        self.values.iter().sum::<f32>() / self.values.len() as f32
    }
}

/// Picks peaks from an onset strength series.
#[derive(Clone, Debug)]
struct Picker {
    /// Recent values for the adaptive threshold.
    recent: History,
    prev2: f32,
    prev1: f32,
    /// Hops since the last onset.
    since: usize,
    /// The largest values lately, falling slowly.
    top: f32,
}

impl Picker {
    fn new(window: usize) -> Self {
        Self { recent: History::new(window), prev2: 0.0, prev1: 0.0, since: usize::MAX / 2, top: 0.0 }
    }

    /// Takes the next value; whether the one before it was an onset (one hop late, since a
    /// peak is only known once the series falls again).
    fn step(&mut self, x: f32, sensitivity: f64, min_gap: usize, fall: f32) -> bool {
        let s = sensitivity.clamp(0.0, 1.0) as f32;
        let lambda = 3.0 - 1.8 * s;
        let delta = 0.02 + 0.13 * (1.0 - s);
        self.top = (self.top * fall).max(x);
        let threshold = self.recent.mean() * lambda + delta * self.top;
        let candidate = self.prev1;
        let onset = candidate > self.prev2
            && candidate >= x
            && candidate > threshold
            && candidate > 1e-6
            && self.since >= min_gap;
        self.since = if onset { 0 } else { self.since.saturating_add(1) };
        self.recent.push(candidate);
        self.prev2 = self.prev1;
        self.prev1 = x;
        onset
    }
}

/// A peak follower for normalising: the loudest lately, falling slowly.
#[derive(Clone, Copy, Debug, Default)]
struct Follower(f64);

impl Follower {
    fn update(&mut self, x: f64, seconds: f64) -> f64 {
        let fall = 10f64.powf(-FALL_DB_PER_S * seconds.max(0.0) / 20.0);
        self.0 = (self.0 * fall).max(x);
        self.0
    }

    /// `x` over the follower (not below `floor`), clamped to 0..1.
    fn scale(self, x: f64, floor: f64) -> f64 {
        let reference = self.0.max(floor);
        if reference > 0.0 { (x / reference).clamp(0.0, 1.0) } else { 0.0 }
    }
}

/// The tempo estimate.
#[derive(Clone, Debug, Default)]
struct Tempo {
    /// Beat period in hops, 0 while unknown.
    period: f64,
    /// A different period that keeps winning, and how often it has.
    pending: Option<(f64, u32)>,
    confidence: f64,
}

/// The analysis state for one signal at one sample rate.
pub struct Analyzer {
    rate: f64,
    hop: usize,
    fft: Fft,
    window: Vec<f32>,
    /// The last frame's samples, a ring written at `pos`.
    ring: Vec<f32>,
    pos: usize,
    since_hop: usize,
    scratch: Vec<Complex>,
    magnitudes: Vec<f32>,
    prev_log: Vec<f32>,
    /// Onset strength, broadband and low band.
    flux: History,
    flux_low: History,
    /// Broadband onset strength with its local mean removed, negative parts dropped.
    odf: History,
    pick: Picker,
    pick_low: Picker,
    hops: u64,
    tempo: Tempo,
    /// Beats as a running count; its fraction is the phase.
    phase: f64,
    locked: bool,
    /// Hops in a row the measured phase was far from the oscillator's.
    off: u32,
    /// The beat count last reported, so a beat ticks once.
    reported: Option<i64>,
    /// Per position in the bar, the onset strength around its beats lately.
    bar_strength: Vec<f64>,
    downbeat: usize,
    /// Bins below 1 kHz (from 40 Hz) whose change marks a downbeat.
    harmonic: (usize, usize),
    /// The beat being averaged, its summed log spectrum and frames.
    beat_spectrum: Option<(i64, Vec<f32>, u32)>,
    /// The previous beat's mean log spectrum.
    last_spectrum: Option<Vec<f32>>,
    followers: [Follower; 4],
    spectrum_follower: Follower,
    /// The band and spectrum amplitudes of the last block that completed a frame, for blocks
    /// too short to complete one.
    last_amps: ([f64; 3], Vec<f64>),
    /// Spectrum band edges in bins for the `(bands, size)` they were made for.
    edges: Option<(usize, Vec<(usize, usize)>)>,
}

impl Analyzer {
    /// An analysis for a signal at `rate` Hz.
    pub fn new(rate: f64) -> Result<Analyzer, String> {
        if !(MIN_RATE..=MAX_RATE).contains(&rate) {
            return Err(format!(
                "a sample rate of {rate} Hz is not audio ({MIN_RATE} to {MAX_RATE} Hz): connect an audio.device_in"
            ));
        }
        // `as`: rate over ~94 hops per second, rounded to a power of two (64 to 8192).
        let hop = 1usize << ((rate / 93.75).log2().round().clamp(6.0, 13.0) as u32);
        let size = hop * 4;
        let fft = Fft::new(size)?;
        let window = (0..size)
            .map(|i| {
                let x = std::f64::consts::TAU * i as f64 / size as f64;
                (0.5 - 0.5 * x.cos()) as f32
            })
            .collect();
        let fps = rate / hop as f64;
        let tempo_len = (TEMPO_SECONDS * fps).ceil() as usize;
        let picker_len = (0.3 * fps).ceil() as usize;
        Ok(Analyzer {
            rate,
            hop,
            fft,
            window,
            ring: vec![0.0; size],
            pos: 0,
            since_hop: 0,
            scratch: vec![Complex::default(); size],
            magnitudes: vec![0.0; size / 2 + 1],
            prev_log: vec![0.0; size / 2 + 1],
            flux: History::new((0.25 * fps).ceil() as usize),
            flux_low: History::new(4),
            odf: History::new(tempo_len),
            pick: Picker::new(picker_len),
            pick_low: Picker::new(picker_len),
            hops: 0,
            tempo: Tempo::default(),
            phase: 0.0,
            locked: false,
            off: 0,
            reported: None,
            bar_strength: Vec::new(),
            downbeat: 0,
            harmonic: (
                // `as`: frequencies to bins.
                ((40.0 / (rate / size as f64)).round() as usize).max(1),
                ((1000.0 / (rate / size as f64)).round() as usize).min(size / 2),
            ),
            beat_spectrum: None,
            last_spectrum: None,
            followers: [Follower::default(); 4],
            spectrum_follower: Follower::default(),
            last_amps: ([0.0; 3], Vec::new()),
            edges: None,
        })
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Samples per hop.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Hops per second.
    fn fps(&self) -> f64 {
        self.rate / self.hop as f64
    }

    fn size(&self) -> usize {
        self.ring.len()
    }

    fn bin_hz(&self) -> f64 {
        self.rate / self.size() as f64
    }

    /// Analyses the next block of samples (non-finite ones count as 0).
    pub fn process(&mut self, samples: &[f32], s: &Settings) -> Features {
        let mut f = Features::default();
        let mut sum_sq = 0.0f64;
        let mut peak = 0.0f32;
        let mut bands_sq = [0.0f64; 3];
        let mut frames = 0usize;
        let bands = s.bands.clamp(1, MAX_BANDS);
        let mut spectrum_sq = vec![0.0f64; bands];
        for &x in samples {
            let x = if x.is_finite() { x.clamp(-16.0, 16.0) } else { 0.0 };
            sum_sq += f64::from(x) * f64::from(x);
            peak = peak.max(x.abs());
            if let Some(slot) = self.ring.get_mut(self.pos) {
                *slot = x;
            }
            self.pos = (self.pos + 1) % self.ring.len().max(1);
            self.since_hop += 1;
            if self.since_hop >= self.hop {
                self.since_hop = 0;
                let frame = self.frame(s, bands);
                frames += 1;
                for (a, b) in bands_sq.iter_mut().zip(frame.bands) {
                    *a += b;
                }
                for (a, b) in spectrum_sq.iter_mut().zip(&frame.spectrum) {
                    *a += b;
                }
                f.onset |= frame.onset;
                f.kick |= frame.kick;
            }
        }
        let seconds = samples.len() as f64 / self.rate;
        let n = samples.len().max(1) as f64;
        f.rms = (sum_sq / n).sqrt();
        f.peak = f64::from(peak);
        let floor = 10f64.powf(s.floor_db.clamp(-200.0, 0.0) / 20.0);
        let per_frame = |v: f64| if frames > 0 { (v / frames as f64).sqrt() } else { 0.0 };
        if frames > 0 {
            self.last_amps = (bands_sq.map(per_frame), spectrum_sq.iter().map(|&v| per_frame(v)).collect());
        }
        let ([low, mid, high], amps) = &self.last_amps;
        let raw = [f.rms, *low, *mid, *high];
        let mut amps = amps.clone();
        amps.resize(bands, 0.0);
        let mut out = [0.0; 4];
        for ((o, r), fl) in out.iter_mut().zip(raw).zip(self.followers.iter_mut()) {
            fl.update(r, seconds);
            *o = if s.normalize { fl.scale(r, floor) } else { r };
        }
        [f.level, f.low, f.mid, f.high] = out;
        let loudest = amps.iter().copied().fold(0.0, f64::max);
        let reference =
            if s.normalize { self.spectrum_follower.update(loudest, seconds).max(floor) } else { 1.0 };
        f.spectrum = amps
            .iter()
            .map(|&a| {
                if a <= 0.0 || reference <= 0.0 {
                    return 0.0;
                }
                (1.0 + 20.0 * (a / reference).log10() / SPECTRUM_DB).clamp(0.0, 1.0)
            })
            .collect();
        self.describe(&mut f, s);
        f
    }

    /// The phases and tempo as they stand.
    fn describe(&mut self, f: &mut Features, s: &Settings) {
        f.confidence = self.tempo.confidence;
        if self.tempo.period > 0.0 && self.locked {
            f.bpm = 60.0 * self.fps() / self.tempo.period;
            // The oscillator stands at the newest frame's centre; the newest sample is half a
            // frame later.
            let ahead = (self.size() / 2) as f64 / self.hop as f64 / self.tempo.period;
            let offset = if s.offset.is_finite() { s.offset.clamp(-10.0, 10.0) } else { 0.0 };
            let p = self.phase + ahead + offset * f.bpm / 60.0;
            f.beat = p.rem_euclid(1.0);
            let beats_per_bar = s.beats_per_bar.clamp(1, 64);
            let index = p.floor() as i64;
            let position = (index - self.downbeat as i64).rem_euclid(i64::from(beats_per_bar));
            // `as`: within 0..beats_per_bar.
            f.bar_beat = position as u32;
            f.bar = (position as f64 + f.beat) / f64::from(beats_per_bar);
            f.beats = index.max(0) as u64;
            f.tick = self.reported.is_some_and(|r| index > r);
            if self.reported.is_none_or(|r| index > r) {
                self.reported = Some(index);
            }
        }
    }

    fn frame(&mut self, s: &Settings, bands: usize) -> Frame {
        let size = self.size();
        for ((c, x), w) in
            self.scratch.iter_mut().zip(self.ring.iter().cycle().skip(self.pos).take(size)).zip(&self.window)
        {
            *c = Complex { re: x * w, im: 0.0 };
        }
        self.fft.forward(&mut self.scratch);
        // Scaled so a full-scale sine reads about 1 in its bin: the window sums to size / 2.
        let scale = 4.0 / size as f32;
        for (m, c) in self.magnitudes.iter_mut().zip(&self.scratch) {
            *m = c.norm() * scale;
        }
        let hz = self.bin_hz();
        let bin = |f: f64| ((f / hz).round().max(0.0) as usize).min(size / 2);
        let (low_end, high_start) = (bin(s.low_hz.max(1.0)), bin(s.high_hz.max(s.low_hz.max(1.0))));
        let first = bin(20.0).max(1);
        let last = bin(SPECTRUM_HIGH.min(self.rate / 2.0));
        let mut out = Frame { spectrum: vec![0.0; bands], ..Frame::default() };
        let mut flux = 0.0f32;
        let mut flux_low = 0.0f32;
        // For the beat: each bin's rise over its index, so every octave weighs the same and
        // the many bins of a hi-hat do not outweigh the few of a kick.
        let mut flux_beat = 0.0f32;
        let beat_first = bin(40.0).max(1);
        for (k, (m, prev)) in self.magnitudes.iter().zip(self.prev_log.iter_mut()).enumerate() {
            let power = f64::from(m * m / HANN_ENBW) / 2.0;
            if (first..=last).contains(&k) {
                let band = if k < low_end {
                    0
                } else if k < high_start {
                    1
                } else {
                    2
                };
                if let Some(b) = out.bands.get_mut(band) {
                    *b += power;
                }
            }
            let log = (1.0 + 1000.0 * m).ln();
            let rise = (log - *prev).max(0.0);
            *prev = log;
            if (first..=last).contains(&k) {
                flux += rise;
                if k < low_end {
                    flux_low += rise;
                }
                if k >= beat_first {
                    // `as`: a bin index to a weight.
                    flux_beat += rise * beat_first as f32 / k as f32;
                }
            }
        }
        self.spectrum_bands(bands, &mut out.spectrum);
        self.hops += 1;
        let fps = self.fps();
        let fall = 10f32.powf(-0.5 / fps as f32);
        // `as`: hops for 50 ms (onsets) and 100 ms (kicks).
        out.onset = self.pick.step(flux, s.sensitivity, (0.05 * fps).ceil() as usize, fall);
        out.kick = self.pick_low.step(flux_low, s.sensitivity, (0.1 * fps).ceil() as usize, fall);
        self.flux_low.push(flux_low);
        let mean = self.flux.mean();
        self.flux.push(flux_beat);
        let odf = (flux_beat - mean).max(0.0);
        self.odf.push(odf);
        // `as`: hops between two tempo estimates.
        let every = ((TEMPO_EVERY * fps).round() as u64).max(1);
        if self.hops.is_multiple_of(every) {
            self.estimate_tempo(s);
        }
        self.advance(s);
        out
    }

    /// Mean power per spectrum band from the current magnitudes.
    fn spectrum_bands(&mut self, bands: usize, out: &mut [f64]) {
        let size = self.size();
        if self.edges.as_ref().is_none_or(|(b, e)| *b != bands || e.len() != bands) {
            let hz = self.bin_hz();
            let high = SPECTRUM_HIGH.min(self.rate / 2.0);
            let ratio = (high / SPECTRUM_LOW).max(1.0);
            let edge = |i: usize| SPECTRUM_LOW * ratio.powf(i as f64 / bands as f64);
            let edges = (0..bands)
                .map(|i| {
                    let (a, b) = (edge(i) / hz, edge(i + 1) / hz);
                    let lo = (a.ceil() as usize).min(size / 2);
                    let hi = (b.floor() as usize).min(size / 2);
                    if hi >= lo {
                        (lo, hi)
                    } else {
                        // Narrower than a bin: the bin nearest its middle.
                        let mid = (((a + b) / 2.0).round() as usize).min(size / 2);
                        (mid, mid)
                    }
                })
                .collect();
            self.edges = Some((bands, edges));
        }
        let Some((_, edges)) = &self.edges else { return };
        for (o, &(lo, hi)) in out.iter_mut().zip(edges) {
            let bins = self.magnitudes.get(lo..=hi).unwrap_or(&[]);
            let sum: f64 = bins.iter().map(|m| f64::from(m * m / HANN_ENBW) / 2.0).sum();
            *o = sum;
        }
    }

    /// Autocorrelates the onset strength and updates the tempo.
    fn estimate_tempo(&mut self, s: &Settings) {
        let fps = self.fps();
        let (min_bpm, max_bpm) = {
            let a = s.min_bpm.clamp(20.0, 400.0);
            let b = s.max_bpm.clamp(20.0, 400.0);
            if a <= b { (a, b) } else { (b, a) }
        };
        // `as`: lags in hops.
        let lag_min = ((60.0 * fps / max_bpm).floor() as usize).max(2);
        let lag_max = ((60.0 * fps / min_bpm).ceil() as usize).max(lag_min);
        let n = self.odf.len();
        let reach = 4 * lag_max + 4;
        if n < reach.max(2 * lag_max + 2) || n < (3.0 * fps) as usize {
            return;
        }
        let x: Vec<f32> = self.odf.values.iter().copied().collect();
        let mean = x.iter().sum::<f32>() / n as f32;
        let x: Vec<f32> = x.iter().map(|v| v - mean).collect();
        let top = reach.min(n - 1);
        let ac: Vec<f64> = (0..=top)
            .map(|lag| {
                let s: f64 = x.iter().zip(x.iter().skip(lag)).map(|(a, b)| f64::from(a * b)).sum();
                s / (n - lag) as f64
            })
            .collect();
        let ac0 = ac.first().copied().unwrap_or(0.0);
        if ac0.is_nan() || ac0 <= 1e-12 {
            self.tempo.confidence *= 0.8;
            return;
        }
        let at = |i: usize| ac.get(i).copied().unwrap_or(0.0);
        // The best value near `center`, within `reach` either side.
        let best_near = |center: usize, reach: usize| -> (usize, f64) {
            let lo = center.saturating_sub(reach);
            (lo..=center + reach)
                .map(|i| (i, at(i)))
                .fold((center, f64::MIN), |b, c| if c.1 > b.1 { c } else { b })
        };
        let mut best = (0usize, f64::MIN);
        for lag in lag_min..=lag_max {
            let mut score = 0.0;
            for m in 1..=4usize {
                score += best_near(m * lag, m - 1).1;
            }
            // Off-beats (hi-hats, a bass between the kicks) speak for the period whose half
            // they fall on; a period of one and a half beats has nothing at its half.
            score += 0.5 * best_near(lag / 2, 1).1.max(0.0);
            let bpm = 60.0 * fps / lag as f64;
            // A preference for tempos near 120, to break ties between related periods.
            let prior = (-0.5 * (bpm / 120.0).log2().powi(2)).exp();
            let score = score * prior;
            if score > best.1 {
                best = (lag, score);
            }
        }
        let (lag, score) = best;
        if lag == 0 {
            return;
        }
        // Refine from the fourth multiple's peak, interpolated.
        let (peak4, _) = best_near(4 * lag, 3);
        let refined = parabola(at(peak4.saturating_sub(1)), at(peak4), at(peak4 + 1))
            .map(|d| (peak4 as f64 + d) / 4.0)
            .filter(|p| (p / lag as f64 - 1.0).abs() < 0.1)
            .unwrap_or(lag as f64);
        let confidence = ((score / 4.0 / ac0 - 0.05) / 0.35).clamp(0.0, 1.0);
        let t = &mut self.tempo;
        t.confidence = 0.7 * t.confidence + 0.3 * confidence;
        if confidence < 0.1 {
            return;
        }
        if t.period <= 0.0 {
            t.period = refined;
            t.pending = None;
        } else if (refined / t.period - 1.0).abs() < TEMPO_SAME {
            t.period += 0.2 * (refined - t.period);
            t.pending = None;
        } else {
            let count = match t.pending {
                Some((p, c)) if (refined / p - 1.0).abs() < TEMPO_SAME => c + 1,
                _ => 1,
            };
            t.pending = Some((refined, count));
            if count >= TEMPO_SWITCH {
                t.period = refined;
                t.pending = None;
                self.locked = false;
            }
        }
    }

    /// Moves the beat oscillator by one hop and pulls it towards the measured phase.
    fn advance(&mut self, s: &Settings) {
        let period = self.tempo.period;
        if period <= 0.0 {
            return;
        }
        self.phase += 1.0 / period;
        if let Some(measured) = self.measure_phase(period) {
            let current = self.phase.rem_euclid(1.0);
            let err = (measured - current + 0.5).rem_euclid(1.0) - 0.5;
            if !self.locked {
                self.phase += err;
                self.locked = true;
                self.off = 0;
            } else {
                self.off = if err.abs() > 0.2 { self.off + 1 } else { 0 };
                // Far off for a second: the music moved; take the measurement.
                if f64::from(self.off) > self.fps() {
                    self.phase += err;
                    self.off = 0;
                } else {
                    // A time constant of about a second at full confidence, so a beat the
                    // comb misplaces now and then hardly moves the phase.
                    let gain = 0.003 + 0.012 * self.tempo.confidence;
                    self.phase += gain * err;
                }
            }
        }
        // The spectrum per beat, for the downbeat.
        let index = self.phase.floor() as i64;
        let (lo, hi) = self.harmonic;
        let bins = self.prev_log.get(lo..=hi).unwrap_or(&[]).to_vec();
        match &mut self.beat_spectrum {
            Some((i, sum, frames)) if *i == index => {
                for (a, b) in sum.iter_mut().zip(&bins) {
                    *a += b;
                }
                *frames += 1;
            }
            current => {
                if let Some((i, sum, frames)) = current.take() {
                    self.finish_beat(i, sum, frames, s.beats_per_bar);
                }
                self.beat_spectrum = Some((index, bins, 1));
            }
        }
    }

    /// The fraction of a beat since the last beat at the newest frame, from a comb over the
    /// onset strength; `None` while there is too little of it or no onsets.
    fn measure_phase(&self, period: f64) -> Option<f64> {
        let needed = (period * COMB_BEATS as f64).ceil() as usize + 2;
        if self.odf.len() < needed {
            return None;
        }
        let steps = period.ceil() as usize;
        let mut best = (0usize, f64::MIN);
        let mut scores = Vec::with_capacity(steps);
        for offset in 0..steps {
            let mut score = 0.0;
            let mut weight = 1.0;
            for k in 0..COMB_BEATS {
                score +=
                    weight * f64::from(self.odf.back_at(offset as f64 + k as f64 * period).unwrap_or(0.0));
                weight *= 0.85;
            }
            scores.push(score);
            if score > best.1 {
                best = (offset, score);
            }
        }
        if best.1.is_nan() || best.1 <= 1e-9 {
            return None;
        }
        let (i, _) = best;
        let get = |j: isize| -> f64 {
            let j = j.rem_euclid(steps as isize) as usize;
            scores.get(j).copied().unwrap_or(0.0)
        };
        let d = parabola(get(i as isize - 1), get(i as isize), get(i as isize + 1)).unwrap_or(0.0);
        Some(((i as f64 + d) / period).rem_euclid(1.0))
    }

    /// Takes beat `index`'s summed spectrum into the downbeat estimate.
    fn finish_beat(&mut self, index: i64, mut sum: Vec<f32>, frames: u32, beats_per_bar: u32) {
        // `as`: a frame count to a divisor.
        let frames = frames.max(1) as f32;
        for v in &mut sum {
            *v /= frames;
        }
        let change = match &self.last_spectrum {
            Some(last) if last.len() == sum.len() => {
                let total: f32 = sum.iter().chain(last).map(|v| v.abs()).sum();
                let diff: f32 = sum.iter().zip(last).map(|(a, b)| (a - b).abs()).sum();
                if total > 1e-6 { f64::from(diff / total) } else { 0.0 }
            }
            _ => 0.0,
        };
        self.last_spectrum = Some(sum);
        let strength = change;
        let n = beats_per_bar.clamp(1, 64) as usize;
        if self.bar_strength.len() != n {
            self.bar_strength = vec![0.0; n];
            self.downbeat = 0;
        }
        let position = index.rem_euclid(n as i64) as usize;
        if let Some(v) = self.bar_strength.get_mut(position) {
            *v = 0.85 * *v + 0.15 * strength;
        }
        let current = self.bar_strength.get(self.downbeat).copied().unwrap_or(0.0);
        let (strongest, value) = self
            .bar_strength
            .iter()
            .copied()
            .enumerate()
            .fold((0, f64::MIN), |b, c| if c.1 > b.1 { c } else { b });
        if value > current * 1.15 {
            self.downbeat = strongest;
        }
    }
}

/// The offset (-0.5 to 0.5) of a parabola's top through three equally spaced points.
fn parabola(a: f64, b: f64, c: f64) -> Option<f64> {
    let d = a - 2.0 * b + c;
    if d.abs() < 1e-18 || !d.is_finite() {
        return None;
    }
    let x = 0.5 * (a - c) / d;
    (x.is_finite() && x.abs() <= 0.5).then_some(x)
}

/// What one analysis frame found.
#[derive(Clone, Debug, Default)]
struct Frame {
    /// Power in the low, mid and high band.
    bands: [f64; 3],
    spectrum: Vec<f64>,
    onset: bool,
    kick: bool,
}
