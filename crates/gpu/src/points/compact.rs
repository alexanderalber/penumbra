//! Dropping points on the GPU while keeping the order of the rest ([`Compactor`]).
//!
//! A node that deletes points writes one flag per point (a `u32`, 0 drops the point) and hands
//! it to [`Compactor::compact`]. The kept points move to the front of new buffers, in the order
//! they had, with every attribute; the new count stays on the GPU ([`Count::Gpu`]). Keeping the
//! order matters: an append with atomics would shuffle the points from frame to frame, and
//! trails, ids and comparison pictures would jump.
//!
//! How: a prefix sum over the flags, in three dispatches of one pass. Blocks of
//! [`BLOCK`] points each count their kept points; one workgroup turns the block counts into
//! where each block's points start and writes the total into the new header; each block then
//! writes, for every kept point, its old index at its new place (a map). Then every attribute is
//! gathered through the map: as many attributes per dispatch as the GPU binds storage buffers
//! for, as many dispatches as it takes, so no attribute is ever left out.
//!
//! What the result keeps: the capacity (the buffers are as large as the input's), the bounds
//! (the kept points lie inside the old box, so it stays a sound box, if a loose one). It drops
//! `dims` and `strips`: the rows and lines no longer hold. Elements past the new count hold
//! whatever the buffers held before.

use std::fmt::Write as _;

use penumbra_core::{Error, ParamSpec, Value};

use super::program::storage_budget;
use super::{Attr, Count, HEADER_BYTES, Points, Seen};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// Points per block of the prefix sum: 256 threads of 4 points each.
pub const BLOCK: u32 = 1024;

/// Workgroups along x of a dispatch over blocks; more blocks take more rows.
const BLOCKS_PER_ROW: u32 = 32768;

/// Points per row of the gather's dispatch grid.
const ROW: u32 = 1 << 16;

/// The prefix sum: count per block, scan of the block counts, the map from new place to old.
const SCAN: &str = "
@group(2) @binding(0) var<storage, read> keep: array<u32>;
@group(2) @binding(1) var<storage, read_write> sums: array<u32>;
@group(2) @binding(2) var<storage, read_write> map: array<u32>;
@group(2) @binding(3) var<storage, read_write> header: array<u32>;
// The input's count where it is on the GPU, else u32::MAX.
@group(2) @binding(4) var<uniform> before: vec4u;

var<workgroup> pts_scan: array<u32, 256>;

// Whether point i is kept: a point of the input whose flag is not 0.
fn pts_kept(i: u32) -> u32 {
    let n = min(before.x, u32(max(params.n, 0)));
    if i < n && keep[i] != 0u {
        return 1u;
    }
    return 0u;
}

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

@compute @workgroup_size(256)
fn count(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = pts_block(w);
    let first = b * 1024u + t * 4u;
    let mine = pts_kept(first) + pts_kept(first + 1u) + pts_kept(first + 2u) + pts_kept(first + 3u);
    pts_exclusive(t, mine);
    if t == 255u && b < u32(max(params.blocks, 0)) {
        sums[b] = pts_scan[255];
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
        mine += sums[j];
    }
    var at = pts_exclusive(t, mine);
    for (var j = lo; j < hi; j++) {
        let v = sums[j];
        sums[j] = at;
        at += v;
    }
    if t == 255u {
        header[0] = pts_scan[255];
        header[1] = 0u;
        header[2] = 0u;
        header[3] = 0u;
    }
}

@compute @workgroup_size(256)
fn place(@builtin(workgroup_id) w: vec3u, @builtin(local_invocation_index) t: u32) {
    let b = pts_block(w);
    let first = b * 1024u + t * 4u;
    let k = vec4u(pts_kept(first), pts_kept(first + 1u), pts_kept(first + 2u), pts_kept(first + 3u));
    let before_me = pts_exclusive(t, k.x + k.y + k.z + k.w);
    if b < u32(max(params.blocks, 0)) {
        var at = sums[b] + before_me;
        for (var j = 0u; j < 4u; j++) {
            if k[j] != 0u {
                map[at] = first + j;
                at += 1u;
            }
        }
    }
}
";

fn scan_params() -> [ParamSpec; 3] {
    [ParamSpec::int("n", 0), ParamSpec::int("blocks", 0), ParamSpec::int("row", 1)]
}

/// The gather of `k` attributes: each output element `j` below the new count takes the input
/// element `map[j]`, `params.s<a>` words each.
pub(super) fn gather_source(k: usize) -> String {
    let mut s = String::from(
        "@group(2) @binding(0) var<storage, read> map: array<u32>;\n\
         @group(2) @binding(1) var<uniform> header: vec4u;\n",
    );
    for a in 0..k {
        let _ = writeln!(s, "@group(2) @binding({}) var<storage, read> in{a}: array<u32>;", 2 + 2 * a);
        let _ = writeln!(s, "@group(2) @binding({}) var<storage, read_write> out{a}: array<u32>;", 3 + 2 * a);
    }
    s.push_str(
        "\n@compute @workgroup_size(64)\n\
         fn gather(@builtin(global_invocation_id) id: vec3u) {\n\
         \x20   let j = id.y * pn.size.x + id.x;\n\
         \x20   if id.x >= pn.size.x || j >= min(header.x, u32(max(params.n, 0))) {\n\
         \x20       return;\n\
         \x20   }\n\
         \x20   let i = map[j];\n",
    );
    for a in 0..k {
        let _ = writeln!(
            s,
            "    let s{a} = u32(max(params.s{a}, 1));\n    for (var w = 0u; w < s{a}; w++) {{\n        out{a}[j * s{a} + w] = in{a}[i * s{a} + w];\n    }}"
        );
    }
    s.push_str("}\n");
    s
}

pub(super) fn gather_params(k: usize) -> Vec<ParamSpec> {
    let mut p = vec![ParamSpec::int("n", 0)];
    p.extend((0..k).map(|a| ParamSpec::int(&format!("s{a}"), 1)));
    p
}

/// Most attributes one gather moves on `gpu`: two storage buffers each, besides the map.
pub fn attrs_per_pass(gpu: &Gpu) -> usize {
    (storage_budget(gpu).saturating_sub(1) / 2).max(1) as usize
}

/// What one node needs to drop points: the buffers it writes, kept across cooks.
#[derive(Default)]
pub struct Compactor {
    map: Option<Buffer>,
    sums: Option<Buffer>,
    headers: BufferRing,
    rings: Vec<(String, BufferRing)>,
    /// The count of the last compaction, which the next one's display follows.
    last: Option<Seen>,
    /// No readback of the count: the caller changes the count again and reads it itself.
    quiet: bool,
}

impl Compactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// A compactor that starts no readback of the count it makes ([`Count::seen`] stays
    /// `None`): for a node that changes the count again in the same step and reads that one.
    pub fn quiet() -> Self {
        Self { quiet: true, ..Self::default() }
    }

    /// The points of `points` whose flag in `keep` is not 0, in their order, with every
    /// attribute, in new buffers of the same capacity; their count on the GPU, read back for
    /// display (unless [`Compactor::quiet`]). `keep` holds a `u32` per point, at least as many as the count may be. Points
    /// past the input's count are never kept, also where that count is on the GPU. Bounds stay,
    /// `dims` and `strips` go. `Err` for a `keep` too short and for anything the GPU refuses.
    pub fn compact(&mut self, gpu: &Gpu, points: &Points, keep: &Buffer) -> Result<Points, Error> {
        let n = points.count().at_most();
        if n == 0 {
            return Ok(points.clone());
        }
        if keep.bytes() < u64::from(n) * 4 {
            return Err(Error::new(format!(
                "the flags hold {} points, fewer than the {n} the points may have",
                keep.bytes() / 4
            )));
        }
        let capacity = points.capacity().max(n);
        let blocks = n.div_ceil(BLOCK);
        let map = sized(gpu, &mut self.map, u64::from(capacity) * 4, "pts compact map")?;
        // Sized by the capacity, so a bound that moves keeps the buffer.
        let sums = sized(gpu, &mut self.sums, u64::from(capacity.div_ceil(BLOCK)) * 4, "pts compact sums")?;
        let header = self.headers.next(gpu, HEADER_BYTES, "pts count")?;
        let before = points.count().header_for(gpu)?;
        let row = blocks.min(BLOCKS_PER_ROW);
        let rows = blocks.div_ceil(row);
        let scan = gpu.cached_compute("pts compact", SCAN, &scan_params())?;
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "blocks" => Some(Value::Int(i64::from(blocks))),
            "row" => Some(Value::Int(i64::from(row))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let over_blocks = Out::Size([row * 256, rows, 1]);
        scan.run_with(
            gpu,
            &[keep, &sums, &map, &header, &before],
            &[
                Step { entry: "count", out: over_blocks, input: &input },
                Step { entry: "scan", out: Out::range(256), input: &input },
                Step { entry: "place", out: over_blocks, input: &input },
            ],
        )?;

        let attrs = points.attrs();
        self.rings.retain(|(name, _)| attrs.iter().any(|a| a.name() == name));
        let mut moved: Vec<Attr> = Vec::with_capacity(attrs.len());
        let per = attrs_per_pass(gpu);
        let (w, h) = (n.clamp(1, ROW), n.div_ceil(n.clamp(1, ROW)));
        for group in attrs.chunks(per) {
            let mut outs = Vec::with_capacity(group.len());
            for a in group {
                let bytes = u64::from(capacity) * a.ty().stride();
                if !self.rings.iter().any(|(name, _)| name == a.name()) {
                    self.rings.push((a.name().to_string(), BufferRing::new()));
                }
                let Some((_, ring)) = self.rings.iter_mut().find(|(name, _)| name == a.name()) else {
                    return Err(Error::new(format!("no buffers for `{}`", a.name())));
                };
                outs.push(ring.next(gpu, bytes, &format!("pts {}", a.name()))?);
            }
            let program =
                gpu.cached_compute("pts gather", &gather_source(group.len()), &gather_params(group.len()))?;
            let mut buffers: Vec<&Buffer> = vec![&map, &header];
            for (a, out) in group.iter().zip(&outs) {
                buffers.push(a.buffer());
                buffers.push(out);
            }
            let strides: Vec<i64> = group.iter().map(|a| (a.ty().stride() / 4) as i64).collect();
            let param = |name: &str| match name {
                "n" => Some(Value::Int(i64::from(n))),
                s => s
                    .strip_prefix('s')
                    .and_then(|k| k.parse::<usize>().ok())
                    .and_then(|k| strides.get(k))
                    .map(|v| Value::Int(*v)),
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            program.run_with(
                gpu,
                &buffers,
                &[Step { entry: "gather", out: Out::grid(w, h), input: &input }],
            )?;
            for (a, out) in group.iter().zip(outs) {
                moved.push(Attr::new(a.name(), a.ty(), out)?);
            }
        }
        // The readback is for display only: one that cannot start leaves the count unseen.
        let seen = if self.quiet {
            Seen::default()
        } else {
            Seen::reading(gpu, &header, self.last.as_ref()).unwrap_or_default()
        };
        self.last = Some(seen.clone());
        let count = Count::gpu(header, n, seen)?;
        Points::new(capacity, 0, moved)?.with_bounds(points.bounds()).with_count(count)
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        let own = [&self.map, &self.sums].iter().filter_map(|b| b.as_ref()).map(Buffer::bytes).sum::<u64>();
        own + self.headers.bytes() + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
    }

    /// Lets go of the buffers nobody downstream holds.
    pub fn trim(&mut self) {
        self.map = None;
        self.sums = None;
        self.headers.trim();
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }
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
