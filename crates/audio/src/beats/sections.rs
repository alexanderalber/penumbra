//! The structure of a sound file: where its tracks and the sections within them begin, which
//! sections sound alike, and where a moment stands among them.
//!
//! - **Bars** are the grid's voted bars ([`BeatGrid::downbeats`]). Each bar gets features from
//!   the envelopes' coarse spectrum ([`Envelopes::spectrum`]): the mean log spectrum, its
//!   cepstrum (a DCT of the log spectrum, 20 coefficients) and the spread of the first 8 over
//!   the bar, the loudness, the spectral flux and the low end. Each feature is standardised
//!   over the whole file; groups of features get fixed weights.
//! - **Boundaries:** the bars' cosine self-similarity, scanned with Foote's checkerboard kernel
//!   (Gaussian taper) at two widths: [`COARSE_KERNEL`] bars on either side for track changes,
//!   [`FINE_KERNEL`] for sections. Peaks of the novelty become boundaries: the highest of their
//!   neighbourhood, above the local mean by [`PEAK_LIFT`] standard deviations, at least the
//!   kernel's width apart. Every track boundary is a section boundary; a section boundary within
//!   [`MERGE_BARS`] of a track boundary is dropped. A section next to a track boundary, at most
//!   [`BLEND_BARS`] long, whose sound moves from the section before it to the one after it is
//!   the blend of two tracks: its other boundary is dropped too, and the blend becomes the
//!   transition zone of the track boundary.
//! - **Labels:** sections are clustered by their mean features (Ward), into [`KINDS`] kinds,
//!   merging further while two groups are closer than [`SAME_KIND`]. The same label means a
//!   similar sound anywhere in the file. A second, local letter tells repeats within one track
//!   (average linkage on cosine distance, below [`LETTER_DISTANCE`]).
//! - **Phrases:** [`PHRASE_BARS`] bars each. Per track, the phrase grid starts on the bar (from
//!   the track's first, modulo the phrase) where the section novelty is highest summed.
//! - **Transitions:** a mix blends one track into the next over many bars, so a boundary is a
//!   zone. Each bar near a boundary is placed on the line from the mean features of the section
//!   before to those after (0 before, 1 after), smoothed over 3 bars. The zone runs from the last
//!   bar before the boundary still at or below [`ZONE_LOW`] to the first bar after it at or
//!   above [`ZONE_HIGH`], within [`ZONE_REACH`] bars and half of either section; a cut has a
//!   zone of no width. A blend of two tracks is the zone of its track boundary. The labels and
//!   letters compare the sections' bars outside the zones.
//!
//! **Only for whole files:** the standardising, the peak threshold and the clustering take the
//! whole file, so the sections of a part of it would change everywhere once more is known.
//! Find them once the grid and the envelopes are complete.
//!
//! **Cost:** a two-hour mix (3600 bars, 190 sections) takes about 0.1 s and a few MB.

use super::{BeatGrid, Envelopes, SPECTRUM_BANDS};

/// Bars on either side of the novelty kernel for track changes.
pub const COARSE_KERNEL: usize = 48;
/// Bars on either side of the novelty kernel for sections.
pub const FINE_KERNEL: usize = 8;
/// Bars between two track boundaries at least.
pub const COARSE_GAP: usize = 48;
/// Bars between two section boundaries at least.
pub const FINE_GAP: usize = 8;
/// How far a novelty peak must rise above the mean around it, in standard deviations of the
/// whole novelty.
pub const PEAK_LIFT: f64 = 0.5;
/// A section boundary this close (bars) to a track boundary is the track boundary.
pub const MERGE_BARS: usize = 4;
/// Kinds of section the clustering ends with at most.
pub const KINDS: usize = 10;
/// Groups of sections whose Ward distance (in standardised feature units) is below this share
/// one label even when that leaves fewer than [`KINDS`] kinds. Sections that sound the same get
/// the same label.
pub const SAME_KIND: f64 = 0.25;
/// Sections of one track closer than this (cosine distance, average linkage) share a letter.
pub const LETTER_DISTANCE: f64 = 0.35;
/// Bars per phrase.
pub const PHRASE_BARS: usize = 8;
/// Bars over which `build` ramps up into a boundary (fewer in a shorter section).
pub const BUILD_BARS: usize = 8;
/// The longest section next to a track boundary that can be the blend of the two tracks, bars.
pub const BLEND_BARS: usize = 16;
/// How far (0 to 1 of the way from the section before to the one after) a blend's last third
/// must have moved beyond its first.
pub const BLEND_RISE: f64 = 0.2;
/// Bars on either side of a boundary whose features the transition zone is found from.
pub const ZONE_REACH: usize = 32;
/// The place on the line between two sections (0 the one before, 1 the one after) where a
/// transition zone begins and ends.
pub const ZONE_LOW: f64 = 0.25;
pub const ZONE_HIGH: f64 = 0.75;
/// The most bars sections are found for: six hours at 200 beats per minute in 4/4 are 18 000.
pub const MAX_BARS: usize = 20_000;

/// Cepstral coefficients per bar, and how many of them also give their spread.
const CEPSTRUM: usize = 20;
const CEPSTRUM_SPREAD: usize = 8;
/// Bands of the coarse spectrum that make the low end (30 Hz to about 260 Hz).
const LOW_BANDS: usize = 11;
/// Feature groups: log spectrum, cepstrum, cepstrum spread, loudness, flux, low end.
const GROUPS: [usize; 6] = [SPECTRUM_BANDS, CEPSTRUM, CEPSTRUM_SPREAD, 1, 1, 1];
/// Group weights for the boundaries and the letters.
const SIMILARITY_WEIGHTS: [f64; 6] = [0.7, 1.0, 0.4, 0.4, 0.3, 0.4];
/// Group weights for the labels: more of the energy, which tells a breakdown from a drop.
const KIND_WEIGHTS: [f64; 6] = [0.7, 1.0, 0.5, 0.6, 0.5, 0.6];

/// One section.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Seconds into the file: the start of its first bar and the end of its last.
    pub start: f64,
    pub end: f64,
    /// Its first bar (an index into [`Sections::bars`]) and its length in bars.
    pub first_bar: usize,
    pub bars: usize,
    /// Its kind, numbered by first appearance: sections of one kind sound alike, anywhere in
    /// the file.
    pub label: usize,
    /// Its letter within its track (0 is A), numbered by first appearance in the track:
    /// repeats within a track share it.
    pub letter: usize,
    /// The track it belongs to, counted from 0.
    pub track: usize,
    /// It begins a track.
    pub track_start: bool,
    /// The novelty at its start, 0 to 1: how sharply the sound changes there (the track novelty
    /// where a track begins, the section novelty elsewhere).
    pub strength: f64,
    /// A number fixed per label (0 to 2^24 - 1, exact in an f32).
    pub seed: u32,
    /// The zone in which the section before blends into this one, seconds: from `zone.0` to
    /// `zone.1`, around `start`. Equal at a cut and for the first section.
    pub zone: (f64, f64),
}

/// Where a moment stands among the sections ([`Sections::at`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionPosition {
    pub section_index: usize,
    pub section_label: usize,
    /// The section's letter within its track (0 is A).
    pub section_letter: usize,
    pub track_index: usize,
    /// The bar within its phrase, 0 to [`PHRASE_BARS`] - 1, and how far through the phrase
    /// the moment is, 0 to 1.
    pub phrase_bar: usize,
    pub phrase_phase: f64,
    /// Bars (with the fraction of the current one) and seconds until the next section begins,
    /// or the file ends.
    pub bars_to_boundary: f64,
    pub seconds_to_boundary: f64,
    /// 0 to 1 over the last [`BUILD_BARS`] bars before the next boundary (over the whole
    /// section when it is shorter); 0 before.
    pub build: f64,
    /// The section's [`Section::seed`].
    pub section_seed: u32,
    /// 0 to 1 through a transition zone, 0 outside one; with the labels blended from and to.
    /// Outside a zone both are the section's label.
    pub transition: f64,
    pub transition_from: usize,
    pub transition_to: usize,
    /// The moment lies between the first bar's start and the file's end.
    pub inside: bool,
}

/// The sections of a sound file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sections {
    /// Bar starts, seconds, ascending.
    bars: Vec<f64>,
    /// The end of the last bar.
    end: f64,
    sections: Vec<Section>,
    /// Per track, the bar a phrase starts on.
    phrase_origins: Vec<usize>,
    /// Per bar, the novelty for tracks and for sections, 0 to 1.
    coarse: Vec<f64>,
    fine: Vec<f64>,
}

impl Sections {
    /// The sections of a piece with this grid and these envelopes, both complete. The file ends
    /// where the envelopes do. `Err` when there are no bars, more than [`MAX_BARS`], or no
    /// spectrum.
    pub fn of(grid: &BeatGrid, env: &Envelopes) -> Result<Sections, String> {
        let end = env.len() as f64 * env.hop_seconds();
        Sections::find(&grid.downbeats(), end, env)
    }

    /// The sections of a piece whose bars start at `bars` (seconds; non-numbers dropped, sorted)
    /// and whose last bar ends at `end` (or after one more bar of the last bar's length, when
    /// `end` is no later than the last start).
    pub fn find(bars: &[f64], end: f64, env: &Envelopes) -> Result<Sections, String> {
        let bars = clean_bars(bars, end)?;
        if env.spectrum_len() == 0 {
            return Err("the envelopes hold no spectrum".into());
        }
        let (starts, end) = bars;
        let features = bar_features(&starts, end, env);
        Ok(Sections::from_features(starts, end, &features))
    }

    /// The sections from per-bar features ([`GROUPS`] columns each, in that order).
    fn from_features(bars: Vec<f64>, end: f64, features: &[Vec<f64>]) -> Sections {
        let n = bars.len();
        let similar = standardise(features, &SIMILARITY_WEIGHTS);
        let kinds = standardise(features, &KIND_WEIGHTS);
        let units: Vec<Vec<f64>> = similar.iter().map(|x| unit(x)).collect();
        let band = similarity_band(&units, 2 * COARSE_KERNEL.max(FINE_KERNEL));
        let coarse = foote(&band, n, COARSE_KERNEL);
        let fine = foote(&band, n, FINE_KERNEL);
        let tracks = pick(&coarse, COARSE_KERNEL / 2, COARSE_GAP, PEAK_LIFT);
        let sections = pick(&fine, FINE_KERNEL / 2, FINE_GAP, PEAK_LIFT);
        let mut bounds: Vec<usize> = tracks.clone();
        for s in sections {
            if tracks.iter().all(|t| t.abs_diff(s) > MERGE_BARS) {
                bounds.push(s);
            }
        }
        bounds.push(0);
        bounds.push(n);
        bounds.sort_unstable();
        bounds.dedup();
        // A track change in a mix is often a blend of the two tracks: the section boundaries at
        // its edges belong to the change.
        let mut blends: Vec<(usize, (usize, usize))> = Vec::new();
        for &c in &tracks {
            let mut zone = (c, c);
            for side in [Side::Before, Side::After] {
                if let Some(f) = blend_edge(&similar, &bounds, c, side) {
                    bounds.retain(|b| *b != f);
                    zone = (zone.0.min(f), zone.1.max(f));
                }
            }
            if zone.1 > zone.0 {
                blends.push((c, zone));
            }
        }
        let spans: Vec<(usize, usize)> = bounds
            .windows(2)
            .filter_map(|w| Some((*w.first()?, *w.get(1)?)))
            .filter(|(a, b)| b > a)
            .collect();
        // The transition zone into each section, bars (none into the first).
        let zones: Vec<(usize, usize)> = spans
            .iter()
            .enumerate()
            .map(|(k, &(b, next))| {
                let Some(&(prev, _)) = k.checked_sub(1).and_then(|j| spans.get(j)) else { return (b, b) };
                match blends.iter().find(|(c, _)| *c == b) {
                    Some(&(_, z)) => z,
                    None => zone(&similar, prev, b, next),
                }
            })
            .collect();
        // The bars of each section outside the zones: its own sound.
        let cores: Vec<(usize, usize)> = spans
            .iter()
            .enumerate()
            .map(|(k, &(a, b))| {
                let from = zones.get(k).map_or(a, |z| z.1).max(a);
                let to = zones.get(k + 1).map_or(b, |z| z.0).min(b);
                if to > from { (from, to) } else { (a, b) }
            })
            .collect();

        // Kinds across the file.
        let means: Vec<Vec<f64>> = cores.iter().map(|&(a, b)| mean_rows(&kinds, a, b)).collect();
        let labels = first_appearance(&cut(&nn_chain(&means, Linkage::Ward), means.len(), KINDS, SAME_KIND));
        // Tracks and letters within them.
        let track_of: Vec<usize> =
            spans.iter().map(|&(a, _)| tracks.iter().filter(|t| **t <= a).count()).collect();
        let mut letters = vec![0usize; spans.len()];
        let track_count = track_of.last().map_or(0, |t| t + 1);
        let mut phrase_origins = Vec::with_capacity(track_count);
        for track in 0..track_count {
            let members: Vec<usize> = (0..spans.len()).filter(|i| track_of.get(*i) == Some(&track)).collect();
            let points: Vec<Vec<f64>> = members
                .iter()
                .filter_map(|i| cores.get(*i))
                .map(|&(a, b)| unit(&mean_rows(&similar, a, b)))
                .collect();
            let merges = nn_chain(&points, Linkage::Cosine);
            let local = first_appearance(&cut(&merges, points.len(), points.len(), LETTER_DISTANCE));
            for (i, l) in members.iter().zip(local) {
                if let Some(slot) = letters.get_mut(*i) {
                    *slot = l;
                }
            }
            let first = members.first().and_then(|i| spans.get(*i)).map_or(0, |s| s.0);
            let last = members.last().and_then(|i| spans.get(*i)).map_or(0, |s| s.1);
            phrase_origins.push(phrase_origin(&fine, first, last));
        }

        let time = |bar: usize| if bar >= n { end } else { bars.get(bar).copied().unwrap_or(end) };
        let out: Vec<Section> = spans
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| {
                let label = labels.get(i).copied().unwrap_or(0);
                let track = track_of.get(i).copied().unwrap_or(0);
                Section {
                    start: time(a),
                    end: time(b),
                    first_bar: a,
                    bars: b - a,
                    label,
                    letter: letters.get(i).copied().unwrap_or(0),
                    track,
                    track_start: i == 0 || track_of.get(i.wrapping_sub(1)) != Some(&track),
                    strength: if a == 0 {
                        0.0
                    } else if tracks.contains(&a) {
                        coarse.get(a).copied().unwrap_or(0.0)
                    } else {
                        fine.get(a).copied().unwrap_or(0.0)
                    },
                    seed: seed(label),
                    zone: zones.get(i).map_or((time(a), time(a)), |z| (time(z.0), time(z.1))),
                }
            })
            .collect();
        Sections { bars, end, sections: out, phrase_origins, coarse, fine }
    }

    /// The sections in order.
    pub fn list(&self) -> &[Section] {
        &self.sections
    }

    /// Start (s), end (s) and label of every section, for a table or a picture.
    pub fn spans(&self) -> Vec<(f64, f64, usize)> {
        self.sections.iter().map(|s| (s.start, s.end, s.label)).collect()
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// The bar starts the sections count in, seconds.
    pub fn bars(&self) -> &[f64] {
        &self.bars
    }

    /// The end of the last bar, seconds.
    pub fn end(&self) -> f64 {
        self.end
    }

    /// Tracks found.
    pub fn tracks(&self) -> usize {
        self.phrase_origins.len()
    }

    /// Kinds of section: one more than the highest label.
    pub fn kinds(&self) -> usize {
        self.sections.iter().map(|s| s.label + 1).max().unwrap_or(0)
    }

    /// Per bar, how sharply the sound changes there at the scale of tracks (0 to 1).
    pub fn track_novelty(&self) -> &[f64] {
        &self.coarse
    }

    /// Per bar, how sharply the sound changes there at the scale of sections (0 to 1).
    pub fn section_novelty(&self) -> &[f64] {
        &self.fine
    }

    /// Where `t` (seconds into the file) stands. Before the first bar it counts as in the first
    /// section, after the end as at the end of the last. `None` with no sections or a `t` that
    /// is no number.
    pub fn at(&self, t: f64) -> Option<SectionPosition> {
        if !t.is_finite() {
            return None;
        }
        let k = self.sections.partition_point(|s| s.start <= t).saturating_sub(1);
        let s = self.sections.get(k)?;
        let bar = self.bar_position(t);
        let next_bar = (s.first_bar + s.bars) as f64;
        let bars_to_boundary = (next_bar - bar).max(0.0);
        let ramp = BUILD_BARS.min(s.bars).max(1) as f64;
        let origin = self.phrase_origins.get(s.track).copied().unwrap_or(s.first_bar) as f64;
        let in_phrase = (bar - origin).rem_euclid(PHRASE_BARS as f64);
        // `as`: within 0..PHRASE_BARS.
        let phrase_bar = (in_phrase.floor() as usize).min(PHRASE_BARS - 1);
        let mut p = SectionPosition {
            section_index: k,
            section_label: s.label,
            section_letter: s.letter,
            track_index: s.track,
            phrase_bar,
            phrase_phase: in_phrase / PHRASE_BARS as f64,
            bars_to_boundary,
            seconds_to_boundary: (s.end - t).max(0.0),
            build: (1.0 - bars_to_boundary / ramp).clamp(0.0, 1.0),
            section_seed: s.seed,
            transition: 0.0,
            transition_from: s.label,
            transition_to: s.label,
            inside: self.bars.first().is_some_and(|b| t >= *b) && t < self.end,
        };
        // Still in the blend into this section, or already in the blend into the next.
        let blend = |into: &Section, from: &Section| {
            let (a, b) = into.zone;
            (b > a && t >= a && t < b).then(|| ((t - a) / (b - a), from.label, into.label))
        };
        let here = k.checked_sub(1).and_then(|j| self.sections.get(j)).and_then(|prev| blend(s, prev));
        let ahead = self.sections.get(k + 1).and_then(|next| blend(next, s));
        if let Some((v, from, to)) = here.or(ahead) {
            (p.transition, p.transition_from, p.transition_to) = (v.clamp(0.0, 1.0), from, to);
        }
        Some(p)
    }

    /// `t` in bars from the first bar's start, with the fraction of the bar; before the first
    /// and after the end at the length of the first and the last bar.
    fn bar_position(&self, t: f64) -> f64 {
        let n = self.bars.len();
        let start = |i: usize| if i >= n { self.end } else { self.bars.get(i).copied().unwrap_or(self.end) };
        let len = |i: usize| (start(i + 1) - start(i)).max(1e-6);
        let Some(&first) = self.bars.first() else { return 0.0 };
        if t < first {
            return (t - first) / len(0);
        }
        if t >= self.end {
            return n as f64 + (t - self.end) / len(n.saturating_sub(1));
        }
        let i = self.bars.partition_point(|b| *b <= t).saturating_sub(1);
        i as f64 + (t - start(i)) / len(i)
    }
}

/// Bar starts sorted, finite, without repeats, with the end of the last bar.
fn clean_bars(bars: &[f64], end: f64) -> Result<(Vec<f64>, f64), String> {
    let mut b: Vec<f64> = bars.iter().copied().filter(|t| t.is_finite()).collect();
    b.sort_by(f64::total_cmp);
    b.dedup();
    if b.len() > MAX_BARS {
        return Err(format!("{} bars are too many to find sections in (at most {MAX_BARS})", b.len()));
    }
    let Some(&last) = b.last() else {
        return Err("there are no bars to find sections in".into());
    };
    let end = if end.is_finite() && end > last {
        end
    } else {
        let n = b.len();
        let before = n.checked_sub(2).and_then(|i| b.get(i)).copied();
        last + before.map_or(2.0, |p| (last - p).max(1e-3))
    };
    Ok((b, end))
}

/// Per bar, the features of [`GROUPS`] in one row, from the frames of the coarse spectrum whose
/// moment lies in the bar. A bar with no frame gets the mean of the others.
fn bar_features(bars: &[f64], end: f64, env: &Envelopes) -> Vec<Vec<f64>> {
    let dct = dct_matrix();
    let width: usize = GROUPS.iter().sum();
    let mut rows: Vec<Option<Vec<f64>>> = Vec::with_capacity(bars.len());
    for (i, &a) in bars.iter().enumerate() {
        let b = bars.get(i + 1).copied().unwrap_or(end);
        let range = env.spectrum_range(a, b);
        let mut previous = range.start.checked_sub(1).and_then(|f| env.spectrum(f)).map(log_frame);
        let mut spec = [0.0; SPECTRUM_BANDS];
        let mut cep = [0.0; CEPSTRUM];
        let mut cep_sq = [0.0; CEPSTRUM_SPREAD];
        let (mut loud, mut flux, mut low, mut count) = (0.0, 0.0, 0.0, 0usize);
        for f in range {
            let Some(x) = env.spectrum(f).map(log_frame) else { continue };
            for (s, v) in spec.iter_mut().zip(&x) {
                *s += v;
            }
            for (k, row) in dct.iter().enumerate() {
                let c: f64 = row.iter().zip(&x).map(|(w, v)| w * v).sum();
                if let Some(s) = cep.get_mut(k) {
                    *s += c;
                }
                if let Some(s) = cep_sq.get_mut(k) {
                    *s += c * c;
                }
            }
            loud += x.iter().sum::<f64>() / SPECTRUM_BANDS as f64;
            low += x.iter().take(LOW_BANDS).sum::<f64>() / LOW_BANDS as f64;
            let p = previous.unwrap_or(x);
            flux += x.iter().zip(&p).map(|(a, b)| (a - b).max(0.0)).sum::<f64>() / SPECTRUM_BANDS as f64;
            previous = Some(x);
            count += 1;
        }
        if count == 0 {
            rows.push(None);
            continue;
        }
        let k = count as f64;
        let mut row = Vec::with_capacity(width);
        row.extend(spec.iter().map(|s| s / k));
        row.extend(cep.iter().map(|s| s / k));
        row.extend(cep_sq.iter().zip(&cep).map(|(sq, s)| (sq / k - (s / k).powi(2)).max(0.0).sqrt()));
        row.extend([loud / k, flux / k, low / k]);
        rows.push(Some(row));
    }
    // A bar without frames: the mean of the bars with frames.
    let known: Vec<&Vec<f64>> = rows.iter().flatten().collect();
    let mut mean = vec![0.0; width];
    for r in &known {
        for (m, v) in mean.iter_mut().zip(r.iter()) {
            *m += v;
        }
    }
    let k = known.len().max(1) as f64;
    mean.iter_mut().for_each(|m| *m /= k);
    rows.into_iter().map(|r| r.unwrap_or_else(|| mean.clone())).collect()
}

/// A frame of the coarse spectrum as log10 of power.
fn log_frame(db: [f32; SPECTRUM_BANDS]) -> [f64; SPECTRUM_BANDS] {
    db.map(|v| f64::from(v) / 10.0)
}

/// The orthonormal DCT-II from [`SPECTRUM_BANDS`] values to [`CEPSTRUM`] coefficients.
fn dct_matrix() -> Vec<[f64; SPECTRUM_BANDS]> {
    let n = SPECTRUM_BANDS as f64;
    (0..CEPSTRUM)
        .map(|k| {
            let scale = if k == 0 { (1.0 / n).sqrt() } else { (2.0 / n).sqrt() };
            std::array::from_fn(|i| {
                scale * (std::f64::consts::PI * k as f64 * (2.0 * i as f64 + 1.0) / (2.0 * n)).cos()
            })
        })
        .collect()
}

/// Each column standardised over the rows (mean 0, deviation 1), each group of [`GROUPS`]
/// scaled to `weights` in total. Non-numbers become 0.
fn standardise(rows: &[Vec<f64>], weights: &[f64; 6]) -> Vec<Vec<f64>> {
    let width: usize = GROUPS.iter().sum();
    let n = rows.len().max(1) as f64;
    let column =
        |j: usize| rows.iter().map(move |r| r.get(j).copied().filter(|v| v.is_finite()).unwrap_or(0.0));
    let mut scale = Vec::with_capacity(width);
    for (g, w) in GROUPS.iter().zip(weights) {
        for _ in 0..*g {
            scale.push(w / (*g as f64).sqrt());
        }
    }
    let stats: Vec<(f64, f64, f64)> = (0..width)
        .map(|j| {
            let mean = column(j).sum::<f64>() / n;
            let var = column(j).map(|v| (v - mean).powi(2)).sum::<f64>() / n;
            (mean, var.sqrt(), scale.get(j).copied().unwrap_or(0.0))
        })
        .collect();
    rows.iter()
        .map(|r| {
            stats
                .iter()
                .enumerate()
                .map(|(j, (mean, sd, w))| {
                    let v = r.get(j).copied().filter(|v| v.is_finite()).unwrap_or(*mean);
                    let z = (v - mean) / (sd + 1e-9) * w;
                    if z.is_finite() { z } else { 0.0 }
                })
                .collect()
        })
        .collect()
}

fn unit(x: &[f64]) -> Vec<f64> {
    let norm = x.iter().map(|v| v * v).sum::<f64>().sqrt() + 1e-9;
    x.iter().map(|v| v / norm).collect()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The mean of rows `a..b`.
fn mean_rows(rows: &[Vec<f64>], a: usize, b: usize) -> Vec<f64> {
    let width = rows.first().map_or(0, Vec::len);
    let mut m = vec![0.0; width];
    let part = rows.get(a..b.min(rows.len())).unwrap_or(&[]);
    for r in part {
        for (s, v) in m.iter_mut().zip(r) {
            *s += v;
        }
    }
    let k = part.len().max(1) as f64;
    m.iter_mut().for_each(|s| *s /= k);
    m
}

/// The similarities of every row with the `width` rows from it on: `band[i * width + d]` is
/// row `i` with row `i + d`.
struct Band {
    width: usize,
    values: Vec<f64>,
}

impl Band {
    fn get(&self, i: usize, j: usize) -> f64 {
        let (lo, d) = if i <= j { (i, j - i) } else { (j, i - j) };
        if d >= self.width {
            return 0.0;
        }
        self.values.get(lo * self.width + d).copied().unwrap_or(0.0)
    }
}

fn similarity_band(unit: &[Vec<f64>], width: usize) -> Band {
    let width = width.max(1);
    let mut values = vec![0.0; unit.len() * width];
    for (i, chunk) in values.chunks_mut(width).enumerate() {
        let Some(a) = unit.get(i) else { continue };
        for (d, v) in chunk.iter_mut().enumerate() {
            if let Some(b) = unit.get(i + d) {
                *v = dot(a, b);
            }
        }
    }
    Band { width, values }
}

/// Foote's novelty with a checkerboard kernel of `k` bars on either side, Gaussian tapered,
/// over `n` bars; the matrix is extended at its edges by repeating the first and last bar.
/// Negative values and the first and last `k / 2` bars are 0; the largest value is 1.
fn foote(band: &Band, n: usize, k: usize) -> Vec<f64> {
    let k = k.max(1);
    let size = 2 * k;
    let taper: Vec<f64> = (0..size)
        .map(|a| {
            let x = -2.0 + 4.0 * a as f64 / (size - 1) as f64;
            let sign = if a < k { -1.0 } else { 1.0 };
            sign * (-0.5 * x * x).exp()
        })
        .collect();
    let last = n.saturating_sub(1);
    let mut out: Vec<f64> = (0..n)
        .map(|t| {
            let mut sum = 0.0;
            for (a, ga) in taper.iter().enumerate() {
                let i = (t + a).saturating_sub(k).min(last);
                for (b, gb) in taper.iter().enumerate() {
                    let j = (t + b).saturating_sub(k).min(last);
                    sum += ga * gb * band.get(i, j);
                }
            }
            sum.max(0.0)
        })
        .collect();
    let edge = k / 2;
    for (t, v) in out.iter_mut().enumerate() {
        if t < edge || t + edge >= n {
            *v = 0.0;
        }
    }
    let top = out.iter().copied().fold(0.0, f64::max);
    if top > 1e-12 {
        out.iter_mut().for_each(|v| *v /= top);
    }
    out
}

/// Peaks of `nov`: the highest within `win` on either side, above the mean within `2 win` by
/// `lift` standard deviations of the whole; then, highest first, those at least `gap` from
/// every one kept. Ascending.
fn pick(nov: &[f64], win: usize, gap: usize, lift: f64) -> Vec<usize> {
    let n = nov.len();
    if n < 3 {
        return Vec::new();
    }
    let mean = nov.iter().sum::<f64>() / n as f64;
    let sd = (nov.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    let mut candidates: Vec<(usize, f64)> = Vec::new();
    for t in 1..n - 1 {
        let v = nov.get(t).copied().unwrap_or(0.0);
        let around = |w: usize| nov.get(t.saturating_sub(w)..(t + w + 1).min(n)).unwrap_or(&[]);
        let near = around(win);
        if v <= 0.0 || near.iter().any(|x| *x > v) {
            continue;
        }
        let wide = around(2 * win);
        let local = wide.iter().sum::<f64>() / wide.len().max(1) as f64;
        if v - local > lift * sd {
            candidates.push((t, v));
        }
    }
    // Highest first; equal heights in time order.
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut keep: Vec<usize> = Vec::new();
    for (t, _) in candidates {
        if keep.iter().all(|k| k.abs_diff(t) >= gap) {
            keep.push(t);
        }
    }
    keep.sort_unstable();
    keep
}

/// The phrase origin of the bars `first..last`: the bar from `first` on (within one phrase)
/// at which the section novelty, summed every [`PHRASE_BARS`] bars, is highest.
fn phrase_origin(fine: &[f64], first: usize, last: usize) -> usize {
    let mut best = (first, -1.0);
    for o in 0..PHRASE_BARS {
        let sum: f64 =
            (first + o..last.min(fine.len())).step_by(PHRASE_BARS).filter_map(|b| fine.get(b)).sum();
        if sum > best.1 + 1e-12 {
            best = (first + o, sum);
        }
    }
    best.0
}

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

/// The section boundary on `side` of the track boundary `c` (both in `bounds`, which holds 0
/// and the bar count, ascending) when the section between them is a blend: it lies within
/// [`BLEND_BARS`] of `c`, and its bars move from the sound of the section before it towards
/// that of the section after it, by at least [`BLEND_RISE`] of the way between its first and
/// its last third.
fn blend_edge(rows: &[Vec<f64>], bounds: &[usize], c: usize, side: Side) -> Option<usize> {
    let i = bounds.iter().position(|b| *b == c)?;
    // The blend runs over `a..b`, between the sections `before..a` and `b..after`.
    let (before, a, b, after) = match side {
        Side::Before => (*bounds.get(i.checked_sub(2)?)?, *bounds.get(i - 1)?, c, *bounds.get(i + 1)?),
        Side::After => (*bounds.get(i.checked_sub(1)?)?, c, *bounds.get(i + 1)?, *bounds.get(i + 2)?),
    };
    let edge = match side {
        Side::Before => a,
        Side::After => b,
    };
    if b - a > BLEND_BARS || a == 0 || b >= rows.len() {
        return None;
    }
    let left = mean_rows(rows, before.max(a.saturating_sub(ZONE_REACH)), a);
    let right = mean_rows(rows, b, after.min(b + ZONE_REACH));
    let place = places(rows, a, b, &left, &right)?;
    let third = (place.len() / 3).max(1);
    let mean = |p: &[f64]| p.iter().sum::<f64>() / p.len().max(1) as f64;
    let first = mean(place.get(..third)?);
    let last = mean(place.get(place.len() - third..)?);
    (last - first >= BLEND_RISE).then_some(edge)
}

/// Each bar of `a..b` placed on the line from `left` (0) to `right` (1); `None` when the two
/// are the same.
fn places(rows: &[Vec<f64>], a: usize, b: usize, left: &[f64], right: &[f64]) -> Option<Vec<f64>> {
    let dir: Vec<f64> = right.iter().zip(left).map(|(r, l)| r - l).collect();
    let length = dot(&dir, &dir);
    if length.is_nan() || length <= 1e-12 {
        return None;
    }
    Some(
        (a..b)
            .map(|i| {
                let x = rows.get(i).map_or(&[][..], |r| r.as_slice());
                let off: Vec<f64> = x.iter().zip(left).map(|(v, l)| v - l).collect();
                dot(&off, &dir) / length
            })
            .collect(),
    )
}

/// The transition zone, bars, around the boundary at bar `b` between the sections `prev..b` and
/// `b..next`.
fn zone(rows: &[Vec<f64>], prev: usize, b: usize, next: usize) -> (usize, usize) {
    let lo = prev.max(b.saturating_sub(ZONE_REACH));
    let hi = next.min(b + ZONE_REACH).min(rows.len());
    if lo >= b || hi <= b {
        return (b, b);
    }
    let left = mean_rows(rows, lo, b);
    let right = mean_rows(rows, b, hi);
    // Each bar's place on the line from the section before (0) to the one after (1).
    let Some(place) = places(rows, lo, hi, &left, &right) else { return (b, b) };
    let smooth: Vec<f64> = (0..place.len())
        .map(|i| {
            let mut w: Vec<f64> =
                place.get(i.saturating_sub(1)..(i + 2).min(place.len())).unwrap_or(&[]).to_vec();
            w.sort_by(f64::total_cmp);
            w.get(w.len() / 2).copied().unwrap_or(0.0)
        })
        .collect();
    let at = |i: usize| smooth.get(i - lo).copied().unwrap_or(0.0);
    let start = (lo..b).rev().find(|&i| at(i) <= ZONE_LOW).map_or(lo, |i| i + 1);
    let end = (b..hi).find(|&i| at(i) >= ZONE_HIGH).unwrap_or(hi);
    // Within half of either section.
    let start = start.max(b - (b - prev) / 2).min(b);
    let end = end.min(b + (next - b) / 2).max(b);
    (start, end)
}

#[derive(Clone, Copy)]
enum Linkage {
    /// Ward's minimum variance on Euclidean distances.
    Ward,
    /// Average linkage on cosine distances (the points are unit vectors).
    Cosine,
}

/// One merge of two clusters, named by their lowest point.
#[derive(Clone, Copy, Debug)]
struct Merge {
    a: usize,
    b: usize,
    height: f64,
}

/// The pairs of points `(i, j)`, `i < j`, in one vector.
fn pair(n: usize, i: usize, j: usize) -> usize {
    let (i, j) = if i < j { (i, j) } else { (j, i) };
    // Rows 0..i hold n - 1, n - 2, ... pairs.
    i * n - i * (i + 1) / 2 + (j - i - 1)
}

/// Agglomerative clustering by nearest-neighbour chains, as many merges as points less one.
/// Both linkages are reducible, so the merges sorted by height are the usual dendrogram.
fn nn_chain(points: &[Vec<f64>], linkage: Linkage) -> Vec<Merge> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    // Ward works on squared distances, the cosine on distances.
    let mut d = vec![0.0f64; n * (n - 1) / 2];
    for i in 0..n {
        for j in i + 1..n {
            let (Some(a), Some(b)) = (points.get(i), points.get(j)) else { continue };
            let v = match linkage {
                Linkage::Ward => a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>(),
                Linkage::Cosine => 1.0 - dot(a, b),
            };
            if let Some(slot) = d.get_mut(pair(n, i, j)) {
                *slot = if v.is_finite() { v } else { 0.0 };
            }
        }
    }
    let dist = |d: &[f64], i: usize, j: usize| d.get(pair(n, i, j)).copied().unwrap_or(f64::INFINITY);
    let mut active = vec![true; n];
    let mut size = vec![1usize; n];
    let mut chain: Vec<usize> = Vec::with_capacity(n);
    let mut merges = Vec::with_capacity(n - 1);
    while merges.len() < n - 1 {
        if chain.is_empty() {
            let Some(first) = active.iter().position(|a| *a) else { break };
            chain.push(first);
        }
        let (a, b) = loop {
            let Some(&a) = chain.last() else { break (usize::MAX, usize::MAX) };
            let before = chain.len().checked_sub(2).and_then(|i| chain.get(i)).copied();
            // The nearest active cluster; the one before in the chain wins a tie.
            let mut best: Option<(usize, f64)> = before.map(|p| (p, dist(&d, a, p)));
            for c in 0..n {
                if c == a || !active.get(c).copied().unwrap_or(false) {
                    continue;
                }
                let v = dist(&d, a, c);
                if best.is_none_or(|(_, bv)| v < bv) {
                    best = Some((c, v));
                }
            }
            let Some((b, _)) = best else { break (usize::MAX, usize::MAX) };
            if Some(b) == before {
                chain.truncate(chain.len() - 2);
                break (a, b);
            }
            chain.push(b);
        };
        if a == usize::MAX {
            break;
        }
        let (keep, gone) = (a.min(b), a.max(b));
        let dab = dist(&d, a, b);
        let (na, nb) = (size.get(a).copied().unwrap_or(1) as f64, size.get(b).copied().unwrap_or(1) as f64);
        for k in 0..n {
            if k == a || k == b || !active.get(k).copied().unwrap_or(false) {
                continue;
            }
            let (dak, dbk) = (dist(&d, a, k), dist(&d, b, k));
            let nk = size.get(k).copied().unwrap_or(1) as f64;
            let v = match linkage {
                Linkage::Ward => ((na + nk) * dak + (nb + nk) * dbk - nk * dab) / (na + nb + nk),
                Linkage::Cosine => (na * dak + nb * dbk) / (na + nb),
            };
            if let Some(slot) = d.get_mut(pair(n, keep, k)) {
                *slot = v;
            }
        }
        if let Some(s) = active.get_mut(gone) {
            *s = false;
        }
        let total = size.get(a).copied().unwrap_or(1) + size.get(b).copied().unwrap_or(1);
        if let Some(s) = size.get_mut(keep) {
            *s = total;
        }
        // The chain may hold the cluster that is gone only as `a` or `b`, both popped.
        let height = match linkage {
            Linkage::Ward => dab.max(0.0).sqrt(),
            Linkage::Cosine => dab,
        };
        merges.push(Merge { a: keep, b: gone, height });
    }
    merges
}

/// Cluster numbers for `n` points: the merges taken lowest first until at most `kinds` clusters
/// are left, and on while they are below `same`.
fn cut(merges: &[Merge], n: usize, kinds: usize, same: f64) -> Vec<usize> {
    let mut sorted = merges.to_vec();
    sorted.sort_by(|x, y| x.height.total_cmp(&y.height));
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        // Bounded by the number of points: every step goes up a tree with no cycles.
        for _ in 0..parent.len() {
            let p = parent.get(i).copied().unwrap_or(i);
            if p == i {
                break;
            }
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = parent.get_mut(i) {
                *slot = gp;
            }
            i = p;
        }
        i
    }
    let mut clusters = n;
    for m in sorted {
        if clusters <= kinds && m.height.total_cmp(&same).is_ge() {
            break;
        }
        let (ra, rb) = (root(&mut parent, m.a), root(&mut parent, m.b));
        if ra != rb {
            if let Some(slot) = parent.get_mut(ra.max(rb)) {
                *slot = ra.min(rb);
            }
            clusters -= 1;
        }
    }
    (0..n).map(|i| root(&mut parent, i)).collect()
}

/// Cluster ids renumbered 0, 1, ... in the order they first appear.
fn first_appearance(ids: &[usize]) -> Vec<usize> {
    let mut seen: Vec<usize> = Vec::new();
    ids.iter()
        .map(|id| match seen.iter().position(|s| s == id) {
            Some(i) => i,
            None => {
                seen.push(*id);
                seen.len() - 1
            }
        })
        .collect()
}

/// A number fixed per label, 24 bits (splitmix64).
fn seed(label: usize) -> u32 {
    let mut z = (label as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // `as`: the top 24 bits.
    (z >> 40) as u32
}

#[cfg(test)]
#[path = "sections_tests.rs"]
mod tests;
