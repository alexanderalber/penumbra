//! Objects in front of a surface, from structured light scans, and the blend of several
//! projectors lighting them.
//!
//! **Parts** ([`segment`]): a scan of the empty scene (the reference) and one with the objects,
//! both from the same camera and projector. Per projector pixel the camera position moves
//! between the two by the parallax of whatever now stands in front of the surface: a few camera
//! pixels of noise on the surface itself, tens to hundreds on an object, because the camera
//! sits beside the projector's lens. Where it moves more than [`ObjectOptions::threshold`] is
//! the objects' silhouette (median filtered, opened, closed, holes filled, small bits dropped).
//! Where the parallax jumps (its gradient above [`ObjectOptions::split`] camera pixels per
//! projector pixel) one object ends and another starts at another depth; cut there, the
//! silhouette falls apart into parts, which then grow back over the cut. Last, pixels near the
//! silhouette whose camera position was only filled in by the decode, not measured, are dropped:
//! they inherited a neighbour's position, and at a silhouette that light lands on the surface
//! behind or on something too dark to decode.
//!
//! **Numbers** ([`number`]): parts of different projectors that cover the same camera pixels
//! are one object; objects are numbered by their height in the camera picture, top first, so
//! object `k` is the same in every projector.
//!
//! **Camera space** ([`camera_labels`]): every labelled projector pixel (eroded, since a
//! silhouette's edge pixels are half surface) through its camera position, closed, holes
//! filled, the largest piece per object. One picture shared by all projectors: content traced
//! from it lands on each object the same way in every projector.
//!
//! **Blend** ([`blend`]): where another projector lands on the same camera pixel, a projector's
//! weight falls towards its frame edge, `d_self / (d_self + d_others)` with `d` the distance to
//! the own frame edge capped at [`BlendOptions::ramp`]; elsewhere 1. The weights of all
//! projectors at one camera pixel add up to 1.
//!
//! **Smoothing** ([`smooth`]): a camera is usually coarser than a projector, so only some
//! projector pixels have a measured camera position and the rest are filled from the nearest;
//! content remapped through such a map has jagged edges. A normalised Gaussian convolution of
//! the measured positions, within each part, smooths it.

use crate::raster::{self, Rect};

/// Most pixels of a map or picture handled.
pub const MAX_PIXELS: usize = 1 << 24;
/// Most parts per projector and objects in all (label pictures are 8-bit).
pub const MAX_PARTS: usize = 255;
/// Most projectors handled together.
pub const MAX_PROJECTORS: usize = 16;
/// Median window for the parallax before the threshold.
const MEDIAN_MASK: usize = 5;
/// Median window for the parallax before its gradient.
const MEDIAN_SPLIT: usize = 7;
/// Opening and closing of the silhouette, projector pixels.
const MASK_OPEN: u32 = 3;
const MASK_CLOSE: u32 = 3;
/// How far the cut at a parallax jump is widened, projector pixels.
const SPLIT_WIDEN: u32 = 2;
/// Closing and opening of an object in camera space, camera pixels.
const CAMERA_CLOSE: u32 = 10;
const CAMERA_OPEN: u32 = 4;
/// Cell size, camera pixels, when comparing parts of different projectors.
const CELL: usize = 4;
/// Share of the smaller part's cells two parts must have in common to be one object.
const SAME_OBJECT: f32 = 0.3;
/// A part's pixels this close to its projector's frame (projector pixels, beyond the erosion)
/// are where it is cut off by the frame.
const FRAME: usize = 3;
/// Share of a part's cells at its frame edge that must touch another projector's part (within
/// [`TOUCH`] cells) for both to be one object, continuing past the frame.
const CONTINUES: f32 = 0.2;
const TOUCH: usize = 2;

/// A map as `scan.decode` writes it: per pixel red and green a position in the other picture
/// (0 to 1), -1 where there is none; blue 1 where measured (projector maps: 0.5 where filled,
/// 0 where empty; camera maps: the confidence).
#[derive(Clone, Debug)]
pub struct Map {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<[f32; 4]>,
}

impl Map {
    pub fn new(width: usize, height: usize, rgba: Vec<[f32; 4]>) -> Result<Self, String> {
        let n = width.checked_mul(height).ok_or("the map is too large")?;
        if n == 0 || n > MAX_PIXELS {
            return Err(format!("a map is 1 to {MAX_PIXELS} pixels, got {width} x {height}"));
        }
        if rgba.len() != n {
            return Err(format!("{} pixels for a {width} x {height} map", rgba.len()));
        }
        Ok(Self { width, height, rgba })
    }

    pub fn len(&self) -> usize {
        self.rgba.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }

    /// The position at pixel `i` (0 to 1), `None` where empty or not a usable number.
    pub fn uv(&self, i: usize) -> Option<[f32; 2]> {
        let [u, v, _, _] = *self.rgba.get(i)?;
        let ok = |c: f32| c.is_finite() && (0.0..=1.0).contains(&c);
        (ok(u) && ok(v)).then_some([u, v])
    }

    /// Pixel `i` has a measured (not filled) position.
    pub fn measured(&self, i: usize) -> bool {
        self.uv(i).is_some() && self.rgba.get(i).is_some_and(|p| p[2] > 0.75)
    }

    /// The pixel of a `w` x `h` picture that pixel `i` points at.
    pub fn pixel_in(&self, i: usize, w: usize, h: usize) -> Option<usize> {
        let [u, v] = self.uv(i)?;
        let x = ((u * w as f32) as usize).min(w.saturating_sub(1));
        let y = ((v * h as f32) as usize).min(h.saturating_sub(1));
        Some(y * w + x)
    }
}

/// How [`segment`] finds the objects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectOptions {
    /// Parallax that makes an object, camera pixels.
    pub threshold: f32,
    /// Parallax jump between two parts, camera pixels per projector pixel.
    pub split: f32,
    /// Smallest silhouette and part kept, projector pixels.
    pub min_area: usize,
    /// Parts shrink by this before they go to camera space, projector pixels.
    pub erode: u32,
    /// A pixel this far (projector pixels) from the nearest measured one is not trusted...
    pub unmeasured: f32,
    /// ...when it is also this close (projector pixels) to the silhouette's edge.
    pub rim: f32,
}

impl Default for ObjectOptions {
    fn default() -> Self {
        Self { threshold: 25.0, split: 2.0, min_area: 8000, erode: 2, unmeasured: 4.0, rim: 25.0 }
    }
}

impl ObjectOptions {
    pub fn check(&self) -> Result<(), String> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.threshold, 0.0, 1e5) {
            return Err("`threshold` is 0 to 100000 camera pixels".into());
        }
        if !ok(self.split, 0.01, 1e5) {
            return Err("`split` is 0.01 to 100000 camera pixels per projector pixel".into());
        }
        if self.erode > 64 {
            return Err("`erode` is 0 to 64 projector pixels".into());
        }
        if !ok(self.unmeasured, 0.0, 1e5) || !ok(self.rim, 0.0, 1e5) {
            return Err("`unmeasured` and `rim` are 0 to 100000 projector pixels".into());
        }
        Ok(())
    }
}

/// One projector's parts.
#[derive(Clone, Debug)]
pub struct Parts {
    pub width: usize,
    pub height: usize,
    /// Per projector pixel: 0 background, else the part (after [`number`], the object).
    pub labels: Vec<u16>,
    /// The cuts at parallax jumps inside the silhouette, for a picture of the parts.
    pub cuts: Vec<bool>,
    /// Pixels dropped as untrusted (filled in near the silhouette).
    pub dropped: Vec<bool>,
    /// Number of parts (after [`number`], the highest object number).
    pub count: usize,
    /// Pixels in the silhouette before the untrusted ones were dropped.
    pub silhouette: usize,
    /// Pixels where both scans have a camera position.
    pub compared: usize,
    /// Parts dropped beyond [`MAX_PARTS`].
    pub over: usize,
}

impl Parts {
    /// Pixels per label 1..=count.
    pub fn pixels(&self) -> Vec<usize> {
        let mut out = vec![0usize; self.count];
        for l in &self.labels {
            if *l > 0
                && let Some(c) = out.get_mut(usize::from(*l) - 1)
            {
                *c += 1;
            }
        }
        out
    }
}

/// The parts of the objects in front of the surface for one projector: `reference` and `scan`
/// are its `proj_to_cam` maps of the empty and the filled scene, `camera` the camera's size.
pub fn segment(
    reference: &Map,
    scan: &Map,
    camera: (usize, usize),
    o: &ObjectOptions,
) -> Result<Parts, String> {
    o.check()?;
    if (reference.width, reference.height) != (scan.width, scan.height) {
        return Err(format!(
            "the reference is {} x {}, the scan {} x {}: both must be of the same projector",
            reference.width, reference.height, scan.width, scan.height
        ));
    }
    let (w, h) = (scan.width, scan.height);
    let (cw, ch) = (camera.0 as f32, camera.1 as f32);
    let n = scan.len();
    let mut dx = vec![0f32; n];
    let mut dy = vec![0f32; n];
    let mut compared = 0usize;
    for (i, (x, y)) in dx.iter_mut().zip(dy.iter_mut()).enumerate() {
        if let (Some(a), Some(b)) = (reference.uv(i), scan.uv(i)) {
            *x = (b[0] - a[0]) * cw;
            *y = (b[1] - a[1]) * ch;
            compared += 1;
        }
    }
    let d: Vec<f32> = dx.iter().zip(&dy).map(|(x, y)| x.hypot(*y)).collect();
    let d = raster::median(w, h, &d, MEDIAN_MASK);
    let mask: Vec<bool> = d.iter().map(|v| *v > o.threshold).collect();
    let mask = raster::open(w, h, &mask, MASK_OPEN);
    let mask = raster::close(w, h, &mask, MASK_CLOSE);
    let mask = raster::fill_holes(w, h, &mask);
    let mask = keep_large(w, h, &mask, o.min_area);

    // Cuts where the parallax jumps.
    let dx = raster::median(w, h, &dx, MEDIAN_SPLIT);
    let dy = raster::median(w, h, &dy, MEDIAN_SPLIT);
    let mut jump = vec![false; n];
    for (i, j) in jump.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let g = |a: &[f32]| {
            let at = |xx: usize, yy: usize| a.get(yy * w + xx).copied().unwrap_or(0.0);
            let gx = match (x > 0, x + 1 < w) {
                (true, true) => (at(x + 1, y) - at(x - 1, y)) / 2.0,
                (false, true) => at(x + 1, y) - at(x, y),
                (true, false) => at(x, y) - at(x - 1, y),
                (false, false) => 0.0,
            };
            let gy = match (y > 0, y + 1 < h) {
                (true, true) => (at(x, y + 1) - at(x, y - 1)) / 2.0,
                (false, true) => at(x, y + 1) - at(x, y),
                (true, false) => at(x, y) - at(x, y - 1),
                (false, false) => 0.0,
            };
            gx.hypot(gy)
        };
        *j = g(&dx).max(g(&dy)) > o.split;
    }
    let jump = raster::dilate(w, h, &jump, SPLIT_WIDEN);
    let cuts: Vec<bool> = mask.iter().zip(&jump).map(|(m, j)| *m && *j).collect();
    let inner: Vec<bool> = mask.iter().zip(&jump).map(|(m, j)| *m && !*j).collect();
    let (comp, sizes) = raster::components(w, h, &inner);
    let mut big: Vec<(usize, usize)> = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s >= o.min_area.max(1))
        .map(|(k, s)| (*s, k + 1))
        .collect();
    big.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let over = big.len().saturating_sub(MAX_PARTS);
    big.truncate(MAX_PARTS);
    let mut map = vec![0u16; sizes.len() + 1];
    for (k, (_, c)) in big.iter().enumerate() {
        if let Some(m) = map.get_mut(*c) {
            *m = u16::try_from(k + 1).unwrap_or(0);
        }
    }
    let mut labels: Vec<u16> = comp.iter().map(|c| map.get(*c as usize).copied().unwrap_or(0)).collect();
    raster::grow(w, h, &mut labels, &mask);
    let silhouette = labels.iter().filter(|l| **l > 0).count();

    // Filled-in pixels near the silhouette's edge are not trusted.
    let measured: Vec<bool> = (0..n).map(|i| scan.measured(i)).collect();
    let (to_measured, _) = raster::distance(w, h, &measured);
    let outside: Vec<bool> = labels.iter().map(|l| *l == 0).collect();
    let (to_edge, _) = raster::distance(w, h, &outside);
    let mut dropped = vec![false; n];
    for (i, (l, dr)) in labels.iter_mut().zip(dropped.iter_mut()).enumerate() {
        let far = to_measured.get(i).is_some_and(|d| *d > o.unmeasured);
        let rim = to_edge.get(i).is_some_and(|d| *d < o.rim);
        if *l > 0 && far && rim {
            *l = 0;
            *dr = true;
        }
    }
    Ok(Parts { width: w, height: h, labels, cuts, dropped, count: big.len(), silhouette, compared, over })
}

/// The mask without its connected pieces smaller than `min_area`.
pub(crate) fn keep_large(w: usize, h: usize, mask: &[bool], min_area: usize) -> Vec<bool> {
    let (comp, sizes) = raster::components(w, h, mask);
    comp.iter().map(|c| *c > 0 && sizes.get(*c as usize - 1).is_some_and(|s| *s >= min_area.max(1))).collect()
}

/// Labels shrunk by `r`: a pixel stays when no background and no other label is within `r`
/// steps (both sides of a seam between two labels give way).
pub(crate) fn eroded_labels(w: usize, h: usize, labels: &[u16], r: u32) -> Vec<u16> {
    if r == 0 {
        return labels.to_vec();
    }
    let edge: Vec<bool> = (0..labels.len())
        .map(|i| {
            let l = labels.get(i).copied().unwrap_or(0);
            l == 0
                || raster::neighbours4(i, w, h)
                    .into_iter()
                    .flatten()
                    .any(|j| labels.get(j).is_some_and(|m| *m != l && *m != 0))
        })
        .collect();
    let d = raster::city_distance(w, h, &edge);
    labels.iter().zip(&d).map(|(l, d)| if *d > r { *l } else { 0 }).collect()
}

/// The objects after [`number`].
#[derive(Clone, Debug, PartialEq)]
pub struct Objects {
    pub count: usize,
    /// Per object, its height in the camera picture (median, camera pixels).
    pub camera_y: Vec<f32>,
    /// Objects beyond [`MAX_PARTS`], dropped.
    pub over: usize,
}

/// Numbers the parts of all projectors as objects: parts of different projectors that cover the
/// same camera pixels are one object, numbered by height in the camera picture (top first).
/// `scans[p]` is projector `p`'s `proj_to_cam` map of the scene with the objects.
pub fn number(
    parts: &mut [Parts],
    scans: &[&Map],
    camera: (usize, usize),
    erode: u32,
) -> Result<Objects, String> {
    number_parts(parts, scans, camera, erode, true)
}

/// [`number`] for flat faces: only parts sharing camera pixels are one face, not those that
/// merely continue past a projector's frame (a face cut off there may well end at another
/// face of another projector).
pub fn number_faces(
    parts: &mut [Parts],
    scans: &[&Map],
    camera: (usize, usize),
    erode: u32,
) -> Result<Objects, String> {
    number_parts(parts, scans, camera, erode, false)
}

fn number_parts(
    parts: &mut [Parts],
    scans: &[&Map],
    camera: (usize, usize),
    erode: u32,
    across_frames: bool,
) -> Result<Objects, String> {
    if parts.len() != scans.len() {
        return Err("one scan per projector".into());
    }
    let (cw, ch) = camera;
    let gw = cw.div_ceil(CELL).max(1);
    // Per part: its camera cells (sorted), cell bounds and the camera heights of its pixels.
    struct Node {
        projector: usize,
        local: u16,
        cells: Vec<u32>,
        bounds: Option<Rect>,
        ys: Vec<f32>,
        /// Cells where the part is cut off by its projector's frame (sorted).
        frame: Vec<u32>,
    }
    let mut nodes: Vec<Node> = Vec::new();
    for (p, (part, scan)) in parts.iter().zip(scans).enumerate() {
        if (part.width, part.height) != (scan.width, scan.height) {
            return Err(format!("projector {p}: parts and scan differ in size"));
        }
        let eroded = eroded_labels(part.width, part.height, &part.labels, erode);
        let base = nodes.len();
        for l in 1..=part.count {
            nodes.push(Node {
                projector: p,
                local: u16::try_from(l).unwrap_or(0),
                cells: Vec::new(),
                bounds: None,
                ys: Vec::new(),
                frame: Vec::new(),
            });
        }
        for (i, l) in eroded.iter().enumerate() {
            if *l == 0 {
                continue;
            }
            let Some(c) = scan.pixel_in(i, cw, ch) else { continue };
            let Some(node) = nodes.get_mut(base + usize::from(*l) - 1) else { continue };
            let (x, y) = (c % cw.max(1), c / cw.max(1));
            node.cells.push(((y / CELL) * gw + x / CELL) as u32);
            node.ys.push(y as f32);
            let (px, py) = (i % part.width.max(1), i / part.width.max(1));
            let margin = erode as usize + FRAME;
            let at_frame =
                px < margin || py < margin || px + margin >= part.width || py + margin >= part.height;
            if at_frame {
                node.frame.push(((y / CELL) * gw + x / CELL) as u32);
            }
        }
        for node in nodes.get_mut(base..).unwrap_or(&mut []) {
            node.cells.sort_unstable();
            node.cells.dedup();
            node.frame.sort_unstable();
            node.frame.dedup();
            node.bounds = node.cells.iter().fold(None, |r: Option<Rect>, c| {
                let (x, y) = (*c as usize % gw, *c as usize / gw);
                Some(match r {
                    None => Rect { x0: x, y0: y, x1: x + 1, y1: y + 1 },
                    Some(r) => {
                        Rect { x0: r.x0.min(x), y0: r.y0.min(y), x1: r.x1.max(x + 1), y1: r.y1.max(y + 1) }
                    }
                })
            });
        }
    }

    // Same object: parts of different projectors sharing enough cells.
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while let Some(&p) = parent.get(i) {
            if p == i {
                break;
            }
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(s) = parent.get_mut(i) {
                *s = gp;
            }
            i = p;
        }
        i
    }
    for a in 0..nodes.len() {
        for b in a + 1..nodes.len() {
            let (Some(na), Some(nb)) = (nodes.get(a), nodes.get(b)) else { continue };
            if na.projector == nb.projector {
                continue;
            }
            let (Some(ra), Some(rb)) = (na.bounds, nb.bounds) else { continue };
            if ra.x1 <= rb.x0 || rb.x1 <= ra.x0 || ra.y1 <= rb.y0 || rb.y1 <= ra.y0 {
                continue;
            }
            let common = shared(&na.cells, &nb.cells);
            let smaller = na.cells.len().min(nb.cells.len()).max(1);
            let same = common as f32 / smaller as f32 >= SAME_OBJECT
                || (across_frames
                    && (continues(&na.frame, &nb.cells, gw) || continues(&nb.frame, &na.cells, gw)));
            if same {
                let (x, y) = (root(&mut parent, a), root(&mut parent, b));
                if let Some(s) = parent.get_mut(x) {
                    *s = y;
                }
            }
        }
    }
    // Groups, ordered by camera height.
    let mut groups: Vec<(usize, Vec<f32>)> = Vec::new();
    let mut group_of = vec![usize::MAX; nodes.len()];
    for i in 0..nodes.len() {
        let r = root(&mut parent, i);
        let g = match groups.iter().position(|(gr, _)| *gr == r) {
            Some(g) => g,
            None => {
                groups.push((r, Vec::new()));
                groups.len() - 1
            }
        };
        if let Some(s) = group_of.get_mut(i) {
            *s = g;
        }
        if let (Some(node), Some(group)) = (nodes.get(i), groups.get_mut(g)) {
            group.1.extend_from_slice(&node.ys);
        }
    }
    let mut order: Vec<(f32, usize)> = groups
        .iter_mut()
        .enumerate()
        .map(|(g, (_, ys))| {
            let mid = ys.len() / 2;
            let y =
                if ys.is_empty() { f32::INFINITY } else { *ys.select_nth_unstable_by(mid, f32::total_cmp).1 };
            (y, g)
        })
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let over = order.len().saturating_sub(MAX_PARTS);
    order.truncate(MAX_PARTS);
    let mut number_of = vec![0u16; groups.len()];
    for (k, (_, g)) in order.iter().enumerate() {
        if let Some(s) = number_of.get_mut(*g) {
            *s = u16::try_from(k + 1).unwrap_or(0);
        }
    }
    // Relabel every projector's parts.
    for (p, part) in parts.iter_mut().enumerate() {
        let mut map = vec![0u16; part.count + 1];
        for (i, node) in nodes.iter().enumerate() {
            if node.projector == p
                && let Some(slot) = map.get_mut(usize::from(node.local))
            {
                *slot = group_of.get(i).and_then(|g| number_of.get(*g)).copied().unwrap_or(0);
            }
        }
        for l in &mut part.labels {
            *l = map.get(usize::from(*l)).copied().unwrap_or(0);
        }
        part.count = order.len();
    }
    Ok(Objects { count: order.len(), camera_y: order.iter().map(|(y, _)| *y).collect(), over })
}

/// A part cut off by its projector's frame continues in another projector's part: enough of
/// its `frame` cells lie next to `other`'s cells (sorted cell indices, `gw` cells a row).
fn continues(frame: &[u32], other: &[u32], gw: usize) -> bool {
    if frame.len() < 4 {
        return false;
    }
    let t = TOUCH as i64;
    let touching = frame
        .iter()
        .filter(|c| {
            let (x, y) = ((**c as usize % gw) as i64, (**c as usize / gw) as i64);
            (-t..=t).any(|dy| {
                (-t..=t).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0
                        && ny >= 0
                        && (nx as usize) < gw
                        && other.binary_search(&((ny as usize * gw + nx as usize) as u32)).is_ok()
                })
            })
        })
        .count();
    touching as f32 >= CONTINUES * frame.len() as f32
}

/// Elements two sorted lists have in common.
fn shared(a: &[u32], b: &[u32]) -> usize {
    let (mut i, mut j, mut n) = (0usize, 0usize, 0usize);
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// The objects in camera space: per camera pixel 0 or the object number. `parts` are numbered
/// by [`number`]; `scans[p]` is projector `p`'s `proj_to_cam` map of the scene with the objects.
pub fn camera_labels(
    parts: &[Parts],
    scans: &[&Map],
    camera: (usize, usize),
    erode: u32,
) -> Result<Vec<u16>, String> {
    let (cw, ch) = camera;
    let n =
        cw.checked_mul(ch).filter(|n| *n > 0 && *n <= MAX_PIXELS).ok_or("the camera size is out of range")?;
    let count = parts.iter().map(|p| p.count).max().unwrap_or(0).min(MAX_PARTS);
    let mut points: Vec<Vec<u32>> = vec![Vec::new(); count];
    for (part, scan) in parts.iter().zip(scans) {
        let eroded = eroded_labels(part.width, part.height, &part.labels, erode);
        for (i, l) in eroded.iter().enumerate() {
            if *l == 0 {
                continue;
            }
            if let (Some(c), Some(list)) = (scan.pixel_in(i, cw, ch), points.get_mut(usize::from(*l) - 1)) {
                list.push(c as u32);
            }
        }
    }
    let mut out = vec![0u16; n];
    // Where objects overlap: the one whose own pixels (closed, before holes are filled) are
    // there, then the smaller one (the one in front, usually).
    let mut rank: Vec<(bool, usize)> = vec![(false, usize::MAX); n];
    for (k, list) in points.iter().enumerate() {
        let mut splat = vec![false; n];
        for c in list {
            if let Some(s) = splat.get_mut(*c as usize) {
                *s = true;
            }
        }
        let Some(b) = raster::bounds(cw, &splat, |s| *s) else { continue };
        let r = b.grown((CAMERA_CLOSE + CAMERA_OPEN + 2) as usize, cw, ch);
        let (rw, rh) = (r.width(), r.height());
        let crop = r.crop(cw, &splat);
        let closed = raster::close(rw, rh, &crop, CAMERA_CLOSE);
        let filled = raster::fill_holes(rw, rh, &closed);
        let opened = raster::open(rw, rh, &filled, CAMERA_OPEN);
        let (comp, sizes) = raster::components(rw, rh, &opened);
        let Some((best, area)) = sizes.iter().enumerate().max_by_key(|(_, s)| **s).map(|(i, s)| (i + 1, *s))
        else {
            continue;
        };
        let label = u16::try_from(k + 1).unwrap_or(0);
        for (j, c) in comp.iter().enumerate() {
            if *c as usize != best {
                continue;
            }
            let (x, y) = (r.x0 + j % rw, r.y0 + j / rw);
            let i = y * cw + x;
            let key = (closed.get(j).copied().unwrap_or(false), area);
            if let (Some(o), Some(rk)) = (out.get_mut(i), rank.get_mut(i)) {
                let better = *o == 0 || (key.0 && !rk.0) || (key.0 == rk.0 && key.1 < rk.1);
                if better {
                    *o = label;
                    *rk = key;
                }
            }
        }
    }
    Ok(out)
}

/// How [`blend`] weighs overlapping projectors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendOptions {
    /// Frame-edge distance (projector pixels) beyond which a projector counts fully.
    pub ramp: f32,
    /// Holes in a camera map are filled from the nearest position this close, camera pixels.
    pub reach: f32,
    /// Gaussian blur of the weights, projector pixels.
    pub blur: f32,
}

impl Default for BlendOptions {
    fn default() -> Self {
        Self { ramp: 200.0, reach: 6.0, blur: 3.0 }
    }
}

impl BlendOptions {
    pub fn check(&self) -> Result<(), String> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.ramp, 1.0, 1e5) {
            return Err("`ramp` is 1 to 100000 projector pixels".into());
        }
        if !ok(self.reach, 0.0, 64.0) {
            return Err("`reach` is 0 to 64 camera pixels".into());
        }
        if !ok(self.blur, 0.0, 64.0) {
            return Err("`blur` is 0 to 64 projector pixels".into());
        }
        Ok(())
    }
}

/// One projector for [`blend`].
pub struct BlendInput<'a> {
    /// Its `proj_to_cam` map.
    pub to_camera: &'a Map,
    /// Its `cam_to_proj` map.
    pub from_camera: &'a Map,
    /// Where it may light (projector pixels, non-zero inside), e.g. its object labels.
    pub mask: Option<&'a [u16]>,
}

/// Per projector its weights (projector pixels, 0 to 1) and the pixels it shares with another.
pub struct Blend {
    pub weights: Vec<Vec<f32>>,
    pub shared: Vec<usize>,
}

/// The blend weights of projectors that light the same camera pixels.
pub fn blend(inputs: &[BlendInput<'_>], o: &BlendOptions) -> Result<Blend, String> {
    o.check()?;
    if inputs.len() > MAX_PROJECTORS {
        return Err(format!("at most {MAX_PROJECTORS} projectors"));
    }
    let Some(first) = inputs.first() else { return Err("no projectors".into()) };
    let camera = (first.from_camera.width, first.from_camera.height);
    for (p, input) in inputs.iter().enumerate() {
        if (input.from_camera.width, input.from_camera.height) != camera {
            return Err(format!("projector {p}'s camera map is not of the same size as projector 0's"));
        }
        if input.mask.is_some_and(|m| m.len() != input.to_camera.len()) {
            return Err(format!("projector {p}'s mask is not of its size"));
        }
    }
    let (cw, ch) = camera;
    // Per projector and camera pixel: the projector pixel there (holes filled nearby).
    let lookups: Vec<Vec<u32>> = inputs
        .iter()
        .map(|input| {
            let m = input.from_camera;
            let (pw, ph) = (input.to_camera.width, input.to_camera.height);
            let has: Vec<bool> = (0..m.len()).map(|i| m.uv(i).is_some()).collect();
            let (dist, nearest) = raster::distance(cw, ch, &has);
            (0..m.len())
                .map(|i| {
                    let near = dist.get(i).is_some_and(|d| *d <= o.reach);
                    let src = nearest.get(i).copied().flatten().filter(|_| near);
                    src.and_then(|s| m.pixel_in(s as usize, pw, ph)).map_or(u32::MAX, |p| p as u32)
                })
                .collect()
        })
        .collect();
    let edge = |i: usize, w: usize, h: usize| -> f32 {
        let (x, y) = (i % w.max(1), i / w.max(1));
        let d = x.min(w.saturating_sub(1 + x)).min(y).min(h.saturating_sub(1 + y));
        (d as f32).min(o.ramp)
    };
    let mut weights = Vec::with_capacity(inputs.len());
    let mut shared = Vec::with_capacity(inputs.len());
    for (s, input) in inputs.iter().enumerate() {
        let (pw, ph) = (input.to_camera.width, input.to_camera.height);
        let inside = |i: usize| input.mask.is_none_or(|m| m.get(i).is_some_and(|v| *v > 0));
        let mut w = vec![1f32; input.to_camera.len()];
        let mut count = 0usize;
        for (i, wi) in w.iter_mut().enumerate() {
            if !inside(i) {
                continue;
            }
            let Some(c) = input.to_camera.pixel_in(i, cw, ch) else { continue };
            let own = edge(i, pw, ph);
            let (mut sum, mut others) = (own, 0usize);
            for (t, other) in inputs.iter().enumerate() {
                if t == s {
                    continue;
                }
                let q = lookups.get(t).and_then(|l| l.get(c)).copied().unwrap_or(u32::MAX);
                if q == u32::MAX {
                    continue;
                }
                let q = q as usize;
                if other.mask.is_some_and(|m| m.get(q).is_none_or(|v| *v == 0)) {
                    continue;
                }
                sum += edge(q, other.to_camera.width, other.to_camera.height);
                others += 1;
            }
            if others > 0 {
                count += 1;
                *wi = if sum > 0.0 { own / sum } else { 1.0 / (others + 1) as f32 };
            }
        }
        let mut w = raster::gaussian(pw, ph, &w, o.blur);
        if input.mask.is_some() {
            for (i, wi) in w.iter_mut().enumerate() {
                if !inside(i) {
                    *wi = 0.0;
                }
            }
        }
        weights.push(w);
        shared.push(count);
    }
    Ok(Blend { weights, shared })
}

/// Largest Gaussian for [`smooth`], projector pixels.
pub const MAX_SIGMA: f32 = 64.0;

/// A `proj_to_cam` map smoothed by a normalised Gaussian convolution (standard deviation
/// `sigma`) of its measured positions: within each part of `parts` (projector pixels, non-zero
/// values are parts) when given, else within the pixels that have a position. Pixels of a part
/// without a position get one (marked filled); everything else stays as it was.
pub fn smooth(map: &Map, parts: Option<&[u16]>, sigma: f32) -> Result<Vec<[f32; 4]>, String> {
    if !sigma.is_finite() || !(0.0..=MAX_SIGMA).contains(&sigma) {
        return Err(format!("`sigma` is 0 to {MAX_SIGMA} projector pixels"));
    }
    let (w, h) = (map.width, map.height);
    let mut out = map.rgba.clone();
    let region: Vec<u16> = match parts {
        Some(p) if p.len() != map.len() => return Err("the parts are not of the map's size".into()),
        Some(p) => p.to_vec(),
        None => (0..map.len()).map(|i| u16::from(map.uv(i).is_some())).collect(),
    };
    let mut ids: Vec<u16> = region.iter().copied().filter(|v| *v > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > MAX_PARTS + 1 {
        return Err(format!("{} parts; at most {}", ids.len(), MAX_PARTS + 1));
    }
    if sigma == 0.0 {
        return Ok(out);
    }
    let margin = (3.0 * sigma).ceil() as usize + 1;
    for id in ids {
        let Some(b) = raster::bounds(w, &region, |v| *v == id) else { continue };
        let r = b.grown(margin, w, h);
        let (rw, rh) = (r.width(), r.height());
        let mut wt = vec![0f32; rw * rh];
        let mut nu = vec![0f32; rw * rh];
        let mut nv = vec![0f32; rw * rh];
        for j in 0..rw * rh {
            let i = (r.y0 + j / rw) * w + r.x0 + j % rw;
            if region.get(i) == Some(&id)
                && map.measured(i)
                && let Some([u, v]) = map.uv(i)
            {
                if let Some(s) = wt.get_mut(j) {
                    *s = 1.0;
                }
                if let Some(s) = nu.get_mut(j) {
                    *s = u;
                }
                if let Some(s) = nv.get_mut(j) {
                    *s = v;
                }
            }
        }
        let den = raster::gaussian(rw, rh, &wt, sigma);
        let nu = raster::gaussian(rw, rh, &nu, sigma);
        let nv = raster::gaussian(rw, rh, &nv, sigma);
        for j in 0..rw * rh {
            let i = (r.y0 + j / rw) * w + r.x0 + j % rw;
            let d = den.get(j).copied().unwrap_or(0.0);
            if region.get(i) != Some(&id) || d <= 1e-3 {
                continue;
            }
            let (u, v) = (nu.get(j).copied().unwrap_or(0.0) / d, nv.get(j).copied().unwrap_or(0.0) / d);
            let had = map.uv(i).is_some();
            if let Some(px) = out.get_mut(i) {
                px[0] = u;
                px[1] = v;
                if !had {
                    px[2] = 0.5;
                }
            }
        }
    }
    Ok(out)
}

/// How well blend weights add up: over the camera pixels where at least two projectors land
/// (inside their masks), how many there are and the mean of `|sum of weights - 1|`.
pub fn coverage(inputs: &[BlendInput<'_>], weights: &[Vec<f32>]) -> (usize, f32) {
    let Some(first) = inputs.first() else { return (0, 0.0) };
    let (cw, ch) = (first.from_camera.width, first.from_camera.height);
    let (mut n, mut err) = (0usize, 0f64);
    for c in 0..cw * ch {
        let (mut sum, mut lit) = (0f32, 0usize);
        for (input, w) in inputs.iter().zip(weights) {
            let Some(p) = input.from_camera.pixel_in(c, input.to_camera.width, input.to_camera.height) else {
                continue;
            };
            if input.mask.is_some_and(|m| m.get(p).is_none_or(|v| *v == 0)) {
                continue;
            }
            sum += w.get(p).copied().unwrap_or(0.0);
            lit += 1;
        }
        if lit >= 2 {
            n += 1;
            err += f64::from((sum - 1.0).abs());
        }
    }
    (n, if n > 0 { (err / n as f64) as f32 } else { 0.0 })
}
