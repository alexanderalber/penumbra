//! Flat faces and the objects among them from one structured light scan, without a scan of
//! the empty scene.
//!
//! **Faces** ([`analyse`]): every flat surface a projector lights (floor, wall, each face of
//! each box) maps its projector pixels to camera pixels by a homography of its own; whatever is
//! not flat fits none of them. So the fits are found one after the other by RANSAC over the
//! projector's `proj_to_cam` pairs: hypotheses from four nearby measured pixels (a face is a
//! contiguous region, so four pixels close together usually lie on one), scored on an even
//! sample of the pixels not yet taken, the best refined by least squares
//! ([`Homography::fit_cauchy`]) over all its inliers. Its region is the inliers, closed and
//! opened (which drops the thin bands where a curved object crosses the plane); each connected
//! piece holding [`SceneOptions::min_plane`] is a face of its own, refitted on its own pixels
//! (coplanar faces of different boxes share the fit, not the face). Their pixels are taken out
//! and the next fit is searched among the rest, until the best fit holds no such face.
//!
//! A curved object holds patches that are flat within the tolerance. Such a patch is dropped
//! when a smooth bowl explains most of its error ([`bend`]), or when it is enclosed by the
//! object, its border touching no other face, no frame edge and no unlit pixel. Last, where two
//! faces meet, each pixel goes to the face nearby that fits it best (the face found first took
//! the crease).
//!
//! **Objects:** the measured pixels on no face, closed, opened, holes filled, small pieces
//! dropped, numbered by size.
//!
//! **Height** ([`relief`]): an object's residual against the face it stands on, along its main
//! direction, as `scan.relief` does against a single plane: camera pixels of parallax, up to
//! scale. The sign comes from the other faces the camera sees below the support (the front of
//! the box, the floor in front of it), which lie under the support's plane, where the object
//! lies above; without them, from the object's edge next to its support, which is its low part.
//!
//! **Outline** ([`outline`]): a rectangular face (a box's top or front) seen by the camera is a
//! quadrilateral, partly hidden by an object in front. Straight lines fitted by RANSAC to its
//! border in the camera picture, without the border along the object, give its four edges also
//! where they run under the object; their crossings are the corners.

use crate::homography::Homography;
use crate::objects::{MAX_PARTS, Map, eroded_labels, keep_large};
use crate::raster;
use crate::relief::{Heuristic, Relief, fill, main_direction, quantile};

/// Most faces found (label pictures are 8-bit).
pub const MAX_PLANES: usize = 255;
/// Fewest measured pixels a projector must have.
pub const MIN_MEASURED: usize = 100;
/// The RANSAC sample: about this many cells along the projector's longer side, one pixel each.
const SAMPLE_CELLS: usize = 96;
const MIN_CELL: usize = 2;
/// Hypotheses per plane, and how far (cells) the other three pixels lie from the first.
const HYPOTHESES: usize = 400;
const REACH: usize = 6;
/// Refits of a plane on its inliers.
const REFITS: usize = 3;
const CAUCHY_ROUNDS: usize = 2;
/// An unmeasured pixel takes the class of the nearest measured one this close, projector
/// pixels (a camera coarser than the projector measures only some of them).
const GAP: f32 = 6.0;
/// Closing and opening of a plane's inliers, projector pixels.
const REGION_CLOSE: u32 = 2;
const REGION_OPEN: u32 = 4;
/// A plane's piece counts when it holds this share of the plane's least size.
const PIECE_SHARE: usize = 4;
/// A plane is kept when this share of its border lies within [`BORDER_REACH`] projector pixels
/// of another plane, the frame or an unlit pixel.
const OPEN_BORDER: f64 = 0.15;
const BORDER_REACH: u32 = 3;
/// A face is a patch of a curved surface, no plane, when a bowl explains more than this share
/// of its error ([`bend`]) and the bowl's RMS is above this, camera pixels. Measured: planes of a
/// real scan 0.003 to 0.28 (the most on a wall filling the frame, the lens's distortion), patches
/// of a ray-cast ball 0.77 to 0.94.
const BEND_SHARE: f64 = 0.5;
const BEND_PX: f64 = 0.3;
/// How far (projector pixels) a pixel looks for a better fitting face at a crease.
const SETTLE: u32 = 3;
/// Closing and opening of the objects, projector pixels.
const OBJECT_CLOSE: u32 = 3;
const OBJECT_OPEN: u32 = 3;
/// Pixels of the other planes below the support needed to decide the sign by them.
const BELOW_MIN: usize = 100;
/// An object's pixels this close to its support (projector pixels) are its contact.
const CONTACT: u32 = 3;
/// The outline: RANSAC rounds per edge, the band (camera pixels) around a line its points lie
/// in, the least share of the border points an edge holds, and the least angle between two
/// neighbouring edges (degrees).
const LINE_HYPOTHESES: usize = 500;
const LINE_BAND: f64 = 1.5;
const LINE_SHARE: f64 = 0.04;
const MIN_CORNER_DEG: f64 = 20.0;
/// Border points this close to the picture's frame (camera pixels) are left out.
const FRAME_MARGIN: usize = 2;

/// How [`analyse`] finds planes and objects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneOptions {
    /// A pixel lies on a plane when the plane's homography puts it this close to where the
    /// camera saw it, camera pixels.
    pub tolerance: f32,
    /// The smallest face, as a share of the projector's measured pixels; the search ends when
    /// the best remaining fit holds no face this large.
    pub min_plane: f32,
    /// Most faces kept.
    pub max_planes: usize,
    /// The smallest object, projector pixels.
    pub min_object: usize,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self { tolerance: 2.0, min_plane: 0.002, max_planes: MAX_PLANES, min_object: 2000 }
    }
}

impl SceneOptions {
    pub fn check(&self) -> Result<(), String> {
        if !self.tolerance.is_finite() || !(0.05..=100.0).contains(&self.tolerance) {
            return Err("`tolerance` is 0.05 to 100 camera pixels".into());
        }
        if !self.min_plane.is_finite() || !(0.001..=0.9).contains(&self.min_plane) {
            return Err("`min_plane` is 0.001 to 0.9 of the measured pixels".into());
        }
        if !(1..=MAX_PLANES).contains(&self.max_planes) {
            return Err(format!("`max_planes` is 1 to {MAX_PLANES}"));
        }
        Ok(())
    }
}

/// One plane a projector lights.
#[derive(Clone, Debug)]
pub struct Plane {
    /// Projector pixels (centres at +0.5) to camera pixels.
    pub homography: Homography,
    /// Measured pixels on it.
    pub pixels: usize,
    /// Median and 90th percentile of its pixels' error, camera pixels.
    pub error_px: [f64; 2],
    /// Faces of one group came from one fit: coplanar, e.g. the tops of boxes side by side.
    pub group: usize,
    /// How much of its pixels' error a smooth bowl explains: the share and its RMS, camera
    /// pixels ([`bend`]).
    pub bend: [f64; 2],
}

/// One projector's planes and objects.
#[derive(Clone, Debug)]
pub struct SceneParts {
    pub width: usize,
    pub height: usize,
    /// Face `k` is label `k + 1` in [`SceneParts::plane_labels`], in the order found.
    pub planes: Vec<Plane>,
    /// Per projector pixel 0 or its face.
    pub plane_labels: Vec<u16>,
    /// Per projector pixel 0 or its object, largest first.
    pub object_labels: Vec<u16>,
    pub objects: usize,
    pub measured: usize,
    /// Measured pixels on neither a plane nor an object.
    pub unexplained: usize,
    /// Faces found and dropped again: curved, or enclosed by an object.
    pub dropped_planes: usize,
}

/// A small deterministic generator: the same scan gives the same planes on every run.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// The measured pairs of a projector: pixel indices, projector and camera positions.
struct Pairs {
    idx: Vec<usize>,
    from: Vec<[f64; 2]>,
    to: Vec<[f64; 2]>,
}

impl Pairs {
    fn of(map: &Map, camera: (usize, usize)) -> Self {
        let (w, cw, ch) = (map.width.max(1), camera.0 as f64, camera.1 as f64);
        let idx: Vec<usize> = (0..map.len()).filter(|i| map.measured(*i)).collect();
        let from = idx.iter().map(|i| [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]).collect();
        let to = idx
            .iter()
            .map(|i| map.uv(*i).map_or([f64::NAN; 2], |[u, v]| [f64::from(u) * cw, f64::from(v) * ch]))
            .collect();
        Self { idx, from, to }
    }

    fn error(&self, h: &Homography, k: usize) -> f64 {
        match (self.from.get(k), self.to.get(k)) {
            (Some(p), Some(q)) => h.apply(*p).map_or(f64::INFINITY, |r| (r[0] - q[0]).hypot(r[1] - q[1])),
            _ => f64::INFINITY,
        }
    }

    fn residual(&self, h: &Homography, k: usize) -> Option<[f64; 2]> {
        let (p, q) = (self.from.get(k)?, self.to.get(k)?);
        let r = h.apply(*p)?;
        let d = [q[0] - r[0], q[1] - r[1]];
        d.iter().all(|v| v.is_finite()).then_some(d)
    }

    fn fit(&self, sel: impl Fn(usize) -> bool) -> Option<Homography> {
        let (f, t): (Vec<[f64; 2]>, Vec<[f64; 2]>) = self
            .from
            .iter()
            .zip(&self.to)
            .enumerate()
            .filter(|(k, _)| sel(*k))
            .map(|(_, (f, t))| (*f, *t))
            .unzip();
        if f.len() < 4 {
            return None;
        }
        Homography::fit_cauchy(&f, &t, CAUCHY_ROUNDS).ok()
    }
}

/// The even sample RANSAC scores on: one measured pixel per cell.
struct Sample {
    cell: usize,
    gw: usize,
    gh: usize,
    /// Per cell the pair there, `u32::MAX` for none.
    at: Vec<u32>,
}

impl Sample {
    fn of(w: usize, h: usize, pairs: &Pairs) -> Self {
        let cell = (w.max(h) / SAMPLE_CELLS).max(MIN_CELL);
        let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
        let mut at = vec![u32::MAX; gw * gh];
        let w = w.max(1);
        for (k, i) in pairs.idx.iter().enumerate() {
            let c = (i / w / cell) * gw + (i % w) / cell;
            if let Some(s) = at.get_mut(c)
                && *s == u32::MAX
            {
                *s = k as u32;
            }
        }
        Self { cell, gw, gh, at }
    }
}

/// The planes and objects one projector lights: `map` is its `proj_to_cam` map, `camera` the
/// camera picture's size.
pub fn analyse(map: &Map, camera: (usize, usize), o: &SceneOptions) -> Result<SceneParts, String> {
    o.check()?;
    if camera.0 == 0 || camera.1 == 0 {
        return Err("the camera picture is empty".into());
    }
    let (w, h) = (map.width, map.height);
    let n = map.len();
    let pairs = Pairs::of(map, camera);
    if pairs.idx.len() < MIN_MEASURED {
        return Err(format!("only {} measured projector pixels (at least {MIN_MEASURED})", pairs.idx.len()));
    }
    let tol = f64::from(o.tolerance);
    let least = ((f64::from(o.min_plane) * pairs.idx.len() as f64).ceil() as usize).max(4);
    let sample = Sample::of(w, h, &pairs);
    let measured: Vec<bool> = (0..n).map(|i| map.measured(i)).collect();
    let (dist, nearest) = raster::distance(w, h, &measured);
    let mut point_at = vec![u32::MAX; n];
    for (k, i) in pairs.idx.iter().enumerate() {
        if let Some(p) = point_at.get_mut(*i) {
            *p = k as u32;
        }
    }
    // Per pixel with a position the pair of the nearest measured pixel within the gap (a pixel
    // without one lit nothing the camera saw).
    let near: Vec<u32> = nearest
        .iter()
        .zip(&dist)
        .enumerate()
        .map(|(i, (j, d))| match j {
            Some(j) if *d <= GAP && map.uv(i).is_some() => {
                point_at.get(*j as usize).copied().unwrap_or(u32::MAX)
            }
            _ => u32::MAX,
        })
        .collect();
    let mut free = vec![true; pairs.idx.len()];
    let mut rng = Lcg(0x5eed);
    // Faces and the fit (group) each came from, until a fit holds no face of the least size.
    let mut found: Vec<(Face, usize)> = Vec::new();
    let mut group = 0usize;
    // Pairs of faces dropped as curved: no plane, but no longer searched.
    let mut curved = vec![false; pairs.idx.len()];
    let mut dropped_planes = 0usize;
    while found.len() < o.max_planes {
        let faces = find_plane(w, h, &pairs, &near, &sample, &free, tol, least, &mut rng);
        if faces.is_empty() {
            break;
        }
        for f in faces {
            let bent = f.bend[0] > BEND_SHARE && f.bend[1] > BEND_PX;
            for k in &f.inliers {
                if let Some(x) = free.get_mut(*k) {
                    *x = false;
                }
                if bent && let Some(c) = curved.get_mut(*k) {
                    *c = true;
                }
            }
            if bent {
                dropped_planes += 1;
            } else if found.len() < o.max_planes {
                found.push((f, group));
            }
        }
        group += 1;
    }

    // Patches inside an object are no planes: drop the enclosed ones, again while dropping
    // leaves others enclosed.
    let unlit: Vec<bool> = (0..n).map(|i| map.uv(i).is_none()).collect();
    for _ in 0..MAX_PLANES {
        let labels = plane_labels(n, &found);
        let shares = open_borders(w, h, &labels, &unlit, found.len());
        let before = found.len();
        let mut k = 0usize;
        found.retain(|(f, _)| {
            let keep = shares.get(k).is_none_or(|s| *s >= OPEN_BORDER);
            k += 1;
            if !keep {
                for i in &f.inliers {
                    if let Some(x) = free.get_mut(*i) {
                        *x = true;
                    }
                }
            }
            keep
        });
        dropped_planes += before - found.len();
        if found.len() == before {
            break;
        }
    }
    let mut labels = settle(w, h, &plane_labels(n, &found), &found, &pairs, &point_at, &near, tol);

    // The objects: pixels on no plane whose nearest measured pixel is on none either.
    let left: Vec<bool> = near
        .iter()
        .zip(&labels)
        .map(|(k, l)| {
            let k = *k as usize;
            *l == 0 && (free.get(k) == Some(&true) || curved.get(k) == Some(&true))
        })
        .collect();
    let mask = raster::close(w, h, &left, OBJECT_CLOSE);
    let mask = raster::open(w, h, &mask, OBJECT_OPEN);
    let mask = raster::fill_holes(w, h, &mask);
    let mask = keep_large(w, h, &mask, o.min_object);
    let (comp, sizes) = raster::components(w, h, &mask);
    let mut order: Vec<(usize, usize)> = sizes.iter().enumerate().map(|(k, s)| (*s, k + 1)).collect();
    order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    order.truncate(MAX_PARTS);
    let mut number = vec![0u16; sizes.len() + 1];
    for (k, (_, c)) in order.iter().enumerate() {
        if let Some(m) = number.get_mut(*c) {
            *m = u16::try_from(k + 1).unwrap_or(0);
        }
    }
    let object_labels: Vec<u16> =
        comp.iter().map(|c| number.get(*c as usize).copied().unwrap_or(0)).collect();
    // The object stands in front: where its closing reaches into a plane, the object wins.
    for (l, ob) in labels.iter_mut().zip(&object_labels) {
        if *ob > 0 {
            *l = 0;
        }
    }

    let planes: Vec<Plane> = found
        .iter()
        .map(|(f, group)| {
            let mut e: Vec<f64> = f.inliers.iter().map(|k| pairs.error(&f.homography, *k)).collect();
            e.sort_by(f64::total_cmp);
            Plane {
                homography: f.homography,
                pixels: f.inliers.len(),
                error_px: [quantile(&e, 0.5), quantile(&e, 0.9)],
                group: *group,
                bend: f.bend,
            }
        })
        .collect();
    let unexplained = pairs
        .idx
        .iter()
        .filter(|i| labels.get(**i) == Some(&0) && object_labels.get(**i) == Some(&0))
        .count();
    Ok(SceneParts {
        width: w,
        height: h,
        planes,
        plane_labels: labels,
        object_labels,
        objects: order.len(),
        measured: pairs.idx.len(),
        unexplained,
        dropped_planes,
    })
}

/// Per pixel the first face whose region holds it.
fn plane_labels(n: usize, found: &[(Face, usize)]) -> Vec<u16> {
    let mut labels = vec![0u16; n];
    for (k, (f, _)) in found.iter().enumerate() {
        let id = u16::try_from(k + 1).unwrap_or(0);
        for i in &f.pixels {
            if let Some(l) = labels.get_mut(*i as usize)
                && *l == 0
            {
                *l = id;
            }
        }
    }
    labels
}

/// Faces found earlier took the pixels near their creases that fit both them and a later face
/// within the tolerance: each measured pixel goes to the face nearby (within [`SETTLE`]) that
/// fits it best, each unmeasured one follows its nearest measured pixel.
#[allow(clippy::too_many_arguments)]
fn settle(
    w: usize,
    h: usize,
    labels: &[u16],
    found: &[(Face, usize)],
    pairs: &Pairs,
    point_at: &[u32],
    near: &[u32],
    tol: f64,
) -> Vec<u16> {
    let r = SETTLE as i64;
    let mut out = labels.to_vec();
    let mut seen: Vec<u16> = Vec::new();
    for (k, i) in pairs.idx.iter().enumerate() {
        let Some(&own) = labels.get(*i) else { continue };
        if own == 0 {
            continue;
        }
        let (x, y) = ((i % w.max(1)) as i64, (i / w.max(1)) as i64);
        seen.clear();
        for dy in -r..=r {
            for dx in -r..=r {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                if let Some(&l) = labels.get(ny as usize * w + nx as usize)
                    && l != 0
                    && !seen.contains(&l)
                {
                    seen.push(l);
                }
            }
        }
        let error = |l: u16| {
            found.get(usize::from(l) - 1).map_or(f64::INFINITY, |(f, _)| pairs.error(&f.homography, k))
        };
        let best = seen.iter().copied().map(|l| (l, error(l))).min_by(|a, b| a.1.total_cmp(&b.1));
        if let (Some((l, e)), Some(o)) = (best, out.get_mut(*i))
            && e < tol
        {
            *o = l;
        }
    }
    let measured_out = out.clone();
    for (i, o) in out.iter_mut().enumerate() {
        if *o == 0 || point_at.get(i).is_some_and(|p| *p != u32::MAX) {
            continue;
        }
        if let Some(&k) = near.get(i)
            && let Some(j) = pairs.idx.get(k as usize)
            && let Some(&l) = measured_out.get(*j)
            && l != 0
        {
            *o = l;
        }
    }
    out
}

/// Per label 1..=count the share of its border pixels within [`BORDER_REACH`] of another
/// label, the frame or an unlit pixel (1 for a label without border).
fn open_borders(w: usize, h: usize, labels: &[u16], unlit: &[bool], count: usize) -> Vec<f64> {
    let mut border = vec![0usize; count];
    let mut open = vec![0usize; count];
    let r = BORDER_REACH as i64;
    for (i, l) in labels.iter().enumerate() {
        if *l == 0 {
            continue;
        }
        let edge = raster::neighbours4(i, w, h).iter().any(|j| j.is_none_or(|j| labels.get(j) != Some(l)));
        if !edge {
            continue;
        }
        let (x, y) = ((i % w.max(1)) as i64, (i / w.max(1)) as i64);
        let mut is_open = false;
        'scan: for dy in -r..=r {
            for dx in -(r - dy.abs())..=(r - dy.abs()) {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    is_open = true;
                    break 'scan;
                }
                let j = ny as usize * w + nx as usize;
                let other = labels.get(j).is_some_and(|m| *m != 0 && m != l);
                if other || unlit.get(j) == Some(&true) {
                    is_open = true;
                    break 'scan;
                }
            }
        }
        let k = usize::from(*l) - 1;
        if let Some(b) = border.get_mut(k) {
            *b += 1;
        }
        if is_open && let Some(o) = open.get_mut(k) {
            *o += 1;
        }
    }
    border.iter().zip(&open).map(|(b, o)| if *b == 0 { 1.0 } else { *o as f64 / *b as f64 }).collect()
}

/// A connected face found by [`find_plane`]: its homography, pixels (its region, unmeasured
/// gaps included) and the pairs on it.
struct Face {
    homography: Homography,
    pixels: Vec<u32>,
    inliers: Vec<usize>,
    /// See [`bend`].
    bend: [f64; 2],
}

/// The best plane among the free pairs, as its faces of at least `least` inliers each; none
/// when it has no such face.
#[allow(clippy::too_many_arguments)]
fn find_plane(
    w: usize,
    h: usize,
    pairs: &Pairs,
    near: &[u32],
    sample: &Sample,
    free: &[bool],
    tol: f64,
    least: usize,
    rng: &mut Lcg,
) -> Vec<Face> {
    let is_free = |k: u32| free.get(k as usize) == Some(&true);
    let cand: Vec<(usize, u32)> = sample
        .at
        .iter()
        .enumerate()
        .filter(|(_, k)| **k != u32::MAX && is_free(**k))
        .map(|(c, k)| (c, *k))
        .collect();
    if cand.len() < 4 {
        return Vec::new();
    }
    let min_area = (sample.cell * sample.cell) as f64;
    let mut best: Option<(usize, Homography)> = None;
    let mut close_by: Vec<u32> = Vec::new();
    for _ in 0..HYPOTHESES {
        let Some(&(cell, seed)) = cand.get(rng.below(cand.len())) else { continue };
        let (cx, cy) = (cell % sample.gw.max(1), cell / sample.gw.max(1));
        close_by.clear();
        for y in cy.saturating_sub(REACH)..(cy + REACH + 1).min(sample.gh) {
            for x in cx.saturating_sub(REACH)..(cx + REACH + 1).min(sample.gw) {
                if let Some(&k) = sample.at.get(y * sample.gw + x)
                    && k != u32::MAX
                    && k != seed
                    && is_free(k)
                {
                    close_by.push(k);
                }
            }
        }
        if close_by.len() < 3 {
            continue;
        }
        let mut pick = [seed as usize; 4];
        for (slot, s) in pick.iter_mut().skip(1).enumerate() {
            // Three distinct picks: the list's remaining range shrinks by one each time.
            let j = rng.below(close_by.len() - slot);
            if let Some(k) = close_by.get(j) {
                *s = *k as usize;
            }
            let last = close_by.len() - 1 - slot;
            close_by.swap(j, last);
        }
        let points: Vec<[f64; 2]> = pick.iter().filter_map(|k| pairs.from.get(*k).copied()).collect();
        if points.len() < 4 || !spread(&points, min_area) {
            continue;
        }
        let quad: Vec<([f64; 2], [f64; 2])> =
            pick.iter().filter_map(|k| Some((*pairs.from.get(*k)?, *pairs.to.get(*k)?))).collect();
        let Ok(hyp) = Homography::from_pairs(&quad) else { continue };
        let score = cand.iter().filter(|(_, k)| pairs.error(&hyp, *k as usize) < tol).count();
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, hyp));
        }
    }
    let Some((_, mut hom)) = best else { return Vec::new() };
    let inlier = |hom: &Homography, k: usize| free.get(k) == Some(&true) && pairs.error(hom, k) < tol;
    for _ in 0..REFITS {
        match pairs.fit(|k| inlier(&hom, k)) {
            Some(next) => hom = next,
            None => break,
        }
    }
    let region_of = |hom: &Homography| -> Vec<bool> {
        let good: Vec<bool> = (0..pairs.idx.len()).map(|k| inlier(hom, k)).collect();
        let m: Vec<bool> =
            near.iter().map(|k| *k != u32::MAX && good.get(*k as usize) == Some(&true)).collect();
        let m = raster::close(w, h, &m, REGION_CLOSE);
        let m = raster::open(w, h, &m, REGION_OPEN);
        keep_large(w, h, &m, (least / PIECE_SHARE).max(1))
    };
    let region = region_of(&hom);
    let on_region = |hom: &Homography, k: usize| {
        inlier(hom, k) && pairs.idx.get(k).is_some_and(|i| region.get(*i) == Some(&true))
    };
    if let Some(next) = pairs.fit(|k| on_region(&hom, k)) {
        hom = next;
    }
    // Each connected piece is a face of its own (coplanar faces of different boxes share the
    // fit, not the face), refitted on its own pixels.
    let region = region_of(&hom);
    let (comp, sizes) = raster::components(w, h, &region);
    let mut pieces: Vec<Vec<u32>> = vec![Vec::new(); sizes.len()];
    for (i, c) in comp.iter().enumerate() {
        if *c > 0
            && let Some(p) = pieces.get_mut(*c as usize - 1)
        {
            p.push(i as u32);
        }
    }
    let mut faces = Vec::new();
    for pixels in pieces {
        let mut here = vec![false; pairs.idx.len()];
        for i in &pixels {
            if let Some(&k) = near.get(*i as usize)
                && pairs.idx.get(k as usize) == Some(&(*i as usize))
                && let Some(x) = here.get_mut(k as usize)
            {
                *x = true;
            }
        }
        let on = |hom: &Homography, k: usize| here.get(k) == Some(&true) && inlier(hom, k);
        if (0..pairs.idx.len()).filter(|k| on(&hom, *k)).count() < least {
            continue;
        }
        let own = pairs.fit(|k| on(&hom, k)).unwrap_or(hom);
        let inliers: Vec<usize> = (0..pairs.idx.len()).filter(|k| on(&own, *k)).collect();
        if inliers.len() >= least {
            let bend = bend(pairs, &own, &inliers);
            faces.push(Face { homography: own, pixels, inliers, bend });
        }
    }
    faces
}

/// Whether every three of the four points span a triangle of at least `min_area`.
fn spread(p: &[[f64; 2]], min_area: f64) -> bool {
    let area = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
    };
    let (Some(a), Some(b), Some(c), Some(d)) = (p.first(), p.get(1), p.get(2), p.get(3)) else {
        return false;
    };
    [area(*a, *b, *c), area(*a, *b, *d), area(*a, *c, *d), area(*b, *c, *d)].iter().all(|s| *s >= min_area)
}

/// The height of the objects (`objects`, per projector pixel non-zero on an object) over their
/// support plane `support` (projector to camera), seen by one projector: `map` is its
/// `proj_to_cam` map, `planes` its plane labels and `support_label` the support's label among
/// them; the light mask is the objects shrunk by `erode`.
pub fn relief(
    map: &Map,
    camera: (usize, usize),
    support: &Homography,
    objects: &[u16],
    planes: &[u16],
    support_label: u16,
    erode: u32,
) -> Result<Relief, String> {
    let (w, h) = (map.width, map.height);
    let n = map.len();
    if objects.len() != n || planes.len() != n {
        return Err("the labels are not of the map's size".into());
    }
    let pairs = Pairs::of(map, camera);
    let on = |k: usize, labels: &[u16], pick: &dyn Fn(u16) -> bool| {
        pairs.idx.get(k).and_then(|i| labels.get(*i)).is_some_and(|l| pick(*l))
    };
    let object: Vec<(usize, [f64; 2])> = (0..pairs.idx.len())
        .filter(|k| on(*k, objects, &|l| l > 0))
        .filter_map(|k| Some((k, pairs.residual(support, k)?)))
        .collect();
    if object.len() < 4 {
        return Err("the objects hold fewer than 4 measured pixels".into());
    }
    let res: Vec<[f64; 2]> = object.iter().map(|(_, r)| *r).collect();
    let mut direction = main_direction(&res);
    let along = |r: &[f64; 2], d: [f64; 2]| (r[0] * d[0] + r[1] * d[1]) as f32;
    let median = |mut v: Vec<f32>| -> f32 {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };

    // The sign: the other planes seen below the support lie under its plane.
    let mut support_y: Vec<f64> = (0..pairs.idx.len())
        .filter(|k| on(*k, planes, &|l| l == support_label))
        .filter_map(|k| pairs.to.get(k).map(|t| t[1]))
        .collect();
    support_y.sort_by(f64::total_cmp);
    let mid_y = quantile(&support_y, 0.5);
    let below: Vec<f32> = (0..pairs.idx.len())
        .filter(|k| on(*k, planes, &|l| l != 0 && l != support_label))
        .filter(|k| pairs.to.get(*k).is_some_and(|t| t[1] > mid_y))
        .filter_map(|k| pairs.residual(support, k).map(|r| along(&r, direction)))
        .collect();
    let heuristic = if below.len() >= BELOW_MIN && mid_y.is_finite() {
        Heuristic::Below { pixels: below.len(), median: median(below) }
    } else {
        let on_support: Vec<bool> = planes.iter().map(|l| *l == support_label).collect();
        let near = raster::dilate(w, h, &on_support, CONTACT);
        let contact: Vec<f32> = object
            .iter()
            .filter(|(k, _)| pairs.idx.get(*k).is_some_and(|i| near.get(*i) == Some(&true)))
            .map(|(_, r)| along(r, direction))
            .collect();
        let all = median(res.iter().map(|r| along(r, direction)).collect());
        Heuristic::Contact { pixels: contact.len(), median: median(contact) - all }
    };
    let up = match heuristic {
        Heuristic::Below { median, .. } | Heuristic::Contact { median, .. } => median <= 0.0,
        _ => true,
    };
    if !up {
        direction = [-direction[0], -direction[1]];
    }

    let mut values = vec![0f32; n];
    let mut weights = vec![0f32; n];
    for (k, r) in &object {
        if let Some(i) = pairs.idx.get(*k)
            && let (Some(v), Some(wt)) = (values.get_mut(*i), weights.get_mut(*i))
        {
            *v = along(r, direction);
            *wt = 1.0;
        }
    }
    let heights = fill(w, h, &values, &weights);
    let piece: Vec<bool> = objects.iter().map(|l| *l > 0).collect();
    let mask: Vec<bool> = eroded_labels(w, h, objects, erode).iter().map(|l| *l > 0).collect();
    let mut errors: Vec<f64> = res.iter().map(|r| r[0].hypot(r[1])).collect();
    errors.sort_by(f64::total_cmp);
    Ok(Relief {
        width: w,
        height: h,
        plane: *support,
        direction,
        heights,
        piece,
        mask,
        measured: pairs.idx.len(),
        piece_measured: object.len(),
        error_px: [quantile(&errors, 0.5), quantile(&errors, 0.9)],
        heuristic,
        flipped: false,
    })
}

/// A straight edge `n . p = c` (`n` a unit normal) and the border points on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub normal: [f64; 2],
    pub offset: f64,
    pub points: usize,
    /// The mean of its points.
    pub centre: [f64; 2],
}

/// A rectangle's outline in the camera picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    /// Top left, top right, bottom right, bottom left (in the camera picture's terms: the far
    /// left corner first, then along the far edge), camera pixels.
    pub corners: [[f64; 2]; 4],
    /// The edges, in the corners' order: far, right, near, left.
    pub edges: [Edge; 4],
    /// Border points offered (without those along `hidden` and the frame).
    pub border: usize,
}

/// The four edges of a quadrilateral region (`mask`, a `w` x `h` picture) and its corners, its
/// border along `hidden` (an object standing in front) left out, so the edges run on under it.
pub fn outline(w: usize, h: usize, mask: &[bool], hidden: &[bool]) -> Result<Outline, String> {
    if mask.len() != w * h || hidden.len() != w * h {
        return Err("the masks are not of the picture's size".into());
    }
    let mut points: Vec<[f64; 2]> = Vec::new();
    let (mut sx, mut sy, mut count) = (0.0, 0.0, 0usize);
    for (i, m) in mask.iter().enumerate() {
        if !*m {
            continue;
        }
        let (x, y) = (i % w.max(1), i / w.max(1));
        sx += x as f64 + 0.5;
        sy += y as f64 + 0.5;
        count += 1;
        let edge = raster::neighbours4(i, w, h).iter().any(|j| j.is_none_or(|j| mask.get(j) != Some(&true)));
        let at_frame = x < FRAME_MARGIN || y < FRAME_MARGIN || x + FRAME_MARGIN >= w || y + FRAME_MARGIN >= h;
        if edge && !at_frame && hidden.get(i) != Some(&true) {
            points.push([x as f64 + 0.5, y as f64 + 0.5]);
        }
    }
    if count == 0 {
        return Err("the region is empty".into());
    }
    let centre = [sx / count as f64, sy / count as f64];
    let border = points.len();
    let least = ((LINE_SHARE * border as f64).ceil() as usize).max(8);
    let mut rng = Lcg(0x0ed9e);
    let mut edges: Vec<Edge> = Vec::new();
    let mut pool = points.clone();
    for _ in 0..12 {
        if edges.len() == 4 {
            break;
        }
        let Some(e) = fit_line(&pool, least, &mut rng) else { break };
        pool.retain(|p| (e.normal[0] * p[0] + e.normal[1] * p[1] - e.offset).abs() > 2.0 * LINE_BAND);
        // The same edge again (its points just past the band): skip it.
        let again = edges.iter().any(|f| {
            let cos = (e.normal[0] * f.normal[0] + e.normal[1] * f.normal[1]).abs();
            let gap = (f.normal[0] * e.centre[0] + f.normal[1] * e.centre[1] - f.offset).abs();
            cos > (10f64).to_radians().cos() && gap < 6.0 * LINE_BAND
        });
        if !again {
            // The border pixels lie inside the region, their centres half a pixel within its
            // edge: move the line out by that.
            let side = e.normal[0] * centre[0] + e.normal[1] * centre[1] - e.offset;
            let offset = if side < 0.0 { e.offset + 0.5 } else { e.offset - 0.5 };
            edges.push(Edge { offset, ..e });
        }
    }
    if edges.len() < 4 {
        return Err(format!(
            "found {} straight edges along the region's border, not 4 ({border} border points)",
            edges.len()
        ));
    }
    edges.sort_by(|a, b| {
        let t = |e: &Edge| (e.centre[1] - centre[1]).atan2(e.centre[0] - centre[0]);
        t(a).total_cmp(&t(b))
    });
    let mut corners = [[0.0; 2]; 4];
    for (k, c) in corners.iter_mut().enumerate() {
        let (Some(a), Some(b)) = (edges.get(k), edges.get((k + 1) % 4)) else {
            return Err("lost an edge".into());
        };
        let cos = (a.normal[0] * b.normal[0] + a.normal[1] * b.normal[1]).abs();
        if cos > MIN_CORNER_DEG.to_radians().cos() {
            return Err(
                "two neighbouring edges are nearly parallel: the region is not a quadrilateral".into()
            );
        }
        let det = a.normal[0] * b.normal[1] - a.normal[1] * b.normal[0];
        *c = [
            (a.offset * b.normal[1] - b.offset * a.normal[1]) / det,
            (a.normal[0] * b.offset - b.normal[0] * a.offset) / det,
        ];
    }
    // Corner k lies between edge k and edge k + 1. Start at the far left corner (smallest
    // x + y), go on towards the right.
    let start = (0..4)
        .min_by(|a, b| {
            let s = |k: usize| corners.get(k).map_or(f64::INFINITY, |c| c[0] + c[1]);
            s(*a).total_cmp(&s(*b))
        })
        .unwrap_or(0);
    let mut order: [usize; 4] = [start, (start + 1) % 4, (start + 2) % 4, (start + 3) % 4];
    let x = |k: usize| corners.get(k).map_or(0.0, |c| c[0]);
    if x(order[1]) < x(order[3]) {
        order = [start, (start + 3) % 4, (start + 2) % 4, (start + 1) % 4];
    }
    let sorted = order.map(|k| corners.get(k).copied().unwrap_or([0.0; 2]));
    // The edge two neighbouring corners share: corner k lies between edge k and edge k + 1.
    let between = |a: usize, b: usize| -> Edge {
        let shared = if (a + 1) % 4 == b { b } else { a };
        edges.get(shared).copied().unwrap_or(Edge {
            normal: [0.0; 2],
            offset: 0.0,
            points: 0,
            centre: [0.0; 2],
        })
    };
    let sorted_edges = [
        between(order[0], order[1]),
        between(order[1], order[2]),
        between(order[2], order[3]),
        between(order[3], order[0]),
    ];
    if Homography::square_to_quad(sorted).is_err() {
        return Err("the corners do not form a convex quadrilateral".into());
    }
    Ok(Outline { corners: sorted, edges: sorted_edges, border })
}

/// The straight line through the most of `points` (within [`LINE_BAND`]), refined by a total
/// least squares fit to them; `None` when it holds fewer than `least`.
fn fit_line(points: &[[f64; 2]], least: usize, rng: &mut Lcg) -> Option<Edge> {
    if points.len() < least.max(2) {
        return None;
    }
    let line = |a: [f64; 2], b: [f64; 2]| -> Option<([f64; 2], f64)> {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if len < 4.0 * LINE_BAND {
            return None;
        }
        let n = [-dy / len, dx / len];
        Some((n, n[0] * a[0] + n[1] * a[1]))
    };
    let inliers = |n: [f64; 2], c: f64| -> Vec<[f64; 2]> {
        points.iter().filter(|p| (n[0] * p[0] + n[1] * p[1] - c).abs() <= LINE_BAND).copied().collect()
    };
    let mut best: Option<(usize, [f64; 2], f64)> = None;
    for _ in 0..LINE_HYPOTHESES {
        let (Some(a), Some(b)) = (points.get(rng.below(points.len())), points.get(rng.below(points.len())))
        else {
            continue;
        };
        let Some((n, c)) = line(*a, *b) else { continue };
        let k = inliers(n, c).len();
        if best.is_none_or(|(s, _, _)| k > s) {
            best = Some((k, n, c));
        }
    }
    let (_, mut n, mut c) = best?;
    let mut on = inliers(n, c);
    for _ in 0..3 {
        if on.len() < 2 {
            return None;
        }
        let m = on.len() as f64;
        let mx = on.iter().map(|p| p[0]).sum::<f64>() / m;
        let my = on.iter().map(|p| p[1]).sum::<f64>() / m;
        let d = main_direction(&on);
        n = [-d[1], d[0]];
        c = n[0] * mx + n[1] * my;
        on = inliers(n, c);
    }
    if on.len() < least {
        return None;
    }
    let m = on.len() as f64;
    let centre = [on.iter().map(|p| p[0]).sum::<f64>() / m, on.iter().map(|p| p[1]).sum::<f64>() / m];
    Some(Edge { normal: n, offset: c, points: on.len(), centre })
}

/// Camera pixels around an object that count towards its support, and around it that do not
/// count as the support's edge.
const RING: u32 = 6;

/// Per plane (label `k` at index `k - 1`, `count` of them) how many columns of an object's
/// lower edge it is the first plane below, within a few pixels: the plane the objects stand on
/// scores highest (from a camera above, the support shows right under an object's lower edge,
/// the wall behind it beside and above it). `planes` and `objects` are camera labels.
pub fn support_scores(w: usize, h: usize, planes: &[u16], objects: &[u16], count: usize) -> Vec<usize> {
    let mut scores = vec![0usize; count];
    if planes.len() != w * h || objects.len() != w * h {
        return scores;
    }
    let highest = objects.iter().copied().max().unwrap_or(0);
    for k in 1..=highest {
        for x in 0..w {
            let Some(bottom) = (0..h).rev().find(|y| objects.get(y * w + x) == Some(&k)) else { continue };
            let below = (bottom + 1..(bottom + 1 + 2 * RING as usize).min(h))
                .filter_map(|y| planes.get(y * w + x).copied())
                .find(|p| *p > 0);
            if let Some(p) = below
                && let Some(s) = scores.get_mut(usize::from(p) - 1)
            {
                *s += 1;
            }
        }
    }
    scores
}

/// [`outline`] of face `support` among the camera labels `planes`, its border along the
/// objects (camera labels `objects`) left out.
pub fn face_outline(
    w: usize,
    h: usize,
    planes: &[u16],
    support: u16,
    objects: &[u16],
) -> Result<Outline, String> {
    let mask: Vec<bool> = planes.iter().map(|l| *l == support).collect();
    let any: Vec<bool> = objects.iter().map(|l| *l > 0).collect();
    let hidden = raster::dilate(w, h, &any, RING);
    outline(w, h, &mask, &hidden)
}

/// How curved a face is: its pairs' errors under its homography `h`, fitted per axis by a
/// quadratic in the projector position; the share of the errors' variance the quadratic
/// explains, and its RMS (camera pixels). On a plane the errors are the decode's noise and the
/// quadratic explains little; on a patch of a curved surface flat within the tolerance they
/// are a bowl, and it explains most.
fn bend(pairs: &Pairs, h: &Homography, inliers: &[usize]) -> [f64; 2] {
    use nalgebra::{SMatrix, SVector};
    let pts: Vec<([f64; 2], [f64; 2])> =
        inliers.iter().filter_map(|k| Some((*pairs.from.get(*k)?, pairs.residual(h, *k)?))).collect();
    if pts.len() < 12 {
        return [0.0, 0.0];
    }
    let m = pts.len() as f64;
    let (mx, my) = (pts.iter().map(|p| p.0[0]).sum::<f64>() / m, pts.iter().map(|p| p.0[1]).sum::<f64>() / m);
    let s = pts.iter().map(|p| (p.0[0] - mx).hypot(p.0[1] - my)).sum::<f64>() / m;
    let s = if s > 1e-9 { s } else { 1.0 };
    let basis = |p: [f64; 2]| {
        let (x, y) = ((p[0] - mx) / s, (p[1] - my) / s);
        SVector::<f64, 6>::from([1.0, x, y, x * x, x * y, y * y])
    };
    let mut a = SMatrix::<f64, 6, 6>::zeros();
    let mut b = [SVector::<f64, 6>::zeros(), SVector::<f64, 6>::zeros()];
    for (p, r) in &pts {
        let v = basis(*p);
        a += v * v.transpose();
        b[0] += v * r[0];
        b[1] += v * r[1];
    }
    let Some(inv) = a.try_inverse() else { return [0.0, 0.0] };
    let coef = [inv * b[0], inv * b[1]];
    let (mut total, mut left) = (0.0, 0.0);
    let mean = [pts.iter().map(|p| p.1[0]).sum::<f64>() / m, pts.iter().map(|p| p.1[1]).sum::<f64>() / m];
    for (p, r) in &pts {
        let v = basis(*p);
        for c in 0..2 {
            let (rc, mc, fit) = match c {
                0 => (r[0], mean[0], coef[0].dot(&v)),
                _ => (r[1], mean[1], coef[1].dot(&v)),
            };
            total += (rc - mc).powi(2);
            left += (rc - fit).powi(2);
        }
    }
    if total <= 0.0 {
        return [0.0, 0.0];
    }
    let share = (1.0 - left / total).clamp(0.0, 1.0);
    [share, ((total - left).max(0.0) / m).sqrt()]
}

/// Gaps a decode leaves in a picture of labels, up to about `2 r` wide, filled: each label
/// closed (nothing grows past its outline), unlabelled pixels taking the closed label.
pub fn fill_gaps(w: usize, h: usize, labels: &[u16], r: u32) -> Vec<u16> {
    let mut out = labels.to_vec();
    let highest = labels.iter().copied().max().unwrap_or(0);
    for k in 1..=highest {
        let Some(b) = raster::bounds(w, labels, |l| *l == k) else { continue };
        let rect = b.grown(r as usize + 1, w, h);
        let (rw, rh) = (rect.width(), rect.height());
        let mask: Vec<bool> = rect.crop(w, labels).iter().map(|l| *l == k).collect();
        let closed = raster::close(rw, rh, &mask, r);
        for (j, c) in closed.iter().enumerate() {
            let i = (rect.y0 + j / rw.max(1)) * w + rect.x0 + j % rw.max(1);
            if *c
                && labels.get(i) == Some(&0)
                && let Some(o) = out.get_mut(i)
                && *o == 0
            {
                *o = k;
            }
        }
    }
    out
}
