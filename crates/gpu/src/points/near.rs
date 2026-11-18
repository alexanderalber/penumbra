//! Finding the points near each point on the GPU ([`NearGrid`]): the search behind
//! `pts.proximity`, `pts.neighbor` and programs that look at their neighbours.
//!
//! The grid: space is cut into cubes a little larger than the search radius ([`CELL_MARGIN`]), so
//! the points within the radius of a point lie in its own cube or one of the 26 around it. A cube
//! is found through a table of buckets: its three whole-number coordinates are hashed to one of
//! `T` buckets (`T` the power of two at or above the number of points, within
//! [`MIN_TABLE_BITS`] and [`MAX_TABLE_BITS`]), so space has no bounds and needs no box. Two cubes
//! may share a bucket: a query checks each point's own cube, so that costs time, never a wrong
//! answer.
//!
//! The build, each time anew: every point gets the key of its bucket; a radix sort (8 bits per
//! pass, stable, ranks counted within tiles of 256 points, so no step depends on which thread
//! comes first) orders the points by bucket and, within a bucket, by index; a last pass writes
//! where each bucket starts and ends and the sorted points with their positions. The same input
//! gives the same grid, bit for bit, whatever the timing of the threads. Atomics only count.
//!
//! The query, in WGSL ([`query_wgsl`]): `near_begin(p, me)` and `near_next(&n)` walk the 27 cubes
//! around `p` and give each point within the radius but the point `me` itself, nearer or farther
//! in no particular order (the same order every time). A bucket gives at most `cap` points to a
//! query, the ones of lowest index: a clump of a million points in one cube costs each query
//! `cap` looks, not a million. How many points lie past the cap of their bucket is counted
//! ([`Near::crowded`]), so a node can say that neighbours were left out.
//!
//! Points with a position that is not a finite number, and points past a count on the GPU, are in
//! no bucket: nobody finds them and they find nobody.
//!
//! Limits: positions are measured from the first point, in cubes; past about 20 000 radii from it
//! the cube of a point is no longer exact to a two-hundredth of a cube, so a pair nearer than the
//! radius by less than about half a percent may be missed, and beyond a million cubes every point
//! counts as lying in the last one (found all the same, but crowding the cap). The table has at
//! most 2^[`MAX_TABLE_BITS`] buckets: with more points than that, buckets share more cubes and
//! queries slow down. A bucket's cap is at most [`MAX_PER_CELL`].

use std::fmt::Write as _;

use penumbra_core::{Error, ParamSpec, Value};

use super::{AttrType, Points};
use crate::{Buffer, Dispatch, Gpu, Out, Step};

/// Fewest and most bits of the bucket table: 64 buckets to about four million.
pub const MIN_TABLE_BITS: u32 = 6;
pub const MAX_TABLE_BITS: u32 = 22;

/// The largest cap on the points one bucket gives a query.
pub const MAX_PER_CELL: u32 = 4096;

/// The cube's side over the radius: a little more than 1, so a pair at the radius that rounding
/// puts a hair apart still lies in neighbouring cubes.
pub const CELL_MARGIN: f32 = 1.0 + 1.0 / 256.0;

/// Words before the buckets in the cells buffer: the origin (3), one over the cube's side, the
/// radius, the mask of the table, the cap, and one unused.
const HEAD: u32 = 8;

/// Points per tile of the radix sort, one per thread.
const TILE: u32 = 256;

/// Workgroups along x of a dispatch over tiles or blocks; more take more rows.
const PER_ROW: u32 = 32768;

/// Points per row of a dispatch grid over points.
const ROW: u32 = 1 << 16;

/// Values per block of the prefix sum: 256 threads of 4.
const SCAN_BLOCK: u32 = 1024;

/// What build and query share: where a position lies in the grid.
const COMMON: &str = "
const PN_NEAR_HEAD: u32 = 8u;
// Beyond this many cubes from the origin every point counts as lying in the last one.
const PN_NEAR_REACH: f32 = 1048576.0;
const PN_NEAR_MAX_CAP: u32 = 4096u;

// A position with three finite numbers.
fn pn_near_finite(p: vec3f) -> bool {
    return all(abs(p) <= vec3f(3.0e38));
}

fn pn_near_cell_of(p: vec3f, origin: vec3f, inv: f32) -> vec3i {
    let q = clamp(floor((p - origin) * inv), vec3f(-PN_NEAR_REACH), vec3f(PN_NEAR_REACH));
    return vec3i(q);
}

fn pn_near_bucket_of(c: vec3i, mask: u32) -> u32 {
    let u = bitcast<vec3u>(c);
    var h = (u.x * 0x8da6b343u) ^ (u.y * 0xd8163841u) ^ (u.z * 0xcb1ab31fu);
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    return h & mask;
}
";

/// The keys of the points, and at the end the buckets' ranges with the sorted points.
const BUILD: &str = "
@group(2) @binding(0) var<storage, read> position: array<vec4f>;
@group(2) @binding(1) var<storage, read_write> keys: array<u32>;
@group(2) @binding(2) var<storage, read_write> vals: array<u32>;
@group(2) @binding(3) var<storage, read_write> cells: array<u32>;
@group(2) @binding(4) var<storage, read_write> sorted: array<vec4f>;
@group(2) @binding(5) var<storage, read_write> crowded: array<atomic<u32>>;
// The count where it is on the GPU, else u32::MAX.
@group(2) @binding(6) var<uniform> header: vec4u;

fn pts_n() -> u32 {
    return u32(max(params.n, 0));
}

fn pts_table() -> u32 {
    return u32(max(params.mask, 0)) + 1u;
}

fn pts_origin() -> vec3f {
    let p = position[0].xyz;
    if pn_near_finite(p) {
        return p;
    }
    return vec3f(0.0);
}

@compute @workgroup_size(64)
fn clear(@builtin(global_invocation_id) id: vec3u) {
    let j = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || j >= 2u * pts_table() {
        return;
    }
    cells[PN_NEAR_HEAD + j] = 0u;
}

@compute @workgroup_size(64)
fn key(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= pts_n() {
        return;
    }
    let origin = pts_origin();
    let inv = params.inv;
    let mask = u32(max(params.mask, 0));
    if i == 0u {
        cells[0] = bitcast<u32>(origin.x);
        cells[1] = bitcast<u32>(origin.y);
        cells[2] = bitcast<u32>(origin.z);
        cells[3] = bitcast<u32>(inv);
        cells[4] = bitcast<u32>(params.radius);
        cells[5] = mask;
        cells[6] = u32(max(params.cap, 1));
        cells[7] = 0u;
        atomicStore(&crowded[0], 0u);
    }
    var k = pts_table();
    let p = position[i].xyz;
    if i < min(header.x, pts_n()) && pn_near_finite(p) {
        k = pn_near_bucket_of(pn_near_cell_of(p, origin, inv), mask);
    }
    keys[i] = k;
}

@compute @workgroup_size(64)
fn ranges(@builtin(global_invocation_id) id: vec3u) {
    let k = id.y * pn.size.x + id.x;
    let n = pts_n();
    if id.x >= pn.size.x || k >= n {
        return;
    }
    let key = keys[k];
    let i = vals[k];
    sorted[k] = vec4f(position[i].xyz, bitcast<f32>(i));
    if key >= pts_table() {
        return;
    }
    if k == 0u || keys[k - 1u] != key {
        cells[PN_NEAR_HEAD + 2u * key] = k;
    }
    if k + 1u >= n || keys[k + 1u] != key {
        cells[PN_NEAR_HEAD + 2u * key + 1u] = k + 1u;
    }
    let cap = u32(max(params.cap, 1));
    if k >= cap && keys[k - cap] == key {
        atomicAdd(&crowded[0], 1u);
    }
}
";

fn build_params() -> [ParamSpec; 5] {
    [
        ParamSpec::int("n", 0),
        ParamSpec::int("mask", 0),
        ParamSpec::int("cap", 1),
        ParamSpec::float("inv", 1.0),
        ParamSpec::float("radius", 1.0),
    ]
}

/// One pass of the radix sort: a count of each digit per tile, then (after a prefix sum of the
/// counts, digit by digit over the tiles) each key to its place. The rank within a tile is the
/// number of the same digit before it there, so the sort is stable.
const SORT: &str = "
@group(2) @binding(0) var<storage, read> keys_in: array<u32>;
@group(2) @binding(1) var<storage, read> vals_in: array<u32>;
@group(2) @binding(2) var<storage, read_write> keys_out: array<u32>;
@group(2) @binding(3) var<storage, read_write> vals_out: array<u32>;
@group(2) @binding(4) var<storage, read_write> counts: array<u32>;

var<workgroup> pts_tally: array<atomic<u32>, 256>;
var<workgroup> pts_digits: array<u32, 256>;

fn pts_tile(w: vec3u) -> u32 {
    return w.y * u32(max(params.row, 1)) + w.x;
}

fn pts_digit(key: u32) -> u32 {
    return (key >> u32(max(params.shift, 0))) & 255u;
}

@compute @workgroup_size(256)
fn tally(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let tile = pts_tile(w);
    let tiles = u32(max(params.tiles, 0));
    let k = tile * 256u + t;
    atomicStore(&pts_tally[t], 0u);
    workgroupBarrier();
    if tile < tiles && k < u32(max(params.n, 0)) {
        atomicAdd(&pts_tally[pts_digit(keys_in[k])], 1u);
    }
    workgroupBarrier();
    if tile < tiles {
        counts[t * tiles + tile] = atomicLoad(&pts_tally[t]);
    }
}

@compute @workgroup_size(256)
fn scatter(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let tile = pts_tile(w);
    let tiles = u32(max(params.tiles, 0));
    let k = tile * 256u + t;
    let valid = tile < tiles && k < u32(max(params.n, 0));
    var key = 0u;
    var val = k;
    var d = 256u;
    if valid {
        key = keys_in[k];
        d = pts_digit(key);
        if params.first == 0 {
            val = vals_in[k];
        }
    }
    pts_digits[t] = d;
    workgroupBarrier();
    if valid {
        var rank = 0u;
        for (var s = 0u; s < t; s++) {
            if pts_digits[s] == d {
                rank += 1u;
            }
        }
        let at = counts[d * tiles + tile] + rank;
        keys_out[at] = key;
        vals_out[at] = val;
    }
}
";

fn sort_params() -> [ParamSpec; 5] {
    [
        ParamSpec::int("n", 0),
        ParamSpec::int("tiles", 0),
        ParamSpec::int("row", 1),
        ParamSpec::int("shift", 0),
        ParamSpec::int("first", 0),
    ]
}

/// An exclusive prefix sum of `u32` values in place, and their total.
const SCAN: &str = "
@group(2) @binding(0) var<storage, read_write> values: array<u32>;
@group(2) @binding(1) var<storage, read_write> sums: array<u32>;
@group(2) @binding(2) var<storage, read_write> total: array<u32>;

var<workgroup> pts_scan: array<u32, 256>;

// The sum of v over the threads before this one. Every thread of the workgroup calls it;
// afterwards pts_scan[255] holds the sum over all of them.
fn pts_exclusive(t: u32, v: u32) -> u32 {
    pts_scan[t] = v;
    workgroupBarrier();
    for (var d = 1u; d < 256u; d = d << 1u) {
        var add = 0u;
        if t >= d {
            add = pts_scan[t - d];
        }
        workgroupBarrier();
        pts_scan[t] += add;
        workgroupBarrier();
    }
    return pts_scan[t] - v;
}

fn pts_block(w: vec3u) -> u32 {
    return w.y * u32(max(params.row, 1)) + w.x;
}

fn pts_value(i: u32) -> u32 {
    if i < u32(max(params.n, 0)) {
        return values[i];
    }
    return 0u;
}

@compute @workgroup_size(256)
fn reduce(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = pts_block(w);
    let first = b * 1024u + t * 4u;
    let mine = pts_value(first) + pts_value(first + 1u) + pts_value(first + 2u) + pts_value(first + 3u);
    pts_exclusive(t, mine);
    if t == 255u && b < u32(max(params.blocks, 0)) {
        sums[b] = pts_scan[255];
    }
}

@compute @workgroup_size(256)
fn top(@builtin(local_invocation_index) t: u32) {
    let blocks = u32(max(params.blocks, 0));
    let chunk = (blocks + 255u) / 256u;
    let lo = min(t * chunk, blocks);
    let hi = min(lo + chunk, blocks);
    var mine = 0u;
    for (var j = lo; j < hi; j++) {
        mine += sums[j];
    }
    var at = pts_exclusive(t, mine);
    for (var j = lo; j < hi; j++) {
        let v = sums[j];
        sums[j] = at;
        at += v;
    }
    if t == 255u {
        total[0] = pts_scan[255];
    }
}

@compute @workgroup_size(256)
fn down(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = pts_block(w);
    let first = b * 1024u + t * 4u;
    let v = vec4u(pts_value(first), pts_value(first + 1u), pts_value(first + 2u), pts_value(first + 3u));
    let before = pts_exclusive(t, v.x + v.y + v.z + v.w);
    if b < u32(max(params.blocks, 0)) {
        var at = sums[b] + before;
        let n = u32(max(params.n, 0));
        for (var j = 0u; j < 4u; j++) {
            if first + j < n {
                values[first + j] = at;
                at += v[j];
            }
        }
    }
}
";

fn scan_params() -> [ParamSpec; 3] {
    [ParamSpec::int("n", 0), ParamSpec::int("blocks", 0), ParamSpec::int("row", 1)]
}

/// The WGSL a program includes to query a grid: the grid's two buffers at bindings `first` and
/// `first + 1` of `@group(2)` (bind [`Near::cells`] and [`Near::points`] there), and
///
/// ```wgsl
/// struct Near { index: u32, position: vec3f, distance: f32, ... }
/// fn near_begin(p: vec3f, me: u32) -> Near   // a walk over the points near p, without point me
/// fn near_next(n: ptr<function, Near>) -> bool  // the next one: false when there is none
/// fn near_radius() -> f32                    // the radius the grid was built for
/// ```
///
/// `pn_near_points[k]` is the k-th point in the grid's order (position in xyz, its index as bits
/// in w): a pass over the points in that order reads neighbours that lie close in memory.
///
/// ```wgsl
/// var n = near_begin(position, index);
/// while near_next(&n) {
///     // n.index, n.position, n.distance: a point within the radius
/// }
/// ```
///
/// The walk visits at most 27 cubes of at most the grid's cap each, so it ends whatever the
/// buffers hold.
pub fn query_wgsl(first: u32) -> String {
    let mut s = String::from(COMMON);
    let _ = writeln!(s, "@group(2) @binding({first}) var<storage, read> pn_near_cells: array<u32>;");
    let _ = writeln!(s, "@group(2) @binding({}) var<storage, read> pn_near_points: array<vec4f>;", first + 1);
    s.push_str(QUERY);
    s
}

const QUERY: &str = "
struct Near {
    index: u32,
    position: vec3f,
    distance: f32,
    pn_at: vec3f,
    pn_me: u32,
    pn_home: vec3i,
    pn_target: vec3i,
    pn_cell: u32,
    pn_slot: u32,
    pn_end: u32,
}

fn near_radius() -> f32 {
    return bitcast<f32>(pn_near_cells[4]);
}

fn pn_near_cell(p: vec3f) -> vec3i {
    let origin = vec3f(
        bitcast<f32>(pn_near_cells[0]),
        bitcast<f32>(pn_near_cells[1]),
        bitcast<f32>(pn_near_cells[2]),
    );
    return pn_near_cell_of(p, origin, bitcast<f32>(pn_near_cells[3]));
}

fn near_begin(p: vec3f, me: u32) -> Near {
    var n: Near;
    n.index = 0xffffffffu;
    n.position = p;
    n.distance = 0.0;
    n.pn_at = p;
    n.pn_me = me;
    n.pn_home = vec3i(0);
    n.pn_target = vec3i(0);
    n.pn_cell = 27u;
    n.pn_slot = 0u;
    n.pn_end = 0u;
    if pn_near_finite(p) {
        n.pn_home = pn_near_cell(p);
        n.pn_cell = 0u;
    }
    return n;
}

fn near_next(n: ptr<function, Near>) -> bool {
    let radius = near_radius();
    let r2 = radius * radius;
    let mask = pn_near_cells[5];
    let cap = clamp(pn_near_cells[6], 1u, PN_NEAR_MAX_CAP);
    var found = false;
    loop {
        if (*n).pn_slot >= (*n).pn_end {
            if (*n).pn_cell >= 27u {
                break;
            }
            let c = (*n).pn_cell;
            let d = vec3i(i32(c % 3u) - 1, i32((c / 3u) % 3u) - 1, i32(c / 9u) - 1);
            (*n).pn_target = (*n).pn_home + d;
            let h = pn_near_bucket_of((*n).pn_target, mask);
            let first = pn_near_cells[PN_NEAR_HEAD + 2u * h];
            let end = pn_near_cells[PN_NEAR_HEAD + 2u * h + 1u];
            (*n).pn_slot = first;
            (*n).pn_end = select(first, min(end, first + cap), end > first);
            (*n).pn_cell = c + 1u;
            continue;
        }
        let q = pn_near_points[(*n).pn_slot];
        (*n).pn_slot += 1u;
        let j = bitcast<u32>(q.w);
        if j == (*n).pn_me {
            continue;
        }
        let v = q.xyz - (*n).pn_at;
        let d2 = dot(v, v);
        if !(d2 <= r2) {
            continue;
        }
        if any(pn_near_cell(q.xyz) != (*n).pn_target) {
            continue;
        }
        (*n).index = j;
        (*n).position = q.xyz;
        (*n).distance = sqrt(d2);
        found = true;
        break;
    }
    return found;
}
";

/// A grid built over a set of points: what a query binds, and what the build found.
#[derive(Clone)]
pub struct Near {
    cells: Buffer,
    points: Buffer,
    crowded: Buffer,
    at_most: u32,
    radius: f32,
    cap: u32,
}

impl Near {
    /// The buckets: bind at the first binding [`query_wgsl`] was given.
    pub fn cells(&self) -> &Buffer {
        &self.cells
    }

    /// The points sorted by bucket: position in xyz, the index (as bits) in w. As many as the
    /// points may be; past the live ones and the ones without a finite position they belong to
    /// no bucket.
    pub fn points(&self) -> &Buffer {
        &self.points
    }

    /// One `u32`: how many points lie past the cap of their bucket, so no query sees them.
    pub fn crowded(&self) -> &Buffer {
        &self.crowded
    }

    /// The number of entries in [`Near::points`]: the input's count at most.
    pub fn at_most(&self) -> u32 {
        self.at_most
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }
}

/// What a node needs to search near points: the buffers of the build, kept across cooks.
#[derive(Default)]
pub struct NearGrid {
    keys: [Option<Buffer>; 2],
    vals: [Option<Buffer>; 2],
    counts: Option<Buffer>,
    sums: Option<Buffer>,
    total: Option<Buffer>,
    cells: Option<Buffer>,
    points: Option<Buffer>,
    crowded: Option<Buffer>,
    dummy: Option<Buffer>,
}

/// Compiles the build's passes, so the first search does not wait for them.
pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    gpu.cached_compute("pts near build", &build_source(), &build_params())?;
    gpu.cached_compute("pts near sort", SORT, &sort_params())?;
    gpu.cached_compute("pts scan", SCAN, &scan_params())?;
    Ok(())
}

fn build_source() -> String {
    format!("{COMMON}\n{BUILD}")
}

/// The bits of the table for `n` points.
pub fn table_bits(n: u32) -> u32 {
    let bits = 32 - n.saturating_sub(1).leading_zeros();
    bits.clamp(MIN_TABLE_BITS, MAX_TABLE_BITS)
}

impl NearGrid {
    pub fn new() -> Self {
        Self::default()
    }

    /// The grid over `points` for a search within `radius` (metres), a bucket giving a query at
    /// most `cap` points (1 to [`MAX_PER_CELL`]). `None` without points. `Err` for points without
    /// `position`, a radius that is not a positive finite number, and anything the GPU refuses.
    pub fn build(
        &mut self,
        gpu: &Gpu,
        points: &Points,
        radius: f32,
        cap: u32,
    ) -> Result<Option<Near>, Error> {
        if !(radius.is_finite() && radius > 0.0) {
            return Err(Error::new(format!("the search radius must be a positive number, not {radius}")));
        }
        let n = points.count().at_most();
        if n == 0 {
            return Ok(None);
        }
        let Some(position) = points.attr("position") else {
            return Err(Error::new("the points have no `position` to search by"));
        };
        if position.ty() != AttrType::Vec3 {
            return Err(Error::new("`position` must be three numbers (vec3)"));
        }
        let cap = cap.clamp(1, MAX_PER_CELL);
        let bits = table_bits(n);
        let table = 1u32 << bits;
        let words = u64::from(n) * 4;
        let tiles = n.div_ceil(TILE);
        let keys = [
            sized(gpu, &mut self.keys[0], words, "pts near keys")?,
            sized(gpu, &mut self.keys[1], words, "pts near keys")?,
        ];
        let vals = [
            sized(gpu, &mut self.vals[0], words, "pts near order")?,
            sized(gpu, &mut self.vals[1], words, "pts near order")?,
        ];
        let counts = sized(gpu, &mut self.counts, u64::from(tiles) * 256 * 4, "pts near counts")?;
        let cells =
            sized(gpu, &mut self.cells, (u64::from(HEAD) + 2 * u64::from(table)) * 4, "pts near cells")?;
        let sorted = sized(gpu, &mut self.points, u64::from(n) * 16, "pts near points")?;
        let crowded = sized(gpu, &mut self.crowded, 16, "pts near crowded")?;
        let dummy = sized(gpu, &mut self.dummy, 16, "pts near none")?;
        let header = points.count().header_for(gpu)?;
        let inv = 1.0 / (radius * CELL_MARGIN);
        if !inv.is_finite() {
            return Err(Error::new(format!("the search radius {radius} is too small")));
        }

        let build = gpu.cached_compute("pts near build", &build_source(), &build_params())?;
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "mask" => Some(Value::Int(i64::from(table - 1))),
            "cap" => Some(Value::Int(i64::from(cap))),
            "inv" => Some(Value::Float(f64::from(inv))),
            "radius" => Some(Value::Float(f64::from(radius))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        build.run_with(
            gpu,
            &[position.buffer(), &keys[0], &vals[0], &cells, &sorted, &crowded, &header],
            &[
                Step { entry: "clear", out: grid(2 * table), input: &input },
                Step { entry: "key", out: grid(n), input: &input },
            ],
        )?;

        // Keys hold bits + 1 bits: the last value says "in no bucket".
        let passes = (bits + 1).div_ceil(8);
        let sort = gpu.cached_compute("pts near sort", SORT, &sort_params())?;
        let row = tiles.min(PER_ROW);
        let over_tiles = Out::Size([row * TILE, tiles.div_ceil(row), 1]);
        for pass in 0..passes {
            let [ka, kb] = &keys;
            let [va, vb] = &vals;
            let (keys_in, vals_from, keys_to, vals_to) =
                if pass.is_multiple_of(2) { (ka, va, kb, vb) } else { (kb, vb, ka, va) };
            let vals_in = if pass == 0 { &dummy } else { vals_from };
            let param = |name: &str| match name {
                "n" => Some(Value::Int(i64::from(n))),
                "tiles" => Some(Value::Int(i64::from(tiles))),
                "row" => Some(Value::Int(i64::from(row))),
                "shift" => Some(Value::Int(i64::from(pass * 8))),
                "first" => Some(Value::Int(i64::from(pass == 0))),
                _ => None,
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            let buffers = [keys_in, vals_in, keys_to, vals_to, &counts];
            sort.run_with(gpu, &buffers, &[Step { entry: "tally", out: over_tiles, input: &input }])?;
            self.scan(gpu, &counts, tiles * 256)?;
            sort.run_with(gpu, &buffers, &[Step { entry: "scatter", out: over_tiles, input: &input }])?;
        }
        let [ka, kb] = &keys;
        let [va, vb] = &vals;
        let (keys_last, vals_last) = if passes.is_multiple_of(2) { (ka, va) } else { (kb, vb) };
        build.run_with(
            gpu,
            &[position.buffer(), keys_last, vals_last, &cells, &sorted, &crowded, &header],
            &[Step { entry: "ranges", out: grid(n), input: &input }],
        )?;
        Ok(Some(Near { cells, points: sorted, crowded, at_most: n, radius, cap }))
    }

    /// An exclusive prefix sum over the first `n` values of `values`, in place; the total goes
    /// to [`NearGrid::total`].
    pub fn scan(&mut self, gpu: &Gpu, values: &Buffer, n: u32) -> Result<(), Error> {
        if n == 0 {
            return Ok(());
        }
        if values.bytes() < u64::from(n) * 4 {
            return Err(Error::new("the values to sum are fewer than asked for"));
        }
        let blocks = n.div_ceil(SCAN_BLOCK);
        let sums = grown(gpu, &mut self.sums, u64::from(blocks) * 4, "pts scan sums")?;
        let total = sized(gpu, &mut self.total, 16, "pts scan total")?;
        let row = blocks.min(PER_ROW);
        let scan = gpu.cached_compute("pts scan", SCAN, &scan_params())?;
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "blocks" => Some(Value::Int(i64::from(blocks))),
            "row" => Some(Value::Int(i64::from(row))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let over_blocks = Out::Size([row * 256, blocks.div_ceil(row), 1]);
        scan.run_with(
            gpu,
            &[values, &sums, &total],
            &[
                Step { entry: "reduce", out: over_blocks, input: &input },
                Step { entry: "top", out: Out::range(256), input: &input },
                Step { entry: "down", out: over_blocks, input: &input },
            ],
        )
    }

    /// One `u32`: the total of the last [`NearGrid::scan`].
    pub fn total(&self) -> Option<&Buffer> {
        self.total.as_ref()
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        let all =
            [&self.counts, &self.sums, &self.total, &self.cells, &self.points, &self.crowded, &self.dummy];
        let pairs = self.keys.iter().chain(&self.vals);
        all.into_iter().chain(pairs).filter_map(|b| b.as_ref()).map(Buffer::bytes).sum()
    }

    /// Lets go of every buffer: the next build makes them again.
    pub fn trim(&mut self) {
        *self = Self::default();
    }
}

/// The dispatch grid over `n` elements: rows of [`ROW`].
pub(crate) fn grid(n: u32) -> Out<'static> {
    let w = n.clamp(1, ROW);
    Out::grid(w, n.div_ceil(w))
}

/// `slot`'s buffer when it has `bytes`, else a new one of that size in its place.
pub(crate) fn sized(gpu: &Gpu, slot: &mut Option<Buffer>, bytes: u64, label: &str) -> Result<Buffer, Error> {
    if let Some(b) = slot.as_ref().filter(|b| b.bytes() == bytes) {
        return Ok(b.clone());
    }
    let b = Buffer::new(gpu, bytes.max(4), label)?;
    *slot = Some(b.clone());
    Ok(b)
}

/// `slot`'s buffer when it holds at least `bytes`, else a new one of that size: for buffers bound
/// as arrays read only as far as the values say.
fn grown(gpu: &Gpu, slot: &mut Option<Buffer>, bytes: u64, label: &str) -> Result<Buffer, Error> {
    if let Some(b) = slot.as_ref().filter(|b| b.bytes() >= bytes) {
        return Ok(b.clone());
    }
    let b = Buffer::new(gpu, bytes.max(4), label)?;
    *slot = Some(b.clone());
    Ok(b)
}

#[cfg(test)]
mod tests;
