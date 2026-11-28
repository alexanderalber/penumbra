//! Tools for the lines through points ([`Strips`]): even spacing along each line
//! ([`Resampler`]) and smoothing along each line ([`Smoother`]).
//!
//! Both work on every kind of line a set of points has: one line through all the points, lines
//! of `len` in a row (closed or not), the rows and columns of a grid, pairs, and lines of any
//! length ([`Strips::Runs`]). Lines never mix: a point only sees the points of its own line.
//!
//! **Resampling** puts new points along each line, evenly spaced by the distance along it. The
//! new points take every attribute from the two old points around them, mixed by where they lie
//! between them; whole numbers (`id`) come from the nearer one. A closed line stays closed: its
//! points are spaced around the whole loop, the closing segment included. The work: a prefix sum
//! of the steps between points, started again at each line's first point, measures every line;
//! a prefix sum over the points each line wants gives where each line's new points start; one
//! pass finds, for every new point, the old segment it lies on; then the attributes are mixed,
//! as many per pass as the GPU binds buffers for.
//!
//! What comes out: by count, every line has the same number of points, so the lines are
//! [`Strips::Regular`] (a grid's rows stay a grid). By length, each line has as many as its
//! length needs, so the lines are [`Strips::Runs`], with [`LINE`] written, and the count is on
//! the GPU. Lines past `max` points are left out whole, from the end of the list.
//!
//! **Smoothing** moves each point towards the mean of its neighbours on its line, `strength` of
//! the way, as often as `iterations` says. A grid with lines both ways smooths as a surface. The
//! ends of open lines stay where they are unless they are let go; then the line shrinks from its
//! ends. Nothing but `position` changes; the count, the lines and the box stay.

use std::fmt::Write as _;

use penumbra_core::{Error, ParamSpec, Value};

use super::compact::attrs_per_pass;
use super::{
    Attr, AttrType, Count, HEADER_BYTES, LINE, POSITION, Points, Reports, Strips, Wake, capacity_for,
    most_elements,
};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// Points per row of a dispatch grid.
const ROW: u32 = 1 << 16;

/// Points per block of the prefix sum: 256 threads of 4 points each.
const BLOCK: u32 = 1024;

/// Workgroups along x of a dispatch over blocks; more blocks take more rows.
const BLOCKS_PER_ROW: u32 = 32768;

/// The most points a resampled line gets by count.
pub const MOST_BY_COUNT: u32 = 1 << 20;

/// The most smoothing steps one cook takes.
pub const MOST_ITERATIONS: u32 = 1000;

/// How the points of a set join into lines, as the programs read it: `params.kind`, `len`,
/// `closed`.
mod kind {
    /// One line through every point, in order.
    pub const ONE: i64 = 0;
    /// Every `len` points in a row are a line (a grid's rows, pairs with `len` 2).
    pub const REGULAR: i64 = 1;
    /// A grid's columns: `len` wide, a line down each column.
    pub const COLUMNS: i64 = 2;
    /// A grid's rows and columns at once (smoothing only).
    pub const GRID: i64 = 3;
    /// Lines of any length, each point's [`super::LINE`] says where it lies.
    pub const RUNS: i64 = 4;
}

/// The WGSL that finds a point's line. Needs `count_in: vec4u` (the count on the GPU, or
/// `u32::MAX`), `runs: array<vec2f>` ([`LINE`]) and the params `n` (the most points), `kind`,
/// `len` and `closed`.
const LAYOUT: &str = "
const LN_NONE: u32 = 0xffffffffu;
const LN_MOST: u32 = 16777216u;

// How many points there are: below the most, and below the count on the GPU.
fn ln_count() -> u32 {
    return min(count_in.x, u32(max(params.n, 0)));
}

// One line: its first point, the place of a point along it, its number of points, and how far
// apart its points lie in the list.
struct LnLine {
    first: u32,
    along: u32,
    points: u32,
    stride: u32,
}

// The line through point i (below the count) when the lines are of `kind`.
fn ln_line_in(i: u32, kind: i32) -> LnLine {
    let n = ln_count();
    let len = u32(max(params.len, 1));
    var l: LnLine;
    l.stride = 1u;
    if kind == 1 || kind == 3 {
        l.along = i % len;
        l.first = i - l.along;
        l.points = min(len, n - l.first);
    } else if kind == 2 {
        l.along = i / len;
        l.first = i % len;
        l.points = (n - l.first + len - 1u) / len;
        l.stride = len;
    } else if kind == 4 {
        // Read as floats and kept inside the list whatever they hold.
        let r = runs[i];
        l.along = min(u32(clamp(r.x, 0.0, f32(LN_MOST))), i);
        l.first = i - l.along;
        l.points = clamp(u32(clamp(r.y, 0.0, f32(LN_MOST))), l.along + 1u, n - l.first);
    } else {
        l.along = i;
        l.first = 0u;
        l.points = n;
    }
    return l;
}

fn ln_line_of(i: u32) -> LnLine {
    return ln_line_in(i, params.kind);
}

// Whether a line of this many points closes: a closed line needs three.
fn ln_closed(points: u32) -> bool {
    return params.closed != 0 && points > 2u;
}

// The point before i on its line, LN_NONE at an open line's start.
fn ln_before(l: LnLine, i: u32) -> u32 {
    if l.along > 0u {
        return i - l.stride;
    }
    if ln_closed(l.points) {
        return l.first + (l.points - 1u) * l.stride;
    }
    return LN_NONE;
}

// The point after i on its line, LN_NONE at an open line's end.
fn ln_after(l: LnLine, i: u32) -> u32 {
    if l.along + 1u < l.points {
        return i + l.stride;
    }
    if ln_closed(l.points) {
        return l.first;
    }
    return LN_NONE;
}
";

fn layout_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::int("n", 0),
        ParamSpec::int("kind", 0),
        ParamSpec::int("len", 1),
        ParamSpec::int("closed", 0),
    ]
}

/// Measures each line: the distance along it at each of its points (`cum`), its whole length
/// with a closed line's closing segment (`lens`, at its first point) and the points it wants
/// (`want`, at its first point; 0 at every other point).
///
/// Lines whose points follow each other in the list take a prefix sum that starts again at each
/// line's first point (`local`, `blocks`, `carry`, then `finish`), so one long line costs about
/// what many short ones do. A grid's columns, whose points lie a row apart, are walked by a
/// thread per column (`walk`).
const MEASURE: &str = "
@group(2) @binding(0) var<storage, read> position: array<vec4f>;
@group(2) @binding(1) var<storage, read> runs: array<vec2f>;
@group(2) @binding(2) var<storage, read_write> cum: array<f32>;
@group(2) @binding(3) var<storage, read_write> lens: array<f32>;
@group(2) @binding(4) var<storage, read_write> want: array<u32>;
// Per block of 1024 points: whether a line starts in it (x 1) and the distance along the line
// running at its end. After `blocks`: the distance along the line running into it (y).
@group(2) @binding(5) var<storage, read_write> carried: array<vec2f>;
@group(2) @binding(6) var<uniform> count_in: vec4u;

var<workgroup> ln_starts: array<u32, 256>;
var<workgroup> ln_runs: array<f32, 256>;

// The distance from point a to point b; none where it is not a number or out of reach.
fn ln_gap(a: u32, b: u32) -> f32 {
    let d = length(position[b].xyz - position[a].xyz);
    if d < 1e30 {
        return d;
    }
    return 0.0;
}

// The points a line of `len` metres and `points` points wants.
fn ln_want(len: f32, points: u32) -> u32 {
    if params.by == 0 {
        return clamp(u32(max(params.count, 1)), 1u, LN_MOST);
    }
    let per = len / max(params.spacing, 1e-30);
    var k = f32(LN_MOST);
    if per < f32(LN_MOST) {
        k = round(per);
    }
    if ln_closed(points) {
        return clamp(u32(k), 3u, LN_MOST);
    }
    return clamp(u32(k) + 1u, 2u, LN_MOST);
}

fn ln_block(w: vec3u) -> u32 {
    return w.y * u32(max(params.row, 1)) + w.x;
}

// Whether point i starts a line. Points past the count count as starts: nothing runs on.
fn ln_starts_at(i: u32) -> bool {
    return i >= ln_count() || ln_line_of(i).along == 0u;
}

// Joins the threads' runs, so each holds the distance running at its end: a run in which a line
// starts keeps its own value, any other adds the value before it. Every thread calls it.
fn ln_join(t: u32, start: u32, run: f32) {
    ln_starts[t] = start;
    ln_runs[t] = run;
    workgroupBarrier();
    for (var d = 1u; d < 256u; d = d << 1u) {
        var start_before = 0u;
        var run_before = 0.0;
        if t >= d {
            start_before = ln_starts[t - d];
            run_before = ln_runs[t - d];
        }
        workgroupBarrier();
        if t >= d {
            if ln_starts[t] == 0u {
                ln_runs[t] += run_before;
            }
            ln_starts[t] |= start_before;
        }
        workgroupBarrier();
    }
}

@compute @workgroup_size(256)
fn local(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = ln_block(w);
    let first = b * 1024u + t * 4u;
    var start = 0u;
    var run = 0.0;
    var at: array<f32, 4>;
    var started: array<u32, 4>;
    for (var k = 0u; k < 4u; k++) {
        let i = first + k;
        if ln_starts_at(i) {
            start = 1u;
            run = 0.0;
        } else {
            run += ln_gap(i - 1u, i);
        }
        at[k] = run;
        started[k] = start;
    }
    ln_join(t, start, run);
    var before = 0.0;
    if t > 0u {
        before = ln_runs[t - 1u];
    }
    let n = u32(max(params.n, 0));
    for (var k = 0u; k < 4u; k++) {
        let i = first + k;
        if i < n {
            var c = at[k];
            if started[k] == 0u {
                c += before;
            }
            cum[i] = c;
        }
    }
    if t == 255u && b < u32(max(params.blocks, 0)) {
        carried[b] = vec2f(f32(ln_starts[255]), ln_runs[255]);
    }
}

@compute @workgroup_size(256)
fn blocks(@builtin(local_invocation_index) t: u32) {
    let blocks = u32(max(params.blocks, 0));
    let chunk = (blocks + 255u) / 256u;
    let lo = min(t * chunk, blocks);
    let hi = min(lo + chunk, blocks);
    var start = 0u;
    var run = 0.0;
    for (var j = lo; j < hi; j++) {
        let c = carried[j];
        if c.x > 0.5 {
            start = 1u;
            run = c.y;
        } else {
            run += c.y;
        }
    }
    ln_join(t, start, run);
    var into = 0.0;
    if t > 0u {
        into = ln_runs[t - 1u];
    }
    for (var j = lo; j < hi; j++) {
        let c = carried[j];
        carried[j] = vec2f(0.0, into);
        if c.x > 0.5 {
            into = c.y;
        } else {
            into += c.y;
        }
    }
}

@compute @workgroup_size(64)
fn carry(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= ln_count() {
        return;
    }
    let block = i / 1024u;
    if ln_line_of(i).first < block * 1024u {
        cum[i] += carried[block].y;
    }
}

@compute @workgroup_size(64)
fn finish(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= u32(max(params.n, 0)) {
        return;
    }
    want[i] = 0u;
    if i >= ln_count() {
        return;
    }
    let l = ln_line_of(i);
    if l.along != 0u {
        return;
    }
    let last = i + (l.points - 1u) * l.stride;
    var len = cum[last];
    if ln_closed(l.points) {
        len += ln_gap(last, i);
    }
    lens[i] = len;
    want[i] = ln_want(len, l.points);
}

@compute @workgroup_size(64)
fn walk(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= u32(max(params.n, 0)) {
        return;
    }
    want[i] = 0u;
    if i >= ln_count() {
        return;
    }
    let l = ln_line_of(i);
    if l.along != 0u {
        return;
    }
    var at = 0.0;
    var last = i;
    cum[i] = 0.0;
    for (var q = 1u; q < l.points; q++) {
        let j = i + q * l.stride;
        at += ln_gap(last, j);
        cum[j] = at;
        last = j;
    }
    if ln_closed(l.points) {
        at += ln_gap(last, i);
    }
    lens[i] = at;
    want[i] = ln_want(at, l.points);
}
";

fn measure_params() -> Vec<ParamSpec> {
    let mut p = layout_params();
    p.extend([
        ParamSpec::int("by", 0),
        ParamSpec::int("count", 2),
        ParamSpec::float("spacing", 1.0),
        ParamSpec::int("blocks", 0),
        ParamSpec::int("row", 1),
    ]);
    p
}

/// The prefix sum over the points each line wants (`offsets`: where each line's new points
/// start), and the header: the new points that fit `cap` with whole lines, then 0, then all the
/// lines want, then 0. Sums stop at `u32::MAX` instead of wrapping.
const SCAN: &str = "
@group(2) @binding(0) var<storage, read> want: array<u32>;
@group(2) @binding(1) var<storage, read_write> sums: array<u32>;
@group(2) @binding(2) var<storage, read_write> offsets: array<u32>;
@group(2) @binding(3) var<storage, read_write> header: array<u32>;

var<workgroup> ln_scan: array<u32, 256>;

fn ln_add(a: u32, b: u32) -> u32 {
    let s = a + b;
    return select(s, 0xffffffffu, s < a);
}

fn ln_value(i: u32) -> u32 {
    if i < u32(max(params.n, 0)) {
        return want[i];
    }
    return 0u;
}

// The sum of v over the threads before this one. Every thread of the workgroup calls it;
// afterwards ln_scan[255] holds the sum over all of them.
fn ln_exclusive(t: u32, v: u32) -> u32 {
    ln_scan[t] = v;
    workgroupBarrier();
    for (var d = 1u; d < 256u; d = d << 1u) {
        var add = 0u;
        if t >= d {
            add = ln_scan[t - d];
        }
        workgroupBarrier();
        ln_scan[t] = ln_add(ln_scan[t], add);
        workgroupBarrier();
    }
    return ln_scan[t] - v;
}

fn ln_block(w: vec3u) -> u32 {
    return w.y * u32(max(params.row, 1)) + w.x;
}

@compute @workgroup_size(256)
fn count(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = ln_block(w);
    let first = b * 1024u + t * 4u;
    let mine = ln_add(ln_add(ln_value(first), ln_value(first + 1u)), ln_add(ln_value(first + 2u), ln_value(first + 3u)));
    ln_exclusive(t, mine);
    if t == 255u && b < u32(max(params.blocks, 0)) {
        sums[b] = ln_scan[255];
    }
}

@compute @workgroup_size(256)
fn scan(@builtin(local_invocation_index) t: u32) {
    let blocks = u32(max(params.blocks, 0));
    let chunk = (blocks + 255u) / 256u;
    let lo = min(t * chunk, blocks);
    let hi = min(lo + chunk, blocks);
    var mine = 0u;
    for (var j = lo; j < hi; j++) {
        mine = ln_add(mine, sums[j]);
    }
    var at = ln_exclusive(t, mine);
    for (var j = lo; j < hi; j++) {
        let v = sums[j];
        sums[j] = at;
        at = ln_add(at, v);
    }
}

@compute @workgroup_size(256)
fn place(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = ln_block(w);
    let first = b * 1024u + t * 4u;
    let v = vec4u(ln_value(first), ln_value(first + 1u), ln_value(first + 2u), ln_value(first + 3u));
    let before_me = ln_exclusive(t, ln_add(ln_add(v.x, v.y), ln_add(v.z, v.w)));
    if b < u32(max(params.blocks, 0)) {
        var at = ln_add(sums[b], before_me);
        for (var j = 0u; j < 4u; j++) {
            if first + j < u32(max(params.n, 0)) {
                offsets[first + j] = at;
                at = ln_add(at, v[j]);
            }
        }
    }
}

// Where the lines end rises along the list, so the lines that fit are the first ones.
fn ln_end(i: u32) -> u32 {
    return ln_add(offsets[i], want[i]);
}

@compute @workgroup_size(1)
fn fit() {
    let n = u32(max(params.n, 0));
    let cap = u32(max(params.cap, 0));
    var lo = 0u;
    var hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2u;
        if ln_end(mid) <= cap {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    var kept = 0u;
    if lo > 0u {
        kept = ln_end(lo - 1u);
    }
    var all = 0u;
    if n > 0u {
        all = ln_end(n - 1u);
    }
    header[0] = kept;
    header[1] = 0u;
    header[2] = all;
    header[3] = 0u;
}
";

fn scan_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::int("n", 0),
        ParamSpec::int("blocks", 0),
        ParamSpec::int("row", 1),
        ParamSpec::int("cap", 0),
    ]
}

/// For every new point: the two old points it lies between and how far from the first (`map`:
/// first, second, the share as float bits, 0), and its place along its line with the line's
/// number of points (`line`).
const PLACE: &str = "
@group(2) @binding(0) var<storage, read> offsets: array<u32>;
@group(2) @binding(1) var<storage, read> want: array<u32>;
@group(2) @binding(2) var<storage, read> lens: array<f32>;
@group(2) @binding(3) var<storage, read> cum: array<f32>;
@group(2) @binding(4) var<storage, read> runs: array<vec2f>;
@group(2) @binding(5) var<storage, read_write> map: array<vec4u>;
@group(2) @binding(6) var<storage, read_write> line: array<vec2f>;
@group(2) @binding(7) var<uniform> header: vec4u;
@group(2) @binding(8) var<uniform> count_in: vec4u;

@compute @workgroup_size(64)
fn locate(@builtin(global_invocation_id) id: vec3u) {
    let o = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || o >= min(header.x, u32(max(params.cap, 0))) {
        return;
    }
    // The line: the last point whose line starts at or before o. Points that start no line
    // want none, so they share the offset of the next line's start.
    let n = ln_count();
    var lo = 0u;
    var hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2u;
        if offsets[mid] <= o {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    let s = max(lo, 1u) - 1u;
    let l = ln_line_of(s);
    let k = max(want[s], 1u);
    let j = min(o - min(offsets[s], o), k - 1u);
    let total = lens[s];
    let closed = ln_closed(l.points);
    var goal = 0.0;
    if closed {
        goal = total * f32(j) / f32(k);
    } else if k > 1u {
        goal = total * f32(j) / f32(k - 1u);
    }
    // The last point of the line at or before the goal.
    var a = 0u;
    var b = l.points;
    while a < b {
        let m = a + (b - a) / 2u;
        if cum[s + m * l.stride] <= goal {
            a = m + 1u;
        } else {
            b = m;
        }
    }
    let q = max(a, 1u) - 1u;
    let i0 = s + q * l.stride;
    var i1 = i0;
    var t = 0.0;
    var gap = 0.0;
    if q + 1u < l.points {
        i1 = i0 + l.stride;
        gap = cum[i1] - cum[i0];
    } else if closed {
        i1 = s;
        gap = total - cum[i0];
    }
    if gap > 0.0 {
        t = clamp((goal - cum[i0]) / gap, 0.0, 1.0);
    }
    map[o] = vec4u(i0, i1, bitcast<u32>(t), 0u);
    line[o] = vec2f(f32(j), f32(k));
}
";

fn place_params() -> Vec<ParamSpec> {
    let mut p = layout_params();
    p.push(ParamSpec::int("cap", 0));
    p
}

/// The mix of `k` attributes: each new point `o` below the new count takes the two old points of
/// `map[o]`, `params.s<a>` words each, mixed by the share where `params.f<a>` is 1 (floats), the
/// nearer one where it is 0 (whole numbers).
fn mix_source(k: usize) -> String {
    let mut s = String::from(
        "@group(2) @binding(0) var<storage, read> map: array<vec4u>;\n\
         @group(2) @binding(1) var<uniform> header: vec4u;\n",
    );
    for a in 0..k {
        let _ = writeln!(s, "@group(2) @binding({}) var<storage, read> in{a}: array<u32>;", 2 + 2 * a);
        let _ = writeln!(s, "@group(2) @binding({}) var<storage, read_write> out{a}: array<u32>;", 3 + 2 * a);
    }
    s.push_str(
        "
// x and y mixed by t, exactly x at 0 and exactly y at 1.
fn ln_mix(x: f32, y: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return x;
    }
    if t >= 1.0 {
        return y;
    }
    return x * (1.0 - t) + y * t;
}

@compute @workgroup_size(64)
fn blend(@builtin(global_invocation_id) id: vec3u) {
    let o = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || o >= min(header.x, u32(max(params.n, 0))) {
        return;
    }
    let m = map[o];
    let t = bitcast<f32>(m.z);
    let near = select(m.x, m.y, t >= 0.5);
",
    );
    for a in 0..k {
        let _ = writeln!(
            s,
            "    let s{a} = u32(max(params.s{a}, 1));
    for (var w = 0u; w < s{a}; w++) {{
        if params.f{a} != 0 {{
            let x = bitcast<f32>(in{a}[m.x * s{a} + w]);
            let y = bitcast<f32>(in{a}[m.y * s{a} + w]);
            out{a}[o * s{a} + w] = bitcast<u32>(ln_mix(x, y, t));
        }} else {{
            out{a}[o * s{a} + w] = in{a}[near * s{a} + w];
        }}
    }}"
        );
    }
    s.push_str("}\n");
    s
}

fn mix_params(k: usize) -> Vec<ParamSpec> {
    let mut p = vec![ParamSpec::int("n", 0)];
    p.extend((0..k).map(|a| ParamSpec::int(&format!("s{a}"), 1)));
    p.extend((0..k).map(|a| ParamSpec::int(&format!("f{a}"), 1)));
    p
}

/// One smoothing step: each point moves `strength` of the way to the mean of its neighbours.
const SMOOTH: &str = "
@group(2) @binding(0) var<storage, read> src: array<vec4f>;
@group(2) @binding(1) var<storage, read_write> dst: array<vec4f>;
@group(2) @binding(2) var<storage, read> runs: array<vec2f>;
@group(2) @binding(3) var<uniform> count_in: vec4u;

struct LnNear {
    sum: vec3f,
    k: u32,
    end: bool,
}

fn ln_near(l: LnLine, i: u32, at: LnNear) -> LnNear {
    var out = at;
    let b = ln_before(l, i);
    let a = ln_after(l, i);
    if b == LN_NONE || a == LN_NONE {
        out.end = true;
    }
    if b != LN_NONE && b != i {
        out.sum += src[b].xyz;
        out.k += 1u;
    }
    if a != LN_NONE && a != i && a != b {
        out.sum += src[a].xyz;
        out.k += 1u;
    }
    return out;
}

@compute @workgroup_size(64)
fn relax(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= u32(max(params.n, 0)) {
        return;
    }
    let p = src[i];
    if i >= ln_count() {
        dst[i] = p;
        return;
    }
    var near: LnNear;
    near.sum = vec3f(0.0);
    near.k = 0u;
    near.end = false;
    if params.kind == 3 {
        near = ln_near(ln_line_in(i, 1), i, near);
        near = ln_near(ln_line_in(i, 2), i, near);
    } else {
        near = ln_near(ln_line_of(i), i, near);
    }
    if near.k == 0u || (near.end && params.pin != 0) {
        dst[i] = p;
        return;
    }
    let q = p.xyz + (near.sum / f32(near.k) - p.xyz) * params.strength;
    // A neighbour that is not a number leaves the point where it is.
    if all(abs(q) < vec3f(1e30)) {
        dst[i] = vec4f(q, p.w);
    } else {
        dst[i] = p;
    }
}
";

fn smooth_params() -> Vec<ParamSpec> {
    let mut p = layout_params();
    p.extend([ParamSpec::int("pin", 1), ParamSpec::float("strength", 0.5)]);
    p
}

/// Compiles the passes, so the first cook does not wait for them.
pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    gpu.cached_compute("pts lines measure", &format!("{LAYOUT}{MEASURE}"), &measure_params())?;
    gpu.cached_compute("pts lines scan", SCAN, &scan_params())?;
    gpu.cached_compute("pts lines place", &format!("{LAYOUT}{PLACE}"), &place_params())?;
    let k = attrs_per_pass(gpu);
    gpu.cached_compute("pts lines mix", &mix_source(k), &mix_params(k))?;
    gpu.cached_compute("pts lines smooth", &format!("{LAYOUT}{SMOOTH}"), &smooth_params())?;
    Ok(())
}

/// How the points join into lines, as the programs read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    kind: i64,
    len: u32,
    closed: bool,
}

impl Layout {
    /// The lines of `points`. `surface`: a grid with lines both ways is one surface (smoothing);
    /// otherwise its rows are the lines. Lines of any length without their [`LINE`] attribute
    /// are taken as one line.
    fn of(points: &Points, surface: bool) -> Self {
        let one = Layout { kind: kind::ONE, len: 1, closed: false };
        match points.strips() {
            Strips::Regular { len, closed } if len > 0 => Layout { kind: kind::REGULAR, len, closed },
            Strips::Pairs => Layout { kind: kind::REGULAR, len: 2, closed: false },
            Strips::Grid { cols, rows, columns } => {
                let len = cols.max(1);
                match (rows, columns) {
                    (true, true) if surface => Layout { kind: kind::GRID, len, closed: false },
                    (true, _) => Layout { kind: kind::REGULAR, len, closed: false },
                    (false, true) => Layout { kind: kind::COLUMNS, len, closed: false },
                    (false, false) => one,
                }
            }
            Strips::Runs { closed } if runs_of(points).is_some() => {
                Layout { kind: kind::RUNS, len: 1, closed }
            }
            _ => one,
        }
    }

    /// The most lines among `n` points.
    fn most_lines(&self, n: u32) -> u32 {
        match self.kind {
            kind::REGULAR | kind::GRID => n.div_ceil(self.len.max(1)),
            kind::COLUMNS => n.min(self.len),
            kind::RUNS => n,
            _ => u32::from(n > 0),
        }
    }

    fn param(&self, name: &str) -> Option<Value> {
        match name {
            "kind" => Some(Value::Int(self.kind)),
            "len" => Some(Value::Int(i64::from(self.len))),
            "closed" => Some(Value::Int(i64::from(self.closed))),
            _ => None,
        }
    }
}

/// The [`LINE`] buffer of `points`, where it has one of the right type.
fn runs_of(points: &Points) -> Option<&Buffer> {
    points.attr(LINE).filter(|a| a.ty() == AttrType::Vec2).map(Attr::buffer)
}

/// How many points each line gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum By {
    /// This many on every line (2 or more).
    Count(u32),
    /// About this far apart (metres): each line as many as its length needs, spaced evenly.
    Length(f32),
}

/// What one resampling does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resample {
    pub by: By,
    /// The most points that come out: lines past it are left out whole.
    pub max: u32,
}

/// What one node needs to resample lines: the buffers it writes, kept across cooks.
#[derive(Default)]
pub struct Resampler {
    cum: Option<Buffer>,
    lens: Option<Buffer>,
    want: Option<Buffer>,
    offsets: Option<Buffer>,
    sums: Option<Buffer>,
    carried: Option<Buffer>,
    map: Option<Buffer>,
    none: Option<Buffer>,
    headers: BufferRing,
    line: BufferRing,
    rings: Vec<(String, BufferRing)>,
    reports: Reports<Resample>,
    /// The most points the lines wanted as read back for the settings of now: the capacity
    /// grows to it.
    wanted: u32,
    /// The settings `wanted` was read under.
    wanted_for: Option<Resample>,
    /// Where the last resampling knew its count on the CPU: the points that came out and the
    /// points all lines wanted.
    known: Option<(u32, u32)>,
}

impl Resampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines of `points`, each with new points evenly spaced along it as `set` says, with
    /// every attribute mixed from the old points around each new one. `Err` for points without
    /// a `vec3f` `position`, and for anything the GPU refuses. `wake` is called when the header
    /// read back for [`Resampler::words`] arrives with news.
    pub fn resample(
        &mut self,
        gpu: &Gpu,
        points: &Points,
        set: &Resample,
        wake: Option<Wake>,
    ) -> Result<Points, Error> {
        let position = points
            .attr(POSITION)
            .ok_or_else(|| Error::new("the points have no `position` to resample along"))?;
        if position.ty() != AttrType::Vec3 {
            return Err(Error::new("`position` must be vec3f to resample along it"));
        }
        let n = points.count().at_most();
        if n == 0 {
            return Ok(points.clone());
        }
        let layout = Layout::of(points, false);
        let lines = layout.most_lines(n);
        let most = most_elements(gpu, AttrType::Vec4);
        let max = set.max.clamp(1, most);
        let by_count = match set.by {
            By::Count(c) => Some(c.clamp(2, MOST_BY_COUNT)),
            By::Length(_) => None,
        };
        // A count the CPU knows: lines that all get the same number of points.
        let known = match (by_count, points.count().known()) {
            (Some(c), Some(_)) if layout.kind != kind::RUNS => {
                let kept = lines.min(max / c);
                self.known = Some((kept * c, u64_to_u32(u64::from(lines) * u64::from(c))));
                Some(kept * c)
            }
            _ => {
                self.known = None;
                None
            }
        };
        if self.wanted_for.as_ref() != Some(set) {
            self.wanted = 0;
            self.wanted_for = Some(*set);
        }
        if let Some(w) = self.reports.words(set) {
            self.wanted = self.wanted.max(w[2]);
        }
        let capacity = match (known, by_count) {
            (Some(k), _) => capacity_for(k, most).max(1),
            (None, Some(c)) => {
                capacity_for(u64_to_u32(u64::from(lines) * u64::from(c)).min(max), most).max(1)
            }
            (None, None) => capacity_for(n.max(self.wanted).min(max), most).max(1),
        };
        let cap = known.unwrap_or(capacity.min(max));

        let in_capacity = points.capacity().max(n);
        let cum = sized(gpu, &mut self.cum, u64::from(in_capacity) * 4, "pts lines along")?;
        let lens = sized(gpu, &mut self.lens, u64::from(in_capacity) * 4, "pts lines length")?;
        let want = sized(gpu, &mut self.want, u64::from(in_capacity) * 4, "pts lines want")?;
        let offsets = sized(gpu, &mut self.offsets, u64::from(in_capacity) * 4, "pts lines offsets")?;
        let sums =
            sized(gpu, &mut self.sums, u64::from(in_capacity.div_ceil(BLOCK).max(1)) * 4, "pts lines sums")?;
        let carried = sized(
            gpu,
            &mut self.carried,
            u64::from(in_capacity.div_ceil(BLOCK).max(1)) * 8,
            "pts lines carried",
        )?;
        let map = sized(gpu, &mut self.map, u64::from(capacity) * 16, "pts lines map")?;
        let none = sized(gpu, &mut self.none, 16, "pts lines none")?;
        let runs = runs_of(points).filter(|_| layout.kind == kind::RUNS).cloned().unwrap_or(none);
        let header = self.headers.next(gpu, HEADER_BYTES, "pts count")?;
        let count_in = points.count().header_for(gpu)?;
        let line = self.line.next(gpu, u64::from(capacity) * AttrType::Vec2.stride(), "pts line")?;

        let (by, count, spacing) = match set.by {
            By::Count(_) => (0, by_count.unwrap_or(2), 1.0),
            By::Length(l) => (1, 2, if l.is_finite() && l > 0.0 { f64::from(l) } else { 1.0 }),
        };
        let blocks = n.div_ceil(BLOCK);
        let row = blocks.min(BLOCKS_PER_ROW);
        let rows = blocks.div_ceil(row);
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "by" => Some(Value::Int(by)),
            "count" => Some(Value::Int(i64::from(count))),
            "spacing" => Some(Value::Float(spacing)),
            "blocks" => Some(Value::Int(i64::from(blocks))),
            "row" => Some(Value::Int(i64::from(row))),
            "cap" => Some(Value::Int(i64::from(cap))),
            other => layout.param(other),
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let measure =
            gpu.cached_compute("pts lines measure", &format!("{LAYOUT}{MEASURE}"), &measure_params())?;
        let over_blocks = Out::Size([row * 256, rows, 1]);
        let walk = [Step { entry: "walk", out: grid(n), input: &input }];
        let sum = [
            Step { entry: "local", out: over_blocks, input: &input },
            Step { entry: "blocks", out: Out::range(256), input: &input },
            Step { entry: "carry", out: grid(n), input: &input },
            Step { entry: "finish", out: grid(n), input: &input },
        ];
        // A grid's columns are not in a row in the list: a thread walks each.
        let measuring: &[Step<'_>] = if layout.kind == kind::COLUMNS { &walk } else { &sum };
        measure.run_with(
            gpu,
            &[position.buffer(), &runs, &cum, &lens, &want, &carried, &count_in],
            measuring,
        )?;
        let scan = gpu.cached_compute("pts lines scan", SCAN, &scan_params())?;
        scan.run_with(
            gpu,
            &[&want, &sums, &offsets, &header],
            &[
                Step { entry: "count", out: over_blocks, input: &input },
                Step { entry: "scan", out: Out::range(256), input: &input },
                Step { entry: "place", out: over_blocks, input: &input },
                Step { entry: "fit", out: Out::range(1), input: &input },
            ],
        )?;
        let place = gpu.cached_compute("pts lines place", &format!("{LAYOUT}{PLACE}"), &place_params())?;
        place.run_with(
            gpu,
            &[&offsets, &want, &lens, &cum, &runs, &map, &line, &header, &count_in],
            &[Step { entry: "locate", out: grid(cap), input: &input }],
        )?;

        let attrs: Vec<&Attr> = points.attrs().iter().filter(|a| a.name() != LINE).collect();
        self.rings.retain(|(name, _)| attrs.iter().any(|a| a.name() == name));
        let mut out: Vec<Attr> = Vec::with_capacity(attrs.len() + 1);
        let per = attrs_per_pass(gpu);
        for group in attrs.chunks(per) {
            let mut outs = Vec::with_capacity(group.len());
            for a in group {
                outs.push(self.next(gpu, a.name(), u64::from(capacity) * a.ty().stride())?);
            }
            let k = group.len();
            let program = gpu.cached_compute("pts lines mix", &mix_source(k), &mix_params(k))?;
            let mut buffers: Vec<&Buffer> = vec![&map, &header];
            for (a, o) in group.iter().zip(&outs) {
                buffers.push(a.buffer());
                buffers.push(o);
            }
            let words: Vec<i64> = group.iter().map(|a| (a.ty().stride() / 4) as i64).collect();
            let floats: Vec<i64> =
                group.iter().map(|a| i64::from(!matches!(a.ty(), AttrType::U32 | AttrType::I32))).collect();
            let param = |name: &str| {
                if name == "n" {
                    return Some(Value::Int(i64::from(cap)));
                }
                let (list, k) = match name.split_at_checked(1)? {
                    ("s", k) => (&words, k),
                    ("f", k) => (&floats, k),
                    _ => return None,
                };
                k.parse::<usize>().ok().and_then(|k| list.get(k)).map(|v| Value::Int(*v))
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            program.run_with(gpu, &buffers, &[Step { entry: "blend", out: grid(cap), input: &input }])?;
            for (a, o) in group.iter().zip(outs) {
                out.push(Attr::new(a.name(), a.ty(), o)?);
            }
        }

        let closed = layout.closed && matches!(layout.kind, kind::REGULAR | kind::RUNS);
        let (strips, dims) = match (by_count, points.strips()) {
            (Some(c), Strips::Grid { rows: true, columns, .. }) if known.is_some() => {
                (Strips::Grid { cols: c, rows: true, columns }, known.map(|k| [k / c, c, 1]))
            }
            (Some(c), _) => (Strips::Regular { len: c, closed }, None),
            (None, _) => {
                out.push(Attr::new(LINE, AttrType::Vec2, line)?);
                (Strips::Runs { closed }, None)
            }
        };
        let made = Points::new(capacity, 0, out)?.with_bounds(points.bounds());
        let made = match known {
            Some(k) => made.with_count(Count::Known(k))?.with_dims(dims),
            None => {
                let seen = self.reports.read(gpu, &header, set, &[position.buffer(), &count_in], wake);
                made.with_count(Count::gpu(header, cap, seen)?)?
            }
        };
        Ok(made.with_strips(strips))
    }

    /// Whether `max` left lines out at the last resampling under `set`: the points that came
    /// out and the points all lines wanted. Where the count is on the GPU this is read back, a
    /// frame or more late.
    pub fn cut(&mut self, set: &Resample) -> Option<(u32, u32)> {
        let (kept, wanted) = match self.known {
            Some(k) => k,
            None => self.reports.words(set).map(|w| (w[0], w[2]))?,
        };
        (wanted > kept && wanted > set.max).then_some((kept, wanted))
    }

    /// A buffer of `bytes` for attribute `name` that nobody else holds.
    fn next(&mut self, gpu: &Gpu, name: &str, bytes: u64) -> Result<Buffer, Error> {
        let label = format!("pts {name}");
        if let Some((_, ring)) = self.rings.iter_mut().find(|(n, _)| n == name) {
            return ring.next(gpu, bytes, &label);
        }
        let mut ring = BufferRing::new();
        let b = ring.next(gpu, bytes, &label)?;
        self.rings.push((name.to_string(), ring));
        Ok(b)
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        let own = [
            &self.cum,
            &self.lens,
            &self.want,
            &self.offsets,
            &self.sums,
            &self.carried,
            &self.map,
            &self.none,
        ]
        .iter()
        .filter_map(|b| b.as_ref())
        .map(Buffer::bytes)
        .sum::<u64>();
        own + self.headers.bytes()
            + self.line.bytes()
            + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
    }

    /// Lets go of the buffers nobody downstream holds.
    pub fn trim(&mut self) {
        for b in [
            &mut self.cum,
            &mut self.lens,
            &mut self.want,
            &mut self.offsets,
            &mut self.sums,
            &mut self.carried,
            &mut self.map,
        ] {
            *b = None;
        }
        self.headers.trim();
        self.line.trim();
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }
}

/// What one node needs to smooth lines: the buffers it writes, kept across cooks.
#[derive(Default)]
pub struct Smoother {
    ring: BufferRing,
    spare: Option<Buffer>,
    none: Option<Buffer>,
}

/// What one smoothing does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smooth {
    /// Steps, each a move of every point towards its neighbours; 0 passes the points on.
    pub iterations: u32,
    /// How far each step moves a point to the mean of its neighbours: 0 not at all, 1 all the
    /// way. Kept between 0 and 1.
    pub strength: f32,
    /// The ends of open lines stay where they are.
    pub pin_ends: bool,
}

impl Smoother {
    pub fn new() -> Self {
        Self::default()
    }

    /// `points` with each position moved along its line as `set` says; every other attribute,
    /// the count, the lines and the box stay. `Err` for points without a `vec3f` `position`,
    /// and for anything the GPU refuses.
    pub fn smooth(&mut self, gpu: &Gpu, points: &Points, set: &Smooth) -> Result<Points, Error> {
        let n = points.count().at_most();
        let iterations = set.iterations.min(MOST_ITERATIONS);
        let strength = if set.strength.is_finite() { set.strength.clamp(0.0, 1.0) } else { 0.0 };
        let Some(position) = points.attr(POSITION) else {
            return Err(Error::new("the points have no `position` to smooth"));
        };
        if position.ty() != AttrType::Vec3 {
            return Err(Error::new("`position` must be vec3f to smooth it"));
        }
        if n == 0 || iterations == 0 || strength == 0.0 {
            return Ok(points.clone());
        }
        let layout = Layout::of(points, true);
        let bytes = u64::from(points.capacity().max(n)) * AttrType::Vec3.stride();
        let out = self.ring.next(gpu, bytes, "pts position")?;
        let spare = sized(gpu, &mut self.spare, bytes, "pts smooth spare")?;
        let none = sized(gpu, &mut self.none, 16, "pts lines none")?;
        let runs = runs_of(points).filter(|_| layout.kind == kind::RUNS).cloned().unwrap_or(none);
        let count_in = points.count().header_for(gpu)?;
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "pin" => Some(Value::Int(i64::from(set.pin_ends))),
            "strength" => Some(Value::Float(f64::from(strength))),
            other => layout.param(other),
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let steps = [Step { entry: "relax", out: grid(n), input: &input }];
        // The last step writes `out`: the steps take turns between `out` and `spare`.
        let first_into_out = iterations % 2 == 1;
        let sets: Vec<[&Buffer; 4]> = (0..iterations)
            .map(|k| {
                let into_out = (k % 2 == 0) == first_into_out;
                let src = if k == 0 {
                    position.buffer()
                } else if into_out {
                    &spare
                } else {
                    &out
                };
                let dst = if into_out { &out } else { &spare };
                [src, dst, &runs, &count_in]
            })
            .collect();
        let runs_list: Vec<(&[&Buffer], &[Step<'_>])> =
            sets.iter().map(|b| (b.as_slice(), steps.as_slice())).collect();
        let program =
            gpu.cached_compute("pts lines smooth", &format!("{LAYOUT}{SMOOTH}"), &smooth_params())?;
        program.run_many(gpu, &runs_list)?;
        points.with_attr(Attr::new(POSITION, AttrType::Vec3, out)?).map(|p| p.with_bounds(points.bounds()))
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        self.ring.bytes()
            + [&self.spare, &self.none].iter().filter_map(|b| b.as_ref()).map(Buffer::bytes).sum::<u64>()
    }

    /// Lets go of the buffers nobody downstream holds.
    pub fn trim(&mut self) {
        self.ring.trim();
        self.spare = None;
    }
}

fn u64_to_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// The dispatch grid over `n` elements: rows of [`ROW`].
fn grid(n: u32) -> Out<'static> {
    let w = n.clamp(1, ROW);
    Out::grid(w, n.div_ceil(w))
}

/// `slot`'s buffer when it has `bytes`, else a new one of that size in its place.
fn sized(gpu: &Gpu, slot: &mut Option<Buffer>, bytes: u64, label: &str) -> Result<Buffer, Error> {
    if let Some(b) = slot.as_ref().filter(|b| b.bytes() == bytes) {
        return Ok(b.clone());
    }
    let b = Buffer::new(gpu, bytes, label)?;
    *slot = Some(b.clone());
    Ok(b)
}

#[cfg(test)]
mod tests;
