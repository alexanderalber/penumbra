//! Matching projectors from stored measurements, without a camera at hand: each projector's
//! camera readings, taken once and kept with the device, are combined with any others taken
//! the same way.
//!
//! Without the camera there is no closed loop, so only comparisons the readings already hold
//! can be used: for each camera channel, the reading of a projector's grey (every channel at
//! the same signal) as a function of that signal, its *neutral curve*. Two projectors give the
//! same light where the camera reads the same, whatever monotone curve the camera applies; and
//! on the neutral axis the camera's colour processing treats both alike, which comparing the
//! primaries one by one does not (a camera that boosts saturated colours reads a projector's red
//! brighter than its white in the red channel).
//!
//! The target takes each camera channel from the projector whose white is the dimmest there:
//! its neutral curve in that channel. So the common white lies between the white points when
//! no projector is the dimmest in every channel (brighter than matching to any one projector's
//! white), and is the dimmest projector's white when one is (that projector keeps its picture).
//! Every projector then gets, per channel, the signal at which its own neutral curve reaches the
//! target's; each reaches any reading up to its own white, so every target is reachable.
//!
//! Between the measured levels a neutral curve is a power law per segment (straight in log
//! signal against log reading): a projector seen through a camera is close to one, and a few
//! levels then suffice where straight segments would bend a steep curve by several per cent.
//!
//! The assumption: the camera's channel `c` of a near-grey mix follows the projector's channel
//! `c` as it does on the grey ramp. It holds for near-neutral colours (the whites and greys
//! being matched); saturated colours are matched through each channel's own curve, without the
//! crosstalk a camera run's matrix corrects.
//!
//! The correction is written in the params' form: a diagonal matrix (`m_c`, the drive of white,
//! as if the projector followed the sRGB curve) and per channel a curve from the encoded drive
//! to the signal, so that grey `x` gets the signal `s_c(x)` whatever `m_c` is.

use serde::Serialize;

use super::{
    IDENTITY, MAX_SAMPLES, Mat3, Measured, Note, apply, check, decode, encode, from_columns, interpolate,
    isotonic, lookup, luminance, normalised,
};

/// One projector as stored: what the camera read of it, black subtracted.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub measured: Measured,
    /// The camera's RGB of grey (every channel at the same signal) at each of the measured
    /// levels; `None` where the measurement has none, then each primary's ramp in its own camera
    /// channel stands in for that channel (white can come out noticeably off).
    pub greys: Option<Vec<[f64; 3]>>,
}

/// One projector's correction from stored measurements.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FromStored {
    /// Diagonal: the drive of white per channel.
    pub matrix: Mat3,
    /// Per channel the signal at encoded drives `i / (n - 1)`.
    pub curves: [Vec<f64>; 3],
    /// Columns: the camera's RGB of full red, green and blue.
    pub primaries: Mat3,
    /// The signal each channel gets for white.
    pub white_signal: [f64; 3],
    /// The camera's reading of its own white (from the neutral curves).
    pub white: [f64; 3],
    /// The brightness of white kept at strength 1 and 0.5, estimated from its own primary
    /// ramps (camera values taken as light, so a rough figure).
    pub peak: [f64; 2],
}

/// The correction of every projector from stored measurements.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredMatch {
    /// The camera's reading of the common white, per channel.
    pub target_white: [f64; 3],
    /// Per camera channel the projector whose white sets it (the dimmest there).
    pub limited_by: [usize; 3],
    pub projectors: Vec<FromStored>,
    pub warnings: Vec<Note>,
}

/// Camera readings at or above this are taken as clipped.
const CLIPPED: f64 = 0.97;
/// Readings below this share of white are taken as lost in the camera's noise.
const FLOOR: f64 = 0.01;

/// The neutral curves of `s`: per camera channel the points (signal, reading), from (0, 0),
/// made monotone.
fn neutral(s: &Stored) -> [Vec<(f64, f64)>; 3] {
    [0, 1, 2].map(|c| {
        let raw: Vec<f64> = match &s.greys {
            Some(g) => g.iter().map(|v| v.get(c).copied().unwrap_or(0.0)).collect(),
            None => s
                .measured
                .ramps
                .get(c)
                .map(|r| r.iter().map(|v| v.get(c).copied().unwrap_or(0.0)).collect())
                .unwrap_or_default(),
        };
        let mono = isotonic(&raw);
        std::iter::once((0.0, 0.0))
            .chain(s.measured.levels.iter().copied().zip(mono.into_iter().map(|v| v.max(0.0))))
            .collect()
    })
}

/// The neutral curve `points` (signal, reading; from (0, 0), increasing signal, monotone
/// reading) at signal `s`: a power law within each segment, the first segment's below the
/// first level, straight where a reading is zero or flat.
fn reading_at(points: &[(f64, f64)], s: f64) -> f64 {
    if s.is_nan() || s <= 0.0 {
        return 0.0;
    }
    let s = s.min(1.0);
    let segment = |(x0, h0): (f64, f64), (x1, h1): (f64, f64), s: f64| {
        if x0 > 0.0 && h0 > 0.0 && h1 > h0 && x1 > x0 && s > 0.0 {
            let p = (h1 / h0).ln() / (x1 / x0).ln();
            h0 * (s / x0).powf(p)
        } else {
            let t = if x1 > x0 { ((s - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 1.0 };
            h0 + t * (h1 - h0)
        }
    };
    let n = points.len();
    for i in 1..n {
        let (Some(&a), Some(&b)) = (points.get(i - 1), points.get(i)) else { break };
        if s <= b.0 {
            // Below the first level: the power law of the first measured segment.
            if a.0 <= 0.0
                && let Some(&c) = points.get(i + 1)
                && b.1 > 0.0
                && c.1 > b.1
            {
                return segment(b, c, s).min(b.1);
            }
            return segment(a, b, s);
        }
    }
    points.last().map_or(0.0, |p| p.1)
}

/// The smallest signal at which `points` (as [`reading_at`] reads them) reach `r`.
fn signal_for(points: &[(f64, f64)], r: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    let n = points.len();
    for i in 1..n {
        let (Some(&a), Some(&b)) = (points.get(i - 1), points.get(i)) else { break };
        if b.1 >= r {
            // Bisection on the segment: `reading_at` is monotone there.
            let (mut lo, mut hi) = (a.0, b.0);
            for _ in 0..60 {
                let mid = 0.5 * (lo + hi);
                if reading_at(points, mid) >= r { hi = mid } else { lo = mid }
            }
            return hi.clamp(0.0, 1.0);
        }
    }
    points.last().map_or(1.0, |p| p.0)
}

/// The light of the response `points` (signal, normalised light) at signal `s`.
fn light(points: &[(f64, f64)], s: f64) -> f64 {
    interpolate(points, s.clamp(0.0, 1.0))
}

/// Matches the projectors of `stored` (each measured as [`Stored`] holds it, all with the same
/// camera, exposure and spot) with curves of `samples` points.
pub fn match_stored(stored: &[Stored], samples: usize) -> Result<StoredMatch, Note> {
    if stored.is_empty() {
        return Err("no projectors to match".into());
    }
    if !(2..=MAX_SAMPLES).contains(&samples) {
        return Err(format!("a tone curve has 2 to {MAX_SAMPLES} samples, got {samples}").into());
    }
    let mut warnings = Vec::new();
    let mut curves_of = Vec::with_capacity(stored.len());
    for (k, s) in stored.iter().enumerate() {
        check(&s.measured).map_err(|e| Note::about(k, e))?;
        if let Some(g) = &s.greys {
            if g.len() != s.measured.levels.len() {
                return Err(Note::about(k, "the greys have not one value per level"));
            }
            if g.iter().flatten().any(|v| !v.is_finite()) {
                return Err(Note::about(k, "a measurement is not a finite number"));
            }
        } else {
            warnings.push(Note::about(
                k,
                "measured without greys: white and greys are matched from each primary alone, which \
                 through a camera that does not add up can leave white noticeably off; measure it again \
                 with save",
            ));
        }
        let n = neutral(s);
        for (c, name) in ["red", "green", "blue"].iter().enumerate() {
            let full = n.get(c).and_then(|p| p.last()).map_or(0.0, |p| p.1);
            if full <= 1e-6 {
                return Err(Note::about(k, format!("its grey reads no {name}")));
            }
            if full >= CLIPPED {
                warnings.push(Note::about(
                    k,
                    format!(
                        "the camera clipped its {name} when it was measured; the top of the match is a guess"
                    ),
                ));
            }
        }
        curves_of.push(n);
    }

    // Per channel the projector whose white is the dimmest there; its neutral curve is the
    // target's in that channel. Every projector reaches every reading up to its own white, so
    // all of them reach that curve.
    let white_of = |n: &[Vec<(f64, f64)>; 3], c: usize| n.get(c).and_then(|p| p.last()).map_or(0.0, |p| p.1);
    let limited_by = [0, 1, 2].map(|c| {
        let mut best = (0, f64::INFINITY);
        for (k, n) in curves_of.iter().enumerate() {
            let w = white_of(n, c);
            if w < best.1 - 1e-12 {
                best = (k, w);
            }
        }
        best.0
    });
    let target_curve = |c: usize| -> &[(f64, f64)] {
        limited_by
            .get(c)
            .and_then(|k| curves_of.get(*k))
            .and_then(|n| n.get(c))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };
    let target_white = [0, 1, 2].map(|c| target_curve(c).last().map_or(0.0, |p| p.1));
    // Below the lowest level at which the camera reads the target clearly, the signal is taken
    // proportional to the one there: a reading lost in the camera's noise says nothing.
    let lowest = [0, 1, 2].map(|c| {
        let floor = FLOOR * target_curve(c).last().map_or(0.0, |p| p.1);
        target_curve(c).iter().find(|(x, h)| *x > 0.0 && *h >= floor).map_or(0.0, |(x, _)| *x)
    });

    let at = |i: usize| i as f64 / (samples - 1).max(1) as f64;
    let steps = (samples - 1).max(1) as f64;
    let mut projectors = Vec::with_capacity(stored.len());
    for (s, n) in stored.iter().zip(&curves_of) {
        // Per channel the signal that reads as the target's grey `x` (linear content).
        let signal = |c: usize, x: f64| {
            let p = n.get(c).map(Vec::as_slice).unwrap_or(&[]);
            let sigma = encode(x);
            let low = lowest.get(c).copied().unwrap_or(0.0);
            if sigma < low && low > 0.0 {
                signal_for(p, reading_at(target_curve(c), low)) * sigma / low
            } else {
                signal_for(p, reading_at(target_curve(c), sigma))
            }
        };
        let white_signal = [0, 1, 2].map(|c| signal(c, 1.0));
        // The drive of white on a sample of the curves, so the bend where grey reaches white
        // falls on a sample and not between two.
        let m = white_signal.map(|v| decode((v.max(0.01) * steps - 1e-9).ceil().max(1.0) / steps));
        let mut curves: [Vec<f64>; 3] = Default::default();
        let mut matrix = IDENTITY;
        for (c, ((curve, row), mc)) in curves.iter_mut().zip(matrix.iter_mut()).zip(m).enumerate() {
            *curve = (0..samples).map(|i| signal(c, (decode(at(i)) / mc).min(1.0))).collect();
            *row = [0, 1, 2].map(|j| if j == c { mc } else { 0.0 });
        }
        let full: [[f64; 3]; 3] = s.measured.ramps.clone().map(|r| r.last().copied().unwrap_or([0.0; 3]));
        let primaries = from_columns(full);
        let response: [Vec<(f64, f64)>; 3] = [0, 1, 2].map(|c| {
            let ramp = s.measured.ramps.get(c).map(Vec::as_slice).unwrap_or(&[]);
            let g = normalised(ramp, full.get(c).copied().unwrap_or([0.0; 3]));
            std::iter::once(0.0).chain(s.measured.levels.iter().copied()).zip(g).collect()
        });
        let peak = [1.0, 0.5].map(|st: f64| {
            let mut lit = [0.0; 3];
            for (((l, mc), curve), resp) in lit.iter_mut().zip(m).zip(&curves).zip(&response) {
                let e = encode((1.0 - st) + st * mc);
                *l = light(resp, (1.0 - st) * e + st * lookup(curve, e));
            }
            let all = luminance(apply(&primaries, [1.0; 3]));
            if all > 1e-12 { luminance(apply(&primaries, lit)) / all } else { 0.0 }
        });
        let white = [0, 1, 2].map(|c| white_of(n, c));
        projectors.push(FromStored { matrix, curves, primaries, white_signal, white, peak });
    }
    Ok(StoredMatch { target_white, limited_by, projectors, warnings })
}

/// How much brighter (camera luminance of white, a rough figure) the common white of
/// [`match_stored`] is than `reference_white` scaled by `alpha`: above 1 where a target between
/// the white points keeps more than matching to one projector's white.
pub fn between_gain(whites: &[[f64; 3]], reference_white: [f64; 3], alpha: f64) -> f64 {
    let envelope = [0, 1, 2]
        .map(|c| whites.iter().map(|w| w.get(c).copied().unwrap_or(0.0)).fold(f64::INFINITY, f64::min));
    let r = luminance(reference_white.map(|v| v * alpha));
    if r > 1e-12 && envelope.iter().all(|v| v.is_finite()) { luminance(envelope) / r } else { 1.0 }
}

#[cfg(test)]
#[path = "colour_stored_tests.rs"]
mod tests;
