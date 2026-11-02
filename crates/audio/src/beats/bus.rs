//! The music bus of a sound file: curves over the whole file that music-driven visuals read
//! beside the beat, worked out once from the finished grid, envelopes and sections.
//!
//! - **Travel:** the grid's tempo per moment (the bpm of the spacing the moment is in, held at
//!   the first and last spacing outside the beats), clamped to a floor and a cap, smoothed by a
//!   critically damped filter of second order ([`TRAVEL_SMOOTH`] seconds to 63 % of a step, as
//!   `chan.filter`'s `critical` type), and added up from the file's start: the beats travelled.
//!   Sampled every [`TRAVEL_HOPS`] hops; between samples the tempo is linear and the beats
//!   travelled its exact integral, so both are continuous and the beats never fall.
//! - **Energy:** level, low, mid and high from the hops' raw dBFS, mapped so that the file's
//!   [`ENERGY_LOW`] quantile is 0 and its [`ENERGY_HIGH`] quantile 1, clamped, then followed by
//!   a first-order lag ([`ENERGY_RISE`] up, [`ENERGY_FALL`] down) hop by hop. Comparable across
//!   the whole file, unlike the live normalisation, which follows the last four seconds.
//! - **Sections:** the mean energy of each section, and a drop where a section's energy exceeds
//!   the one before by [`DROP_RISE`] or more.
//!
//! About 8 bytes per hop for the energies and 12 per travel sample: about 7.5 MB for two hours.

use super::{BeatGrid, DEFAULT_BEATS_PER_BAR, Envelopes, LEVEL_LAG_HOPS, Sections};

/// Seconds the travel tempo's filter takes to cover 63 % of a step.
pub const TRAVEL_SMOOTH: f64 = 1.5;
/// The default floor and cap of the travel tempo, beats per minute.
pub const TRAVEL_FLOOR: f64 = 80.0;
pub const TRAVEL_CAP: f64 = 180.0;
/// Hops per sample of the travel curve.
pub const TRAVEL_HOPS: usize = 4;
/// The quantiles of a file's raw levels that map to energy 0 and 1.
pub const ENERGY_LOW: f64 = 0.05;
pub const ENERGY_HIGH: f64 = 0.95;
/// Seconds the energy follower takes to cover 63 % of a step up and down.
pub const ENERGY_RISE: f64 = 0.05;
pub const ENERGY_FALL: f64 = 0.3;
/// How much more energy a section needs than the one before it to begin with a drop.
pub const DROP_RISE: f64 = 0.2;
/// Seconds over which a drop falls to 37 %.
pub const DROP_DECAY: f64 = 0.5;

/// The 63 % time of a critically damped second-order lag whose poles are both at -1.
const CRITICAL_T63: f64 = 2.146_193_220_621;
/// Most travel samples kept: six hours at 4 hops of 48 kHz.
const MAX_SAMPLES: usize = 6 * 3600 * 48_000 / 512 / TRAVEL_HOPS + 2;
/// The raw levels' histogram: steps of 0.5 dB from -120 dBFS.
const DB_FLOOR: f64 = -120.0;
const DB_STEP: f64 = 0.5;
const DB_BINS: usize = 256;

/// The beats travelled over a file: a smoothed and clamped tempo, added up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Travel {
    floor: f64,
    cap: f64,
    /// Seconds between samples.
    step: f64,
    /// The smoothed tempo at each sample, beats per minute.
    bpm: Vec<f32>,
    /// The beats travelled at each sample.
    beats: Vec<f64>,
}

impl Travel {
    /// The travel of a file `end` seconds long, sampled every `step` seconds, with the grid's
    /// tempo clamped to `floor` and `cap` beats per minute (0 or less: no clamp on that side; the
    /// floor wins over a lower cap).
    /// Without a tempo (fewer than two beats) the tempo is 0 everywhere.
    pub fn new(grid: &BeatGrid, end: f64, step: f64, floor: f64, cap: f64) -> Travel {
        let step = if step.is_finite() && step > 0.0 { step } else { 0.01 };
        let end = if end.is_finite() { end.max(0.0) } else { 0.0 };
        // `as`: clamped to MAX_SAMPLES.
        let n = ((end / step).ceil().min(MAX_SAMPLES as f64) as usize).max(1) + 1;
        let (beats, tempo) = (grid.beats(), grid.tempo());
        let mut gap = 0usize;
        let mut tempo_at = |t: f64| -> f64 {
            if tempo.is_empty() {
                return 0.0;
            }
            // The spacing `t` is in, the first before the beats and the last after them.
            while gap + 1 < tempo.len() && beats.get(gap + 1).is_some_and(|b| *b <= t) {
                gap += 1;
            }
            clamp_bpm(tempo.get(gap).copied().unwrap_or(0.0), floor, cap)
        };
        // Two equal first-order lags in a row, exact for an input held over each step.
        let tau = TRAVEL_SMOOTH / CRITICAL_T63;
        let a = (-step / tau).exp();
        let k = step / tau * a;
        let x0 = tempo_at(0.0);
        let (mut y1, mut y2) = (x0, x0);
        let mut bpm = Vec::with_capacity(n);
        let mut travelled = Vec::with_capacity(n);
        // `as`: a tempo in f32 is exact to far below a beat per minute.
        let mut last = x0 as f32;
        let mut sum = 0.0;
        bpm.push(last);
        travelled.push(sum);
        for i in 1..n {
            // The tempo in the middle of the step, held over it.
            // `as`: a sample count, exact in f64.
            let x = tempo_at((i as f64 - 0.5) * step);
            let (e1, e2) = (y1 - x, y2 - x);
            y1 = x + e1 * a;
            y2 = x + e2 * a + e1 * k;
            let now = y2.max(0.0) as f32;
            sum += step * (f64::from(last) + f64::from(now)) / 120.0;
            bpm.push(now);
            travelled.push(sum);
            last = now;
        }
        Travel { floor, cap, step, bpm, beats: travelled }
    }

    /// The floor and the cap it was made with.
    pub fn clamps(&self) -> (f64, f64) {
        (self.floor, self.cap)
    }

    /// Beats travelled and the tempo they move by (beats per minute) at `t` seconds into the
    /// file. Before the start and after the end the tempo holds at the first and the last value.
    pub fn at(&self, t: f64) -> (f64, f64) {
        let (Some(&b0), Some(&first)) = (self.bpm.first(), self.beats.first()) else { return (0.0, 0.0) };
        if !t.is_finite() {
            return (0.0, 0.0);
        }
        if t <= 0.0 {
            return (first + f64::from(b0) * t / 60.0, f64::from(b0));
        }
        let u = t / self.step;
        // `as`: saturates; the index is checked below.
        let i = u.floor() as usize;
        match (self.bpm.get(i), self.bpm.get(i + 1), self.beats.get(i)) {
            (Some(&a), Some(&b), Some(&base)) => {
                let (a, b) = (f64::from(a), f64::from(b));
                // `as`: a sample index, exact in f64.
                let f = u - i as f64;
                let bpm = a + (b - a) * f;
                (base + self.step * (a * f + (b - a) * f * f / 2.0) / 60.0, bpm)
            }
            _ => {
                let last = self.bpm.last().map_or(0.0, |v| f64::from(*v));
                let base = self.beats.last().copied().unwrap_or(0.0);
                // `as`: a sample count, exact in f64.
                let at = (self.beats.len() - 1) as f64 * self.step;
                (base + last * (t - at) / 60.0, last)
            }
        }
    }

    /// Bytes it takes in memory.
    pub fn bytes(&self) -> usize {
        self.bpm.len() * std::mem::size_of::<f32>() + self.beats.len() * std::mem::size_of::<f64>()
    }
}

/// A tempo clamped to `floor` and `cap` (each only when above 0; the floor wins when it lies
/// above the cap); 0 for no number.
fn clamp_bpm(x: f64, floor: f64, cap: f64) -> f64 {
    let mut y = if x.is_finite() { x.max(0.0) } else { 0.0 };
    if cap.is_finite() && cap > 0.0 {
        y = y.min(cap);
    }
    if floor.is_finite() && floor > 0.0 {
        y = y.max(floor);
    }
    y
}

/// One section's energy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionEnergy {
    /// Seconds into the file.
    pub start: f64,
    pub end: f64,
    /// The mean `energy` (level) over the section, 0 to 1.
    pub energy: f64,
    /// It begins with a drop: its energy exceeds the section before by [`DROP_RISE`] or more.
    pub drop: bool,
}

/// What the bus gives at a moment ([`Bus::at`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BusReading {
    pub travelled: f64,
    pub travel_bpm: f64,
    /// Level, low, mid and high, 0 to 1.
    pub energy: [f64; 4],
    /// The mean energy of the section, 0 without sections.
    pub section_energy: f64,
    /// 1 at the start of a section with a drop, falling with [`DROP_DECAY`]; 0 elsewhere.
    pub drop: f64,
}

/// The music bus of a whole file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bus {
    hop_seconds: f64,
    /// Seconds into the file the hops end at.
    end: f64,
    /// Per hop: level, low, mid and high, 0 to 1 in 65 535 steps.
    energy: Vec<[u16; 4]>,
    travel: Travel,
    /// The bar length most bars of the grid have, in beats.
    bar_beats: usize,
    sections: Vec<SectionEnergy>,
}

impl Bus {
    /// The bus of a file with this complete grid and these complete envelopes, the travel
    /// clamped to `floor` and `cap` beats per minute ([`Travel::new`]). The file ends where the
    /// envelopes do.
    pub fn new(grid: &BeatGrid, env: &Envelopes, sections: Option<&Sections>, floor: f64, cap: f64) -> Bus {
        let hop_seconds = env.hop_seconds();
        // `as`: a hop count, exact in f64.
        let end = env.len() as f64 * hop_seconds;
        let energy = energies(env);
        let travel = Travel::new(grid, end, TRAVEL_HOPS as f64 * hop_seconds, floor, cap);
        let mut bus =
            Bus { hop_seconds, end, energy, travel, bar_beats: bar_beats(grid), sections: Vec::new() };
        if let Some(s) = sections {
            let mut before: Option<f64> = None;
            for x in s.list() {
                let energy = bus.mean_energy(x.start, x.end);
                let drop = before.is_some_and(|b| energy - b >= DROP_RISE);
                bus.sections.push(SectionEnergy { start: x.start, end: x.end, energy, drop });
                before = Some(energy);
            }
        }
        bus
    }

    /// The travel it was made with.
    pub fn travel(&self) -> &Travel {
        &self.travel
    }

    /// A travel over the same file with another floor and cap.
    pub fn travel_with(&self, grid: &BeatGrid, floor: f64, cap: f64) -> Travel {
        Travel::new(grid, self.end, self.travel.step, floor, cap)
    }

    /// The bar length most of the grid's bars have, in beats (4 without bars).
    pub fn bar_beats(&self) -> usize {
        self.bar_beats
    }

    /// The sections' energies, in order; empty without sections.
    pub fn sections(&self) -> &[SectionEnergy] {
        &self.sections
    }

    /// Bytes it takes in memory.
    pub fn bytes(&self) -> usize {
        self.energy.len() * std::mem::size_of::<[u16; 4]>()
            + self.travel.bytes()
            + self.sections.len() * std::mem::size_of::<SectionEnergy>()
    }

    /// The bus at `t` seconds into the file, with its own travel.
    pub fn at(&self, t: f64) -> BusReading {
        self.at_with(t, &self.travel)
    }

    /// The bus at `t` seconds into the file, with `travel` for the beats travelled.
    pub fn at_with(&self, t: f64, travel: &Travel) -> BusReading {
        if !t.is_finite() {
            return BusReading::default();
        }
        let (travelled, travel_bpm) = travel.at(t);
        let mut r = BusReading { travelled, travel_bpm, energy: self.energy_at(t), ..BusReading::default() };
        let k = self.sections.partition_point(|s| s.start <= t).saturating_sub(1);
        if let Some(s) = self.sections.get(k) {
            r.section_energy = s.energy;
            if s.drop && t >= s.start {
                r.drop = (-(t - s.start) / DROP_DECAY).exp();
            }
        }
        r
    }

    /// The hop index (with fraction) whose moment is `t`: hop n describes (n + 1 - lag) hops.
    fn hop_at(&self, t: f64) -> f64 {
        if self.hop_seconds > 0.0 { t / self.hop_seconds - 1.0 + LEVEL_LAG_HOPS } else { 0.0 }
    }

    /// The energies at `t`, between the two hops around it; the first or last hop outside them.
    fn energy_at(&self, t: f64) -> [f64; 4] {
        let Some(last) = self.energy.len().checked_sub(1) else { return [0.0; 4] };
        // `as`: a hop count, exact in f64.
        let x = self.hop_at(t).clamp(0.0, last as f64);
        // `as`: within 0..=last.
        let i = x.floor() as usize;
        let f = x - x.floor();
        let at = |i: usize| self.energy.get(i.min(last)).copied().unwrap_or_default();
        let (a, b) = (at(i), at(i + 1));
        let mut out = [0.0; 4];
        for ((o, a), b) in out.iter_mut().zip(a).zip(b) {
            let (a, b) = (f64::from(a) / 65_535.0, f64::from(b) / 65_535.0);
            *o = a + (b - a) * f;
        }
        out
    }

    /// The mean energy (level) of the hops whose moments lie from `from` to `to`; the energy in
    /// the middle when none does.
    fn mean_energy(&self, from: f64, to: f64) -> f64 {
        let first = self.hop_at(from).ceil().max(0.0);
        let last = self.hop_at(to).ceil().max(first);
        // `as`: hop counts, clamped to the hops known.
        let (a, b) = (first as usize, (last as usize).min(self.energy.len()));
        let hops = self.energy.get(a..b).unwrap_or_default();
        if hops.is_empty() {
            return self.energy_at((from + to) / 2.0)[0];
        }
        // `as`: a hop count, exact in f64.
        hops.iter().map(|h| f64::from(h[0]) / 65_535.0).sum::<f64>() / hops.len() as f64
    }
}

/// The bar length most bars of `grid` have, in beats.
fn bar_beats(grid: &BeatGrid) -> usize {
    let beats = grid.beats();
    let starts: Vec<usize> = grid.downbeats().iter().map(|d| beats.partition_point(|b| b < d)).collect();
    let mut counts = std::collections::BTreeMap::<usize, usize>::new();
    for w in starts.windows(2) {
        if let [a, b] = *w
            && b > a
        {
            *counts.entry(b - a).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(len, n)| (*n, std::cmp::Reverse(*len)))
        .map_or(DEFAULT_BEATS_PER_BAR, |(l, _)| l)
}

/// The energies of every hop: each band's raw dBFS [`normalise`]d over the file, then
/// [`follow`]ed.
fn energies(env: &Envelopes) -> Vec<[u16; 4]> {
    let n = env.len();
    let mut out = vec![[0u16; 4]; n];
    let mut band = Vec::with_capacity(n);
    for b in 0..4 {
        band.clear();
        band.extend(
            (0..n).map(|i| env.get(i).map_or(DB_FLOOR, |h| h.raw_db().get(b).copied().unwrap_or(DB_FLOOR))),
        );
        normalise(&mut band);
        follow(&mut band, env.hop_seconds(), ENERGY_RISE, ENERGY_FALL);
        for (o, v) in out.iter_mut().zip(&band) {
            if let Some(slot) = o.get_mut(b) {
                // `as`: clamped to 0..=1 first.
                *slot = (v.clamp(0.0, 1.0) * 65_535.0).round() as u16;
            }
        }
    }
    out
}

/// Maps levels in dBFS in place so that their [`ENERGY_LOW`] quantile is 0 and their
/// [`ENERGY_HIGH`] quantile 1, clamped to 0 to 1. The quantiles come from steps of 0.5 dB from
/// -120 dBFS up. All 0 when both quantiles are equal (silence, a constant tone).
pub fn normalise(db: &mut [f64]) {
    let bin = |v: f64| {
        let x = if v.is_finite() { ((v - DB_FLOOR) / DB_STEP).round() } else { 0.0 };
        // `as`: clamped to the bins first.
        x.clamp(0.0, (DB_BINS - 1) as f64) as usize
    };
    let mut hist = [0usize; DB_BINS];
    for v in db.iter() {
        if let Some(h) = hist.get_mut(bin(*v)) {
            *h += 1;
        }
    }
    let quantile = |q: f64| {
        // `as`: a count of levels, exact in f64 below 2^53.
        let need = (q * db.len() as f64).ceil().max(1.0);
        let mut seen = 0usize;
        for (i, h) in hist.iter().enumerate() {
            seen += h;
            // `as`: as above.
            if seen as f64 >= need {
                // `as`: a bin index.
                return DB_FLOOR + i as f64 * DB_STEP;
            }
        }
        // `as`: a bin index.
        DB_FLOOR + (DB_BINS - 1) as f64 * DB_STEP
    };
    let (lo, hi) = (quantile(ENERGY_LOW), quantile(ENERGY_HIGH));
    for v in db.iter_mut() {
        *v = if hi > lo && v.is_finite() { ((*v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 };
    }
}

/// Follows `x` in place with a first-order lag stepped every `step` seconds: `rise` seconds to
/// cover 63 % of a step up, `fall` of a step down (0 follows at once). The first value passes
/// unchanged.
pub fn follow(x: &mut [f64], step: f64, rise: f64, fall: f64) {
    let gain = |tau: f64| if tau > 0.0 && step > 0.0 { 1.0 - (-step / tau).exp() } else { 1.0 };
    let (up, down) = (gain(rise), gain(fall));
    let Some(&first) = x.first() else { return };
    let mut y = first;
    for v in x.iter_mut() {
        let g = if *v > y { up } else { down };
        y = if g >= 1.0 { *v } else { y + (*v - y) * g };
        *v = y;
    }
}

#[cfg(test)]
#[path = "bus_tests.rs"]
mod tests;
