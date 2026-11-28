//! Values of one attribute, gathered on the GPU into a small buffer to read back ([`Probe`]): the
//! count, the first points' values, and the mean, least and greatest value over all points.
//!
//! The summary goes over every point in two passes: up to 4096 threads each add up and compare
//! every 4096th point, then one thread combines their results (no workgroup memory, so nothing
//! that differs between backends). So a network can follow an attribute of a million points by
//! reading back a few dozen bytes.
//!
//! Layout of the buffer, in `vec4f`s: the count (its bits in the first word), the mean, the
//! least, the greatest, then the first points' values. Every value is four numbers as a picture
//! of the attribute has them ([`super::Points::attr_picture`]): a scalar in all three colours,
//! a `vec2f` in the first two. With no points the summary is zeros.

use penumbra_core::{Error, ParamSpec, Value};

use super::{AttrType, Points, canonical, default_of};
use crate::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};

/// `vec4f`s before the first point's value.
pub const HEAD: u64 = 4;

/// Most points whose values one probe reads.
pub const MAX_FIRST: u32 = 4096;

/// Threads of the summary's first pass at most: the second pass, one thread, reads one result
/// each.
const THREADS: u32 = 4096;

/// Points each thread of the first pass looks at, at most: with [`THREADS`] threads that covers
/// 2^32 points.
const MAX_PER: u32 = 1 << 20;

const SOURCE: &str = "
@group(2) @binding(0) var<storage, read> pts_words: array<u32>;
@group(2) @binding(1) var<uniform> pts_header: vec4u;
@group(2) @binding(2) var<storage, read_write> pts_partial: array<vec4f>;
@group(2) @binding(3) var<storage, read_write> pts_out: array<vec4f>;

const PTS_THREADS: u32 = 4096u;
const PTS_MAX_PER: u32 = 1048576u;
const PTS_FAR: f32 = 3.0e38;

fn pts_word(i: u32) -> f32 {
    return bitcast<f32>(pts_words[i]);
}

fn pts_count() -> u32 {
    return min(pts_header.x, u32(max(params.count, 0)));
}

// Point i's value as four numbers. (No return inside the switch: FXC miscompiles it.)
fn pts_value(i: u32) -> vec4f {
    var v = params.fill;
    switch params.kind {
        case 0: {
            v = vec4f(vec3f(pts_word(i)), 1.0);
        }
        case 1: {
            v = vec4f(pts_word(2u * i), pts_word(2u * i + 1u), 0.0, 1.0);
        }
        case 2: {
            v = vec4f(vec3f(f32(pts_words[i])), 1.0);
        }
        case 3: {
            v = vec4f(vec3f(f32(bitcast<i32>(pts_words[i]))), 1.0);
        }
        case 4: {
            v = vec4f(pts_word(4u * i), pts_word(4u * i + 1u), pts_word(4u * i + 2u), 1.0);
        }
        case 5: {
            v = vec4f(pts_word(4u * i), pts_word(4u * i + 1u), pts_word(4u * i + 2u), pts_word(4u * i + 3u));
        }
        default: {}
    }
    return v;
}

@compute @workgroup_size(64)
fn pts_first(@builtin(global_invocation_id) gid: vec3u) {
    let t = gid.x;
    if t >= u32(max(params.first, 0)) {
        return;
    }
    var v = vec4f(0.0);
    if t < pts_count() {
        v = pts_value(t);
    }
    pts_out[4u + t] = v;
}

// The first pass: thread t looks at points t, t + threads, t + 2 threads, ...
@compute @workgroup_size(64)
fn pts_part(@builtin(global_invocation_id) gid: vec3u) {
    let t = gid.x;
    let threads = min(u32(max(params.threads, 1)), PTS_THREADS);
    if t >= threads {
        return;
    }
    let n = pts_count();
    let per = min(u32(max(params.per, 1)), PTS_MAX_PER);
    var sum = vec4f(0.0);
    var low = vec4f(PTS_FAR);
    var high = vec4f(-PTS_FAR);
    for (var j = 0u; j < per; j++) {
        let i = j * threads + t;
        if i < n {
            let v = pts_value(i);
            sum += v;
            low = min(low, v);
            high = max(high, v);
        }
    }
    pts_partial[3u * t] = sum;
    pts_partial[3u * t + 1u] = low;
    pts_partial[3u * t + 2u] = high;
}

// The second pass, one thread: the first pass's results combined, and the count.
@compute @workgroup_size(64)
fn pts_total(@builtin(global_invocation_id) gid: vec3u) {
    if gid.x != 0u {
        return;
    }
    let n = pts_count();
    pts_out[0] = vec4f(bitcast<f32>(n), 0.0, 0.0, 0.0);
    pts_out[1] = vec4f(0.0);
    pts_out[2] = vec4f(0.0);
    pts_out[3] = vec4f(0.0);
    if n == 0u || params.summary == 0 {
        return;
    }
    let threads = min(u32(max(params.threads, 1)), PTS_THREADS);
    var sum = vec4f(0.0);
    var low = vec4f(PTS_FAR);
    var high = vec4f(-PTS_FAR);
    for (var t = 0u; t < threads; t++) {
        sum += pts_partial[3u * t];
        low = min(low, pts_partial[3u * t + 1u]);
        high = max(high, pts_partial[3u * t + 2u]);
    }
    pts_out[1] = sum / f32(n);
    pts_out[2] = low;
    pts_out[3] = high;
}
";

fn params() -> [ParamSpec; 7] {
    [
        ParamSpec::int("kind", 0),
        ParamSpec::int("count", 0),
        ParamSpec::vec4("fill", [0.0; 4]),
        ParamSpec::int("first", 0),
        ParamSpec::int("threads", 1),
        ParamSpec::int("per", 1),
        ParamSpec::int("summary", 0),
    ]
}

/// What a probe reads: an attribute, how many of the first points, and the summary or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask<'a> {
    pub name: &'a str,
    pub first: u32,
    pub summary: bool,
}

/// The buffers one node keeps to probe points.
#[derive(Default)]
pub struct Probe {
    partial: Option<Buffer>,
    none: Option<Buffer>,
    /// The buffers it fills, reused once nobody reads them any more.
    outs: BufferRing,
}

impl Probe {
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding what `ask` wants of `points`, filled after the GPU work recorded
    /// before: read it back ([`Buffer::read_async`]). The buffer is one of its own, used again
    /// once nobody holds it. `Err` for an attribute the points lack and penumbra does not know,
    /// and for anything the GPU refuses.
    pub fn fill(&mut self, gpu: &Gpu, points: &Points, ask: Ask<'_>) -> Result<Buffer, Error> {
        let first = ask.first.min(MAX_FIRST);
        let out = self.outs.next(gpu, (HEAD + u64::from(first)) * 16, "pts probe")?;
        let attr = points.attr(ask.name);
        let (kind, fill) = match (attr.map(super::Attr::ty), default_of(ask.name)) {
            (Some(AttrType::F32), _) => (0, [0.0; 4]),
            (Some(AttrType::Vec2), _) => (1, [0.0; 4]),
            (Some(AttrType::U32), _) => (2, [0.0; 4]),
            (Some(AttrType::I32), _) => (3, [0.0; 4]),
            (Some(AttrType::Vec3), _) => (4, [0.0; 4]),
            (Some(AttrType::Vec4), _) => (5, [0.0; 4]),
            (None, Some(s)) => (6, super::texel_of(s.ty, s.default)),
            (None, None) => {
                return Err(Error::new(format!("the points have no attribute `{}`", canonical(ask.name))));
            }
        };
        let n = points.count().at_most();
        let src = match attr {
            Some(a) if n > 0 => a.buffer().clone(),
            _ => self.none(gpu)?,
        };
        let header = points.count().header_for(gpu)?;
        let threads = n.clamp(1, THREADS);
        let per = n.div_ceil(threads).clamp(1, MAX_PER);
        let partial = match self.partial.as_ref() {
            Some(b) => b.clone(),
            None => {
                let b = Buffer::new(gpu, u64::from(THREADS) * 3 * 16, "pts probe partial")?;
                self.partial = Some(b.clone());
                b
            }
        };
        let param = |name: &str| match name {
            "kind" => Some(Value::Int(kind)),
            "count" => Some(Value::Int(i64::from(n))),
            "fill" => Some(Value::Vec4(fill.map(f64::from))),
            "first" => Some(Value::Int(i64::from(first))),
            "threads" => Some(Value::Int(i64::from(threads))),
            "per" => Some(Value::Int(i64::from(per))),
            "summary" => Some(Value::Int(i64::from(ask.summary))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let program = gpu.cached_compute("pts probe", SOURCE, &params())?;
        let mut steps = Vec::new();
        if first > 0 {
            steps.push(Step { entry: "pts_first", out: Out::range(first), input: &input });
        }
        // The count is always there; the summary only where asked for.
        if ask.summary {
            steps.push(Step { entry: "pts_part", out: Out::range(threads), input: &input });
        }
        steps.push(Step { entry: "pts_total", out: Out::range(1), input: &input });
        program.run_with(gpu, &[&src, &header, &partial, &out], &steps)?;
        Ok(out)
    }

    fn none(&mut self, gpu: &Gpu) -> Result<Buffer, Error> {
        if let Some(b) = &self.none {
            return Ok(b.clone());
        }
        let b = Buffer::new(gpu, 16, "pts probe none")?;
        self.none = Some(b.clone());
        Ok(b)
    }

    pub fn bytes(&self) -> u64 {
        [&self.partial, &self.none].iter().filter_map(|b| b.as_ref()).map(Buffer::bytes).sum::<u64>()
            + self.outs.bytes()
    }

    /// Lets go of the buffers nobody reads.
    pub fn trim(&mut self) {
        self.outs.trim();
    }
}

/// What a probe's buffer says: the count, the summary (mean, least, greatest) and the first
/// points' values, as read back.
#[derive(Clone, Debug, PartialEq)]
pub struct Probed {
    pub count: u32,
    pub mean: [f32; 4],
    pub min: [f32; 4],
    pub max: [f32; 4],
    pub first: Vec<[f32; 4]>,
}

impl Probed {
    /// The buffer's `vec4f`s, read back; `None` when it is shorter than its head.
    pub fn of(values: &[[f32; 4]]) -> Option<Self> {
        let [count, mean, min, max, rest @ ..] = values else { return None };
        Some(Self { count: count[0].to_bits(), mean: *mean, min: *min, max: *max, first: rest.to_vec() })
    }
}

/// A buffer to read back, from `ring`: the header of `points`' count (zeros for a count the CPU
/// knows), then the first `rows` elements of each attribute named in `names`, one attribute
/// after the other, as their buffers lay them out. An attribute the points lack, or whose buffer
/// is too short, stays zeros.
pub fn first_rows(
    gpu: &Gpu,
    points: &Points,
    names: &[&str],
    rows: u32,
    ring: &mut BufferRing,
) -> Result<Buffer, Error> {
    let n = u64::from(rows);
    let len = |name: &str| points.attr(name).map_or(0, |a| n * a.ty().stride());
    let bytes = super::HEADER_BYTES + names.iter().map(|name| len(name)).sum::<u64>();
    let out = ring.next(gpu, bytes, "pts first rows")?;
    let ((), error) = gpu.scoped(|| {
        let mut encoder = gpu.encoder("pts first rows");
        // A buffer used before holds what it read then.
        encoder.clear_buffer(out.buffer(), 0, None);
        if let Some(h) = points.count().header() {
            encoder.copy_buffer_to_buffer(h.buffer(), 0, out.buffer(), 0, super::HEADER_BYTES);
        }
        let mut at = super::HEADER_BYTES;
        for name in names {
            let size = len(name);
            if let Some(a) = points.attr(name)
                && size > 0
                && a.buffer().bytes() >= size
            {
                encoder.copy_buffer_to_buffer(a.buffer().buffer(), 0, out.buffer(), at, size);
            }
            at += size;
        }
        gpu.submit([encoder.finish()]);
    });
    match error {
        Some(e) => Err(Error::new(format!("cannot copy the points: {e}"))),
        None => Ok(out),
    }
}
