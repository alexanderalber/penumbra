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
//! - **A section's character** ([`SectionEnergy`]): its bass, how many onsets come per beat, the
//!   share of its beats with a kick, the jump in energy from the section before, a [`Role`] in
//!   its track, and `change`: how strongly the music changes where the section begins, 0 to 1,
//!   the one number to scale a change of the picture by.
//!
//! About 8 bytes per hop for the energies and 12 per travel sample: about 7.5 MB for two hours.

use super::{
    BeatGrid, DEFAULT_BEATS_PER_BAR, Envelopes, KICK_LAG_HOPS, LEVEL_LAG_HOPS, ONSET_LAG_HOPS, Section,
    Sections,
};

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
/// A kick counts when the low band at it comes within this many dB of the file's loud low level
/// (its [`ENERGY_HIGH`] quantile): the live kick also fires on soft bass notes, which a
/// breakdown keeps.
pub const KICK_DB: f64 = 6.0;
/// Hops after a kick's flag that its attack may peak in.
const KICK_PEAK_HOPS: usize = 6;
/// A section with a kick in fewer than this share of its beats is a break.
pub const BREAK_KICK: f64 = 0.3;
/// A section whose energy lies this much or more below its track's median section energy is a
/// break.
pub const BREAK_DIP: f64 = 0.2;
/// How much more energy a build's last quarter needs than its first.
pub const BUILD_RISE: f64 = 0.15;
/// How much a boundary's novelty `strength` counts in its `change`. Below 1, so the many small
/// changes of timbre inside a track stay below about 0.3.
pub const CHANGE_NOVELTY: f64 = 0.7;
/// How much a boundary's energy jump counts in its `change`: a jump of 0.25 (a drop or the start
/// of a breakdown) gives 0.5.
pub const CHANGE_JUMP: f64 = 2.0;
/// What a track start adds to a boundary's `change`: enough to bring every track start near 1.
pub const CHANGE_TRACK: f64 = 0.6;

/// The 63 % time of a critically damped second-order lag whose poles are both at -1.
const CRITICAL_T63: f64 = 2.146_193_220_621;
/// Most travel samples kept: six hours at 4 hops of 48 kHz.
const MAX_SAMPLES: usize = 6 * 3600 * 48_000 / 512 / TRAVEL_HOPS + 2;
/// The raw levels' histogram: steps of 0.5 dB from -120 dBFS.
const DB_FLOOR: f64 = -120.0;
const DB_STEP: f64 = 0.5;
const DB_BINS: usize = 256;

/// A spacing of the grid longer than this many beats of the tempo around it is a break: a
/// stretch in which the network found no beat, which the fold left open.
pub const BREAK_SPACINGS: f64 = 1.5;

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
    /// The bar length most bars of the grid have, in beats.
    bar_beats: f64,
    /// The grid's breaks, in order, for [`Travel::drift`].
    breaks: Vec<Break>,
}

/// A break of the grid as [`Travel::drift`] bridges it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Break {
    /// The beats before and after it, seconds.
    start: f64,
    end: f64,
    /// The drift at its start and its end, with the whole bars of every break up to it taken off.
    from: f64,
    to: f64,
    /// The whole bars taken off the drift after it, this break's and those before.
    after: f64,
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
        let mut travel = Travel {
            floor,
            cap,
            step,
            bpm,
            beats: travelled,
            bar_beats: bar_beats(grid) as f64,
            breaks: Vec::new(),
        };
        travel.breaks = travel.find_breaks(grid);
        travel
    }

    /// The grid's breaks: the spacings longer than [`BREAK_SPACINGS`] beats of their tempo.
    fn find_breaks(&self, grid: &BeatGrid) -> Vec<Break> {
        let (beats, tempo) = (grid.beats(), grid.tempo());
        let mut out = Vec::new();
        let mut before = 0.0;
        for (i, w) in beats.windows(2).enumerate() {
            let [start, end] = *w else { continue };
            let bpm = tempo.get(i).copied().unwrap_or(0.0);
            if !(bpm > 0.0 && bpm.is_finite()) || end - start <= BREAK_SPACINGS * 60.0 / bpm {
                continue;
            }
            let (Some(a), Some(b)) = (grid.at(start), grid.at(end)) else { continue };
            // `as`: counts far below 2^53.
            let raw = |t: f64, p: super::Position| p.bar as f64 + p.bar_phase - self.at(t).0 / self.bar_beats;
            let (from, to) = (raw(start, a) - before, raw(end, b) - before);
            // The whole bars that bring the end nearest the start: the drift then changes by
            // less than half a bar over the break.
            let shift = (to - from).round();
            out.push(Break { start, end, from, to: to - shift, after: before + shift });
            before += shift;
        }
        out
    }

    /// Where the bars stand against the beats travelled at `t` seconds into the file, in bars:
    /// `bars` (the bars the grid counts there, with the fraction of the bar) minus the beats
    /// travelled over the usual bar length. In a break the grid counts one beat over the whole
    /// break while the beats travelled go on, so this would fall fast there and things placed by
    /// the bars would stop. Instead it runs evenly through each break, from its value at the
    /// break's start to its value at the end less whole bars, and keeps those whole bars off
    /// after the break. So it changes slowly everywhere, and a downbeat still falls where it adds
    /// to a whole number with the beats travelled over the bar length.
    pub fn drift(&self, t: f64, bars: f64) -> f64 {
        let raw = bars - self.at(t).0 / self.bar_beats.max(1.0);
        let k = self.breaks.partition_point(|b| b.start <= t);
        match k.checked_sub(1).and_then(|i| self.breaks.get(i)) {
            None => raw,
            Some(b) if t < b.end && b.end > b.start => {
                b.from + (b.to - b.from) * (t - b.start) / (b.end - b.start)
            }
            Some(b) => raw - b.after,
        }
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
        self.bpm.len() * std::mem::size_of::<f32>()
            + self.beats.len() * std::mem::size_of::<f64>()
            + self.breaks.len() * std::mem::size_of::<Break>()
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

/// What a section does in its track, numbered 0 to 5 in this order ([`Role::index`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Role {
    /// The first section of a track with more than one.
    Intro,
    /// Few kicks ([`BREAK_KICK`]) or much less energy than the track's middle ([`BREAK_DIP`]).
    Break,
    /// The energy rises over it ([`BUILD_RISE`]), and a drop follows.
    Build,
    /// It begins with a drop ([`DROP_RISE`]).
    Drop,
    /// The last section of a track with more than one, unless it begins with a drop.
    Outro,
    /// Anything else.
    #[default]
    Main,
}

impl Role {
    /// Every role, in the order of their numbers.
    pub const ALL: [Role; 6] = [Role::Intro, Role::Break, Role::Build, Role::Drop, Role::Outro, Role::Main];

    /// Its number, 0 to 5.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Its name in lower case, as tables and commands write it.
    pub fn name(self) -> &'static str {
        match self {
            Role::Intro => "intro",
            Role::Break => "break",
            Role::Build => "build",
            Role::Drop => "drop",
            Role::Outro => "outro",
            Role::Main => "main",
        }
    }
}

/// One section's energy and character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionEnergy {
    /// Seconds into the file.
    pub start: f64,
    pub end: f64,
    /// The mean `energy` (level) over the section, 0 to 1.
    pub energy: f64,
    /// It begins with a drop: its energy exceeds the section before by [`DROP_RISE`] or more.
    pub drop: bool,
    /// The mean `energy_low` (bass) over the section, 0 to 1.
    pub bass: f64,
    /// Onsets per grid beat in the section: about 1 for a beat of kicks alone, more for busy
    /// percussion, 0 without beats.
    pub density: f64,
    /// The share of the section's beats with a kick, 0 to 1: a kick flag whose low band comes
    /// within [`KICK_DB`] of the file's loud low level. A beat's window begins a quarter beat
    /// early, so a kick a little ahead of the grid still counts.
    pub kick: f64,
    /// Its energy minus the section before's, -1 to 1; 0 for the first section.
    pub jump: f64,
    /// What it does in its track.
    pub role: Role,
    /// How strongly the music changes where it begins, 0 to 1; 0 for the first section: the
    /// larger of [`CHANGE_NOVELTY`] times the novelty `strength` and [`CHANGE_JUMP`] times the
    /// jump's size, plus [`CHANGE_TRACK`] at a track start, clamped. The number to scale a change
    /// of the picture by.
    pub change: f64,
}

/// What a section's role and change are found from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Traits {
    track: usize,
    track_start: bool,
    strength: f64,
    energy: f64,
    kick: f64,
    /// The mean energy of its last quarter minus its first.
    rise: f64,
}

/// What [`judge`] finds for one section.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Verdict {
    jump: f64,
    drop: bool,
    role: Role,
    change: f64,
}

/// The median of `v`, 0 when empty.
fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => at(n / 2),
        _ => (at(n / 2 - 1) + at(n / 2)) / 2.0,
    }
}

/// Jump, drop, role and change of each section, in order. The first role that fits wins, in
/// this order: intro, drop, outro, build, break, main. So a track's first section is its intro
/// even when quiet, and a drop stays a drop at the end of a track.
fn judge(traits: &[Traits]) -> Vec<Verdict> {
    let mut by_track = std::collections::BTreeMap::<usize, Vec<f64>>::new();
    for t in traits {
        by_track.entry(t.track).or_default().push(t.energy);
    }
    let medians: std::collections::BTreeMap<usize, f64> =
        by_track.iter().map(|(track, v)| (*track, median(v))).collect();
    let jumps: Vec<f64> = traits
        .iter()
        .enumerate()
        .map(|(i, t)| i.checked_sub(1).and_then(|p| traits.get(p)).map_or(0.0, |b| t.energy - b.energy))
        .collect();
    let drops: Vec<bool> = jumps.iter().enumerate().map(|(i, j)| i > 0 && *j >= DROP_RISE).collect();
    traits
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let jump = jumps.get(i).copied().unwrap_or(0.0);
            let drop = drops.get(i).copied().unwrap_or(false);
            let first = i.checked_sub(1).and_then(|p| traits.get(p)).is_none_or(|b| b.track != t.track);
            let last = traits.get(i + 1).is_none_or(|n| n.track != t.track);
            let middle = medians.get(&t.track).copied().unwrap_or(0.0);
            let role = if first && !last {
                Role::Intro
            } else if drop {
                Role::Drop
            } else if last && !first {
                Role::Outro
            } else if t.rise >= BUILD_RISE && drops.get(i + 1).copied().unwrap_or(false) {
                Role::Build
            } else if t.kick < BREAK_KICK || t.energy <= middle - BREAK_DIP {
                Role::Break
            } else {
                Role::Main
            };
            let change = if i == 0 {
                0.0
            } else {
                let start = if t.track_start { CHANGE_TRACK } else { 0.0 };
                let c = (CHANGE_NOVELTY * t.strength).max(CHANGE_JUMP * jump.abs()) + start;
                if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 }
            };
            Verdict { jump, drop, role, change }
        })
        .collect()
}

/// The moments of the hops with an onset and of those with a kick whose low band reaches
/// `kick_low` dBFS, seconds into the file, in order.
fn flag_moments(env: &Envelopes, kick_low: f64) -> (Vec<f64>, Vec<f64>) {
    let h = env.hop_seconds();
    let (mut onsets, mut kicks) = (Vec::new(), Vec::new());
    let low = |i: usize| env.get(i).map_or(DB_FLOOR, |x| x.raw_db().get(1).copied().unwrap_or(DB_FLOOR));
    for i in 0..env.len() {
        let Some(hop) = env.get(i) else { continue };
        // `as`: a hop count, exact in f64.
        let n = i as f64 + 1.0;
        if hop.onset() {
            onsets.push((n - ONSET_LAG_HOPS) * h);
        }
        // The low band from just before the flag's hop to the hops after it: the attack.
        if hop.kick() && (i.saturating_sub(1)..=i + KICK_PEAK_HOPS).any(|j| low(j) >= kick_low) {
            kicks.push((n - KICK_LAG_HOPS) * h);
        }
    }
    (onsets, kicks)
}

/// How many of the sorted `moments` lie from `from` up to `to`.
fn count_within(moments: &[f64], from: f64, to: f64) -> usize {
    moments.partition_point(|m| *m < to).saturating_sub(moments.partition_point(|m| *m < from))
}

/// Onsets per beat and the share of beats with a kick from `from` to `to` seconds, for the
/// grid's `beats`; both 0 without a beat there.
fn rhythm(beats: &[f64], onsets: &[f64], kicks: &[f64], from: f64, to: f64) -> (f64, f64) {
    let (a, b) = (beats.partition_point(|x| *x < from), beats.partition_point(|x| *x < to));
    if b <= a {
        return (0.0, 0.0);
    }
    let mut with_kick = 0usize;
    for k in a..b {
        let Some(&at) = beats.get(k) else { continue };
        // The beat's length: to the next beat, or the one before's for the file's last beat.
        let gap = match (beats.get(k + 1), k.checked_sub(1).and_then(|p| beats.get(p))) {
            (Some(n), _) => n - at,
            (None, Some(p)) => at - p,
            (None, None) => 0.5,
        };
        let early = gap / 4.0;
        if count_within(kicks, at - early, at + gap - early) > 0 {
            with_kick += 1;
        }
    }
    // `as`: counts far below 2^53.
    let n = (b - a) as f64;
    (count_within(onsets, from, to) as f64 / n, with_kick as f64 / n)
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
    /// The section under the moment; none without sections.
    pub section: Option<SectionEnergy>,
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
        let (energy, loud_low) = energies(env);
        let travel = Travel::new(grid, end, TRAVEL_HOPS as f64 * hop_seconds, floor, cap);
        let mut bus =
            Bus { hop_seconds, end, energy, travel, bar_beats: bar_beats(grid), sections: Vec::new() };
        if let Some(s) = sections {
            bus.sections = bus.characters(s.list(), grid.beats(), env, loud_low);
        }
        bus
    }

    /// The energy and character of each of `list`, with the grid's `beats` and the envelopes'
    /// onsets and kicks; `loud_low` is the low band's level of a loud kick, dBFS.
    fn characters(
        &self,
        list: &[Section],
        beats: &[f64],
        env: &Envelopes,
        loud_low: f64,
    ) -> Vec<SectionEnergy> {
        let (onsets, kicks) = flag_moments(env, loud_low - KICK_DB);
        let mut out = Vec::with_capacity(list.len());
        let mut traits = Vec::with_capacity(list.len());
        for x in list {
            let energy = self.mean_band(x.start, x.end, 0);
            let bass = self.mean_band(x.start, x.end, 1);
            let (density, kick) = rhythm(beats, &onsets, &kicks, x.start, x.end);
            let quarter = (x.end - x.start).max(0.0) / 4.0;
            let rise =
                self.mean_band(x.end - quarter, x.end, 0) - self.mean_band(x.start, x.start + quarter, 0);
            traits.push(Traits {
                track: x.track,
                track_start: x.track_start,
                strength: x.strength,
                energy,
                kick,
                rise,
            });
            out.push(SectionEnergy {
                start: x.start,
                end: x.end,
                energy,
                drop: false,
                bass,
                density,
                kick,
                jump: 0.0,
                role: Role::Main,
                change: 0.0,
            });
        }
        for (o, v) in out.iter_mut().zip(judge(&traits)) {
            (o.jump, o.drop, o.role, o.change) = (v.jump, v.drop, v.role, v.change);
        }
        out
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
            r.section = Some(*s);
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

    /// The mean energy of `band` (0 level, 1 low, 2 mid, 3 high) over the hops whose moments lie
    /// from `from` to `to`; the energy in the middle when none does.
    fn mean_band(&self, from: f64, to: f64, band: usize) -> f64 {
        let first = self.hop_at(from).ceil().max(0.0);
        let last = self.hop_at(to).ceil().max(first);
        // `as`: hop counts, clamped to the hops known.
        let (a, b) = (first as usize, (last as usize).min(self.energy.len()));
        let hops = self.energy.get(a..b).unwrap_or_default();
        if hops.is_empty() {
            return self.energy_at((from + to) / 2.0).get(band).copied().unwrap_or(0.0);
        }
        let sum: f64 = hops.iter().map(|h| f64::from(h.get(band).copied().unwrap_or(0)) / 65_535.0).sum();
        // `as`: a hop count, exact in f64.
        sum / hops.len() as f64
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
/// [`follow`]ed. Also the low band's [`ENERGY_HIGH`] quantile in dBFS, the level of a loud kick.
fn energies(env: &Envelopes) -> (Vec<[u16; 4]>, f64) {
    let mut loud_low = DB_FLOOR;
    let n = env.len();
    let mut out = vec![[0u16; 4]; n];
    let mut band = Vec::with_capacity(n);
    for b in 0..4 {
        band.clear();
        band.extend(
            (0..n).map(|i| env.get(i).map_or(DB_FLOOR, |h| h.raw_db().get(b).copied().unwrap_or(DB_FLOOR))),
        );
        if b == 1 {
            loud_low = quantiles(&band).1;
        }
        normalise(&mut band);
        follow(&mut band, env.hop_seconds(), ENERGY_RISE, ENERGY_FALL);
        for (o, v) in out.iter_mut().zip(&band) {
            if let Some(slot) = o.get_mut(b) {
                // `as`: clamped to 0..=1 first.
                *slot = (v.clamp(0.0, 1.0) * 65_535.0).round() as u16;
            }
        }
    }
    (out, loud_low)
}

/// Maps levels in dBFS in place so that their [`ENERGY_LOW`] quantile is 0 and their
/// [`ENERGY_HIGH`] quantile 1, clamped to 0 to 1. The quantiles come from steps of 0.5 dB from
/// -120 dBFS up. All 0 when both quantiles are equal (silence, a constant tone).
pub fn normalise(db: &mut [f64]) {
    let (lo, hi) = quantiles(db);
    for v in db.iter_mut() {
        *v = if hi > lo && v.is_finite() { ((*v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 };
    }
}

/// The [`ENERGY_LOW`] and [`ENERGY_HIGH`] quantiles of levels in dBFS, in steps of 0.5 dB from
/// -120 dBFS up.
fn quantiles(db: &[f64]) -> (f64, f64) {
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
    (quantile(ENERGY_LOW), quantile(ENERGY_HIGH))
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
