//! Lines between near points on the GPU ([`Pairer`]): what `pts.proximity` draws.
//!
//! Each point chooses up to `most` of the points within `distance` of it and at least
//! `min_distance` away ([`Choose`]: the nearest, or a random choice that stays the same while
//! the pair stays near). A pair becomes a line once, whichever end chose it, unless `both`
//! keeps a line from each end that chose the other. So a point that others choose may have more
//! lines than `most`.
//!
//! What comes out: two points per line, its ends in a row ([`super::Strips::Pairs`]), with the
//! position of each end's point and the attributes asked for, and per line `span` (its length in
//! metres) and `span01` (the length over `distance`, 0 for touching points, 1 at the farthest).
//! The lines are in the order of their first point, then of the choice; their number is on the
//! GPU, at most `max`. The header's words: the ends, the lines, the points past the cap of their
//! cube ([`super::near`]), the lines wanted before `max`.
//!
//! The same points give the same lines bit for bit: the choice is a total order (rank, then
//! index), the grid visits points in a fixed order, and a prefix sum places the lines.

use penumbra_core::{Error, ParamSpec, Value};

use super::compact::{attrs_per_pass, gather_params, gather_source};
use super::near::{self, NearGrid, grid, sized};
use super::{Attr, AttrType, Count, HEADER_BYTES, Points, Seen, Strips};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// The length of a line in metres.
pub const SPAN: &str = "span";

/// The length of a line over the search distance: 0 to 1.
pub const SPAN01: &str = "span01";

/// Readbacks of the header a [`Pairer`] waits for at once.
const PENDING: usize = 8;

/// The most lines one point chooses.
pub const MAX_LINES: u32 = 32;

/// Which of its candidates a point chooses when it has more than `most`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choose {
    /// The nearest first.
    Nearest,
    /// A random rank per pair from the two points' keys (their `id`, or their index) and the
    /// seed: the same for a pair as long as both are there.
    Random { seed: u32 },
}

/// What lines to make.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The longest line, in metres: the radius of the search.
    pub distance: f32,
    /// The shortest line.
    pub min_distance: f32,
    /// Lines each point chooses, 1 to [`MAX_LINES`].
    pub most: u32,
    pub choose: Choose,
    /// Keep a line from each end that chose the other: a pair may be drawn twice.
    pub both: bool,
    /// The most lines.
    pub max: u32,
    /// The most points one cube gives a search ([`NearGrid::build`]).
    pub cap: u32,
    /// The attributes each end carries from its point besides `position`; those the points lack
    /// are left out.
    pub attrs: Vec<String>,
}

const COMMON: &str = "
fn pts_most() -> u32 {
    return u32(clamp(params.most, 1, 32));
}
";

/// Each point's choice: up to `most` candidates by rank, then index.
const CHOOSE: &str = "
@group(2) @binding(0) var<storage, read> keys: array<u32>;
@group(2) @binding(1) var<storage, read_write> chosen: array<u32>;
@group(2) @binding(2) var<storage, read_write> chosen_n: array<u32>;
@group(2) @binding(3) var<uniform> header: vec4u;

fn pts_less(r: u32, j: u32, r2: u32, j2: u32) -> bool {
    return r < r2 || (r == r2 && j < j2);
}

fn pts_key(i: u32) -> u32 {
    if params.keyed != 0 {
        return keys[i];
    }
    return i;
}

fn pts_rank(i: u32, j: u32, d: f32) -> u32 {
    if params.random != 0 {
        let a = pts_key(i);
        let b = pts_key(j);
        return pn_hash(vec3u(min(a, b), max(a, b), u32(params.seed))).x;
    }
    // A distance is not negative: its bits order as its value does.
    return bitcast<u32>(d);
}

@compute @workgroup_size(64)
fn choose(@builtin(global_invocation_id) id: vec3u) {
    let k = id.y * pn.size.x + id.x;
    let n = u32(max(params.n, 0));
    if id.x >= pn.size.x || k >= n {
        return;
    }
    let q = pn_near_points[k];
    let i = bitcast<u32>(q.w);
    if i >= min(header.x, n) {
        return;
    }
    let most = pts_most();
    var rank: array<u32, 32>;
    var who: array<u32, 32>;
    var c = 0u;
WALK
    chosen_n[i] = c;
    for (var s = 0u; s < c; s++) {
        chosen[i * most + s] = who[s];
    }
}
";

/// Whether point i makes the line to its choice j: always with `both`, else unless j chose i
/// too and comes first.
const EMITS: &str = "
fn pts_chose(j: u32, i: u32) -> bool {
    let most = pts_most();
    let c = min(chosen_n[j], most);
    var found = false;
    for (var s = 0u; s < c; s++) {
        if chosen[j * most + s] == i {
            found = true;
        }
    }
    return found;
}

fn pts_emits(i: u32, j: u32) -> bool {
    return params.both != 0 || i < j || !pts_chose(j, i);
}
";

/// The number of lines each point makes.
const COUNT: &str = "
@group(2) @binding(0) var<storage, read> chosen: array<u32>;
@group(2) @binding(1) var<storage, read> chosen_n: array<u32>;
@group(2) @binding(2) var<storage, read_write> counts: array<u32>;
@group(2) @binding(3) var<uniform> header: vec4u;

@compute @workgroup_size(64)
fn count(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    let n = u32(max(params.n, 0));
    if id.x >= pn.size.x || i >= n {
        return;
    }
    var e = 0u;
    if i < min(header.x, n) {
        let most = pts_most();
        let c = min(chosen_n[i], most);
        for (var s = 0u; s < c; s++) {
            if pts_emits(i, chosen[i * most + s]) {
                e += 1u;
            }
        }
    }
    counts[i] = e;
}
";

/// The lines: each point's at the place the prefix sum gave it, both ends.
const EMIT: &str = "
@group(2) @binding(0) var<storage, read> chosen: array<u32>;
@group(2) @binding(1) var<storage, read> chosen_n: array<u32>;
@group(2) @binding(2) var<storage, read> places: array<u32>;
@group(2) @binding(3) var<storage, read> position: array<vec4f>;
@group(2) @binding(4) var<storage, read_write> map: array<u32>;
@group(2) @binding(5) var<storage, read_write> span: array<f32>;
@group(2) @binding(6) var<storage, read_write> span01: array<f32>;
@group(2) @binding(7) var<uniform> header: vec4u;

@compute @workgroup_size(64)
fn emit(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    let n = u32(max(params.n, 0));
    if id.x >= pn.size.x || i >= min(header.x, n) {
        return;
    }
    let most = pts_most();
    let c = min(chosen_n[i], most);
    let max_lines = u32(max(params.max, 0));
    var p = places[i];
    for (var s = 0u; s < c; s++) {
        let j = chosen[i * most + s];
        if pts_emits(i, j) {
            if p < max_lines {
                let d = distance(position[i].xyz, position[j].xyz);
                let f = d / max(params.distance, 1e-30);
                map[2u * p] = i;
                map[2u * p + 1u] = j;
                span[2u * p] = d;
                span[2u * p + 1u] = d;
                span01[2u * p] = f;
                span01[2u * p + 1u] = f;
            }
            p += 1u;
        }
    }
}
";

/// The header of the lines.
const FINISH: &str = "
@group(2) @binding(0) var<storage, read> total: array<u32>;
@group(2) @binding(1) var<storage, read> crowded: array<u32>;
@group(2) @binding(2) var<storage, read_write> out: array<u32>;

@compute @workgroup_size(64)
fn finish(@builtin(global_invocation_id) id: vec3u) {
    if id.x != 0u {
        return;
    }
    let wanted = total[0];
    let lines = min(wanted, u32(max(params.max, 0)));
    out[0] = 2u * lines;
    out[1] = lines;
    out[2] = crowded[0];
    out[3] = wanted;
}
";

/// What a point does with each neighbour: keeps the best `most` by rank, then index.
const KEEP: &str = "
            if w.distance < params.min_distance {
                continue;
            }
            let j = w.index;
            let r = pts_rank(i, j, w.distance);
            if c == most && !pts_less(r, j, rank[most - 1u], who[most - 1u]) {
                continue;
            }
            var at = c;
            if c < most {
                c += 1u;
            } else {
                at = most - 1u;
            }
            loop {
                if at == 0u || !pts_less(r, j, rank[at - 1u], who[at - 1u]) {
                    break;
                }
                rank[at] = rank[at - 1u];
                who[at] = who[at - 1u];
                at -= 1u;
            }
            rank[at] = r;
            who[at] = j;
";

fn choose_source() -> String {
    let choose = CHOOSE.replace("WALK", &near::near_loop("q.xyz", "i", KEEP));
    format!("{}\n{COMMON}\n{choose}", near::query_wgsl(4))
}

fn count_source() -> String {
    format!("{COMMON}\n{COUNT}\n{EMITS}")
}

fn emit_source() -> String {
    format!("{COMMON}\n{EMIT}\n{EMITS}")
}

fn params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::int("n", 0),
        ParamSpec::int("most", 1),
        ParamSpec::int("keyed", 0),
        ParamSpec::int("random", 0),
        ParamSpec::int("seed", 0),
        ParamSpec::int("both", 0),
        ParamSpec::int("max", 0),
        ParamSpec::float("min_distance", 0.0),
        ParamSpec::float("distance", 1.0),
    ]
}

/// Compiles the passes, so the first lines do not wait for them.
pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    near::warm_up(gpu)?;
    gpu.cached_compute("pts proximity choose", &choose_source(), &params())?;
    gpu.cached_compute("pts proximity count", &count_source(), &params())?;
    gpu.cached_compute("pts proximity emit", &emit_source(), &params())?;
    gpu.cached_compute("pts proximity finish", FINISH, &params())?;
    for k in 1..=3 {
        gpu.cached_compute("pts gather", &gather_source(k), &gather_params(k))?;
    }
    Ok(())
}

/// What one node needs to make lines between near points: the buffers it writes, kept across
/// cooks.
#[derive(Default)]
pub struct Pairer {
    grid: NearGrid,
    chosen: Option<Buffer>,
    chosen_n: Option<Buffer>,
    counts: Option<Buffer>,
    map: Option<Buffer>,
    none: Option<Buffer>,
    headers: BufferRing,
    rings: Vec<(String, BufferRing)>,
    last: Option<Seen>,
    /// Readbacks not yet arrived, oldest first, at most [`PENDING`]: under load a readback can
    /// take longer than a frame, and the newest one that arrived still counts.
    pending: Vec<Seen>,
    /// The header of the newest lines read back so far.
    words: Option<[u32; 4]>,
}

impl Pairer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The lines between the near points of `points`. `Err` for points without `position`, a
    /// distance that is not a positive number, more line ends than a buffer holds, and anything
    /// the GPU refuses.
    pub fn pairs(&mut self, gpu: &Gpu, points: &Points, set: &Settings) -> Result<Points, Error> {
        self.settle();
        let n = points.count().at_most();
        let most = set.most.clamp(1, MAX_LINES);
        let carried = carried(points, set)?;
        let ends_most = u64::from(super::most_elements(gpu, AttrType::Vec4));
        let lines_cap = (u64::from(n) * u64::from(most)).min(u64::from(set.max)).min(ends_most / 2);
        let lines_cap = u32::try_from(lines_cap).unwrap_or(0);
        let capacity = (2 * lines_cap).max(2);
        let near = self.grid.build(gpu, points, set.distance, set.cap)?;
        let header = self.headers.next(gpu, HEADER_BYTES, "pts count")?;
        let span = self.next(gpu, SPAN, u64::from(capacity) * 4)?;
        let span01 = self.next(gpu, SPAN01, u64::from(capacity) * 4)?;
        let mut out = Vec::with_capacity(carried.len() + 2);
        let Some(near) = near.filter(|_| lines_cap > 0) else {
            // No points: no lines, with every attribute all the same.
            for (name, ty) in &carried {
                let b = self.next(gpu, name, u64::from(capacity) * ty.stride())?;
                out.push(Attr::new(name, *ty, b)?);
            }
            out.push(Attr::new(SPAN, AttrType::F32, span)?);
            out.push(Attr::new(SPAN01, AttrType::F32, span01)?);
            return Ok(Points::new(capacity, 0, out)?
                .with_bounds(points.bounds())
                .with_strips(Strips::Pairs));
        };
        let position = points.attr("position").ok_or_else(|| Error::new("the points have no `position`"))?;
        let chosen =
            sized(gpu, &mut self.chosen, u64::from(n) * u64::from(most) * 4, "pts proximity chosen")?;
        let chosen_n = sized(gpu, &mut self.chosen_n, u64::from(n) * 4, "pts proximity chosen count")?;
        let counts = sized(gpu, &mut self.counts, u64::from(n) * 4, "pts proximity counts")?;
        let map = sized(gpu, &mut self.map, u64::from(capacity) * 4, "pts proximity ends")?;
        let none = sized(gpu, &mut self.none, 16, "pts proximity no keys")?;
        let count_now = points.count().header_for(gpu)?;
        let keys = points.attr("id").filter(|a| matches!(a.ty(), AttrType::U32 | AttrType::I32));
        let (random, seed) = match set.choose {
            Choose::Nearest => (0, 0),
            Choose::Random { seed } => (1, seed),
        };
        let param = |name: &str| match name {
            "n" => Some(Value::Int(i64::from(n))),
            "most" => Some(Value::Int(i64::from(most))),
            "keyed" => Some(Value::Int(i64::from(keys.is_some()))),
            "random" => Some(Value::Int(random)),
            "seed" => Some(Value::Int(i64::from(seed & 0x7fff_ffff))),
            "both" => Some(Value::Int(i64::from(set.both))),
            "max" => Some(Value::Int(i64::from(lines_cap))),
            "min_distance" => Some(Value::Float(f64::from(set.min_distance))),
            "distance" => Some(Value::Float(f64::from(set.distance))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let step = |entry| [Step { entry, out: grid(n), input: &input }];
        let key_buffer = keys.map_or(&none, |a| a.buffer());
        gpu.cached_compute("pts proximity choose", &choose_source(), &params())?.run_with(
            gpu,
            &[key_buffer, &chosen, &chosen_n, &count_now, near.cells(), near.points()],
            &step("choose"),
        )?;
        gpu.cached_compute("pts proximity count", &count_source(), &params())?.run_with(
            gpu,
            &[&chosen, &chosen_n, &counts, &count_now],
            &step("count"),
        )?;
        self.grid.scan(gpu, &counts, n)?;
        gpu.cached_compute("pts proximity emit", &emit_source(), &params())?.run_with(
            gpu,
            &[&chosen, &chosen_n, &counts, position.buffer(), &map, &span, &span01, &count_now],
            &step("emit"),
        )?;
        let total =
            self.grid.total().cloned().ok_or_else(|| Error::new("the sum of the lines is missing"))?;
        gpu.cached_compute("pts proximity finish", FINISH, &params())?.run_with(
            gpu,
            &[&total, near.crowded(), &header],
            &[Step { entry: "finish", out: Out::range(1), input: &input }],
        )?;
        self.gather(gpu, &carried, points, &map, &header, 2 * lines_cap, &mut out)?;
        out.push(Attr::new(SPAN, AttrType::F32, span)?);
        out.push(Attr::new(SPAN01, AttrType::F32, span01)?);
        // For display, and for what the node says about crowded cubes and lines left out.
        let seen = Seen::reading(gpu, &header, self.last.as_ref()).unwrap_or_default();
        self.last = Some(seen.clone());
        self.pending.push(seen.clone());
        if self.pending.len() > PENDING {
            self.pending.remove(0);
        }
        let count = Count::gpu(header, 2 * lines_cap, seen)?;
        Ok(Points::new(capacity, 0, out)?
            .with_bounds(points.bounds())
            .with_count(count)?
            .with_strips(Strips::Pairs))
    }

    /// The header of the newest lines read back so far, a frame or more late: the line ends, the
    /// lines, the points past the cap of their cube, the lines wanted before `max`. `None` before
    /// the first arrives.
    pub fn words(&mut self) -> Option<[u32; 4]> {
        self.settle();
        self.words
    }

    /// Takes in the readbacks that arrived; the newest of them gives [`Pairer::words`].
    fn settle(&mut self) {
        let mut newest = None;
        self.pending.retain(|s| match s.arrived_words() {
            Some(w) => {
                newest = Some(w);
                false
            }
            None => true,
        });
        if newest.is_some() {
            self.words = newest;
        }
    }

    /// The attributes `carried` of each end's point, through `map`, into `out`.
    #[allow(clippy::too_many_arguments)]
    fn gather(
        &mut self,
        gpu: &Gpu,
        carried: &[(String, AttrType)],
        points: &Points,
        map: &Buffer,
        header: &Buffer,
        ends: u32,
        out: &mut Vec<Attr>,
    ) -> Result<(), Error> {
        let capacity = u64::from(ends.max(2));
        let (w, h) = (ends.clamp(1, 1 << 16), ends.div_ceil(ends.clamp(1, 1 << 16)));
        for group in carried.chunks(attrs_per_pass(gpu)) {
            let mut buffers: Vec<Buffer> = vec![map.clone(), header.clone()];
            let mut strides = Vec::with_capacity(group.len());
            for (name, ty) in group {
                let src = points.attr(name).ok_or_else(|| Error::new(format!("the points lost `{name}`")))?;
                let dst = self.next(gpu, name, capacity * ty.stride())?;
                buffers.push(src.buffer().clone());
                buffers.push(dst.clone());
                strides.push((ty.stride() / 4) as i64);
                out.push(Attr::new(name, *ty, dst)?);
            }
            if ends == 0 {
                continue;
            }
            let program =
                gpu.cached_compute("pts gather", &gather_source(group.len()), &gather_params(group.len()))?;
            let param = |name: &str| match name {
                "n" => Some(Value::Int(i64::from(ends))),
                s => s
                    .strip_prefix('s')
                    .and_then(|k| k.parse::<usize>().ok())
                    .and_then(|k| strides.get(k))
                    .map(|v| Value::Int(*v)),
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            let refs: Vec<&Buffer> = buffers.iter().collect();
            program.run_with(gpu, &refs, &[Step { entry: "gather", out: Out::grid(w, h), input: &input }])?;
        }
        Ok(())
    }

    /// A buffer of `bytes` for the attribute `name` that nobody else holds.
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
        let own = [&self.chosen, &self.chosen_n, &self.counts, &self.map, &self.none]
            .iter()
            .filter_map(|b| b.as_ref())
            .map(Buffer::bytes)
            .sum::<u64>();
        own + self.grid.bytes()
            + self.headers.bytes()
            + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
    }

    /// Lets go of the buffers nobody downstream holds, and of the search's.
    pub fn trim(&mut self) {
        self.grid.trim();
        self.chosen = None;
        self.chosen_n = None;
        self.counts = None;
        self.map = None;
        self.headers.trim();
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }
}

/// `position` and the attributes asked for that the points have, each once, with their types;
/// never `span` or `span01`, which the lines make themselves.
fn carried(points: &Points, set: &Settings) -> Result<Vec<(String, AttrType)>, Error> {
    let Some(p) = points.attr("position") else {
        return Err(Error::new("the points have no `position` to join"));
    };
    let mut out = vec![(p.name().to_string(), p.ty())];
    for name in &set.attrs {
        let Some(a) = points.attr(name) else { continue };
        if [SPAN, SPAN01].contains(&a.name()) || out.iter().any(|(n, _)| n == a.name()) {
            continue;
        }
        out.push((a.name().to_string(), a.ty()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
