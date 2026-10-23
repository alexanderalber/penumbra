//! Regions: the faces [`crate::scene`] finds, grouped into the parts content treats as one (a
//! crate's front with its ribs and rims, a lid, a floor) and cleaned up.
//!
//! The face search splits what a person sees as one surface: a Euro crate's front falls apart
//! into its panels and the rib strips between them (the ribs stand a centimetre or two proud, a
//! plane of their own), a slightly bulged panel into two fits, and every face's edge is as ragged
//! as the decode there. Which faces belong together cannot be told from the geometry alone: two
//! crates standing side by side or stacked have coplanar fronts separated by a seam much like a
//! rib groove. So the grouping is given (by the engine, from pixels picked in the camera
//! picture), and this module does what can be automatic:
//!
//! - **Planes** ([`fit_faces`]): per face and projector a homography from the face's camera
//!   pixels, for the classes below and the regions' plane data.
//! - **Classes** ([`classify`]): which faces lie on the floor, which parallel to it, which
//!   upright, from the plane-plus-parallax relation of each face's homography to the support's.
//!   Against the support, every other face's camera-to-camera map (through one projector) is a
//!   planar homology `I + e a^T`: `e` is the projector's centre seen from the camera, shared by
//!   all faces, and `a . x = 0` the line where the face's plane meets the support's. Faces
//!   parallel to the support meet it at its vanishing line only, so their `a` are all parallel,
//!   and `((a_F - a_g) . x) / (a_F . x)` is constant over a face `g` parallel to a horizontal
//!   face `F`: 0 on `F`'s plane, 1 on the support's. The floor is the lowest large face in the
//!   camera picture below the support whose line `a` runs past it and the support on the same
//!   side (the horizon, for a camera looking down at the scene); an upright face meets the
//!   support in a line between them or across them.
//! - **Shape** ([`shape`]): each region's faces joined, the seams between them (rib grooves,
//!   pixels nothing decoded) closed, holes filled, the ragged edge smoothed, and where the result
//!   is a quadrilateral, its outline replaced by four straight lines fitted to it.
//! - **Suggestions** ([`suggestions`]): thin strips lying on the plane of a large face they touch
//!   (a crate's rib strip next to its panel), offered to the person grouping, never applied.

use nalgebra::{DMatrix, Matrix3, Vector3};

use crate::homography::Homography;
use crate::objects::Map;
use crate::raster;
use crate::scene::outline;

/// Most regions (label pictures are 8-bit).
pub const MAX_REGIONS: usize = 255;
/// Camera pixels a face's fit is sampled from at most, the fewest it needs, and how far its
/// mask shrinks first (its edge decodes worst).
const FIT_SAMPLE: usize = 4000;
const FIT_MIN: usize = 300;
const FIT_ERODE: u32 = 2;
const CAUCHY_ROUNDS: usize = 3;
/// A face is parallel to the floor when its height (support 1, floor 0) changes by less than
/// this over half the camera picture's width ([`level_fit`]), and on the floor when its median
/// height is within [`FLOOR_LEVEL`] of 0. Measured on a real scan of crates: floor faces 0 to
/// 0.25, the support 0, upright faces 2.9 to 22 (thin rib strips included). The floor candidate
/// needs [`FLOOR_MIN`] fitted pixels.
const FLAT: f64 = 1.0;
const FLOOR_LEVEL: f64 = 0.1;
const FLOOR_MIN: usize = 2000;
/// A face lies on one side of a line when this share of its pixels does.
const SIDE_SHARE: f64 = 0.98;
/// [`suggestions`]: a strip is a face no wider than twice this (camera pixels, its largest
/// inscribed radius); it touches a face across a seam of at most twice [`SUGGEST_GAP`] along
/// [`SUGGEST_CONTACT`] pixels, and lies on its plane within [`SUGGEST_PX`] camera pixels (the
/// median, in the projector where it fits best). Measured on a real scan of Euro crates: rib
/// strips and rims 0.8 to 1.5 off their panel's plane, the two fits of one bulged panel 1.0.
const STRIP_RADIUS: f32 = 10.0;
const SUGGEST_GAP: u32 = 4;
const SUGGEST_CONTACT: usize = 20;
const SUGGEST_PX: f64 = 3.0;
/// A panel is at least this many times as large as the strip it is offered for.
const PANEL_SHARE: usize = 3;
/// [`shape`]: the outline's border this close to an object (camera pixels) is not used for its
/// straight edges.
const STRAIGHT_HIDDEN: u32 = 6;
/// Notches up to about twice this deep (camera pixels) are closed before the lines are fitted,
/// tried in turn.
const STRAIGHT_CLOSE: [u32; 4] = [0, 15, 30, STRAIGHT_CLOSE_MOST];
const STRAIGHT_CLOSE_MOST: u32 = 45;
/// How far (pixels) a region running into the picture's frame is carried past it for its lines.
const FRAME_PAD: usize = 4;
/// The straight quadrilateral replaces the region when at most this share of the region lies
/// outside it, at most this share of it lies on other faces (pixels nothing measured do not
/// count), and the region fills at least this share of it.
const STRAIGHT_OUT: f64 = 0.04;
const STRAIGHT_FOREIGN: f64 = 0.04;
const STRAIGHT_FILL: f64 = 0.75;
/// Smoothing that takes more than this share of a region away (a thin strip) is not applied.
const SMOOTH_KEEP: f64 = 0.7;
/// Gaps in a projector's regions up to about twice this are closed, projector pixels.
const PROJECTOR_GAP: u32 = 4;

/// Where a face lies, as [`classify`] tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Not fitted well enough, or no support or floor to compare with.
    Unknown,
    /// On the floor's plane.
    Floor,
    /// Parallel to the floor, not on it (the support, other tops).
    Level,
    /// Neither (walls, crate fronts and sides).
    Upright,
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Unknown => "unknown",
            Kind::Floor => "floor",
            Kind::Level => "level",
            Kind::Upright => "upright",
        }
    }
}

/// One face's plane as one projector sees it.
#[derive(Clone, Debug)]
pub struct FaceFit {
    /// Projector pixels to camera pixels.
    pub homography: Homography,
    /// Camera pixels it was fitted on.
    pub pixels: usize,
    /// Their median error, camera pixels.
    pub error_px: f64,
    /// The sample: projector and camera positions.
    pub from: Vec<[f64; 2]>,
    pub to: Vec<[f64; 2]>,
}

impl FaceFit {
    /// Median error of `h` over this sample, camera pixels.
    pub fn median_error(&self, h: &Homography) -> f64 {
        let mut e: Vec<f64> = self
            .from
            .iter()
            .zip(&self.to)
            .map(|(p, q)| h.apply(*p).map_or(f64::INFINITY, |r| (r[0] - q[0]).hypot(r[1] - q[1])))
            .collect();
        median(&mut e)
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::INFINITY)
}

/// Per face (label `k` at index `k - 1`, `count` of them) and projector its plane, from the
/// face's camera pixels (`planes`, a `camera`-sized label picture) and each projector's
/// `cam_to_proj` map (`from_camera`, positions 0 to 1 in a picture of `projectors[p]`).
pub fn fit_faces(
    camera: (usize, usize),
    planes: &[u16],
    count: usize,
    from_camera: &[&Map],
    projectors: &[(usize, usize)],
) -> Vec<Vec<Option<FaceFit>>> {
    let (w, h) = camera;
    let mut out: Vec<Vec<Option<FaceFit>>> = (0..count).map(|_| vec![None; from_camera.len()]).collect();
    if planes.len() != w * h {
        return out;
    }
    for (k, slot) in out.iter_mut().enumerate() {
        let Ok(label) = u16::try_from(k + 1) else { break };
        let Some(rect) = raster::bounds(w, planes, |l| *l == label) else { continue };
        let rect = rect.grown(1, w, h);
        let rw = rect.width();
        let mask: Vec<bool> = rect.crop(w, planes).iter().map(|l| *l == label).collect();
        let core = raster::erode(rw, rect.height(), &mask, FIT_ERODE);
        let mut inside: Vec<usize> = core
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(|(j, _)| (rect.y0 + j / rw.max(1)) * w + rect.x0 + j % rw.max(1))
            .collect();
        if inside.len() < FIT_MIN {
            // A thin face: its whole mask.
            inside = planes.iter().enumerate().filter(|(_, l)| **l == label).map(|(i, _)| i).collect();
        }
        for (p, (map, size)) in from_camera.iter().zip(projectors).enumerate() {
            let lit: Vec<usize> = inside.iter().copied().filter(|i| map.uv(*i).is_some()).collect();
            if lit.len() < FIT_MIN {
                continue;
            }
            let stride = lit.len().div_ceil(FIT_SAMPLE).max(1);
            let (from, to): (Vec<[f64; 2]>, Vec<[f64; 2]>) = lit
                .iter()
                .step_by(stride)
                .filter_map(|i| {
                    let [u, v] = map.uv(*i)?;
                    let c = [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5];
                    Some(([f64::from(u) * size.0 as f64, f64::from(v) * size.1 as f64], c))
                })
                .unzip();
            let Ok(hm) = Homography::fit_cauchy(&from, &to, CAUCHY_ROUNDS) else { continue };
            let mut fit = FaceFit { homography: hm, pixels: lit.len(), error_px: 0.0, from, to };
            fit.error_px = fit.median_error(&fit.homography);
            if let Some(s) = slot.get_mut(p) {
                *s = Some(fit);
            }
        }
    }
    out
}

/// The plane through all of `fits`' samples (one projector's fits of several faces): projector
/// pixels to camera pixels, and its median error over each of them.
pub fn fit_union(fits: &[&FaceFit]) -> Option<(Homography, Vec<f64>)> {
    let total: usize = fits.iter().map(|f| f.from.len()).sum();
    let stride = total.div_ceil(2 * FIT_SAMPLE).max(1);
    let mut from = Vec::new();
    let mut to = Vec::new();
    for f in fits {
        from.extend(f.from.iter().step_by(stride));
        to.extend(f.to.iter().step_by(stride));
    }
    let h = Homography::fit_cauchy(&from, &to, CAUCHY_ROUNDS).ok()?;
    let errors = fits.iter().map(|f| f.median_error(&h)).collect();
    Some((h, errors))
}

/// Camera pixels to the conditioned coordinates the homologies are taken in (centre 0, half
/// the width 1).
fn conditioner(camera: (usize, usize)) -> Matrix3<f64> {
    let s = (camera.0.max(1) as f64) / 2.0;
    let cy = camera.1 as f64 / 2.0;
    Matrix3::new(1.0 / s, 0.0, -1.0, 0.0, 1.0 / s, -cy / s, 0.0, 0.0, 1.0)
}

fn matrix(h: &Homography) -> Matrix3<f64> {
    let r = h.rows();
    Matrix3::new(r[0][0], r[0][1], r[0][2], r[1][0], r[1][1], r[1][2], r[2][0], r[2][1], r[2][2])
}

/// The classes of the faces and their height (support 1, floor 0; `None` where unknown).
/// `fits` as [`fit_faces`] gives them, `support` the face the objects stand on, `camera_y` each
/// face's mean row in the camera picture. The third value is the face taken for the floor.
pub fn classify(
    camera: (usize, usize),
    fits: &[Vec<Option<FaceFit>>],
    support: Option<u16>,
    camera_y: &[f64],
) -> (Vec<Kind>, Vec<Option<f64>>, Option<u16>) {
    let count = fits.len();
    let mut kinds = vec![Kind::Unknown; count];
    let mut heights: Vec<Option<f64>> = vec![None; count];
    let mut best: Vec<usize> = vec![0; count];
    let mut floor_face: Option<u16> = None;
    let Some(s) = support.and_then(|s| usize::from(s).checked_sub(1)) else {
        return (kinds, heights, None);
    };
    let projectors = fits.first().map_or(0, |f| f.len());
    let c = conditioner(camera);
    let Some(c_inv) = c.try_inverse() else { return (kinds, heights, None) };
    for p in 0..projectors {
        let Some(sf) = fits.get(s).and_then(|f| f.get(p)?.as_ref()) else { continue };
        let hs = matrix(&sf.homography);
        // Per face G = M / lambda - I, M the camera-to-camera map through the face and back
        // through the support, lambda its double eigenvalue.
        let mut g: Vec<Option<Matrix3<f64>>> = vec![None; count];
        for (k, f) in fits.iter().enumerate() {
            if k == s {
                continue;
            }
            let Some(ff) = f.get(p).and_then(|x| x.as_ref()) else { continue };
            let Some(hf_inv) = matrix(&ff.homography).try_inverse() else { continue };
            let m = c * hs * hf_inv * c_inv;
            let ev = m.complex_eigenvalues();
            let pairs = [(0, 1), (0, 2), (1, 2)];
            let Some(lambda) = pairs
                .iter()
                .filter_map(|(i, j)| Some((*ev.get(*i)?, *ev.get(*j)?)))
                .min_by(|a, b| (a.0 - a.1).norm().total_cmp(&(b.0 - b.1).norm()))
                .map(|(a, b)| ((a + b) / 2.0).re)
            else {
                continue;
            };
            if !lambda.is_finite() || lambda.abs() < 1e-12 {
                continue;
            }
            if let Some(slot) = g.get_mut(k) {
                *slot = Some(m / lambda - Matrix3::identity());
            }
        }
        // e: the columns all G share, weighted by the face's size.
        let cols: Vec<Vector3<f64>> = g
            .iter()
            .enumerate()
            .filter_map(|(k, m)| {
                let m = (*m)?;
                let n = m.norm();
                let w = (fits.get(k)?.get(p)?.as_ref()?.pixels as f64).sqrt();
                (n > 1e-12 && n.is_finite())
                    .then(|| (m * (w / n)).column_iter().map(|c| c.into_owned()).collect::<Vec<_>>())
            })
            .flatten()
            .collect();
        if cols.len() < 3 {
            continue;
        }
        let stack = DMatrix::from_fn(3, cols.len(), |r, k| cols.get(k).map_or(0.0, |c| c[r]));
        let Some(u) = stack.svd(true, false).u else { continue };
        let (Some(e0), Some(e1), Some(e2)) = (u.get((0, 0)), u.get((1, 0)), u.get((2, 0))) else { continue };
        let e = Vector3::new(*e0, *e1, *e2);
        let a: Vec<Option<Vector3<f64>>> = (0..count)
            .map(|k| {
                if k == s {
                    Some(Vector3::zeros())
                } else {
                    g.get(k).copied().flatten().map(|m| m.transpose() * e)
                }
            })
            .collect();
        let xs = |k: usize| -> Vec<Vector3<f64>> {
            fits.get(k)
                .and_then(|f| f.get(p)?.as_ref())
                .map(|f| f.to.iter().map(|q| c * Vector3::new(q[0], q[1], 1.0)).collect())
                .unwrap_or_default()
        };
        let height = |floor: &Vector3<f64>, ag: &Vector3<f64>, pts: &[Vector3<f64>]| -> Vec<f64> {
            pts.iter()
                .filter_map(|x| {
                    let d = floor.dot(x);
                    (d.abs() > 1e-12).then(|| (floor - ag).dot(x) / d)
                })
                .collect()
        };
        // The floor: the lowest large face in the picture that others lie parallel to.
        let mut candidates: Vec<usize> = (0..count)
            .filter(|k| {
                *k != s
                    && a.get(*k).is_some_and(|x| x.is_some())
                    && fits.get(*k).and_then(|f| f.get(p)?.as_ref()).is_some_and(|f| f.pixels >= FLOOR_MIN)
                    && camera_y.get(*k).copied().unwrap_or(0.0)
                        > camera_y.get(s).copied().unwrap_or(f64::INFINITY)
            })
            .collect();
        candidates.sort_by(|x, y| {
            camera_y.get(*y).copied().unwrap_or(0.0).total_cmp(&camera_y.get(*x).copied().unwrap_or(0.0))
        });
        // Which side of the line a . x = 0 a face lies on: +1 or -1 when (nearly) all of it lies
        // on one side, 0 when the line crosses it.
        let side = |line: &Vector3<f64>, k: usize| -> i32 {
            let pts = xs(k);
            let pos = pts.iter().filter(|x| line.dot(x) > 0.0).count();
            let n = pts.len().max(1) as f64;
            if pos as f64 >= SIDE_SHARE * n {
                1
            } else if (pts.len() - pos) as f64 >= SIDE_SHARE * n {
                -1
            } else {
                0
            }
        };
        let mut floor: Option<usize> = None;
        for cand in candidates {
            let Some(Some(af)) = a.get(cand) else { continue };
            // A face parallel to the support meets it at its vanishing line, which runs past both
            // on the same side (the horizon, above them for a camera looking down); a face that
            // is not meets it in a line between them or across them (a crate's front meets its
            // lid along their shared edge). Flatness against the candidate would not tell: faces
            // coplanar with a wall lie flat against it too.
            let sc = side(af, cand);
            if sc == 0 || sc != side(af, s) {
                continue;
            }
            floor = Some(cand);
            break;
        }
        let Some(fl) = floor else { continue };
        let Some(Some(af)) = a.get(fl).copied() else { continue };
        for k in 0..count {
            let Some(ff) = fits.get(k).and_then(|f| f.get(p)?.as_ref()) else { continue };
            if ff.pixels <= best.get(k).copied().unwrap_or(0) {
                continue;
            }
            let Some(Some(ag)) = a.get(k).copied() else { continue };
            let pts = xs(k);
            let hv = height(&af, &ag, &pts);
            if hv.is_empty() || hv.len() != pts.len() {
                continue;
            }
            let Some((m, slope)) = level_fit(&pts, &hv) else { continue };
            let flat = slope < FLAT;
            let kind = if k == fl || flat && m.abs() < FLOOR_LEVEL {
                Kind::Floor
            } else if k == s || flat {
                Kind::Level
            } else {
                Kind::Upright
            };
            if let (Some(kd), Some(ht), Some(b)) = (kinds.get_mut(k), heights.get_mut(k), best.get_mut(k)) {
                *kd = kind;
                *ht = Some(m);
                *b = ff.pixels;
            }
        }
        if floor_face.is_none() {
            floor_face = u16::try_from(fl + 1).ok();
        }
    }
    (kinds, heights, floor_face)
}

/// The height over a face as a plane `h = c + g . x` in conditioned camera coordinates: its
/// median and the length of `g` (height per half the picture's width). A face parallel to the
/// floor has the same height everywhere; a thin strip of an upright face nearly so too, which is
/// why the slope is fitted rather than the spread measured.
fn level_fit(pts: &[Vector3<f64>], hv: &[f64]) -> Option<(f64, f64)> {
    let mut a = Matrix3::<f64>::zeros();
    let mut b = Vector3::<f64>::zeros();
    for (x, h) in pts.iter().zip(hv) {
        if !h.is_finite() {
            continue;
        }
        let r = Vector3::new(1.0, x[0], x[1]);
        a += r * r.transpose();
        b += r * *h;
    }
    let c = a.try_inverse()? * b;
    let mut v = hv.to_vec();
    Some((median(&mut v), c.y.hypot(c.z)))
}

/// How [`shape`] cleans the regions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeOptions {
    /// Seams between a region's faces up to about twice this wide are closed, camera pixels.
    pub seam: u32,
    /// The edge is smoothed by a Gaussian of this sigma, camera pixels (0: not at all).
    pub smooth: f32,
    /// A region that is a quadrilateral gets four straight edges.
    pub straight: bool,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self { seam: 10, smooth: 2.5, straight: true }
    }
}

impl ShapeOptions {
    pub fn check(&self) -> Result<(), String> {
        if self.seam > 64 {
            return Err("`seam` is 0 to 64 camera pixels".into());
        }
        if !self.smooth.is_finite() || !(0.0..=32.0).contains(&self.smooth) {
            return Err("`smooth` is 0 to 32 camera pixels".into());
        }
        Ok(())
    }
}

/// The regions cleaned: per camera pixel 0 or its region, and per region its straight corners
/// where it got them (far left first, as [`outline`] gives them).
#[derive(Clone, Debug)]
pub struct Shaped {
    pub labels: Vec<u16>,
    pub straight: Vec<Option<[[f64; 2]; 4]>>,
}

/// Each region (label `k`, `count` of them, in `base`: per camera pixel the region of the face
/// there, 0 for none or a disabled face) joined and cleaned: its seams closed and holes filled
/// with `free` pixels (nothing measured there, neither face nor object), the edge smoothed, and
/// replaced by its straight quadrilateral where that fits. Objects (`objects`) stay out; where
/// two regions claim a pixel, the one whose own face lies nearest gets it.
pub fn shape(
    w: usize,
    h: usize,
    base: &[u16],
    free: &[bool],
    objects: &[bool],
    count: usize,
    o: &ShapeOptions,
) -> Shaped {
    let n = w * h;
    let mut labels = vec![0u16; base.len()];
    let mut straight: Vec<Option<[[f64; 2]; 4]>> = vec![None; count];
    if base.len() != n || free.len() != n || objects.len() != n {
        return Shaped { labels, straight };
    }
    let any: Vec<bool> = base.iter().map(|l| *l > 0).collect();
    let (_, nearest) = raster::distance(w, h, &any);
    let nearest_label = |i: usize| -> u16 {
        nearest.get(i).copied().flatten().and_then(|j| base.get(j as usize).copied()).unwrap_or(0)
    };
    let margin =
        (2 * o.seam as usize + (3.0 * o.smooth).ceil() as usize + STRAIGHT_CLOSE_MOST as usize + 2).min(256);
    for (k, corners) in straight.iter_mut().enumerate() {
        let Ok(r) = u16::try_from(k + 1) else { break };
        let Some(rect) = raster::bounds(w, base, |l| *l == r) else { continue };
        let rect = rect.grown(margin, w, h);
        let (rw, rh) = (rect.width(), rect.height());
        let m: Vec<bool> = rect.crop(w, base).iter().map(|l| *l == r).collect();
        let fr = rect.crop(w, free);
        let ob = rect.crop(w, objects);
        let bc = rect.crop(w, base);
        let own_or_free = |j: usize| m.get(j) == Some(&true) || fr.get(j) == Some(&true);
        // Seams and holes.
        let mut mask: Vec<bool> =
            raster::close(rw, rh, &m, o.seam).iter().enumerate().map(|(j, c)| *c && own_or_free(j)).collect();
        let filled = raster::fill_holes(rw, rh, &mask);
        for (j, f) in filled.iter().enumerate() {
            if *f
                && fr.get(j) == Some(&true)
                && let Some(x) = mask.get_mut(j)
            {
                *x = true;
            }
        }
        // The ragged edge.
        if o.smooth > 0.0 {
            let v: Vec<f32> = mask.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
            let g = raster::gaussian(rw, rh, &v, o.smooth);
            let smooth: Vec<bool> = g
                .iter()
                .enumerate()
                .map(|(j, x)| *x >= 0.5 && own_or_free(j) && ob.get(j) != Some(&true))
                .collect();
            let before = mask.iter().filter(|b| **b).count();
            let after = smooth.iter().filter(|b| **b).count();
            if after as f64 >= SMOOTH_KEEP * before as f64 {
                mask = smooth;
            }
        }
        // Straight edges where the region is a quadrilateral.
        if o.straight {
            let hidden = raster::dilate(rw, rh, &ob, STRAIGHT_HIDDEN);
            let region = mask.iter().filter(|b| **b).count();
            // The lines are fitted to the region with its notches closed (a crate's handle
            // recess, a gap between two rib strips), so they follow its outer edges; deeper
            // notches need more closing, which rounds the corners of others, so it grows until
            // a quadrilateral fits.
            // Where the region runs into the picture's frame, it is carried a few pixels past
            // it, so the frame gives an edge too (the outline leaves the border at its own frame
            // out).
            let pad = FRAME_PAD;
            let (pw, ph) = (rw + 2 * pad, rh + 2 * pad);
            let open = [rect.x0 == 0, rect.x1 == w, rect.y0 == 0, rect.y1 == h];
            let padded = |src: &[bool]| -> Vec<bool> {
                (0..pw * ph)
                    .map(|j| {
                        let (x, y) = ((j % pw) as isize - pad as isize, (j / pw) as isize - pad as isize);
                        let past = [x < 0, x >= rw as isize, y < 0, y >= rh as isize];
                        if past.iter().zip(&open).any(|(p, o)| *p && !*o) {
                            return false;
                        }
                        let (cx, cy) =
                            (x.clamp(0, rw as isize - 1) as usize, y.clamp(0, rh as isize - 1) as usize);
                        src.get(cy * rw + cx).copied().unwrap_or(false)
                    })
                    .collect()
            };
            let (mask_p, hidden_p) = (padded(&mask), padded(&hidden));
            for close in STRAIGHT_CLOSE {
                let closed = raster::close(pw, ph, &mask_p, close);
                let Ok(out) = outline(pw, ph, &closed, &hidden_p) else { continue };
                let q = out.corners.map(|c| [c[0] - pad as f64, c[1] - pad as f64]);
                let quad: Vec<bool> = (0..rw * rh)
                    .map(|j| {
                        let p = [(j % rw.max(1)) as f64 + 0.5, (j / rw.max(1)) as f64 + 0.5];
                        inside_quad(&q, p)
                            && ob.get(j) != Some(&true)
                            && bc.get(j).is_some_and(|l| *l == 0 || *l == r)
                    })
                    .collect();
                // The quadrilateral fits when the region hardly sticks out of it, it hardly
                // covers other faces (switched off ones, such as the floor), and the region
                // fills most of it; what nothing measured inside it (seams, a dark corner) does
                // not count against it.
                let inside = quad.iter().zip(&mask).filter(|(a, b)| **a && **b).count();
                let area = quad.iter().filter(|q| **q).count();
                let foreign = quad
                    .iter()
                    .enumerate()
                    .filter(|(j, q)| {
                        **q && !m.get(*j).copied().unwrap_or(false) && fr.get(*j) == Some(&false)
                    })
                    .count();
                if region > 0
                    && area > 0
                    && (region - inside) as f64 <= STRAIGHT_OUT * region as f64
                    && foreign as f64 <= STRAIGHT_FOREIGN * area as f64
                    && inside as f64 >= STRAIGHT_FILL * area as f64
                {
                    mask = quad;
                    let (dx, dy) = (rect.x0 as f64, rect.y0 as f64);
                    *corners = Some(q.map(|c| [c[0] + dx, c[1] + dy]));
                    break;
                }
            }
        }
        for (j, on) in mask.iter().enumerate() {
            if !*on {
                continue;
            }
            let i = (rect.y0 + j / rw.max(1)) * w + rect.x0 + j % rw.max(1);
            let Some(l) = labels.get_mut(i) else { continue };
            if *l == 0 || (*l != r && nearest_label(i) == r) {
                *l = r;
            }
        }
    }
    Shaped { labels, straight }
}

/// `p` lies inside the convex quadrilateral `q` (either winding).
fn inside_quad(q: &[[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut sign = 0.0f64;
    for (k, a) in q.iter().enumerate() {
        let b = q.get((k + 1) % 4).copied().unwrap_or(*a);
        let c = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if c == 0.0 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// A projector's regions: per projector pixel the camera region it lights (`regions`, a
/// `camera`-sized label picture) through `to_camera` (its `proj_to_cam` map), none on its
/// objects (`objects`, its object labels), then each region closed over gaps up to about twice
/// [`PROJECTOR_GAP`]. A pixel the decode only filled in (a rib groove it left dark) counts when
/// the nearest measured pixel lies in the same region: at an edge where the camera sees another
/// surface behind, its filled position lands on that surface's region and does not.
pub fn projector_regions(
    camera: (usize, usize),
    regions: &[u16],
    to_camera: &Map,
    objects: &[u16],
) -> Vec<u16> {
    let (pw, ph) = (to_camera.width, to_camera.height);
    let region_at = |j: usize| -> u16 {
        if objects.get(j).is_some_and(|o| *o > 0) {
            return 0;
        }
        to_camera.pixel_in(j, camera.0, camera.1).and_then(|c| regions.get(c).copied()).unwrap_or(0)
    };
    let measured: Vec<u16> =
        (0..to_camera.len()).map(|j| if to_camera.measured(j) { region_at(j) } else { 0 }).collect();
    let source: Vec<bool> = (0..to_camera.len()).map(|j| to_camera.measured(j)).collect();
    let (_, nearest) = raster::distance(pw, ph, &source);
    let labels: Vec<u16> = (0..to_camera.len())
        .map(|j| {
            if to_camera.measured(j) {
                return measured.get(j).copied().unwrap_or(0);
            }
            let l = region_at(j);
            let near = nearest.get(j).copied().flatten().and_then(|k| measured.get(k as usize).copied());
            if l > 0 && near == Some(l) { l } else { 0 }
        })
        .collect();
    crate::scene::fill_gaps(pw, ph, &labels, PROJECTOR_GAP)
        .into_iter()
        .zip(objects)
        .map(|(l, o)| if *o > 0 { 0 } else { l })
        .collect()
}

/// A face's address: the pixel deepest inside it (camera pixels), per face (`count` of them).
pub fn face_centres(w: usize, h: usize, planes: &[u16], count: usize) -> Vec<Option<[usize; 2]>> {
    let mut out = vec![None; count];
    if planes.len() != w * h {
        return out;
    }
    let edge = edges(w, h, planes);
    let (d, _) = raster::distance(w, h, &edge);
    let mut best = vec![-1.0f32; count];
    for (i, l) in planes.iter().enumerate() {
        let Some(k) = usize::from(*l).checked_sub(1) else { continue };
        let v = d.get(i).copied().unwrap_or(0.0);
        if let (Some(b), Some(o)) = (best.get_mut(k), out.get_mut(k))
            && v > *b
        {
            *b = v;
            *o = Some([i % w, i / w]);
        }
    }
    out
}

/// The pixels on no face or on a face's edge (a neighbour on another label or outside).
fn edges(w: usize, h: usize, planes: &[u16]) -> Vec<bool> {
    planes
        .iter()
        .enumerate()
        .map(|(i, l)| {
            *l == 0 || raster::neighbours4(i, w, h).iter().any(|j| j.is_none_or(|j| planes.get(j) != Some(l)))
        })
        .collect()
}

/// The face under camera pixel `at`, or the nearest one within `reach` pixels.
pub fn face_at(w: usize, h: usize, planes: &[u16], at: [f64; 2], reach: u32) -> Option<u16> {
    if !at.iter().all(|v| v.is_finite()) || at[0] < 0.0 || at[1] < 0.0 {
        return None;
    }
    let (x, y) = (at[0] as usize, at[1] as usize);
    if x >= w || y >= h {
        return None;
    }
    let r = reach as isize;
    let mut best: Option<(isize, u16)> = None;
    for dy in -r..=r {
        for dx in -r..=r {
            let d = dx * dx + dy * dy;
            if d > r * r || best.is_some_and(|(b, _)| b <= d) {
                continue;
            }
            let (xx, yy) = (x as isize + dx, y as isize + dy);
            if xx < 0 || yy < 0 || xx as usize >= w || yy as usize >= h {
                continue;
            }
            if let Some(l) = planes.get(yy as usize * w + xx as usize)
                && *l > 0
            {
                best = Some((d, *l));
            }
        }
    }
    best.map(|(_, l)| l)
}

/// A face that probably belongs to another one's region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suggestion {
    /// The thin face and the large one it lies next to.
    pub strip: u16,
    pub panel: u16,
    /// Pixels along which they touch (across a seam).
    pub contact: usize,
    /// The strip's median distance from the panel's plane, camera pixels, in the projector where
    /// it is least.
    pub error_px: f64,
}

/// Thin faces (rib strips, rims) lying on the plane of a large face they touch: per strip the
/// panel it touches longest. `planes` the camera labels, `objects` the camera's object mask,
/// `fits` and `kinds` as [`fit_faces`] and [`classify`] give them.
pub fn suggestions(
    w: usize,
    h: usize,
    planes: &[u16],
    objects: &[bool],
    fits: &[Vec<Option<FaceFit>>],
    kinds: &[Kind],
) -> Vec<Suggestion> {
    let count = fits.len();
    if planes.len() != w * h || objects.len() != w * h {
        return Vec::new();
    }
    let (d, _) = raster::distance(w, h, &edges(w, h, planes));
    let mut radius = vec![0f32; count];
    let mut size = vec![0usize; count];
    for (i, l) in planes.iter().enumerate() {
        let Some(k) = usize::from(*l).checked_sub(1) else { continue };
        if let (Some(r), Some(s)) = (radius.get_mut(k), size.get_mut(k)) {
            *r = r.max(d.get(i).copied().unwrap_or(0.0));
            *s += 1;
        }
    }
    // Contacts across seams: the labels grown a little into the pixels nothing measured.
    let labelled: Vec<bool> = planes.iter().map(|l| *l > 0).collect();
    let near = raster::city_distance(w, h, &labelled);
    let within: Vec<bool> = (0..w * h)
        .map(|i| {
            planes.get(i) == Some(&0)
                && objects.get(i) != Some(&true)
                && near.get(i).is_some_and(|d| *d <= SUGGEST_GAP)
        })
        .collect();
    let mut grown = planes.to_vec();
    raster::grow(w, h, &mut grown, &within);
    let mut contact: std::collections::HashMap<(u16, u16), usize> = std::collections::HashMap::new();
    for i in 0..w * h {
        let a = grown.get(i).copied().unwrap_or(0);
        if a == 0 {
            continue;
        }
        for j in [(i % w + 1 < w).then_some(i + 1), (i + w < w * h).then_some(i + w)].into_iter().flatten() {
            let b = grown.get(j).copied().unwrap_or(0);
            if b > 0 && b != a {
                *contact.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
    }
    let mut out = Vec::new();
    for s in 0..count {
        let (Some(rs), Some(ss)) = (radius.get(s).copied(), size.get(s).copied()) else { continue };
        if rs > STRIP_RADIUS || ss == 0 {
            continue;
        }
        let Ok(sl) = u16::try_from(s + 1) else { continue };
        let mut best: Option<Suggestion> = None;
        for g in 0..count {
            let Ok(gl) = u16::try_from(g + 1) else { continue };
            let c = contact.get(&(sl.min(gl), sl.max(gl))).copied().unwrap_or(0);
            if g == s
                || c < SUGGEST_CONTACT
                || radius.get(g).copied().unwrap_or(0.0) <= STRIP_RADIUS
                || size.get(g).copied().unwrap_or(0) < PANEL_SHARE * ss
            {
                continue;
            }
            let (ks, kg) = (
                kinds.get(s).copied().unwrap_or(Kind::Unknown),
                kinds.get(g).copied().unwrap_or(Kind::Unknown),
            );
            if ks != Kind::Unknown && kg != Kind::Unknown && ks != kg {
                continue;
            }
            // Its distance from the panel's plane, in the projector where that is least (a
            // projector that lights only a corner of the panel fits it poorly).
            let error = (0..fits.get(s).map_or(0, |f| f.len()))
                .filter_map(|p| {
                    let fs = fits.get(s)?.get(p)?.as_ref()?;
                    let fg = fits.get(g)?.get(p)?.as_ref()?;
                    Some(fs.median_error(&fg.homography))
                })
                .min_by(f64::total_cmp);
            let Some(error) = error.filter(|e| *e <= SUGGEST_PX) else { continue };
            if best.is_none_or(|b| c > b.contact) {
                best = Some(Suggestion { strip: sl, panel: gl, contact: c, error_px: error });
            }
        }
        out.extend(best);
    }
    out
}
