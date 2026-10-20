//! Mask to shapes: the outlines of the regions of a mask or label picture as polygons, simplified,
//! to become `poly` lines of a `tex.shapes`. Pure CPU math on read-back pixels.
//!
//! A **region** is a 4-connected set of pixels of one class: inside the mask (the value above the
//! threshold), or one value of a label picture. Pixels touching only at a corner are separate
//! regions; touching regions of a mask are one (a label picture keeps them apart).
//!
//! The outline runs along the pixel edges with the region on the right, which is clockwise on
//! screen. Every edge gives one point, moved across the edge to where the values cross the
//! threshold between the two pixels (so an anti-aliased edge is found to a fraction of a pixel,
//! a hard one runs through the edges' midpoints); where the outline turns, the corner where the
//! two moved edges meet is a point too (so a hard-edged rectangle keeps its corners).
//! Douglas-Peucker then drops the points within the tolerance of the line their neighbours span.
//!
//! **Holes:** a polygon cannot have one, so only the outer outline of a region is traced and its
//! holes are filled; how many is counted (from the region's Euler number). A region inside a
//! hole is one of its own and comes after the one around it, so it is drawn over it.

use std::collections::HashMap;

use crate::shapes::{Geom, MAX_POINTS, Shape};

/// Largest picture traced, in pixels (4096 x 2048, more than a 4K UHD frame).
pub const MAX_PIXELS: usize = 1 << 23;
/// Most distinct values a label picture may have.
pub const MAX_LABELS: usize = 65536;

/// The channel a mask is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Alpha when any pixel is less than opaque (a cut-out), else luma (a black and white mask).
    Auto,
    Red,
    Green,
    Blue,
    Alpha,
    Luma,
}

impl Channel {
    pub const NAMES: [&'static str; 6] = ["auto", "red", "green", "blue", "alpha", "luma"];

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "auto" => Channel::Auto,
            "red" => Channel::Red,
            "green" => Channel::Green,
            "blue" => Channel::Blue,
            "alpha" => Channel::Alpha,
            "luma" => Channel::Luma,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Channel::Auto => "auto",
            Channel::Red => "red",
            Channel::Green => "green",
            Channel::Blue => "blue",
            Channel::Alpha => "alpha",
            Channel::Luma => "luma",
        }
    }
}

/// One channel of RGBA pixels as values, and the channel taken (never `Auto`). Luma uses the
/// Rec. 709 weights on the values as they are (linear).
pub fn channel(pixels: &[[f32; 4]], channel: Channel) -> (Vec<f32>, Channel) {
    let channel = match channel {
        Channel::Auto if pixels.iter().any(|p| p[3] < 0.999) => Channel::Alpha,
        Channel::Auto => Channel::Luma,
        c => c,
    };
    let values = pixels
        .iter()
        .map(|p| match channel {
            Channel::Red => p[0],
            Channel::Green => p[1],
            Channel::Blue => p[2],
            Channel::Alpha => p[3],
            Channel::Auto | Channel::Luma => 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2],
        })
        .collect();
    (values, channel)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Inside the mask is above this (not used for labels).
    pub threshold: f32,
    /// Inside the mask is below the threshold instead (dark objects on a bright wall).
    pub invert: bool,
    /// Every distinct value is a class of its own, 0 and below are the background, instead of
    /// one class above the threshold.
    pub labels: bool,
    /// Douglas-Peucker tolerance in pixels; 0 keeps every point.
    pub tolerance: f64,
    /// Regions of fewer pixels are specks and dropped.
    pub min_area: u64,
    /// Most regions kept: the largest.
    pub max_regions: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { threshold: 0.5, invert: false, labels: false, tolerance: 1.0, min_area: 64, max_regions: 4096 }
    }
}

/// One region's outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// In pixels of the picture, (0, 0) its top left corner, clockwise on screen; at most
    /// [`MAX_POINTS`].
    pub points: Vec<[f64; 2]>,
    /// Pixels in the region, holes not counted.
    pub area: u64,
    /// Holes filled because a polygon has none.
    pub holes: u64,
    /// The value of its first pixel (for labels, the label).
    pub value: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traced {
    /// In the order of their top left pixels, row by row.
    pub regions: Vec<Region>,
    /// Regions dropped as smaller than `min_area` (or too thin to stay a polygon).
    pub specks: usize,
    /// Regions dropped beyond `max_regions` (the smallest).
    pub dropped: usize,
    /// Regions simplified with a larger tolerance than asked to stay within [`MAX_POINTS`].
    pub coarser: usize,
}

impl Traced {
    /// Holes filled in the regions kept.
    pub fn holes(&self) -> u64 {
        self.regions.iter().map(|r| r.holes).sum()
    }
}

/// Traces the regions of `values`, `width` x `height` row by row from the top.
pub fn trace(width: u32, height: u32, values: &[f32], o: &Options) -> Result<Traced, String> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or("the picture is too large")?;
    if n > MAX_PIXELS {
        return Err(format!("the picture has {width} x {height} pixels; at most {MAX_PIXELS} are traced"));
    }
    if n != values.len() {
        return Err(format!("{width} x {height} pixels but {} values", values.len()));
    }
    if !o.tolerance.is_finite() || o.tolerance < 0.0 {
        return Err("the tolerance must be 0 or more".into());
    }
    if !o.threshold.is_finite() {
        return Err("the threshold must be a number".into());
    }
    let class = classes(values, o)?;
    let (comp, comps) = components(w, h, &class);
    let euler = euler_numbers(w, h, &comp, comps.len());

    // Specks out, then the largest `max_regions`, back in reading order.
    let mut keep: Vec<(u32, usize, u64)> = Vec::new();
    let mut traced = Traced::default();
    for (label, (start, area)) in (1u32..).zip(&comps) {
        if *area < o.min_area {
            traced.specks += 1;
        } else {
            keep.push((label, *start, *area));
        }
    }
    if keep.len() > o.max_regions {
        keep.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
        traced.dropped = keep.len() - o.max_regions;
        keep.truncate(o.max_regions);
        keep.sort_by_key(|k| k.1);
    }

    let grid = Grid { w, h, comp: &comp, values, threshold: o.threshold, labels: o.labels };
    for (label, start, area) in keep {
        let outline = grid.outline(label, start);
        let (points, coarser) = simplify_within(&outline, o.tolerance, MAX_POINTS);
        if points.len() < 3 {
            traced.specks += 1;
            continue;
        }
        if coarser {
            traced.coarser += 1;
        }
        let e = euler.get(label as usize).copied().unwrap_or(4);
        let holes = u64::try_from(1 - e / 4).unwrap_or(0);
        let value = values.get(start).copied().unwrap_or(0.0);
        traced.regions.push(Region { points, area, holes, value });
    }
    Ok(traced)
}

/// The regions as `poly` shapes tagged `tag`, in the surface coordinates of the picture
/// (`width` x `height` pixels): a pixel position divided by the picture's size.
pub fn to_shapes(regions: &[Region], width: u32, height: u32, tag: &str) -> Vec<Shape> {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    regions
        .iter()
        .map(|r| Shape {
            tag: tag.to_string(),
            geom: Geom::Poly(r.points.iter().map(|p| [p[0] / w, p[1] / h]).collect()),
            repeat: None,
        })
        .collect()
}

/// Per pixel its class: 0 for the background.
fn classes(values: &[f32], o: &Options) -> Result<Vec<u32>, String> {
    if !o.labels {
        // NaN is outside either way.
        let inside = |v: f32| if o.invert { v < o.threshold } else { v > o.threshold };
        return Ok(values.iter().map(|v| u32::from(inside(*v))).collect());
    }
    let mut keys: HashMap<i64, u32> = HashMap::new();
    let mut class = Vec::with_capacity(values.len());
    for v in values {
        // Quantised finely enough to tell 8-bit labels apart after an sRGB decode.
        let key = if v.is_finite() { (f64::from(*v) * 65536.0).round() as i64 } else { 0 };
        if key <= 0 {
            class.push(0);
            continue;
        }
        let next = u32::try_from(keys.len() + 1).unwrap_or(u32::MAX);
        let c = *keys.entry(key).or_insert(next);
        if keys.len() > MAX_LABELS {
            return Err(format!(
                "more than {MAX_LABELS} distinct values: not a label picture (trace it as a mask)"
            ));
        }
        class.push(c);
    }
    Ok(class)
}

/// Per pixel its region (0: none, else 1 + the index in the list) and per region its first
/// pixel and its area.
fn components(w: usize, h: usize, class: &[u32]) -> (Vec<u32>, Vec<(usize, u64)>) {
    let mut comp = vec![0u32; class.len()];
    let mut comps: Vec<(usize, u64)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..class.len() {
        let c = class.get(start).copied().unwrap_or(0);
        if c == 0 || comp.get(start).copied().unwrap_or(1) != 0 {
            continue;
        }
        // At most one region per two pixels, so this fits.
        let label = u32::try_from(comps.len() + 1).unwrap_or(u32::MAX);
        let mut area = 0u64;
        if let Some(slot) = comp.get_mut(start) {
            *slot = label;
        }
        stack.push(start);
        while let Some(i) = stack.pop() {
            area += 1;
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if class.get(j) == Some(&c)
                    && let Some(slot) = comp.get_mut(j)
                    && *slot == 0
                {
                    *slot = label;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        comps.push((start, area));
    }
    (comp, comps)
}

/// Four times the Euler number (pieces minus holes) of every region under 4-connectivity, by
/// Gray's bit quads: per 2 x 2 window, +1 with one pixel of the region in it, -1 with three, +2
/// with two on a diagonal. Indexed by region label.
fn euler_numbers(w: usize, h: usize, comp: &[u32], count: usize) -> Vec<i64> {
    let mut e = vec![0i64; count + 1];
    let at = |x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return 0;
        }
        comp.get(y as usize * w + x as usize).copied().unwrap_or(0)
    };
    for y in -1..h as isize {
        for x in -1..w as isize {
            let q = [at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)];
            for (k, &l) in q.iter().enumerate() {
                if l == 0 || q.iter().take(k).any(|m| *m == l) {
                    continue;
                }
                let m = q.map(|v| v == l);
                let d = match m.iter().filter(|b| **b).count() {
                    1 => 1,
                    3 => -1,
                    2 if (m[0] && m[3]) || (m[1] && m[2]) => 2,
                    _ => 0,
                };
                if let Some(s) = e.get_mut(l as usize) {
                    *s += d;
                }
            }
        }
    }
    e
}

/// East, south, west, north: y grows downwards.
const DIRS: [(isize, isize); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

struct Grid<'a> {
    w: usize,
    h: usize,
    comp: &'a [u32],
    values: &'a [f32],
    threshold: f32,
    labels: bool,
}

impl Grid<'_> {
    fn index(&self, x: isize, y: isize) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h)
            .then(|| y as usize * self.w + x as usize)
    }

    fn is(&self, label: u32, x: isize, y: isize) -> bool {
        self.index(x, y).and_then(|i| self.comp.get(i)) == Some(&label)
    }

    /// Where between the centres of `inside` and `outside` the values cross the threshold:
    /// 0 at the inside one, 1 at the outside one, a half for labels and the picture's border.
    fn crossing(&self, inside: (isize, isize), outside: (isize, isize)) -> f64 {
        if self.labels {
            return 0.5;
        }
        let value = |p: (isize, isize)| self.index(p.0, p.1).and_then(|i| self.values.get(i)).copied();
        let (Some(vi), Some(vo)) = (value(inside), value(outside)) else { return 0.5 };
        let t = f64::from(vi - self.threshold) / f64::from(vi - vo);
        if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.5 }
    }

    /// The outer outline of region `label`, starting at the top left corner of its first pixel
    /// `start` (nothing is above it, so its top edge is on the outer outline), heading east with
    /// the region on the right. Begins at the topmost, then leftmost point.
    fn outline(&self, label: u32, start: usize) -> Vec<[f64; 2]> {
        let origin = ((start % self.w.max(1)) as isize, (start / self.w.max(1)) as isize);
        let mut corner = origin;
        let mut dir = 0usize;
        let mut points: Vec<[f64; 2]> = Vec::new();
        // The previous edge's offset across itself (towards the outside), for the corner point.
        let mut last: Option<(usize, f64)> = None;
        let mut first: Option<(usize, f64)> = None;
        // Every edge is walked at most once; the bound only guards against a mistake.
        let limit = 4 * (self.w + 1) * (self.h + 1) + 8;
        for _ in 0..limit {
            let (dx, dy) = DIRS.get(dir).copied().unwrap_or((1, 0));
            let (rx, ry) = DIRS.get((dir + 1) % 4).copied().unwrap_or((0, 1));
            // Doubled coordinates: a pixel's centre is at 2p + 1.
            let mid2 = (2 * corner.0 + dx, 2 * corner.1 + dy);
            let inside = ((mid2.0 + rx - 1) / 2, (mid2.1 + ry - 1) / 2);
            let outside = ((mid2.0 - rx - 1) / 2, (mid2.1 - ry - 1) / 2);
            let offset = self.crossing(inside, outside) - 0.5;
            // Towards the outside is to the left: minus the right.
            let left = (-(rx as f64), -(ry as f64));
            if let Some((ld, lo)) = last
                && ld != dir
            {
                let (pdx, pdy) = DIRS.get((ld + 3) % 4).copied().unwrap_or((0, -1));
                points.push([
                    corner.0 as f64 + lo * pdx as f64 + offset * left.0,
                    corner.1 as f64 + lo * pdy as f64 + offset * left.1,
                ]);
            }
            points.push([mid2.0 as f64 / 2.0 + offset * left.0, mid2.1 as f64 / 2.0 + offset * left.1]);
            last = Some((dir, offset));
            first.get_or_insert((dir, offset));
            // At the edge's end: right when the pixel ahead on the right is out, left when the
            // one ahead on the left is in (it shares an edge with the one ahead on the right).
            corner = (corner.0 + dx, corner.1 + dy);
            let ahead_right =
                (corner.0 + (dx + rx - 1).div_euclid(2), corner.1 + (dy + ry - 1).div_euclid(2));
            let ahead_left = (corner.0 + (dx - rx - 1).div_euclid(2), corner.1 + (dy - ry - 1).div_euclid(2));
            dir = if !self.is(label, ahead_right.0, ahead_right.1) {
                (dir + 1) % 4
            } else if self.is(label, ahead_left.0, ahead_left.1) {
                (dir + 3) % 4
            } else {
                dir
            };
            if corner == origin && dir == 0 {
                break;
            }
        }
        // The corner the outline closes on.
        if let (Some((ld, lo)), Some((fd, fo))) = (last, first)
            && ld != fd
        {
            let (pdx, pdy) = DIRS.get((ld + 3) % 4).copied().unwrap_or((0, -1));
            let (fdx, fdy) = DIRS.get((fd + 3) % 4).copied().unwrap_or((0, -1));
            points.push([
                origin.0 as f64 + lo * pdx as f64 + fo * fdx as f64,
                origin.1 as f64 + lo * pdy as f64 + fo * fdy as f64,
            ]);
        }
        // Start at the topmost, then leftmost point: an extreme point anchors the simplification.
        if let Some(k) = points
            .iter()
            .enumerate()
            .min_by(|a, b| a.1[1].total_cmp(&b.1[1]).then(a.1[0].total_cmp(&b.1[0])))
            .map(|(k, _)| k)
        {
            points.rotate_left(k);
        }
        points
    }
}

/// Douglas-Peucker on a closed outline (the last point joins the first), with `tolerance` raised
/// until at most `max` points remain; whether it had to be raised. Where raising it would take
/// the outline below three points (a comb thinner than its teeth are long), every so many points
/// of the last result are kept instead.
pub fn simplify_within(points: &[[f64; 2]], tolerance: f64, max: usize) -> (Vec<[f64; 2]>, bool) {
    let mut tol = tolerance;
    let mut out = simplify_closed(points, tol);
    if out.len() <= max {
        return (out, false);
    }
    for _ in 0..200 {
        tol = tol.max(0.25) * 1.5;
        let next = simplify_closed(points, tol);
        if next.len() < 3 {
            break;
        }
        out = next;
        if out.len() <= max {
            return (out, true);
        }
    }
    let step = out.len().div_ceil(max.max(3));
    (out.into_iter().step_by(step.max(1)).collect(), true)
}

/// Douglas-Peucker on a closed outline: the first point and the one farthest from it are kept,
/// both halves between them are simplified, then the first point goes too if it lies within the
/// tolerance of its neighbours' line.
pub fn simplify_closed(points: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
    let n = points.len();
    let Some(&p0) = points.first() else { return Vec::new() };
    if n < 4 {
        return points.to_vec();
    }
    let at = |i: usize| points.get(i % n).copied().unwrap_or(p0);
    let far = (0..n).max_by(|a, b| dist2(at(*a), p0).total_cmp(&dist2(at(*b), p0))).unwrap_or(0);
    let mut keep = vec![false; n];
    if let Some(k) = keep.get_mut(0) {
        *k = true;
    }
    if let Some(k) = keep.get_mut(far) {
        *k = true;
    }
    let mut stack = vec![(0usize, far), (far, n)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (at(a), at(b));
        let mut best = (0.0f64, a);
        for i in a + 1..b {
            let d = segment_distance(at(i), pa, pb);
            if d > best.0 {
                best = (d, i);
            }
        }
        if best.0 > tolerance {
            if let Some(k) = keep.get_mut(best.1 % n) {
                *k = true;
            }
            stack.push((a, best.1));
            stack.push((best.1, b));
        }
    }
    let mut out: Vec<[f64; 2]> = points.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| *p).collect();
    if out.len() > 3
        && let (Some(&prev), Some(&next)) = (out.last(), out.get(1))
        && segment_distance(p0, prev, next) <= tolerance
    {
        out.remove(0);
    }
    out
}

fn dist2(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Distance from `p` to the segment from `a` to `b`.
fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    if len2 <= f64::EPSILON {
        return dist2(p, a).sqrt();
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
    dist2(p, [a[0] + t * dx, a[1] + t * dy]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` x `h` mask, 1 where `inside` says so.
    fn mask(w: u32, h: u32, inside: impl Fn(u32, u32) -> bool) -> Vec<f32> {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| f32::from(u8::from(inside(x, y))))
            .collect()
    }

    fn opts() -> Options {
        Options { min_area: 1, ..Options::default() }
    }

    fn area(points: &[[f64; 2]]) -> f64 {
        let s: f64 =
            points.iter().zip(points.iter().cycle().skip(1)).map(|(p, q)| p[0] * q[1] - q[0] * p[1]).sum();
        s / 2.0
    }

    #[test]
    fn a_rectangle_keeps_its_four_corners() {
        let m = mask(32, 24, |x, y| (4..12).contains(&x) && (2..7).contains(&y));
        let t = trace(32, 24, &m, &opts()).unwrap();
        assert_eq!(t.regions.len(), 1);
        let r = &t.regions[0];
        assert_eq!(r.points, vec![[4.0, 2.0], [12.0, 2.0], [12.0, 7.0], [4.0, 7.0]]);
        assert_eq!((r.area, r.holes), (40, 0));
        // Clockwise on screen (y down): positive area.
        assert!((area(&r.points) - 40.0).abs() < 1e-9);
        // Surface coordinates: divided by the picture's size.
        let s = to_shapes(&t.regions, 32, 24, "book");
        assert_eq!(s[0].tag, "book");
        let b = s[0].geom.bounds();
        assert!(
            b.iter().zip([0.125, 2.0 / 24.0, 0.25, 5.0 / 24.0]).all(|(a, e)| (a - e).abs() < 1e-12),
            "{b:?}"
        );
    }

    #[test]
    fn an_anti_aliased_edge_is_found_between_pixels() {
        // Columns 0..10 inside, column 10 three quarters covered, the rest out.
        let m: Vec<f32> = (0..16 * 8)
            .map(|i| match i % 16 {
                0..10 => 1.0,
                10 => 0.75,
                _ => 0.0,
            })
            .collect();
        let t = trace(16, 8, &m, &Options { threshold: 0.5, ..opts() }).unwrap();
        let right = t.regions[0].points.iter().map(|p| p[0]).fold(0.0, f64::max);
        // 0.75 at the centre 10.5, 0 at 11.5: 0.5 is crossed a third of the way.
        assert!((right - (10.5 + 1.0 / 3.0)).abs() < 1e-9, "{right}");
    }

    #[test]
    fn a_circle_stays_round_with_few_points() {
        let (c, rad) = (32.0f64, 20.0f64);
        let m = mask(64, 64, |x, y| (f64::from(x) + 0.5 - c).hypot(f64::from(y) + 0.5 - c) < rad);
        let t = trace(64, 64, &m, &opts()).unwrap();
        let p = &t.regions[0].points;
        assert!((8..=40).contains(&p.len()), "{}", p.len());
        for q in p {
            let r = (q[0] - c).hypot(q[1] - c);
            assert!((r - rad).abs() < 1.5, "{q:?} at {r}");
        }
        let a = area(p);
        assert!((a / (std::f64::consts::PI * rad * rad) - 1.0).abs() < 0.06, "{a}");
    }

    #[test]
    fn touching_blobs_are_one_region_and_corner_neighbours_two() {
        // Two squares sharing an edge.
        let m = mask(20, 10, |x, y| (2..8).contains(&y) && (2..14).contains(&x));
        assert_eq!(trace(20, 10, &m, &opts()).unwrap().regions.len(), 1);
        // Two squares touching only at a corner.
        let m = mask(20, 20, |x, y| (x < 5 && y < 5) || ((5..10).contains(&x) && (5..10).contains(&y)));
        let t = trace(20, 20, &m, &opts()).unwrap();
        assert_eq!(t.regions.len(), 2);
        assert_eq!(t.regions[1].points, vec![[5.0, 5.0], [10.0, 5.0], [10.0, 10.0], [5.0, 10.0]]);
    }

    #[test]
    fn a_hole_is_filled_and_counted() {
        // A ring: 12 x 12 with a 4 x 4 hole, and a dot inside the hole.
        let m = mask(16, 16, |x, y| {
            let ring = (2..14).contains(&x)
                && (2..14).contains(&y)
                && !((6..10).contains(&x) && (6..10).contains(&y));
            ring || ((7..9).contains(&x) && (7..9).contains(&y))
        });
        let t = trace(16, 16, &m, &opts()).unwrap();
        assert_eq!(t.regions.len(), 2);
        let ring = &t.regions[0];
        assert_eq!(ring.points, vec![[2.0, 2.0], [14.0, 2.0], [14.0, 14.0], [2.0, 14.0]]);
        assert_eq!((ring.area, ring.holes), (128, 1));
        assert_eq!(t.holes(), 1);
        // The dot in the hole comes after the ring, so it is drawn over it.
        assert_eq!(t.regions[1].area, 4);
        // Two holes in one region.
        let m = mask(12, 6, |x, y| {
            !(y == 2 && (x == 3 || x == 7)) && (1..5).contains(&y) && (1..11).contains(&x)
        });
        assert_eq!(trace(12, 6, &m, &opts()).unwrap().regions[0].holes, 2);
    }

    #[test]
    fn specks_are_dropped() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 16
        };
        let mut m = mask(64, 64, |x, y| (20..40).contains(&x) && (20..40).contains(&y));
        for _ in 0..60 {
            let (x, y) = (rnd() % 64, rnd() % 64);
            if !(18..42).contains(&x) || !(18..42).contains(&y) {
                m[(y * 64 + x) as usize] = 1.0;
            }
        }
        let t = trace(64, 64, &m, &Options { min_area: 4, ..Options::default() }).unwrap();
        assert_eq!(t.regions.len(), 1);
        assert!(t.specks > 30, "{}", t.specks);
        assert_eq!(t.regions[0].area, 400);
    }

    #[test]
    fn inverted_the_dark_parts_are_inside() {
        let m = mask(16, 9, |x, y| !((2..6).contains(&x) && (3..5).contains(&y)));
        let t = trace(16, 9, &m, &Options { invert: true, ..opts() }).unwrap();
        assert_eq!(t.regions[0].points, vec![[2.0, 3.0], [6.0, 3.0], [6.0, 5.0], [2.0, 5.0]]);
        // Not inverted: one region with that hole filled.
        let t = trace(16, 9, &m, &opts()).unwrap();
        assert_eq!((t.regions.len(), t.holes()), (1, 1));
    }

    #[test]
    fn empty_and_full_masks() {
        let t = trace(16, 9, &[0.0; 144], &opts()).unwrap();
        assert!(t.regions.is_empty());
        let t = trace(16, 9, &[1.0; 144], &opts()).unwrap();
        assert_eq!(t.regions[0].points, vec![[0.0, 0.0], [16.0, 0.0], [16.0, 9.0], [0.0, 9.0]]);
        assert!(trace(0, 0, &[], &opts()).unwrap().regions.is_empty());
    }

    #[test]
    fn labels_keep_touching_regions_apart() {
        // Background 0, a left half 0.2 and a right half 0.6 sharing an edge, a speck of 0.9.
        let m: Vec<f32> = (0..20 * 10)
            .map(|i| {
                let (x, y) = (i % 20, i / 20);
                match (x, y) {
                    (19, 9) => 0.9,
                    (_, 0 | 9) => 0.0,
                    (0..10, _) => 0.2,
                    _ => 0.6,
                }
            })
            .collect();
        let o = Options { labels: true, min_area: 2, ..Options::default() };
        let t = trace(20, 10, &m, &o).unwrap();
        assert_eq!(t.regions.len(), 2);
        assert_eq!(t.specks, 1);
        assert_eq!(t.regions[0].points, vec![[0.0, 1.0], [10.0, 1.0], [10.0, 9.0], [0.0, 9.0]]);
        assert_eq!(t.regions[1].points, vec![[10.0, 1.0], [20.0, 1.0], [20.0, 9.0], [10.0, 9.0]]);
        assert_eq!((t.regions[0].value, t.regions[1].value), (0.2, 0.6));
        // As a mask, the halves and the speck touching them are one region.
        let t = trace(20, 10, &m, &Options { min_area: 2, threshold: 0.1, ..Options::default() }).unwrap();
        assert_eq!(t.regions.len(), 1);
        // A photo is not a label picture.
        let noise: Vec<f32> = (0..300 * 300u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as f32 / 16_777_216.0 * 9.0 + 0.01)
            .collect();
        assert!(trace(300, 300, &noise, &o).unwrap_err().contains("distinct values"));
    }

    #[test]
    fn hostile_input_is_bounded() {
        assert!(trace(4, 4, &[0.0; 3], &opts()).unwrap_err().contains("values"));
        assert!(trace(1 << 16, 1 << 16, &[], &opts()).unwrap_err().contains("at most"));
        assert!(trace(2, 2, &[0.0; 4], &Options { tolerance: f64::NAN, ..opts() }).is_err());
        // NaN and infinite values do not break anything. (A tolerance of half a pixel keeps a
        // single pixel a polygon; at the default of 1 it is too thin and a speck.)
        let fine = Options { tolerance: 0.5, ..opts() };
        let m = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0];
        let t = trace(2, 2, &m, &fine).unwrap();
        assert_eq!(t.regions.len(), 1);
        // Isolated pixels: at most `max_regions`, the largest.
        let m = mask(64, 64, |x, y| (x % 2 == 0 && y % 2 == 0) || (x < 6 && y < 6));
        let t = trace(64, 64, &m, &Options { max_regions: 10, ..fine }).unwrap();
        assert_eq!(t.regions.len(), 10);
        // The block with the six dots beside it, first in reading order too.
        assert_eq!(t.regions[0].area, 42);
        assert_eq!(t.dropped, 1000);
        // A comb: thousands of corners, simplified harder to stay within MAX_POINTS.
        let m = mask(1200, 4, |x, y| y == 0 || x % 2 == 0);
        let t = trace(1200, 4, &m, &Options { tolerance: 0.0, ..opts() }).unwrap();
        assert_eq!(t.coarser, 1);
        assert!(t.regions[0].points.len() <= MAX_POINTS);
    }

    #[test]
    fn channels() {
        let px = [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 0.5]];
        assert_eq!(channel(&px, Channel::Auto).1, Channel::Alpha);
        assert_eq!(channel(&px[..1], Channel::Auto).1, Channel::Luma);
        assert_eq!(channel(&px, Channel::Red).0, vec![1.0, 0.0]);
        assert!(Channel::NAMES.iter().all(|n| Channel::parse(n).map(Channel::name) == Some(*n)));
    }
}
