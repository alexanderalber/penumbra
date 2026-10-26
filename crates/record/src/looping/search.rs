//! The search for the best loop in a recording, on luma thumbnails.
//!
//! Two frames are compared by the mean absolute difference of their thumbnails. A candidate
//! loop from frame `i` to frame `j` (played `i` to `j - 1`, then `i` again) is scored over a
//! window of frames around the seam: frame `i + k` against `j + k` for `k` in `-HALF..=HALF`.
//! So the motion around the seam has to match as well as the picture. A frame dropped while
//! recording repeats the frame before and shifts everything after it by one; each comparison
//! but the centre one may therefore take `j + k - 1` or `j + k + 1` instead, and the worst
//! comparison of the window is left out.
//!
//! Comparing every pair at full thumbnail size costs n^2 / 2 times 576 samples. The search
//! first compares 8 x 6 cell means along each lag (`j - i`), on every frame or, for long
//! recordings, every s-th, then refines the best candidates at full size within s frames.

use rayon::prelude::*;

use super::convert::{THUMB, THUMB_H, THUMB_W};

/// Frames on each side of the seam a window compares.
const HALF: usize = 3;
/// Cells of the coarse descriptor: 8 x 6 means of 4 x 3 thumbnail samples.
const COARSE_W: usize = 8;
const COARSE_H: usize = 6;
const COARSE: usize = COARSE_W * COARSE_H;
/// Frame pairs the coarse pass compares at most; beyond that it takes every s-th frame. About
/// a second on 8 cores.
const COARSE_PAIRS: f64 = 3.0e8;
/// Coarse candidates refined at full size.
const REFINE: usize = 24;

/// The thumbnails of every frame of a recording, in order.
#[derive(Default)]
pub struct Thumbs {
    data: Vec<u8>,
}

impl Thumbs {
    pub fn with_capacity(frames: usize) -> Self {
        Thumbs { data: Vec::with_capacity(frames.saturating_mul(THUMB)) }
    }

    pub fn push(&mut self, thumb: &[u8; THUMB]) {
        self.data.extend_from_slice(thumb);
    }

    pub fn len(&self) -> usize {
        self.data.len() / THUMB
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn get(&self, i: usize) -> &[u8] {
        self.data.get(i * THUMB..(i + 1) * THUMB).unwrap_or(&[])
    }
}

/// The loop found: frames `start..end` (`end` not played), and its score, the mean absolute
/// luma difference across the seam window in 8-bit steps (0 is a perfect match).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub start: usize,
    pub end: usize,
    pub score: f64,
}

/// The best loop at least `min_len` frames long. `stop` is asked now and then; when it says
/// yes the search ends with an error.
pub fn find_cut(thumbs: &Thumbs, min_len: usize, stop: &(dyn Fn() -> bool + Sync)) -> Result<Cut, String> {
    let n = thumbs.len();
    let min_len = min_len.max(2);
    if n < min_len + 1 {
        return Err(format!(
            "{n} frames are too few for a loop of at least {min_len} frames (and one to compare)"
        ));
    }
    let coarse: Vec<u8> = (0..n).flat_map(|i| coarse_of(thumbs.get(i))).collect();
    let pairs = ((n - min_len) as f64).powi(2) / 2.0;
    let s = ((pairs / COARSE_PAIRS).sqrt().ceil() as usize).max(1);
    // Every s-th frame.
    let sub: Vec<u8> = coarse.as_chunks::<COARSE>().0.iter().step_by(s).flatten().copied().collect();
    let m = sub.len() / COARSE;
    let sub_min = min_len.div_ceil(s).max(1);
    if m < sub_min + 1 {
        return Err("the recording is too short for the loop length asked".into());
    }
    let candidates = coarse_candidates(&sub, m, sub_min, stop)?;
    let full = |a: usize, b: usize| {
        if a >= n || b >= n {
            return None;
        }
        Some(distance(thumbs.get(a), thumbs.get(b)))
    };
    let radius = if s > 1 { s } else { 1 };
    let mut best: Option<Cut> = None;
    for (_, i, lag) in candidates {
        if stop() {
            return Err("stopped".into());
        }
        let (ci, cj) = (i * s, (i + lag) * s);
        for i in ci.saturating_sub(radius)..=(ci + radius).min(n - 1) {
            for j in cj.saturating_sub(radius)..=(cj + radius).min(n - 1) {
                if j < i + min_len {
                    continue;
                }
                let score = window(i, j, n, &full);
                if score.is_finite() && best.is_none_or(|b| score < b.score as f32) {
                    best = Some(Cut { start: i, end: j, score: f64::from(score) });
                }
            }
        }
    }
    best.ok_or_else(|| "no loop found".to_string())
}

/// How much the picture changes from one frame to the next: the mean distance of neighbouring
/// thumbnails, on the scale of [`Cut::score`].
pub fn motion(thumbs: &Thumbs) -> f64 {
    let n = thumbs.len();
    if n < 2 {
        return 0.0;
    }
    let sum: f64 = (1..n).map(|i| f64::from(distance(thumbs.get(i - 1), thumbs.get(i)))).sum();
    sum / (n - 1) as f64
}

/// 8 x 6 means of a thumbnail.
fn coarse_of(t: &[u8]) -> [u8; COARSE] {
    let (bw, bh) = (THUMB_W / COARSE_W, THUMB_H / COARSE_H);
    let mut out = [0u8; COARSE];
    for (c, o) in out.iter_mut().enumerate() {
        let (cx, cy) = (c % COARSE_W, c / COARSE_W);
        let mut sum = 0u32;
        for y in cy * bh..(cy + 1) * bh {
            let row = t.get(y * THUMB_W + cx * bw..y * THUMB_W + (cx + 1) * bw).unwrap_or(&[]);
            sum += row.iter().map(|&v| u32::from(v)).sum::<u32>();
        }
        *o = (sum / (bw * bh) as u32) as u8;
    }
    out
}

/// Mean absolute difference of two equally long sample runs.
fn distance(a: &[u8], b: &[u8]) -> f32 {
    let sum: u32 = a.iter().zip(b).map(|(&x, &y)| u32::from(x.abs_diff(y))).sum();
    sum as f32 / a.len().max(1) as f32
}

/// Score of the loop `i..j` over the window around its seam, from the frame distance `d`
/// (`None` outside the clip). Infinite when too little of the window lies inside the clip.
fn window(i: usize, j: usize, n: usize, d: &impl Fn(usize, usize) -> Option<f32>) -> f32 {
    let Some(centre) = d(i, j) else { return f32::INFINITY };
    let mut sum = centre;
    let mut worst = centre;
    let mut count = 1usize;
    for k in 1..=HALF {
        // Before the seam (i - k against j - k) and after it (i + k against j + k).
        for (a, b) in [(i.checked_sub(k), j.checked_sub(k)), (Some(i + k), Some(j + k))] {
            let (Some(a), Some(b)) = (a, b) else { continue };
            if a >= n {
                continue;
            }
            let best = [b.checked_sub(1), Some(b), Some(b + 1)]
                .into_iter()
                .flatten()
                .filter(|&b| b > a && b < n)
                .filter_map(|b| d(a, b))
                .min_by(f32::total_cmp);
            if let Some(c) = best {
                sum += c;
                worst = worst.max(c);
                count += 1;
            }
        }
    }
    if count < HALF + 1 {
        return f32::INFINITY;
    }
    // The worst comparison is left out: one damaged frame does not spoil a good loop.
    (sum - worst) / (count - 1) as f32
}

/// The best coarse candidates (score, i, lag) of `m` descriptors, lags from `min_lag`.
fn coarse_candidates(
    sub: &[u8],
    m: usize,
    min_lag: usize,
    stop: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<(f32, usize, usize)>, String> {
    let desc = |i: usize| sub.get(i * COARSE..(i + 1) * COARSE).unwrap_or(&[]);
    let row = |lag: usize| -> Vec<f32> {
        if lag == 0 || lag >= m {
            return Vec::new();
        }
        (0..m - lag).map(|a| distance(desc(a), desc(a + lag))).collect()
    };
    let lags: Vec<usize> = (min_lag..m).collect();
    let found: Vec<Vec<(f32, usize, usize)>> = lags
        .par_chunks(32)
        .map(|chunk| {
            let mut top: Vec<(f32, usize, usize)> = Vec::new();
            // Only scores below the worst one kept count, once enough are kept.
            let mut bar = f32::INFINITY;
            let Some(&first) = chunk.first() else { return top };
            if stop() {
                return top;
            }
            // Rows for lag - 1, lag, lag + 1.
            let mut rows = [row(first.saturating_sub(1)), row(first), row(first + 1)];
            for &lag in chunk {
                if lag != first {
                    rows.rotate_left(1);
                    rows[2] = row(lag + 1);
                }
                let d = |a: usize, b: usize| {
                    let k = (b.checked_sub(a)? + 1).checked_sub(lag)?;
                    rows.get(k)?.get(a).copied()
                };
                for i in 0..m - lag {
                    let score = window(i, i + lag, m, &d);
                    if score < bar {
                        top.push((score, i, lag));
                        if top.len() > 8 * REFINE {
                            prune(&mut top);
                            bar = top.last().map_or(f32::INFINITY, |t| t.0);
                        }
                    }
                }
            }
            prune(&mut top);
            top
        })
        .collect();
    if stop() {
        return Err("stopped".into());
    }
    let mut all: Vec<(f32, usize, usize)> = found.into_iter().flatten().collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Neighbours of a candidate taken already add nothing: the refinement covers them.
    let mut picked: Vec<(f32, usize, usize)> = Vec::new();
    for c in all {
        if picked.len() >= REFINE {
            break;
        }
        if !picked.iter().any(|p| p.1.abs_diff(c.1) <= 1 && p.2.abs_diff(c.2) <= 1) {
            picked.push(c);
        }
    }
    Ok(picked)
}

/// Keeps the best few candidates, spread out: one per neighbourhood of a few frames.
fn prune(top: &mut Vec<(f32, usize, usize)>) {
    top.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept: Vec<(f32, usize, usize)> = Vec::with_capacity(2 * REFINE);
    for &c in top.iter() {
        if kept.len() >= 2 * REFINE {
            break;
        }
        if !kept.iter().any(|p| p.1.abs_diff(c.1) <= 1 && p.2.abs_diff(c.2) <= 1) {
            kept.push(c);
        }
    }
    *top = kept;
}

#[cfg(test)]
pub(super) fn window_for_tests(thumbs: &Thumbs, i: usize, j: usize) -> f32 {
    let n = thumbs.len();
    window(i, j, n, &|a, b| (a < n && b < n).then(|| distance(thumbs.get(a), thumbs.get(b))))
}
