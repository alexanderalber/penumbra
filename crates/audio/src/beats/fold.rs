//! Beats folded onto the tempo around them.
//!
//! The network sometimes counts the half beats of a passage (a hi-hat run at twice the tempo)
//! or only every second beat (a sparse intro). On a two-hour mix it counted double for 16 and
//! for 29 seconds and half for half a minute of the intro. Every binding on the grid would then
//! run at twice or half the speed there, and the bars would be half or twice as long.
//!
//! Here each beat gets the tempo of the minute around it: the beat spacing that covers the
//! most time within [`WINDOW`] seconds on either side. Weighing spacings by their length
//! matters: a passage counted double has twice the beats but not twice the time. Then the
//! beats are walked as a path of kept beats. A step from one kept beat to a later one costs
//! little when the time between them is a whole number of beats at that tempo (one to
//! [`MAX_BEATS`]): the beats skipped are dropped and the missing beats are inserted evenly.
//! A step that fits no whole number keeps the next beat as it is, at a higher cost
//! ([`BREAK`]). The cheapest path wins. Where the beats follow the tempo, the path keeps every
//! beat untouched.
//!
//! Finality: the tempo of a beat needs [`WINDOW`] seconds after it, and the cheapest path up
//! to a beat can change while later beats arrive. [`fold_prefix`] tells how many folded beats
//! no later beat can change: those up to the last kept beat that every possible continuation
//! passes through.

/// Seconds on either side of a beat whose spacings decide its tempo.
pub const WINDOW: f64 = 60.0;
/// The shortest and longest spacing that counts for the tempo (240 and 40 beats per minute).
/// Longer gaps are breaks, not beats.
const SHORTEST: f64 = 0.25;
const LONGEST: f64 = 1.5;
/// The tempo is the mean of the spacings within this fraction of the time-weighted median.
const NEAR_MEDIAN: f64 = 0.08;
/// Histogram bins of the spacings, seconds. Beats sit on frames of 20 ms.
const BIN: f64 = 0.005;
/// The most beats one step may span: a missing bar of four is filled, a longer break not.
pub const MAX_BEATS: usize = 4;
/// The most found beats one step may pass over (all but the last of them dropped).
const REACH: usize = 8;
/// How far a step of one found beat may stray from the tempo and still keep it cheaply.
const KEEP_TOLERANCE: f64 = 0.15;
/// How far the spacing of a step that drops or inserts beats may stray from the tempo.
const FOLD_TOLERANCE: f64 = 0.1;
/// The cost of keeping a beat that fits no whole number of beats.
pub const BREAK: f64 = 1.0;
/// The cost of each beat dropped.
const DROP: f64 = 0.4;
/// The cost of each beat inserted.
const INSERT: f64 = 0.2;
/// The cost of a step at the edge of its tolerance.
const KEEP_STRAY: f64 = 0.25;
const FOLD_STRAY: f64 = 0.5;
/// The downbeat logit an inserted beat gets: as unlikely to begin a bar as most beats.
pub const INSERTED_LOGIT: f32 = -10.0;

/// The beats after folding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folded {
    /// Ascending, seconds.
    pub beats: Vec<f64>,
    /// One per beat: the found beat's logit, [`INSERTED_LOGIT`] for an inserted one. Empty
    /// when the input had no logit for every beat.
    pub logits: Vec<f32>,
    /// One per beat: the index of the found beat it is, `None` for an inserted one.
    pub found: Vec<Option<usize>>,
    /// How many of the first folded beats stay as they are whatever beats follow (all of them
    /// for a whole piece).
    pub final_count: usize,
}

impl Folded {
    /// How many found beats were dropped.
    pub fn dropped(&self, found: usize) -> usize {
        found.saturating_sub(self.found.iter().filter(|f| f.is_some()).count())
    }

    /// How many beats were inserted.
    pub fn inserted(&self) -> usize {
        self.found.iter().filter(|f| f.is_none()).count()
    }
}

/// The beats of a whole piece, folded. `beats` ascending; `logits` one per beat or empty.
pub fn fold(beats: &[f64], logits: &[f32]) -> Folded {
    let mut f = fold_prefix(beats, logits);
    f.final_count = f.beats.len();
    f
}

/// The beats of the start of a piece, folded as [`fold`] folds them, with
/// [`Folded::final_count`] telling how many of them [`fold`] of any longer piece that begins
/// with these beats gives the same way.
pub fn fold_prefix(beats: &[f64], logits: &[f32]) -> Folded {
    let n = beats.len();
    let with_logits = logits.len() == n;
    if n < 2 || beats.iter().any(|t| !t.is_finite()) {
        return Folded {
            beats: beats.to_vec(),
            logits: if with_logits { logits.to_vec() } else { Vec::new() },
            found: (0..n).map(Some).collect(),
            final_count: 0,
        };
    }
    let period = periods(beats);
    // Cheapest path to each beat, kept, and the step into it: (previous kept beat, beats
    // the step spans). Paths may begin on any of the first REACH beats, the earlier ones
    // dropped.
    let mut cost: Vec<f64> =
        (0..n).map(|j| if j < REACH { DROP * j as f64 } else { f64::INFINITY }).collect();
    let mut from: Vec<(Option<usize>, usize)> = vec![(None, 1); n];
    for j in 1..n {
        for i in j.saturating_sub(REACH)..j {
            let Some(step) = step_cost(beats, &period, i, j) else { continue };
            let total = cost.get(i).copied().unwrap_or(f64::INFINITY) + step.0;
            if cost.get(j).is_some_and(|c| total < *c) {
                if let Some(c) = cost.get_mut(j) {
                    *c = total;
                }
                if let Some(f) = from.get_mut(j) {
                    *f = (Some(i), step.1);
                }
            }
        }
    }
    // The path ends on one of the last REACH beats, those after it dropped.
    let last = n - 1;
    let end = (n.saturating_sub(REACH)..n)
        .min_by(|a, b| {
            let ca = cost.get(*a).copied().unwrap_or(f64::INFINITY) + DROP * (last - a) as f64;
            let cb = cost.get(*b).copied().unwrap_or(f64::INFINITY) + DROP * (last - b) as f64;
            ca.total_cmp(&cb)
        })
        .unwrap_or(last);
    let path = path_to(&from, end);
    let settled = settled(beats, &from);
    let mut out = Folded::default();
    let mut prev: Option<usize> = None;
    for &j in &path {
        let span = from.get(j).map_or(1, |f| f.1);
        if let (Some(i), Some(a), Some(b)) = (prev, prev.and_then(|i| beats.get(i)), beats.get(j))
            && span > 1
            && from.get(j).is_some_and(|f| f.0 == Some(i))
        {
            for q in 1..span {
                // `as`: at most MAX_BEATS.
                out.beats.push(a + (b - a) * q as f64 / span as f64);
                out.found.push(None);
                if with_logits {
                    out.logits.push(INSERTED_LOGIT);
                }
            }
        }
        out.beats.push(beats.get(j).copied().unwrap_or(0.0));
        out.found.push(Some(j));
        if with_logits {
            out.logits.push(logits.get(j).copied().unwrap_or(INSERTED_LOGIT));
        }
        if Some(j) == settled {
            out.final_count = out.beats.len();
        }
        prev = Some(j);
    }
    out
}

/// The kept beats of the cheapest path ending on `end`, in order.
fn path_to(from: &[(Option<usize>, usize)], end: usize) -> Vec<usize> {
    let mut path = vec![end];
    let mut j = end;
    while let Some(i) = from.get(j).and_then(|f| f.0) {
        path.push(i);
        j = i;
    }
    path.reverse();
    path
}

/// The latest kept beat that every path through later beats keeps: the cheapest paths into
/// the last REACH beats whose tempo is final all pass through it, and every step into a
/// later beat starts from one of them. `None` when there is none yet.
fn settled(beats: &[f64], from: &[(Option<usize>, usize)]) -> Option<usize> {
    let n = beats.len();
    let last = *beats.last()?;
    // The tempo of a beat is final once a spacing starting after its window is known.
    let m = beats.partition_point(|t| t + WINDOW < last);
    // Beats 0..=top have final costs: every step into them starts at a beat below m.
    let top = m.min(n - 1);
    // A path may begin on any of the first REACH beats; one beginning beyond `top` would
    // pass through none of the candidates.
    if top + 1 < REACH {
        return None;
    }
    let candidates: Vec<usize> = ((top + 1).saturating_sub(REACH)..=top).collect();
    let mut seen = vec![0usize; top + 1];
    for &c in &candidates {
        let mut j = Some(c);
        while let Some(k) = j {
            if let Some(s) = seen.get_mut(k) {
                *s += 1;
            }
            j = from.get(k).and_then(|f| f.0);
        }
    }
    (0..=top).rev().find(|k| seen.get(*k) == Some(&candidates.len()))
}

/// The cost of stepping from kept beat `i` to kept beat `j`, and how many beats the step
/// spans. `None` when no such step is allowed.
fn step_cost(beats: &[f64], period: &[f64], i: usize, j: usize) -> Option<(f64, usize)> {
    let gap = beats.get(j)? - beats.get(i)?;
    let p = period.get(i).copied().unwrap_or(0.0);
    let skipped = j - i;
    let mut best = (skipped == 1).then_some((BREAK, 1));
    if p > 0.0 {
        let ratio = gap / p;
        if skipped == 1 && (ratio - 1.0).abs() <= KEEP_TOLERANCE {
            let e = (ratio - 1.0) / KEEP_TOLERANCE;
            return Some((KEEP_STRAY * e * e, 1));
        }
        let span = ratio.round();
        // `as`: a small whole number, checked against MAX_BEATS first.
        if span >= 1.0 && span <= MAX_BEATS as f64 {
            let span = span as usize;
            let e = (ratio / span as f64 - 1.0) / FOLD_TOLERANCE;
            if e.abs() <= 1.0 {
                let c = DROP * (skipped - 1) as f64 + INSERT * (span - 1) as f64 + FOLD_STRAY * e * e;
                if best.is_none_or(|b| c < b.0) {
                    best = Some((c, span));
                }
            }
        }
    }
    best
}

/// The tempo at each beat as a beat spacing in seconds, 0 where no spacing counts: the mean
/// of the spacings near the time-weighted median of those starting within [`WINDOW`] seconds.
fn periods(beats: &[f64]) -> Vec<f64> {
    // `as`: LONGEST / BIN, a few hundred.
    let bins = (LONGEST / BIN) as usize + 1;
    let bin_of = |g: f64| -> Option<usize> {
        // `as`: g within SHORTEST..=LONGEST, so 50 to 300.
        (SHORTEST..=LONGEST).contains(&g).then(|| ((g / BIN) as usize).min(bins - 1))
    };
    let gaps: Vec<(f64, f64)> = beats.iter().zip(beats.iter().skip(1)).map(|(a, b)| (*a, b - a)).collect();
    let mut sum = vec![0.0f64; bins];
    let mut count = vec![0u32; bins];
    let (mut lo, mut hi) = (0usize, 0usize);
    let mut out = Vec::with_capacity(beats.len());
    let mut total = 0.0;
    for &t in beats {
        while let Some(&(s, g)) = gaps.get(hi) {
            if s > t + WINDOW {
                break;
            }
            if let Some(b) = bin_of(g)
                && let (Some(s), Some(c)) = (sum.get_mut(b), count.get_mut(b))
            {
                *s += g;
                *c += 1;
                total += g;
            }
            hi += 1;
        }
        while let Some(&(s, g)) = gaps.get(lo) {
            if s >= t - WINDOW || lo >= hi {
                break;
            }
            if let Some(b) = bin_of(g)
                && let (Some(s), Some(c)) = (sum.get_mut(b), count.get_mut(b))
            {
                *s -= g;
                *c = c.saturating_sub(1);
                total -= g;
            }
            lo += 1;
        }
        out.push(tempo_of(&sum, &count, total));
    }
    out
}

/// The tempo of one window's spacing histogram (see [`periods`]).
fn tempo_of(sum: &[f64], count: &[u32], total: f64) -> f64 {
    if total <= 0.0 {
        return 0.0;
    }
    let mut acc = 0.0;
    let mut median = None;
    for (b, s) in sum.iter().enumerate() {
        // Sums drift a little below zero once all their spacings left.
        if count.get(b).is_none_or(|c| *c == 0) {
            continue;
        }
        acc += s;
        if acc >= total / 2.0 {
            // `as`: c is at least 1.
            median = count.get(b).map(|c| s / *c as f64);
            break;
        }
    }
    let Some(m) = median.filter(|m| *m > 0.0) else { return 0.0 };
    let (mut s, mut c) = (0.0, 0u32);
    for (b, bs) in sum.iter().enumerate() {
        // `as`: a bin index, a few hundred.
        let centre = (b as f64 + 0.5) * BIN;
        if (centre - m).abs() <= NEAR_MEDIAN * m + BIN
            && let Some(bc) = count.get(b)
            && *bc > 0
        {
            s += bs;
            c += bc;
        }
    }
    // `as`: a count of spacings.
    if c > 0 { s / c as f64 } else { m }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Beats every `p` seconds from 0 to `end`, each moved by up to one 20 ms frame.
    fn steady(p: f64, end: f64) -> Vec<f64> {
        let mut out = Vec::new();
        let mut k = 0u32;
        loop {
            let t = f64::from(k) * p;
            if t > end {
                break out;
            }
            // A fixed pattern of -1, 0, 1 frames.
            let jitter = [0.0, 0.02, 0.0, -0.02, 0.0][k as usize % 5];
            out.push((t + jitter).max(0.0));
            k += 1;
        }
    }

    #[test]
    fn steady_beats_stay_as_they_are() {
        let b = steady(0.5, 240.0);
        let f = fold(&b, &[]);
        assert_eq!(f.beats, b);
        assert_eq!(f.inserted(), 0);
        // A tempo change between two tracks: 120 to 128 to 90 beats per minute.
        let mut b = steady(0.5, 120.0);
        let t = *b.last().unwrap();
        b.extend(steady(60.0 / 128.0, 120.0).iter().skip(1).map(|x| x + t));
        let t = *b.last().unwrap();
        b.extend(steady(60.0 / 90.0, 120.0).iter().skip(1).map(|x| x + t));
        assert_eq!(fold(&b, &[]).beats, b);
    }

    #[test]
    fn half_and_double_passages_come_back_to_the_tempo() {
        let truth = steady(0.5, 300.0);
        let logits: Vec<f32> = (0..truth.len()).map(|i| if i % 4 == 0 { 5.0 } else { -8.0 }).collect();
        let (mut b, mut l) = (Vec::new(), Vec::new());
        for (i, (t, g)) in truth.iter().zip(&logits).enumerate() {
            // Half time from 10 to 40 s: every second beat missing.
            if (10.0..40.0).contains(t) && i % 2 == 1 {
                continue;
            }
            b.push(*t);
            l.push(*g);
            // Double time from 100 to 130 s: a beat between every two.
            if (100.0..130.0).contains(t) {
                b.push(t + 0.25);
                l.push(-9.0);
            }
        }
        let f = fold(&b, &l);
        assert_eq!(f.beats.len(), truth.len());
        for (got, want) in f.beats.iter().zip(&truth) {
            assert!((got - want).abs() < 0.03, "{got} for {want}");
        }
        assert_eq!(f.inserted(), 30);
        assert_eq!(f.dropped(b.len()), 60);
        // The found beats keep their logits, inserted ones get the low one.
        for ((t, g), src) in f.beats.iter().zip(&f.logits).zip(&f.found) {
            match src {
                Some(i) => assert_eq!(Some(g), l.get(*i), "{t}"),
                None => assert_eq!(*g, INSERTED_LOGIT, "{t}"),
            }
        }
    }

    #[test]
    fn a_prefix_folds_as_the_whole_up_to_its_final_count() {
        let mut b = steady(0.5, 400.0);
        // Double time from 150 to 170 s and an extra beat in a fill at 250 s.
        let extra: Vec<f64> = b.iter().filter(|t| (150.0..170.0).contains(*t)).map(|t| t + 0.25).collect();
        b.extend(extra);
        b.push(250.1);
        b.sort_by(f64::total_cmp);
        let whole = fold(&b, &[]);
        let mut finals = 0;
        for k in (0..=b.len()).step_by(3) {
            let p = fold_prefix(&b[..k], &[]);
            assert!(p.final_count <= p.beats.len());
            assert_eq!(p.beats[..p.final_count], whole.beats[..p.final_count], "{k} beats");
            if p.final_count > 0 {
                finals += 1;
                // The tempo takes a minute: nothing within it of the last beat known is final.
                let last = b[k - 1];
                assert!(p.beats[p.final_count - 1] + WINDOW <= last + 0.5, "{k} beats");
            }
        }
        assert!(finals > 100, "{finals}");
    }

    #[test]
    fn odd_input_does_not_panic() {
        for b in
            [vec![], vec![1.0], vec![1.0, 1.0], vec![0.0, 100.0], vec![0.0, f64::NAN, 1.0], vec![5.0; 50]]
        {
            let f = fold_prefix(&b, &[1.0]);
            assert!(f.final_count <= f.beats.len());
            let _ = fold(&b, &[]);
        }
    }
}
