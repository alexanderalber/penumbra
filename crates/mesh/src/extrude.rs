//! Solid bodies from closed 2D outlines, as `geo.text` makes lettering: a flat front and back and
//! walls between them.
//!
//! An outline may be anything a font or a hostile file holds: overlapping contours, contours that
//! cross themselves, duplicate points, spikes. [`shape`] first reduces the contours to the edge of
//! what they fill by the non-zero rule, which is how a 2D text renderer fills them. It works on an
//! integer grid: each segment is split where it meets another, and a piece stays only if one side
//! of it is filled and the other is not. What comes out are loops that do not cross, outer ones
//! counter-clockwise, holes clockwise. They are then bridged into one ring per outer loop and
//! clipped into triangles by ear clipping.
//!
//! The mesh is closed whatever the geometry is. Clipping a ring always uses each ring edge once and
//! each diagonal twice, and a ring edge is either a wall's edge or a bridge walked both ways. A
//! flawed input can give overlapping or zero-area triangles, never a gap.
//!
//! Which edges a wireframe draws comes with the triangles ([`Mesh::inner_edges`]): diagonals of
//! the caps and walls, and wall edges across the steps of a curve, are inner. The outlines of front
//! and back and the wall edges at corners sharper than [`CREASE_DEGREES`] stay.

use std::collections::HashMap;

use crate::{Builder, Error, Limits, Mesh, Vertex};

/// A point on the grid.
pub type Point = [i64; 2];

/// Most segments one shape may have. Splitting and classifying them takes time quadratic in
/// this: 8192 segments take well under a second, and a real glyph has a few hundred.
pub const MAX_SEGMENTS: usize = 8192;

/// Coordinates beyond this many grid units leave a contour out.
pub const MAX_COORD: f64 = 1e12;

/// A wall's edge at a turn sharper than this stays visible and keeps its normals apart. A curve
/// should be stepped more finely than this, or its steps show.
pub const CREASE_DEGREES: f64 = 30.0;

/// Ear tests per ring before the rest of it is clipped without looking: a hostile ring must not
/// hold a job for minutes. Far above what a glyph needs.
const MAX_EAR_TESTS: u64 = 1 << 27;

/// Rounds of splitting: a split rounded to the grid can bend a segment across a third one.
const SPLIT_ROUNDS: usize = 4;

fn get(p: &[Point], i: usize) -> Point {
    p.get(i).copied().unwrap_or([0, 0])
}

/// Twice the area of a, b, c: positive when they turn counter-clockwise.
fn cross(a: Point, b: Point, c: Point) -> i128 {
    i128::from(b[0] - a[0]) * i128::from(c[1] - a[1]) - i128::from(b[1] - a[1]) * i128::from(c[0] - a[0])
}

fn dot(a: Point, b: Point, c: Point, d: Point) -> i128 {
    i128::from(b[0] - a[0]) * i128::from(d[0] - c[0]) + i128::from(b[1] - a[1]) * i128::from(d[1] - c[1])
}

/// Twice the signed area of a loop.
fn area2(points: &[Point]) -> i128 {
    let n = points.len();
    (0..n).map(|i| cross([0, 0], get(points, i), get(points, (i + 1) % n))).sum()
}

/// Non-zero winding of a point given in doubled grid units around one loop.
fn inside_loop(points: &[Point], at: Point) -> bool {
    let n = points.len();
    let mut wind = 0i64;
    for i in 0..n {
        let (a, b) = (get(points, i).map(|c| 2 * c), get(points, (i + 1) % n).map(|c| 2 * c));
        if a[1] <= at[1] && at[1] < b[1] && cross(a, b, at) > 0 {
            wind += 1;
        } else if b[1] <= at[1] && at[1] < a[1] && cross(a, b, at) < 0 {
            wind -= 1;
        }
    }
    wind != 0
}

/// One directed segment with where it has to be split.
struct Segment {
    a: Point,
    b: Point,
    cuts: Vec<Point>,
}

fn x_range(s: &Segment) -> (i64, i64) {
    (s.a[0].min(s.b[0]), s.a[0].max(s.b[0]))
}

fn y_range(s: &Segment) -> (i64, i64) {
    (s.a[1].min(s.b[1]), s.a[1].max(s.b[1]))
}

/// Where the segments a-b and c-d have to be cut because of each other.
fn cuts(a: Point, b: Point, c: Point, d: Point) -> (Vec<Point>, Vec<Point>) {
    let on = |p: Point, s: Point, e: Point| p != s && p != e && cross(s, e, p) == 0 && dot(s, p, p, e) >= 0;
    let (ax, ay) = (i128::from(b[0] - a[0]), i128::from(b[1] - a[1]));
    let (cx, cy) = (i128::from(d[0] - c[0]), i128::from(d[1] - c[1]));
    let denominator = ax * cy - ay * cx;
    if denominator == 0 {
        if cross(a, b, c) != 0 {
            return (Vec::new(), Vec::new());
        }
        let first = [c, d].into_iter().filter(|p| on(*p, a, b)).collect();
        let second = [a, b].into_iter().filter(|p| on(*p, c, d)).collect();
        return (first, second);
    }
    let (ex, ey) = (i128::from(c[0] - a[0]), i128::from(c[1] - a[1]));
    let (t, u) = (ex * cy - ey * cx, ex * ay - ey * ax);
    let within = |v: i128| {
        if denominator > 0 { (0..=denominator).contains(&v) } else { (denominator..=0).contains(&v) }
    };
    if !within(t) || !within(u) {
        return (Vec::new(), Vec::new());
    }
    let share = t as f64 / denominator as f64;
    let p = [
        (a[0] as f64 + share * (b[0] - a[0]) as f64).round() as i64,
        (a[1] as f64 + share * (b[1] - a[1]) as f64).round() as i64,
    ];
    let first = if p != a && p != b { vec![p] } else { Vec::new() };
    let second = if p != c && p != d { vec![p] } else { Vec::new() };
    (first, second)
}

/// Splits segments where they meet, until none is left to split or the rounds run out.
fn split(mut segments: Vec<Segment>) -> Result<Vec<Segment>, Error> {
    for _ in 0..SPLIT_ROUNDS {
        segments.sort_by_key(|s| x_range(s).0);
        let mut found = false;
        // A sweep along x: two segments can meet only while their x ranges overlap.
        let mut rest = segments.as_mut_slice();
        while let Some((s, later)) = rest.split_first_mut() {
            let (x_end, ys) = (x_range(s).1, y_range(s));
            for o in later.iter_mut() {
                if x_range(o).0 > x_end {
                    break;
                }
                let yo = y_range(o);
                if yo.0 > ys.1 || yo.1 < ys.0 {
                    continue;
                }
                let (first, second) = cuts(s.a, s.b, o.a, o.b);
                found |= !first.is_empty() || !second.is_empty();
                s.cuts.extend(first);
                o.cuts.extend(second);
            }
            rest = later;
        }
        if !found {
            return Ok(segments);
        }
        let mut next = Vec::with_capacity(segments.len());
        for mut s in segments {
            let (a, b) = (s.a, s.b);
            s.cuts.sort_by_key(|p| dot(a, b, a, *p));
            s.cuts.dedup();
            let mut from = a;
            for p in s.cuts {
                if p != from {
                    next.push(Segment { a: from, b: p, cuts: Vec::new() });
                }
                from = p;
            }
            if from != b {
                next.push(Segment { a: from, b, cuts: Vec::new() });
            }
        }
        if next.len() > 4 * MAX_SEGMENTS {
            return Err(Error::new(format!("more than {} crossing segments", 4 * MAX_SEGMENTS)));
        }
        segments = next;
    }
    Ok(segments)
}

/// One undirected piece of the outlines in doubled units, `count` the outlines' net passes from a
/// to b (positive).
#[derive(Clone, Copy)]
struct Piece {
    a: Point,
    b: Point,
    count: i64,
}

/// The pieces that separate filled from empty, directed with the filled side on their left.
fn boundary(segments: &[Segment]) -> Vec<(Point, Point)> {
    let mut net: HashMap<(Point, Point), i64> = HashMap::new();
    for s in segments {
        if s.a < s.b {
            *net.entry((s.a, s.b)).or_default() += 1;
        } else if s.b < s.a {
            *net.entry((s.b, s.a)).or_default() -= 1;
        }
    }
    let mut pieces: Vec<Piece> = net
        .into_iter()
        .filter(|(_, count)| *count != 0)
        .map(|((a, b), count)| {
            let (a, b) = (a.map(|c| 2 * c), b.map(|c| 2 * c));
            if count > 0 { Piece { a, b, count } } else { Piece { a: b, b: a, count: -count } }
        })
        .collect();
    pieces.sort_by_key(|p| (p.a, p.b));
    let mut kept = Vec::new();
    for (k, piece) in pieces.iter().enumerate() {
        // A horizontal piece is judged in a picture turned by a quarter, so a ray along x leaves
        // it sideways. A turn keeps the sense of every winding.
        let turned = piece.a[1] == piece.b[1];
        let view = |p: Point| if turned { [p[1], -p[0]] } else { p };
        let (from, to) = (view(piece.a), view(piece.b));
        let at = [(from[0] + to[0]) / 2, (from[1] + to[1]) / 2];
        // Winding just past the piece on the +x side, by a ray along +x.
        let mut plus = 0i64;
        for (j, other) in pieces.iter().enumerate() {
            if j == k {
                continue;
            }
            let (u, v) = (view(other.a), view(other.b));
            if u[1] <= at[1] && at[1] < v[1] {
                if cross(u, v, at) > 0 {
                    plus += other.count;
                }
            } else if v[1] <= at[1] && at[1] < u[1] && cross(u, v, at) < 0 {
                plus -= other.count;
            }
        }
        let (low, high, up) =
            if from[1] < to[1] { (piece.a, piece.b, piece.count) } else { (piece.b, piece.a, -piece.count) };
        let minus = plus + up;
        if (plus != 0) == (minus != 0) {
            continue;
        }
        let (low, high) = (low.map(|c| c / 2), high.map(|c| c / 2));
        // Going from low to high, the -x side is on the left.
        kept.push(if minus != 0 { (low, high) } else { (high, low) });
    }
    kept
}

/// Joins directed pieces into loops, turning left wherever several go on, so that each loop goes
/// round one filled area.
fn chain(pieces: &[(Point, Point)]) -> Vec<Vec<Point>> {
    let mut leaving: HashMap<Point, Vec<Point>> = HashMap::new();
    for (a, b) in pieces {
        leaving.entry(*a).or_default().push(*b);
    }
    let mut loops = Vec::new();
    for (start, first) in pieces {
        let Some(list) = leaving.get_mut(start) else { continue };
        let Some(k) = list.iter().position(|p| p == first) else { continue };
        list.swap_remove(k);
        let mut points = vec![*start];
        let (mut previous, mut at) = (*start, *first);
        let mut closed = false;
        for _ in 0..=pieces.len() {
            if at == *start {
                closed = true;
                break;
            }
            points.push(at);
            let Some(list) = leaving.get_mut(&at) else { break };
            let back = [(previous[0] - at[0]) as f64, (previous[1] - at[1]) as f64];
            let mut best: Option<(usize, f64)> = None;
            for (k, p) in list.iter().enumerate() {
                let d = [(p[0] - at[0]) as f64, (p[1] - at[1]) as f64];
                // Clockwise from the way back: the first one found turns furthest left.
                let mut angle = (d[0] * back[1] - d[1] * back[0]).atan2(d[0] * back[0] + d[1] * back[1]);
                if angle <= 0.0 {
                    angle += std::f64::consts::TAU;
                }
                if best.is_none_or(|(_, a)| angle < a) {
                    best = Some((k, angle));
                }
            }
            let Some((k, _)) = best else { break };
            let next = list.swap_remove(k);
            previous = at;
            at = next;
        }
        // An open end is a flaw of a grid unit's size. The loop goes; the rest stays closed.
        if closed {
            loops.push(points);
        }
    }
    loops
}

/// A loop without the points in the middle of a straight run, so a straight side stays one edge.
fn straighten(points: Vec<Point>) -> Vec<Point> {
    let mut points = points;
    loop {
        let n = points.len();
        if n < 3 {
            return points;
        }
        let kept: Vec<Point> = (0..n)
            .filter(|i| {
                let (p, c, q) = (get(&points, (i + n - 1) % n), get(&points, *i), get(&points, (i + 1) % n));
                cross(p, c, q) != 0 || dot(p, c, c, q) <= 0
            })
            .map(|i| get(&points, i))
            .collect();
        if kept.len() == n {
            return points;
        }
        points = kept;
    }
}

/// The filled area of some contours: loops that do not cross, and the triangles of their caps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// Outer loops counter-clockwise, holes clockwise. Only loops that have triangles.
    pub loops: Vec<Vec<Point>>,
    rings: Vec<Ring>,
    /// Contours left out for a coordinate that is not a number or too large.
    pub unusable: usize,
}

/// One outer loop with its holes bridged in, and its triangles.
#[derive(Clone, Debug, Default, PartialEq)]
struct Ring {
    points: Vec<Point>,
    triangles: Vec<[u32; 3]>,
    /// Bit k: the edge opposite corner k is a diagonal or a bridge.
    inner: Vec<u8>,
}

impl Shape {
    pub fn is_empty(&self) -> bool {
        self.loops.is_empty()
    }

    /// Triangles of the solid this shape makes.
    pub fn triangle_count(&self) -> usize {
        let cap: usize = self.rings.iter().map(|r| r.triangles.len()).sum();
        let walls: usize = self.loops.iter().map(Vec::len).sum();
        2 * cap + 2 * walls
    }

    /// The box of all loops: min x, min y, max x, max y.
    pub fn bounds(&self) -> Option<[i64; 4]> {
        let mut points = self.loops.iter().flatten();
        let first = points.next()?;
        Some(points.fold([first[0], first[1], first[0], first[1]], |b, p| {
            [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]
        }))
    }
}

/// What the given contours fill by the non-zero rule. Contours are closed implicitly, in grid
/// units (one unit is the finest step kept). Fails if they have more than [`MAX_SEGMENTS`].
pub fn shape(contours: &[Vec<[f64; 2]>]) -> Result<Shape, Error> {
    let mut unusable = 0;
    let mut segments = Vec::new();
    for contour in contours {
        if contour.iter().flatten().any(|c| !c.is_finite() || c.abs() > MAX_COORD) {
            unusable += 1;
            continue;
        }
        let points: Vec<Point> = contour.iter().map(|p| p.map(|c| c.round() as i64)).collect();
        for (k, a) in points.iter().enumerate() {
            let b = get(&points, (k + 1) % points.len());
            if *a != b {
                segments.push(Segment { a: *a, b, cuts: Vec::new() });
            }
        }
        if segments.len() > MAX_SEGMENTS {
            return Err(Error::new(format!("more than {MAX_SEGMENTS} segments")));
        }
    }
    let segments = split(segments)?;
    let loops = chain(&boundary(&segments));
    let loops: Vec<Vec<Point>> =
        loops.into_iter().map(straighten).filter(|l| l.len() >= 3 && area2(l) != 0).collect();
    Ok(fill(loops, unusable))
}

/// Gives each hole to the smallest outer loop around it and clips one ring per outer loop.
fn fill(loops: Vec<Vec<Point>>, unusable: usize) -> Shape {
    let (outer, holes): (Vec<Vec<Point>>, Vec<Vec<Point>>) = loops.into_iter().partition(|l| area2(l) > 0);
    let mut held: Vec<Vec<Vec<Point>>> = vec![Vec::new(); outer.len()];
    for hole in holes {
        let (a, b) = (get(&hole, 0), get(&hole, 1));
        let at = [a[0] + b[0], a[1] + b[1]];
        let around = outer
            .iter()
            .enumerate()
            .filter(|(_, o)| inside_loop(o, at))
            .min_by_key(|(_, o)| area2(o))
            .map(|(k, _)| k);
        // A hole with nothing around it can only be a flaw: it goes, walls and all.
        if let Some(list) = around.and_then(|k| held.get_mut(k)) {
            list.push(hole);
        }
    }
    let mut shape = Shape { unusable, ..Shape::default() };
    for (o, list) in outer.into_iter().zip(held) {
        shape.rings.push(clip(&o, &list));
        shape.loops.push(o);
        shape.loops.extend(list);
    }
    shape
}

/// The ring with a hole spliced in at a vertex that sees the hole's rightmost point. Holes go in
/// rightmost first, so a vertex to the right of this point cannot be hidden by a later hole.
fn bridge(points: &mut Vec<Point>, walls: &mut Vec<bool>, hole: &[Point]) {
    let Some(right) = (0..hole.len()).max_by_key(|i| get(hole, *i)) else { return };
    let m = get(hole, right);
    let n = points.len();
    let cone = |i: usize| {
        let (p, v, q) = (get(points, (i + n - 1) % n), get(points, i), get(points, (i + 1) % n));
        if cross(p, v, q) >= 0 {
            cross(p, v, m) >= 0 && cross(v, q, m) >= 0
        } else {
            cross(p, v, m) >= 0 || cross(v, q, m) >= 0
        }
    };
    let clear = |i: usize| {
        let v = get(points, i);
        (0..n).all(|k| {
            let (a, b) = (get(points, k), get(points, (k + 1) % n));
            if a == v || b == v || a == m || b == m {
                return true;
            }
            let (s, t) = (cross(m, v, a), cross(m, v, b));
            let (u, w) = (cross(a, b, m), cross(a, b, v));
            let apart = (s > 0 && t > 0) || (s < 0 && t < 0) || (u > 0 && w > 0) || (u < 0 && w < 0);
            let beside = s == 0 && t == 0 && (dot(m, v, m, a) < 0 && dot(m, v, m, b) < 0);
            apart || beside
        })
    };
    let distance = |i: usize| {
        let v = get(points, i);
        i128::from(v[0] - m[0]).pow(2) + i128::from(v[1] - m[1]).pow(2)
    };
    let mut near: Vec<usize> = (0..n).filter(|i| get(points, *i)[0] >= m[0]).collect();
    near.sort_by_key(|i| distance(*i));
    // One always exists for a hole inside its loop. Should a flaw leave none, the nearest one is
    // still bridged: the triangles may overlap, the mesh stays closed.
    let at = near
        .iter()
        .copied()
        .find(|i| cone(*i) && clear(*i))
        .or_else(|| near.first().copied())
        .or_else(|| (0..n).min_by_key(|i| distance(*i)))
        .unwrap_or(0);
    let v = get(points, at);
    let around: Vec<Point> =
        hole.iter().cycle().skip(right).take(hole.len()).copied().chain([m, v]).collect();
    let flags: Vec<bool> =
        std::iter::once(false).chain(std::iter::repeat_n(true, hole.len())).chain([false]).collect();
    let (after, after_flags) = (at + 1, at);
    points.splice(after..after, around);
    walls.splice(after_flags..after_flags, flags);
}

/// Triangles of an outer loop with its holes. Every triangle comes from cutting one corner off the
/// ring. When no corner is a clean ear, one is cut anyway, so the ring always ends closed.
fn clip(outer: &[Point], holes: &[Vec<Point>]) -> Ring {
    let mut points = outer.to_vec();
    let mut walls = vec![true; points.len()];
    let mut order: Vec<&Vec<Point>> = holes.iter().collect();
    order.sort_by_key(|h| std::cmp::Reverse(h.iter().max().copied()));
    for hole in order {
        bridge(&mut points, &mut walls, hole);
    }
    let n = points.len();
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1) % n).collect();
    let mut previous: Vec<usize> = (0..n).map(|i| (i + n - 1) % n).collect();
    let step = |list: &[usize], i: usize| list.get(i).copied().unwrap_or(0);
    let is_wall = |a: usize, b: usize| (a + 1) % n == b && walls.get(a).copied().unwrap_or(false);
    let mut ring = Ring { points: Vec::new(), triangles: Vec::new(), inner: Vec::new() };
    let mut left = n;
    let mut at = 0;
    let mut looked = 0usize;
    let mut tests = 0u64;
    while left >= 3 {
        let (p, q) = (step(&previous, at), step(&next, at));
        let (a, c, b) = (get(&points, p), get(&points, at), get(&points, q));
        let area = cross(a, c, b);
        let stuck = looked >= left;
        let mut ear = area == 0 || stuck;
        if !ear && area > 0 {
            ear = true;
            if tests < MAX_EAR_TESTS {
                let mut k = step(&next, q);
                while k != p {
                    tests += 1;
                    let v = get(&points, k);
                    if v != a && v != b && v != c {
                        let reflex =
                            cross(get(&points, step(&previous, k)), v, get(&points, step(&next, k))) <= 0;
                        if reflex && cross(a, c, v) >= 0 && cross(c, b, v) >= 0 && cross(b, a, v) >= 0 {
                            ear = false;
                            break;
                        }
                    }
                    k = step(&next, k);
                }
            }
        }
        if ear && (!stuck || area >= 0 || looked >= 2 * left) {
            ring.triangles.push([p as u32, at as u32, q as u32]);
            if let Some(s) = next.get_mut(p) {
                *s = q;
            }
            if let Some(s) = previous.get_mut(q) {
                *s = p;
            }
            left -= 1;
            looked = 0;
            at = p;
        } else {
            looked += 1;
            at = q;
        }
    }
    relax(&points, &mut ring.triangles);
    // Corners a, b, c: bit 0 is the edge opposite a, which runs from b to c, and so on.
    for [a, b, c] in ring.triangles.iter().map(|t| t.map(|k| k as usize)) {
        let mut mask = 0u8;
        for (bit, (from, to)) in [(b, c), (c, a), (a, b)].into_iter().enumerate() {
            if !is_wall(from, to) {
                mask |= 1 << bit;
            }
        }
        ring.inner.push(mask);
    }
    ring.points = points;
    ring
}

/// Turns the diagonals until no pair of triangles would be rounder the other way (Delaunay flips).
/// Ear clipping leaves long fans of slivers. A wireframe's line needs its triangle's coordinates
/// to change evenly from pixel to pixel, and a sliver thinner than a pixel scatters specks of line
/// along its length.
fn relax(points: &[Point], triangles: &mut [[u32; 3]]) {
    let mut owner: HashMap<(u32, u32), usize> = HashMap::with_capacity(3 * triangles.len());
    for (i, [a, b, c]) in triangles.iter().copied().enumerate() {
        owner.extend([((a, b), i), ((b, c), i), ((c, a), i)]);
    }
    let mut todo: Vec<(u32, u32)> = owner.keys().copied().filter(|(a, b)| a < b).collect();
    todo.sort_unstable();
    let at = |k: u32| get(points, k as usize);
    let opposite = |t: [u32; 3], a: u32, b: u32| t.iter().copied().find(|k| *k != a && *k != b);
    // Angle at c in the triangle a, b, c.
    let angle = |a: Point, b: Point, c: Point| {
        let u = [(a[0] - c[0]) as f64, (a[1] - c[1]) as f64];
        let v = [(b[0] - c[0]) as f64, (b[1] - c[1]) as f64];
        (u[0] * v[1] - u[1] * v[0]).abs().atan2(u[0] * v[0] + u[1] * v[1])
    };
    let mut budget = 32 * triangles.len();
    while let Some((a, b)) = todo.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        let (Some(&s), Some(&t)) = (owner.get(&(a, b)), owner.get(&(b, a))) else { continue };
        let (Some(&ts), Some(&tt)) = (triangles.get(s), triangles.get(t)) else { continue };
        let (Some(c), Some(d)) = (opposite(ts, a, b), opposite(tt, a, b)) else { continue };
        if c == d || owner.contains_key(&(c, d)) || owner.contains_key(&(d, c)) {
            continue;
        }
        // The quad must stay convex for the turned diagonal to lie inside it.
        if cross(at(c), at(a), at(d)) <= 0 || cross(at(d), at(b), at(c)) <= 0 {
            continue;
        }
        if angle(at(a), at(b), at(c)) + angle(at(a), at(b), at(d)) <= std::f64::consts::PI + 1e-9 {
            continue;
        }
        if let Some(slot) = triangles.get_mut(s) {
            *slot = [c, a, d];
        }
        if let Some(slot) = triangles.get_mut(t) {
            *slot = [d, b, c];
        }
        owner.remove(&(a, b));
        owner.remove(&(b, a));
        owner.extend([((c, a), s), ((a, d), s), ((d, c), s), ((d, b), t), ((b, c), t), ((c, d), t)]);
        todo.extend([(c, a), (a, d), (d, b), (b, c)].map(|(u, v)| if u < v { (u, v) } else { (v, u) }));
    }
}

/// How a [`Shape`] becomes metres.
#[derive(Clone, Copy, Debug)]
pub struct Place {
    /// Metres per grid unit.
    pub scale: f64,
    /// The grid point that lands on the world's x = 0, z = 0.
    pub origin: [f64; 2],
    /// Thickness along y, in metres. The body is centred on y = 0, its front faces -y. At 0 there
    /// is a flat front only.
    pub depth: f32,
    /// The grid box a texture covers: min x, min y, max x, max y.
    pub picture: [i64; 4],
}

/// The solid and its front alone, with the same outline edges.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Solid {
    pub body: Mesh,
    pub front: Mesh,
}

impl Place {
    fn corner(&self, p: Point, y: f32, normal: [f32; 3]) -> Vertex {
        let [x0, y0, x1, y1] = self.picture.map(|c| c as f64);
        let (x, z) = (p[0] as f64, p[1] as f64);
        Vertex {
            position: [
                ((x - self.origin[0]) * self.scale) as f32,
                y,
                ((z - self.origin[1]) * self.scale) as f32,
            ],
            normal,
            uv: [((x - x0) / (x1 - x0).max(1.0)) as f32, ((y1 - z) / (y1 - y0).max(1.0)) as f32],
        }
    }
}

/// Adds a cap. `flip` for the back, which faces +y.
fn cap(b: &mut Builder<'_>, part: u32, ring: &Ring, place: &Place, y: f32, flip: bool) -> Result<(), Error> {
    let normal = [0.0, if flip { 1.0 } else { -1.0 }, 0.0];
    let first = b.vertex_count() as u32;
    b.reserve_vertices(ring.points.len())?;
    for p in &ring.points {
        b.vertex(place.corner(*p, y, normal))?;
    }
    for (t, inner) in ring.triangles.iter().zip(&ring.inner) {
        let [a, c, d] = t.map(|k| first + k);
        if flip {
            // Swaps the corners 1 and 2, and with them the bits of their opposite edges.
            b.triangle([a, d, c], part, 0)?;
            b.inner_edges((inner & 0b001) | ((inner & 0b010) << 1) | ((inner & 0b100) >> 1));
        } else {
            b.triangle([a, c, d], part, 0)?;
            b.inner_edges(*inner);
        }
    }
    Ok(())
}

/// Adds the walls of one loop: each edge a quad, normals joined across turns under the crease.
fn walls(b: &mut Builder<'_>, part: u32, points: &[Point], place: &Place) -> Result<(), Error> {
    let n = points.len();
    let half = 0.5 * place.depth;
    let outward = |i: usize| {
        let (a, c) = (get(points, i), get(points, (i + 1) % n));
        let (dx, dz) = ((c[0] - a[0]) as f64, (c[1] - a[1]) as f64);
        let len = (dx * dx + dz * dz).sqrt().max(1e-9);
        [dz / len, -dx / len]
    };
    let limit = CREASE_DEGREES.to_radians().cos();
    let smooth = |i: usize| {
        let (u, v) = (outward((i + n - 1) % n), outward(i));
        u[0] * v[0] + u[1] * v[1] >= limit
    };
    let joined = |i: usize, own: [f64; 2]| {
        if !smooth(i) {
            return own;
        }
        let (u, v) = (outward((i + n - 1) % n), outward(i));
        let (x, z) = (u[0] + v[0], u[1] + v[1]);
        let len = (x * x + z * z).sqrt().max(1e-9);
        [x / len, z / len]
    };
    for i in 0..n {
        let k = (i + 1) % n;
        let own = outward(i);
        let (na, nc) = (joined(i, own), joined(k, own));
        let at = |p: Point, m: [f64; 2], y: f32| place.corner(p, y, [m[0] as f32, 0.0, m[1] as f32]);
        let (a, c) = (get(points, i), get(points, k));
        let front_a = b.vertex(at(a, na, -half))?;
        let front_c = b.vertex(at(c, nc, -half))?;
        let back_c = b.vertex(at(c, nc, half))?;
        let back_a = b.vertex(at(a, na, half))?;
        // Seen from outside, counter-clockwise.
        b.triangle([front_a, back_c, front_c], part, 0)?;
        // Opposite corner 2 is the diagonal; opposite corner 0 is the wall edge at c.
        b.inner_edges(0b100 | if smooth(k) { 0b001 } else { 0 });
        b.triangle([front_a, back_a, back_c], part, 0)?;
        // Opposite corner 1 is the diagonal; opposite corner 2 is the wall edge at a.
        b.inner_edges(0b010 | if smooth(i) { 0b100 } else { 0 });
    }
    Ok(())
}

/// The closed mesh of `shapes`, and their front alone.
pub fn extrude(shapes: &[Shape], place: &Place, limits: &Limits) -> Result<Solid, Error> {
    let mut body = Builder::new(limits);
    let mut front = Builder::new(limits);
    let body_part = body.part("text")?;
    let front_part = front.part("front")?;
    let half = 0.5 * place.depth;
    for shape in shapes {
        for ring in &shape.rings {
            cap(&mut front, front_part, ring, place, -half, false)?;
            cap(&mut body, body_part, ring, place, -half, false)?;
            if place.depth > 0.0 {
                cap(&mut body, body_part, ring, place, half, true)?;
            }
        }
        if place.depth > 0.0 {
            for points in &shape.loops {
                walls(&mut body, body_part, points, place)?;
            }
        }
    }
    Ok(Solid { body: body.finish(), front: front.finish() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<[f64; 2]> {
        vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
    }

    fn place(depth: f32) -> Place {
        Place { scale: 0.01, origin: [0.0, 0.0], depth, picture: [0, 0, 100, 100] }
    }

    /// Each edge, by its corners' positions, is used by exactly two triangles, once each way.
    pub(crate) fn assert_closed(mesh: &Mesh) {
        let mut edges: HashMap<([u32; 3], [u32; 3]), i64> = HashMap::new();
        let mut uses: HashMap<([u32; 3], [u32; 3]), i64> = HashMap::new();
        for t in mesh.indices.as_chunks::<3>().0 {
            let p = t.map(|i| mesh.vertices[i as usize].position.map(f32::to_bits));
            for k in 0..3 {
                let (a, b) = (p[k], p[(k + 1) % 3]);
                if a == b {
                    continue;
                }
                let key = if a < b { (a, b) } else { (b, a) };
                *edges.entry(key).or_default() += if a < b { 1 } else { -1 };
                *uses.entry(key).or_default() += 1;
            }
        }
        for (k, net) in &edges {
            assert_eq!(*net, 0, "edge {k:?} is not walked both ways");
            assert_eq!(uses[k] % 2, 0, "edge {k:?}");
        }
    }

    fn solid(contours: &[Vec<[f64; 2]>], depth: f32) -> Solid {
        let s = shape(contours).unwrap();
        extrude(&[s], &place(depth), &Limits::default()).unwrap()
    }

    fn signed_volume(mesh: &Mesh) -> f64 {
        mesh.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| mesh.vertices[i as usize].position.map(f64::from));
                a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0])
            })
            .sum::<f64>()
            / 6.0
    }

    #[test]
    fn a_square_is_a_closed_box_with_outward_faces_and_its_edges() {
        let m = solid(&[square(0.0, 0.0, 100.0)], 2.0).body;
        assert_closed(&m);
        // Six faces of two triangles, the corners all sharp.
        assert_eq!(m.triangle_count(), 12);
        assert!((signed_volume(&m) - 2.0).abs() < 1e-3, "{}", signed_volume(&m));
        // Each triangle has one inner edge, its diagonal: as a box drawn with polygons.
        for mask in &m.inner_edges {
            assert_eq!(mask.count_ones(), 1, "{mask:b}");
        }
    }

    #[test]
    fn a_hole_leaves_its_area_out_either_way_the_outer_winds() {
        let hole = |mut s: Vec<[f64; 2]>| {
            s.reverse();
            s
        };
        for flip in [false, true] {
            let mut outer = square(0.0, 0.0, 100.0);
            let mut inner = hole(square(25.0, 25.0, 50.0));
            if flip {
                outer.reverse();
                inner.reverse();
            }
            let s = solid(&[outer, inner], 2.0);
            assert_closed(&s.body);
            let v = signed_volume(&s.body);
            // 1 x 1 m minus 0.5 x 0.5 m, 2 m thick.
            assert!((v - 1.5).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn two_holes_and_an_island_in_a_hole() {
        let rev = |mut s: Vec<[f64; 2]>| {
            s.reverse();
            s
        };
        let contours = [
            square(0.0, 0.0, 100.0),
            rev(square(10.0, 10.0, 30.0)),
            rev(square(55.0, 55.0, 30.0)),
            square(60.0, 60.0, 10.0),
        ];
        let s = shape(&contours).unwrap();
        assert_eq!(s.loops.len(), 4);
        assert_eq!(s.rings.len(), 2);
        let m = extrude(&[s], &place(1.0), &Limits::default()).unwrap().body;
        assert_closed(&m);
        let expected = 1.0 - 0.09 - 0.09 + 0.01;
        assert!((signed_volume(&m) - expected).abs() < 1e-3, "{}", signed_volume(&m));
    }

    #[test]
    fn overlapping_contours_are_joined_and_inner_lines_go() {
        let s = shape(&[square(0.0, 0.0, 100.0), square(50.0, 50.0, 100.0)]).unwrap();
        assert_eq!(s.loops.len(), 1, "{:?}", s.loops);
        assert_eq!(s.loops[0].len(), 8);
        let m = extrude(&[s], &place(1.0), &Limits::default()).unwrap().body;
        assert_closed(&m);
        assert!((signed_volume(&m) - 1.75).abs() < 1e-3);
    }

    #[test]
    fn the_same_contour_twice_is_one_and_a_contour_against_itself_is_none() {
        let s = shape(&[square(0.0, 0.0, 100.0), square(0.0, 0.0, 100.0)]).unwrap();
        assert_eq!(s.loops.len(), 1);
        let mut back = square(0.0, 0.0, 100.0);
        back.reverse();
        assert!(shape(&[square(0.0, 0.0, 100.0), back]).unwrap().is_empty());
    }

    #[test]
    fn a_contour_that_crosses_itself_fills_as_the_non_zero_rule_says() {
        // A bow tie: two triangles of opposite winding, both filled.
        let bow = vec![[0.0, 0.0], [100.0, 100.0], [100.0, 0.0], [0.0, 100.0]];
        let s = solid(&[bow], 1.0);
        assert_closed(&s.body);
        assert!(signed_volume(&s.body) > 0.45, "{}", signed_volume(&s.body));
        // A pentagram: the middle winds twice and stays filled, no hole.
        let star: Vec<[f64; 2]> = (0..5)
            .map(|k| {
                let a = 0.4 * std::f64::consts::TAU * f64::from(k);
                [500.0 + 400.0 * a.sin(), 500.0 + 400.0 * a.cos()]
            })
            .collect();
        let s = shape(&[star]).unwrap();
        assert_eq!(s.loops.len(), 1, "{:?}", s.loops);
        assert_eq!(s.loops[0].len(), 10);
        assert_closed(&extrude(&[s], &place(1.0), &Limits::default()).unwrap().body);
    }

    #[test]
    fn degenerate_contours_give_nothing() {
        let line = vec![[0.0, 0.0], [100.0, 0.0], [50.0, 0.0]];
        let point = vec![[5.0, 5.0], [5.0, 5.0], [5.0, 5.0]];
        let two = vec![[0.0, 0.0], [9.0, 9.0]];
        let none: Vec<[f64; 2]> = Vec::new();
        let s = shape(&[line, point, two, none]).unwrap();
        assert!(s.is_empty());
        let m = extrude(&[s], &place(1.0), &Limits::default()).unwrap();
        assert_eq!(m.body.triangle_count(), 0);
    }

    #[test]
    fn a_spike_and_repeated_points_do_not_show() {
        let s = vec![
            [0.0, 0.0],
            [0.0, 0.0],
            [100.0, 0.0],
            [300.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [0.0, 100.0],
            [0.0, 100.0],
        ];
        let s = shape(&[s]).unwrap();
        assert_eq!(s.loops.len(), 1);
        assert_eq!(s.loops[0].len(), 4, "{:?}", s.loops);
    }

    #[test]
    fn not_a_number_and_huge_coordinates_leave_the_contour_out() {
        let bad = vec![[f64::NAN, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let inf = vec![[f64::INFINITY, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let huge = vec![[1e300, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let s = shape(&[bad, inf, huge, square(0.0, 0.0, 100.0)]).unwrap();
        assert_eq!(s.unusable, 3);
        assert_eq!(s.loops.len(), 1);
    }

    #[test]
    fn too_many_segments_are_refused() {
        let many: Vec<[f64; 2]> = (0..MAX_SEGMENTS + 10)
            .map(|k| {
                let a = k as f64 * std::f64::consts::TAU / (MAX_SEGMENTS + 10) as f64;
                [1e6 * a.cos(), 1e6 * a.sin()]
            })
            .collect();
        assert!(shape(&[many]).is_err());
    }

    #[test]
    fn depth_zero_is_a_flat_front() {
        let s = solid(&[square(0.0, 0.0, 100.0)], 0.0);
        assert_eq!(s.body.triangle_count(), 2);
        assert_eq!(s.front.triangle_count(), 2);
        assert!(s.body.vertices.iter().all(|v| v.position[1] == 0.0 && v.normal[1] < 0.0));
    }

    #[test]
    fn a_curve_shows_no_steps_and_a_corner_does() {
        // A circle of 24 steps, 15 degrees each.
        let circle: Vec<[f64; 2]> = (0..24)
            .map(|k| {
                let a = f64::from(k) * std::f64::consts::TAU / 24.0;
                [1000.0 * a.cos(), 1000.0 * a.sin()]
            })
            .collect();
        let m = solid(&[circle], 1.0).body;
        assert_closed(&m);
        // Wall triangles carry two inner edges: the diagonal and the line along the depth.
        let walls = m.indices.as_chunks::<3>().0.iter().zip(&m.inner_edges).filter(|(t, _)| {
            let y = t.map(|i| m.vertices[i as usize].position[1]);
            y[0] != y[1] || y[1] != y[2]
        });
        let masks: Vec<u32> = walls.map(|(_, mask)| mask.count_ones()).collect();
        assert_eq!(masks.len(), 48);
        assert!(masks.iter().all(|n| *n == 2), "{masks:?}");
        // A square's walls keep their corner lines: one inner edge per wall triangle.
        let m = solid(&[square(0.0, 0.0, 100.0)], 1.0).body;
        assert!(m.inner_edges.iter().all(|mask| mask.count_ones() == 1));
    }

    /// The edges with a line, as (corner, corner) positions.
    fn lines(m: &Mesh) -> Vec<([f32; 3], [f32; 3])> {
        let mut out = Vec::new();
        for (t, mask) in m.indices.as_chunks::<3>().0.iter().zip(&m.inner_edges) {
            let p = t.map(|i| m.vertices[i as usize].position);
            for k in 0..3 {
                if mask & (1 << k) == 0 {
                    out.push((p[(k + 1) % 3], p[(k + 2) % 3]));
                }
            }
        }
        out
    }

    #[test]
    fn lines_run_along_the_outlines_and_at_corners_only() {
        // A square with one cut corner: straight edges, none of the lines may cross a face.
        let m = solid(&[square(0.0, 0.0, 100.0)], 1.0).body;
        for (a, b) in lines(&m) {
            let differing = (0..3).filter(|k| a[*k] != b[*k]).count();
            assert_eq!(differing, 1, "{a:?} {b:?} crosses a face");
        }
        // A circle: every line lies in the plane of the front or of the back.
        let circle: Vec<[f64; 2]> = (0..24)
            .map(|k| {
                let a = f64::from(k) * std::f64::consts::TAU / 24.0;
                [1000.0 * a.cos(), 1000.0 * a.sin()]
            })
            .collect();
        let m = solid(&[circle], 1.0).body;
        let lines = lines(&m);
        assert!(!lines.is_empty());
        for (a, b) in lines {
            assert_eq!(a[1], b[1], "{a:?} {b:?} runs along the depth");
            // The rim, not a chord across the cap: its ends are neighbours on the circle.
            let d = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(d < 3.0, "{a:?} {b:?}");
        }
    }

    #[test]
    fn walls_face_outward_and_a_flat_front_has_no_inner_lines() {
        let m = solid(&[square(0.0, 0.0, 100.0)], 1.0).body;
        for t in m.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| m.vertices[i as usize]);
            let n = crate::face_normal([a.position, b.position, c.position]);
            let dot = n[0] * a.normal[0] + n[1] * a.normal[1] + n[2] * a.normal[2];
            assert!(dot > 0.9, "{n:?} against {:?}", a.normal);
        }
    }

    /// Random polygons are as hostile as it gets: every one stays closed and nothing panics.
    #[test]
    fn random_scribbles_stay_closed() {
        let mut seed = 12345u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % 2001) as f64 - 1000.0
        };
        for round in 0..300 {
            let contours: Vec<Vec<[f64; 2]>> =
                (0..1 + round % 4).map(|_| (0..3 + round % 40).map(|_| [next(), next()]).collect()).collect();
            let s = solid(&contours, 1.0);
            assert_closed(&s.body);
        }
    }

    /// Snapped to a coarse grid, many segments lie on top of each other or just touch.
    #[test]
    fn scribbles_on_a_coarse_grid_stay_closed() {
        let mut seed = 777u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % 9) as f64
        };
        for round in 0..500 {
            let contours: Vec<Vec<[f64; 2]>> =
                (0..1 + round % 3).map(|_| (0..3 + round % 12).map(|_| [next(), next()]).collect()).collect();
            let s = solid(&contours, 1.0);
            assert_closed(&s.body);
        }
    }

    #[test]
    fn caps_cover_what_the_loops_enclose() {
        let mut seed = 99u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % 201) as f64 - 100.0
        };
        for round in 0..200 {
            let contour: Vec<[f64; 2]> = (0..4 + round % 16).map(|_| [next(), next()]).collect();
            let s = shape(&[contour]).unwrap();
            let enclosed: i128 = s.loops.iter().map(|l| area2(l)).sum();
            let covered: i128 = s
                .rings
                .iter()
                .flat_map(|r| {
                    r.triangles.iter().map(|t| {
                        cross(r.points[t[0] as usize], r.points[t[1] as usize], r.points[t[2] as usize])
                    })
                })
                .sum();
            assert_eq!(enclosed, covered, "round {round}");
            // No clipped triangle runs backwards in a clean loop.
            for r in &s.rings {
                for t in &r.triangles {
                    let c = cross(r.points[t[0] as usize], r.points[t[1] as usize], r.points[t[2] as usize]);
                    assert!(c >= 0, "round {round}");
                }
            }
        }
    }
}
