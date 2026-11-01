//! The envelopes of a sound file, found ahead of time: what [`Analyzer`] measures while the file
//! plays (level, low, mid, high, onset, kick), worked out once over the whole file, so a reader
//! at the playhead gets them with no lag.
//!
//! - **The analysis** is the live one: the file is mixed to mono, resampled to
//!   [`ENVELOPE_RATE`] (the rate an `audio.file` plays at unless told otherwise) and fed to an
//!   [`Analyzer`] with the default [`Settings`], one hop at a time. Each hop's normalised levels,
//!   its raw levels and its onset and kick flags are kept: 13 bytes per hop, about 10 kB per
//!   second, 8.6 MB for two hours.
//! - **Time:** the live analysis reports a hop when it ends. Its levels describe the middle of
//!   the frame, [`LEVEL_LAG_HOPS`] earlier; an onset is known [`ONSET_LAG_HOPS`] after the
//!   attack that caused it, a kick [`KICK_LAG_HOPS`]. [`Envelopes::read`] takes both lags out, so the reading at a moment
//!   is what the live analysis reports that much later.
//! - **Raw levels** (dBFS before normalising) stay comparable across the whole file, unlike the
//!   normalised ones, which follow the last four seconds. [`Envelopes::energy_db`] averages them
//!   over any span, a beat or a bar.
//! - **Coarse spectrum:** [`SPECTRUM_BANDS`] log-spaced bands from 30 Hz to 16 kHz in dBFS, the
//!   mean power of every [`SPECTRUM_HOPS`] hops (about 43 ms), 32 bytes each: about 0.75 kB per
//!   second, 5.3 MB for two hours. Enough to tell a section's sound from another's
//!   ([`super::Sections`]); the three bands alone are not.
//! - **Partial results:** the hops come in blocks of [`BLOCK_HOPS`], each shared, so handing on
//!   the envelopes so far costs no copy. A moment not analysed yet reads 0.
//! - **Cache:** one binary file per sound file beside the beat cache, keyed by the sound
//!   file's size and modification time, the format's version and the analysis settings, with a
//!   checksum over the hops.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::analyze::{MAX_SECONDS, fnv1a};
use super::{BeatError, ToRate, cache, cancelled, mix_down};
use crate::analysis::{Analyzer, Settings};
use crate::file;
use crate::resample::{MAX_CHANNELS, MAX_RATE, MIN_RATE};

/// The rate the envelopes are measured at, samples per second.
pub const ENVELOPE_RATE: f64 = 48_000.0;
/// Hops in one shared block.
pub const BLOCK_HOPS: usize = 4096;
/// Hops a level lags behind the sound: the frame is four hops long, and its middle is two hops
/// before its end.
pub const LEVEL_LAG_HOPS: f64 = 2.0;
/// Hops an onset lags behind the attack that caused it: a peak of the onset strength is known
/// one hop after it, and it comes when the attack has reached into the frame. Measured on the
/// example track's beats: median 2.5 hops.
pub const ONSET_LAG_HOPS: f64 = 2.5;
/// The same for a kick, whose low band rises sooner: median 2 hops.
pub const KICK_LAG_HOPS: f64 = 2.0;
/// The longest span [`Envelopes::read`] averages over; a longer step reads one moment.
const MAX_READ_SECONDS: f64 = 2.0;
/// First word of a cache file.
const MAGIC: &str = "penumbra-envelopes";
/// Raised whenever the analysis, the resampling or the file's layout changes what a hop holds
/// (2: the coarse spectrum).
const VERSION: u32 = 2;
/// Bands of the coarse spectrum, log-spaced from [`crate::analysis::SPECTRUM_LOW`] to
/// [`crate::analysis::SPECTRUM_HIGH`].
pub const SPECTRUM_BANDS: usize = 32;
/// Hops averaged into one frame of the coarse spectrum.
pub const SPECTRUM_HOPS: usize = 4;
/// One frame of the coarse spectrum: per band, the mean power in [`RAW_STEP_DB`] steps above
/// [`RAW_FLOOR_DB`], as the raw levels.
type Frame = [u8; SPECTRUM_BANDS];
/// Bytes per hop in a cache file.
const HOP_BYTES: usize = 13;
/// The raw levels as stored: steps of this many dB above [`RAW_FLOOR_DB`]; code 0 is that
/// floor or quieter.
const RAW_STEP_DB: f64 = 0.5;
const RAW_FLOOR_DB: f64 = -120.0;
const ONSET: u8 = 1;
const KICK: u8 = 2;

/// One hop of the analysis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hop {
    /// Level, low, mid and high as the live analysis gives them, 0 to 1 in 65 535 steps.
    norm: [u16; 4],
    /// The same before normalising, in [`RAW_STEP_DB`] steps above [`RAW_FLOOR_DB`].
    raw: [u8; 4],
    flags: u8,
}

impl Hop {
    fn new(norm: [f64; 4], raw: [f64; 4], onset: bool, kick: bool) -> Hop {
        // `as`: clamped to the code ranges first.
        let n = norm.map(|v| if v.is_finite() { (v.clamp(0.0, 1.0) * 65_535.0).round() as u16 } else { 0 });
        let r = raw.map(|a| {
            if !(a.is_finite() && a > 0.0) {
                return 0;
            }
            ((20.0 * a.log10() - RAW_FLOOR_DB) / RAW_STEP_DB).round().clamp(0.0, 255.0) as u8
        });
        let flags = if onset { ONSET } else { 0 } | if kick { KICK } else { 0 };
        Hop { norm: n, raw: r, flags }
    }

    /// Level, low, mid and high as the live analysis gives them, 0 to 1.
    pub fn levels(&self) -> [f64; 4] {
        self.norm.map(|v| f64::from(v) / 65_535.0)
    }

    /// Level, low, mid and high before normalising, as power (amplitude squared, full scale 1);
    /// 0 at or below -120 dBFS.
    pub fn power(&self) -> [f64; 4] {
        self.raw.map(|c| {
            if c == 0 { 0.0 } else { 10f64.powf((RAW_FLOOR_DB + f64::from(c) * RAW_STEP_DB) / 10.0) }
        })
    }

    pub fn onset(&self) -> bool {
        self.flags & ONSET != 0
    }

    pub fn kick(&self) -> bool {
        self.flags & KICK != 0
    }

    fn write(&self, out: &mut Vec<u8>) {
        for v in self.norm {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.raw);
        out.push(self.flags);
    }

    fn read(b: &[u8; HOP_BYTES]) -> Option<Hop> {
        let [a0, a1, b0, b1, c0, c1, d0, d1, r0, r1, r2, r3, flags] = *b;
        if flags & !(ONSET | KICK) != 0 {
            return None;
        }
        Some(Hop {
            norm: [
                u16::from_le_bytes([a0, a1]),
                u16::from_le_bytes([b0, b1]),
                u16::from_le_bytes([c0, c1]),
                u16::from_le_bytes([d0, d1]),
            ],
            raw: [r0, r1, r2, r3],
            flags,
        })
    }
}

/// What [`Envelopes::read`] gives for a moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub level: f64,
    pub low: f64,
    pub mid: f64,
    pub high: f64,
    pub onset: bool,
    pub kick: bool,
    /// The moment is analysed. When not, every value is 0.
    pub known: bool,
}

/// A power (full scale 1) as a code of [`RAW_STEP_DB`] steps above [`RAW_FLOOR_DB`].
fn power_code(p: f64) -> u8 {
    if !(p.is_finite() && p > 0.0) {
        return 0;
    }
    // `as`: clamped to the code range first.
    ((10.0 * p.log10() - RAW_FLOOR_DB) / RAW_STEP_DB).round().clamp(0.0, 255.0) as u8
}

/// A code of [`power_code`] in dBFS.
fn code_db(c: u8) -> f32 {
    // `as`: a few hundred dB in steps of 0.5, exact in f32.
    (RAW_FLOOR_DB + f64::from(c) * RAW_STEP_DB) as f32
}

/// A sound file's envelopes, as far as they are known.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelopes {
    rate: f64,
    hop: usize,
    blocks: Vec<Arc<[Hop]>>,
    len: usize,
    /// The coarse spectrum, one block per block of hops ([`BLOCK_HOPS`] / [`SPECTRUM_HOPS`]
    /// frames each).
    spectra: Vec<Arc<[Frame]>>,
    frames: usize,
    complete: bool,
}

impl Envelopes {
    fn new(
        rate: f64,
        hop: usize,
        blocks: Vec<Arc<[Hop]>>,
        spectra: Vec<Arc<[Frame]>>,
        complete: bool,
    ) -> Self {
        let len = blocks.iter().map(|b| b.len()).sum();
        let frames = spectra.iter().map(|b| b.len()).sum();
        Envelopes { rate, hop, blocks, len, spectra, frames, complete }
    }

    /// Samples per second the hops are counted at.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Samples per hop.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Seconds per hop.
    pub fn hop_seconds(&self) -> f64 {
        self.hop as f64 / self.rate.max(1.0)
    }

    /// Hops known.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The whole file is analysed.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Bytes the hops and the coarse spectrum take in memory.
    pub fn bytes(&self) -> usize {
        self.len * std::mem::size_of::<Hop>() + self.frames * SPECTRUM_BANDS
    }

    /// Frames of the coarse spectrum known.
    pub fn spectrum_len(&self) -> usize {
        self.frames
    }

    /// Seconds per frame of the coarse spectrum.
    pub fn spectrum_seconds(&self) -> f64 {
        SPECTRUM_HOPS as f64 * self.hop_seconds()
    }

    /// The moment frame `i` of the coarse spectrum describes, seconds into the file: the mean
    /// of its hops' moments (each hop's frame middle).
    pub fn spectrum_time(&self, i: usize) -> f64 {
        // Hop n's moment is (n + 1 - LEVEL_LAG_HOPS) hops; frame i holds hops 4i to 4i + 3.
        // `as`: hop counts to hops.
        let first = (i * SPECTRUM_HOPS) as f64 + 1.0 - LEVEL_LAG_HOPS;
        (first + (SPECTRUM_HOPS as f64 - 1.0) / 2.0) * self.hop_seconds()
    }

    /// The frames of the coarse spectrum whose moment lies from `from` (included) to `to`
    /// seconds (excluded); empty for times that are no numbers.
    pub fn spectrum_range(&self, from: f64, to: f64) -> std::ops::Range<usize> {
        let step = self.spectrum_seconds();
        let zero = self.spectrum_time(0);
        if !(from.is_finite() && to.is_finite() && step > 0.0) {
            return 0..0;
        }
        // `as`: saturates, and the bounds are clamped to the frames known.
        let index = |t: f64| (((t - zero) / step).ceil().max(0.0) as usize).min(self.frames);
        let (a, b) = (index(from), index(to));
        a..b.max(a)
    }

    /// Frame `i` of the coarse spectrum: per band, low to high, the mean power of its hops in
    /// dBFS, -120 for silence.
    pub fn spectrum(&self, i: usize) -> Option<[f32; SPECTRUM_BANDS]> {
        let per = BLOCK_HOPS / SPECTRUM_HOPS;
        let frame = self.spectra.get(i / per)?.get(i % per)?;
        Some(frame.map(code_db))
    }

    /// Hop `i`, counted from the file's start.
    pub fn get(&self, i: usize) -> Option<Hop> {
        let block = self.blocks.get(i / BLOCK_HOPS)?;
        block.get(i % BLOCK_HOPS).copied()
    }

    /// Seconds into the file up to which [`Envelopes::read`] knows every value; infinite once
    /// complete.
    pub fn known_until(&self) -> f64 {
        if self.complete {
            return f64::INFINITY;
        }
        // Hop n ends at (n + 1) hops; the later of the two lags decides.
        (self.len as f64 - LEVEL_LAG_HOPS.max(ONSET_LAG_HOPS).max(KICK_LAG_HOPS)) * self.hop_seconds()
    }

    /// The last hop whose moment (its end, `lag` hops back) is at or before `t`; negative
    /// before the first.
    fn index(&self, t: f64, lag: f64) -> i64 {
        let h = self.hop_seconds();
        if !(t.is_finite() && h > 0.0) {
            return -1;
        }
        // `as`: a hop count, far inside i64 for any sound of up to six hours.
        (t / h + lag).floor() as i64 - 1
    }

    /// Calls `f` with every hop whose moment (`lag` hops before its end) lies in `from..=to`
    /// (seconds, `from` excluded), in a file that repeats every `length` seconds when given.
    /// Returns how many there were.
    fn each(&self, from: f64, to: f64, lag: f64, length: Option<f64>, f: &mut dyn FnMut(&Hop)) -> usize {
        let mut n = 0;
        let mut run = |a: i64, b: i64, f: &mut dyn FnMut(&Hop)| {
            // Hops a + 1 ..= b, within those known.
            let first = usize::try_from(a.saturating_add(1).max(0)).unwrap_or(0);
            let last = usize::try_from(b.max(-1).saturating_add(1)).unwrap_or(0).min(self.len);
            for i in first..last {
                if let Some(h) = self.get(i) {
                    f(&h);
                    n += 1;
                }
            }
        };
        match length.filter(|l| l.is_finite() && *l > 0.0) {
            None => run(self.index(from, lag), self.index(to, lag), f),
            Some(l) => {
                let (pf, pt) = ((from / l).floor(), (to / l).floor());
                let (wf, wt) = (from - pf * l, to - pt * l);
                if pf == pt {
                    run(self.index(wf, lag), self.index(wt, lag), f);
                } else {
                    // Across the end: the rest of one pass, then the start of the next.
                    // `as`: a hop count.
                    run(self.index(wf, lag), self.len as i64 - 1, f);
                    run(-1, self.index(wt, lag), f);
                }
            }
        }
        n
    }

    /// The envelopes at `to` seconds into the file, as the live analysis reports them
    /// [`LEVEL_LAG_HOPS`] (levels), [`ONSET_LAG_HOPS`] (onsets) and [`KICK_LAG_HOPS`] (kicks)
    /// later. With `from`, a
    /// reader that moved on from there since its last reading (forward, by at most two
    /// seconds): the levels are the mean of the hops in between, and an onset or kick in
    /// between counts, as in the live analysis' block since its last call. `length` is the
    /// file's length when it loops: then times count on from its start in every pass. A
    /// moment not analysed yet reads 0 and not `known`.
    pub fn read(&self, from: Option<f64>, to: f64, length: Option<f64>) -> Reading {
        let length = length.filter(|l| l.is_finite() && *l > 0.0);
        let here = match length {
            Some(l) => to.rem_euclid(l),
            None => to,
        };
        if !to.is_finite() || here > self.known_until() {
            return Reading::default();
        }
        let from = from.filter(|f| f.is_finite() && *f < to && to - *f <= MAX_READ_SECONDS);
        let mut sum = [0.0; 4];
        let mut add = |h: &Hop| {
            for (s, v) in sum.iter_mut().zip(h.levels()) {
                *s += v;
            }
        };
        let mut count = from.map_or(0, |f| self.each(f, to, LEVEL_LAG_HOPS, length, &mut add));
        if count == 0 {
            // No hop ended in between (or no step): the newest one.
            let i = self.index(here, LEVEL_LAG_HOPS);
            if let Some(h) = usize::try_from(i).ok().and_then(|i| self.get(i)) {
                add(&h);
                count = 1;
            }
        }
        let (mut onset, mut kick) = (false, false);
        if let Some(f) = from {
            self.each(f, to, ONSET_LAG_HOPS, length, &mut |h| onset |= h.onset());
            self.each(f, to, KICK_LAG_HOPS, length, &mut |h| kick |= h.kick());
        }
        let k = count.max(1) as f64;
        let [level, low, mid, high] = sum.map(|s| s / k);
        Reading { level, low, mid, high, onset, kick, known: true }
    }

    /// The mean power of level, low, mid and high from `from` to `to` seconds into the file
    /// (by the middles of the frames), in dBFS before normalising: features of a beat or a
    /// bar that stay comparable across the whole file. -120 for silence; `None` where no hop
    /// is analysed.
    pub fn energy_db(&self, from: f64, to: f64) -> Option<[f64; 4]> {
        let mut sum = [0.0; 4];
        let n = self.each(from, to, LEVEL_LAG_HOPS, None, &mut |h| {
            for (s, p) in sum.iter_mut().zip(h.power()) {
                *s += p;
            }
        });
        if n == 0 {
            return None;
        }
        Some(sum.map(|s| {
            let p = s / n as f64;
            if p > 0.0 { (10.0 * p.log10()).max(RAW_FLOOR_DB) } else { RAW_FLOOR_DB }
        }))
    }
}

/// The settings the envelopes are measured with: `chan.beat_analysis`'s defaults, with
/// [`SPECTRUM_BANDS`] spectrum bands.
fn settings() -> Settings {
    Settings { bands: SPECTRUM_BANDS, ..Settings::default() }
}

/// Builds the envelopes from decoded samples pushed in blocks of any length. Splitting the sound
/// into other blocks gives the same envelopes.
pub struct EnvelopeBuilder {
    channels: usize,
    from: f64,
    received: u64,
    carry: Vec<f32>,
    mono: Vec<f32>,
    resample: ToRate,
    /// Samples at [`ENVELOPE_RATE`] not yet a whole hop.
    pending: Vec<f32>,
    analyzer: Analyzer,
    settings: Settings,
    blocks: Vec<Arc<[Hop]>>,
    current: Vec<Hop>,
    /// The coarse spectrum: the power summed over the hops of the frame being built, and how
    /// many there are so far.
    spectrum_sum: [f64; SPECTRUM_BANDS],
    spectrum_hops: usize,
    spectra: Vec<Arc<[Frame]>>,
    spectrum_current: Vec<Frame>,
}

impl EnvelopeBuilder {
    /// A builder for `channels` channels at `rate` samples per second.
    pub fn new(rate: f64, channels: usize) -> Result<Self, BeatError> {
        if !(rate.is_finite() && (MIN_RATE..=MAX_RATE).contains(&rate)) {
            return Err(BeatError::Input(format!(
                "a sample rate of {rate} Hz cannot be analysed (from {MIN_RATE} to {MAX_RATE})"
            )));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(BeatError::Input(format!(
                "{channels} channels cannot be analysed (from 1 to {MAX_CHANNELS})"
            )));
        }
        let analyzer = Analyzer::new(ENVELOPE_RATE).map_err(BeatError::Input)?;
        Ok(EnvelopeBuilder {
            channels,
            from: rate,
            received: 0,
            carry: Vec::new(),
            mono: Vec::new(),
            resample: ToRate::new(rate, ENVELOPE_RATE),
            pending: Vec::new(),
            current: Vec::with_capacity(BLOCK_HOPS),
            analyzer,
            settings: settings(),
            blocks: Vec::new(),
            spectrum_sum: [0.0; SPECTRUM_BANDS],
            spectrum_hops: 0,
            spectra: Vec::new(),
            spectrum_current: Vec::with_capacity(BLOCK_HOPS / SPECTRUM_HOPS),
        })
    }

    /// Adds interleaved samples.
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        mix_down(self.channels, &mut self.carry, samples, &mut self.mono);
        self.received += self.mono.len() as u64;
        let mut out = std::mem::take(&mut self.pending);
        self.resample.push(&self.mono, &mut out);
        self.pending = out;
        self.hops();
    }

    /// Seconds of input taken so far.
    pub fn seconds_in(&self) -> f64 {
        self.received as f64 / self.from.max(1.0)
    }

    /// Hops done so far.
    pub fn len(&self) -> usize {
        self.blocks.len() * BLOCK_HOPS + self.current.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Full blocks done so far.
    pub fn blocks(&self) -> usize {
        self.blocks.len()
    }

    /// The envelopes of the full blocks so far, sharing them.
    pub fn snapshot(&self) -> Envelopes {
        Envelopes::new(ENVELOPE_RATE, self.analyzer.hop(), self.blocks.clone(), self.spectra.clone(), false)
    }

    /// Ends the sound. A last piece shorter than a hop is not analysed, as live.
    pub fn finish(mut self) -> Envelopes {
        let mut out = std::mem::take(&mut self.pending);
        self.resample.finish(&mut out);
        self.pending = out;
        self.hops();
        if self.spectrum_hops > 0 {
            self.end_frame();
        }
        if !self.current.is_empty() {
            self.blocks.push(Arc::from(std::mem::take(&mut self.current)));
        }
        if !self.spectrum_current.is_empty() {
            self.spectra.push(Arc::from(std::mem::take(&mut self.spectrum_current)));
        }
        Envelopes::new(ENVELOPE_RATE, self.analyzer.hop(), self.blocks, self.spectra, true)
    }

    /// Ends the frame of the coarse spectrum being built.
    fn end_frame(&mut self) {
        let n = self.spectrum_hops.max(1) as f64;
        let frame = self.spectrum_sum.map(|s| power_code(s / n));
        self.spectrum_current.push(frame);
        self.spectrum_sum = [0.0; SPECTRUM_BANDS];
        self.spectrum_hops = 0;
    }

    /// Analyses the whole hops waiting.
    fn hops(&mut self) {
        let hop = self.analyzer.hop();
        let mut done = 0;
        while let Some(block) = self.pending.get(done..done + hop) {
            let f = self.analyzer.process(block, &self.settings);
            let raw = self.analyzer.raw_levels();
            self.current.push(Hop::new([f.level, f.low, f.mid, f.high], raw, f.onset, f.kick));
            for (s, a) in self.spectrum_sum.iter_mut().zip(self.analyzer.raw_spectrum()) {
                if a.is_finite() {
                    *s += a * a;
                }
            }
            self.spectrum_hops += 1;
            if self.spectrum_hops == SPECTRUM_HOPS {
                self.end_frame();
            }
            if self.current.len() == BLOCK_HOPS {
                let full = std::mem::replace(&mut self.current, Vec::with_capacity(BLOCK_HOPS));
                self.blocks.push(Arc::from(full));
                // BLOCK_HOPS is a multiple of SPECTRUM_HOPS: the block's frames are done too.
                let frames = std::mem::replace(
                    &mut self.spectrum_current,
                    Vec::with_capacity(BLOCK_HOPS / SPECTRUM_HOPS),
                );
                self.spectra.push(Arc::from(frames));
            }
            done += hop;
        }
        self.pending.drain(..done.min(self.pending.len()));
    }
}

/// The envelopes of samples in memory, interleaved, `channels` channels at `rate`.
pub fn envelopes_of(samples: &[f32], channels: usize, rate: f64) -> Result<Envelopes, BeatError> {
    let mut b = EnvelopeBuilder::new(rate, channels)?;
    b.push_interleaved(samples);
    Ok(b.finish())
}

/// The envelopes of a sound file. With `cache_dir`, a result for this file (its size and
/// modification time) is read from there instead, and a new result is written there; a cache
/// entry that cannot be read is computed again. `cancel` is checked for every decoded block.
/// `progress` hears the share done (0 to 1), `partial` the envelopes so far after every
/// [`BLOCK_HOPS`] hops. Call it from a thread of its own: it blocks until the job is done.
pub fn analyze_envelopes(
    path: &Path,
    cache_dir: Option<&Path>,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(f64),
    partial: &mut dyn FnMut(&Envelopes),
) -> Result<Envelopes, BeatError> {
    let key = cache_dir.and_then(|dir| Key::of(path, dir));
    if let Some(key) = &key
        && let Some(e) = load(key)
    {
        progress(1.0);
        return Ok(e);
    }
    let mut builder: Option<EnvelopeBuilder> = None;
    let mut failure: Option<BeatError> = None;
    let mut length: Option<f64> = None;
    let mut last_report = -1.0f64;
    let mut sink = |info: &file::Info, block: &[f32]| -> bool {
        if cancelled(cancel) {
            failure = Some(BeatError::Cancelled);
            return false;
        }
        let b = match &mut builder {
            Some(b) => b,
            None => match EnvelopeBuilder::new(f64::from(info.rate), info.channels) {
                Ok(b) => {
                    length = info.seconds();
                    builder.insert(b)
                }
                Err(e) => {
                    failure = Some(e);
                    return false;
                }
            },
        };
        if b.seconds_in() > MAX_SECONDS {
            failure = Some(BeatError::Input(format!(
                "envelopes are measured in sounds of up to {} hours, and this one is longer",
                MAX_SECONDS / 3600.0
            )));
            return false;
        }
        let blocks = b.blocks();
        b.push_interleaved(block);
        if b.blocks() > blocks {
            partial(&b.snapshot());
        }
        let done = b.seconds_in();
        if done - last_report >= 1.0 {
            last_report = done;
            if let Some(l) = length.filter(|l| *l > 0.0) {
                progress((done / l).clamp(0.0, 0.99));
            }
        }
        true
    };
    let decoded = file::decode_each(path, &mut sink);
    if let Some(e) = failure {
        return Err(e);
    }
    decoded.map_err(BeatError::Input)?;
    let Some(b) = builder else {
        return Err(BeatError::Input("the file holds no sound".into()));
    };
    let env = b.finish();
    progress(1.0);
    if let Some(key) = &key
        && let Err(e) = store(key, &env)
    {
        log::warn!("envelope cache: {e}");
    }
    Ok(env)
}

/// Where the envelopes of one sound file are kept, and the header they must carry.
struct Key {
    file: PathBuf,
    header: String,
}

impl Key {
    fn of(sound: &Path, dir: &Path) -> Option<Key> {
        let (file, size, mtime) = cache::place(sound, dir, "envelopes")?;
        let analysis = fnv1a(format!("{:?}", settings()).as_bytes());
        // `as`: the rate is a whole number of samples per second.
        let rate = ENVELOPE_RATE as u64;
        Some(Key { file, header: format!("{MAGIC} {VERSION} {size} {mtime} {rate} {analysis:016x}\n") })
    }
}

/// The most hops a cache file may hold: six hours at the smallest hop.
fn max_hops() -> usize {
    // `as`: a few million.
    (MAX_SECONDS * ENVELOPE_RATE / 64.0).ceil() as usize
}

fn load(key: &Key) -> Option<Envelopes> {
    let file = std::fs::File::open(&key.file).ok()?;
    let limit =
        key.header.len() + 8 + 8 + max_hops() * HOP_BYTES + frames_of(max_hops()) * SPECTRUM_BANDS + 8 + 4;
    let mut bytes = Vec::new();
    // `as`: a limit of some tens of MB.
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > limit {
        return None;
    }
    parse(&bytes, &key.header)
}

/// Frames of the coarse spectrum for `hops` hops.
fn frames_of(hops: usize) -> usize {
    hops.div_ceil(SPECTRUM_HOPS)
}

/// Header line, hop size and count (u64 each, little endian), the hops, the frames of the
/// coarse spectrum ([`frames_of`] the hop count), an FNV-1a hash of the hops' and frames'
/// bytes, `end\n`.
fn format(env: &Envelopes, header: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(header.len() + 28 + env.len() * HOP_BYTES + env.frames * SPECTRUM_BANDS);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&(env.hop as u64).to_le_bytes());
    out.extend_from_slice(&(env.len as u64).to_le_bytes());
    let start = out.len();
    for block in &env.blocks {
        for h in block.iter() {
            h.write(&mut out);
        }
    }
    for block in &env.spectra {
        for f in block.iter() {
            out.extend_from_slice(f);
        }
    }
    let hash = fnv1a(out.get(start..).unwrap_or(&[]));
    out.extend_from_slice(&hash.to_le_bytes());
    out.extend_from_slice(b"end\n");
    out
}

fn parse(bytes: &[u8], header: &str) -> Option<Envelopes> {
    let rest = bytes.strip_prefix(header.as_bytes())?;
    let (hop, rest) = rest.split_first_chunk::<8>()?;
    let (count, rest) = rest.split_first_chunk::<8>()?;
    let hop = usize::try_from(u64::from_le_bytes(*hop)).ok()?;
    let count = usize::try_from(u64::from_le_bytes(*count)).ok()?;
    if !(64..=8192).contains(&hop) || !hop.is_power_of_two() || count > max_hops() {
        return None;
    }
    let body = count.checked_mul(HOP_BYTES)?;
    let spectrum = frames_of(count).checked_mul(SPECTRUM_BANDS)?;
    if rest.len() != body + spectrum + 8 + 4 {
        return None;
    }
    let (data, tail) = rest.split_at(body + spectrum);
    let (hash, end) = tail.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*hash) != fnv1a(data) || end != b"end\n" {
        return None;
    }
    let (hops, frames) = data.split_at(body);
    let mut blocks = Vec::with_capacity(count.div_ceil(BLOCK_HOPS));
    for chunk in hops.chunks(BLOCK_HOPS * HOP_BYTES) {
        let block: Option<Vec<Hop>> = chunk.as_chunks::<HOP_BYTES>().0.iter().map(Hop::read).collect();
        blocks.push(Arc::from(block?));
    }
    let spectra = frames
        .chunks(BLOCK_HOPS / SPECTRUM_HOPS * SPECTRUM_BANDS)
        .map(|chunk| Arc::from(chunk.as_chunks::<SPECTRUM_BANDS>().0.to_vec()))
        .collect();
    Some(Envelopes::new(ENVELOPE_RATE, hop, blocks, spectra, true))
}

fn store(key: &Key, env: &Envelopes) -> Result<(), String> {
    cache::write_whole(&key.file, &format(env, &key.header))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 48 kHz test signal: a 60 Hz kick every half second, a soft noise bed, a tone burst
    /// from 3 to 4 s.
    fn signal(seconds: f64) -> Vec<f32> {
        let rate = ENVELOPE_RATE;
        let n = (seconds * rate) as usize;
        let mut seed = 1u32;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) * 0.02;
                let k = t % 0.5;
                let kick = 0.8 * (-k / 0.08).exp() * (std::f64::consts::TAU * 60.0 * k).sin();
                let tone = if (3.0..4.0).contains(&t) {
                    0.3 * (std::f64::consts::TAU * 1000.0 * t).sin()
                } else {
                    0.0
                };
                (noise + kick + tone) as f32
            })
            .collect()
    }

    #[test]
    fn the_hops_are_the_live_analysis_fed_hop_by_hop() {
        let x = signal(8.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        let mut a = Analyzer::new(ENVELOPE_RATE).unwrap();
        let s = settings();
        let hop = a.hop();
        assert_eq!(env.hop(), hop);
        assert_eq!(env.len(), x.len() / hop);
        for (i, block) in x.chunks_exact(hop).enumerate() {
            let f = a.process(block, &s);
            let h = env.get(i).unwrap();
            for (got, want) in h.levels().iter().zip([f.level, f.low, f.mid, f.high]) {
                assert!((got - want).abs() <= 0.5 / 65_535.0 + 1e-12, "hop {i}: {got} vs {want}");
            }
            assert_eq!((h.onset(), h.kick()), (f.onset, f.kick), "hop {i}");
        }
        // Blocks of another length give the same.
        let mut b = EnvelopeBuilder::new(ENVELOPE_RATE, 1).unwrap();
        for c in x.chunks(777) {
            b.push_interleaved(c);
        }
        assert_eq!(b.finish(), env);
    }

    #[test]
    fn readings_take_out_the_lag() {
        let x = signal(8.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        let h = env.hop_seconds();
        // Kicks read on their attack: every reading window holding a kick start is within the
        // onset lag's rounding of it.
        let mut kicks = Vec::new();
        let step = 1.0 / 60.0;
        let mut t = 0.0;
        while t < 7.9 {
            if env.read(Some(t), t + step, None).kick {
                kicks.push(t + step);
            }
            t += step;
        }
        assert!((14..=16).contains(&kicks.len()), "{kicks:?}");
        for k in &kicks {
            let off = (k / 0.5).round() * 0.5;
            assert!(k - off >= -1e-9 && k - off <= step + 2.0 * h, "kick read at {k}");
        }
        // The 1 kHz burst from 3 to 4 s: its raw mid power, read by the frames' middles, is half
        // its full power where the burst begins and ends.
        let mid = |n: usize| env.get(n).unwrap().power()[2];
        let full = mid((3.5 / h) as usize);
        let crossing = |rising: bool| {
            let n = (1..env.len())
                .find(|n| {
                    let (a, b) = (mid(n - 1), mid(*n));
                    if rising { a < full / 2.0 && b >= full / 2.0 } else { a >= full / 2.0 && b < full / 2.0 }
                })
                .unwrap();
            // Hop n's middle: its end, two hops back.
            (n as f64 + 1.0 - LEVEL_LAG_HOPS) * h
        };
        assert!((crossing(true) - 3.0).abs() <= h, "rises at {}", crossing(true));
        assert!((crossing(false) - 4.0).abs() <= h, "falls at {}", crossing(false));
        assert!(env.read(None, 3.5, None).mid > 0.5);
        // Before the file and beyond the analysed part.
        assert!(env.read(None, -1.0, None).known);
        assert_eq!(env.read(None, -1.0, None).level, 0.0);
        let partial = Envelopes::new(env.rate, env.hop, env.blocks[..0].to_vec(), Vec::new(), false);
        assert_eq!(partial.read(None, 1.0, None), Reading::default());
    }

    #[test]
    fn a_looped_reading_runs_on_across_the_end() {
        let x = signal(2.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        let length = 2.0;
        // A kick at 0 s of the second pass, read by the step from 1.99 to 2.01.
        assert!(env.read(Some(1.99), 2.01, Some(length)).kick);
        assert!(!env.read(Some(1.95), 1.97, Some(length)).kick);
        // The same moment in another pass reads the same.
        let a = env.read(None, 0.7, Some(length));
        let b = env.read(None, 4.7, Some(length));
        assert_eq!(a, b);
    }

    #[test]
    fn the_coarse_spectrum_holds_the_tone_in_its_band() {
        let x = signal(8.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        assert_eq!(env.spectrum_len(), env.len().div_ceil(SPECTRUM_HOPS));
        assert!((env.spectrum_seconds() - 4.0 * 512.0 / 48_000.0).abs() < 1e-12);
        // The band holding 1 kHz: log-spaced from 30 Hz to 16 kHz.
        let ratio: f64 = 16_000.0 / 30.0;
        let band = ((1000.0f64 / 30.0).ln() / ratio.ln() * SPECTRUM_BANDS as f64) as usize;
        let mean = |from: f64, to: f64, k: usize| {
            let r = env.spectrum_range(from, to);
            let n = r.len() as f64;
            r.map(|i| f64::from(env.spectrum(i).unwrap()[k])).sum::<f64>() / n
        };
        let (burst, before) = (mean(3.1, 3.9, band), mean(2.1, 2.9, band));
        assert!(burst > before + 20.0, "{burst} vs {before}");
        // A band far above stays.
        let top = SPECTRUM_BANDS - 2;
        assert!((mean(3.1, 3.9, top) - mean(2.1, 2.9, top)).abs() < 3.0);
        // The frames' moments: the burst's frames lie between 3 and 4 s.
        let r = env.spectrum_range(3.0, 4.0);
        assert!(env.spectrum_time(r.start) >= 3.0 && env.spectrum_time(r.end - 1) < 4.0);
        assert!(env.spectrum_time(r.start - 1) < 3.0 && env.spectrum_time(r.end) >= 4.0);
        let rise = (0..env.spectrum_len())
            .find(|i| f64::from(env.spectrum(*i).unwrap()[band]) > before + 10.0)
            .unwrap();
        assert!(
            (env.spectrum_time(rise) - 3.0).abs() <= env.spectrum_seconds(),
            "{}",
            env.spectrum_time(rise)
        );
        // Out of range and no numbers.
        assert!(env.spectrum(env.spectrum_len()).is_none());
        assert_eq!(env.spectrum_range(f64::NAN, 1.0), 0..0);
        assert_eq!(env.spectrum_range(100.0, 200.0).len(), 0);
        assert_eq!(env.spectrum_range(-5.0, 1e300).len(), env.spectrum_len());
        // Partial results carry the frames of their blocks.
        // (The test signal is the same from its start, however long.)
        let mut b = EnvelopeBuilder::new(ENVELOPE_RATE, 1).unwrap();
        b.push_interleaved(&signal(45.0));
        let part = b.snapshot();
        assert_eq!(part.len(), BLOCK_HOPS);
        assert_eq!(part.spectrum_len(), BLOCK_HOPS / SPECTRUM_HOPS);
        assert_eq!(part.spectrum(5), env.spectrum(5));
    }

    #[test]
    fn energy_spans_stay_comparable() {
        let x = signal(8.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        let burst = env.energy_db(3.0, 4.0).unwrap();
        let before = env.energy_db(2.0, 3.0).unwrap();
        // The 1 kHz tone lifts the mid band by many dB, the low band (the kicks) stays.
        assert!(burst[2] > before[2] + 20.0, "{burst:?} vs {before:?}");
        assert!((burst[1] - before[1]).abs() < 1.0, "{burst:?} vs {before:?}");
        assert!(env.energy_db(100.0, 101.0).is_none());
    }

    #[test]
    fn the_cache_round_trips_and_rejects_damage() {
        let x = signal(1.0);
        let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
        let header = "penumbra-envelopes 1 10 20 48000 00\n";
        let bytes = format(&env, header);
        assert_eq!(parse(&bytes, header), Some(env.clone()));
        assert!(parse(&bytes, "penumbra-envelopes 1 10 21 48000 00\n").is_none());
        for cut in 0..bytes.len() {
            assert!(parse(&bytes[..cut], header).is_none(), "cut at {cut}");
        }
        for i in header.len()..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x10;
            assert!(parse(&b, header).is_none(), "flip at {i}");
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(parse(&long, header).is_none());
    }

    #[test]
    fn a_file_is_analysed_once_and_cached() {
        let dir = std::env::temp_dir().join(format!("penumbra-envelopes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let x: Vec<f64> = signal(3.0).iter().map(|v| f64::from(*v)).collect();
        // Stereo at 44.1 kHz: resampled to 48 kHz on the way.
        let stereo: Vec<f64> = x.iter().step_by(1).flat_map(|v| [*v, *v]).collect();
        let sound = dir.join("s.wav");
        std::fs::write(&sound, file::wav_bytes(44_100, 2, file::WavFormat::Float32, &stereo)).unwrap();
        let cache = dir.join("cache");
        let mut last = 0.0;
        let env = analyze_envelopes(&sound, Some(&cache), None, &mut |p| last = p, &mut |_| {}).unwrap();
        assert_eq!(last, 1.0);
        assert!(env.complete());
        // 3 s of 48 kHz samples played at 44.1 kHz last 3.27 s.
        let seconds = env.len() as f64 * env.hop_seconds();
        assert!((seconds - 3.0 * 48_000.0 / 44_100.0).abs() < 0.02, "{seconds}");
        let start = std::time::Instant::now();
        let again = analyze_envelopes(&sound, Some(&cache), None, &mut |_| {}, &mut |_| {}).unwrap();
        assert_eq!(again, env);
        assert!(start.elapsed().as_millis() < 200, "{:?}", start.elapsed());
        let stop = AtomicBool::new(true);
        for entry in std::fs::read_dir(&cache).unwrap() {
            std::fs::write(entry.unwrap().path(), "garbage").unwrap();
        }
        let r = analyze_envelopes(&sound, Some(&cache), Some(&stop), &mut |_| {}, &mut |_| {});
        assert_eq!(r, Err(BeatError::Cancelled));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn the_example_track_reads_its_hits_on_the_beat() {
        // examples/beat-grid/music.wav: a beat every 10 670 samples at 22 050 Hz from 0 s, a kick
        // on each outside the breakdown (bars 17 to 24), hats and snare between.
        let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
        let env = analyze_envelopes(&music, None, None, &mut |_| {}, &mut |_| {}).unwrap();
        let h = env.hop_seconds();
        let beat = 10_670.0 / 22_050.0;
        for (lag, kick) in [(ONSET_LAG_HOPS, false), (KICK_LAG_HOPS, true)] {
            // Each hit's moment, the lag taken out, against the nearest beat, in hops.
            let mut d: Vec<f64> = (0..env.len())
                .filter_map(|n| {
                    let hop = env.get(n).unwrap();
                    let hit = if kick { hop.kick() } else { hop.onset() };
                    let t = (n as f64 + 1.0 - lag) * h;
                    let off = t - (t / beat).round() * beat;
                    (hit && off.abs() < 0.1).then_some(off / h)
                })
                .collect();
            d.sort_by(f64::total_cmp);
            assert!(d.len() > 90, "{kick}: {} hits", d.len());
            let median = d[d.len() / 2];
            assert!(median.abs() <= 0.5, "kick {kick}: median {median} hops off the beat");
        }
    }
}
