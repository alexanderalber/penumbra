//! Straight borders in a label picture (the faces of a scene in the camera's or a projector's
//! picture): where the measured border between two labels is a line, the line; where it is not
//! (a pumpkin, a tree, a ragged patch), the measured raster.
//!
//! - **Chains.** The borders run on the cracks between pixels, not through pixel centres, so a
//!   border is the same set of cracks for the labels on both sides. They are cut into chains at
//!   the corners where three labels meet (or two meet twice, a checkerboard corner); along a
//!   chain the same two labels lie on its sides, so a border two areas share is fitted once, for
//!   both, and nothing opens between them or overlaps.
//! - **Runs.** A chain is cut where it bends (the window of points a total least squares line
//!   holds within half the tolerance, the cuts then moved to where both neighbours fit best) and
//!   neighbouring pieces in nearly the same direction joined again. A run is straight when it is
//!   at least [`BorderOptions::min_length`] long, nine tenths of its crack corners lie within
//!   [`BorderOptions::tolerance`] of its line, and a parabola through their offsets bows by less
//!   than half the tolerance: a curve cut short enough to pass the first two still bows. Runs
//!   that are not straight keep the measured cracks.
//! - **Where two planes meet.** Between two faces whose planes are known (per face and view, a
//!   homography from this picture into the view), the border of two faces that touch is the line
//!   where their planes cross: the points both planes send to the same place in a view, the axis
//!   of the homology `T_G^-1 T_F`. When every view gives the same line and the measured border
//!   lies on it (its median offset within the tolerance, nine tenths within twice that: the
//!   pixels along a crease go to whichever face fits them better, so the measured border may sit
//!   to one side), that line is used instead of the fitted one: it rests on every pixel of both
//!   faces, not on the few along the border. Faces in front of another (an occluding edge),
//!   nearly coplanar ones and planes fitted too roughly to place their edge do not pass and get
//!   the fitted line. Two faces a narrow seam apart (a crease the decode left dark) are tried the
//!   same way first, the seam split between them: where their meeting line holds, the seam's
//!   pixels go to the face on their side of it ([`SEAM`]).
//! - **Corners.** Neighbouring straight runs end where their lines cross; the corners where
//!   chains meet move to where the straight lines ending there cross (along the picture's frame,
//!   where a line meets it), at most [`NODE_REACH`] tolerances away.
//! - The picture is filled again from the chains, row by row through pixel centres: a pixel takes
//!   the label between the crossings left and right of it where both agree on it, else it keeps
//!   its own (two lines crossed, a strip narrower than the tolerance). Labels at or above `fixed`
//!   (objects) are never changed and never given to other pixels: their borders stay measured.

use nalgebra::{Matrix2, Matrix3, Vector2, Vector3};

use crate::homography::Homography;
use crate::raster;

/// The label of the picture's outside, for its frame.
pub const OUTSIDE: u16 = u16::MAX;
/// Neighbouring straight runs closer than this in direction are tried as one (degrees).
const MERGE_DEG: f64 = 12.0;
/// Two lines meet in a corner only when they differ by at least this much in direction
/// (degrees); flatter, their crossing is too uncertain.
const CORNER_DEG: f64 = 20.0;
/// A corner moves at most this many tolerances from the crack corner it replaces.
pub const NODE_REACH: f64 = 3.0;
/// The cuts between runs are moved by at most this many points to where both fit best.
const REFINE: usize = 12;
/// The lines of two views agree when they lie this many tolerances apart at most, at the
/// chain's ends.
const AGREE: f64 = 2.0;
/// Most points a chain may hold for fitting (longer ones keep their raster).
const MAX_CHAIN: usize = 1 << 20;
/// Two faces a seam of unlabelled pixels apart (a crease the decode left dark, a rim) are tried
/// for their planes' meeting line when the seam is at most about twice this wide, pixels.
pub const SEAM: u32 = 6;

/// How [`straighten`] judges a border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderOptions {
    /// Pixels the measured border may lie off its line (nine tenths of its crack corners).
    pub tolerance: f64,
    /// Shortest straight run, pixels.
    pub min_length: f64,
}

impl Default for BorderOptions {
    fn default() -> Self {
        Self { tolerance: 2.0, min_length: 16.0 }
    }
}

impl BorderOptions {
    pub fn check(&self) -> Result<(), String> {
        if !self.tolerance.is_finite() || !(0.25..=16.0).contains(&self.tolerance) {
            return Err("the border `tolerance` is 0.25 to 16 pixels".into());
        }
        if !self.min_length.is_finite() || !(4.0..=4096.0).contains(&self.min_length) {
            return Err("the shortest straight border is 4 to 4096 pixels".into());
        }
        Ok(())
    }
}

/// Where a straight run's line comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Fitted to the measured border.
    Fitted,
    /// Where the two faces' planes meet.
    Meeting,
}

impl How {
    pub fn name(&self) -> &'static str {
        match self {
            How::Fitted => "fitted",
            How::Meeting => "planes",
        }
    }
}

/// A straight run of a border.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub how: How,
    /// The labels on its two sides.
    pub labels: [u16; 2],
    /// Its ends as drawn, pixels.
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// The measured crack corners it replaced, the 90th percentile of their distance from the
    /// line and how far a parabola through their offsets bows (pixels).
    pub points: usize,
    pub p90: f64,
    pub bow: f64,
}

/// What [`straighten`] did, for the reply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Chains between two labels it may change (neither fixed nor the outside).
    pub chains: usize,
    /// Of them, those with a straight run.
    pub straight_chains: usize,
    /// Their length (cracks), and how much of it became straight lines, and of that how much
    /// lies on planes' meeting lines.
    pub length: f64,
    pub straight_length: f64,
    pub meeting_length: f64,
    /// Chains between two faces where the planes' meeting line was tried, and where it held.
    pub meeting_tried: usize,
    pub meeting_held: usize,
    /// Of those tried, where no view gave a meeting line (the planes nearly one, or a fit too
    /// poor), and where the views' lines disagreed.
    pub meeting_none: usize,
    pub meeting_disagree: usize,
    /// Two faces across a seam: the pairs tried, the pairs whose meeting line held, and the seam
    /// pixels given to them.
    pub seams_tried: usize,
    pub seams_held: usize,
    pub seam_px: usize,
    /// Pixels whose label changed.
    pub changed: usize,
}

/// The picture with straight borders, the straight runs and what was done.
#[derive(Clone, Debug)]
pub struct Straightened {
    pub labels: Vec<u16>,
    pub runs: Vec<Run>,
    pub stats: Stats,
}

/// A line `n . p = c`, `n` a unit normal.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Line {
    n: [f64; 2],
    c: f64,
}

impl Line {
    fn distance(&self, p: [f64; 2]) -> f64 {
        self.n[0] * p[0] + self.n[1] * p[1] - self.c
    }
    fn project(&self, p: [f64; 2]) -> [f64; 2] {
        let d = self.distance(p);
        [p[0] - d * self.n[0], p[1] - d * self.n[1]]
    }
    /// Along the line (a unit direction).
    fn along(&self) -> [f64; 2] {
        [-self.n[1], self.n[0]]
    }
    fn from_homogeneous(l: Vector3<f64>) -> Option<Line> {
        let s = l.x.hypot(l.y);
        (s > 1e-12 && l.iter().all(|v| v.is_finite())).then(|| Line { n: [l.x / s, l.y / s], c: -l.z / s })
    }
    /// Where two lines cross, when they differ by at least [`CORNER_DEG`].
    fn cross(&self, o: &Line) -> Option<[f64; 2]> {
        let det = self.n[0] * o.n[1] - self.n[1] * o.n[0];
        if det.abs() < CORNER_DEG.to_radians().sin() {
            return None;
        }
        Some([(self.c * o.n[1] - o.c * self.n[1]) / det, (self.n[0] * o.c - o.n[0] * self.c) / det])
    }
    fn angle_to(&self, o: &Line) -> f64 {
        (self.n[0] * o.n[0] + self.n[1] * o.n[1]).abs().clamp(0.0, 1.0).acos().to_degrees()
    }
}

/// The crack graph of a label picture: edges between pixels of different labels (the outside
/// counts as [`OUTSIDE`]), corners on the (w + 1) x (h + 1) grid.
struct Grid<'a> {
    w: usize,
    h: usize,
    labels: &'a [u16],
    exists: Vec<bool>,
}

impl<'a> Grid<'a> {
    fn new(w: usize, h: usize, labels: &'a [u16]) -> Self {
        let mut g = Grid { w, h, labels, exists: Vec::new() };
        let count = w * (h + 1) + (w + 1) * h;
        g.exists = (0..count).map(|e| g.sides_raw(e).is_some_and(|(a, b)| a != b)).collect();
        g
    }
    fn at(&self, x: isize, y: isize) -> u16 {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return OUTSIDE;
        }
        self.labels.get(y as usize * self.w + x as usize).copied().unwrap_or(OUTSIDE)
    }
    fn horizontal(&self) -> usize {
        self.w * (self.h + 1)
    }
    /// The edge's position: horizontal (x, y) runs from corner (x, y) to (x + 1, y), vertical
    /// from (x, y) to (x, y + 1).
    fn place(&self, e: usize) -> (bool, isize, isize) {
        if e < self.horizontal() {
            (true, (e % self.w.max(1)) as isize, (e / self.w.max(1)) as isize)
        } else {
            let k = e - self.horizontal();
            (false, (k % (self.w + 1)) as isize, (k / (self.w + 1)) as isize)
        }
    }
    /// The labels on the + and - side when the edge runs from its first corner to its second
    /// (+ is where `d.x * q.y - d.y * q.x` is positive, `d` its direction).
    fn sides_raw(&self, e: usize) -> Option<(u16, u16)> {
        let (hor, x, y) = self.place(e);
        if hor { Some((self.at(x, y), self.at(x, y - 1))) } else { Some((self.at(x - 1, y), self.at(x, y))) }
    }
    fn ends(&self, e: usize) -> (usize, usize) {
        let (hor, x, y) = self.place(e);
        let c = |x: isize, y: isize| y as usize * (self.w + 1) + x as usize;
        if hor { (c(x, y), c(x + 1, y)) } else { (c(x, y), c(x, y + 1)) }
    }
    fn corner(&self, c: usize) -> [f64; 2] {
        [(c % (self.w + 1)) as f64, (c / (self.w + 1)) as f64]
    }
    fn exists(&self, e: usize) -> bool {
        self.exists.get(e).copied().unwrap_or(false)
    }
    /// The edges at corner `c`.
    fn incident(&self, c: usize) -> impl Iterator<Item = usize> + '_ {
        let (x, y) = (c % (self.w + 1), c / (self.w + 1));
        let hc = self.horizontal();
        [
            (x >= 1 && x - 1 < self.w).then(|| y * self.w + x - 1),
            (x < self.w).then(|| y * self.w + x),
            (y >= 1).then(|| hc + (y - 1) * (self.w + 1) + x),
            (y < self.h).then(|| hc + y * (self.w + 1) + x),
        ]
        .into_iter()
        .flatten()
        .filter(|e| self.exists(*e))
    }
}

/// A border between two labels: its crack corners in order, the labels on its + and - side,
/// whether it closes on itself.
struct Chain {
    corners: Vec<usize>,
    plus: u16,
    minus: u16,
    closed: bool,
}

/// The chains of `g`: from every corner where three labels meet, then the closed loops. Where
/// two labels meet twice (a checkerboard corner, four cracks), a chain passes through, turning.
fn chains(g: &Grid<'_>) -> Vec<Chain> {
    let corners = (g.w + 1) * (g.h + 1);
    let degree: Vec<u8> = (0..corners).map(|c| g.incident(c).count() as u8).collect();
    let checker = |c: usize| -> bool {
        let (x, y) = ((c % (g.w + 1)) as isize, (c / (g.w + 1)) as isize);
        let (a, b) = (g.at(x - 1, y - 1), g.at(x, y - 1));
        g.at(x, y) == a && g.at(x - 1, y) == b
    };
    let node = |c: usize| degree.get(c).is_some_and(|d| *d == 3 || (*d == 4 && !checker(c)));
    let mut used = vec![false; g.exists.len()];
    let mut out = Vec::new();
    let walk = |start: usize, first: usize, used: &mut Vec<bool>| -> Option<Chain> {
        let forward = g.ends(first).0 == start;
        let (p, m) = g.sides_raw(first)?;
        let (plus, minus) = if forward { (p, m) } else { (m, p) };
        let mut list = vec![start];
        let (mut cur, mut e) = (start, first);
        for _ in 0..g.exists.len() {
            if let Some(u) = used.get_mut(e) {
                *u = true;
            }
            let (a2, b2) = g.ends(e);
            let next = if a2 == cur { b2 } else { a2 };
            list.push(next);
            if node(next) || next == start {
                break;
            }
            let turn = degree.get(next) == Some(&4);
            let across = |x: usize| (x < g.horizontal()) != (e < g.horizontal());
            let Some(n) = g
                .incident(next)
                .find(|x| *x != e && !used.get(*x).copied().unwrap_or(true) && (!turn || across(*x)))
            else {
                break;
            };
            cur = next;
            e = n;
        }
        let closed = !node(start) && list.last() == Some(&start);
        Some(Chain { corners: list, plus, minus, closed })
    };
    for c in 0..corners {
        if !node(c) {
            continue;
        }
        let edges: Vec<usize> = g.incident(c).collect();
        for e in edges {
            if !used.get(e).copied().unwrap_or(true)
                && let Some(ch) = walk(c, e, &mut used)
            {
                out.push(ch);
            }
        }
    }
    for e in 0..g.exists.len() {
        if g.exists(e) && !used.get(e).copied().unwrap_or(true) {
            let (a, b) = g.ends(e);
            let start = if degree.get(a) == Some(&4) { b } else { a };
            if let Some(ch) = walk(start, e, &mut used) {
                out.push(ch);
            }
        }
    }
    out
}

/// Prefix sums of a point list's moments, for total least squares lines over any window.
struct Moments {
    origin: [f64; 2],
    s: Vec<[f64; 5]>,
}

impl Moments {
    fn new(pts: &[[f64; 2]]) -> Self {
        let origin = pts.first().copied().unwrap_or([0.0; 2]);
        let mut s = Vec::with_capacity(pts.len() + 1);
        let mut acc = [0.0; 5];
        s.push(acc);
        for p in pts {
            let (x, y) = (p[0] - origin[0], p[1] - origin[1]);
            acc[0] += x;
            acc[1] += y;
            acc[2] += x * x;
            acc[3] += x * y;
            acc[4] += y * y;
            s.push(acc);
        }
        Moments { origin, s }
    }
    /// The line through points `i..=j` and their sum of squared distances from it.
    fn fit(&self, i: usize, j: usize) -> Option<(Line, f64)> {
        if j <= i {
            return None;
        }
        let (a, b) = (self.s.get(i)?, self.s.get(j + 1)?);
        let m = (j + 1 - i) as f64;
        let d: Vec<f64> =
            (0..5).map(|k| b.get(k).copied().unwrap_or(0.0) - a.get(k).copied().unwrap_or(0.0)).collect();
        let (sx, sy, sxx, sxy, syy) = (*d.first()?, *d.get(1)?, *d.get(2)?, *d.get(3)?, *d.get(4)?);
        let (mx, my) = (sx / m, sy / m);
        let (cxx, cxy, cyy) = (sxx - sx * mx, sxy - sx * my, syy - sy * my);
        // The smaller eigenvalue of the scatter matrix and its eigenvector (the normal).
        let tr = cxx + cyy;
        let det = cxx * cyy - cxy * cxy;
        let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
        let small = tr / 2.0 - disc;
        let n = if cxy.abs() > 1e-12 {
            [cxy, small - cxx]
        } else if cxx <= cyy {
            [1.0, 0.0]
        } else {
            [0.0, 1.0]
        };
        let len = n[0].hypot(n[1]);
        if len < 1e-12 {
            return None;
        }
        let n = [n[0] / len, n[1] / len];
        let c = n[0] * (mx + self.origin[0]) + n[1] * (my + self.origin[1]);
        Some((Line { n, c }, small.max(0.0)))
    }
    fn rms(&self, i: usize, j: usize) -> f64 {
        self.fit(i, j).map_or(0.0, |(_, sse)| (sse / (j + 1 - i) as f64).sqrt())
    }
}

/// A piece of a chain, points `a..=b`: straight (with its line) or measured.
#[derive(Clone, Copy, Debug)]
struct Piece {
    a: usize,
    b: usize,
    line: Option<(Line, How, f64, f64)>,
}

/// How well `line` holds `pts`: its length along the line, the 90th percentile distance, the
/// bow of a parabola through the offsets and their median (signed).
fn judge(pts: &[[f64; 2]], line: &Line) -> (f64, f64, f64, f64) {
    if pts.len() < 2 {
        return (0.0, f64::INFINITY, f64::INFINITY, f64::INFINITY);
    }
    let d = line.along();
    let t: Vec<f64> = pts.iter().map(|p| p[0] * d[0] + p[1] * d[1]).collect();
    let r: Vec<f64> = pts.iter().map(|p| line.distance(*p)).collect();
    let (lo, hi) = t.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    let length = hi - lo;
    let mut abs: Vec<f64> = r.iter().map(|v| v.abs()).collect();
    abs.sort_by(f64::total_cmp);
    let p90 = abs.get(((abs.len() as f64) * 0.9) as usize).or(abs.last()).copied().unwrap_or(f64::INFINITY);
    // r = a + b s + c s^2 with s in -1..1 over the run; the bow is c (its sagitta).
    let mid = (lo + hi) / 2.0;
    let half = (length / 2.0).max(1e-9);
    let mut ata = Matrix3::<f64>::zeros();
    let mut atb = Vector3::<f64>::zeros();
    for (ti, ri) in t.iter().zip(&r) {
        let s = (ti - mid) / half;
        let row = Vector3::new(1.0, s, s * s);
        ata += row * row.transpose();
        atb += row * *ri;
    }
    let bow = ata.try_inverse().map_or(0.0, |inv| (inv * atb).z.abs());
    let mut signed = r;
    signed.sort_by(f64::total_cmp);
    let median = signed.get(signed.len() / 2).copied().unwrap_or(f64::INFINITY);
    (length, p90, bow, median)
}

/// Whether the fitted `line` makes `pts` a straight run: (p90, bow) when it does.
fn straight(pts: &[[f64; 2]], line: &Line, o: &BorderOptions) -> Option<(f64, f64)> {
    let (length, p90, bow, _) = judge(pts, line);
    (length >= o.min_length && p90 <= o.tolerance && bow <= o.tolerance / 2.0).then_some((p90, bow))
}

/// Whether two planes' meeting line holds `pts`: the measured border may lie off it by up to
/// the tolerance as a whole (the pixels along a crease go to whichever face fits them better, so
/// the measured border can sit to one side of it), and scatter about it by twice that.
fn meets(pts: &[[f64; 2]], line: &Line, o: &BorderOptions) -> Option<(f64, f64)> {
    let (length, p90, bow, median) = judge(pts, line);
    (length >= o.min_length && median.abs() <= o.tolerance && p90 <= 2.0 * o.tolerance).then_some((p90, bow))
}

/// A chain's points cut into straight and measured pieces. `meeting` is the planes' meeting line
/// when the chain lies between two faces.
fn pieces(pts: &[[f64; 2]], meeting: Option<&Line>, o: &BorderOptions) -> Vec<Piece> {
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    // The whole chain on the meeting line.
    if let Some(m) = meeting
        && let Some((p90, bow)) = meets(pts, m, o)
    {
        return vec![Piece { a: 0, b: n - 1, line: Some((*m, How::Meeting, p90, bow)) }];
    }
    let mo = Moments::new(pts);
    let split = o.tolerance / 2.0;
    let mut cuts = vec![0usize];
    let mut i = 0;
    while i + 1 < n {
        let mut j = i + 1;
        while j + 1 < n && mo.rms(i, j + 1) <= split {
            j += 1;
        }
        cuts.push(j);
        i = j;
    }
    // Each inner cut where both neighbours fit best.
    for k in 1..cuts.len().saturating_sub(1) {
        let (Some(a), Some(b), Some(c)) =
            (cuts.get(k - 1).copied(), cuts.get(k).copied(), cuts.get(k + 1).copied())
        else {
            continue;
        };
        let lo = (a + 1).max(b.saturating_sub(REFINE));
        let hi = c.saturating_sub(1).min(b + REFINE);
        let cost =
            |m: usize| -> f64 { mo.fit(a, m).map_or(0.0, |f| f.1) + mo.fit(m, c).map_or(0.0, |f| f.1) };
        let best = (lo..=hi).min_by(|x, y| cost(*x).total_cmp(&cost(*y)));
        if let (Some(best), Some(slot)) = (best, cuts.get_mut(k)) {
            *slot = best;
        }
    }
    let judge_piece = |a: usize, b: usize| -> Piece {
        let line = pts.get(a..=b).and_then(|seg| {
            let (l, _) = mo.fit(a, b)?;
            let (p90, bow) = straight(seg, &l, o)?;
            Some((l, How::Fitted, p90, bow))
        });
        Piece { a, b, line }
    };
    let mut list: Vec<Piece> =
        cuts.windows(2).filter_map(|w| Some(judge_piece(*w.first()?, *w.get(1)?))).collect();
    // Join neighbours: straight ones in nearly the same direction when the whole is straight,
    // measured ones always.
    let mut joined = true;
    while joined {
        joined = false;
        let mut k = 0;
        while k + 1 < list.len() {
            let (Some(p), Some(q)) = (list.get(k).copied(), list.get(k + 1).copied()) else { break };
            let merged = match (p.line, q.line) {
                (Some((lp, ..)), Some((lq, ..))) if lp.angle_to(&lq) < MERGE_DEG => {
                    let m = judge_piece(p.a, q.b);
                    m.line.is_some().then_some(m)
                }
                (None, None) => Some(Piece { a: p.a, b: q.b, line: None }),
                _ => None,
            };
            if let Some(m) = merged {
                if let Some(slot) = list.get_mut(k) {
                    *slot = m;
                }
                list.remove(k + 1);
                joined = true;
            } else {
                k += 1;
            }
        }
    }
    // A short measured piece between straight ones (the few points round a corner, a bump on
    // an edge) goes to them where they stay straight: to both as one where they run on in the
    // same direction, else split between them where both fit best. At a chain's end, the
    // straight piece next to it is carried on over it.
    let mut absorbed = true;
    while absorbed {
        absorbed = false;
        for k in 0..list.len() {
            let Some(raw) = list.get(k).copied() else { break };
            if raw.line.is_some() || (raw.b - raw.a) as f64 > 2.0 * o.min_length {
                continue;
            }
            let prev = k.checked_sub(1).and_then(|j| list.get(j)).copied().filter(|p| p.line.is_some());
            let next = list.get(k + 1).copied().filter(|p| p.line.is_some());
            let replace: Option<(usize, Vec<Piece>)> = match (prev, next) {
                (Some(p), Some(q)) => {
                    let (Some((lp, ..)), Some((lq, ..))) = (p.line, q.line) else { continue };
                    if lp.angle_to(&lq) < MERGE_DEG {
                        let m = judge_piece(p.a, q.b);
                        m.line.is_some().then(|| (k - 1, vec![m]))
                    } else {
                        let cost = |m: usize| -> f64 {
                            mo.fit(p.a, m).map_or(f64::INFINITY, |f| f.1)
                                + mo.fit(m, q.b).map_or(f64::INFINITY, |f| f.1)
                        };
                        (raw.a..=raw.b).min_by(|x, y| cost(*x).total_cmp(&cost(*y))).and_then(|cut| {
                            let (x, y) = (judge_piece(p.a, cut), judge_piece(cut, q.b));
                            (x.line.is_some() && y.line.is_some()).then(|| (k - 1, vec![x, y]))
                        })
                    }
                }
                (Some(p), None) if k + 1 == list.len() => {
                    let m = judge_piece(p.a, raw.b);
                    m.line.is_some().then(|| (k - 1, vec![m]))
                }
                (None, Some(q)) if k == 0 => {
                    let m = judge_piece(raw.a, q.b);
                    m.line.is_some().then(|| (k, vec![m]))
                }
                _ => None,
            };
            if let Some((at, new)) = replace {
                let end = (k + 2).min(list.len()).max(at + 1);
                let end = if next.is_some() { end } else { k + 1 };
                list.splice(at..end, new);
                absorbed = true;
                break;
            }
        }
    }
    // A straight piece the meeting line also holds takes that line.
    if let Some(m) = meeting {
        for p in list.iter_mut() {
            if p.line.is_some()
                && let Some(seg) = pts.get(p.a..=p.b)
                && let Some((p90, bow)) = meets(seg, m, o)
            {
                p.line = Some((*m, How::Meeting, p90, bow));
            }
        }
    }
    list
}

/// Pixel coordinates to conditioned ones (centre 0, half the width 1).
fn conditioner(w: usize, h: usize) -> Matrix3<f64> {
    let s = (w.max(h).max(1) as f64) / 2.0;
    Matrix3::new(1.0 / s, 0.0, -(w as f64) / 2.0 / s, 0.0, 1.0 / s, -(h as f64) / 2.0 / s, 0.0, 0.0, 1.0)
}

fn matrix(h: &Homography) -> Matrix3<f64> {
    let r = h.rows();
    Matrix3::new(r[0][0], r[0][1], r[0][2], r[1][0], r[1][1], r[1][2], r[2][0], r[2][1], r[2][2])
}

/// The line where two planes meet in this picture, from their maps `f` and `g` into one view:
/// the axis of the homology `g^-1 f`, the eigenvector of its transpose for the eigenvalue apart
/// from the other two (the axis of `I + k v a^T` is `a`, and `(I + k v a^T)^T a = (1 + k a.v) a`).
/// `None` when the map is no homology (the planes are one, or the fits too poor to tell).
fn meeting_line(f: &Homography, g: &Homography, cond: &Matrix3<f64>) -> Option<Line> {
    let cond_inv = cond.try_inverse()?;
    let m = cond * matrix(g).try_inverse()? * matrix(f) * cond_inv;
    let scale = m.norm();
    if !scale.is_finite() || scale < 1e-12 {
        return None;
    }
    let m = m / scale;
    let ev = m.complex_eigenvalues();
    let mut best: Option<(f64, f64, f64)> = None;
    for (i, j, k) in [(0, 1, 2), (1, 0, 2), (2, 0, 1)] {
        let (Some(a), Some(b), Some(c)) = (ev.get(i), ev.get(j), ev.get(k)) else { continue };
        let apart = (a - (b + c) / 2.0).norm();
        let pair = (b - c).norm();
        if best.is_none_or(|(d, ..)| apart > d) {
            best = Some((apart, pair, a.re));
        }
    }
    let (apart, pair, mu) = best?;
    // Two eigenvalues nearly equal and the third clearly apart from them.
    if apart < 1e-4 || pair > 0.5 * apart {
        return None;
    }
    let a = m.transpose() - Matrix3::identity() * mu;
    let svd = a.try_svd(false, true, f64::EPSILON, 200)?;
    let v_t = svd.v_t?;
    let k = svd.singular_values.iter().enumerate().min_by(|x, y| x.1.total_cmp(y.1)).map(|(k, _)| k)?;
    let l = v_t.row(k).transpose();
    Line::from_homogeneous(cond.transpose() * l)
}

/// Why two faces got no meeting line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoMeeting {
    /// No view gave one.
    None,
    /// The views' lines lie apart.
    Disagree,
}

/// The meeting line of faces `f` and `g` that every view seeing both agrees on, near the chain
/// `pts`.
fn agreed_meeting(
    f: &[Option<Homography>],
    g: &[Option<Homography>],
    pts: &[[f64; 2]],
    cond: &Matrix3<f64>,
    o: &BorderOptions,
) -> Result<Line, NoMeeting> {
    let lines: Vec<Line> =
        f.iter().zip(g).filter_map(|(a, b)| meeting_line(a.as_ref()?, b.as_ref()?, cond)).collect();
    let first = lines.first().ok_or(NoMeeting::None)?;
    let (start, end) = (pts.first().ok_or(NoMeeting::None)?, pts.last().ok_or(NoMeeting::None)?);
    let ends: Vec<[f64; 2]> = lines.iter().flat_map(|l| [l.project(*start), l.project(*end)]).collect();
    for l in &lines {
        for e in [first.project(*start), first.project(*end)] {
            if l.distance(e).abs() > AGREE * o.tolerance {
                return Err(NoMeeting::Disagree);
            }
        }
    }
    if lines.len() == 1 {
        return Ok(*first);
    }
    let mo = Moments::new(&ends);
    mo.fit(0, ends.len() - 1).map(|(l, _)| l).ok_or(NoMeeting::None)
}

/// Seams between two faces closed on the line their planes meet in: every unlabelled pixel near
/// a face (within [`SEAM`]) goes to the nearest labelled one; where two faces then touch and the
/// border between them lies on their meeting line (as [`pieces`] judges it), the seam's pixels
/// near that line go to the face on their side of it. The other seam pixels stay unlabelled.
#[allow(clippy::too_many_arguments)]
fn close_seams(
    w: usize,
    h: usize,
    labels: &[u16],
    fixed: u16,
    views: &[Vec<Option<Homography>>],
    cond: &Matrix3<f64>,
    o: &BorderOptions,
    stats: &mut Stats,
) -> Vec<u16> {
    if views.is_empty() || !labels.contains(&0) {
        return labels.to_vec();
    }
    let face = |l: u16| l > 0 && l < fixed && l != OUTSIDE;
    let source: Vec<bool> = labels.iter().map(|l| face(*l)).collect();
    let near = raster::city_distance(w, h, &source);
    let within: Vec<bool> = labels.iter().zip(&near).map(|(l, d)| *l == 0 && *d <= SEAM).collect();
    let mut grown = labels.to_vec();
    raster::grow(w, h, &mut grown, &within);
    let grid = Grid::new(w, h, &grown);
    let mut out = labels.to_vec();
    let view_of = |l: u16| usize::from(l).checked_sub(1).and_then(|k| views.get(k));
    let unlabelled = |x: usize, y: usize| x < w && y < h && labels.get(y * w + x) == Some(&0);
    for ch in chains(&grid) {
        if !face(ch.plus) || !face(ch.minus) || (ch.corners.len() as f64) <= o.min_length {
            continue;
        }
        // Only borders that run through a seam: some of their cracks have a seam pixel beside.
        let seam = ch.corners.iter().any(|c| {
            let (x, y) = (c % (w + 1), c / (w + 1));
            unlabelled(x, y)
                || unlabelled(x.wrapping_sub(1), y)
                || unlabelled(x, y.wrapping_sub(1))
                || unlabelled(x.wrapping_sub(1), y.wrapping_sub(1))
        });
        if !seam {
            continue;
        }
        let (Some(f), Some(g)) = (view_of(ch.plus), view_of(ch.minus)) else { continue };
        let pts: Vec<[f64; 2]> = ch.corners.iter().map(|c| grid.corner(*c)).collect();
        stats.seams_tried += 1;
        let Ok(m) = agreed_meeting(f, g, &pts, cond, o) else { continue };
        let mut held = false;
        for p in pieces(&pts, Some(&m), o) {
            if !p.line.is_some_and(|l| l.1 == How::Meeting) {
                continue;
            }
            let (Some(a), Some(b)) = (pts.get(p.a), pts.get(p.b)) else { continue };
            let (from, to) = (m.project(*a), m.project(*b));
            held = true;
            let reach = f64::from(SEAM) + 0.5;
            let lo = |a: f64, b: f64| (a.min(b) - reach - 1.0).max(0.0) as usize;
            let hi = |a: f64, b: f64, n: usize| ((a.max(b) + reach + 1.0).max(0.0).ceil() as usize).min(n);
            let (x0, x1, y0, y1) =
                (lo(from[0], to[0]), hi(from[0], to[0], w), lo(from[1], to[1]), hi(from[1], to[1], h));
            let d = [to[0] - from[0], to[1] - from[1]];
            let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-12);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * w + x;
                    let gl = grown.get(i).copied().unwrap_or(0);
                    if labels.get(i) != Some(&0) || (gl != ch.plus && gl != ch.minus) {
                        continue;
                    }
                    let q = [x as f64 + 0.5 - from[0], y as f64 + 0.5 - from[1]];
                    let t = ((q[0] * d[0] + q[1] * d[1]) / len2).clamp(0.0, 1.0);
                    if (q[0] - t * d[0]).hypot(q[1] - t * d[1]) > reach {
                        continue;
                    }
                    let side = d[0] * q[1] - d[1] * q[0];
                    if let Some(slot) = out.get_mut(i) {
                        *slot = if side > 0.0 { ch.plus } else { ch.minus };
                        stats.seam_px += 1;
                    }
                }
            }
        }
        if held {
            stats.seams_held += 1;
        }
    }
    out
}

/// Least squares point of `lines` (weighted), when they cross clearly.
fn crossing(lines: &[(Line, f64)]) -> Option<[f64; 2]> {
    let mut a = Matrix2::<f64>::zeros();
    let mut b = Vector2::<f64>::zeros();
    for (l, w) in lines {
        let n = Vector2::new(l.n[0], l.n[1]);
        a += n * n.transpose() * *w;
        b += n * (l.c * *w);
    }
    let total: f64 = lines.iter().map(|(_, w)| *w).sum();
    if total <= 0.0 {
        return None;
    }
    let e = (a / total).symmetric_eigenvalues();
    let small = e.iter().copied().fold(f64::INFINITY, f64::min);
    // Lines differing by CORNER_DEG give a smaller eigenvalue of about sin^2 / 4 (two lines).
    if small < (CORNER_DEG.to_radians().sin().powi(2)) / 4.0 {
        return None;
    }
    let p = a.try_inverse()? * b;
    Some([p.x, p.y])
}

/// `labels` (`w` x `h`) with straight borders where they fit. Labels from `fixed` on (objects)
/// keep every pixel and their borders; so do borders along the picture's frame. `views` holds per
/// face (label `k` at index `k - 1`) and view the face's homography from this picture into that
/// view, for the lines where two faces' planes meet (`None` where a view does not see it).
pub fn straighten(
    w: usize,
    h: usize,
    labels: &[u16],
    fixed: u16,
    views: &[Vec<Option<Homography>>],
    o: &BorderOptions,
) -> Straightened {
    let unchanged = || Straightened { labels: labels.to_vec(), runs: Vec::new(), stats: Stats::default() };
    if w == 0 || h == 0 || labels.len() != w * h || o.check().is_err() {
        return unchanged();
    }
    let original = labels;
    let cond = conditioner(w, h);
    let mut stats = Stats::default();
    let closed = close_seams(w, h, labels, fixed, views, &cond, o, &mut stats);
    let labels = closed.as_slice();
    let grid = Grid::new(w, h, labels);
    let list = chains(&grid);
    let movable = |l: u16| l != OUTSIDE && l < fixed;

    // Per chain its points (closed ones start at their point farthest from their middle, a
    // likely corner) and its pieces.
    struct Fitted {
        pts: Vec<[f64; 2]>,
        ends: [usize; 2],
        plus: u16,
        minus: u16,
        pieces: Vec<Piece>,
    }
    let mut fitted: Vec<Fitted> = Vec::with_capacity(list.len());
    for ch in &list {
        let mut corners = ch.corners.clone();
        if ch.closed && corners.len() > 2 {
            corners.pop();
            let pts: Vec<[f64; 2]> = corners.iter().map(|c| grid.corner(*c)).collect();
            let m = pts.len() as f64;
            let mid = [pts.iter().map(|p| p[0]).sum::<f64>() / m, pts.iter().map(|p| p[1]).sum::<f64>() / m];
            let far = pts
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    let d = |p: &[f64; 2]| (p[0] - mid[0]).hypot(p[1] - mid[1]);
                    d(a.1).total_cmp(&d(b.1))
                })
                .map_or(0, |(k, _)| k);
            corners.rotate_left(far);
            if let Some(first) = corners.first().copied() {
                corners.push(first);
            }
        }
        let pts: Vec<[f64; 2]> = corners.iter().map(|c| grid.corner(*c)).collect();
        let ends = [corners.first().copied().unwrap_or(0), corners.last().copied().unwrap_or(0)];
        let eligible = movable(ch.plus) && movable(ch.minus) && pts.len() <= MAX_CHAIN;
        let mut list_pieces = Vec::new();
        if eligible {
            stats.chains += 1;
            stats.length += (pts.len() - 1) as f64;
            let face = |l: u16| usize::from(l).checked_sub(1).and_then(|k| views.get(k));
            let meeting = match (face(ch.plus), face(ch.minus)) {
                (Some(f), Some(g)) if ch.plus > 0 && ch.minus > 0 && (pts.len() as f64) > o.min_length => {
                    stats.meeting_tried += 1;
                    match agreed_meeting(f, g, &pts, &cond, o) {
                        Ok(m) => Some(m),
                        Err(NoMeeting::None) => {
                            stats.meeting_none += 1;
                            None
                        }
                        Err(NoMeeting::Disagree) => {
                            stats.meeting_disagree += 1;
                            None
                        }
                    }
                }
                _ => None,
            };
            if (pts.len() as f64) > o.min_length {
                list_pieces = pieces(&pts, meeting.as_ref(), o);
            }
            if list_pieces.iter().any(|p| p.line.is_some_and(|l| l.1 == How::Meeting)) {
                stats.meeting_held += 1;
            }
            if list_pieces.iter().any(|p| p.line.is_some()) {
                stats.straight_chains += 1;
            }
        }
        if list_pieces.is_empty() && pts.len() >= 2 {
            list_pieces.push(Piece { a: 0, b: pts.len() - 1, line: None });
        }
        fitted.push(Fitted { pts, ends, plus: ch.plus, minus: ch.minus, pieces: list_pieces });
    }

    // The corners where chains meet: moved to where the straight lines ending there cross.
    let mut at_node: std::collections::HashMap<usize, Vec<(Line, f64)>> = std::collections::HashMap::new();
    for f in &fitted {
        let (Some(first), Some(last)) = (f.pieces.first(), f.pieces.last()) else { continue };
        for (piece, node) in [(first, f.ends[0]), (last, f.ends[1])] {
            if let Some((l, ..)) = piece.line {
                let len = (piece.b - piece.a) as f64;
                at_node.entry(node).or_default().push((l, len));
            }
        }
    }
    let reach = NODE_REACH * o.tolerance;
    let mut node_at: std::collections::HashMap<usize, [f64; 2]> = std::collections::HashMap::new();
    for (node, lines) in &at_node {
        let p = grid.corner(*node);
        let (x, y) = (p[0], p[1]);
        let on_frame = [x == 0.0, x == w as f64, y == 0.0, y == h as f64];
        let frame_lines: Vec<Line> = on_frame
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(k, _)| match k {
                0 => Line { n: [1.0, 0.0], c: 0.0 },
                1 => Line { n: [1.0, 0.0], c: w as f64 },
                2 => Line { n: [0.0, 1.0], c: 0.0 },
                _ => Line { n: [0.0, 1.0], c: h as f64 },
            })
            .collect();
        let heaviest = lines.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(l, _)| *l);
        let moved = match frame_lines.len() {
            0 => crossing(lines).or_else(|| heaviest.map(|l| l.project(p))),
            1 => heaviest.and_then(|l| frame_lines.first().and_then(|fl| l.cross(fl))),
            _ => None,
        };
        if let Some(q) = moved
            && q.iter().all(|v| v.is_finite())
            && (q[0] - x).hypot(q[1] - y) <= reach
        {
            node_at.insert(*node, q);
        }
    }

    // The chains as drawn.
    let mut runs = Vec::new();
    let mut polylines: Vec<(Vec<[f64; 2]>, u16, u16)> = Vec::with_capacity(fitted.len());
    for f in &fitted {
        let node_pos = |c: usize, fallback: [f64; 2]| node_at.get(&c).copied().unwrap_or(fallback);
        let start = node_pos(f.ends[0], f.pts.first().copied().unwrap_or([0.0; 2]));
        let end = node_pos(f.ends[1], f.pts.last().copied().unwrap_or([0.0; 2]));
        let mut out = vec![start];
        let count = f.pieces.len();
        for (k, piece) in f.pieces.iter().enumerate() {
            let line = piece.line.map(|l| l.0);
            // A straight piece runs on its line from end to end: where the vertex before it lies
            // off the line (a chain's end the corner did not move to), it is joined to the line
            // first.
            let mut from = out.last().copied().unwrap_or(start);
            if let Some(l) = line
                && l.distance(from).abs() > 1e-6
            {
                from = l.project(from);
                out.push(from);
            }
            if piece.line.is_none()
                && let Some(inner) = f.pts.get(piece.a + 1..piece.b)
            {
                out.extend_from_slice(inner);
            }
            // Where this piece ends: the chain's end, the crossing with the next straight piece,
            // or the measured point projected onto whichever of the two is straight.
            let to = if k + 1 == count {
                end
            } else {
                let p = f.pts.get(piece.b).copied().unwrap_or(end);
                let next = f.pieces.get(k + 1).and_then(|n| n.line);
                match (piece.line, next) {
                    (Some((a, ..)), Some((b, ..))) => a
                        .cross(&b)
                        .filter(|q| (q[0] - p[0]).hypot(q[1] - p[1]) <= reach)
                        .unwrap_or_else(|| a.project(p)),
                    (Some((a, ..)), None) | (None, Some((a, ..))) => a.project(p),
                    (None, None) => p,
                }
            };
            let line_end = match line {
                Some(l) if l.distance(to).abs() > 1e-6 => l.project(to),
                _ => to,
            };
            if line_end != to {
                out.push(line_end);
            }
            out.push(to);
            if let Some((_, how, p90, bow)) = piece.line {
                stats.straight_length += (piece.b - piece.a) as f64;
                if how == How::Meeting {
                    stats.meeting_length += (piece.b - piece.a) as f64;
                }
                runs.push(Run {
                    how,
                    labels: [f.plus, f.minus],
                    from,
                    to: line_end,
                    points: piece.b - piece.a + 1,
                    p90,
                    bow,
                });
            }
        }
        polylines.push((out, f.plus, f.minus));
    }

    // Filled again row by row: a pixel takes the label right of the last crossing left of its
    // centre, when that is also the label left of the next crossing. Where it is not (two lines
    // crossed each other, a strip narrower than the tolerance), the pixel keeps its label.
    let mut rows: Vec<Vec<(f64, u16, u16)>> = vec![Vec::new(); h];
    for (pl, plus, minus) in &polylines {
        for seg in pl.windows(2) {
            let (Some(p0), Some(p1)) = (seg.first(), seg.get(1)) else { continue };
            if p0[1] == p1[1] || !p0.iter().chain(p1.iter()).all(|v| v.is_finite()) {
                continue;
            }
            let (lo, hi) = (p0[1].min(p1[1]), p0[1].max(p1[1]));
            let first = (lo - 0.5).ceil().max(0.0) as usize;
            let (left, right) = if p1[1] < p0[1] { (*minus, *plus) } else { (*plus, *minus) };
            let mut r = first;
            while r < h && (r as f64 + 0.5) < hi {
                let yc = r as f64 + 0.5;
                if yc >= lo {
                    let x = p0[0] + (yc - p0[1]) * (p1[0] - p0[0]) / (p1[1] - p0[1]);
                    if let Some(row) = rows.get_mut(r) {
                        row.push((x, left, right));
                    }
                }
                r += 1;
            }
        }
    }
    let mut out = labels.to_vec();
    for (y, row) in rows.iter_mut().enumerate() {
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut k = 0;
        let mut current = OUTSIDE;
        for x in 0..w {
            let xc = x as f64 + 0.5;
            while let Some((cx, _, r)) = row.get(k) {
                if *cx > xc {
                    break;
                }
                current = *r;
                k += 1;
            }
            let consistent = row.get(k).is_none_or(|(_, l, _)| *l == current);
            let i = y * w + x;
            let (Some(slot), Some(was)) = (out.get_mut(i), labels.get(i)) else { continue };
            // Objects keep their pixels and take no others.
            *slot = if *was >= fixed || current >= fixed || !consistent { *was } else { current };
        }
    }
    stats.changed = out.iter().zip(original).filter(|(a, b)| a != b).count();
    Straightened { labels: out, runs, stats }
}
