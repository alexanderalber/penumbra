//! Sending: the node converts its input to UYVY(A) on the GPU and starts reading it back
//! ([`Packer`]); a thread of its own waits for the bytes and hands them to the runtime, which
//! compresses and sends them. Sound goes through the same thread, in the order the node hands
//! it on, so a block and the picture of the same frame leave together. A frame that finds the
//! ring full is left out, and so is a block of sound that finds the queue full; the cook thread
//! never waits.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use penumbra_core::lock;
use penumbra_gpu::{Alpha, Gpu, Tex};

use crate::convert::{Packed, Packer};
use crate::ffi::{self, OutAudio, OutFrame};

/// Frames on their way back from the GPU or to the runtime, at most.
const RING: usize = 3;
/// Blocks of sound waiting for the send thread, at most: about half a second at 60 fps.
const AUDIO_QUEUE: usize = 32;
/// How long the send thread waits for one frame from the GPU.
const READBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// What one [`Sender::send`] or [`Sender::send_audio`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Frame,
    /// Every buffer was on its way: this frame (or block) was left out.
    Busy,
}

#[derive(Default)]
struct Shared {
    /// The full name receivers see, once the source is announced.
    name: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    connections: AtomicUsize,
    /// Blocks of sound left out because the queue was full.
    dropped_audio: AtomicU64,
}

struct Frame {
    packed: Packed,
    rate: (u32, u32),
    timecode: Option<i64>,
}

/// One block of sound for the send thread.
struct Block {
    rate: u32,
    channels: Vec<Vec<f32>>,
    timecode: Option<i64>,
}

enum Item {
    Picture(Frame),
    Sound(Block),
}

/// One sending node's source. Dropping it takes the source off the network once the frames
/// still on their way are sent.
pub struct Sender {
    packer: Packer,
    items: SyncSender<Item>,
    shared: Arc<Shared>,
}

impl Sender {
    /// Announces the source `name` (the runtime puts the machine's name before it). Fails at
    /// once without the runtime; the announcement itself happens on the send thread.
    pub fn new(name: &str) -> Result<Sender, String> {
        let ndi = ffi::runtime()?;
        let (items, incoming) = sync_channel::<Item>(RING + AUDIO_QUEUE);
        let shared = Arc::new(Shared::default());
        let thread = shared.clone();
        let name = name.to_string();
        std::thread::Builder::new()
            .name("ndi send".into())
            .spawn(move || {
                let sender = match ffi::Sender::new(ndi, &name) {
                    Ok(s) => s,
                    Err(e) => {
                        *lock(&thread.error) = Some(e);
                        return;
                    }
                };
                *lock(&thread.name) = Some(sender.name().to_string());
                // Ends when the node drops its side.
                while let Ok(item) = incoming.recv() {
                    let result = match item {
                        Item::Picture(frame) => frame.packed.wait(READBACK_TIMEOUT).and_then(|()| {
                            let p = &frame.packed;
                            p.with_bytes(|bytes| {
                                sender.send(&OutFrame {
                                    width: p.width,
                                    height: p.height,
                                    fourcc: p.fourcc,
                                    stride: p.stride,
                                    data: bytes,
                                    rate: frame.rate,
                                    timecode: frame.timecode,
                                })
                            })?
                        }),
                        Item::Sound(block) => sender.send_audio(&OutAudio {
                            rate: block.rate,
                            channels: &block.channels,
                            timecode: block.timecode,
                        }),
                    };
                    *lock(&thread.error) = result.err();
                    thread.connections.store(sender.connections(), Ordering::Release);
                }
            })
            .map_err(|e| format!("cannot start sending: {e}"))?;
        Ok(Sender { packer: Packer::new(RING), items, shared })
    }

    /// The full name receivers see (`MACHINE (name)`), once announced.
    pub fn name(&self) -> Option<String> {
        lock(&self.shared.name).clone()
    }

    /// Why the last frame was not sent, or the source not announced.
    pub fn error(&self) -> Option<String> {
        lock(&self.shared.error).clone()
    }

    /// Receivers connected, as of the last frame sent.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::Acquire)
    }

    /// Blocks of sound left out so far because the send thread fell behind.
    pub fn dropped_audio(&self) -> u64 {
        self.shared.dropped_audio.load(Ordering::Relaxed)
    }

    /// Converts `tex` and queues it for sending, announced at `rate` frames per second
    /// (numerator, denominator), at `timecode` (100 ns units; `None`: the runtime's clock).
    /// Never waits.
    pub fn send(
        &mut self,
        gpu: &Gpu,
        tex: &Tex,
        alpha: Alpha,
        rate: (u32, u32),
        timecode: Option<i64>,
    ) -> Result<Sent, String> {
        let Some(packed) = self.packer.grab(gpu, tex, alpha)? else { return Ok(Sent::Busy) };
        self.queue(Item::Picture(Frame { packed, rate, timecode }))
    }

    /// Queues a block of sound (one buffer per channel, all as long) for sending, its first
    /// sample at `timecode` as in [`Sender::send`]. Never waits; a block that finds the queue
    /// full is left out and counted ([`Sender::dropped_audio`]).
    pub fn send_audio(
        &self,
        rate: u32,
        channels: Vec<Vec<f32>>,
        timecode: Option<i64>,
    ) -> Result<Sent, String> {
        let sent = self.queue(Item::Sound(Block { rate, channels, timecode }))?;
        if sent == Sent::Busy {
            self.shared.dropped_audio.fetch_add(1, Ordering::Relaxed);
        }
        Ok(sent)
    }

    fn queue(&self, item: Item) -> Result<Sent, String> {
        match self.items.try_send(item) {
            Ok(()) => Ok(Sent::Frame),
            Err(TrySendError::Full(_)) => Ok(Sent::Busy),
            Err(TrySendError::Disconnected(_)) => {
                Err(self.error().unwrap_or_else(|| "the NDI send thread ended".into()))
            }
        }
    }
}
