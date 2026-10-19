//! Sending: the node converts its input to UYVY(A) on the GPU and starts reading it back
//! ([`Packer`]); a thread of its own waits for the bytes and hands them to the runtime, which
//! compresses and sends them. A frame that finds the ring full is left out; the cook thread
//! never waits.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use penumbra_core::lock;
use penumbra_gpu::{Alpha, Gpu, Tex};

use crate::convert::{Packed, Packer};
use crate::ffi::{self, OutFrame};

/// Frames on their way back from the GPU or to the runtime, at most.
const RING: usize = 3;
/// How long the send thread waits for one frame from the GPU.
const READBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// What one [`Sender::send`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Frame,
    /// Every buffer was on its way: this frame was left out.
    Busy,
}

#[derive(Default)]
struct Shared {
    /// The full name receivers see, once the source is announced.
    name: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
    connections: AtomicUsize,
}

struct Frame {
    packed: Packed,
    rate: (u32, u32),
}

/// One sending node's source. Dropping it takes the source off the network once the frames
/// still on their way are sent.
pub struct Sender {
    packer: Packer,
    frames: SyncSender<Frame>,
    shared: Arc<Shared>,
}

impl Sender {
    /// Announces the source `name` (the runtime puts the machine's name before it). Fails at
    /// once without the runtime; the announcement itself happens on the send thread.
    pub fn new(name: &str) -> Result<Sender, String> {
        let ndi = ffi::runtime()?;
        let (frames, incoming) = sync_channel::<Frame>(RING);
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
                while let Ok(frame) = incoming.recv() {
                    let result = frame.packed.wait(READBACK_TIMEOUT).and_then(|()| {
                        let p = &frame.packed;
                        p.with_bytes(|bytes| {
                            sender.send(&OutFrame {
                                width: p.width,
                                height: p.height,
                                fourcc: p.fourcc,
                                stride: p.stride,
                                data: bytes,
                                rate: frame.rate,
                            })
                        })?
                    });
                    *lock(&thread.error) = result.err();
                    thread.connections.store(sender.connections(), Ordering::Release);
                }
            })
            .map_err(|e| format!("cannot start sending: {e}"))?;
        Ok(Sender { packer: Packer::new(RING), frames, shared })
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

    /// Converts `tex` and queues it for sending, announced at `rate` frames per second
    /// (numerator, denominator). Never waits.
    pub fn send(&mut self, gpu: &Gpu, tex: &Tex, alpha: Alpha, rate: (u32, u32)) -> Result<Sent, String> {
        let Some(packed) = self.packer.grab(gpu, tex, alpha)? else { return Ok(Sent::Busy) };
        match self.frames.try_send(Frame { packed, rate }) {
            Ok(()) => Ok(Sent::Frame),
            Err(TrySendError::Full(_)) => Ok(Sent::Busy),
            Err(TrySendError::Disconnected(_)) => {
                Err(self.error().unwrap_or_else(|| "the NDI send thread ended".into()))
            }
        }
    }
}
