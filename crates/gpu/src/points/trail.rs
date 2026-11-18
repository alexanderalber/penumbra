//! Trails of points on the GPU ([`Trailer`]): for each point, a line of the places it had.
//!
//! Each step the trailer takes the points now and the trails it made a step before. Point `i`
//! gets a line of `len` points: its own place first (the head), then the places of the trail it
//! had, each one step older, the oldest last (the tail). Which old trail is the point's: the
//! one with the same key (an attribute such as `id`), or the one at the same index where the
//! points have no key. A point without an old trail (born since) starts with every place its
//! own, so a new trail grows from where the point is, never from the origin or from another
//! point's place. A trail whose point is gone is not carried on.
//!
//! The lookup by key first tries the same index, then a binary search over the old trails: it
//! finds every key where the keys rise along the list, as `pts.particles` gives its ids, and where
//! each point keeps its index.
//!
//! A step that does not `shift` moves only the heads to the points' places and keeps the rest:
//! a trail that samples every few steps, or a change while time stands.
//!
//! The work: one pass pairs each point with its old trail and writes the new count (points times
//! `len`, at most `max` trails); then one pass per attribute copies every element of the new
//! trails, from the points or from the old trails.

use penumbra_core::{Error, ParamSpec, Value};

use super::{Attr, AttrType, Count, HEADER_BYTES, Points, Seen, Strips, most_elements};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// Points per row of a dispatch grid.
const ROW: u32 = 1 << 16;

/// The attribute along a trail: 0 at the head, 1 at the tail.
pub const ALONG: &str = "trail";

/// Pairs each point with its old trail, and writes the count of the new trails.
const PAIR: &str = "
@group(2) @binding(0) var<storage, read> keys_now: array<u32>;
@group(2) @binding(1) var<storage, read> keys_before: array<u32>;
@group(2) @binding(2) var<storage, read_write> pairs: array<u32>;
@group(2) @binding(3) var<storage, read_write> header: array<u32>;
// The counts where they are on the GPU, else u32::MAX.
@group(2) @binding(4) var<uniform> count_now: vec4u;
@group(2) @binding(5) var<uniform> count_before: vec4u;

const NONE: u32 = 0xffffffffu;

fn pts_len() -> u32 {
    return u32(max(params.len, 1));
}

// Trails now: one per point, at most `max`.
fn pts_trails() -> u32 {
    return min(min(count_now.x, u32(max(params.now, 0))), u32(max(params.max, 0)));
}

fn pts_trails_before() -> u32 {
    return min(count_before.x, u32(max(params.before, 0))) / pts_len();
}

fn pts_less(a: u32, b: u32) -> bool {
    if params.by == 2 {
        return bitcast<i32>(a) < bitcast<i32>(b);
    }
    return a < b;
}

@compute @workgroup_size(64)
fn pair(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x {
        return;
    }
    if i == 0u {
        header[0] = pts_trails() * pts_len();
        header[1] = 0u;
        header[2] = 0u;
        header[3] = 0u;
    }
    if i >= pts_trails() {
        return;
    }
    let n = pts_trails_before();
    let len = pts_len();
    var found = NONE;
    if params.by == 0 {
        if i < n {
            found = i;
        }
    } else {
        let key = keys_now[i];
        if i < n && keys_before[i * len] == key {
            found = i;
        } else {
            var lo = 0u;
            var hi = n;
            while lo < hi {
                let mid = lo + (hi - lo) / 2u;
                if pts_less(keys_before[mid * len], key) {
                    lo = mid + 1u;
                } else {
                    hi = mid;
                }
            }
            if lo < n && keys_before[lo * len] == key {
                found = lo;
            }
        }
    }
    pairs[i] = found;
}
";

fn pair_params() -> [ParamSpec; 5] {
    [
        ParamSpec::int("now", 0),
        ParamSpec::int("before", 0),
        ParamSpec::int("len", 1),
        ParamSpec::int("max", 0),
        ParamSpec::int("by", 0),
    ]
}

/// Copies one attribute into the new trails, `params.s` words an element.
const GATHER: &str = "
@group(2) @binding(0) var<storage, read> pairs: array<u32>;
@group(2) @binding(1) var<uniform> header: vec4u;
@group(2) @binding(2) var<storage, read> now: array<u32>;
@group(2) @binding(3) var<storage, read> before: array<u32>;
@group(2) @binding(4) var<storage, read_write> out: array<u32>;

@compute @workgroup_size(64)
fn gather(@builtin(global_invocation_id) id: vec3u) {
    let j = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || j >= min(header.x, u32(max(params.n, 0))) {
        return;
    }
    let len = u32(max(params.len, 1));
    let s = u32(max(params.s, 1));
    let i = j / len;
    let k = j % len;
    let m = pairs[i];
    if k > 0u && m != 0xffffffffu {
        // The old trail, one place further back where the step shifts it.
        let back = m * len + k - u32(max(params.shift, 0));
        for (var w = 0u; w < s; w++) {
            out[j * s + w] = before[back * s + w];
        }
    } else {
        for (var w = 0u; w < s; w++) {
            out[j * s + w] = now[i * s + w];
        }
    }
}
";

fn gather_params() -> [ParamSpec; 4] {
    [ParamSpec::int("n", 0), ParamSpec::int("len", 1), ParamSpec::int("s", 1), ParamSpec::int("shift", 0)]
}

/// Compiles the trailer's passes, so the first trail does not wait for them.
pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    gpu.cached_compute("pts trail pair", PAIR, &pair_params())?;
    gpu.cached_compute("pts trail gather", GATHER, &gather_params())?;
    Ok(())
}

/// How the points now find their old trails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Point `i` continues old trail `i`.
    Index,
    /// By this attribute (`u32` or `i32`), as `pts.particles` gives `id`.
    Attr(String),
}

/// One step of a trail.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Points per trail, the head included: 2 or more.
    pub len: u32,
    /// Most trails: points past this many get none.
    pub max: u32,
    pub key: Key,
    /// Whether the old places move one back: false only moves the heads.
    pub shift: bool,
    /// The attributes the trails carry, as the points have them; `position` always.
    pub attrs: Vec<String>,
}

/// What one node needs to make trails: the buffers it writes, kept across steps.
#[derive(Default)]
pub struct Trailer {
    pairs: Option<Buffer>,
    headers: BufferRing,
    rings: Vec<(String, BufferRing)>,
    /// The attribute along the trails for (len, capacity): the same every step.
    along: Option<(u32, u32, Buffer)>,
    /// Keys of none, bound where the points have no key.
    none: Option<Buffer>,
    last: Option<Seen>,
}

impl Trailer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The trails of `now` after one step from `before` (the trails this trailer made a step
    /// ago; `None`, or trails of another shape, start every trail afresh). `Err` for points
    /// without `position`, a key of a type other than `u32` or `i32`, more trail points than a
    /// buffer holds, and anything the GPU refuses.
    pub fn step(
        &mut self,
        gpu: &Gpu,
        now: &Points,
        before: Option<&Points>,
        set: &Settings,
    ) -> Result<Points, Error> {
        let len = set.len.max(2);
        let attrs = self.carried(now, set)?;
        let by = match &set.key {
            Key::Index => 0,
            Key::Attr(name) => match now.attr(name).map(Attr::ty) {
                Some(AttrType::U32) => 1,
                Some(AttrType::I32) => 2,
                Some(ty) => {
                    return Err(Error::new(format!(
                        "trails match points by `{name}`, which is {}; it must be a whole number (uint or int)",
                        ty.wgsl()
                    )));
                }
                None => return Err(Error::new(format!("the points have no `{name}` to match trails by"))),
            },
        };
        let before = before.filter(|b| fits(b, len, &attrs));
        let trails_cap = now.capacity().max(now.count().at_most()).min(set.max);
        let trails = now.count().at_most().min(set.max);
        let capacity = trails_cap.checked_mul(len).filter(|c| *c <= most_elements(gpu, AttrType::Vec4));
        let Some(capacity) = capacity else {
            return Err(Error::new(format!(
                "{trails_cap} trails of {len} points are more than a buffer holds; lower `max` or `length`"
            )));
        };
        let n = trails * len;

        let pairs = sized(gpu, &mut self.pairs, u64::from(trails_cap.max(1)) * 4, "pts trail pairs")?;
        let header = self.headers.next(gpu, HEADER_BYTES, "pts count")?;
        let none = match &self.none {
            Some(b) => b.clone(),
            None => {
                let b = Buffer::new(gpu, 16, "pts trail no keys")?;
                self.none = Some(b.clone());
                b
            }
        };
        let key_of = |p: &Points| match &set.key {
            Key::Attr(name) => p.attr(name).map_or(none.clone(), |a| a.buffer().clone()),
            Key::Index => none.clone(),
        };
        let keys_now = key_of(now);
        let keys_before = before.map_or(none.clone(), key_of);
        let count_now = now.count().header_for(gpu)?;
        let count_before = match before {
            Some(b) => b.count().header_for(gpu)?,
            None => Count::Known(0).header_for(gpu)?,
        };
        let before_n = before.map_or(0, |b| b.count().at_most());
        let pair = gpu.cached_compute("pts trail pair", PAIR, &pair_params())?;
        let param = |name: &str| match name {
            "now" => Some(Value::Int(i64::from(now.count().at_most()))),
            "before" => Some(Value::Int(i64::from(before_n))),
            "len" => Some(Value::Int(i64::from(len))),
            "max" => Some(Value::Int(i64::from(set.max))),
            "by" => Some(Value::Int(by)),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        // At least one thread, which writes the header.
        pair.run_with(
            gpu,
            &[&keys_now, &keys_before, &pairs, &header, &count_now, &count_before],
            &[Step { entry: "pair", out: grid(trails.max(1)), input: &input }],
        )?;

        self.rings.retain(|(name, _)| attrs.iter().any(|(a, _)| a == name));
        let gather = gpu.cached_compute("pts trail gather", GATHER, &gather_params())?;
        let mut out: Vec<Attr> = Vec::with_capacity(attrs.len() + 1);
        for (name, ty) in &attrs {
            let src = now
                .attr(name)
                .map(|a| a.buffer().clone())
                .ok_or_else(|| Error::new(format!("the points lost `{name}`")))?;
            let old = before.and_then(|b| b.attr(name)).map_or(none.clone(), |a| a.buffer().clone());
            let buffer = self.next(gpu, name, u64::from(capacity.max(1)) * ty.stride())?;
            if n > 0 {
                let words = (ty.stride() / 4) as i64;
                let param = |p: &str| match p {
                    "n" => Some(Value::Int(i64::from(n))),
                    "len" => Some(Value::Int(i64::from(len))),
                    "s" => Some(Value::Int(words)),
                    "shift" => Some(Value::Int(i64::from(set.shift))),
                    _ => None,
                };
                let input = Dispatch {
                    volumes: &[],
                    pictures: &[],
                    data: None,
                    seconds: 0.0,
                    frame: 0,
                    param: &param,
                };
                gather.run_with(
                    gpu,
                    &[&pairs, &header, &src, &old, &buffer],
                    &[Step { entry: "gather", out: grid(n), input: &input }],
                )?;
            }
            out.push(Attr::new(name, *ty, buffer)?);
        }
        out.push(Attr::new(ALONG, AttrType::F32, self.along(gpu, len, capacity)?)?);

        let points = Points::new(capacity, 0, out)?;
        let strips = Strips::regular(len);
        let points = match now.count() {
            Count::Known(_) => {
                points.with_count(Count::Known(n))?.with_dims(Some([trails, len, 1])).with_strips(strips)
            }
            Count::Gpu { .. } => {
                // For display only: one that cannot start leaves the count unseen.
                let seen = Seen::reading(gpu, &header, self.last.as_ref()).unwrap_or_default();
                self.last = Some(seen.clone());
                points.with_count(Count::gpu(header, n, seen)?)?.with_strips(strips)
            }
        };
        Ok(points)
    }

    /// The attributes the trails carry: `position`, the key, and the ones asked for that the
    /// points have, each once, with their types.
    fn carried(&self, now: &Points, set: &Settings) -> Result<Vec<(String, AttrType)>, Error> {
        let mut names: Vec<&str> = vec!["position"];
        if let Key::Attr(k) = &set.key {
            names.push(k);
        }
        names.extend(set.attrs.iter().map(String::as_str));
        let mut out: Vec<(String, AttrType)> = Vec::new();
        for name in names {
            let Some(a) = now.attr(name) else {
                if super::canonical(name) == "position" {
                    return Err(Error::new("the points have no `position` to make trails of"));
                }
                continue;
            };
            if a.name() != ALONG && !out.iter().any(|(n, _)| n == a.name()) {
                out.push((a.name().to_string(), a.ty()));
            }
        }
        Ok(out)
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

    /// `trail` for trails of `len` in buffers of `capacity`: 0 at each head to 1 at each tail.
    fn along(&mut self, gpu: &Gpu, len: u32, capacity: u32) -> Result<Buffer, Error> {
        if let Some((l, c, b)) = &self.along
            && *l == len
            && *c == capacity
        {
            return Ok(b.clone());
        }
        let step = 1.0 / (len.max(2) - 1) as f32;
        let values: Vec<f32> = (0..capacity.max(1)).map(|j| (j % len) as f32 * step).collect();
        let b = Buffer::new(gpu, u64::from(capacity.max(1)) * 4, "pts trail")?;
        b.write(gpu, 0, &values)?;
        self.along = Some((len, capacity, b.clone()));
        Ok(b)
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        let own = [&self.pairs, &self.none].iter().filter_map(|b| b.as_ref()).map(Buffer::bytes).sum::<u64>();
        own + self.along.as_ref().map_or(0, |(_, _, b)| b.bytes())
            + self.headers.bytes()
            + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
    }

    /// Lets go of the buffers nobody downstream holds.
    pub fn trim(&mut self) {
        self.headers.trim();
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }
}

/// Whether `before` are trails of `len` with the attributes `attrs` of the same types, so the
/// points now can carry them on.
fn fits(before: &Points, len: u32, attrs: &[(String, AttrType)]) -> bool {
    before.strips() == Strips::regular(len)
        && attrs.iter().all(|(name, ty)| before.attr(name).is_some_and(|a| a.ty() == *ty))
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
