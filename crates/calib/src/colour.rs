//! Matching projectors to each other by colour, from what a camera saw of them.
//!
//! The model: a projector driven with linear drive `d` (per channel, 0..1) shows the light
//! `P d`, where the columns of `P` are the camera's picture of its full red, green and blue
//! (black subtracted), once each channel's tone curve is undone. The correction of projector
//! `k` is a matrix in linear light, `d = M_k c` for content `c`, then per channel a tone curve
//! from the drive to the signal sent to the projector. All projectors aim at the same target
//! primaries `Q = alpha P_ref`: `M_k = P_k^-1 Q`, with `alpha` as large as every projector can
//! reach (drive within 0..1 for every content in the unit cube).
//!
//! The tone curve is a table of signal values at drives evenly spaced in the sRGB-encoded
//! drive, so a projector that responds like the sRGB curve gets the straight line `0..1`. What
//! it aims at is the [`Target`]: by default the reference projector's own response (the
//! reference keeps the straight line, the others are bent to respond like it), which holds
//! through any monotone tone curve of the camera, since only responses are compared; or the
//! sRGB curve itself, which needs a camera known to be linear.
//!
//! Approximations, all reported in the result where they bite: channels add up (white is
//! red + green + blue; [`Matched`] reports how far off white is); a target that needs a
//! negative drive is clamped to zero and the row scaled back to its sum, which keeps white
//! matched and moves saturated colours slightly; the projectors' black level is subtracted but
//! not compensated (in the dark, the brighter black still shows).
//!
//! The camera side: Y'CbCr frames to linear RGB ([`linear_picture`]), the pixels a projector
//! lights ([`lit`]), where to measure ([`spots`]) and a robust mean there ([`trimmed_mean`]).

use serde::Serialize;

/// A 3x3 matrix, row by row.
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Fewest pixels a measuring spot may have (in the picture as [`linear_picture`] samples it).
pub const MIN_PIXELS: usize = 16;
/// A projector whose white lights no pixel by at least this much (linear, over black) is
/// taken as not seen by the camera.
pub const MIN_SIGNAL: f32 = 0.02;
/// Share of the values dropped at each end by [`trimmed_mean`].
pub const TRIM: f64 = 0.1;

/// The sRGB encoding of linear `x` (0..1).
pub fn encode(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Linear light of the sRGB-encoded `v` (0..1).
pub fn decode(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    a.map(|row| {
        let col = |j: usize| row.iter().zip(b).map(|(x, brow)| x * brow.get(j).copied().unwrap_or(0.0)).sum();
        [col(0), col(1), col(2)]
    })
}

pub fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    m.map(|row| row.iter().zip(v).map(|(a, b)| a * b).sum())
}

fn det(m: &Mat3) -> f64 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
}

/// The inverse, `None` when `m` is (nearly) singular relative to the size of its columns.
pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let norm = |x: f64, y: f64, z: f64| (x * x + y * y + z * z).sqrt();
    let scale = norm(a, d, g) * norm(b, e, h) * norm(c, f, i);
    let dt = det(m);
    if !(scale.is_finite() && scale > 0.0 && dt.abs() > 1e-4 * scale) {
        return None;
    }
    let r = 1.0 / dt;
    Some([
        [(e * i - f * h) * r, (c * h - b * i) * r, (b * f - c * e) * r],
        [(f * g - d * i) * r, (a * i - c * g) * r, (c * d - a * f) * r],
        [(d * h - e * g) * r, (b * g - a * h) * r, (a * e - b * d) * r],
    ])
}

fn columns(m: &Mat3) -> [[f64; 3]; 3] {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    [[a, d, g], [b, e, h], [c, f, i]]
}

fn from_columns(cols: [[f64; 3]; 3]) -> Mat3 {
    columns(&cols)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// What the camera saw of one projector, black subtracted.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measured {
    /// Signal levels of the ramp steps, increasing within (0, 1], the last one 1.
    pub levels: Vec<f64>,
    /// Per channel (red, green, blue) the camera's linear RGB at each level.
    pub ramps: [Vec<[f64; 3]>; 3],
}

/// One projector's correction.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Correction {
    /// Drive from content in linear light, row by row (`d = M c`).
    pub matrix: Mat3,
    /// Per channel the signal at encoded drives `i / (n - 1)`.
    pub curves: [Vec<f64>; 3],
    /// The measured primaries: columns are the camera's RGB of full red, green and blue.
    pub primaries: Mat3,
    /// Per channel the normalised light (0 at signal 0, 1 at full) at signal 0 and each level,
    /// after it was made monotone.
    pub response: [Vec<f64>; 3],
    /// White over the sum of the three primaries, per camera channel: 1 when channels add up.
    pub additivity: Option<[f64; 3]>,
    /// The brightness of white kept, against the projector uncorrected, with the matrix at
    /// strength 1 and 0.5 ([`blend_matrix`]).
    pub peak: [f64; 2],
}

/// The correction of every projector.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Matched {
    /// The projector whose primaries (scaled) are the target.
    pub reference: usize,
    /// The target's brightness against the reference's full output.
    pub alpha: f64,
    pub projectors: Vec<Correction>,
    pub warnings: Vec<String>,
}

/// What the tone curves aim at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Respond like the reference projector: it keeps straight curves, the others map each
    /// drive to the signal at which they give the light the reference gives (both normalised to
    /// full). Relative only, so the camera's own tone curve and colour processing cancel.
    #[default]
    Reference,
    /// Respond like the sRGB curve: each projector's measured response is inverted. Trusts the
    /// camera's values as linear light.
    Srgb,
}

/// Most samples per tone curve.
pub const MAX_SAMPLES: usize = 64;

/// Matches the projectors of `measured` (optionally with a measured white each, black
/// subtracted, for the additivity check), with curves of `samples` points.
pub fn match_projectors(
    measured: &[Measured],
    whites: &[Option<[f64; 3]>],
    samples: usize,
    target: Target,
) -> Result<Matched, String> {
    if measured.is_empty() {
        return Err("no projectors to match".into());
    }
    if !(2..=MAX_SAMPLES).contains(&samples) {
        return Err(format!("a tone curve has 2 to {MAX_SAMPLES} samples, not {samples}"));
    }
    let mut primaries = Vec::with_capacity(measured.len());
    let mut responses = Vec::with_capacity(measured.len());
    let mut warnings = Vec::new();
    for (k, m) in measured.iter().enumerate() {
        check(m).map_err(|e| format!("projector {k}: {e}"))?;
        let full: [[f64; 3]; 3] = m.ramps.clone().map(|r| r.last().copied().unwrap_or([0.0; 3]));
        if full.iter().any(|c| c.iter().sum::<f64>() <= 1e-6) {
            return Err(format!("projector {k}: a full primary is not brighter than black"));
        }
        let p = from_columns(full);
        if inverse(&p).is_none() {
            return Err(format!(
                "projector {k}: its red, green and blue look alike to the camera (camera clipping, or a white balance that hides the colours?)"
            ));
        }
        let response: [Vec<f64>; 3] = [0, 1, 2].map(|c| {
            let primary = full.get(c).copied().unwrap_or([0.0; 3]);
            let ramp = m.ramps.get(c).map(Vec::as_slice).unwrap_or(&[]);
            normalised(ramp, primary)
        });
        primaries.push(p);
        responses.push(response);
    }

    // The target: each projector's primaries tried as the reference, the brightest reachable
    // one wins.
    let mut best: Option<(usize, f64, Vec<Mat3>, usize)> = None;
    for (r, pr) in primaries.iter().enumerate() {
        let mut alpha: f64 = 1.0;
        let mut mats = Vec::with_capacity(primaries.len());
        let mut clamped = 0;
        for pk in &primaries {
            let inv = inverse(pk).ok_or("singular primaries")?;
            let (a, n) = clamp_rows(&mul(&inv, pr));
            clamped += n;
            let max_row = a.iter().map(|row| row.iter().sum::<f64>()).fold(0.0, f64::max);
            if !(max_row.is_finite() && max_row > 1e-9) {
                alpha = 0.0;
            } else {
                alpha = alpha.min(1.0 / max_row);
            }
            mats.push(a);
        }
        if best.as_ref().is_none_or(|(_, b, _, _)| alpha > *b + 1e-12) {
            best = Some((r, alpha, mats, clamped));
        }
    }
    let Some((reference, alpha, mats, clamped)) = best else {
        return Err("no projectors to match".into());
    };
    if alpha <= 1e-6 {
        return Err(
            "the projectors cannot be brought to common primaries (one cannot show a colour another shows)"
                .into(),
        );
    }
    if clamped > 0 {
        warnings.push(format!(
            "{clamped} matrix entr{} needed a negative drive and were clamped to zero: saturated colours match only approximately",
            if clamped == 1 { "y" } else { "ies" }
        ));
    }

    let reference_points: Option<[Vec<(f64, f64)>; 3]> = match target {
        Target::Srgb => None,
        Target::Reference => {
            let m = measured.get(reference).ok_or("no reference")?;
            let r = responses.get(reference).ok_or("no reference")?;
            Some([0, 1, 2].map(|c| points(&m.levels, r.get(c).map(Vec::as_slice).unwrap_or(&[]))))
        }
    };
    let mut projectors = Vec::with_capacity(measured.len());
    for (k, ((m, p), (a, response))) in
        measured.iter().zip(&primaries).zip(mats.iter().zip(responses)).enumerate()
    {
        let matrix = a.map(|row| row.map(|v| v * alpha));
        let curves = [0, 1, 2].map(|c| {
            let own = points(&m.levels, response.get(c).map(Vec::as_slice).unwrap_or(&[]));
            let at = |i: usize| i as f64 / (samples - 1).max(1) as f64;
            match &reference_points {
                // The reference as it is; the others to its response at each signal.
                Some(_) if k == reference => (0..samples).map(at).collect(),
                Some(r) => {
                    let r = r.get(c).map(Vec::as_slice).unwrap_or(&[]);
                    (0..samples).map(|i| invert(&own, interpolate(r, at(i)))).collect()
                }
                None => (0..samples).map(|i| invert(&own, at(i))).collect(),
            }
        });
        let additivity = whites.get(k).copied().flatten().map(|w| {
            let sum = apply(p, [1.0; 3]);
            [0, 1, 2].map(|i| {
                let s = sum.get(i).copied().unwrap_or(0.0);
                let w = w.get(i).copied().unwrap_or(0.0);
                if s.abs() > 1e-9 { w / s } else { 0.0 }
            })
        });
        if let Some(add) = additivity
            && add.iter().any(|v| (v - 1.0).abs() > 0.1)
        {
            warnings.push(format!(
                "projector {k}: white is {:.2}, {:.2}, {:.2} times red + green + blue (a white boost or dynamic contrast? switch it off for a good match)",
                add[0], add[1], add[2]
            ));
        }
        let peak = [1.0, 0.5].map(|s| peak(p, &blend_matrix(&matrix, s)));
        projectors.push(Correction { matrix, curves, primaries: *p, response, additivity, peak });
    }
    Ok(Matched { reference, alpha, projectors, warnings })
}

/// `(1 - s) I + s M`: the matrix at strength `s` (0..1), between no correction and full.
pub fn blend_matrix(m: &Mat3, s: f64) -> Mat3 {
    let s = if s.is_finite() { s.clamp(0.0, 1.0) } else { 1.0 };
    let mut out = *m;
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let id = if i == j { 1.0 } else { 0.0 };
            *v = (1.0 - s) * id + s * *v;
        }
    }
    out
}

/// Rec. 709 luminance of camera RGB (the camera's own primaries are close enough for a ratio).
pub fn luminance(c: [f64; 3]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The luminance of white through matrix `m` (drive clamped to 0..1) on primaries `p`, over
/// that of white uncorrected.
pub fn peak(p: &Mat3, m: &Mat3) -> f64 {
    let drive = apply(m, [1.0; 3]).map(|d| d.clamp(0.0, 1.0));
    let full = luminance(apply(p, [1.0; 3]));
    if full > 1e-12 { luminance(apply(p, drive)) / full } else { 0.0 }
}

fn check(m: &Measured) -> Result<(), String> {
    if m.levels.is_empty() {
        return Err("no ramp levels".into());
    }
    if m.ramps.iter().any(|r| r.len() != m.levels.len()) {
        return Err("a ramp has not one value per level".into());
    }
    let increasing = m.levels.windows(2).all(|w| matches!(w, [a, b] if b > a));
    let last = m.levels.last().copied().unwrap_or(0.0);
    if !increasing || m.levels.iter().any(|l| !(*l > 0.0 && *l <= 1.0)) || (last - 1.0).abs() > 1e-9 {
        return Err("ramp levels must increase within (0, 1] and end at 1".into());
    }
    if m.ramps.iter().flatten().flatten().any(|v| !v.is_finite()) {
        return Err("a measurement is not a finite number".into());
    }
    Ok(())
}

/// The light of a ramp along its own primary, 0 at signal 0 and 1 at full, monotone; with the
/// value at signal 0 first.
fn normalised(ramp: &[[f64; 3]], primary: [f64; 3]) -> Vec<f64> {
    let pp = dot(primary, primary).max(1e-12);
    let raw: Vec<f64> = ramp.iter().map(|v| dot(*v, primary) / pp).collect();
    let mut out = vec![0.0];
    out.extend(isotonic(&raw).into_iter().map(|v| v.clamp(0.0, 1.0)));
    out
}

/// The closest non-decreasing sequence in least squares (pool adjacent violators).
pub fn isotonic(values: &[f64]) -> Vec<f64> {
    let mut blocks: Vec<(f64, usize)> = Vec::with_capacity(values.len());
    for &v in values {
        blocks.push((v, 1));
        while let [.., (s1, c1), (s2, c2)] = blocks.as_slice() {
            if s1 / *c1 as f64 <= s2 / *c2 as f64 {
                break;
            }
            let merged = (s1 + s2, c1 + c2);
            blocks.truncate(blocks.len().saturating_sub(2));
            blocks.push(merged);
        }
    }
    blocks.iter().flat_map(|&(s, c)| std::iter::repeat_n(s / c as f64, c)).collect()
}

/// Negative entries to zero, each row scaled back to its sum (so white stays where it was);
/// with how many entries were clamped (ignoring rounding noise).
fn clamp_rows(m: &Mat3) -> (Mat3, usize) {
    let mut clamped = 0;
    let out = m.map(|row| {
        let sum: f64 = row.iter().sum();
        let big = row.iter().fold(0.0_f64, |a, v| a.max(v.abs()));
        clamped += row.iter().filter(|v| **v < -1e-3 * big.max(1e-12)).count();
        let pos = row.map(|v| v.max(0.0));
        let psum: f64 = pos.iter().sum();
        if psum > 1e-12 && sum > 0.0 { pos.map(|v| v * sum / psum) } else { pos }
    });
    (out, clamped)
}

/// The response `g` (at signal 0 and `levels`) as points of signal and encoded light, so a
/// projector near the sRGB curve gives nearly a straight line and interpolates well. With
/// [`Target::Srgb`], the curve for encoded drive `e` is where these points reach `e`.
fn points(levels: &[f64], g: &[f64]) -> Vec<(f64, f64)> {
    std::iter::once(0.0).chain(levels.iter().copied()).zip(g.iter().map(|v| encode(*v))).collect()
}

/// The piecewise linear `points` (increasing `x`) at `x`.
fn interpolate(points: &[(f64, f64)], x: f64) -> f64 {
    for w in points.windows(2) {
        if let [(x0, h0), (x1, h1)] = *w
            && x <= x1
        {
            let t = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 1.0 };
            return h0 + t * (h1 - h0);
        }
    }
    points.last().map_or(x, |p| p.1)
}

/// The smallest `x` where the piecewise linear, non-decreasing `points` reach `e`.
fn invert(points: &[(f64, f64)], e: f64) -> f64 {
    if let Some(&(x0, h0)) = points.first()
        && h0 >= e
    {
        return x0;
    }
    for w in points.windows(2) {
        if let [(x0, h0), (x1, h1)] = *w
            && h1 >= e
        {
            let t = if h1 > h0 { (e - h0) / (h1 - h0) } else { 1.0 };
            return (x0 + t * (x1 - x0)).clamp(0.0, 1.0);
        }
    }
    points.last().map_or(1.0, |p| p.0)
}

/// Evaluates a tone curve table at encoded drive `e`, as the output pass does.
pub fn lookup(table: &[f64], e: f64) -> f64 {
    let n = table.len();
    if n < 2 {
        return e.clamp(0.0, 1.0);
    }
    let t = e.clamp(0.0, 1.0) * (n - 1) as f64;
    let i = (t.floor() as usize).min(n - 2);
    let f = t - i as f64;
    match (table.get(i), table.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * f,
        _ => e,
    }
}

/// The signal (sRGB-encoded, uncorrected) that content `c` (linear light) becomes through
/// `matrix` and `curves`, as the output pass computes it.
pub fn signal(matrix: &Mat3, curves: &[Vec<f64>; 3], c: [f64; 3]) -> [f64; 3] {
    let d = apply(matrix, c);
    let mut out = [0.0; 3];
    for ((o, d), curve) in out.iter_mut().zip(d).zip(curves) {
        *o = lookup(curve, encode(d.clamp(0.0, 1.0)));
    }
    out
}

/// A tone curve table with its signal scaled by `e^y` around encoded drives: `knots` are
/// `(encoded drive, y)`, increasing; the factor is interpolated linearly between them, 1 below
/// the first and held from the last. Kept within 0..1 and non-decreasing.
pub fn reshape(curve: &[f64], knots: &[(f64, f64)]) -> Vec<f64> {
    let n = curve.len().saturating_sub(1).max(1) as f64;
    let points: Vec<(f64, f64)> = std::iter::once((0.0, 0.0)).chain(knots.iter().copied()).collect();
    let mut top: f64 = 0.0;
    curve
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let y = interpolate(&points, i as f64 / n);
            let v = (v * if y.is_finite() { y.exp() } else { 1.0 }).clamp(0.0, 1.0);
            top = top.max(v);
            top
        })
        .collect()
}

/// Largest change of a log scale in one [`Broyden`] step.
pub const MAX_STEP: f64 = 0.7;

/// Finds where three coupled functions of three unknowns are all zero, from their values alone
/// (Broyden's method: a secant step in several dimensions). Here the unknowns are the log
/// scales of a matrix's rows and the functions the log ratios of the camera's reading of white
/// to the reference's, per channel; they are coupled where the camera's channels see each
/// other's light. The first step assumes a linear camera without crosstalk (the identity);
/// each value seen corrects that estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct Broyden {
    jacobian: Mat3,
    last: Option<([f64; 3], [f64; 3])>,
}

impl Default for Broyden {
    fn default() -> Self {
        Self { jacobian: IDENTITY, last: None }
    }
}

impl Broyden {
    pub fn new() -> Self {
        Self::default()
    }

    /// A solver for a similar problem: what this one learnt of the slopes, none of its points.
    pub fn restart(&self) -> Self {
        Self { jacobian: self.jacobian, last: None }
    }

    /// The next `x` after seeing `f` at `x`: each unknown moves at most [`MAX_STEP`] and stays
    /// within `lo..=hi`. A step that cannot be computed (a singular estimate, not finite) falls
    /// back to the first-step assumption.
    pub fn step(&mut self, x: [f64; 3], f: [f64; 3], lo: f64, hi: [f64; 3]) -> [f64; 3] {
        let finite = |v: &[f64; 3]| v.iter().all(|x| x.is_finite());
        if let Some((x0, f0)) = self.last
            && finite(&f)
        {
            let dx = [x[0] - x0[0], x[1] - x0[1], x[2] - x0[2]];
            let df = [f[0] - f0[0], f[1] - f0[1], f[2] - f0[2]];
            let dd = dot(dx, dx);
            if dd > 1e-12 {
                let jdx = apply(&self.jacobian, dx);
                let u = [df[0] - jdx[0], df[1] - jdx[1], df[2] - jdx[2]];
                let mut next = self.jacobian;
                for (row, ui) in next.iter_mut().zip(u) {
                    for (v, dxj) in row.iter_mut().zip(dx) {
                        *v += ui * dxj / dd;
                    }
                }
                // Brighter rows must read brighter: an estimate that says otherwise is noise.
                if finite(&next[0])
                    && finite(&next[1])
                    && finite(&next[2])
                    && next[0][0] > 0.1
                    && next[1][1] > 0.1
                    && next[2][2] > 0.1
                {
                    self.jacobian = next;
                }
            }
        }
        if finite(&f) {
            self.last = Some((x, f));
        }
        let f = f.map(|v| if v.is_finite() { v } else { 0.0 });
        let delta = inverse(&self.jacobian).map(|inv| apply(&inv, f)).filter(|d| finite(d)).unwrap_or(f);
        let mut out = x;
        for ((o, d), h) in out.iter_mut().zip(delta).zip(hi) {
            *o = (*o - d.clamp(-MAX_STEP, MAX_STEP)).clamp(lo, h.max(lo));
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The camera side.

/// A camera picture in linear RGB, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f32; 3]>,
}

/// A camera frame's planes and how to read them.
pub struct Planes<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub cb: &'a [u8],
    pub cr: &'a [u8],
    /// Chroma subsampling as shifts: (1, 1) is 4:2:0.
    pub shift: (u32, u32),
    /// The Y'CbCr matrix's red and blue weights (BT.709: 0.2126, 0.0722).
    pub kr_kb: (f32, f32),
    pub full_range: bool,
}

/// The frame in linear RGB, every `step`-th pixel across and down (nearest chroma sample), as
/// `tex.video_in` converts it (without its chroma interpolation).
pub fn linear_picture(p: &Planes<'_>, step: usize) -> Result<Picture, String> {
    let step = step.max(1);
    let (w, h) = (p.width as usize, p.height as usize);
    let (sx, sy) = (p.shift.0.min(4), p.shift.1.min(4));
    let cw = w.div_ceil(1 << sx);
    let ch = h.div_ceil(1 << sy);
    if w == 0 || h == 0 || p.y.len() < w * h || p.cb.len() < cw * ch || p.cr.len() < cw * ch {
        return Err(format!("a {w}x{h} camera frame with too few bytes"));
    }
    let (kr, kb) = p.kr_kb;
    let kg = 1.0 - kr - kb;
    if kg.abs() < 1e-3 {
        return Err("a camera frame with an impossible colour matrix".into());
    }
    let to_linear = |v: f32| decode(f64::from(v)) as f32;
    let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
    let mut rgb = Vec::with_capacity(ow * oh);
    for yy in (0..h).step_by(step) {
        for xx in (0..w).step_by(step) {
            let yv = f32::from(p.y.get(yy * w + xx).copied().unwrap_or(0));
            let ci = (yy >> sy) * cw + (xx >> sx);
            let cb = f32::from(p.cb.get(ci).copied().unwrap_or(128));
            let cr = f32::from(p.cr.get(ci).copied().unwrap_or(128));
            let (y, u, v) = if p.full_range {
                (yv / 255.0, (cb - 128.0) / 255.0, (cr - 128.0) / 255.0)
            } else {
                ((yv - 16.0) / 219.0, (cb - 128.0) / 224.0, (cr - 128.0) / 224.0)
            };
            let r = y + 2.0 * (1.0 - kr) * v;
            let b = y + 2.0 * (1.0 - kb) * u;
            let g = (y - kr * r - kb * b) / kg;
            rgb.push([to_linear(r), to_linear(g), to_linear(b)]);
        }
    }
    Ok(Picture { width: ow, height: oh, rgb })
}

/// The pixels `white` lights over `black` (same size): brighter by at least a third of the
/// brightest (99th percentile) difference. `None` when nothing is lit noticeably.
pub fn lit(white: &Picture, black: &Picture) -> Option<Vec<bool>> {
    if white.width != black.width || white.height != black.height || white.rgb.len() != black.rgb.len() {
        return None;
    }
    let diff: Vec<f32> = white
        .rgb
        .iter()
        .zip(&black.rgb)
        .map(|(w, b)| (w[0] - b[0] + w[1] - b[1] + w[2] - b[2]) / 3.0)
        .collect();
    let mut sorted: Vec<f32> = diff.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f32::total_cmp);
    let top = sorted.get((sorted.len() - 1) * 99 / 100).copied().unwrap_or(0.0);
    if top < MIN_SIGNAL {
        return None;
    }
    let mask: Vec<bool> = diff.iter().map(|d| *d > top / 3.0).collect();
    (count(&mask) >= MIN_PIXELS).then_some(mask)
}

pub fn count(mask: &[bool]) -> usize {
    mask.iter().filter(|m| **m).count()
}

/// The pixels of `mask` whose whole square of radius `r` lies in `mask`.
pub fn erode(mask: &[bool], width: usize, height: usize, r: usize) -> Vec<bool> {
    if mask.len() != width * height {
        return vec![false; mask.len()];
    }
    // Summed area table, one larger each way.
    let sw = width + 1;
    let mut sat = vec![0u32; sw * (height + 1)];
    for y in 0..height {
        let mut row = 0u32;
        for x in 0..width {
            row += u32::from(mask.get(y * width + x).copied().unwrap_or(false));
            let above = sat.get(y * sw + x + 1).copied().unwrap_or(0);
            if let Some(s) = sat.get_mut((y + 1) * sw + x + 1) {
                *s = above + row;
            }
        }
    }
    let at = |x: usize, y: usize| sat.get(y * sw + x).copied().unwrap_or(0);
    let side = (2 * r + 1) as u32;
    let mut out = vec![false; mask.len()];
    for y in r..height.saturating_sub(r) {
        for x in r..width.saturating_sub(r) {
            let (x0, y0, x1, y1) = (x - r, y - r, x + r + 1, y + r + 1);
            let sum = at(x1, y1) + at(x0, y0) - at(x0, y1) - at(x1, y0);
            if sum == side * side
                && let Some(o) = out.get_mut(y * width + x)
            {
                *o = true;
            }
        }
    }
    out
}

/// The central part of `mask`: eroded by a tenth of its size, less where that leaves fewer
/// than [`MIN_PIXELS`].
pub fn central(mask: &[bool], width: usize, height: usize) -> Vec<bool> {
    let n = count(mask);
    let mut r = ((n as f64).sqrt() * 0.1).round() as usize;
    while r > 0 {
        let e = erode(mask, width, height, r);
        if count(&e) >= MIN_PIXELS.min(n) {
            return e;
        }
        r /= 2;
    }
    mask.to_vec()
}

/// Where each projector is measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Spots {
    /// All projectors are measured in the same pixels (where all of them light the wall).
    pub shared: bool,
    pub masks: Vec<Vec<bool>>,
}

/// The measuring spots for projectors lighting `lit` (one mask each): their common pixels when
/// there are enough, else the central part of each one's own.
pub fn spots(lit: &[Vec<bool>], width: usize, height: usize) -> Spots {
    let Some(first) = lit.first() else { return Spots { shared: false, masks: Vec::new() } };
    let common: Vec<bool> =
        (0..first.len()).map(|i| lit.iter().all(|m| m.get(i).copied().unwrap_or(false))).collect();
    if lit.len() > 1 && count(&common) >= MIN_PIXELS {
        let c = central(&common, width, height);
        return Spots { shared: true, masks: vec![c; lit.len()] };
    }
    Spots { shared: lit.len() == 1, masks: lit.iter().map(|m| central(m, width, height)).collect() }
}

/// The mean of `picture` over `mask`, per channel without the lowest and highest [`TRIM`].
pub fn trimmed_mean(picture: &Picture, mask: &[bool]) -> Option<[f64; 3]> {
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut v: Vec<f32> = picture
            .rgb
            .iter()
            .zip(mask)
            .filter(|(_, m)| **m)
            .filter_map(|(p, _)| p.get(c).copied())
            .filter(|v| v.is_finite())
            .collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(f32::total_cmp);
        let cut = (v.len() as f64 * TRIM).floor() as usize;
        let kept = v.get(cut..v.len() - cut).filter(|s| !s.is_empty()).unwrap_or(&v);
        *o = kept.iter().map(|x| f64::from(*x)).sum::<f64>() / kept.len() as f64;
    }
    Some(out)
}

#[cfg(test)]
#[path = "colour_tests.rs"]
mod tests;
