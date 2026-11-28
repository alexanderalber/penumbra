//! GPU time per node, measured with timestamp queries.
//!
//! Every command encoder penumbra records ([`Gpu::encoder`], and the passes of [`crate::deferred`])
//! gets a timestamp at its start and one at its end while timing is on, if the thread is cooking a
//! node ([`penumbra_core::cooking`]). The pair is put down to that node. So one place covers render
//! passes, compute dispatches and copies of every node kind.
//!
//! The timestamps of one cook pass go into one query set (a "slot"). At the end of the pass
//! ([`Gpu::end_timing_frame`]) the set is resolved and copied into a buffer that maps once the GPU
//! got there; [`Gpu::poll`] adds what arrived. Nothing waits for the GPU: the numbers come a frame
//! or more late, like any readback. A slot still in flight when the next pass ends is not reused;
//! with all [`SLOTS`] in flight a pass goes untimed.
//!
//! What it does not see: work recorded outside a node's cook (thumbnails, outputs, free views, the
//! editor), work on threads a node started, uploads through the queue, and encoders of the crates
//! that record their own (video in and out, screen capture). An encoder's two timestamps also
//! enclose any GPU idle time between them and work of other command buffers that overlaps.
//!
//! Timing needs `TIMESTAMP_QUERY` and `TIMESTAMP_QUERY_INSIDE_ENCODERS` (Direct3D 12 and Vulkan
//! have them, Apple GPUs do not); without them it stays off and costs nothing.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, lock};

use crate::Gpu;
use crate::readback::ReadAge;

/// Timestamps in one slot: wgpu's largest query set. Two per encoder, so 2048 encoders a pass;
/// more go untimed.
const QUERIES: u32 = wgpu::QUERY_SET_MAX_QUERIES;

/// Slots in use at once: one recording, the others resolved and waiting for the GPU.
pub(crate) const SLOTS: usize = 4;

/// An encoder pair longer than this is no measurement (a slot reused before an encoder that
/// failed wrote into it).
const LONGEST: Duration = Duration::from_secs(10);

/// Whether timing can run and runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingState {
    /// The device lacks timestamp queries inside encoders.
    Unsupported,
    Off,
    On,
}

impl TimingState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

/// GPU time of one node, summed over the timed passes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSpent {
    /// Command encoders timed.
    pub encoders: u64,
    pub time: Duration,
}

/// What [`Gpu::take_gpu_times`] hands back: the sums since the last reset.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuTimes {
    pub state: TimingState,
    /// Cook passes whose timestamps arrived. Divide by this for time per pass.
    pub frames: u64,
    /// By node, in id order.
    pub nodes: Vec<(NodeId, GpuSpent)>,
    /// Per pass, from the first timed encoder's start to the last one's end: the GPU's span over
    /// the timed work, gaps included. Summed over the passes.
    pub span: Duration,
    /// Encoders a node recorded while timing was on that went untimed: no free slot, or a slot full.
    pub untimed: u64,
    /// Passes whose timestamps did not arrive (the readback failed or took too long).
    pub lost: u64,
}

/// One query set and the buffers its timestamps travel through.
struct Slot {
    serial: u64,
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// The node of each pair of queries, in order: pair `i` is queries `2i` and `2i + 1`.
    /// `None` for a pair whose encoder failed and was never submitted.
    pairs: Vec<Option<NodeId>>,
}

/// A slot resolved and waiting for its buffer to map.
struct Flight {
    slot: Slot,
    state: Arc<AtomicU8>,
    age: ReadAge,
}

const WAITING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// The timing state of a device, in [`crate::Inner`].
pub(crate) struct Timing {
    supported: bool,
    /// Read without the lock by every encoder: off costs one atomic load.
    on: AtomicBool,
    inner: std::sync::Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Timing stays on until then ([`Gpu::time_gpu_until`]).
    until: Option<Instant>,
    /// The slot the running pass records into.
    open: Option<Slot>,
    flight: Vec<Flight>,
    spare: Vec<Slot>,
    /// Slots made so far, for the serial numbers.
    made: u64,
    frames: u64,
    nodes: HashMap<NodeId, GpuSpent>,
    span: Duration,
    untimed: u64,
    lost: u64,
}

impl Timing {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let needed = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        Self {
            supported: device.features().contains(needed),
            on: AtomicBool::new(false),
            inner: std::sync::Mutex::new(State::default()),
        }
    }
}

/// The two timestamps of one encoder: where they go.
pub(crate) struct Stamp {
    set: wgpu::QuerySet,
    serial: u64,
    pair: u32,
}

/// A command encoder that is timed for the node being cooked while timing is on
/// ([`Gpu::encoder`]). Used as a `wgpu::CommandEncoder`; [`Encoder::finish`] ends it.
pub(crate) struct Encoder {
    encoder: wgpu::CommandEncoder,
    stamp: Option<Stamp>,
}

impl Deref for Encoder {
    type Target = wgpu::CommandEncoder;
    fn deref(&self) -> &wgpu::CommandEncoder {
        &self.encoder
    }
}

impl DerefMut for Encoder {
    fn deref_mut(&mut self) -> &mut wgpu::CommandEncoder {
        &mut self.encoder
    }
}

impl Encoder {
    /// Writes the closing timestamp and finishes the encoder.
    pub(crate) fn finish(self) -> wgpu::CommandBuffer {
        self.finish_stamped().0
    }

    /// [`Encoder::finish`], also handing back where its timestamps went, so a caller that does not
    /// submit the buffer (its recording failed) can take the pair back ([`Gpu::void_stamp`]).
    pub(crate) fn finish_stamped(mut self) -> (wgpu::CommandBuffer, Option<(u64, u32)>) {
        let id = self.stamp.take().map(|s| {
            self.encoder.write_timestamp(&s.set, 2 * s.pair + 1);
            (s.serial, s.pair)
        });
        (self.encoder.finish(), id)
    }
}

impl Gpu {
    /// A command encoder, timed for the node this thread cooks while timing is on (see
    /// [`crate::timing`]). Everything in this crate records through it.
    pub(crate) fn encoder(&self, label: &str) -> Encoder {
        let mut encoder =
            self.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        let stamp = self.stamp();
        if let Some(s) = &stamp {
            encoder.write_timestamp(&s.set, 2 * s.pair);
        }
        Encoder { encoder, stamp }
    }

    /// A pair of queries in the open slot for the node this thread cooks; `None` while timing is
    /// off, outside a cook, or without room.
    fn stamp(&self) -> Option<Stamp> {
        let timing = &self.inner.timing;
        if !timing.on.load(Ordering::Relaxed) {
            return None;
        }
        let node = penumbra_core::cooking::node()?;
        let mut st = lock(&timing.inner);
        let Some(slot) = st.open.as_mut() else {
            st.untimed += 1;
            return None;
        };
        let pair = u32::try_from(slot.pairs.len()).ok().filter(|p| 2 * p + 2 <= QUERIES);
        let Some(pair) = pair else {
            st.untimed += 1;
            return None;
        };
        slot.pairs.push(Some(node));
        Some(Stamp { set: slot.set.clone(), serial: slot.serial, pair })
    }

    /// Takes back the pair of an encoder that was never submitted: its queries hold nothing.
    pub(crate) fn void_stamp(&self, (serial, pair): (u64, u32)) {
        let mut st = lock(&self.inner.timing.inner);
        if let Some(slot) = st.open.as_mut().filter(|s| s.serial == serial)
            && let Some(p) = slot.pairs.get_mut(pair as usize)
        {
            *p = None;
        }
    }

    /// Whether this device can time GPU work, and whether it does now.
    pub fn timing_state(&self) -> TimingState {
        let timing = &self.inner.timing;
        if !timing.supported {
            TimingState::Unsupported
        } else if timing.on.load(Ordering::Relaxed) {
            TimingState::On
        } else {
            TimingState::Off
        }
    }

    /// Times GPU work per node until `until` (`None`: stop now). From the next cook pass on;
    /// nothing happens on a device without timestamp queries. A call with a later time extends.
    /// Call it between cook passes, from the thread that cooks.
    pub fn time_gpu_until(&self, until: Option<Instant>) {
        let timing = &self.inner.timing;
        if !timing.supported {
            return;
        }
        lock(&timing.inner).until = until;
        self.end_timing(false);
    }

    /// Ends the timing of a cook pass: the open slot's timestamps are resolved into a buffer
    /// that maps once the GPU got there, and a slot opens for the next pass while timing stays
    /// on. Never waits. The cook thread calls it at the end of every pass, before the pass's
    /// last submission, so the resolve goes out with it.
    pub fn end_timing_frame(&self) {
        if self.inner.timing.supported {
            self.end_timing(true);
        }
    }

    /// [`Gpu::end_timing_frame`]; `pass` says whether a pass ended, so a slot without work
    /// counts as a pass at no GPU time.
    fn end_timing(&self, pass: bool) {
        let timing = &self.inner.timing;
        let (closing, on) = {
            let mut st = lock(&timing.inner);
            let on = st.until.is_some_and(|t| Instant::now() < t);
            let closing = match st.open.take() {
                Some(slot) if slot.pairs.is_empty() => {
                    if pass {
                        st.frames += 1;
                    }
                    st.spare.push(slot);
                    None
                }
                other => other,
            };
            (closing, on)
        };
        if let Some(slot) = closing {
            self.resolve(slot);
        }
        let mut open = None;
        if on {
            let made = {
                let mut st = lock(&timing.inner);
                match st.spare.pop() {
                    Some(s) => {
                        open = Some(s);
                        None
                    }
                    None if st.flight.len() < SLOTS => {
                        st.made += 1;
                        Some(st.made)
                    }
                    None => None,
                }
            };
            if let Some(serial) = made {
                open = self.new_slot(serial);
            }
        }
        let mut st = lock(&timing.inner);
        if let Some(mut slot) = open {
            slot.pairs.clear();
            st.open = Some(slot);
        }
        timing.on.store(on, Ordering::Relaxed);
    }

    /// Records the resolve and copy of a closed slot and starts mapping once they went out.
    fn resolve(&self, slot: Slot) {
        let n = 2 * slot.pairs.len() as u32;
        let bytes = u64::from(n) * wgpu::QUERY_SIZE as u64;
        let state = Arc::new(AtomicU8::new(WAITING));
        let set = state.clone();
        let to_map = slot.read.clone();
        let map: crate::deferred::After = Box::new(move || {
            to_map.map_async(wgpu::MapMode::Read, 0..bytes, move |r| {
                set.store(if r.is_ok() { READY } else { FAILED }, Ordering::Release);
            });
        });
        // Recorded outside any node's scope by the caller, so these commands are not timed.
        let error = penumbra_core::cooking::outside(|| {
            self.record_commands(
                "gpu timing",
                |encoder| {
                    encoder.resolve_query_set(&slot.set, 0..n, &slot.resolve, 0);
                    encoder.copy_buffer_to_buffer(&slot.resolve, 0, &slot.read, 0, bytes);
                },
                Some(map),
            )
        });
        let mut st = lock(&self.inner.timing.inner);
        match error {
            Some(e) => {
                log::warn!("GPU timing: the timestamps were not resolved: {e}");
                st.lost += 1;
                st.spare.push(slot);
            }
            None => st.flight.push(Flight { slot, state, age: ReadAge::new(self) }),
        }
    }

    fn new_slot(&self, serial: u64) -> Option<Slot> {
        let device = self.device();
        let bytes = u64::from(QUERIES) * wgpu::QUERY_SIZE as u64;
        let (slot, error) = penumbra_core::cooking::outside(|| {
            self.scoped(|| Slot {
                serial,
                set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("gpu timing"),
                    ty: wgpu::QueryType::Timestamp,
                    count: QUERIES,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu timing resolve"),
                    size: bytes,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                read: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu timing read"),
                    size: bytes,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                pairs: Vec::new(),
            })
        });
        match error {
            Some(e) => {
                log::warn!("GPU timing: no query set: {e}");
                None
            }
            None => Some(slot),
        }
    }

    /// Adds the timestamps that arrived. [`Gpu::poll`] calls it; never waits.
    pub(crate) fn collect_timing(&self) {
        let timing = &self.inner.timing;
        if !timing.supported {
            return;
        }
        let mut st = lock(&timing.inner);
        if st.flight.is_empty() {
            return;
        }
        let period = f64::from(self.inner.queue.get_timestamp_period());
        let flights = std::mem::take(&mut st.flight);
        for f in flights {
            match f.state.load(Ordering::Acquire) {
                READY => {
                    let n = 2 * f.slot.pairs.len() as u64;
                    let bytes = n * wgpu::QUERY_SIZE as u64;
                    if let Ok(view) = f.slot.read.get_mapped_range(0..bytes) {
                        let ticks: Vec<u64> = bytemuck::pod_collect_to_vec(&view);
                        drop(view);
                        st.add(&f.slot.pairs, &ticks, period);
                    } else {
                        st.lost += 1;
                    }
                    f.slot.read.unmap();
                    st.spare.push(f.slot);
                }
                // A slot whose mapping failed or never came is let go: its buffer may still be
                // waiting to map.
                FAILED => st.lost += 1,
                _ if f.age.expired() => st.lost += 1,
                _ => st.flight.push(f),
            }
        }
    }

    /// The GPU time per node since the last reset; `reset` starts afresh. Pairs still in flight
    /// land in the new count.
    pub fn take_gpu_times(&self, reset: bool) -> GpuTimes {
        let state = self.timing_state();
        let mut st = lock(&self.inner.timing.inner);
        let mut nodes: Vec<(NodeId, GpuSpent)> = st.nodes.iter().map(|(k, v)| (*k, *v)).collect();
        nodes.sort_by_key(|(id, _)| *id);
        let out =
            GpuTimes { state, frames: st.frames, nodes, span: st.span, untimed: st.untimed, lost: st.lost };
        if reset {
            st.frames = 0;
            st.nodes.clear();
            st.span = Duration::ZERO;
            st.untimed = 0;
            st.lost = 0;
        }
        out
    }
}

impl State {
    /// Adds one pass's timestamps: `ticks` holds a start and an end per pair.
    fn add(&mut self, pairs: &[Option<NodeId>], ticks: &[u64], period: f64) {
        let mut first = u64::MAX;
        let mut last = 0;
        for (node, t) in pairs.iter().zip(ticks.as_chunks::<2>().0) {
            let (Some(node), [start, end]) = (node, t) else { continue };
            let Some(d) = end.checked_sub(*start) else { continue };
            let time = Duration::from_secs_f64(d as f64 * period / 1e9);
            if time > LONGEST {
                continue;
            }
            first = first.min(*start);
            last = last.max(*end);
            let spent = self.nodes.entry(*node).or_default();
            spent.encoders += 1;
            spent.time += time;
        }
        if let Some(d) = last.checked_sub(first) {
            self.span += Duration::from_secs_f64(d as f64 * period / 1e9);
        }
        self.frames += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pass, PassInput, Tex};
    use penumbra_core::cooking;

    /// A device of its own: timing is the device's, and the shared test device's other users
    /// would see it switch on and off.
    fn gpu() -> Option<Gpu> {
        Gpu::headless(crate::AdapterChoice::Software).ok()
    }

    const FILL: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(params.gain); }";

    fn fill(gpu: &Gpu) -> Pass {
        Pass::compile(gpu, "fill", FILL, &[penumbra_core::ParamSpec::float("gain", 0.5)]).unwrap()
    }

    fn render(gpu: &Gpu, pass: &Pass, target: &Tex) -> Result<(), penumbra_core::Error> {
        let none = |_: &str| None;
        pass.render(gpu, target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &none })
    }

    /// Polls until `frames` passes have arrived.
    fn arrived(gpu: &Gpu, frames: u64) -> GpuTimes {
        for _ in 0..1000 {
            gpu.poll();
            let t = gpu.take_gpu_times(false);
            if t.frames >= frames {
                return t;
            }
            gpu.settle(Duration::from_millis(10));
        }
        panic!("the timestamps did not arrive: {:?}", gpu.take_gpu_times(false));
    }

    #[test]
    fn work_is_timed_for_the_node_that_recorded_it_without_waiting() {
        let Some(gpu) = gpu() else { return };
        if gpu.timing_state() == TimingState::Unsupported {
            eprintln!("skipped: the software adapter has no timestamp queries inside encoders");
            return;
        }
        let pass = fill(&gpu);
        let target = Tex::new(&gpu, 256, 256, "t").unwrap();
        assert_eq!(gpu.timing_state(), TimingState::Off, "off until asked for");
        gpu.time_gpu_until(Some(Instant::now() + Duration::from_secs(60)));
        assert_eq!(gpu.timing_state(), TimingState::On);
        {
            let _deferral = gpu.defer();
            {
                let _a = cooking::enter(NodeId(1));
                render(&gpu, &pass, &target).unwrap();
                render(&gpu, &pass, &target).unwrap();
            }
            {
                let _b = cooking::enter(NodeId(2));
                render(&gpu, &pass, &target).unwrap();
                // A copy through an encoder of its own counts for the node too.
                gpu.copy(&target, &Tex::new(&gpu, 256, 256, "copy").unwrap()).unwrap();
            }
            // Nobody's work is not timed.
            render(&gpu, &pass, &target).unwrap();
            gpu.end_timing_frame();
            // The resolve waits with the passes for the deferral's end: no submission of its own.
            assert!(!lock(&gpu.inner.deferred).is_empty());
        }
        // Nothing arrives without a poll: ending the pass did not wait for the GPU.
        assert_eq!(gpu.take_gpu_times(false).frames, 0);
        let t = arrived(&gpu, 1);
        let encoders: Vec<(NodeId, u64)> = t.nodes.iter().map(|(id, s)| (*id, s.encoders)).collect();
        assert_eq!(encoders, [(NodeId(1), 2), (NodeId(2), 2)], "{t:?}");
        let sum: Duration = t.nodes.iter().map(|(_, s)| s.time).sum();
        assert!(t.span >= sum.mul_f64(0.5), "the span covers the timed work: {t:?}");
        assert_eq!((t.untimed, t.lost), (0, 0));
        // A pass without GPU work counts at no time.
        gpu.end_timing_frame();
        assert_eq!(arrived(&gpu, 2).nodes, t.nodes);
        // Off: nothing is stamped any more.
        gpu.time_gpu_until(None);
        assert_eq!(gpu.timing_state(), TimingState::Off);
        {
            let _a = cooking::enter(NodeId(1));
            render(&gpu, &pass, &target).unwrap();
        }
        gpu.end_timing_frame();
        gpu.settle(Duration::from_secs(1));
        gpu.poll();
        let off = gpu.take_gpu_times(true);
        assert_eq!((off.frames, off.nodes), (2, t.nodes));
        assert_eq!(gpu.take_gpu_times(false).frames, 0, "reset");
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }

    #[test]
    fn an_encoder_that_failed_gives_its_timestamps_back() {
        let Some(gpu) = gpu() else { return };
        if gpu.timing_state() == TimingState::Unsupported {
            return;
        }
        let pass = fill(&gpu);
        // Passes render into half floats; a target of 32-bit floats does not fit the pipeline.
        let wrong = Tex::new_float(&gpu, 4, 4, "wrong").unwrap();
        let right = Tex::new(&gpu, 4, 4, "right").unwrap();
        gpu.time_gpu_until(Some(Instant::now() + Duration::from_secs(60)));
        {
            let _deferral = gpu.defer();
            {
                let _a = cooking::enter(NodeId(1));
                assert!(render(&gpu, &pass, &wrong).is_err());
            }
            let _b = cooking::enter(NodeId(2));
            render(&gpu, &pass, &right).unwrap();
            gpu.end_timing_frame();
        }
        let t = arrived(&gpu, 1);
        assert_eq!(t.nodes.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [NodeId(2)], "{t:?}");
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }

    /// CPU time to record and submit small passes, timed and not, on the hardware GPU.
    #[test]
    #[ignore = "times the recording on the hardware GPU: cargo test -p penumbra-gpu --release timing_cost -- --ignored --nocapture"]
    fn timing_cost() {
        let gpu = Gpu::headless(crate::AdapterChoice::Hardware).unwrap();
        eprintln!("{} ({:?})", gpu.name(), gpu.timing_state());
        let pass = fill(&gpu);
        let target = Tex::new(&gpu, 64, 64, "t").unwrap();
        let passes = 80;
        let frame = |gpu: &Gpu| {
            let t0 = Instant::now();
            {
                let _deferral = gpu.defer();
                let scope = cooking::enter(NodeId(1));
                for _ in 0..passes {
                    render(gpu, &pass, &target).unwrap();
                }
                drop(scope);
                gpu.end_timing_frame();
            }
            let spent = t0.elapsed();
            gpu.poll();
            spent
        };
        for round in 0..3 {
            for on in [false, true] {
                gpu.time_gpu_until(on.then(|| Instant::now() + Duration::from_secs(60)));
                let mut times: Vec<f64> = (0..300).map(|_| frame(&gpu).as_secs_f64() * 1e6).collect();
                gpu.wait(Duration::from_secs(10)).unwrap();
                times.drain(..20);
                times.sort_by(f64::total_cmp);
                let median = times[times.len() / 2];
                eprintln!(
                    "round {round} timing {on}: {median:.1} us a frame of {passes} passes, {:.2} us a pass",
                    median / passes as f64
                );
            }
        }
        let t = gpu.take_gpu_times(false);
        eprintln!("{} frames timed, {:?} GPU per frame", t.frames, t.span / t.frames.max(1) as u32);
        // Recording alone: empty encoders, finished and dropped, never submitted.
        for on in [false, true, false, true] {
            gpu.time_gpu_until(on.then(|| Instant::now() + Duration::from_secs(60)));
            let _a = cooking::enter(NodeId(1));
            let n = 2000;
            let t0 = Instant::now();
            for _ in 0..n {
                let e = gpu.encoder("e");
                let (_buffer, stamp) = e.finish_stamped();
                if let Some(s) = stamp {
                    gpu.void_stamp(s);
                }
                if gpu.inner.timing.on.load(Ordering::Relaxed) {
                    // Keep the slot from filling: 2000 encoders need 4000 queries.
                    if let Some(slot) = lock(&gpu.inner.timing.inner).open.as_mut() {
                        slot.pairs.clear();
                    }
                }
            }
            eprintln!("timing {on}: {:.2} us an empty encoder", t0.elapsed().as_secs_f64() * 1e6 / n as f64);
        }
    }

    #[test]
    fn a_device_without_timestamps_times_nothing_and_works() {
        let Ok(gpu) = Gpu::headless_without_timestamps(crate::AdapterChoice::Software) else { return };
        assert_eq!(gpu.timing_state(), TimingState::Unsupported);
        gpu.time_gpu_until(Some(Instant::now() + Duration::from_secs(60)));
        assert_eq!(gpu.timing_state(), TimingState::Unsupported);
        let pass = fill(&gpu);
        let target = Tex::new(&gpu, 4, 4, "t").unwrap();
        {
            let _a = cooking::enter(NodeId(1));
            render(&gpu, &pass, &target).unwrap();
        }
        gpu.end_timing_frame();
        gpu.poll();
        let t = gpu.take_gpu_times(false);
        assert_eq!((t.state, t.frames, t.nodes.len()), (TimingState::Unsupported, 0, 0));
        let [r, ..] = gpu.read(&target).unwrap().get(1, 1).unwrap();
        assert_eq!(r, 0.5);
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }

    #[test]
    fn a_pass_adds_by_node_and_skips_what_is_no_measurement() {
        let mut st = State::default();
        let (a, b) = (NodeId(1), NodeId(2));
        // Ticks of 1 ns: a 100, b 50, a again 25, a voided pair, one ending before it starts.
        let ticks = [1000, 1100, 1100, 1150, 1200, 1225, 0, 5, 900, 800];
        st.add(&[Some(a), Some(b), Some(a), None, Some(b)], &ticks, 1.0);
        assert_eq!(st.frames, 1);
        assert_eq!(st.nodes[&a], GpuSpent { encoders: 2, time: Duration::from_nanos(125) });
        assert_eq!(st.nodes[&b], GpuSpent { encoders: 1, time: Duration::from_nanos(50) });
        assert_eq!(st.span, Duration::from_nanos(225));
        // A pass without valid pairs counts, at no span.
        st.add(&[None], &[1, 2], 1.0);
        assert_eq!((st.frames, st.span), (2, Duration::from_nanos(225)));
    }
}
