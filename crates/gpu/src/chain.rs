//! Many small steps of one node recorded into one command encoder ([`Chain`]).
//!
//! Every command encoder costs the CPU tens of microseconds in wgpu's recording, tracking and
//! validation, and a deferring thread submits every few encoders ([`crate::deferred`]). A node
//! that runs a dozen small compute steps over several programs, a copy here and a readback there,
//! pays that a dozen times. A chain collects the steps first and records them all at once, in the
//! order they were added: compute dispatches that follow each other share one compute pass, and a
//! copy ends the pass and starts the next. A dispatch reads what the dispatches and copies before
//! it wrote, as in separate encoders.
//!
//! A chain records all of it or nothing. A step that cannot even be prepared (an entry point that
//! does not exist, too many workgroups) fails when it is added; anything the GPU refuses fails
//! [`Chain::run`]. Either way nothing of the chain is submitted, and readbacks added to it turn
//! failed. Keep one chain to one node: one invalid recording voids the whole encoder.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use penumbra_core::Error;

use crate::deferred::After;
use crate::{Buffer, Compute, Gpu, Step};

/// One dispatch, checked and with everything it binds, ready to record.
pub(crate) struct Ready {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) groups: [u32; 3],
    /// The three uniforms: the grid, the time, the params.
    pub(crate) parts: [Vec<u8>; 3],
    pub(crate) inputs: wgpu::BindGroup,
    /// The program's own buffers, set when `bind_buffers` says so.
    pub(crate) buffers: Option<wgpu::BindGroup>,
    /// The first dispatch of a set of buffers: it binds them.
    pub(crate) bind_buffers: bool,
}

/// Commands without uniforms (a copy).
type Commands = Box<dyn FnOnce(&mut wgpu::CommandEncoder)>;

enum Part {
    Pass(Vec<Ready>),
    Commands(Commands),
}

/// Steps of several compute programs, copies and readbacks, recorded into one command encoder
/// when [`Chain::run`] is called (see the module).
pub struct Chain {
    label: String,
    parts: Vec<Part>,
    /// What each added step is called, for an error.
    names: Vec<String>,
    after: Vec<After>,
    /// States of the readbacks added, set to failed when the chain does not go out.
    reads: Vec<Arc<AtomicU8>>,
}

impl Chain {
    /// An empty chain; `label` names its command encoder and compute passes.
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            parts: Vec::new(),
            names: Vec::new(),
            after: Vec::new(),
            reads: Vec::new(),
        }
    }

    /// Whether nothing was added.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// [`Compute::run_with`] into the chain: `steps` with `buffers` bound to `@group(2)`. `Err`
    /// as there for what can be checked before recording; then nothing is added.
    pub fn add(
        &mut self,
        gpu: &Gpu,
        program: &Compute,
        buffers: &[&Buffer],
        steps: &[Step<'_>],
    ) -> Result<(), Error> {
        self.add_runs(gpu, program, &[(buffers, steps)])
    }

    /// [`Compute::run_many`] into the chain: each run's steps with its own buffers.
    pub fn add_runs(
        &mut self,
        gpu: &Gpu,
        program: &Compute,
        runs: &[(&[&Buffer], &[Step<'_>])],
    ) -> Result<(), Error> {
        let ready = program.prepare(gpu, runs)?;
        let entries: Vec<&str> = runs.iter().flat_map(|(_, steps)| steps.iter().map(|s| s.entry)).collect();
        self.names.push(format!("{} {}", program.label(), entries.join(", ")));
        if ready.is_empty() {
            return Ok(());
        }
        match self.parts.last_mut() {
            Some(Part::Pass(list)) => list.extend(ready),
            _ => self.parts.push(Part::Pass(ready)),
        }
        Ok(())
    }

    /// Commands without uniforms (a copy) recorded at this place in the chain, between compute
    /// passes. `name` says what they do, for an error.
    pub(crate) fn commands(&mut self, name: &str, record: impl FnOnce(&mut wgpu::CommandEncoder) + 'static) {
        self.names.push(name.to_string());
        self.parts.push(Part::Commands(Box::new(record)));
    }

    /// Work for after the chain was submitted (a readback maps its staging buffer), and the state
    /// to set to failed when the chain never goes out.
    pub(crate) fn after(&mut self, after: After, state: Arc<AtomicU8>) {
        self.after.push(after);
        self.reads.push(state);
    }

    /// Records everything added into one command encoder, which waits with the thread's other
    /// passes like any (submitted at once unless the thread defers). `Err` names the steps and
    /// the error the recording raised; then none of it runs.
    pub fn run(mut self, gpu: &Gpu) -> Result<(), Error> {
        if self.parts.is_empty() {
            return Ok(());
        }
        let after = std::mem::take(&mut self.after);
        let after: Option<After> = (!after.is_empty()).then(|| {
            Box::new(move || {
                for f in after {
                    f();
                }
            }) as After
        });
        let label = self.label.clone();
        let mut commands = Vec::new();
        let mut passes = Vec::new();
        // The parts in order, the commands taken out, so the recording borrows the passes only.
        let order: Vec<bool> = std::mem::take(&mut self.parts)
            .into_iter()
            .map(|p| match p {
                Part::Pass(list) => {
                    passes.push(list);
                    true
                }
                Part::Commands(c) => {
                    commands.push(c);
                    false
                }
            })
            .collect();
        let uniforms: Vec<[&[u8]; 3]> = passes
            .iter()
            .flatten()
            .map(|r| [r.parts[0].as_slice(), r.parts[1].as_slice(), r.parts[2].as_slice()])
            .collect();
        let error = gpu.record_then(
            &self.label,
            &uniforms,
            |encoder, slots| {
                let mut slots = slots.iter();
                let mut passes = passes.iter();
                let mut commands = commands.into_iter();
                for is_pass in order {
                    if !is_pass {
                        if let Some(c) = commands.next() {
                            c(encoder);
                        }
                        continue;
                    }
                    let Some(list) = passes.next() else { continue };
                    let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some(&label),
                        timestamp_writes: None,
                    });
                    for r in list {
                        let Some(s) = slots.next() else { break };
                        if r.bind_buffers
                            && let Some(g) = &r.buffers
                        {
                            cp.set_bind_group(crate::compute::BUFFER_GROUP, g, &[]);
                        }
                        cp.set_pipeline(&r.pipeline);
                        cp.set_bind_group(0, &s.group, &s.offsets);
                        cp.set_bind_group(1, &r.inputs, &[]);
                        let [x, y, z] = r.groups;
                        cp.dispatch_workgroups(x, y, z);
                    }
                }
            },
            after,
        );
        match error {
            Some(e) => Err(Error::new(format!("{}: {e}", self.names.join("; ")))),
            None => {
                self.reads.clear();
                Ok(())
            }
        }
    }
}

impl Drop for Chain {
    /// Readbacks of a chain that never went out never arrive: they fail now instead of waiting
    /// out their time.
    fn drop(&mut self) {
        for state in &self.reads {
            state.store(crate::buffer::FAILED, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use penumbra_core::{ParamSpec, Value, lock};

    use super::*;
    use crate::{Dispatch, Out, ReadState};

    /// A device of its own, so the other tests' work does not send this one's list out.
    fn gpu() -> Option<Gpu> {
        Gpu::headless(crate::AdapterChoice::Software).ok()
    }

    const ADD: &str = "@group(2) @binding(0) var<storage, read_write> a: array<u32>;
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3u) {
    if id.x < arrayLength(&a) { a[id.x] = a[id.x] + u32(params.k); }
}";

    const DOUBLE: &str = "@group(2) @binding(0) var<storage, read> src: array<u32>;
@group(2) @binding(1) var<storage, read_write> to: array<u32>;
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3u) {
    if id.x < arrayLength(&to) { to[id.x] = 2u * src[id.x]; }
}";

    fn programs(gpu: &Gpu) -> (Compute, Compute) {
        let add = Compute::compile(gpu, "add", ADD, &[ParamSpec::int("k", 0)]).unwrap();
        let double = Compute::compile(gpu, "double", DOUBLE, &[]).unwrap();
        (add, double)
    }

    fn adding(gpu: &Gpu, chain: &mut Chain, program: &Compute, b: &Buffer, k: i64) -> Result<(), Error> {
        let param = move |n: &str| (n == "k").then_some(Value::Int(k));
        let d = Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        chain.add(gpu, program, &[b], &[Step { entry: "main", out: Out::range(4), input: &d }])
    }

    fn doubling(gpu: &Gpu, chain: &mut Chain, program: &Compute, from: &Buffer, to: &Buffer) {
        let none = |_: &str| None;
        let d = Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &none };
        chain
            .add(gpu, program, &[from, to], &[Step { entry: "main", out: Out::range(4), input: &d }])
            .unwrap();
    }

    fn settled(gpu: &Gpu, read: &crate::BufferRead) -> ReadState {
        for _ in 0..500 {
            if read.state() != ReadState::Waiting {
                break;
            }
            gpu.settle(Duration::from_millis(10));
            gpu.poll();
        }
        read.state()
    }

    #[test]
    fn steps_of_several_programs_and_a_readback_go_out_as_one_encoder_in_order() {
        let Some(gpu) = gpu() else { return };
        let (add, double) = programs(&gpu);
        let a = Buffer::of::<u32>(&gpu, 4, "a").unwrap();
        let b = Buffer::of::<u32>(&gpu, 4, "b").unwrap();
        let c = Buffer::of::<u32>(&gpu, 4, "c").unwrap();
        let deferral = gpu.defer();
        let mut chain = Chain::new("test");
        adding(&gpu, &mut chain, &add, &a, 3).unwrap();
        doubling(&gpu, &mut chain, &double, &a, &b);
        adding(&gpu, &mut chain, &add, &b, 1).unwrap();
        // A readback between the steps sees what came before it, and ends the compute pass.
        let early = b.read_async_in(&mut chain, &gpu, None, None).unwrap();
        doubling(&gpu, &mut chain, &double, &b, &c);
        let late = c.read_async_in(&mut chain, &gpu, None, None).unwrap();
        assert!(lock(&gpu.inner.deferred).is_empty(), "nothing is recorded before the run");
        chain.run(&gpu).unwrap();
        assert_eq!(lock(&gpu.inner.deferred).waiting(), 1, "one command buffer for the whole chain");
        drop(deferral);
        assert_eq!(settled(&gpu, &early), ReadState::Ready);
        assert_eq!(settled(&gpu, &late), ReadState::Ready);
        assert_eq!(early.get::<u32>().unwrap(), [7; 4]);
        assert_eq!(late.get::<u32>().unwrap(), [14; 4]);
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }

    #[test]
    fn a_chain_that_fails_runs_nothing_and_its_readbacks_fail() {
        let Some(gpu) = gpu() else { return };
        let (add, double) = programs(&gpu);
        let a = Buffer::of::<u32>(&gpu, 4, "a").unwrap();
        let other = Buffer::of::<u32>(&gpu, 4, "other").unwrap();
        let _deferral = gpu.defer();
        let mut before = Chain::new("before");
        adding(&gpu, &mut before, &add, &other, 5).unwrap();
        let mut chain = Chain::new("failing");
        adding(&gpu, &mut chain, &add, &a, 1).unwrap();
        let read = a.read_async_in(&mut chain, &gpu, None, None).unwrap();
        // One buffer both read and written by the same dispatch: refused when recorded.
        doubling(&gpu, &mut chain, &double, &a, &a);
        let mut after = Chain::new("after");
        adding(&gpu, &mut after, &add, &other, 6).unwrap();
        before.run(&gpu).unwrap();
        let e = chain.run(&gpu).err().unwrap();
        assert!(e.0.starts_with("add main; buffer read_async; double main:"), "{e}");
        after.run(&gpu).unwrap();
        assert_eq!(read.state(), ReadState::Failed, "a readback of a chain that failed does not wait");
        assert_eq!(a.read::<u32>(&gpu).unwrap(), [0; 4], "nothing of the failed chain ran");
        assert_eq!(other.read::<u32>(&gpu).unwrap(), [11; 4], "the chains around it ran");
    }

    #[test]
    fn a_step_that_cannot_be_prepared_fails_when_added_and_a_dropped_chain_fails_its_readbacks() {
        let Some(gpu) = gpu() else { return };
        let (add, _) = programs(&gpu);
        let a = Buffer::of::<u32>(&gpu, 4, "a").unwrap();
        let mut chain = Chain::new("test");
        let none = |_: &str| None;
        let d = Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &none };
        let e = chain.add(&gpu, &add, &[&a], &[Step { entry: "nothing", out: Out::range(4), input: &d }]);
        assert!(e.is_err());
        assert!(
            chain.add(&gpu, &add, &[], &[Step { entry: "main", out: Out::range(4), input: &d }]).is_err()
        );
        assert!(chain.is_empty());
        let read = a.read_async_in(&mut chain, &gpu, None, None).unwrap();
        drop(chain);
        assert_eq!(read.state(), ReadState::Failed);
    }
}
