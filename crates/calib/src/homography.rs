//! Planar homographies: the 3x3 projective map between two planes, e.g. from a flat surface to
//! a projector's image, estimated from point pairs.

use nalgebra::{DMatrix, Matrix3, SMatrix, SymmetricEigen, Vector3};
use penumbra_core::Error;

/// Most point pairs [`Homography::from_pairs`] accepts (the matrix it decomposes has twice as
/// many rows).
pub const MAX_PAIRS: usize = 10_000;

/// Most point pairs one round of [`Homography::fit_robust`] fits; more are thinned evenly.
pub const ROBUST_SAMPLE: usize = 60_000;
/// Rounds of [`Homography::fit_robust`].
const ROBUST_ROUNDS: usize = 5;
/// The share of the kept pairs whose error bounds the next round's.
const ROBUST_QUANTILE: f64 = 0.9;

/// What [`Homography::fit_robust`] found.
#[derive(Clone, Debug)]
pub struct RobustFit {
    pub homography: Homography,
    /// Per pair the distance between `H(from)` and `to` (infinite where `from` maps to infinity).
    pub errors: Vec<f64>,
    /// Pairs the last round fitted (before thinning).
    pub kept: usize,
}

/// A projective map of the plane, `p' ~ H p` in homogeneous coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography {
    m: Matrix3<f64>,
}

impl Default for Homography {
    fn default() -> Self {
        Self::identity()
    }
}

impl Homography {
    pub fn identity() -> Self {
        Self { m: Matrix3::identity() }
    }

    /// From its matrix, row by row.
    pub fn from_rows(rows: [[f64; 3]; 3]) -> Self {
        let [[a, b, c], [d, e, f], [g, h, i]] = rows;
        Self { m: Matrix3::new(a, b, c, d, e, f, g, h, i) }
    }

    /// The matrix, row by row.
    pub fn rows(&self) -> [[f64; 3]; 3] {
        let m = &self.m;
        [[m.m11, m.m12, m.m13], [m.m21, m.m22, m.m23], [m.m31, m.m32, m.m33]]
    }

    /// The homography taking each `from` to its `to`, from four or more pairs by the normalised
    /// DLT: both point sets are moved to their centroid and scaled to a mean distance of √2,
    /// the null vector of the stacked constraints is the smallest right singular vector, and
    /// the normalisation is undone. With exactly four pairs the map is exact; with more it is
    /// the algebraic least-squares fit (good with noise of a few pixels, not robust against
    /// outliers).
    ///
    /// Fails on fewer than four pairs, non-finite coordinates, and degenerate configurations
    /// (three of four points on a line, all points on one line, coincident points).
    pub fn from_pairs(pairs: &[([f64; 2], [f64; 2])]) -> Result<Self, Error> {
        if pairs.len() < 4 {
            return Err(Error::new(format!(
                "a homography needs at least 4 point pairs, got {}",
                pairs.len()
            )));
        }
        if pairs.len() > MAX_PAIRS {
            return Err(Error::new(format!("at most {MAX_PAIRS} point pairs, got {}", pairs.len())));
        }
        if pairs.iter().any(|(a, b)| a.iter().chain(b).any(|v| !v.is_finite())) {
            return Err(Error::new("point coordinates must be finite numbers"));
        }
        let from: Vec<[f64; 2]> = pairs.iter().map(|p| p.0).collect();
        let to: Vec<[f64; 2]> = pairs.iter().map(|p| p.1).collect();
        let t_from = normalising(&from)?;
        let t_to = normalising(&to)?;
        let rows = (2 * pairs.len()).max(9);
        let mut a = DMatrix::<f64>::zeros(rows, 9);
        for (k, (p, q)) in pairs.iter().enumerate() {
            let p = t_from * Vector3::new(p[0], p[1], 1.0);
            let q = t_to * Vector3::new(q[0], q[1], 1.0);
            let (x, y) = (p.x / p.z, p.y / p.z);
            let (u, v) = (q.x / q.z, q.y / q.z);
            let r = 2 * k;
            let first = [-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u];
            let second = [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v];
            for (c, (f, s)) in first.iter().zip(&second).enumerate() {
                if let Some(e) = a.get_mut((r, c)) {
                    *e = *f;
                }
                if let Some(e) = a.get_mut((r + 1, c)) {
                    *e = *s;
                }
            }
        }
        let svd = a
            .try_svd(false, true, f64::EPSILON, 1000)
            .ok_or_else(|| Error::new("the decomposition failed"))?;
        let v_t = svd.v_t.as_ref().ok_or_else(|| Error::new("the decomposition failed"))?;
        let mut order: Vec<(usize, f64)> = svd.singular_values.iter().copied().enumerate().collect();
        order.sort_by(|x, y| x.1.total_cmp(&y.1));
        let (smallest, second, largest) = match order.as_slice() {
            [s, t, .., l] => (s.0, t.1, l.1),
            _ => return Err(Error::new("the decomposition failed")),
        };
        // A second (near) zero singular value means the points do not pin the map down.
        if largest.is_nan() || largest <= 0.0 || second <= largest * 1e-9 {
            return Err(Error::new("the points are degenerate (on one line, or coincident)"));
        }
        let h: Vec<f64> = v_t.row(smallest).iter().copied().collect();
        let n = match h.as_slice() {
            &[a, b, c, d, e, f, g, h, i] => Matrix3::new(a, b, c, d, e, f, g, h, i),
            _ => return Err(Error::new("the decomposition failed")),
        };
        let back = t_to.try_inverse().ok_or_else(|| Error::new("the points are degenerate"))?;
        let m = back * n * t_from;
        Self::scaled(m).ok_or_else(|| Error::new("the points are degenerate (the map collapses the plane)"))
    }

    /// The homography taking each `from` to its `to` for many pairs with outliers (a decoded
    /// structured light map): the normalised DLT over all pairs (by its normal equations, so any
    /// number of them), then five rounds that keep only the pairs whose error is within the 90th
    /// percentile of the pairs kept before (and always those within `floor`), each round fitting
    /// at most [`ROBUST_SAMPLE`] of them, evenly thinned. Fails on fewer than four pairs, lists of
    /// different lengths, and degenerate configurations; pairs with non-finite coordinates are
    /// left out.
    pub fn fit_robust(from: &[[f64; 2]], to: &[[f64; 2]], floor: f64) -> Result<RobustFit, Error> {
        if from.len() != to.len() {
            return Err(Error::new(format!("{} points to map, {} images", from.len(), to.len())));
        }
        let finite = |p: &[f64; 2]| p.iter().all(|v| v.is_finite());
        let mut keep: Vec<bool> = from.iter().zip(to).map(|(a, b)| finite(a) && finite(b)).collect();
        let floor = if floor.is_finite() { floor.max(0.0) } else { 0.0 };
        let mut fit: Option<(Homography, Vec<f64>, usize)> = None;
        for _ in 0..ROBUST_ROUNDS {
            let kept: Vec<usize> = keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect();
            if kept.len() < 4 {
                break;
            }
            let stride = kept.len().div_ceil(ROBUST_SAMPLE).max(1);
            let sample: Vec<usize> = kept.iter().copied().step_by(stride).collect();
            let h = match normal_dlt(&sample, from, to, None) {
                Ok(h) => h,
                Err(e) if fit.is_none() => return Err(e),
                Err(_) => break,
            };
            let errors: Vec<f64> = from
                .iter()
                .zip(to)
                .map(|(p, q)| match h.apply(*p) {
                    Some(r) if finite(q) => ((r[0] - q[0]).powi(2) + (r[1] - q[1]).powi(2)).sqrt(),
                    _ => f64::INFINITY,
                })
                .collect();
            let mut kept_errors: Vec<f64> = kept.iter().filter_map(|i| errors.get(*i).copied()).collect();
            kept_errors.sort_by(f64::total_cmp);
            let at = ((kept_errors.len() as f64 - 1.0) * ROBUST_QUANTILE).round() as usize;
            let bound = kept_errors.get(at).copied().unwrap_or(f64::INFINITY).max(floor);
            for (k, e) in keep.iter_mut().zip(&errors) {
                *k = *e < bound;
            }
            fit = Some((h, errors, kept.len()));
        }
        let (homography, errors, kept) =
            fit.ok_or_else(|| Error::new("a homography needs at least 4 point pairs"))?;
        Ok(RobustFit { homography, errors, kept })
    }

    /// The homography taking each `from` to its `to` by iteratively reweighted least squares
    /// with Cauchy weights: the normalised DLT over the pairs (at most [`ROBUST_SAMPLE`], evenly
    /// thinned), then `rounds` refits, each pair weighted by `1 / (1 + (e / c)^2)` with `e` its
    /// error under the fit before and `c` the median error. Unlike [`Homography::fit_robust`] no
    /// pair is dropped outright; far ones fade, so a surface holding most of the pairs wins over
    /// a smaller one at another depth. Pairs with non-finite coordinates are left out; fails on
    /// fewer than four pairs, lists of different lengths and degenerate configurations.
    pub fn fit_cauchy(from: &[[f64; 2]], to: &[[f64; 2]], rounds: usize) -> Result<Self, Error> {
        if from.len() != to.len() {
            return Err(Error::new(format!("{} points to map, {} images", from.len(), to.len())));
        }
        let finite = |p: &[f64; 2]| p.iter().all(|v| v.is_finite());
        let usable: Vec<usize> = from
            .iter()
            .zip(to)
            .enumerate()
            .filter(|(_, (a, b))| finite(a) && finite(b))
            .map(|(i, _)| i)
            .collect();
        let stride = usable.len().div_ceil(ROBUST_SAMPLE).max(1);
        let sample: Vec<usize> = usable.into_iter().step_by(stride).collect();
        let mut h = normal_dlt(&sample, from, to, None)?;
        for _ in 0..rounds {
            let errors: Vec<f64> = sample
                .iter()
                .map(|i| match (from.get(*i), to.get(*i)) {
                    (Some(p), Some(q)) => match h.apply(*p) {
                        Some(r) => ((r[0] - q[0]).powi(2) + (r[1] - q[1]).powi(2)).sqrt(),
                        None => f64::INFINITY,
                    },
                    _ => f64::INFINITY,
                })
                .collect();
            let mut sorted = errors.clone();
            sorted.sort_by(f64::total_cmp);
            let c = sorted.get(sorted.len() / 2).copied().unwrap_or(1.0);
            let c = if c.is_finite() { c.max(1e-6) } else { 1.0 };
            let weights: Vec<f64> = errors
                .iter()
                .map(|e| if e.is_finite() { 1.0 / (1.0 + (e / c).powi(2)) } else { 0.0 })
                .collect();
            match normal_dlt(&sample, from, to, Some(&weights)) {
                Ok(next) => h = next,
                Err(_) => break,
            }
        }
        Ok(h)
    }

    /// The homography taking the unit square's corners (top left, top right, bottom right,
    /// bottom left: (0,0), (1,0), (1,1), (0,1)) to `quad`. Fails unless `quad` is a convex,
    /// non-degenerate quadrilateral in that order (either winding), because otherwise the square
    /// would fold over or pass through infinity in between.
    pub fn square_to_quad(quad: [[f64; 2]; 4]) -> Result<Self, Error> {
        if !convex(&quad) {
            return Err(Error::new("the corners cross or fold over (the quadrilateral must be convex)"));
        }
        let square = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let pairs: Vec<([f64; 2], [f64; 2])> = square.into_iter().zip(quad).collect();
        Self::from_pairs(&pairs)
    }

    /// `H (x, y, 1)`, not divided.
    pub fn apply_homogeneous(&self, p: [f64; 2]) -> [f64; 3] {
        let v = self.m * Vector3::new(p[0], p[1], 1.0);
        [v.x, v.y, v.z]
    }

    /// The image of `p`; `None` for points mapped to infinity (or non-finite results).
    pub fn apply(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let [x, y, w] = self.apply_homogeneous(p);
        if w.abs() < 1e-12 {
            return None;
        }
        let q = [x / w, y / w];
        q.iter().all(|v| v.is_finite()).then_some(q)
    }

    pub fn inverse(&self) -> Option<Self> {
        self.m.try_inverse().and_then(Self::scaled)
    }

    /// `self` after `first`: `p -> self(first(p))`.
    pub fn after(&self, first: &Homography) -> Homography {
        Self::scaled(self.m * first.m).unwrap_or(Homography { m: self.m * first.m })
    }

    /// Root mean square distance between `H(from)` and `to` over `pairs`; infinite when a point
    /// maps to infinity.
    pub fn rms_error(&self, pairs: &[([f64; 2], [f64; 2])]) -> f64 {
        if pairs.is_empty() {
            return 0.0;
        }
        let mut sum = 0.0;
        for (p, q) in pairs {
            let Some(r) = self.apply(*p) else { return f64::INFINITY };
            sum += (r[0] - q[0]).powi(2) + (r[1] - q[1]).powi(2);
        }
        (sum / pairs.len() as f64).sqrt()
    }

    /// Scaled so the bottom right entry is 1 (or the matrix has unit norm when that entry is
    /// near zero); `None` for singular or non-finite matrices.
    fn scaled(m: Matrix3<f64>) -> Option<Self> {
        let norm = m.norm();
        if !norm.is_finite() || norm < 1e-300 {
            return None;
        }
        let m = m / norm;
        if m.determinant().abs() < 1e-12 {
            return None;
        }
        let m = if m.m33.abs() > 1e-9 { m / m.m33 } else { m };
        m.iter().all(|v| v.is_finite()).then_some(Self { m })
    }
}

/// The normalised DLT over the pairs `indices` of `from` and `to`, by the normal equations (the
/// 9 x 9 matrix of the stacked constraints times itself) and its smallest eigenvector.
fn normal_dlt(
    indices: &[usize],
    from: &[[f64; 2]],
    to: &[[f64; 2]],
    weights: Option<&[f64]>,
) -> Result<Homography, Error> {
    let pick = |points: &[[f64; 2]]| -> Vec<[f64; 2]> {
        indices.iter().filter_map(|i| points.get(*i).copied()).collect()
    };
    let (a, b) = (pick(from), pick(to));
    if a.len() < 4 || a.len() != b.len() {
        return Err(Error::new("a homography needs at least 4 point pairs"));
    }
    let t_from = normalising(&a)?;
    let t_to = normalising(&b)?;
    let mut m = SMatrix::<f64, 9, 9>::zeros();
    for (k, (p, q)) in a.iter().zip(&b).enumerate() {
        // A pair's rows scaled by its weight: its share of the normal matrix by the square.
        let w2 = weights.map_or(1.0, |w| w.get(k).copied().unwrap_or(0.0).powi(2));
        let p = t_from * Vector3::new(p[0], p[1], 1.0);
        let q = t_to * Vector3::new(q[0], q[1], 1.0);
        let (x, y) = (p.x / p.z, p.y / p.z);
        let (u, v) = (q.x / q.z, q.y / q.z);
        for row in
            [[-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u], [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v]]
        {
            for (i, ri) in row.iter().enumerate() {
                for (j, rj) in row.iter().enumerate() {
                    if let Some(e) = m.get_mut((i, j)) {
                        *e += w2 * ri * rj;
                    }
                }
            }
        }
    }
    if m.iter().any(|v| !v.is_finite()) {
        return Err(Error::new("point coordinates must be finite numbers"));
    }
    let eigen = SymmetricEigen::new(m);
    let mut order: Vec<(usize, f64)> = eigen.eigenvalues.iter().copied().enumerate().collect();
    order.sort_by(|x, y| x.1.total_cmp(&y.1));
    let (smallest, second, largest) = match order.as_slice() {
        [s, t, .., l] => (s.0, t.1, l.1),
        _ => return Err(Error::new("the decomposition failed")),
    };
    // Eigenvalues are squared singular values: a second (near) zero leaves the map open.
    if largest.is_nan() || largest <= 0.0 || second <= largest * 1e-18 {
        return Err(Error::new("the points are degenerate (on one line, or coincident)"));
    }
    let h: Vec<f64> = eigen.eigenvectors.column(smallest).iter().copied().collect();
    let n = match h.as_slice() {
        &[a, b, c, d, e, f, g, h, i] => Matrix3::new(a, b, c, d, e, f, g, h, i),
        _ => return Err(Error::new("the decomposition failed")),
    };
    let back = t_to.try_inverse().ok_or_else(|| Error::new("the points are degenerate"))?;
    Homography::scaled(back * n * t_from)
        .ok_or_else(|| Error::new("the points are degenerate (the map collapses the plane)"))
}

/// The similarity moving `points` to their centroid with a mean distance of √2 from it.
pub(crate) fn normalising(points: &[[f64; 2]]) -> Result<Matrix3<f64>, Error> {
    let n = points.len() as f64;
    let cx = points.iter().map(|p| p[0]).sum::<f64>() / n;
    let cy = points.iter().map(|p| p[1]).sum::<f64>() / n;
    let mean = points.iter().map(|p| ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()).sum::<f64>() / n;
    if !mean.is_finite() || mean <= 1e-12 {
        return Err(Error::new("the points are degenerate (all in one place)"));
    }
    let s = std::f64::consts::SQRT_2 / mean;
    Ok(Matrix3::new(s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0))
}

/// Whether the four points, in order, form a convex quadrilateral with non-zero area (either
/// winding).
fn convex(q: &[[f64; 2]; 4]) -> bool {
    let mut sign = 0.0;
    let scale = q.iter().flat_map(|p| p.iter()).fold(0.0f64, |m, v| m.max(v.abs())).max(1e-12);
    for i in 0..4 {
        let mut it = q.iter().cycle().skip(i);
        let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) else { return false };
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if !cross.is_finite() || cross.abs() < 1e-9 * scale * scale {
            return false;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator, so the synthetic data is the same on every run.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.next()
        }
    }

    /// A projector-like homography: a 1920x1080 image of a 4 x 2.25 m wall seen at an angle.
    fn truth() -> Homography {
        Homography::from_rows([[410.0, 35.0, 120.0], [-12.0, 430.0, 60.0], [0.03, -0.02, 1.0]])
    }

    fn close(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
        (a[0] - b[0]).abs() < tol && (a[1] - b[1]).abs() < tol
    }

    #[test]
    fn four_pairs_are_exact() {
        let h = truth();
        let from = [[0.0, 0.0], [4.0, 0.0], [4.0, 2.25], [0.0, 2.25]];
        let pairs: Vec<_> = from.iter().map(|p| (*p, h.apply(*p).unwrap())).collect();
        let est = Homography::from_pairs(&pairs).unwrap();
        assert!(est.rms_error(&pairs) < 1e-8, "{}", est.rms_error(&pairs));
        // Any other point agrees too: the map is determined by four points.
        for p in [[1.3, 0.7], [3.9, 2.0], [-1.0, 5.0]] {
            assert!(close(est.apply(p).unwrap(), h.apply(p).unwrap(), 1e-6), "{p:?}");
        }
    }

    #[test]
    fn many_noisy_pairs_recover_the_map() {
        let h = truth();
        let mut rng = Lcg(7);
        let pairs: Vec<_> = (0..60)
            .map(|_| {
                let p = [rng.range(0.0, 4.0), rng.range(0.0, 2.25)];
                let q = h.apply(p).unwrap();
                // Half a pixel of noise.
                (p, [q[0] + rng.range(-0.5, 0.5), q[1] + rng.range(-0.5, 0.5)])
            })
            .collect();
        let est = Homography::from_pairs(&pairs).unwrap();
        // The fit is about as good as the noise (uniform ±0.5 has an RMS of 0.29 per axis).
        assert!(est.rms_error(&pairs) < 0.5, "{}", est.rms_error(&pairs));
        for p in [[0.0, 0.0], [4.0, 2.25], [2.0, 1.1]] {
            assert!(close(est.apply(p).unwrap(), h.apply(p).unwrap(), 0.5), "{p:?}");
        }
    }

    #[test]
    fn normalisation_copes_with_far_apart_scales() {
        // Millimetres on one side, normalised coordinates on the other, offset far from 0.
        let h = Homography::from_rows([[0.0004, 0.00002, -3.0], [0.00001, 0.0005, 1.5], [1e-7, -2e-7, 1.0]]);
        let mut rng = Lcg(3);
        let pairs: Vec<_> = (0..8)
            .map(|_| {
                let p = [rng.range(10_000.0, 14_000.0), rng.range(-2_000.0, 250.0)];
                (p, h.apply(p).unwrap())
            })
            .collect();
        let est = Homography::from_pairs(&pairs).unwrap();
        assert!(est.rms_error(&pairs) < 1e-9, "{}", est.rms_error(&pairs));
    }

    #[test]
    fn square_to_quad_and_back() {
        let quad = [[0.1, 0.05], [0.93, 0.0], [1.0, 0.97], [0.02, 0.9]];
        let h = Homography::square_to_quad(quad).unwrap();
        let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (c, q) in corners.iter().zip(&quad) {
            assert!(close(h.apply(*c).unwrap(), *q, 1e-12), "{c:?}");
        }
        let inv = h.inverse().unwrap();
        let p = [0.4, 0.6];
        assert!(close(inv.apply(h.apply(p).unwrap()).unwrap(), p, 1e-12));
        let id = inv.after(&h);
        assert!(close(id.apply([0.25, 0.75]).unwrap(), [0.25, 0.75], 1e-12));
        // The identity square maps to itself.
        let same = Homography::square_to_quad(corners).unwrap();
        assert!(close(same.apply([0.3, 0.8]).unwrap(), [0.3, 0.8], 1e-12));
    }

    #[test]
    fn degenerate_input_is_refused() {
        let line = [
            ([0.0, 0.0], [0.0, 0.0]),
            ([1.0, 1.0], [1.0, 0.0]),
            ([2.0, 2.0], [2.0, 0.0]),
            ([3.0, 3.0], [3.0, 0.0]),
        ];
        assert!(Homography::from_pairs(&line).is_err());
        assert!(Homography::from_pairs(&line[..3]).is_err());
        let same = [([1.0, 1.0], [0.0, 0.0]); 5];
        assert!(Homography::from_pairs(&same).is_err());
        let nan = [
            ([f64::NAN, 0.0], [0.0, 0.0]),
            ([1.0, 0.0], [1.0, 0.0]),
            ([1.0, 1.0], [1.0, 1.0]),
            ([0.0, 1.0], [0.0, 1.0]),
        ];
        assert!(Homography::from_pairs(&nan).is_err());
        // A bow tie (two corners swapped) and a collapsed corner.
        assert!(Homography::square_to_quad([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]).is_err());
        assert!(Homography::square_to_quad([[0.0, 0.0], [1.0, 0.0], [1.0, 0.0], [0.0, 1.0]]).is_err());
        // A concave quadrilateral.
        assert!(Homography::square_to_quad([[0.0, 0.0], [1.0, 0.0], [0.3, 0.3], [0.0, 1.0]]).is_err());
        // Mirrored winding is fine (a projector hung upside down, or rear projection).
        assert!(Homography::square_to_quad([[1.0, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]).is_ok());
    }

    #[test]
    fn points_at_infinity_are_none() {
        let h = Homography::from_rows([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
        assert_eq!(h.apply([0.0, 5.0]), None);
        assert!(h.rms_error(&[([0.0, 5.0], [0.0, 0.0])]).is_infinite());
    }
    #[test]
    fn robust_fit_ignores_outliers_among_many_pairs() {
        let h = truth();
        let mut rng = Lcg(11);
        let mut from = Vec::new();
        let mut to = Vec::new();
        for k in 0..80_000 {
            let p = [rng.range(0.0, 4.0), rng.range(0.0, 2.25)];
            let q = h.apply(p).unwrap();
            // A tenth wildly off, the rest within a quarter pixel.
            let q = if k % 10 == 0 {
                [rng.range(0.0, 1920.0), rng.range(0.0, 1080.0)]
            } else {
                [q[0] + rng.range(-0.25, 0.25), q[1] + rng.range(-0.25, 0.25)]
            };
            from.push(p);
            to.push(q);
        }
        from.push([f64::NAN, 0.0]);
        to.push([0.0, 0.0]);
        let fit = Homography::fit_robust(&from, &to, 1.0).unwrap();
        assert_eq!(fit.errors.len(), from.len());
        for p in [[0.0, 0.0], [4.0, 2.25], [2.0, 1.0]] {
            assert!(close(fit.homography.apply(p).unwrap(), h.apply(p).unwrap(), 0.1), "{p:?}");
        }
        assert!(fit.kept < 80_000 && fit.kept > 60_000, "{}", fit.kept);
        assert!(Homography::fit_robust(&from[..3], &to[..3], 1.0).is_err());
        assert!(Homography::fit_robust(&from, &to[..5], 1.0).is_err());
        let line: Vec<[f64; 2]> = (0..10).map(|k| [f64::from(k), 0.0]).collect();
        assert!(Homography::fit_robust(&line, &line, 1.0).is_err());
    }
}
