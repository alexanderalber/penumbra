//! Storage buffers: arrays of plain structs or scalars that compute programs read and write
//! ([`crate::Compute::encode_with`]), such as particles, grid cells or accumulators of
//! `atomic<i32>`, and uniforms of constants.
//!
//! A [`Buffer`] is bytes on the GPU, zero when made. The shader decides what they are: an
//! `array<P>` of a struct, an `array<atomic<i32>>`, a single struct of constants. On the CPU
//! the same bytes are a slice of any [`bytemuck::Pod`] type of the same layout ([`Buffer::write`],
//! [`Buffer::read`]). [`Buffer::read`] waits for the GPU and is meant for tests and diagnostics;
//! [`Buffer::read_async`] does not wait, for a few numbers every frame.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use bytemuck::Pod;
use penumbra_core::Error;

use crate::Gpu;
use crate::readback::{READ_TIMEOUT, ReadAge, ReadState};

struct Inner {
    buffer: wgpu::Buffer,
    bytes: u64,
}

/// Bytes on the GPU that compute programs bind as a storage buffer, or as a uniform when it is
/// small enough. Cheap to clone; clones share the buffer.
#[derive(Clone)]
pub struct Buffer(Arc<Inner>);

/// A buffer's identity ([`Buffer::key`]): equal for keys of the same buffer, never for two
/// buffers, even after the first is gone. It does not keep the buffer.
#[derive(Clone)]
pub struct BufferKey(Weak<Inner>);

impl BufferKey {
    /// Whether this is the key of `buffer`.
    pub fn is(&self, buffer: &Buffer) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&buffer.0))
    }

    /// Whether someone still holds the buffer.
    pub(crate) fn alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}

impl PartialEq for BufferKey {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for BufferKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BufferKey({:p})", self.0.as_ptr())
    }
}

impl Buffer {
    /// A buffer of `bytes` bytes, all zero. `Err` for 0 bytes, a size that is not a multiple of
    /// 4, and one above [`Buffer::max_bytes`].
    pub fn new(gpu: &Gpu, bytes: u64, label: &str) -> Result<Self, Error> {
        check_bytes(gpu, bytes, label)?;
        let mut usage =
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        // Small enough for a uniform: a program may bind it as one.
        if bytes <= gpu.device().limits().max_uniform_buffer_binding_size {
            usage |= wgpu::BufferUsages::UNIFORM;
        }
        let (buffer, error) = gpu.scoped(|| {
            gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes,
                usage,
                mapped_at_creation: false,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot create the buffer `{label}` of {bytes} bytes: {e}")));
        }
        Ok(Self(Arc::new(Inner { buffer, bytes })))
    }

    /// A buffer of `len` elements of `T`, all zero: as [`Buffer::new`] with their bytes.
    pub fn of<T: Pod>(gpu: &Gpu, len: u64, label: &str) -> Result<Self, Error> {
        let bytes = len
            .checked_mul(size_of::<T>() as u64)
            .ok_or_else(|| Error::new(format!("the buffer `{label}` of {len} elements is too large")))?;
        Self::new(gpu, bytes, label)
    }

    /// The largest buffer `gpu` binds: its storage binding limit, or its buffer size limit when
    /// that is lower. wgpu's default is 128 MiB.
    pub fn max_bytes(gpu: &Gpu) -> u64 {
        let limits = gpu.device().limits();
        limits.max_storage_buffer_binding_size.min(limits.max_buffer_size)
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        self.0.bytes
    }

    /// How many elements of `T` the buffer holds, whole ones only.
    pub fn len_of<T>(&self) -> u64 {
        self.0.bytes.checked_div(size_of::<T>() as u64).unwrap_or(0)
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.0.buffer
    }

    pub fn same(&self, other: &Buffer) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// What tells this buffer apart from every other one, without holding it: a node can
    /// remember which buffer a result was made from and still let a ring hand that buffer out.
    pub fn key(&self) -> BufferKey {
        BufferKey(Arc::downgrade(&self.0))
    }

    /// Held by someone besides the one asking (downstream data, a snapshot).
    pub(crate) fn shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// Replaces elements `first..first + values.len()`, counted in elements of `T`, for the
    /// dispatches recorded after this. `Err` when they reach past the end, or start or end off
    /// a multiple of 4 bytes (wgpu copies whole words).
    pub fn write<T: Pod>(&self, gpu: &Gpu, first: u64, values: &[T]) -> Result<(), Error> {
        let data: &[u8] = bytemuck::cast_slice(values);
        if data.is_empty() {
            return Ok(());
        }
        let len = data.len() as u64;
        let offset = first.saturating_mul(size_of::<T>() as u64);
        if offset.checked_add(len).is_none_or(|end| end > self.0.bytes) {
            return Err(Error::new(format!(
                "{} values from element {first} do not fit a buffer of {} bytes",
                values.len(),
                self.0.bytes
            )));
        }
        let align = wgpu::COPY_BUFFER_ALIGNMENT;
        if !offset.is_multiple_of(align) || !len.is_multiple_of(align) {
            return Err(Error::new(format!(
                "a write into a buffer starts and ends on a multiple of {align} bytes, not at byte \
                 {offset} with {len} bytes"
            )));
        }
        let ((), error) = gpu.scoped(|| gpu.queue().write_buffer(&self.0.buffer, offset, data));
        match error {
            Some(e) => Err(Error::new(format!("cannot write the buffer: {e}"))),
            None => Ok(()),
        }
    }

    /// Sets every byte to zero, after the dispatches recorded before this.
    pub fn clear(&self, gpu: &Gpu) -> Result<(), Error> {
        let ((), error) = gpu.scoped(|| {
            let mut encoder =
                gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("clear") });
            encoder.clear_buffer(&self.0.buffer, 0, None);
            gpu.submit([encoder.finish()]);
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot clear the buffer: {e}"))),
            None => Ok(()),
        }
    }

    /// Makes this handle a new buffer of `bytes` bytes that starts with as much of the old
    /// content as fits, zero beyond it. Clones made before keep the old buffer. `Err` as for
    /// [`Buffer::new`]; then this handle is unchanged.
    pub fn resize(&mut self, gpu: &Gpu, bytes: u64, label: &str) -> Result<(), Error> {
        if bytes == self.0.bytes {
            return Ok(());
        }
        let new = Self::new(gpu, bytes, label)?;
        let kept = bytes.min(self.0.bytes);
        let ((), error) = gpu.scoped(|| {
            let mut encoder = gpu
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("resize") });
            encoder.copy_buffer_to_buffer(&self.0.buffer, 0, &new.0.buffer, 0, kept);
            gpu.submit([encoder.finish()]);
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot copy the buffer into its new size: {e}")));
        }
        *self = new;
        Ok(())
    }

    /// The whole buffer as elements of `T`, after the dispatches recorded before this. Waits for
    /// the GPU: for tests and diagnostics. `Err` when the size is no whole number of `T`, or
    /// the GPU does not deliver in time.
    pub fn read<T: Pod>(&self, gpu: &Gpu) -> Result<Vec<T>, Error> {
        let size = size_of::<T>() as u64;
        if size == 0 || !self.0.bytes.is_multiple_of(size) {
            return Err(Error::new(format!(
                "a buffer of {} bytes is no whole number of elements of {size} bytes",
                self.0.bytes
            )));
        }
        let device = gpu.device();
        let (staging, error) = gpu.scoped(|| {
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buffer readback"),
                size: self.0.bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("buffer readback") });
            encoder.copy_buffer_to_buffer(&self.0.buffer, 0, &staging, 0, self.0.bytes);
            gpu.submit([encoder.finish()]);
            staging
        });
        if let Some(e) = error {
            return Err(Error::new(format!("buffer readback failed: {e}")));
        }
        let mapped = Arc::new(OnceLock::new());
        let set = mapped.clone();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = set.set(r);
        });
        gpu.poll_until(READ_TIMEOUT, || mapped.get().is_some())
            .map_err(|e| Error::new(format!("waiting for the GPU: {e}")))?;
        match mapped.get() {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(Error::new(format!("cannot map the readback buffer: {e}"))),
            None => return Err(Error::new("the GPU did not deliver the readback in time")),
        }
        let view = staging.get_mapped_range(..).map_err(|e| Error::new(format!("buffer readback: {e}")))?;
        let out: Vec<T> = bytemuck::pod_collect_to_vec(&view);
        drop(view);
        staging.unmap();
        Ok(out)
    }

    /// Starts reading the whole buffer back without waiting, after the dispatches recorded
    /// before this ([`BufferRead`]). The copy is submitted with them: at once, or when a
    /// deferring thread sends its passes ([`Gpu::defer`]). The bytes arrive once the GPU got
    /// there and [`Gpu::poll`] ran (the engine polls every frame). For a few counters a
    /// simulation reports every frame. `spare`, a finished earlier readback of the same size,
    /// gives its staging buffer back, so a readback per frame allocates nothing.
    pub fn read_async(&self, gpu: &Gpu, spare: Option<BufferRead>) -> Result<BufferRead, Error> {
        self.read_async_notify(gpu, spare, None)
    }

    /// [`Buffer::read_async`] that calls `ready` once the bytes arrived (or the readback
    /// failed), from whatever thread polls the GPU: to wake a node that waits for them.
    pub fn read_async_notify(
        &self,
        gpu: &Gpu,
        spare: Option<BufferRead>,
        ready: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<BufferRead, Error> {
        let device = gpu.device();
        let reuse = spare.filter(|s| s.bytes == self.0.bytes && s.state() == ReadState::Ready).map(|s| {
            s.staging.unmap();
            s.staging
        });
        let (staging, error) = gpu.scoped(|| {
            reuse.unwrap_or_else(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("buffer read_async"),
                    size: self.0.bytes,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("buffer readback failed: {e}")));
        }
        let state = Arc::new(AtomicU8::new(WAITING));
        let set = state.clone();
        let to_map = staging.clone();
        // A buffer may be mapped only after the copy into it was submitted. The copy waits
        // with the passes of a deferring thread (a submission costs more than the copy), so
        // the mapping starts once they go out.
        let map: crate::deferred::After = Box::new(move || {
            to_map.map_async(wgpu::MapMode::Read, .., move |r| {
                set.store(if r.is_ok() { READY } else { FAILED }, Ordering::Release);
                if let Some(ready) = &ready {
                    ready();
                }
            });
        });
        let error = gpu.record_commands(
            "buffer read_async",
            |encoder| encoder.copy_buffer_to_buffer(&self.0.buffer, 0, &staging, 0, self.0.bytes),
            Some(map),
        );
        if let Some(e) = error {
            return Err(Error::new(format!("buffer readback failed: {e}")));
        }
        Ok(BufferRead { staging, bytes: self.0.bytes, state, age: ReadAge::new(gpu) })
    }
}

const WAITING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// A readback of a [`Buffer`] started without waiting ([`Buffer::read_async`]).
pub struct BufferRead {
    staging: wgpu::Buffer,
    bytes: u64,
    state: Arc<AtomicU8>,
    /// When it has waited too long (see [`ReadAge`]).
    age: ReadAge,
}

impl BufferRead {
    pub fn state(&self) -> ReadState {
        match self.state.load(Ordering::Acquire) {
            READY => ReadState::Ready,
            FAILED => ReadState::Failed,
            // The poll that expired it may have delivered it after the look above.
            _ if self.age.expired() => match self.state.load(Ordering::Acquire) {
                READY => ReadState::Ready,
                _ => ReadState::Failed,
            },
            _ => ReadState::Waiting,
        }
    }

    /// Lets the readback wait `timeout` instead of [`READ_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.age.set_timeout(timeout);
    }

    /// Waits at most `timeout` for the copy, then says where the readback stands. Blocks the
    /// calling thread: for tests, and for a result wanted in the same frame.
    pub fn wait(&self, gpu: &Gpu, timeout: Duration) -> ReadState {
        // The copy may still wait to be submitted.
        gpu.flush();
        // Running out of time is no error here: the caller looks again later.
        let _ = gpu.poll_until(timeout, || self.state() != ReadState::Waiting);
        self.state()
    }

    /// The bytes as elements of `T`, once [`ReadState::Ready`]. `Err` before, and when the size
    /// is no whole number of `T`.
    pub fn get<T: Pod>(&self) -> Result<Vec<T>, Error> {
        if self.state() != ReadState::Ready {
            return Err(Error::new("the buffer readback is not ready"));
        }
        let size = size_of::<T>() as u64;
        if size == 0 || !self.bytes.is_multiple_of(size) {
            return Err(Error::new(format!(
                "a buffer of {} bytes is no whole number of elements of {size} bytes",
                self.bytes
            )));
        }
        let view =
            self.staging.get_mapped_range(..).map_err(|e| Error::new(format!("buffer readback: {e}")))?;
        let out: Vec<T> = bytemuck::pod_collect_to_vec(&view);
        drop(view);
        Ok(out)
    }

    /// Bytes of the staging buffer.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn check_bytes(gpu: &Gpu, bytes: u64, label: &str) -> Result<(), Error> {
    let max = Buffer::max_bytes(gpu);
    if bytes == 0 || !bytes.is_multiple_of(4) {
        return Err(Error::new(format!(
            "the buffer `{label}` must have a positive multiple of 4 bytes, not {bytes}"
        )));
    }
    if bytes > max {
        return Err(Error::new(format!(
            "the buffer `{label}` of {bytes} bytes is larger than the {max} bytes this GPU binds at most"
        )));
    }
    Ok(())
}

/// Most buffers a [`BufferRing`] keeps.
const RING: usize = 3;

/// The output buffers of one node, reused across cooks, as [`crate::TexRing`] does for
/// textures: a buffer nobody else holds any more is written again instead of allocating a new
/// one every cook. A reused buffer keeps whatever it held: the node overwrites what it uses.
#[derive(Default)]
pub struct BufferRing {
    buffers: Vec<Buffer>,
}

impl BufferRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer of `bytes` bytes that nobody else is reading. `Err` as for [`Buffer::new`].
    pub fn next(&mut self, gpu: &Gpu, bytes: u64, label: &str) -> Result<Buffer, Error> {
        self.buffers.retain(|b| b.bytes() == bytes);
        if let Some(b) = self.buffers.iter().find(|b| !b.shared()) {
            return Ok(b.clone());
        }
        let b = Buffer::new(gpu, bytes, label)?;
        if self.buffers.len() >= RING {
            // All held elsewhere: forget the oldest, its holders keep it alive.
            self.buffers.remove(0);
        }
        self.buffers.push(b.clone());
        Ok(b)
    }

    /// Forgets the buffers nobody else holds, keeping the ones still read.
    pub fn trim(&mut self) {
        self.buffers.retain(Buffer::shared);
    }

    /// Bytes on the GPU of the buffers the ring keeps.
    pub fn bytes(&self) -> u64 {
        self.buffers.iter().map(Buffer::bytes).sum()
    }

    /// The buffers the ring keeps.
    pub fn iter(&self) -> impl Iterator<Item = &Buffer> {
        self.buffers.iter()
    }
}

/// The bytes of distinct buffers among `buffers`.
pub fn unique_buffer_bytes<'a>(buffers: impl IntoIterator<Item = &'a Buffer>) -> u64 {
    let mut seen: Vec<&Buffer> = Vec::new();
    for b in buffers {
        if !seen.iter().any(|s| s.same(b)) {
            seen.push(b);
        }
    }
    seen.iter().map(|b| b.bytes()).sum()
}
