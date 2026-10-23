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
//! When the strict search ends, a rough one goes on with [`ROUGH`] times the tolerance, for
//! faces whose decode is noisier (tops seen at a grazing angle, surfaces lit by light the walls
//! next to them throw back).
//!
//! A curved object holds patches that are flat within the tolerance. Such a patch is dropped
//! when a smooth bowl explains much of its error ([`bend`]), or when the surface round it goes
//! on bending the way that bowl does ([`continues`]): a flat face ends at a crease, a step or a
//! rim instead. Last, where two faces meet, each pixel goes to the face nearby that fits it best
//! (the face found first took the crease).
//!
//! **Objects:** the measured pixels on no face, without the surface detail next to a face (rims,
//! ribs, noise: within a few tolerances of its plane), closed, opened, holes filled (but not
//! with faces), small pieces dropped, numbered by size. With several projectors, an object of
//! one that lies on another's faces in the camera picture is dropped
//! ([`drop_objects_on_faces`]).
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
/// The second, rough pass: its tolerance as a multiple of `tolerance`.
const ROUGH: f64 = 2.5;
/// A face is a patch of a curved surface, no plane, when a bowl explains more than this share
/// of its error ([`bend`]) and the bowl's RMS is above this, camera pixels. Measured: planes of a
/// real scan 0.003 to 0.28 (the most on a wall filling the frame, the lens's distortion), patches
/// of a ray-cast ball 0.77 to 0.94.
const BEND_SHARE: f64 = 0.5;
const BEND_PX: f64 = 0.3;
/// Or when the bowl's RMS is above [`BEND_PX`] and it bows more than this over [`BOW_SPAN`]
/// projector pixels, camera pixels. Measured with a real decode's noise: faces of a real scan
/// up to 3.5 (the small ones, whose bow is noisy), its wall 0.03, patches of a flower pot 6 to
/// 16, of a ray-cast pumpkin 7 to 13.
const BEND_BOW: f64 = 5.0;
const BOW_SPAN: f64 = 100.0;
/// Or when the bowl's RMS alone is above this share of the pass's tolerance. Measured on a real
/// scan of crates: faces up to 0.2 of it (0.39 px at 2 px), patches of a pumpkin 0.37 (0.74 px)
/// and in the rough pass 0.4 (2 px at 5 px).
const BEND_TOLERANCE: f64 = 0.3;
/// The ring past a face's edge (projector pixels) [`continues`] looks at, and when the surface
/// is taken to bend on past the edge: this many measured pixels in the ring, this share of them
/// off the face's plane, and this share of those where its bowl predicts them.
const RING_IN: u32 = 2;
const RING_OUT: u32 = 12;
const CONTINUE_MIN: usize = 50;
const CONTINUE_OFF: f64 = 0.3;
/// Measured: faces of real scans 0 to 0.3 (the most on a wall filling the frame), patches of a
/// flower pot 0.42 and 0.52.
const CONTINUE_FOLLOWS: f64 = 0.36;
/// How far (projector pixels) a pixel looks for a better fitting face at a crease.
const SETTLE: u32 = 3;
/// Measured pixels on no face lie on an object unless the nearest face, within the projector's
/// longer side over this, fits them within this many tolerances: then they are its rims, ribs
/// or noise, surface detail.
const DETAIL_SPAN: usize = 32;
const DETAIL: f64 = 6.0;
/// How far (projector pixels) an object grows back into the detail next to it.
const REGROW: u32 = 6;
/// [`drop_objects_on_faces`]: an object of one projector is a face when more than this share of
/// its pixels another projector measures lie on that one's faces (at least this many), the
/// camera pictures' masks grown by this (camera pixels, the decode leaves gaps).
const FACE_WINS: f64 = 0.5;
const FACE_WINS_MIN: usize = 200;
const CAMERA_REACH: u32 = 2;
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
    /// How much of its pixels' error a smooth bowl explains: the share, its RMS and its bow
    /// over 100 projector pixels, camera pixels ([`bend`]).
    pub bend: [f64; 3],
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
    /// Faces found and dropped again as curved.
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
    // A strict pass, then a rough one for faces whose decode is noisier (tops at a grazing
    // angle, surfaces catching light thrown back by the walls next to them).
    for pass_tol in [tol, tol * ROUGH] {
        while found.len() < o.max_planes {
            let faces = find_plane(w, h, &pairs, &near, &sample, &free, pass_tol, least, &mut rng);
            if faces.is_empty() {
                break;
            }
            for f in faces {
                let bent = f.bend[1] > BEND_PX && (f.bend[0] > BEND_SHARE || f.bend[2] > BEND_BOW)
                    || f.bend[1] > BEND_TOLERANCE * pass_tol;
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
    }

    // A patch of a curved object, flat within the tolerance, goes on bending past its edge the
    // way its bowl does; a flat face ends in a crease, a step or a rim.
    let labels = plane_labels(n, &found);
    let keep: Vec<bool> =
        found.iter().map(|(f, _)| !continues(w, h, &pairs, &near, &labels, f, tol)).collect();
    let mut k = 0usize;
    found.retain(|(f, _)| {
        let kept = keep.get(k).copied().unwrap_or(true);
        k += 1;
        if !kept {
            dropped_planes += 1;
            for i in &f.inliers {
                if let Some(c) = curved.get_mut(*i) {
                    *c = true;
                }
            }
        }
        kept
    });
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
    // Surface detail is no object: a rim, a rib or a noisy patch next to a face stands off its
    // plane by a few pixels, an object standing in front of it by many.
    let reach = (w.max(h) / DETAIL_SPAN).max(1) as f32;
    let on_face: Vec<bool> = labels.iter().map(|l| *l > 0).collect();
    let (face_dist, face_near) = raster::distance(w, h, &on_face);
    let core: Vec<bool> = left
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if !*l {
                return false;
            }
            let (Some(d), Some(Some(j)), Some(&k)) = (face_dist.get(i), face_near.get(i), near.get(i)) else {
                return true;
            };
            let face = labels.get(*j as usize).copied().unwrap_or(0);
            if *d > reach || face == 0 {
                return true;
            }
            found
                .get(usize::from(face) - 1)
                .is_none_or(|(f, _)| pairs.error(&f.homography, k as usize) >= DETAIL * tol)
        })
        .collect();
    let mask = raster::close(w, h, &core, OBJECT_CLOSE);
    let mask = raster::open(w, h, &mask, OBJECT_OPEN);
    let mask = keep_large(w, h, &mask, o.min_object);
    // The objects take back the detail right next to them (an object's foot is near the plane
    // it stands on, and would else be cut off), each its own, never joining another.
    let mask = regrow(w, h, &mask, &left);
    // Holes filled, but not with faces: a face the leftover runs round (a crate's front
    // between rims and noisy tops) stays a face.
    let filled = raster::fill_holes(w, h, &mask);
    let mask: Vec<bool> =
        filled.iter().zip(&mask).zip(&labels).map(|((f, m), l)| *m || (*f && *l == 0)).collect();
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

/// `mask`'s parts grown by [`REGROW`] pixels into `into`, one pixel a round; a pixel two parts
/// would reach (or two neighbours taken by different parts in one round) stays out, so parts
/// that were apart stay apart.
fn regrow(w: usize, h: usize, mask: &[bool], into: &[bool]) -> Vec<bool> {
    let (mut part, _) = raster::components(w, h, mask);
    for _ in 0..REGROW {
        let mut next = part.clone();
        for (i, n) in next.iter_mut().enumerate() {
            if *n != 0 || into.get(i) != Some(&true) {
                continue;
            }
            let mut by = 0u32;
            for j in raster::neighbours4(i, w, h).into_iter().flatten() {
                match part.get(j).copied().unwrap_or(0) {
                    0 => {}
                    m if by == 0 || by == m => by = m,
                    _ => {
                        by = u32::MAX;
                        break;
                    }
                }
            }
            if by != u32::MAX {
                *n = by;
            }
        }
        let mut grew = false;
        for i in 0..next.len() {
            let (Some(&now), Some(&before)) = (next.get(i), part.get(i)) else { continue };
            if now == 0 || before != 0 {
                continue;
            }
            let clash = raster::neighbours4(i, w, h)
                .into_iter()
                .flatten()
                .any(|j| next.get(j).is_some_and(|m| *m != 0 && *m != now));
            if !clash && let Some(p) = part.get_mut(i) {
                *p = now;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    part.iter().map(|p| *p != 0).collect()
}

/// Where two projectors disagree, a face wins: an object of one projector whose pixels land
/// mostly (over [`FACE_WINS`] of those the other projector measures) on the other's faces in
/// the camera picture is a flat surface the first one decoded badly (a top catching light the
/// walls throw back), no object. Such objects are dropped (their pixels left unexplained) and
/// the rest renumbered by size. `maps` are the projectors' `proj_to_cam` maps in the order of
/// `scenes`; returns how many objects each projector dropped.
pub fn drop_objects_on_faces(scenes: &mut [SceneParts], maps: &[&Map], camera: (usize, usize)) -> Vec<usize> {
    let (cw, ch) = camera;
    let mut dropped = vec![0usize; scenes.len()];
    if scenes.len() < 2 || maps.len() != scenes.len() || cw == 0 || ch == 0 {
        return dropped;
    }
    // Per projector the camera pixels it measures, and those on its faces.
    let views: Vec<(Vec<bool>, Vec<bool>)> = scenes
        .iter()
        .zip(maps)
        .map(|(s, map)| {
            let (mut seen, mut face) = (vec![false; cw * ch], vec![false; cw * ch]);
            for i in 0..map.len() {
                if !map.measured(i) {
                    continue;
                }
                let Some(c) = map.pixel_in(i, cw, ch) else { continue };
                if let Some(x) = seen.get_mut(c) {
                    *x = true;
                }
                if s.plane_labels.get(i).is_some_and(|l| *l > 0)
                    && let Some(x) = face.get_mut(c)
                {
                    *x = true;
                }
            }
            (raster::dilate(cw, ch, &seen, CAMERA_REACH), raster::dilate(cw, ch, &face, CAMERA_REACH))
        })
        .collect();
    for (p, (s, map)) in scenes.iter_mut().zip(maps).enumerate() {
        let count = usize::from(s.object_labels.iter().copied().max().unwrap_or(0));
        if count == 0 {
            continue;
        }
        // Per object and other projector: pixels that one measures, and of those on its faces.
        let mut tally = vec![[0usize; 2]; count + 1];
        for (i, l) in s.object_labels.iter().enumerate() {
            if *l == 0 || !map.measured(i) {
                continue;
            }
            let Some(c) = map.pixel_in(i, cw, ch) else { continue };
            for (q, (seen, face)) in views.iter().enumerate() {
                if q == p || seen.get(c) != Some(&true) {
                    continue;
                }
                if let Some(t) = tally.get_mut(usize::from(*l)) {
                    t[0] += 1;
                    if face.get(c) == Some(&true) {
                        t[1] += 1;
                    }
                }
            }
        }
        let wins: Vec<bool> =
            tally.iter().map(|[n, f]| *n >= FACE_WINS_MIN && *f as f64 > FACE_WINS * *n as f64).collect();
        if !wins.iter().any(|w| *w) {
            continue;
        }
        let mut sizes = vec![0usize; count + 1];
        for l in &s.object_labels {
            if let Some(z) = sizes.get_mut(usize::from(*l)) {
                *z += 1;
            }
        }
        let mut kept: Vec<(usize, usize)> = (1..=count)
            .filter(|k| wins.get(*k) == Some(&false))
            .map(|k| (sizes.get(k).copied().unwrap_or(0), k))
            .collect();
        kept.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut number = vec![0u16; count + 1];
        for (n, (_, k)) in kept.iter().enumerate() {
            if let Some(m) = number.get_mut(*k) {
                *m = u16::try_from(n + 1).unwrap_or(0);
            }
        }
        let mut gone = 0usize;
        for (i, l) in s.object_labels.iter_mut().enumerate() {
            if *l == 0 {
                continue;
            }
            *l = number.get(usize::from(*l)).copied().unwrap_or(0);
            if *l == 0 && map.measured(i) && s.plane_labels.get(i) == Some(&0) {
                gone += 1;
            }
        }
        s.unexplained += gone;
        s.objects = kept.len();
        if let Some(d) = dropped.get_mut(p) {
            *d = count - kept.len();
        }
    }
    dropped
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

/// Whether a face's surface bends on past its edge, as on a curved object, rather than ending
/// in a step or a crease, as a flat face does: among the measured pixels in a ring round it on
/// no other face, enough lie off its plane (beyond `tol`) and most of those where its bowl
/// ([`bend`]), carried on past the edge, puts them.
fn continues(w: usize, h: usize, pairs: &Pairs, near: &[u32], labels: &[u16], face: &Face, tol: f64) -> bool {
    let Some(bowl) = face.bowl else { return false };
    if face.pixels.is_empty() || w == 0 {
        return false;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for i in &face.pixels {
        let (x, y) = (*i as usize % w, *i as usize / w);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
    }
    let rect = raster::Rect { x0, y0, x1, y1 }.grown(RING_OUT as usize + 1, w, h);
    let (rw, rh) = (rect.width(), rect.height());
    let mut local = vec![false; rw * rh];
    for i in &face.pixels {
        let (x, y) = (*i as usize % w, *i as usize / w);
        if let Some(r) = local.get_mut((y - rect.y0) * rw + x - rect.x0) {
            *r = true;
        }
    }
    let inner = raster::dilate(rw, rh, &local, RING_IN);
    let outer = raster::dilate(rw, rh, &local, RING_OUT);
    let (mut ring, mut off, mut follows) = (0usize, 0usize, 0usize);
    for (j, (o, n)) in outer.iter().zip(&inner).enumerate() {
        if !*o || *n {
            continue;
        }
        let i = (rect.y0 + j / rw.max(1)) * w + rect.x0 + j % rw.max(1);
        if labels.get(i).is_some_and(|l| *l != 0) {
            continue;
        }
        let Some(&k) = near.get(i) else { continue };
        let k = k as usize;
        if pairs.idx.get(k) != Some(&i) {
            continue;
        }
        let (Some(r), Some(p)) = (pairs.residual(&face.homography, k), pairs.from.get(k)) else { continue };
        ring += 1;
        if r[0].hypot(r[1]) > tol {
            off += 1;
            let q = bowl.at(*p);
            if (r[0] - q[0]).hypot(r[1] - q[1]) < tol {
                follows += 1;
            }
        }
    }
    ring >= CONTINUE_MIN
        && off as f64 >= CONTINUE_OFF * ring as f64
        && follows as f64 >= CONTINUE_FOLLOWS * off as f64
}

/// A connected face found by [`find_plane`]: its homography, pixels (its region, unmeasured
/// gaps included) and the pairs on it.
struct Face {
    homography: Homography,
    pixels: Vec<u32>,
    inliers: Vec<usize>,
    /// See [`bend`].
    bend: [f64; 3],
    bowl: Option<Bowl>,
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
            let (bend, bowl) = bend(pairs, &own, &inliers);
            faces.push(Face { homography: own, pixels, inliers, bend, bowl });
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

/// How far below an object's lower edge (a share of the camera picture's height) the support is
/// looked for: under a pumpkin on a crate the camera sees a band nothing measures (the top in the
/// pumpkin's shadow, its underside curving away), about 40 pixels in full HD.
const SUPPORT_SPAN: usize = 12;

/// Per plane (label `k` at index `k - 1`, `count` of them) how many columns of an object's
/// lower edge it is the first plane below, within a twelfth of the picture's height: the plane
/// the objects stand on scores highest (from a camera above, the support shows right under an object's lower edge,
/// the wall behind it beside and above it). `planes` and `objects` are camera labels.
pub fn support_scores(w: usize, h: usize, planes: &[u16], objects: &[u16], count: usize) -> Vec<usize> {
    let mut scores = vec![0usize; count];
    if planes.len() != w * h || objects.len() != w * h {
        return scores;
    }
    let highest = objects.iter().copied().max().unwrap_or(0);
    let reach = (h / SUPPORT_SPAN).max(2 * RING as usize);
    for k in 1..=highest {
        for x in 0..w {
            let Some(bottom) = (0..h).rev().find(|y| objects.get(y * w + x) == Some(&k)) else { continue };
            let below = (bottom + 1..(bottom + 1 + reach).min(h))
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

/// A face's bowl: the quadratic [`bend`] fits to its errors, to extrapolate past its edge.
#[derive(Clone, Copy, Debug)]
struct Bowl {
    centre: [f64; 2],
    scale: f64,
    coef: [nalgebra::SVector<f64, 6>; 2],
}

impl Bowl {
    fn at(&self, p: [f64; 2]) -> [f64; 2] {
        let (x, y) = ((p[0] - self.centre[0]) / self.scale, (p[1] - self.centre[1]) / self.scale);
        let v = nalgebra::SVector::<f64, 6>::from([1.0, x, y, x * x, x * y, y * y]);
        [self.coef[0].dot(&v), self.coef[1].dot(&v)]
    }
}

/// How curved a face is: its pairs' errors under its homography `h`, fitted per axis by a
/// quadratic in the projector position; the share of the errors' variance the quadratic
/// explains, its RMS (camera pixels), and its bow: how far it bends over [`BOW_SPAN`]
/// projector pixels (camera pixels, the larger axis). On a plane the errors are the decode's
/// noise and the quadratic explains little; on a patch of a curved surface flat within the
/// tolerance they are a bowl, and it explains most when the noise is low. With a real decode's
/// noise the share stays low on small patches, but their bow stands out: a lens's distortion
/// bows a wall over its whole size, a pumpkin over a hand's breadth.
/// Also the bowl itself, `None` when too few pairs or a degenerate fit.
fn bend(pairs: &Pairs, h: &Homography, inliers: &[usize]) -> ([f64; 3], Option<Bowl>) {
    use nalgebra::{SMatrix, SVector};
    let pts: Vec<([f64; 2], [f64; 2])> =
        inliers.iter().filter_map(|k| Some((*pairs.from.get(*k)?, pairs.residual(h, *k)?))).collect();
    if pts.len() < 12 {
        return ([0.0; 3], None);
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
    let Some(inv) = a.try_inverse() else { return ([0.0; 3], None) };
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
    let fitted = Bowl { centre: [mx, my], scale: s, coef };
    if total <= 0.0 {
        return ([0.0; 3], Some(fitted));
    }
    let share = (1.0 - left / total).clamp(0.0, 1.0);
    // The bowl's second-order terms back in projector pixels: its bow over BOW_SPAN.
    let bow =
        coef.iter().map(|k| (k[3].powi(2) + 0.5 * k[4].powi(2) + k[5].powi(2)).sqrt()).fold(0.0f64, f64::max)
            * (BOW_SPAN / s).powi(2);
    ([share, ((total - left).max(0.0) / m).sqrt(), bow], Some(fitted))
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

/// A rectified face's measured pixels this far from its homography (in tolerances) are fitted by
/// [`face_map`]: a face that sags or warps a few pixels, not the object standing on it.
const WARP_REACH: f64 = 6.0;
/// Fewest pixels [`face_map`] fits a correction to.
const WARP_MIN: usize = 300;
/// IRLS rounds of the correction.
const WARP_ROUNDS: usize = 6;
/// The correction is kept when it lowers the median error by at least this share.
const WARP_GAIN: f64 = 0.1;
/// The grid over the face (per side) on which the correction is checked for running away.
const WARP_GRID: usize = 9;

/// How a rectified face's projector pixels map to the camera: its homography, and where the
/// face is not quite flat (a lid sagging under the object on it, a warped board) a smooth
/// quadratic correction on top, fitted to every measured pixel inside the face's outline that
/// lies near the plane. Without it a plane fit leaves such a face a few camera pixels off in
/// places, and at a grazing angle a pixel spans centimetres of the face, so content drawn per
/// projector lands off and two projectors disagree.
#[derive(Clone, Copy, Debug)]
pub struct FaceMap {
    /// Projector pixels to camera pixels.
    pub homography: Homography,
    bowl: Option<Bowl>,
    /// Pixels fitted, their median error under the homography and under the corrected map, the
    /// correction's RMS over them and its largest size over the face, camera pixels.
    pub pixels: usize,
    pub error_px: [f64; 2],
    pub bend_px: [f64; 2],
}

impl FaceMap {
    /// The camera position of projector point `p`.
    pub fn apply(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let q = self.homography.apply(p)?;
        Some(match &self.bowl {
            Some(b) => {
                let d = b.at(p);
                [q[0] + d[0], q[1] + d[1]]
            }
            None => q,
        })
    }

    /// Whether a correction is applied.
    pub fn corrected(&self) -> bool {
        self.bowl.is_some()
    }
}

/// The map of a rectified face in one projector: `h` its plane's homography (projector to
/// camera), `to_face` the camera picture to the face's (u, v), `tolerance` the scene's (camera
/// pixels). Fits a quadratic correction, by reweighted least squares, to the measured pixels of
/// `map` that land inside the face (with a margin of `margin` in u and v) within [`WARP_REACH`]
/// tolerances of `h`, whatever they are labelled: parts of a sagging lid fail the plane's
/// tolerance and end up as objects or unexplained. Keeps it only when it fits clearly better
/// and stays within that reach over the whole face (it also extrapolates under objects).
pub fn face_map(
    map: &Map,
    camera: (usize, usize),
    h: &Homography,
    to_face: &Homography,
    tolerance: f64,
    margin: f64,
) -> FaceMap {
    let plain = FaceMap { homography: *h, bowl: None, pixels: 0, error_px: [0.0; 2], bend_px: [0.0; 2] };
    let reach = WARP_REACH * tolerance;
    let pairs = Pairs::of(map, camera);
    let on = |u: f64, v: f64| (-margin..=1.0 + margin).contains(&u) && (-margin..=1.0 + margin).contains(&v);
    let pts: Vec<([f64; 2], [f64; 2])> = (0..pairs.idx.len())
        .filter_map(|k| {
            let (p, q) = (*pairs.from.get(k)?, *pairs.to.get(k)?);
            let [u, v] = to_face.apply(q)?;
            let r = pairs.residual(h, k)?;
            (on(u, v) && r[0].hypot(r[1]) <= reach).then_some((p, r))
        })
        .collect();
    let m = pts.len();
    if m < WARP_MIN {
        return FaceMap { pixels: m, ..plain };
    }
    let mf = m as f64;
    let (mx, my) =
        (pts.iter().map(|p| p.0[0]).sum::<f64>() / mf, pts.iter().map(|p| p.0[1]).sum::<f64>() / mf);
    let s = pts.iter().map(|p| (p.0[0] - mx).hypot(p.0[1] - my)).sum::<f64>() / mf;
    let s = if s > 1e-9 { s } else { 1.0 };
    let norm = |r: [f64; 2]| r[0].hypot(r[1]);
    let median = |mut v: Vec<f64>| -> f64 {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let before = median(pts.iter().map(|p| norm(p.1)).collect());
    let mut fit = FaceMap { pixels: m, error_px: [before, before], ..plain };
    // Quadratic first; when that runs away over the parts of the face nothing measured, the
    // linear terms alone (a better plane over all of the face's pixels).
    for terms in [6, 3] {
        let Some(bowl) = fit_bowl(&pts, terms, [mx, my], s) else { continue };
        let corrected = |(p, r): &([f64; 2], [f64; 2])| {
            let d = bowl.at(*p);
            (r[0] - d[0]).hypot(r[1] - d[1])
        };
        let after = median(pts.iter().map(corrected).collect());
        let rms = (pts.iter().map(|(p, _)| norm(bowl.at(*p)).powi(2)).sum::<f64>() / mf).sqrt();
        // Over the whole face: through the camera picture's quad back to the projector.
        let largest = to_face.inverse().zip(h.inverse()).map_or(f64::INFINITY, |(from_face, back)| {
            let mut most = 0.0f64;
            for i in 0..WARP_GRID {
                for j in 0..WARP_GRID {
                    let uv = [i as f64 / (WARP_GRID - 1) as f64, j as f64 / (WARP_GRID - 1) as f64];
                    let Some(p) = from_face.apply(uv).and_then(|q| back.apply(q)) else {
                        return f64::INFINITY;
                    };
                    most = most.max(norm(bowl.at(p)));
                }
            }
            most
        });
        fit = FaceMap { pixels: m, error_px: [before, after], bend_px: [rms, largest], ..plain };
        if after <= before * (1.0 - WARP_GAIN) && largest.is_finite() && largest <= reach {
            return FaceMap { bowl: Some(bowl), ..fit };
        }
    }
    fit
}

/// The first `terms` (3 or 6) of a quadratic in the projector position fitted to the residuals
/// `pts` (projector position, camera pixels off), by least squares reweighted with Cauchy
/// weights: rims, ribs and the foot of an object near the plane fade.
fn fit_bowl(pts: &[([f64; 2], [f64; 2])], terms: usize, centre: [f64; 2], scale: f64) -> Option<Bowl> {
    use nalgebra::{SMatrix, SVector};
    let basis = |p: [f64; 2]| {
        let (x, y) = ((p[0] - centre[0]) / scale, (p[1] - centre[1]) / scale);
        let v = [1.0, x, y, x * x, x * y, y * y];
        SVector::<f64, 6>::from_fn(|j, _| if j < terms { v.get(j).copied().unwrap_or(0.0) } else { 0.0 })
    };
    let mut weights = vec![1.0f64; pts.len()];
    let mut bowl = None;
    for _ in 0..WARP_ROUNDS {
        let mut a = SMatrix::<f64, 6, 6>::zeros();
        let mut b = [SVector::<f64, 6>::zeros(), SVector::<f64, 6>::zeros()];
        for ((p, r), w) in pts.iter().zip(&weights) {
            let v = basis(*p);
            a += v * v.transpose() * *w;
            b[0] += v * (r[0] * w);
            b[1] += v * (r[1] * w);
        }
        for j in terms..6 {
            if let Some(d) = a.get_mut((j, j)) {
                *d = 1.0;
            }
        }
        let inv = a.try_inverse()?;
        let fitted = Bowl { centre, scale, coef: [inv * b[0], inv * b[1]] };
        let mut errors: Vec<f64> = pts
            .iter()
            .map(|(p, r)| {
                let d = fitted.at(*p);
                (r[0] - d[0]).hypot(r[1] - d[1])
            })
            .collect();
        let mut sorted = errors.clone();
        sorted.sort_by(f64::total_cmp);
        let c = (sorted.get(sorted.len() / 2).copied().unwrap_or(1.0) * 2.0).max(0.1);
        for e in &mut errors {
            *e = 1.0 / (1.0 + (*e / c).powi(2));
        }
        weights = errors;
        bowl = Some(fitted);
    }
    bowl
}

/// Hypotheses of [`claim_face`]'s search, and the most pairs each is scored on.
const CLAIM_HYPOTHESES: usize = 400;
const CLAIM_SAMPLE: usize = 4000;
/// A prior's camera pixels are grown by this (camera pixels) before [`claim_face`] searches
/// among them.
const PRIOR_GROW: u32 = 4;
/// Cells per side of the grid over the face [`claim_face`] scores coverage on.
const CLAIM_CELLS: usize = 12;
/// Rounds in which a claimed face grows with its bend.
const CLAIM_GROW: usize = 4;

/// A face given by its corners in the camera picture (`to_face`: camera picture to the face's
/// u and v, the face 0 to 1), claimed in one projector's `parts`: the plane that most of the
/// measured pixels inside the corners lie on, with [`face_map`]'s correction where the face is
/// not quite flat, and every pixel inside that fits it within [`ROUGH`] tolerances becomes this
/// face, taken from the faces and objects it was given to. For a face the search cannot find
/// alone: a lid seen at a grazing angle that sags under the object on it fits no plane within the
/// tolerance, falls apart into patches that each look curved, and ends up as part of that
/// object. The plane is searched among the pixels inside the corners, or when `prior` (a mask
/// of the camera picture: where another projector found the face) holds enough of them, among
/// those: a projector that decodes the face sparsely may see more of the object in front.
/// Returns the new face's index in `parts.planes` and the camera pixels its measured pixels land
/// on; `None` when too few pixels inside the corners fit one plane.
pub fn claim_face(
    parts: &mut SceneParts,
    map: &Map,
    camera: (usize, usize),
    to_face: &Homography,
    tolerance: f64,
    margin: f64,
    prior: Option<&[bool]>,
) -> Option<Claim> {
    let (w, h) = (map.width, map.height);
    if parts.width != w || parts.height != h || parts.planes.len() >= MAX_PLANES {
        return None;
    }
    let pairs = Pairs::of(map, camera);
    let on = |q: &[f64; 2]| {
        to_face.apply(*q).is_some_and(|[u, v]| {
            (-margin..=1.0 + margin).contains(&u) && (-margin..=1.0 + margin).contains(&v)
        })
    };
    let inside: Vec<usize> = (0..pairs.idx.len()).filter(|k| pairs.to.get(*k).is_some_and(on)).collect();
    if inside.len() < WARP_MIN {
        return None;
    }
    let cam_index = |q: &[f64; 2]| -> Option<usize> {
        let (x, y) = (q[0].floor(), q[1].floor());
        (x >= 0.0 && y >= 0.0 && (x as usize) < camera.0 && (y as usize) < camera.1)
            .then(|| y as usize * camera.0 + x as usize)
    };
    let grown = prior.map(|m| raster::dilate(camera.0, camera.1, m, PRIOR_GROW));
    let in_prior: Vec<usize> = match &grown {
        Some(mask) => inside
            .iter()
            .copied()
            .filter(|k| pairs.to.get(*k).and_then(cam_index).is_some_and(|c| mask.get(c) == Some(&true)))
            .collect(),
        None => Vec::new(),
    };
    let search = if in_prior.len() >= WARP_MIN { &in_prior } else { &inside };
    let rough = ROUGH * tolerance;
    let stride = search.len().div_ceil(CLAIM_SAMPLE).max(1);
    let scored: Vec<usize> = search.iter().copied().step_by(stride).collect();
    // The face covers the corners' whole quad: a hypothesis scores by the cells of the face it
    // reaches before its pixel count, so a dense patch of the object standing on a sparsely
    // decoded face does not win.
    let g = CLAIM_CELLS as f64;
    let cell_of: Vec<usize> = scored
        .iter()
        .map(|k| {
            pairs.to.get(*k).and_then(|q| to_face.apply(*q)).map_or(0, |[u, v]| {
                let (x, y) = ((u.clamp(0.0, 1.0) * g) as usize, (v.clamp(0.0, 1.0) * g) as usize);
                y.min(CLAIM_CELLS - 1) * CLAIM_CELLS + x.min(CLAIM_CELLS - 1)
            })
        })
        .collect();
    let cell = (w.max(h) / SAMPLE_CELLS).max(MIN_CELL) as f64;
    let mut rng = Lcg(0xc1a1);
    let mut best: Option<(usize, Homography)> = None;
    for _ in 0..CLAIM_HYPOTHESES {
        let pick: Vec<usize> = (0..4).filter_map(|_| search.get(rng.below(search.len())).copied()).collect();
        let points: Vec<[f64; 2]> = pick.iter().filter_map(|k| pairs.from.get(*k).copied()).collect();
        if points.len() < 4 || !spread(&points, cell * cell) {
            continue;
        }
        let quad: Vec<([f64; 2], [f64; 2])> =
            pick.iter().filter_map(|k| Some((*pairs.from.get(*k)?, *pairs.to.get(*k)?))).collect();
        let Ok(hyp) = Homography::from_pairs(&quad) else { continue };
        let mut hits = [0u8; CLAIM_CELLS * CLAIM_CELLS];
        let mut count = 0usize;
        for (k, c) in scored.iter().zip(&cell_of) {
            if pairs.error(&hyp, *k) < rough {
                count += 1;
                if let Some(h) = hits.get_mut(*c) {
                    *h = h.saturating_add(1);
                }
            }
        }
        let cells = hits.iter().filter(|h| **h >= 2).count();
        let score = cells * scored.len() + count;
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, hyp));
        }
    }
    let (_, mut hom) = best?;
    for _ in 0..REFITS {
        let (f, t): (Vec<[f64; 2]>, Vec<[f64; 2]>) = search
            .iter()
            .filter(|k| pairs.error(&hom, **k) < rough)
            .filter_map(|k| Some((*pairs.from.get(*k)?, *pairs.to.get(*k)?)))
            .unzip();
        if f.len() < WARP_MIN {
            return None;
        }
        hom = Homography::fit_cauchy(&f, &t, CAUCHY_ROUNDS).ok()?;
    }
    // Grown with the bend: a bowl fitted to the pixels the map so far takes, which takes more of
    // a sagging face each round, while it stays within reach of the plane over the whole face.
    let mut fm = FaceMap { homography: hom, bowl: None, pixels: 0, error_px: [0.0; 2], bend_px: [0.0; 2] };
    let error_of = |fm: &FaceMap, k: usize| -> f64 {
        match (pairs.from.get(k).and_then(|p| fm.apply(*p)), pairs.to.get(k)) {
            (Some(r), Some(q)) => (r[0] - q[0]).hypot(r[1] - q[1]),
            _ => f64::INFINITY,
        }
    };
    let back = to_face.inverse().zip(hom.inverse());
    for _ in 0..CLAIM_GROW {
        let pts: Vec<([f64; 2], [f64; 2])> = inside
            .iter()
            .filter(|k| error_of(&fm, **k) < rough)
            .filter_map(|k| Some((*pairs.from.get(*k)?, pairs.residual(&hom, *k)?)))
            .collect();
        if pts.len() < WARP_MIN {
            break;
        }
        let m = pts.len() as f64;
        let centre =
            [pts.iter().map(|p| p.0[0]).sum::<f64>() / m, pts.iter().map(|p| p.0[1]).sum::<f64>() / m];
        let scale = pts.iter().map(|p| (p.0[0] - centre[0]).hypot(p.0[1] - centre[1])).sum::<f64>() / m;
        let Some(bowl) = fit_bowl(&pts, 6, centre, if scale > 1e-9 { scale } else { 1.0 }) else { break };
        let tame = back.is_some_and(|(from_face, to_proj)| {
            (0..WARP_GRID * WARP_GRID).all(|g| {
                let uv = [
                    (g % WARP_GRID) as f64 / (WARP_GRID - 1) as f64,
                    (g / WARP_GRID) as f64 / (WARP_GRID - 1) as f64,
                ];
                from_face.apply(uv).and_then(|q| to_proj.apply(q)).is_some_and(|p| {
                    let d = bowl.at(p);
                    d[0].hypot(d[1]) <= WARP_REACH * tolerance
                })
            })
        });
        if !tame {
            break;
        }
        fm.bowl = Some(bowl);
    }
    let error = |k: usize| error_of(&fm, k);
    let mut member = vec![false; pairs.idx.len()];
    for k in &inside {
        if error(*k) < rough
            && let Some(m) = member.get_mut(*k)
        {
            *m = true;
        }
    }
    // Pixels: measured members, and the unmeasured ones whose nearest measured pixel is one.
    let n = w * h;
    let measured: Vec<bool> = (0..n).map(|i| map.measured(i)).collect();
    let (dist, nearest) = raster::distance(w, h, &measured);
    let mut point_at = vec![u32::MAX; n];
    for (k, i) in pairs.idx.iter().enumerate() {
        if let Some(p) = point_at.get_mut(*i) {
            *p = k as u32;
        }
    }
    let mask: Vec<bool> = (0..n)
        .map(|i| match (nearest.get(i).copied().flatten(), dist.get(i)) {
            (Some(j), Some(d)) if *d <= GAP && map.uv(i).is_some() => {
                point_at.get(j as usize).is_some_and(|k| member.get(*k as usize) == Some(&true))
            }
            _ => false,
        })
        .collect();
    let mask = raster::close(w, h, &mask, REGION_CLOSE);
    let mask = raster::open(w, h, &mask, REGION_OPEN);
    let mask = keep_large(w, h, &mask, WARP_MIN);
    let label = u16::try_from(parts.planes.len() + 1).ok()?;
    let mut errors = Vec::new();
    let mut seen = vec![false; camera.0 * camera.1];
    for (i, m) in mask.iter().enumerate() {
        if !*m {
            continue;
        }
        if let Some(l) = parts.plane_labels.get_mut(i) {
            *l = label;
        }
        if let Some(o) = parts.object_labels.get_mut(i) {
            *o = 0;
        }
        if let Some(&k) = point_at.get(i)
            && k != u32::MAX
        {
            errors.push(error(k as usize));
            if let Some(c) = pairs.to.get(k as usize).and_then(cam_index)
                && let Some(x) = seen.get_mut(c)
            {
                *x = true;
            }
        }
    }
    if errors.len() < WARP_MIN {
        return None;
    }
    errors.sort_by(f64::total_cmp);
    let group = parts.planes.iter().map(|p| p.group + 1).max().unwrap_or(0);
    parts.planes.push(Plane {
        homography: hom,
        pixels: errors.len(),
        error_px: [quantile(&errors, 0.5), quantile(&errors, 0.9)],
        group,
        bend: [0.0; 3],
    });
    Some(Claim { plane: parts.planes.len() - 1, camera: seen })
}

/// What [`claim_face`] claimed: the face's index in the projector's planes, and per camera pixel
/// whether a measured pixel of it lands there.
#[derive(Clone, Debug)]
pub struct Claim {
    pub plane: usize,
    pub camera: Vec<bool>,
}
