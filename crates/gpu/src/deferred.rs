//! Passes recorded now and submitted later, together.
//!
//! A submission that carries work costs the CPU 60 to 75 microseconds on Direct3D 12, more than
//! recording a pass. So [`Pass::render`](crate::Pass::render) does not submit by itself while
//! its thread defers ([`Gpu::defer`]; the cook thread does for the whole cook pass): each pass
//! is recorded into a command buffer of its own (its errors still name its node) and waits in
//! the thread's list. Its uniforms go into a block of uniform memory of the list, written with
//! a single upload when the list goes out.
//!
//! A deferring thread still submits every [`FLUSH_AT`] passes, so the GPU works alongside.
//!
//! Order stays as recorded. The waiting passes go out before anything else reaches the queue:
//! [`Gpu::submit`] puts them first, and [`Gpu::queue`] sends them before handing out the queue.
//! An upload into a texture that a waiting pass reads (a video frame, an image) therefore lands
//! after that pass, as it would without deferring. A readback submits, so it sees every pass
//! recorded before it. Sending them never waits for the GPU.
//!
//! Any thread's submission sends every thread's list: a thread that hands a texture to another
//! one (or lets it go, so another reuses it) can rely on the queue having seen its passes before
//! the other thread's work.
//!
//! Work that should go out now, without waiting for the end of the cook pass, goes through a
//! [`Batch`](crate::Batch) (a simulation that starts a long GPU job, say) or [`Gpu::submit`].
//! Either way [`Gpu::done`] tells without blocking when it has finished.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

use penumbra_core::lock;

use crate::Gpu;

/// Bytes of one block of uniform memory. A pass takes three slots of 256 bytes or more, so a
/// block holds the uniforms of about 300 passes; a list of more uses a second block.
pub(crate) const BLOCK: u64 = 256 * 1024;

/// Passes a deferring thread collects before it submits them after all. Holding back every pass
/// of a cook pass leaves the GPU idle until the end, and a readback in the cook (`chan.from_tex`)
/// then waits for all of it; a submission every few passes keeps the GPU busy alongside. In a
/// project of 60 passes a frame 8 took 0.6 ms less per frame than holding all, and 4 about as
/// much as 8.
const FLUSH_AT: usize = 8;

/// Blocks kept for reuse after the list that used them went away.
const SPARE: usize = 4;

/// One buffer of uniform memory and the bytes waiting to be written into it. Reused after a
/// submission: a later upload into it is ordered after the passes that read it, since both go
/// through the one queue.
pub(crate) struct Block {
    buffer: wgpu::Buffer,
    staged: Vec<u8>,
    /// Bind groups of the pass globals over this buffer, by the size of the `params` uniform
    /// (the other two are 16 bytes each). Dynamic offsets pick the slots.
    groups: HashMap<u64, wgpu::BindGroup>,
}

/// The passes one thread recorded and not yet submitted.
struct List {
    thread: ThreadId,
    /// How many [`Deferral`]s the thread holds; 0 for a thread that does not defer, whose
    /// list lives only until its pass is submitted.
    depth: u32,
    /// The blocks of the list; the waiting passes use the first `used`, the last of those
    /// still filling.
    blocks: Vec<Block>,
    used: usize,
    buffers: Vec<wgpu::CommandBuffer>,
}

/// What waits to be submitted, by thread.
#[derive(Default)]
pub(crate) struct Deferred {
    lists: Vec<List>,
    spare: Vec<Block>,
}

impl Deferred {
    fn is_empty(&self) -> bool {
        self.lists.iter().all(|l| l.buffers.is_empty() && l.used == 0)
    }

    /// The list of `thread`, made when it has none; `None` only if that failed.
    fn list(&mut self, thread: ThreadId) -> Option<&mut List> {
        if !self.lists.iter().any(|l| l.thread == thread) {
            self.lists.push(List { thread, depth: 0, blocks: Vec::new(), used: 0, buffers: Vec::new() });
        }
        self.lists.iter_mut().find(|l| l.thread == thread)
    }

    /// Lets go of the lists of threads that no longer defer and have nothing waiting.
    fn prune(&mut self) {
        let (gone, kept): (Vec<List>, Vec<List>) = std::mem::take(&mut self.lists)
            .into_iter()
            .partition(|l| l.depth == 0 && l.buffers.is_empty() && l.used == 0);
        self.lists = kept;
        for mut b in gone.into_iter().flat_map(|l| l.blocks) {
            if self.spare.len() < SPARE {
                b.staged.clear();
                self.spare.push(b);
            }
        }
    }
}

/// Where a pass's three uniforms went, for `set_bind_group`.
pub(crate) struct Slots {
    pub group: wgpu::BindGroup,
    pub offsets: [u32; 3],
}

/// Keeps the passes of the thread that made it waiting until it is dropped (see
/// [`Gpu::defer`]).
pub struct Deferral<'a> {
    gpu: &'a Gpu,
    thread: ThreadId,
}

impl Drop for Deferral<'_> {
    fn drop(&mut self) {
        let last = {
            let mut d = lock(&self.gpu.inner.deferred);
            d.list(self.thread).is_none_or(|list| {
                list.depth = list.depth.saturating_sub(1);
                list.depth == 0
            })
        };
        if last {
            self.gpu.flush();
            lock(&self.gpu.inner.deferred).prune();
        }
    }
}

/// Whether work submitted before it was asked for has finished on the GPU ([`Gpu::done`]).
/// Cheap to clone and to ask.
#[derive(Clone, Debug)]
pub struct Done(Arc<AtomicBool>);

impl Done {
    /// Never waits. Turns true during a [`Gpu::poll`] (the cook thread polls once a frame)
    /// after the GPU finished.
    pub fn is_done(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Gpu {
    /// Lets the calling thread's passes wait and go out in one submission when the returned
    /// guard drops (or earlier, when anything submits or uses the queue). For a cook pass: one
    /// submission instead of one per pass. Guards nest; the outermost one submits.
    pub fn defer(&self) -> Deferral<'_> {
        let thread = std::thread::current().id();
        if let Some(list) = lock(&self.inner.deferred).list(thread) {
            list.depth += 1;
        }
        Deferral { gpu: self, thread }
    }

    /// Submits the passes waiting, of every thread, if any. Does not wait for the GPU.
    pub fn flush(&self) {
        if lock(&self.inner.deferred).is_empty() {
            return;
        }
        self.submit([]);
    }

    /// Sends what waits and tells when everything submitted so far has finished on the GPU,
    /// without blocking: for work spread over several frames that must not stall a cook pass.
    pub fn done(&self) -> Done {
        self.flush();
        let flag = Arc::new(AtomicBool::new(false));
        let set = flag.clone();
        self.inner.queue.on_submitted_work_done(move || set.store(true, Ordering::Release));
        Done(flag)
    }

    /// Records one pass: its uniforms go into a block, `record` fills a command encoder of its
    /// own, which waits in the thread's list. Returns the first error the recording raised; a
    /// pass that failed is not submitted. Submits at once unless this thread defers.
    pub(crate) fn record(
        &self,
        label: &str,
        parts: [&[u8]; 3],
        record: impl FnOnce(&mut wgpu::CommandEncoder, &Slots),
    ) -> Option<String> {
        self.record_many(label, &[parts], |encoder, slots| {
            if let Some(s) = slots.first() {
                record(encoder, s);
            }
        })
    }

    /// Records several passes into one command encoder, each with its own uniforms (`parts`,
    /// one entry per pass; `record` gets their slots in the same order). One encoder costs the
    /// CPU less than one per pass, which counts for a simulation's dozens of steps a frame.
    pub(crate) fn record_many(
        &self,
        label: &str,
        parts: &[[&[u8]; 3]],
        record: impl FnOnce(&mut wgpu::CommandEncoder, &[Slots]),
    ) -> Option<String> {
        let thread = std::thread::current().id();
        let mut d = lock(&self.inner.deferred);
        let (buffer, error) = self.scoped(|| {
            let slots = parts.iter().map(|p| self.slots(&mut d, thread, *p)).collect::<Option<Vec<_>>>()?;
            let mut encoder =
                self.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            record(&mut encoder, &slots);
            Some(encoder.finish())
        });
        let Some(list) = d.list(thread) else { return Some("no list to record into".into()) };
        // A buffer that failed is invalid; submitting it would raise the error again, on
        // whatever submits next.
        match (buffer, error) {
            (_, Some(e)) => return Some(e),
            (None, None) => return Some("the pass's uniforms do not fit a block".into()),
            (Some(b), None) => list.buffers.push(b),
        }
        let defers = list.depth > 0 && list.buffers.len() < FLUSH_AT;
        drop(d);
        if !defers {
            let ((), error) = self.scoped(|| self.flush());
            return error;
        }
        None
    }

    /// Stages the three uniforms at aligned offsets of the filling block of `thread`'s list (a
    /// new one when it is full) and returns the bind group over them. `None` when they do not
    /// fit a block at all, which no declared set of parameters reaches.
    fn slots(&self, d: &mut Deferred, thread: ThreadId, parts: [&[u8]; 3]) -> Option<Slots> {
        let align = self.layouts().uniform_align;
        let sizes = parts.map(|p| p.len().max(16).next_multiple_of(16) as u64);
        let need: u64 = sizes.iter().map(|s| s.next_multiple_of(align)).sum();
        if need > BLOCK {
            log::error!("pass uniforms of {need} bytes do not fit a block of {BLOCK}");
            return None;
        }
        let mut spare = d.spare.pop();
        let list = d.list(thread)?;
        let full = list.used == 0
            || list
                .blocks
                .get(list.used - 1)
                .is_none_or(|b| (b.staged.len() as u64).next_multiple_of(align) + need > BLOCK);
        if full {
            if list.blocks.len() <= list.used {
                let block = spare.take().unwrap_or_else(|| self.new_block());
                list.blocks.push(block);
            }
            list.used += 1;
        }
        let used = list.used;
        let block = list.blocks.get_mut(used - 1)?;
        let mut offsets = [0u32; 3];
        for ((offset, part), size) in offsets.iter_mut().zip(parts).zip(sizes) {
            let at = (block.staged.len() as u64).next_multiple_of(align);
            block.staged.resize(at as usize, 0);
            block.staged.extend_from_slice(part);
            block.staged.resize((at + size) as usize, 0);
            *offset = u32::try_from(at).ok()?;
        }
        let params = sizes[2];
        let group = match block.groups.get(&params) {
            Some(g) => g.clone(),
            None => {
                let g = self.globals_group(&block.buffer, params);
                block.groups.insert(params, g.clone());
                g
            }
        };
        if let Some(b) = spare {
            d.spare.push(b);
        }
        Some(Slots { group, offsets })
    }

    fn new_block(&self) -> Block {
        let buffer = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("pass uniforms"),
            size: BLOCK,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Block { buffer, staged: Vec::new(), groups: HashMap::new() }
    }

    fn globals_group(&self, buffer: &wgpu::Buffer, params: u64) -> wgpu::BindGroup {
        let binding = |size: u64| {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: wgpu::BufferSize::new(size),
            })
        };
        self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass globals"),
            layout: &self.layouts().group0,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: binding(16) },
                wgpu::BindGroupEntry { binding: 1, resource: binding(16) },
                wgpu::BindGroupEntry { binding: 2, resource: binding(params) },
            ],
        })
    }

    /// Submits what waits, followed by `more`, in one submission: the uniform uploads are
    /// queued first, so they land before the passes that read them. The caller holds the
    /// shared side of the submit lock. The lists stay locked until the submission is done, so
    /// another thread's submission cannot overtake it.
    pub(crate) fn submit_deferred(
        &self,
        more: impl IntoIterator<Item = wgpu::CommandBuffer>,
    ) -> wgpu::SubmissionIndex {
        let mut d = lock(&self.inner.deferred);
        let mut buffers = Vec::new();
        for list in &mut d.lists {
            let used = std::mem::take(&mut list.used);
            for b in list.blocks.iter_mut().take(used) {
                if !b.staged.is_empty() {
                    self.inner.queue.write_buffer(&b.buffer, 0, &b.staged);
                    b.staged.clear();
                }
            }
            buffers.append(&mut list.buffers);
        }
        d.prune();
        buffers.extend(more);
        self.inner.queue.submit(buffers)
    }
}

#[cfg(test)]
mod tests {
    use penumbra_core::Value;

    use crate::{Pass, PassInput, Tex};

    /// A device of its own: a deferral on the shared test device would be flushed by the other
    /// tests' passes, and theirs would wait for its end.
    fn gpu() -> Option<crate::Gpu> {
        crate::Gpu::headless(crate::AdapterChoice::Software).ok()
    }

    const COPY: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f {
        return textureLoad(in0, vec2i(v.pos.xy), 0) * params.gain;
    }";

    fn half(x: f32) -> [u8; 2] {
        half::f16::from_f32(x).to_le_bytes()
    }

    fn filled(gpu: &crate::Gpu, x: f32) -> Tex {
        let mut bytes: Vec<u8> = (0..4 * 2 * 4).flat_map(|_| half(x)).collect();
        gpu.upload_rgba16f(4, 2, &mut bytes, "filled").unwrap()
    }

    #[test]
    fn an_upload_lands_after_the_passes_recorded_before_it() {
        let Some(gpu) = gpu() else { return };
        let pass =
            Pass::compile(&gpu, "copy", COPY, &[penumbra_core::ParamSpec::float("gain", 1.0)]).unwrap();
        let source = filled(&gpu, 1.0);
        let (before, after) =
            (Tex::new(&gpu, 4, 2, "before").unwrap(), Tex::new(&gpu, 4, 2, "after").unwrap());
        let none = |_: &str| None;
        {
            let _deferral = gpu.defer();
            let input = PassInput { inputs: &[Some(&source)], seconds: 0.0, frame: 0, param: &none };
            pass.render(&gpu, &before, &input).unwrap();
            assert!(!lock(&gpu.inner.deferred).is_empty(), "the pass waits");
            let mut bytes: Vec<u8> = (0..4 * 2 * 4).flat_map(|_| half(2.0)).collect();
            gpu.write_rgba16f(&source, &mut bytes).unwrap();
            pass.render(&gpu, &after, &input).unwrap();
        }
        assert!(lock(&gpu.inner.deferred).is_empty(), "the deferral's end submitted");
        assert_eq!(gpu.read(&before).unwrap().get(1, 1).unwrap(), [1.0; 4]);
        assert_eq!(gpu.read(&after).unwrap().get(1, 1).unwrap(), [2.0; 4]);
        assert_eq!(gpu.errors().count, 0);
    }

    use penumbra_core::lock;

    #[test]
    fn a_failing_pass_names_itself_and_the_others_go_out() {
        let Some(gpu) = gpu() else { return };
        let pass =
            Pass::compile(&gpu, "copy", COPY, &[penumbra_core::ParamSpec::float("gain", 1.0)]).unwrap();
        let source = filled(&gpu, 0.5);
        // Passes render into half floats; a target of 32-bit floats does not fit the pipeline.
        let wrong = Tex::new_float(&gpu, 4, 2, "wrong").unwrap();
        let (a, b) = (Tex::new(&gpu, 4, 2, "a").unwrap(), Tex::new(&gpu, 4, 2, "b").unwrap());
        let two = |name: &str| (name == "gain").then_some(Value::Float(2.0));
        let input = PassInput { inputs: &[Some(&source)], seconds: 0.0, frame: 0, param: &two };
        {
            let _deferral = gpu.defer();
            pass.render(&gpu, &a, &input).unwrap();
            let e = pass.render(&gpu, &wrong, &input).err().unwrap();
            assert!(e.0.starts_with("copy:"), "{e}");
            pass.render(&gpu, &b, &input).unwrap();
        }
        assert_eq!(gpu.read(&a).unwrap().get(0, 0).unwrap(), [1.0; 4]);
        assert_eq!(gpu.read(&b).unwrap().get(3, 1).unwrap(), [1.0; 4]);
        assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    }

    #[test]
    fn many_passes_go_out_in_parts_and_keep_their_own_uniforms() {
        let Some(gpu) = gpu() else { return };
        let pass =
            Pass::compile(&gpu, "copy", COPY, &[penumbra_core::ParamSpec::float("gain", 1.0)]).unwrap();
        let source = filled(&gpu, 1.0);
        let n = 2 * (super::BLOCK / (3 * gpu.layouts().uniform_align)) as usize + 5;
        let targets: Vec<Tex> = (0..n).map(|_| Tex::new(&gpu, 4, 2, "t").unwrap()).collect();
        {
            let _deferral = gpu.defer();
            // A batch inside the cook pass collects the passes like the cook pass's deferral.
            let batch = crate::Batch::new(&gpu, "inner");
            for (i, t) in targets.iter().enumerate() {
                let gain = move |name: &str| (name == "gain").then_some(Value::Float(i as f64 / 8.0));
                let input = PassInput { inputs: &[Some(&source)], seconds: 0.0, frame: 0, param: &gain };
                pass.render(&gpu, t, &input).unwrap();
            }
            assert!(lock(&gpu.inner.deferred).lists.iter().all(|l| l.buffers.len() < super::FLUSH_AT));
            // A batch goes out when submitted, also inside the deferral.
            let done = batch.submit();
            assert!(lock(&gpu.inner.deferred).is_empty());
            let _ = done.is_done();
        }
        for (i, t) in targets.iter().enumerate().step_by(37) {
            let [r, ..] = gpu.read(t).unwrap().get(2, 1).unwrap();
            assert_eq!(r, half::f16::from_f32(i as f32 / 8.0).to_f32(), "pass {i}");
        }
        assert_eq!(gpu.errors().count, 0);
    }

    #[test]
    fn each_thread_defers_its_own_passes_and_a_submission_sends_all() {
        let Some(gpu) = gpu() else { return };
        let pass =
            Pass::compile(&gpu, "copy", COPY, &[penumbra_core::ParamSpec::float("gain", 1.0)]).unwrap();
        let source = filled(&gpu, 1.0);
        let mine = Tex::new(&gpu, 4, 2, "a").unwrap();
        let theirs = Tex::new(&gpu, 4, 2, "b").unwrap();
        let at_once = Tex::new(&gpu, 4, 2, "c").unwrap();
        let render = |target: &Tex| {
            let none = |_: &str| None;
            let input = PassInput { inputs: &[Some(&source)], seconds: 0.0, frame: 0, param: &none };
            pass.render(&gpu, target, &input).unwrap();
        };
        let deferral = gpu.defer();
        render(&mine);
        std::thread::scope(|s| {
            s.spawn(|| {
                // A thread of its own (a simulation) defers into a list of its own.
                let deferral = gpu.defer();
                render(&theirs);
                assert_eq!(lock(&gpu.inner.deferred).lists.len(), 2);
                drop(deferral);
                // Its end sent both threads' passes, without waiting for the GPU.
                assert!(lock(&gpu.inner.deferred).is_empty());
                // A thread that does not defer submits at once.
                render(&at_once);
                assert!(lock(&gpu.inner.deferred).is_empty());
            });
        });
        drop(deferral);
        assert!(lock(&gpu.inner.deferred).lists.is_empty(), "lists of threads done deferring go");
        for t in [&mine, &theirs, &at_once] {
            assert_eq!(gpu.read(t).unwrap().get(0, 0).unwrap(), [1.0; 4]);
        }
        let done = gpu.done();
        for _ in 0..500 {
            if done.is_done() {
                break;
            }
            gpu.settle(std::time::Duration::from_millis(10));
            gpu.poll();
        }
        assert!(done.is_done());
    }
}
