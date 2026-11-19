//! Several sets of points as one ([`Merger`]): the points of the first set, then those of the
//! second, and so on, each set in its own order.
//!
//! Every attribute that any set has is in the result. A set without it gives its points the
//! attribute's default ([`super::default_of`]; 0 for a name penumbra does not know). An attribute
//! that two sets give different types is an error.
//!
//! Where every count is known on the CPU, so is the result's. Where a set keeps its count on the
//! GPU, the result does too: the counts are gathered on the GPU (the header words copied side by
//! side), each set's points go to where the sets before it end, and a last pass writes the total
//! into the result's header. Nothing waits for the GPU.
//!
//! The capacity is the sum of the sets' capacities, so it keeps its size while their counts move.
//! The bounds are the box around the sets' boxes, where every set has one. `dims` go. Lines stay
//! where every set has the same regular lines ([`Strips::Regular`]) whole, or every set is lines
//! of two points ([`Strips::Pairs`]): then the result's header holds the number of lines too.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_mesh::Bounds;

use super::{Attr, AttrType, Count, HEADER_BYTES, Points, Seen, Strips, default_of, most_elements};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// Most sets one merge takes.
pub const MAX_INPUTS: usize = 64;

/// Points per row of a copy's dispatch grid.
const ROW: u32 = 1 << 16;

/// Copies one attribute of one set into place, or fills it with a value where the set lacks it.
/// `pts_counts` holds two words per set: its count and the most it may be.
const MERGE: &str = "
@group(2) @binding(0) var<storage, read> pts_counts: array<u32>;
@group(2) @binding(1) var<storage, read> pts_src: array<u32>;
@group(2) @binding(2) var<storage, read_write> pts_dst: array<u32>;

const PTS_MAX_INPUTS: u32 = 64u;

fn pts_live(k: u32) -> u32 {
    return min(pts_counts[2u * k], pts_counts[2u * k + 1u]);
}

@compute @workgroup_size(64)
fn pts_merge(@builtin(global_invocation_id) gid: vec3u) {
    let t = gid.y * pn.size.x + gid.x;
    let k = min(u32(max(params.input, 0)), PTS_MAX_INPUTS - 1u);
    if gid.x >= pn.size.x || t >= pts_live(k) {
        return;
    }
    var offset = 0u;
    for (var j = 0u; j < k; j++) {
        offset += pts_live(j);
    }
    let words = min(u32(max(params.words, 1)), 4u);
    let o = (offset + t) * words;
    if params.copy == 1 {
        let i = t * words;
        for (var w = 0u; w < words; w++) {
            pts_dst[o + w] = pts_src[i + w];
        }
    } else {
        for (var w = 0u; w < words; w++) {
            pts_dst[o + w] = bitcast<u32>(params.fill[w]);
        }
    }
}

";

/// Writes the total into the header of the merged points; for lines of two points, also the
/// number of lines.
const TOTAL: &str = "
@group(2) @binding(0) var<storage, read> pts_counts: array<u32>;
@group(2) @binding(1) var<storage, read_write> pts_header: array<u32>;

const PTS_MAX_INPUTS: u32 = 64u;

fn pts_live(k: u32) -> u32 {
    return min(pts_counts[2u * k], pts_counts[2u * k + 1u]);
}

@compute @workgroup_size(64)
fn pts_total(@builtin(global_invocation_id) gid: vec3u) {
    if gid.x != 0u {
        return;
    }
    let n = min(u32(max(params.inputs, 0)), PTS_MAX_INPUTS);
    var total = 0u;
    for (var j = 0u; j < n; j++) {
        total += pts_live(j);
    }
    pts_header[0] = total;
    pts_header[1] = select(0u, total / 2u, params.pairs == 1);
    pts_header[2] = 0u;
    pts_header[3] = 0u;
}
";

fn params() -> [ParamSpec; 6] {
    [
        ParamSpec::int("input", 0),
        ParamSpec::int("words", 1),
        ParamSpec::int("copy", 0),
        ParamSpec::vec4("fill", [0.0; 4]),
        ParamSpec::int("inputs", 0),
        ParamSpec::int("pairs", 0),
    ]
}

/// The buffers one node keeps to merge points, across cooks.
#[derive(Default)]
pub struct Merger {
    rings: Vec<(String, BufferRing)>,
    headers: BufferRing,
    counts: Option<Buffer>,
    /// Stands in for the attribute of a set that lacks it.
    none: Option<Buffer>,
    /// The count of the last merge, which the next one's display follows.
    last: Option<Seen>,
}

impl Merger {
    pub fn new() -> Self {
        Self::default()
    }

    /// The points of every set in `sets`, one after the other. Each set comes with the number
    /// its input has, for the messages. `Err` for an attribute of two types, more than
    /// [`MAX_INPUTS`] sets, more points than a buffer holds, and anything the GPU refuses.
    pub fn merge(&mut self, gpu: &Gpu, sets: &[(usize, &Points)]) -> Result<Points, Error> {
        if sets.len() > MAX_INPUTS {
            return Err(Error::new(format!(
                "{} sets of points are more than the {MAX_INPUTS} a merge takes",
                sets.len()
            )));
        }
        let attrs = union(sets)?;
        let capacity =
            sets.iter().map(|(_, p)| u64::from(p.capacity().max(p.count().at_most()))).sum::<u64>();
        let at_most = sets.iter().map(|(_, p)| u64::from(p.count().at_most())).sum::<u64>();
        for (name, ty) in &attrs {
            if capacity > u64::from(most_elements(gpu, *ty)) {
                return Err(Error::new(format!(
                    "{capacity} points of `{name}` are more than one buffer on this GPU holds"
                )));
            }
        }
        let capacity = u32::try_from(capacity).map_err(|_| Error::new("too many points to merge"))?;
        let at_most = u32::try_from(at_most).map_err(|_| Error::new("too many points to merge"))?;
        let on_gpu = sets.iter().any(|(_, p)| p.count().known().is_none());
        let strips = strips(sets);
        let pairs = strips == Strips::Pairs;

        self.rings.retain(|(name, _)| attrs.iter().any(|(n, _)| n == name));
        let mut out: Vec<Attr> = Vec::with_capacity(attrs.len());
        let mut buffers: Vec<Buffer> = Vec::with_capacity(attrs.len());
        for (name, ty) in &attrs {
            if !self.rings.iter().any(|(n, _)| n == name) {
                self.rings.push((name.clone(), BufferRing::new()));
            }
            let Some((_, ring)) = self.rings.iter_mut().find(|(n, _)| n == name) else {
                return Err(Error::new(format!("no buffers for `{name}`")));
            };
            let buffer = ring.next(gpu, u64::from(capacity.max(1)) * ty.stride(), &format!("pts {name}"))?;
            out.push(Attr::new(name, *ty, buffer.clone())?);
            buffers.push(buffer);
        }

        // Two words per set: its count (known, or copied from its header below) and its most.
        let counts_bytes = (MAX_INPUTS as u64) * 8;
        let counts = match self.counts.as_ref() {
            Some(b) => b.clone(),
            None => {
                let b = Buffer::new(gpu, counts_bytes, "pts merge counts")?;
                self.counts = Some(b.clone());
                b
            }
        };
        let mut words = vec![0u32; MAX_INPUTS * 2];
        for (k, (_, p)) in sets.iter().enumerate() {
            let count = p.count();
            if let (Some(c), Some(m)) = (words.get_mut(2 * k), count.known()) {
                *c = m;
            }
            if let Some(m) = words.get_mut(2 * k + 1) {
                *m = count.at_most();
            }
        }
        counts.write(gpu, 0, &words)?;
        let headers: Vec<(usize, Buffer)> = sets
            .iter()
            .enumerate()
            .filter_map(|(k, (_, p))| p.count().header().map(|h| (k, h.clone())))
            .collect();
        if !headers.is_empty() {
            let ((), error) = gpu.scoped(|| {
                let mut encoder = gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("pts merge counts"),
                });
                for (k, h) in &headers {
                    encoder.copy_buffer_to_buffer(h.buffer(), 0, counts.buffer(), (*k as u64) * 8, 4);
                }
                gpu.submit([encoder.finish()]);
            });
            if let Some(e) = error {
                return Err(Error::new(format!("cannot gather the counts: {e}")));
            }
        }

        let none = match self.none.as_ref() {
            Some(b) => b.clone(),
            None => {
                let b = Buffer::new(gpu, 16, "pts merge none")?;
                self.none = Some(b.clone());
                b
            }
        };
        let program = gpu.cached_compute("pts merge", MERGE, &params())?;
        for (k, (_, p)) in sets.iter().enumerate() {
            let n = p.count().at_most();
            if n == 0 {
                continue;
            }
            for ((name, ty), dst) in attrs.iter().zip(&buffers) {
                let src = p.attr(name);
                let fill = fill_of(name, *ty);
                let words = (ty.stride() / 4) as i64;
                let copy = src.is_some();
                let param = |n: &str| match n {
                    "input" => Some(Value::Int(k as i64)),
                    "words" => Some(Value::Int(words)),
                    "copy" => Some(Value::Int(i64::from(copy))),
                    "fill" => Some(Value::Vec4(fill.map(f64::from))),
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
                let src = src.map_or(&none, Attr::buffer);
                let (w, h) = (n.clamp(1, ROW), n.div_ceil(n.clamp(1, ROW)));
                program.run_with(
                    gpu,
                    &[&counts, src, dst],
                    &[Step { entry: "pts_merge", out: Out::grid(w, h), input: &input }],
                )?;
            }
        }

        let bounds = bounds(sets);
        let made = Points::new(capacity, 0, out)?.with_bounds(bounds);
        if !on_gpu {
            let made = made.with_count(Count::Known(at_most))?;
            return Ok(made.with_strips(strips));
        }
        let header = self.headers.next(gpu, HEADER_BYTES, "pts count")?;
        let inputs = sets.len() as i64;
        let param = |n: &str| match n {
            "inputs" => Some(Value::Int(inputs)),
            "pairs" => Some(Value::Int(i64::from(pairs))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let total = gpu.cached_compute("pts merge total", TOTAL, &params())?;
        total.run_with(
            gpu,
            &[&counts, &header],
            &[Step { entry: "pts_total", out: Out::range(1), input: &input }],
        )?;
        // The readback is for display only: one that cannot start leaves the count unseen.
        let seen = Seen::reading(gpu, &header, self.last.as_ref()).unwrap_or_default();
        self.last = Some(seen.clone());
        let made = made.with_count(Count::gpu(header, at_most, seen)?)?;
        Ok(made.with_strips(strips))
    }

    /// Bytes on the GPU of the buffers it keeps.
    pub fn bytes(&self) -> u64 {
        let own =
            [&self.counts, &self.none].iter().filter_map(|b| b.as_ref()).map(Buffer::bytes).sum::<u64>();
        own + self.headers.bytes() + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
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

/// Every attribute of the sets, in the order they first appear, with its type.
fn union(sets: &[(usize, &Points)]) -> Result<Vec<(String, AttrType)>, Error> {
    let mut out: Vec<(String, AttrType, usize)> = Vec::new();
    for (input, p) in sets {
        for a in p.attrs() {
            match out.iter().find(|(n, _, _)| n == a.name()) {
                Some((_, ty, first)) if *ty != a.ty() => {
                    return Err(Error::new(format!(
                        "`{}` is {} in input {first} and {} in input {input}: the types must match (pts.math can convert one)",
                        a.name(),
                        ty.wgsl(),
                        a.ty().wgsl()
                    )));
                }
                Some(_) => {}
                None => out.push((a.name().to_string(), a.ty(), *input)),
            }
        }
    }
    Ok(out.into_iter().map(|(n, t, _)| (n, t)).collect())
}

/// The value a set without the attribute gives its points, as the attribute's words hold it.
fn fill_of(name: &str, ty: AttrType) -> [f32; 4] {
    let d = default_of(name).filter(|d| d.ty == ty).map_or([0.0; 4], |d| d.default);
    match ty {
        // Whole numbers: their bits, which for the defaults (0) are the float's.
        AttrType::U32 | AttrType::I32 => [f32::from_bits(d[0] as u32), 0.0, 0.0, 0.0],
        // A position's fourth float is 1, any other vec3's 0, as the nodes that make them write.
        AttrType::Vec3 => [d[0], d[1], d[2], if name == "position" { 1.0 } else { 0.0 }],
        _ => d,
    }
}

/// The box around every set's box, where each set that has points has one.
fn bounds(sets: &[(usize, &Points)]) -> Option<Bounds> {
    let mut out: Option<Bounds> = None;
    for (_, p) in sets {
        if p.count().at_most() == 0 {
            continue;
        }
        let b = p.bounds()?;
        out = Some(match out {
            None => b,
            Some(o) => Bounds {
                min: [o.min[0].min(b.min[0]), o.min[1].min(b.min[1]), o.min[2].min(b.min[2])],
                max: [o.max[0].max(b.max[0]), o.max[1].max(b.max[1]), o.max[2].max(b.max[2])],
            },
        });
    }
    out
}

/// The lines of the merged points: the sets' own where every set has the same lines and they
/// stay whole when the sets follow each other; none otherwise.
fn strips(sets: &[(usize, &Points)]) -> Strips {
    let with_points: Vec<&Points> =
        sets.iter().map(|(_, p)| *p).filter(|p| p.count().at_most() > 0).collect();
    let Some(first) = with_points.first() else { return Strips::None };
    let on_gpu = with_points.iter().any(|p| p.count().known().is_none());
    // Lines of two points: a set's own pairs, or open lines of two whose count is even.
    let pair = |p: &Points| match (p.strips(), p.count().known()) {
        (Strips::Pairs, _) => true,
        (Strips::Regular { len: 2, closed: false }, Some(n)) => n % 2 == 0,
        _ => false,
    };
    if with_points.iter().all(|p| pair(p)) {
        return if on_gpu { Strips::Pairs } else { Strips::regular(2) };
    }
    match first.strips() {
        Strips::Regular { len, closed } if len > 0 => {
            // A count on the GPU holds whole lines where the node that made it gave these strips.
            let whole = |p: &Points| {
                p.strips() == Strips::Regular { len, closed }
                    && p.count().known().is_none_or(|n| n % len == 0)
            };
            if with_points.iter().all(|p| whole(p)) { Strips::Regular { len, closed } } else { Strips::None }
        }
        _ => Strips::None,
    }
}
