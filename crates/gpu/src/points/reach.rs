//! The box around points whose positions only the GPU knows ([`LateBounds`]): after a program
//! moved them, or for particles. A reduction of `position` on the GPU, read back a frame or more
//! later without waiting, so the CPU has a box to frame a picture and to size a shadow map by.
//!
//! The box is late: points that moved since lie outside it by as much as they moved. It is grown
//! by [`MARGIN`] of its size, so a cloud that drifts or swells slowly stays inside. Use it where
//! a box a little too small costs a little (framing, shadow maps), never where it would hide
//! points.

use std::collections::VecDeque;

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_mesh::Bounds;

use super::{Points, Wake};
use crate::{Buffer, BufferKey, BufferRead, Dispatch, Gpu, Out, ReadState, Step};

/// Points per row of the dispatch grid.
const ROW: u32 = 1 << 16;

/// Readbacks in flight at most; while this many wait, no new reduction starts.
const IN_FLIGHT: usize = 2;

/// How much the box read back grows on each side, as a part of its largest side.
pub const MARGIN: f32 = 0.1;

/// The smallest and largest of each coordinate, as `u32` keys whose order is the floats' order,
/// so atomics find them. Points that are no finite number are left out.
const REDUCE: &str = "
@group(2) @binding(0) var<storage, read> pts_position: array<vec4f>;
@group(2) @binding(1) var<storage, read_write> pts_box: array<atomic<u32>, 8>;
@group(2) @binding(2) var<uniform> pts_header: vec4u;

var<workgroup> pts_lo: array<atomic<u32>, 3>;
var<workgroup> pts_hi: array<atomic<u32>, 3>;

// Larger floats give larger keys: negative ones flipped whole, positive ones above them.
fn pts_key(f: f32) -> u32 {
    let u = bitcast<u32>(f);
    return select(u | 0x80000000u, ~u, (u & 0x80000000u) != 0u);
}

@compute @workgroup_size(64)
fn clear(@builtin(global_invocation_id) id: vec3u) {
    if id.x < 3u {
        atomicStore(&pts_box[id.x], 0xffffffffu);
        atomicStore(&pts_box[id.x + 4u], 0u);
    }
}

@compute @workgroup_size(64)
fn reduce(@builtin(global_invocation_id) id: vec3u, @builtin(local_invocation_index) t: u32) {
    if t < 3u {
        atomicStore(&pts_lo[t], 0xffffffffu);
        atomicStore(&pts_hi[t], 0u);
    }
    workgroupBarrier();
    let i = id.y * pn.size.x + id.x;
    let n = min(pts_header.x, u32(max(params.n, 0)));
    if id.x < pn.size.x && i < n {
        let p = pts_position[i].xyz;
        // NaN fails every comparison, infinity this one.
        if all(abs(p) < vec3f(1e30)) {
            for (var k = 0u; k < 3u; k++) {
                atomicMin(&pts_lo[k], pts_key(p[k]));
                atomicMax(&pts_hi[k], pts_key(p[k]));
            }
        }
    }
    workgroupBarrier();
    if t < 3u {
        atomicMin(&pts_box[t], atomicLoad(&pts_lo[t]));
        atomicMax(&pts_box[t + 4u], atomicLoad(&pts_hi[t]));
    }
}
";

/// The float a key of [`REDUCE`] stands for.
fn unkey(k: u32) -> f32 {
    f32::from_bits(if k & 0x8000_0000 != 0 { k & 0x7fff_ffff } else { !k })
}

/// The box a readback holds: `None` where no point had a finite position.
fn box_of(words: &[u32]) -> Option<Bounds> {
    let [l0, l1, l2, _, h0, h1, h2, ..] = *words else { return None };
    let (min, max) = ([unkey(l0), unkey(l1), unkey(l2)], [unkey(h0), unkey(h1), unkey(h2)]);
    let sound = min.iter().zip(&max).all(|(lo, hi)| lo.is_finite() && hi.is_finite() && lo <= hi);
    sound.then_some(Bounds { min, max })
}

/// `b` grown by [`MARGIN`] of its largest side on each side.
fn grown(b: Bounds) -> Bounds {
    let side = b.max.iter().zip(&b.min).map(|(hi, lo)| hi - lo).fold(0.0f32, f32::max);
    let m = side * MARGIN;
    Bounds { min: b.min.map(|v| v - m), max: b.max.map(|v| v + m) }
}

/// The box around the positions of points, worked out on the GPU and read back late. One per
/// node; it keeps its buffer and readbacks across cooks.
#[derive(Default)]
pub struct LateBounds {
    target: Option<Buffer>,
    reads: VecDeque<BufferRead>,
    spare: Option<BufferRead>,
    last: Option<Bounds>,
    /// A box that differs from the one before it arrived since the last reduction.
    fresh: bool,
    /// The buffers of the points of the last reduction: `position` and the count's header.
    from: Option<[BufferKey; 2]>,
}

impl LateBounds {
    pub fn new() -> Self {
        Self::default()
    }

    /// The box around the positions of `points` as last read back, grown by [`MARGIN`]: a frame
    /// or more late, `None` before the first arrives or where no point has a finite position.
    /// Starts the next reduction unless [`IN_FLIGHT`] readbacks still wait. `wake` is called when
    /// its box arrives, unless nothing changed: the points come from the same buffers as at the
    /// last reduction and the last box that arrived was the same as the one before. So a node on
    /// points that stopped changing cooks once or twice more to take their box, then rests.
    /// `Err` for anything the GPU refuses.
    pub fn update(
        &mut self,
        gpu: &Gpu,
        points: &Points,
        wake: Option<Wake>,
    ) -> Result<Option<Bounds>, Error> {
        self.settle();
        let n = points.count().at_most();
        let Some(position) = points.attr("position").filter(|a| a.ty() == super::AttrType::Vec3) else {
            return Ok(self.last.map(grown));
        };
        if n == 0 || self.reads.len() >= IN_FLIGHT {
            return Ok(self.last.map(grown));
        }
        let target = match &self.target {
            Some(b) => b.clone(),
            None => {
                let b = Buffer::new(gpu, 32, "pts box")?;
                self.target = Some(b.clone());
                b
            }
        };
        let header = points.count().header_for(gpu)?;
        let keys = [position.buffer().key(), header.key()];
        let same = !self.fresh && self.last.is_some() && self.from.as_ref() == Some(&keys);
        self.fresh = false;
        self.from = Some(keys);
        let wake = wake.filter(|_| !same);
        let program = gpu.cached_compute("pts box", REDUCE, &[ParamSpec::int("n", 0)])?;
        let param = |name: &str| (name == "n").then_some(Value::Int(i64::from(n)));
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        let (w, h) = (n.clamp(1, ROW), n.div_ceil(n.clamp(1, ROW)));
        program.run_with(
            gpu,
            &[position.buffer(), &target, &header],
            &[
                Step { entry: "clear", out: Out::range(3), input: &input },
                Step { entry: "reduce", out: Out::grid(w, h), input: &input },
            ],
        )?;
        self.reads.push_back(target.read_async_notify(gpu, self.spare.take(), wake)?);
        Ok(self.last.map(grown))
    }

    /// Takes in the readbacks that arrived, oldest first; drops those that failed.
    fn settle(&mut self) {
        while let Some(state) = self.reads.front().map(BufferRead::state) {
            if state == ReadState::Waiting {
                break;
            }
            let Some(read) = self.reads.pop_front() else { break };
            if state == ReadState::Ready {
                if let Ok(words) = read.get::<u32>() {
                    let b = box_of(&words);
                    self.fresh |= b != self.last;
                    self.last = b;
                }
                self.spare = Some(read);
            }
        }
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        self.target.as_ref().map_or(0, Buffer::bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::points::{Attr, AttrType, Count, Seen};
    use crate::testing::gpu;

    #[test]
    fn keys_keep_the_order_of_the_floats() {
        let key = |f: f32| {
            let u = f.to_bits();
            if u & 0x8000_0000 != 0 { !u } else { u | 0x8000_0000 }
        };
        let floats = [-1e9f32, -2.5, -1.0, -0.0, 0.0, 1e-30, 0.5, 3.0, 7e20];
        for pair in floats.windows(2) {
            assert!(key(pair[0]) <= key(pair[1]), "{pair:?}");
        }
        for f in floats {
            assert_eq!(unkey(key(f)), f);
        }
        assert_eq!(box_of(&[u32::MAX, u32::MAX, u32::MAX, 0, 0, 0, 0, 0]), None, "no point");
    }

    #[test]
    fn the_box_arrives_late_grown_and_without_points_that_are_no_number() {
        let Some(gpu) = gpu() else { return };
        let mut pos: Vec<f32> = vec![-1.0, 2.0, 0.5, 1.0, 3.0, -4.0, 1.5, 1.0];
        pos.extend([f32::NAN, 0.0, 0.0, 1.0, f32::INFINITY, 0.0, 0.0, 1.0]);
        // Past the live count of 2 on the GPU: never in the box.
        pos.extend([100.0, 100.0, 100.0, 1.0]);
        let buffer = Buffer::of::<f32>(&gpu, pos.len() as u64, "position").unwrap();
        buffer.write(&gpu, 0, &pos).unwrap();
        let attrs = vec![Attr::new("position", AttrType::Vec3, buffer).unwrap()];
        let all = Points::new(5, 4, attrs.clone()).unwrap();
        let mut late = LateBounds::new();
        assert_eq!(late.update(&gpu, &all, None).unwrap(), None, "nothing read back yet");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let b = loop {
            gpu.poll();
            if let Some(b) = late.update(&gpu, &all, None).unwrap() {
                break b;
            }
            assert!(std::time::Instant::now() < deadline);
        };
        // Points from (-1, -4, 0.5) to (3, 2, 1.5): the largest side is 6, grown by 0.6.
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5);
        assert!(close(b.min, [-1.6, -4.6, -0.1]) && close(b.max, [3.6, 2.6, 2.1]), "{b:?}");
        // A count on the GPU: the points past it stay out.
        let header = Buffer::of::<u32>(&gpu, 4, "count").unwrap();
        header.write(&gpu, 0, &[1u32, 0, 0, 0]).unwrap();
        let some = Points::new(5, 0, attrs)
            .unwrap()
            .with_count(Count::gpu(header, 5, Seen::default()).unwrap())
            .unwrap();
        let mut late = LateBounds::new();
        let b = loop {
            gpu.poll();
            if let Some(b) = late.update(&gpu, &some, None).unwrap() {
                break b;
            }
            assert!(std::time::Instant::now() < deadline);
        };
        assert!(close(b.min, [-1.0, 2.0, 0.5]) && close(b.max, [-1.0, 2.0, 0.5]), "one point: {b:?}");
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }
}
