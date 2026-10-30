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
//! **Confidence and lock:** the confidence is how far the winning period's score stands above
//! the median score of all periods in range, as a share of the room above the median. Each
//! score sums maxima over small windows of lags, so noise scores above 0 too; the median
//! carries the same bias, and the difference does not. Noise, rain and speech stay below 0.15,
//! clean music reaches 0.4 to 0.8. A beat is **locked** once the estimates have backed the
//! tempo for a while: an estimate backs it when its confidence is at least 0.2, it agrees with
//! the tempo within 4 % (or an octave or two from it), and the last 1.5 s hold onsets of their
//! own. The share of backing estimates, averaged over 2.5 s, locks the beat above 0.6 and lets
//! it go below 0.35: about 2.3 s of clear beat to lock, 2.6 s without one to let go. Ticks
//! come only while locked. The phases keep running at the last tempo, so a visual that follows
//! `beat` stays in time through a breakdown; tempo and phases read 0 until the first lock.
//! Frames below the silence floor give no onset strength, so a quiet room's noise makes no
//! beat.
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
//!
//! **Normalising:** level and bands each keep their values of the last four seconds, one per
//! hop (a hop is the unit the bands are measured in, and its length does not depend on how
//! often the caller asks). A value reads 0 at the window's 5th percentile and 1 near its 95th,
//! with a soft knee above 0.85, so a short loud moment never sets the top of the range: it is
//! fewer than 5 % of the window and drops out of it after four seconds. The spectrum takes the
//! 95th percentile of its loudest band as its reference. Frames quieter than the floor are
//! silence: they do not enter the window, and the outputs read 0.

use std::collections::VecDeque;

use crate::fft::{Complex, Fft};
use crate::spectrum::{band_bins, band_power, bin_power, hann, magnitudes};

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
/// Confidence an estimate needs to count towards the lock, and to move the tempo.
const LOCK_CONFIDENCE: f64 = 0.2;
/// Seconds the lock's support averages over.
const SUPPORT_SECONDS: f64 = 2.5;
/// Support above which the beat locks, and below which it lets go.
const LOCK_ABOVE: f64 = 0.6;
const UNLOCK_BELOW: f64 = 0.35;
/// Seconds of onset strength that must hold some of the window's onsets for an estimate to
/// count: a beat that stopped is not supported by the seconds before it.
const RECENT_SECONDS: f64 = 1.5;
/// The share of the window's mean onset strength those seconds must reach.
const RECENT_SHARE: f64 = 0.15;
/// Periods laid over the onset strength to find the beat.
const COMB_BEATS: usize = 6;
/// Lowest spectrum frequency, Hz.
pub const SPECTRUM_LOW: f64 = 30.0;
/// Highest spectrum frequency, Hz (or the Nyquist frequency, if lower).
pub const SPECTRUM_HIGH: f64 = 16_000.0;
/// The span of the spectrum's values: 0 is this far below the reference.
const SPECTRUM_DB: f64 = 60.0;
/// Seconds of levels the normalisation ranges over.
const RANGE_SECONDS: f64 = 4.0;
/// The percentiles of those levels that map to 0 and to 1.
const RANGE_LOW: f64 = 0.05;
const RANGE_HIGH: f64 = 0.95;
/// The smallest range, as a share of its top: a steady tone reads near 1 instead of its tiny
/// wobble being stretched over 0 to 1.
const RANGE_MIN_SPAN: f64 = 0.25;
/// Above this the normalised value bends towards 1 instead of being cut off, so peaks over
/// the range still read louder.
const KNEE: f64 = 0.85;
/// A range whose top is below this (linear) reads 0.
const RANGE_TINY: f64 = 1e-6;
/// Largest lift or cut of the analysed signal, dB.
pub const MAX_TRIM_DB: f64 = 24.0;
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
    /// Map levels and spectrum onto the range of the last seconds, so a quiet input still
    /// spans 0 to 1.
    pub normalize: bool,
    /// With `normalize`: a level below this (dBFS, after `trim_db`) is silence. Levels and
    /// spectrum read 0 and the range is kept as it was.
    pub floor_db: f64,
    /// Gain in dB on the signal before it is analysed (clamped to [`MAX_TRIM_DB`] either
    /// way); `rms` and `peak` report the signal as it came in.
    pub trim_db: f64,
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
            trim_db: 0.0,
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
    /// RMS from 20 Hz to 16 kHz (the three bands together) after the trim, normalised when
    /// asked. Unlike `rms`, it leaves out noise above the hearing range, which some interfaces
    /// have plenty of at 96 kHz.
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
    /// 0 to 1: how far the onsets repeat at that tempo more than at the other tempos in range.
    pub confidence: f64,
    /// A beat is heard: the tempo has kept winning with some confidence for the last seconds.
    pub locked: bool,
    /// 0 at a beat, rising to 1 at the next; keeps running while not locked.
    pub beat: f64,
    /// A beat started in the block (only while locked).
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

/// Normalising by percentiles: the recent values of one level, one per hop, and where its
/// range lies. A loud moment leaves the window after [`RANGE_SECONDS`], so a single knock
/// cannot hold the top of the range for long.
#[derive(Clone, Debug)]
struct Ranger {
    /// A ring of the newest values, written at `pos`.
    values: Vec<f32>,
    cap: usize,
    pos: usize,
    /// For the percentiles, so a query allocates nothing.
    scratch: Vec<f32>,
    /// The percentiles at [`RANGE_LOW`] and [`RANGE_HIGH`], `None` while out of date.
    bounds: Option<(f64, f64)>,
}

impl Ranger {
    fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self { values: Vec::with_capacity(cap), cap, pos: 0, scratch: Vec::with_capacity(cap), bounds: None }
    }

    fn push(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        // `as`: a level to the ring's precision.
        let v = v.max(0.0) as f32;
        if self.values.len() < self.cap {
            self.values.push(v);
        } else if let Some(slot) = self.values.get_mut(self.pos) {
            *slot = v;
        }
        self.pos = (self.pos + 1) % self.cap;
        self.bounds = None;
    }

    /// The low and high percentile of the window; `None` while it is empty.
    fn bounds(&mut self) -> Option<(f64, f64)> {
        if self.bounds.is_none() && !self.values.is_empty() {
            self.scratch.clear();
            self.scratch.extend_from_slice(&self.values);
            let last = self.scratch.len() - 1;
            // `as`: shares of the window to indices.
            let lo = ((last as f64 * RANGE_LOW).round() as usize).min(last);
            let hi = ((last as f64 * RANGE_HIGH).round() as usize).clamp(lo, last);
            let (_, p_lo, above) = self.scratch.select_nth_unstable_by(lo, f32::total_cmp);
            let p_lo = *p_lo;
            let p_hi =
                if hi == lo { p_lo } else { *above.select_nth_unstable_by(hi - lo - 1, f32::total_cmp).1 };
            self.bounds = Some((f64::from(p_lo), f64::from(p_hi)));
        }
        self.bounds
    }

    /// `v` in the window's range: 0 at the low percentile, 0.85 at 85 % of the range, then
    /// bending towards 1. A range with a top below [`RANGE_TINY`] gives 0.
    fn scale(&mut self, v: f64) -> f64 {
        let Some((lo, hi)) = self.bounds() else { return 0.0 };
        if hi.is_nan() || hi <= RANGE_TINY || !v.is_finite() {
            return 0.0;
        }
        let span = (hi - lo).max(RANGE_MIN_SPAN * hi);
        knee((v - (hi - span)) / span)
    }
}

/// The level below which a frame is silence (linear).
fn floor(s: &Settings) -> f64 {
    let db = if s.floor_db.is_finite() { s.floor_db.clamp(-200.0, 0.0) } else { -60.0 };
    10f64.powf(db / 20.0)
}

/// 0 to 1 from a value that is mostly within 0 to 1: linear up to [`KNEE`], then bending to an
/// asymptote at 1 with no kink.
fn knee(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        0.0
    } else if x <= KNEE {
        x
    } else {
        let room = 1.0 - KNEE;
        KNEE + room * (1.0 - (-(x - KNEE) / room).exp())
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
    /// 0 to 1: how much of the last seconds the estimates agreed with the tempo, confidently.
    support: f64,
    /// Whether a beat is heard, from `support` with hysteresis.
    locked: bool,
    /// The beat has locked at least once: from then on tempo and phases are reported.
    found: bool,
}

impl Tempo {
    /// Takes one estimate into the support: whether it backs the tempo. `alpha` is the share
    /// one estimate has in the average.
    fn support(&mut self, backs: bool, alpha: f64) {
        let target = if backs { 1.0 } else { 0.0 };
        self.support += alpha * (target - self.support);
        if self.locked && self.support < UNLOCK_BELOW {
            self.locked = false;
        } else if !self.locked && self.support > LOCK_ABOVE && self.period > 0.0 {
            self.locked = true;
            self.found = true;
        }
    }
}

/// Whether two periods are the same within [`TEMPO_SAME`], also an octave or two apart.
fn same_folded(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() || a <= 0.0 || b <= 0.0 {
        return false;
    }
    let octaves = (a / b).log2();
    (octaves - octaves.round()).abs() < (1.0 + TEMPO_SAME).log2() && octaves.round().abs() <= 2.0
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
    /// The oscillator has taken a measured phase (since the tempo last jumped).
    phased: bool,
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
    /// The ranges of level, low, mid and high.
    ranges: [Ranger; 4],
    /// The range of the loudest spectrum band, whose top is the spectrum's reference.
    spectrum_range: Ranger,
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
        let window = hann(size);
        let fps = rate / hop as f64;
        let tempo_len = (TEMPO_SECONDS * fps).ceil() as usize;
        let picker_len = (0.3 * fps).ceil() as usize;
        let range_len = (RANGE_SECONDS * fps).ceil() as usize;
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
            phased: false,
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
            ranges: std::array::from_fn(|_| Ranger::new(range_len)),
            spectrum_range: Ranger::new(range_len),
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
        let trim = if s.trim_db.is_finite() { s.trim_db.clamp(-MAX_TRIM_DB, MAX_TRIM_DB) } else { 0.0 };
        // `as`: a gain to the samples' precision.
        let gain = 10f64.powf(trim / 20.0) as f32;
        let floor = floor(s);
        for &x in samples {
            let x = if x.is_finite() { x.clamp(-16.0, 16.0) } else { 0.0 };
            sum_sq += f64::from(x) * f64::from(x);
            peak = peak.max(x.abs());
            if let Some(slot) = self.ring.get_mut(self.pos) {
                *slot = (x * gain).clamp(-16.0, 16.0);
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
                self.range_hop(&frame, floor);
            }
        }
        let n = samples.len().max(1) as f64;
        f.rms = (sum_sq / n).sqrt();
        f.peak = f64::from(peak);
        let per_frame = |v: f64| if frames > 0 { (v / frames as f64).sqrt() } else { 0.0 };
        if frames > 0 {
            self.last_amps = (bands_sq.map(per_frame), spectrum_sq.iter().map(|&v| per_frame(v)).collect());
        }
        let ([low, mid, high], amps) = &self.last_amps;
        let level = (low * low + mid * mid + high * high).sqrt();
        let raw = [level, *low, *mid, *high];
        let mut amps = amps.clone();
        amps.resize(bands, 0.0);
        let silent = s.normalize && level < floor;
        let mut out = [0.0; 4];
        if !silent {
            for ((o, r), range) in out.iter_mut().zip(raw).zip(self.ranges.iter_mut()) {
                *o = if s.normalize { range.scale(r) } else { r };
            }
        }
        [f.level, f.low, f.mid, f.high] = out;
        let reference = if !s.normalize {
            1.0
        } else if silent {
            0.0
        } else {
            self.spectrum_range.bounds().map_or(0.0, |(_, hi)| hi)
        };
        f.spectrum = amps
            .iter()
            .map(|&a| {
                if a <= 0.0 || reference <= RANGE_TINY {
                    return 0.0;
                }
                (1.0 + 20.0 * (a / reference).log10() / SPECTRUM_DB).clamp(0.0, 1.0)
            })
            .collect();
        self.describe(&mut f, s);
        f
    }

    /// Takes one frame's levels into the ranges, unless the frame is quieter than `floor`:
    /// silence keeps the range of the sound before it.
    fn range_hop(&mut self, frame: &Frame, floor: f64) {
        let [low, mid, high] = frame.bands.map(|p| p.max(0.0).sqrt());
        let level = (low * low + mid * mid + high * high).sqrt();
        if level.is_nan() || level < floor {
            return;
        }
        for (range, v) in self.ranges.iter_mut().zip([level, low, mid, high]) {
            range.push(v);
        }
        let loudest = frame.spectrum.iter().copied().fold(0.0, f64::max);
        self.spectrum_range.push(loudest.max(0.0).sqrt());
    }

    /// The phases and tempo as they stand.
    fn describe(&mut self, f: &mut Features, s: &Settings) {
        f.confidence = self.tempo.confidence;
        f.locked = self.tempo.locked;
        if self.tempo.period > 0.0 && self.phased && self.tempo.found {
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
            f.tick = self.tempo.locked && self.reported.is_some_and(|r| index > r);
            if self.reported.is_none_or(|r| index > r) {
                self.reported = Some(index);
            }
        }
    }

    fn frame(&mut self, s: &Settings, bands: usize) -> Frame {
        let size = self.size();
        magnitudes(&self.fft, &self.ring, self.pos, &self.window, &mut self.scratch, &mut self.magnitudes);
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
            let power = bin_power(*m);
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
        // Silence has no onsets: the room's noise below the floor must not make a beat.
        let level = out.bands.iter().map(|p| p.max(0.0)).sum::<f64>().sqrt();
        if s.normalize && (level.is_nan() || level < floor(s)) {
            (flux, flux_low, flux_beat) = (0.0, 0.0, 0.0);
        }
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
            let high = SPECTRUM_HIGH.min(self.rate / 2.0);
            let edges = band_bins(bands, SPECTRUM_LOW, high, true, self.bin_hz(), size / 2);
            self.edges = Some((bands, edges));
        }
        let Some((_, edges)) = &self.edges else { return };
        band_power(&self.magnitudes, edges, out);
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
        let alpha = 1.0 - (-TEMPO_EVERY / SUPPORT_SECONDS).exp();
        if ac0.is_nan() || ac0 <= 1e-12 {
            self.tempo.confidence *= 0.7;
            self.tempo.support(false, alpha);
            return;
        }
        let at = |i: usize| ac.get(i).copied().unwrap_or(0.0) / ac0;
        // The best value near `center`, within `reach` either side.
        let best_near = |center: usize, reach: usize| -> (usize, f64) {
            let lo = center.saturating_sub(reach);
            (lo..=center + reach)
                .map(|i| (i, at(i)))
                .fold((center, f64::MIN), |b, c| if c.1 > b.1 { c } else { b })
        };
        // Per period, how well the onsets repeat at it (at most 1).
        let mut scores = Vec::with_capacity(lag_max - lag_min + 1);
        let mut best = (0usize, f64::MIN, 0.0);
        for lag in lag_min..=lag_max {
            let mut score = 0.0;
            for m in 1..=4usize {
                score += best_near(m * lag, m - 1).1;
            }
            // Off-beats (hi-hats, a bass between the kicks) speak for the period whose half
            // they fall on; a period of one and a half beats has nothing at its half.
            score += 0.5 * best_near(lag / 2, 1).1.max(0.0);
            let score = score / 4.5;
            scores.push(score);
            let bpm = 60.0 * fps / lag as f64;
            // A preference for tempos near 120, to break ties between related periods.
            let prior = (-0.5 * (bpm / 120.0).log2().powi(2)).exp();
            if score * prior > best.1 {
                best = (lag, score * prior, score);
            }
        }
        let (lag, _, score) = best;
        if lag == 0 {
            return;
        }
        // Confidence is how far the winner stands out from the typical period in range. Each
        // score is a sum of maxima over windows of lags, so even noise scores above 0; the
        // median carries the same bias and takes it out.
        scores.sort_by(f64::total_cmp);
        let median = scores.get(scores.len() / 2).copied().unwrap_or(0.0);
        let confidence = if median < 1.0 { ((score - median) / (1.0 - median)).clamp(0.0, 1.0) } else { 0.0 };
        // Refine from the fourth multiple's peak, interpolated.
        let (peak4, _) = best_near(4 * lag, 3);
        let refined = parabola(at(peak4.saturating_sub(1)), at(peak4), at(peak4 + 1))
            .map(|d| (peak4 as f64 + d) / 4.0)
            .filter(|p| (p / lag as f64 - 1.0).abs() < 0.1)
            .unwrap_or(lag as f64);
        // A beat that stopped leaves its repeats in the window for seconds; the newest part
        // must hold onsets of its own.
        // `as`: seconds to hops.
        let recent = ((RECENT_SECONDS * fps).ceil() as usize).max(1);
        let recent_mean =
            self.odf.values.iter().rev().take(recent).sum::<f32>() / recent.min(n).max(1) as f32;
        let active = f64::from(recent_mean) >= RECENT_SHARE * f64::from(mean);
        let t = &mut self.tempo;
        t.confidence = 0.7 * t.confidence + 0.3 * confidence;
        if confidence < LOCK_CONFIDENCE {
            t.support(false, alpha);
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
                self.phased = false;
            }
        }
        let backs = active && same_folded(refined, t.period);
        t.support(backs, alpha);
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
            if !self.phased {
                self.phase += err;
                self.phased = true;
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
                    // Clean music has a confidence of about 0.5 and up.
                    let gain = 0.003 + 0.012 * (self.tempo.confidence / 0.5).min(1.0);
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
