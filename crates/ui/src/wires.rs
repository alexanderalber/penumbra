//! Wire routing for the node editor, in graph units: a wire is a plain curve unless that clips a
//! node, then it runs through a free horizontal corridor; wires from different outputs keep
//! apart from each other. Also the measures `ui.measure` reports for a level.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Stroke, pos2, vec2};
use penumbra_core::NodeId;

/// The path of a wire: cubic segments, each given by its four control points.
pub(crate) type Route = Vec<[Pos2; 4]>;

/// Control points of a wire from output `a` to input `b`, blind to the nodes around it. A wire
/// running backwards (the end of a feedback loop into its start) swings below.
pub(crate) fn wire_points(a: Pos2, b: Pos2, zoom: f32) -> [Pos2; 4] {
    if b.x < a.x - 20.0 * zoom {
        let out = 80.0 * zoom;
        let down = 140.0 * zoom + (a.x - b.x) * 0.08;
        return [a, a + vec2(out, down), b + vec2(-out, down), b];
    }
    let dx = ((b.x - a.x).abs() * 0.5).max(30.0 * zoom);
    [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b]
}

/// The route of a wire from output `a` of node `from` to input `b` of node `to`, on its own:
/// the cheapest of [`candidates`] by [`route_cost`].
pub(crate) fn wire_route(a: Pos2, b: Pos2, from: Rect, to: Rect, nodes: &[Rect], zoom: f32) -> Route {
    candidates(a, b, from, to, nodes, zoom, false)
        .into_iter()
        .map(|(r, weight)| (route_cost(&r, nodes, zoom) * weight, r))
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map_or_else(|| vec![wire_points(a, b, zoom)], |(_, r)| r)
}

/// Ways a wire from output `a` of node `from` to input `b` of node `to` may run, each with a
/// weight its cost is multiplied by (the calm ones are cheaper).
///
/// A forward wire is the plain curve when that clips no node; otherwise it may also bend into a
/// free horizontal corridor between the nodes in its way, run along it and bend out to its
/// input. Corridors lie just above and below each node between the two ends, nearest first, and
/// with `more` also further out in tracks, and the plain curve comes with longer and shorter
/// tangents: room for wires to keep off each other.
///
/// A backward wire can swing below both nodes, above them, or snake through the gap between them
/// when one sits above the other, each as one curve or as a wider loop around both.
fn candidates(
    a: Pos2,
    b: Pos2,
    from: Rect,
    to: Rect,
    nodes: &[Rect],
    zoom: f32,
    more: bool,
) -> Vec<(Route, f32)> {
    if b.x >= a.x - 20.0 * zoom {
        return forward_candidates(a, b, nodes, zoom, more);
    }
    let out = 80.0 * zoom;
    let margin = 30.0 * zoom;
    let below = from.max.y.max(to.max.y) + margin;
    let above = from.min.y.min(to.min.y) - margin;
    let right = from.max.x.max(to.max.x) + margin;
    let left = from.min.x.min(to.min.x) - margin;
    // Each single curve also comes swinging out wider, to clear a corner it would graze.
    let wides = [1.0, 1.25, 1.5];
    // One curve, both inner control points at one height so that its middle lands at `y`.
    let swing = |y: f32, wide: f32| {
        let c = (y - (a.y + b.y) / 8.0) / 0.75;
        vec![[a, pos2(a.x + out * wide, c), pos2(b.x - out * wide, c), b]]
    };
    // The loops are wider and less calm than the single curves: chosen when those clip.
    let mut list = Vec::new();
    let shifts: &[f32] = if more { &[0.0, 1.0, -1.0, 2.0] } else { &[0.0] };
    for &k in shifts {
        let d = k * TRACK * zoom;
        list.push((around(a, b, right + d.abs(), below + d.abs(), left - d.abs(), margin), 1.3));
        list.push((around(a, b, right + d.abs(), above - d.abs(), left - d.abs(), margin), 1.3));
    }
    for wide in wides {
        list.push((swing(below, wide), 1.0));
        list.push((swing(above, wide), 1.0));
    }
    let (upper, lower) = if to.center().y < from.center().y { (to, from) } else { (from, to) };
    if lower.min.y - upper.max.y > 10.0 * zoom {
        // One S crossing midway between the ports, or a loop through the middle of the gap.
        for wide in wides {
            let k = (out * 1.5).max((a.x - b.x) * 0.75) * wide;
            list.push((vec![[a, a + vec2(k, 0.0), b - vec2(k, 0.0), b]], 1.0));
        }
        let middle = (upper.max.y + lower.min.y) / 2.0;
        for &k in shifts {
            list.push((around(a, b, right, middle + k * TRACK * zoom, left, margin), 1.3));
        }
    }
    list
}

/// A wire level out of `a`, down or up at `x`, level into `b`, with rounded corners.
fn elbow(a: Pos2, b: Pos2, x: f32, zoom: f32) -> Route {
    let radius = (14.0 * zoom).min(x - a.x).min(b.x - x).max(0.0);
    rounded(&[a, pos2(x, a.y), pos2(x, b.y), b], radius)
}

/// Lanes for steep wires fanning into one node from the same side (all from above or all from
/// below), at zoom 1: the shortest runs furthest from the node, the longest nearest, so none
/// crosses another, and together they sit in the middle of the gap. `None` for other wires.
fn fan_lanes(wires: &[Ends]) -> Vec<Option<f32>> {
    let steep = |w: &Ends| {
        let dx = w.b.x - w.a.x;
        dx >= 30.0 && (w.b.y - w.a.y).abs() > dx
    };
    let mut groups: HashMap<(u32, u32, bool), Vec<usize>> = HashMap::new();
    for (i, w) in wires.iter().enumerate().filter(|(_, w)| steep(w)) {
        groups.entry((w.to.min.x.to_bits(), w.to.min.y.to_bits(), w.a.y < w.b.y)).or_default().push(i);
    }
    let mut lanes = vec![None; wires.len()];
    for group in groups.values().filter(|g| g.len() >= 2) {
        let mut group = group.clone();
        let span = |i: usize| wires.get(i).map_or(0.0, |w| (w.b.y - w.a.y).abs());
        group.sort_by(|x, y| span(*x).total_cmp(&span(*y)).then(x.cmp(y)));
        let gap =
            group.iter().filter_map(|i| wires.get(*i)).map(|w| w.b.x - w.a.x).fold(f32::INFINITY, f32::min);
        let n = group.len() as f32;
        let step = TRACK.min((gap - 24.0) / (n - 1.0));
        if step < 3.0 {
            continue;
        }
        for (r, &i) in group.iter().enumerate() {
            let Some(w) = wires.get(i) else { continue };
            let x = w.b.x - gap / 2.0 + (r as f32 - (n - 1.0) / 2.0) * step;
            put_lane(&mut lanes, i, x.clamp(w.a.x + 10.0, w.b.x - 14.0));
        }
    }
    lanes
}

fn put_lane(lanes: &mut [Option<f32>], i: usize, x: f32) {
    if let Some(slot) = lanes.get_mut(i) {
        *slot = Some(x);
    }
}

/// Space between neighbouring tracks of wires running side by side, at zoom 1.
const TRACK: f32 = 8.0;

fn forward_candidates(a: Pos2, b: Pos2, nodes: &[Rect], zoom: f32, more: bool) -> Vec<(Route, f32)> {
    let plain = wire_points(a, b, zoom);
    let mut list = vec![(vec![plain], 1.0)];
    if more {
        // Longer or shorter tangents: the curve's middle moves, its ends stay level.
        let dx = (b.x - a.x).abs();
        for (share, weight) in [(0.35, 1.02), (0.68, 1.02), (0.22, 1.05), (0.85, 1.05)] {
            let d = (dx * share).max(30.0 * zoom);
            list.push((vec![[a, a + vec2(d, 0.0), b - vec2(d, 0.0), b]], weight));
        }
    }
    let (dx, dy) = (b.x - a.x, (b.y - a.y).abs());
    if more && dy > dx && dx >= 30.0 * zoom {
        // A steep wire may also run as an elbow: level out of its output, down or up at one of
        // a few places across the gap, level into its input. Wires fanning into one node get
        // a lane each that way.
        let (first, last, step) = (a.x + 10.0 * zoom, b.x - 14.0 * zoom, TRACK * zoom);
        let lanes = ((last - first) / step).floor().clamp(0.0, 12.0) as usize;
        for j in 0..=lanes {
            let x = first + j as f32 * step;
            let off = (x - (a.x + dx / 2.0)).abs() / zoom;
            list.push((elbow(a, b, x, zoom), 1.04 + 0.002 * off));
        }
    }
    // The bends need room: a wire to the neighbouring column stays a plain curve.
    let bend = 60.0 * zoom;
    if b.x - a.x < bend * 3.0 {
        return list;
    }
    if !more && route_cost(&[plain], nodes, zoom) <= route_cost(&[plain], &[], zoom) * 1.001 {
        return list;
    }
    let span =
        Rect::from_min_max(pos2(a.x + bend * 0.5, f32::NEG_INFINITY), pos2(b.x - bend * 0.5, f32::INFINITY));
    let middle = (a.y + b.y) / 2.0;
    let margin = 12.0 * zoom;
    let tracks = if more { 4 } else { 1 };
    let mut heights: Vec<f32> = nodes
        .iter()
        .filter(|r| r.min.x < span.max.x && r.max.x > span.min.x)
        .flat_map(|r| {
            (0..tracks).flat_map(move |k| {
                let d = margin + k as f32 * TRACK * zoom;
                [r.min.y - d, r.max.y + d]
            })
        })
        .collect();
    heights.sort_by(|x, y| (x - middle).abs().total_cmp(&(y - middle).abs()));
    heights.dedup_by(|x, y| (*x - *y).abs() < 1.0);
    heights.truncate(if more { 40 } else { 16 });
    // An S from each end into the corridor and a straight run along it.
    let along = |y: f32| -> Route {
        let (p, q) = (pos2(a.x + bend, y), pos2(b.x - bend, y));
        let h = bend * 0.5;
        vec![
            [a, a + vec2(h, 0.0), p - vec2(h, 0.0), p],
            [p, p.lerp(q, 1.0 / 3.0), p.lerp(q, 2.0 / 3.0), q],
            [q, q + vec2(h, 0.0), b - vec2(h, 0.0), b],
        ]
    };
    // A corridor route is less calm than the plain curve: it has to be clearly better.
    list.extend(heights.into_iter().map(|y| (along(y), 1.15)));
    list
}

/// What running on top of a wire from another output costs, per unit of length: more than a
/// detour of the same length, much less than clipping a node.
const OVERLAP_COST: f32 = 25.0;
/// What crossing a wire from another output costs, as length.
const CROSSING_COST: f32 = 30.0;
/// Closer than this, two wires count as running on top of each other (at zoom 1).
const APART: f32 = 4.5;

/// A short piece of a routed wire and the output the wire comes from.
type Segment = (Pos2, Pos2, (NodeId, u32));

/// The wires routed so far, as short segments by grid cell, with the output each comes from.
#[derive(Default)]
struct Taken {
    cells: HashMap<(i32, i32), Vec<Segment>>,
}

impl Taken {
    const CELL: f32 = 12.0;

    fn cell(p: Pos2) -> (i32, i32) {
        ((p.x / Self::CELL).floor() as i32, (p.y / Self::CELL).floor() as i32)
    }

    fn add(&mut self, line: &[Pos2], source: (NodeId, u32)) {
        for (p, q) in pieces(line) {
            let (c0, c1) = (Self::cell(p), Self::cell(q));
            for x in c0.0.min(c1.0)..=c0.0.max(c1.0) {
                for y in c0.1.min(c1.1)..=c0.1.max(c1.1) {
                    self.cells.entry((x, y)).or_default().push((p, q, source));
                }
            }
        }
    }

    /// Length of `line` running on top of wires from other outputs, how often it crosses them,
    /// and how many pieces that looked at.
    fn conflicts(&self, line: &[Pos2], source: (NodeId, u32)) -> (f32, usize, usize) {
        let mut overlap = 0.0;
        let mut crossed: Vec<Pos2> = Vec::new();
        let pieces = pieces(line);
        let count = pieces.len();
        for (p, q) in pieces {
            let (cx, cy) = Self::cell(p.lerp(q, 0.5));
            let near = (-1..=1)
                .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
                .filter_map(|k| self.cells.get(&k))
                .flatten()
                .filter(|s| s.2 != source);
            let mut close = false;
            for &(c, d, _) in near {
                close |= segment_distance(p.lerp(q, 0.5), c, d) < APART && parallel(q - p, d - c);
                if segments_cross(p, q, c, d) && crossed.iter().all(|x| x.distance(p) > 8.0) {
                    crossed.push(p);
                }
            }
            if close {
                overlap += p.distance(q);
            }
        }
        (overlap, crossed.len(), count)
    }
}

/// A line cut into pieces no longer than about a third of a grid cell.
fn pieces(line: &[Pos2]) -> Vec<(Pos2, Pos2)> {
    let mut out = Vec::new();
    for w in line.windows(2) {
        let &[p, q] = w else { continue };
        let n = (p.distance(q) / 4.0).ceil().max(1.0);
        let mut at = p;
        for i in 1..=n as usize {
            let next = p.lerp(q, i as f32 / n);
            out.push((at, next));
            at = next;
        }
    }
    out
}

/// Routes for all `wires` among the `nodes`, in graph units. Each wire takes the cheapest of its
/// candidate routes, counting what it would cost to run on top of or across the wires routed
/// before it; wires from one output may share their way. The wires whose plain curve is free go
/// first (they have the least choice), then the others from the longest down.
pub(crate) fn route_all(wires: &[Ends], nodes: &[Rect]) -> Vec<Route> {
    let mut first: Vec<(usize, bool, f32)> = wires
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let plain = [wire_points(w.a, w.b, 1.0)];
            let free = w.b.x >= w.a.x - 20.0
                && route_cost(&plain, nodes, 1.0) <= route_cost(&plain, &[], 1.0) * 1.001;
            (i, free, w.a.distance(w.b))
        })
        .collect();
    first.sort_by(|x, y| y.1.cmp(&x.1).then(y.2.total_cmp(&x.2)).then(x.0.cmp(&y.0)));
    let mut routes: Vec<Route> = wires.iter().map(|w| vec![wire_points(w.a, w.b, 1.0)]).collect();
    let mut taken = Taken::default();
    let lanes = fan_lanes(wires);
    let mut work = 0usize;
    for (i, _, _) in first {
        let Some(w) = wires.get(i) else { continue };
        if work > WORK {
            // A huge level: the rest are routed on their own, so the editor stays responsive.
            if let Some(slot) = routes.get_mut(i) {
                *slot = wire_route(w.a, w.b, w.from, w.to, nodes, 1.0);
            }
            continue;
        }
        let mut options = candidates(w.a, w.b, w.from, w.to, nodes, 1.0, true);
        if let Some(x) = lanes.get(i).copied().flatten() {
            options.push((elbow(w.a, w.b, x, 1.0), 0.9));
        }
        // Only the nodes any of them could touch.
        let reach = options
            .iter()
            .flat_map(|(r, _)| r.iter().flatten())
            .fold(Rect::NOTHING, |acc, p| acc.union(Rect::from_min_max(*p, *p)))
            .expand(20.0);
        let near: Vec<Rect> = nodes.iter().copied().filter(|r| r.intersects(reach)).collect();
        work += options.len() * (near.len() + 1) * 30;
        let mut options: Vec<(f32, Route)> =
            options.into_iter().map(|(r, weight)| (route_cost(&r, &near, 1.0) * weight, r)).collect();
        options.sort_by(|x, y| x.0.total_cmp(&y.0));
        // Cheapest first; conflicts only add, so once a route costs more on its own than the
        // best so far with its conflicts, none after it can win.
        let mut best: Option<(f32, Route, Vec<Pos2>)> = None;
        for (cost, route) in options {
            if best.as_ref().is_some_and(|b| cost >= b.0) {
                break;
            }
            let line = flatten(&route);
            let (overlap, crossings, pieces) = taken.conflicts(&line, w.source);
            work += pieces;
            let total = cost + overlap * OVERLAP_COST + crossings as f32 * CROSSING_COST;
            if best.as_ref().is_none_or(|b| total < b.0) {
                best = Some((total, route, line));
            }
        }
        if let Some((_, route, line)) = best {
            taken.add(&line, w.source);
            if let Some(slot) = routes.get_mut(i) {
                *slot = route;
            }
        }
    }
    routes
}

/// Routes for all `wires`, each on its own as if the others were not there: quicker than
/// [`route_all`], for while nodes are being dragged.
pub(crate) fn route_each(wires: &[Ends], nodes: &[Rect]) -> Vec<Route> {
    wires.iter().map(|w| wire_route(w.a, w.b, w.from, w.to, nodes, 1.0)).collect()
}

/// How much routing [`route_all`] does with care (roughly, points of wire checked against nodes
/// and against other wires) before it routes the remaining wires each on its own.
const WORK: usize = 1_000_000;

/// A loop from `a` out to the right as far as `right`, across at height `y`, back to `left` and
/// into `b` from the left: straight runs with rounded corners, so it keeps clear of whatever
/// lies inside that frame.
pub(crate) fn around(a: Pos2, b: Pos2, right: f32, y: f32, left: f32, radius: f32) -> Route {
    rounded(&[a, pos2(right, a.y), pos2(right, y), pos2(left, y), pos2(left, b.y), b], radius)
}

/// A path along `points`, each inner corner rounded to a quarter circle of up to `radius`.
/// The corners next to the ends may use the whole first or last run, the others half of theirs.
pub(crate) fn rounded(points: &[Pos2], radius: f32) -> Route {
    // Bezier handle length of a quarter circle, as a fraction of its radius.
    const ARC: f32 = 0.5523;
    let line = |p: Pos2, q: Pos2| [p, p.lerp(q, 1.0 / 3.0), p.lerp(q, 2.0 / 3.0), q];
    let last = points.len().saturating_sub(1);
    let mut route = Vec::new();
    let mut at = match points.first() {
        Some(p) => *p,
        None => return route,
    };
    for (i, w) in points.windows(3).enumerate() {
        let &[p, c, q] = w else { continue };
        let (din, dout) = ((c - p).normalized(), (q - c).normalized());
        let share_in = if i == 0 { 1.0 } else { 0.5 };
        let share_out = if i + 2 == last { 1.0 } else { 0.5 };
        let r = radius.min((c - p).length() * share_in).min((q - c).length() * share_out);
        let (s, e) = (c - din * r, c + dout * r);
        if at.distance(s) > 0.01 {
            route.push(line(at, s));
        }
        route.push([s, s + din * r * ARC, e - dout * r * ARC, e]);
        at = e;
    }
    if let Some(end) = points.last()
        && at.distance(*end) > 0.01
    {
        route.push(line(at, *end));
    }
    route
}

/// Length of a route, with the stretches inside a node counted two hundred times: any clean route
/// beats one that clips a node.
pub(crate) fn route_cost(route: &[[Pos2; 4]], nodes: &[Rect], zoom: f32) -> f32 {
    let points: Vec<Pos2> =
        route.iter().flat_map(|s| (0..=32).map(move |i| cubic_at(s, i as f32 / 32.0))).collect();
    let bounds = Rect::from_points(&points);
    // Grown, so a wire grazing a corner counts as clipping it; except right at the ports.
    let near: Vec<Rect> =
        nodes.iter().map(|r| r.expand(8.0 * zoom)).filter(|r| r.intersects(bounds)).collect();
    let ends: Vec<Pos2> = route.first().map(|s| s[0]).into_iter().chain(route.last().map(|s| s[3])).collect();
    let clips =
        |p: Pos2| ends.iter().all(|e| e.distance(p) > 12.0 * zoom) && near.iter().any(|r| r.contains(p));
    points
        .windows(2)
        .filter_map(|w| match w {
            [p, q] => {
                let len = p.distance(*q);
                let mid = p.lerp(*q, 0.5);
                Some(if clips(mid) { len * 200.0 } else { len })
            }
            _ => None,
        })
        .sum()
}

pub(crate) fn cubic_at(s: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let [p0, p1, p2, p3] = s.map(|p| p.to_vec2());
    (p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t)).to_pos2()
}

pub(crate) fn flatten(route: &[[Pos2; 4]]) -> Vec<Pos2> {
    let mut points = Vec::new();
    for s in route {
        let curve =
            egui::epaint::CubicBezierShape::from_points_stroke(*s, false, Color32::TRANSPARENT, Stroke::NONE);
        // Segments share their end points.
        let skip = usize::from(!points.is_empty());
        points.extend(curve.flatten(Some(0.5)).into_iter().skip(skip));
    }
    points
}

/// A wire to route, in graph units: from output `a` of the node at `from` to input `b` of the
/// node at `to`. `source` is the output it comes from; wires from one output may run together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ends {
    pub a: Pos2,
    pub b: Pos2,
    pub from: Rect,
    pub to: Rect,
    pub source: (NodeId, u32),
}

/// Distance below which two wires count as running on top of each other.
const COINCIDENT: f32 = 2.5;

/// How tidy a level looks, in graph units: the nodes' bounds, how long the wires are, how often
/// they cross, how much of them runs on top of a wire from another output and through nodes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measure {
    pub nodes: usize,
    pub wires: usize,
    pub width: f32,
    pub height: f32,
    pub wire_length: f32,
    pub longest: f32,
    pub crossings: usize,
    pub overlap: f32,
    /// Where wires cross.
    pub crossings_at: Vec<Pos2>,
    /// Where wires run on top of each other: one point per stretch.
    pub overlaps: Vec<Pos2>,
    pub through_nodes: f32,
}

/// Whether two directions differ by less than about twelve degrees.
fn parallel(u: egui::Vec2, v: egui::Vec2) -> bool {
    (u.x * v.y - u.y * v.x).abs() <= 0.2 * u.length() * v.length()
}

/// Counts a stretch of wire running alongside another, if it is long enough to see.
fn take_run(run: &mut (f32, Option<Pos2>), overlap: &mut f32, overlaps: &mut Vec<Pos2>) {
    if let (len, Some(at)) = *run
        && len >= MIN_OVERLAP
    {
        *overlap += len;
        if overlaps.iter().all(|o| o.distance(at) > 40.0) {
            overlaps.push(at);
        }
    }
    *run = (0.0, None);
}

/// The shortest stretch counted as overlapping.
const MIN_OVERLAP: f32 = 10.0;

fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    let t = if len2 > 0.0 { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}

fn segments_cross(a: Pos2, b: Pos2, c: Pos2, d: Pos2) -> bool {
    let side = |p: Pos2, q: Pos2, r: Pos2| (q - p).x * (r - p).y - (q - p).y * (r - p).x;
    let (d1, d2) = (side(c, d, a), side(c, d, b));
    let (d3, d4) = (side(a, b, c), side(a, b, d));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// Segments of polylines by grid cell, to find the ones near a point quickly.
struct Grid {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<(usize, usize)>>,
}

impl Grid {
    fn new(lines: &[Vec<Pos2>], cell: f32) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<(usize, usize)>> = HashMap::new();
        for (w, line) in lines.iter().enumerate() {
            for (s, pair) in line.windows(2).enumerate() {
                let &[p, q] = pair else { continue };
                let r = Rect::from_two_pos(p, q);
                let (x0, y0) = ((r.min.x / cell).floor() as i32, (r.min.y / cell).floor() as i32);
                let (x1, y1) = ((r.max.x / cell).floor() as i32, (r.max.y / cell).floor() as i32);
                for x in x0..=x1.min(x0 + 64) {
                    for y in y0..=y1.min(y0 + 64) {
                        cells.entry((x, y)).or_default().push((w, s));
                    }
                }
            }
        }
        Self { cell, cells }
    }

    /// The (line, segment) pairs in the cells around `p`.
    fn near(&self, p: Pos2) -> impl Iterator<Item = (usize, usize)> + '_ {
        let (cx, cy) = ((p.x / self.cell).floor() as i32, (p.y / self.cell).floor() as i32);
        (-1..=1)
            .flat_map(move |dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|k| self.cells.get(&k))
            .flatten()
            .copied()
    }
}

/// Measures the level made of `nodes` and `wires`, routed as [`route_all`] routes them.
pub fn measure(nodes: &[Rect], wires: &[Ends]) -> Measure {
    let routes = route_all(wires, nodes);
    // Short segments, so the grid finds every crossing near either end.
    let lines: Vec<Vec<Pos2>> = routes
        .iter()
        .map(|r| {
            let line = flatten(r);
            let mut points: Vec<Pos2> = line.first().copied().into_iter().collect();
            for w in line.windows(2) {
                let &[p, q] = w else { continue };
                let n = (p.distance(q) / 3.0).ceil().max(1.0);
                points.extend((1..=n as usize).map(|i| p.lerp(q, i as f32 / n)));
            }
            points
        })
        .collect();
    let bounds = nodes.iter().fold(Rect::NOTHING, |acc, r| acc.union(*r));
    let length = |line: &Vec<Pos2>| {
        line.windows(2)
            .map(|w| match w {
                [p, q] => p.distance(*q),
                _ => 0.0,
            })
            .sum::<f32>()
    };
    let lengths: Vec<f32> = lines.iter().map(length).collect();
    let grid = Grid::new(&lines, 16.0);
    let source = |w: usize| wires.get(w).map(|e| e.source);
    let segment = |w: usize, s: usize| -> Option<(Pos2, Pos2)> {
        let line = lines.get(w)?;
        Some((*line.get(s)?, *line.get(s + 1)?))
    };
    let mut overlap = 0.0;
    let mut overlaps = Vec::new();
    let mut through_nodes = 0.0;
    let mut crossed: HashMap<(usize, usize), Vec<Pos2>> = HashMap::new();
    for (w, line) in lines.iter().enumerate() {
        let (start, end) = (line.first().copied(), line.last().copied());
        // Only a stretch running alongside another wire counts, not a crossing at a flat angle.
        let mut run = (0.0f32, None::<Pos2>);
        for (p, q) in pieces(line) {
            let mid = p.lerp(q, 0.5);
            let len = p.distance(q);
            let alongside = grid
                .near(mid)
                .filter(|(o, _)| source(*o) != source(w))
                .filter_map(|(o, s)| segment(o, s))
                .any(|(a, b)| segment_distance(mid, a, b) < COINCIDENT && parallel(q - p, b - a));
            if alongside {
                run = (run.0 + len, run.1.or(Some(mid)));
            } else {
                take_run(&mut run, &mut overlap, &mut overlaps);
            }
            let at_end = [start, end].into_iter().flatten().any(|e| e.distance(mid) < 12.0);
            if !at_end && nodes.iter().any(|r| r.shrink(1.0).contains(mid)) {
                through_nodes += len;
            }
        }
        take_run(&mut run, &mut overlap, &mut overlaps);
        for pair in line.windows(2) {
            let &[p, q] = pair else { continue };
            for (o, t) in grid.near(p.lerp(q, 0.5)) {
                if o <= w || source(o) == source(w) {
                    continue;
                }
                if let Some((c, d)) = segment(o, t)
                    && segments_cross(p, q, c, d)
                {
                    // One crossing may show as two pairs of segments; nearby hits count once.
                    let at = crossed.entry((w, o)).or_default();
                    if at.iter().all(|x| x.distance(p) > 8.0) {
                        at.push(p);
                    }
                }
            }
        }
    }
    Measure {
        nodes: nodes.len(),
        wires: wires.len(),
        width: if nodes.is_empty() { 0.0 } else { bounds.width() },
        height: if nodes.is_empty() { 0.0 } else { bounds.height() },
        wire_length: lengths.iter().sum(),
        longest: lengths.iter().copied().fold(0.0, f32::max),
        crossings: crossed.values().map(Vec::len).sum(),
        crossings_at: crossed.values().flatten().copied().collect(),
        overlap,
        overlaps,
        through_nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `PN_DUMP` set, writes nodes and routes into that file, to look at.
    fn dump(nodes: &[Rect], wires: &[Ends]) {
        let Some(path) = std::env::var_os("PN_DUMP") else { return };
        let mut out = String::new();
        for r in nodes {
            out += &format!(
                "R {} {} {} {}
",
                r.min.x, r.min.y, r.max.x, r.max.y
            );
        }
        for r in route_all(wires, nodes) {
            let pts: Vec<String> = flatten(&r).iter().map(|p| format!("{} {}", p.x, p.y)).collect();
            out += &format!(
                "L {}
",
                pts.join(" ")
            );
        }
        std::fs::write(path, out).unwrap();
    }

    fn ends(a: Pos2, b: Pos2, from: Rect, to: Rect, source: u64) -> Ends {
        Ends { a, b, from, to, source: (NodeId(source), 0) }
    }

    #[test]
    fn wires_fanning_into_one_node_neither_overlap_nor_cross() {
        // Eight sources stacked in the column left of a switch, each into the next input.
        let target = Rect::from_min_size(pos2(200.0, 380.0), vec2(150.0, 180.0));
        let mut nodes = vec![target];
        let mut wires = Vec::new();
        for i in 0..8 {
            let from = Rect::from_min_size(pos2(0.0, i as f32 * 130.0), vec2(150.0, 110.0));
            nodes.push(from);
            let a = pos2(150.0, from.min.y + 60.0);
            let b = pos2(200.0, target.min.y + 30.0 + i as f32 * 18.0);
            wires.push(ends(a, b, from, target, i as u64 + 1));
        }
        dump(&nodes, &wires);
        let m = measure(&nodes, &wires);
        assert_eq!(m.overlap, 0.0, "{:?}", m.overlaps);
        assert_eq!(m.crossings, 0);
    }

    #[test]
    fn wires_through_one_gap_take_tracks_of_their_own() {
        // Two sources below a node in the middle column, wired into neighbouring inputs of a
        // node right of it: both have to pass the middle node, through the same gap.
        let size = vec2(150.0, 110.0);
        let middle = Rect::from_min_size(pos2(200.0, 0.0), vec2(150.0, 190.0));
        let below = Rect::from_min_size(pos2(200.0, 250.0), size);
        let target = Rect::from_min_size(pos2(400.0, 60.0), vec2(150.0, 190.0));
        let one = Rect::from_min_size(pos2(0.0, 180.0), size);
        let two = Rect::from_min_size(pos2(0.0, 330.0), size);
        let nodes = [middle, below, target, one, two];
        let wires = [
            ends(pos2(150.0, 240.0), pos2(400.0, 140.0), one, target, 1),
            ends(pos2(150.0, 390.0), pos2(400.0, 158.0), two, target, 2),
        ];
        dump(&nodes, &wires);
        let m = measure(&nodes, &wires);
        assert_eq!(m.through_nodes, 0.0);
        assert_eq!(m.overlap, 0.0, "{:?}", m.overlaps);
        assert_eq!(m.crossings, 0);
    }
}
