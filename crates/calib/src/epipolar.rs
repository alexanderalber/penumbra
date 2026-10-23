//! Epipolar geometry between two views of one scene (a camera and a projector): every pair of
//! points that see the same spot satisfies `q^T F p = 0` for one fundamental matrix `F`,
//! whatever the scene's shape. A structured light decode that pairs a camera pixel with the
//! wrong projector pixel breaks it, so it tells a wrong decode from a right one without knowing
//! the scene.
//!
//! [`fit`] finds `F` by RANSAC over the normalised eight-point algorithm (Hartley and
//! Zisserman, algorithm 11.1, with the rank enforced to 2), [`Fundamental::distance`] measures
//! how far a pair lies off it (the Sampson distance, in pixels).

use crate::homography::normalising;
use nalgebra::{Matrix3, SMatrix, SVector, SymmetricEigen, Vector3};

/// Hypotheses [`fit`] tries at most, and at least before it may stop early.
const MAX_HYPOTHESES: usize = 3000;
const MIN_HYPOTHESES: usize = 200;
/// The chance of having missed the right fit at which [`fit`] stops early.
const MISS: f64 = 1e-3;
/// Pairs a hypothesis is scored on, evenly thinned.
const SCORE_SAMPLE: usize = 4000;
/// Pairs a refit uses at most, evenly thinned, and the refits.
const REFIT_SAMPLE: usize = 50_000;
const REFITS: usize = 3;

/// A fundamental matrix: pairs `(p, q)` that see the same spot satisfy `q^T F p = 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fundamental {
    m: Matrix3<f64>,
}

impl Fundamental {
    /// The Sampson distance of the pair `(p, q)`: about how far (pixels, in the joint space of
    /// both points) the pair must move to satisfy the geometry.
    pub fn distance(&self, p: [f64; 2], q: [f64; 2]) -> f64 {
        let (a, b) = (Vector3::new(p[0], p[1], 1.0), Vector3::new(q[0], q[1], 1.0));
        let fa = self.m * a;
        let fb = self.m.transpose() * b;
        let den = fa.x * fa.x + fa.y * fa.y + fb.x * fb.x + fb.y * fb.y;
        let num = b.dot(&fa);
        if den > 0.0 && num.is_finite() { (num * num / den).sqrt() } else { f64::INFINITY }
    }
}

/// What [`fit`] found.
#[derive(Clone, Copy, Debug)]
pub struct GeometryFit {
    pub fundamental: Fundamental,
    /// The share of the pairs within the threshold.
    pub share: f64,
}

/// The fundamental matrix most pairs `(from[k], to[k])` satisfy within `threshold` pixels:
/// hypotheses from eight pairs drawn among `seeds` (indices; the pairs most likely right, e.g.
/// the brightest), scored on an even sample of all pairs, the best refitted on its inliers.
/// `None` on fewer than eight usable pairs or seeds, or when no hypothesis holds.
pub fn fit(from: &[[f64; 2]], to: &[[f64; 2]], seeds: &[usize], threshold: f64) -> Option<GeometryFit> {
    let n = from.len().min(to.len());
    let finite =
        |k: usize| from.get(k).zip(to.get(k)).is_some_and(|(a, b)| a.iter().chain(b).all(|v| v.is_finite()));
    let usable: Vec<usize> = (0..n).filter(|k| finite(*k)).collect();
    let seeds: Vec<usize> = seeds.iter().copied().filter(|k| *k < n && finite(*k)).collect();
    if usable.len() < 8 || seeds.len() < 8 || !(threshold.is_finite() && threshold > 0.0) {
        return None;
    }
    let pick =
        |list: &[usize]| -> Vec<[f64; 2]> { list.iter().filter_map(|k| from.get(*k).copied()).collect() };
    let pick_to =
        |list: &[usize]| -> Vec<[f64; 2]> { list.iter().filter_map(|k| to.get(*k).copied()).collect() };
    let t_from = normalising(&pick(&usable)).ok()?;
    let t_to = normalising(&pick_to(&usable)).ok()?;
    let norm = |t: &Matrix3<f64>, p: [f64; 2]| {
        let v = t * Vector3::new(p[0], p[1], 1.0);
        [v.x / v.z, v.y / v.z]
    };
    let solve = |list: &[usize]| -> Option<Fundamental> {
        let mut a = SMatrix::<f64, 9, 9>::zeros();
        for k in list {
            let (Some(p), Some(q)) = (from.get(*k), to.get(*k)) else { continue };
            let ([x, y], [u, v]) = (norm(&t_from, *p), norm(&t_to, *q));
            let row = SVector::<f64, 9>::from([u * x, u * y, u, v * x, v * y, v, x, y, 1.0]);
            a += row * row.transpose();
        }
        let eig = SymmetricEigen::new(a);
        let (smallest, _) = eig.eigenvalues.iter().enumerate().min_by(|x, y| x.1.total_cmp(y.1))?;
        let f: Vec<f64> = eig.eigenvectors.column(smallest).iter().copied().collect();
        let &[f0, f1, f2, f3, f4, f5, f6, f7, f8] = f.as_slice() else { return None };
        let m = Matrix3::new(f0, f1, f2, f3, f4, f5, f6, f7, f8);
        let svd = m.try_svd(true, true, f64::EPSILON, 200)?;
        let (u, v_t) = (svd.u?, svd.v_t?);
        let mut s = svd.singular_values;
        let (low, _) = s.iter().enumerate().min_by(|x, y| x.1.total_cmp(y.1))?;
        if let Some(z) = s.get_mut(low) {
            *z = 0.0;
        }
        let m = t_to.transpose() * (u * Matrix3::from_diagonal(&s) * v_t) * t_from;
        let scale = m.norm();
        (scale.is_finite() && scale > 0.0).then(|| Fundamental { m: m / scale })
    };
    let stride = usable.len().div_ceil(SCORE_SAMPLE).max(1);
    let scored: Vec<usize> = usable.iter().copied().step_by(stride).collect();
    let score = |f: &Fundamental| {
        scored
            .iter()
            .filter(|k| matches!((from.get(**k), to.get(**k)), (Some(p), Some(q)) if f.distance(*p, *q) < threshold))
            .count()
    };
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut below = |n: usize| -> usize {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((rng >> 33) as usize) % n.max(1)
    };
    let mut best: Option<(usize, Fundamental)> = None;
    let mut needed = MAX_HYPOTHESES;
    let mut tried = 0usize;
    while tried < needed.clamp(MIN_HYPOTHESES, MAX_HYPOTHESES) {
        tried += 1;
        let mut eight = [0usize; 8];
        for j in 0..eight.len() {
            let mut k = seeds.get(below(seeds.len())).copied().unwrap_or(0);
            // Eight distinct pairs; a repeat is drawn again (a few times at most).
            for _ in 0..8 {
                if !eight.iter().take(j).any(|e| *e == k) {
                    break;
                }
                k = seeds.get(below(seeds.len())).copied().unwrap_or(0);
            }
            if let Some(slot) = eight.get_mut(j) {
                *slot = k;
            }
        }
        let Some(f) = solve(&eight) else { continue };
        let s = score(&f);
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, f));
            let w = s as f64 / scored.len().max(1) as f64;
            let miss_one = 1.0 - w.powi(8);
            needed = if miss_one <= 0.0 {
                MIN_HYPOTHESES
            } else if miss_one >= 1.0 {
                MAX_HYPOTHESES
            } else {
                ((MISS.ln() / miss_one.ln()).ceil() as usize).min(MAX_HYPOTHESES)
            };
        }
    }
    let (_, mut f) = best?;
    for _ in 0..REFITS {
        let inliers: Vec<usize> = usable
            .iter()
            .copied()
            .filter(|k| matches!((from.get(*k), to.get(*k)), (Some(p), Some(q)) if f.distance(*p, *q) < threshold))
            .collect();
        if inliers.len() < 8 {
            break;
        }
        let stride = inliers.len().div_ceil(REFIT_SAMPLE).max(1);
        let thinned: Vec<usize> = inliers.into_iter().step_by(stride).collect();
        match solve(&thinned) {
            Some(next) => f = next,
            None => break,
        }
    }
    let share = score(&f) as f64 / scored.len().max(1) as f64;
    Some(GeometryFit { fundamental: f, share })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pinhole pair looking at random depths: camera at the origin, projector shifted and
    /// turned a little.
    fn views(n: usize, seed: u64) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let (fc, fp) = (1000.0, 1200.0);
        let (c, sn) = (0.2f64.cos(), 0.2f64.sin());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for _ in 0..n {
            let (x, y, z) = (next() * 2.0 - 1.0, next() * 1.2 - 0.6, 2.0 + next() * 3.0);
            a.push([960.0 + fc * x / z, 540.0 + fc * y / z]);
            // Projector: turned about y by 0.2 rad, shifted 0.5 along x.
            let (px, pz) = (c * x + sn * z - 0.5, -sn * x + c * z);
            b.push([960.0 + fp * px / pz, 540.0 + fp * y / pz]);
        }
        (a, b)
    }

    #[test]
    fn finds_the_geometry_among_wrong_pairs() {
        let (a, mut b) = views(20_000, 3);
        // 60 % of the pairs wrong (random projector pixels), the seeds the right ones and as many
        // wrong ones.
        // Which pairs are wrong, scattered (not every fifth: the scoring thins evenly).
        let class = |k: usize| (k.wrapping_mul(2_654_435_761) >> 7) % 5;
        let mut s = 99u64;
        for (k, q) in b.iter_mut().enumerate() {
            if class(k) < 3 {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *q = [((s >> 33) % 1920) as f64, ((s >> 13) % 1080) as f64];
            }
        }
        let seeds: Vec<usize> = (0..a.len()).filter(|k| class(*k) != 0).collect();
        let fit = fit(&a, &b, &seeds, 2.0).expect("a fit");
        assert!((fit.share - 0.4).abs() < 0.03, "share {}", fit.share);
        let f = fit.fundamental;
        let right = (0..a.len()).filter(|k| class(*k) >= 3).all(|k| f.distance(a[k], b[k]) < 0.5);
        let wrong: Vec<usize> = (0..a.len()).filter(|k| class(*k) < 3).collect();
        let wrong_off = wrong.iter().filter(|k| f.distance(a[**k], b[**k]) > 2.0).count();
        assert!(right);
        assert!(wrong_off as f64 > 0.95 * wrong.len() as f64, "{wrong_off} of {}", wrong.len());
    }

    #[test]
    fn too_few_pairs_give_none() {
        let (a, b) = views(7, 1);
        assert!(fit(&a, &b, &[0, 1, 2, 3, 4, 5, 6], 2.0).is_none());
        let (a, b) = views(100, 1);
        assert!(fit(&a, &b, &[0, 1], 2.0).is_none());
        assert!(fit(&a, &b, &(0..100).collect::<Vec<_>>(), f64::NAN).is_none());
    }
}
