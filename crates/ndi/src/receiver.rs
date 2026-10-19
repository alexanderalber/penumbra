//! Receiving one source: a thread of its own finds the source, connects and keeps the newest
//! picture and the audio since the node last took it; the node uploads the picture when it
//! cooks. The cook thread never waits for the network.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::lock;

use crate::convert::Picture;
use crate::directory;
use crate::ffi::{self, Bandwidth, Capture, ColorFormat, Recv};

/// Most seconds of audio kept for the node; older samples are dropped.
pub const MAX_AUDIO_SECONDS: f64 = 0.5;
/// Least time between two wakes for audio alone (pictures wake at once).
const AUDIO_WAKE: Duration = Duration::from_millis(15);
/// How often the thread looks for a source not found yet.
const LOOK_EVERY: Duration = Duration::from_millis(250);

/// Audio taken from a receiver: one buffer per channel at `rate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audio {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

/// Where a receiver stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The source connected to (or being connected to), by its full name.
    pub source: Option<String>,
    pub connected: bool,
    /// Why there is no picture, when there is a reason.
    pub error: Option<String>,
    /// Frames per second as the sender states them.
    pub frame_rate: f64,
    pub frames: u64,
}

#[derive(Default)]
struct AudioQueue {
    rate: u32,
    channels: Vec<VecDeque<f32>>,
}

struct Shared {
    stop: AtomicBool,
    /// The newest picture not taken yet, replaced by every new one.
    picture: Mutex<Option<Picture>>,
    /// Buffers of taken pictures, to receive into again.
    spare: Mutex<Vec<Vec<u8>>>,
    audio: Mutex<AudioQueue>,
    want_audio: AtomicBool,
    status: Mutex<Status>,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// One receiving node's connection. Dropping it ends the thread (within a tenth of a second,
/// without waiting for it).
pub struct Receiver {
    shared: Arc<Shared>,
}

impl Receiver {
    /// Starts receiving the source `wanted` names ([`directory::find`]); `wake` is called when
    /// a picture (or, while wanted, audio) arrives.
    pub fn new(
        wanted: &str,
        bandwidth: Bandwidth,
        name: &str,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Receiver, String> {
        let ndi = ffi::runtime()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            picture: Mutex::new(None),
            spare: Mutex::new(Vec::new()),
            audio: Mutex::new(AudioQueue::default()),
            want_audio: AtomicBool::new(false),
            status: Mutex::new(Status::default()),
            wake: Box::new(wake),
        });
        let thread = shared.clone();
        let (wanted, name) = (wanted.trim().to_string(), name.to_string());
        std::thread::Builder::new()
            .name("ndi receive".into())
            .spawn(move || run(ndi, &thread, &wanted, bandwidth, &name))
            .map_err(|e| format!("cannot start receiving: {e}"))?;
        Ok(Receiver { shared })
    }

    /// Whether audio is kept for [`Receiver::take_audio`]; off, arriving audio is dropped.
    pub fn want_audio(&self, on: bool) {
        let was = self.shared.want_audio.swap(on, Ordering::AcqRel);
        if was && !on {
            *lock(&self.shared.audio) = AudioQueue::default();
        }
    }

    /// The newest picture, if it arrived after the last one taken.
    pub fn take_picture(&self) -> Option<Picture> {
        lock(&self.shared.picture).take()
    }

    /// Hands a taken picture's buffer back to receive into.
    pub fn recycle(&self, picture: Picture) {
        let mut spare = lock(&self.shared.spare);
        if spare.len() < 2 {
            spare.push(picture.data);
        }
    }

    /// The audio since the last call; empty channels when none came.
    pub fn take_audio(&self) -> Audio {
        let mut q = lock(&self.shared.audio);
        let rate = q.rate;
        let channels = q.channels.iter_mut().map(|c| c.drain(..).collect()).collect();
        Audio { rate, channels }
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

fn set_status(shared: &Shared, f: impl FnOnce(&mut Status)) {
    f(&mut lock(&shared.status));
}

fn run(ndi: &'static ffi::Ndi, shared: &Shared, wanted: &str, bandwidth: Bandwidth, name: &str) {
    let label = if wanted.is_empty() { "a source".to_string() } else { format!("`{wanted}`") };
    let source = loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        match directory::find(wanted) {
            Ok(Some(s)) => break s,
            Ok(None) => {
                let error = (!directory::settling()).then(|| format!("no NDI source {label} on the network"));
                set_status(shared, |s| s.error = error);
            }
            Err(e) => {
                set_status(shared, |s| s.error = Some(e));
                (shared.wake)();
                return;
            }
        }
        std::thread::sleep(LOOK_EVERY);
    };
    set_status(shared, |s| {
        s.source = Some(source.name.clone());
        s.error = None;
    });
    let recv = match Recv::new(ndi, &source, bandwidth, ColorFormat::Fastest, name) {
        Ok(r) => r,
        Err(e) => {
            set_status(shared, |s| s.error = Some(e));
            (shared.wake)();
            return;
        }
    };
    (shared.wake)();
    let mut last_audio_wake = Instant::now();
    let mut last_check = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        match recv.capture(100) {
            Capture::Video(frame) => {
                let Some(bytes) = frame.data() else {
                    let what = ffi::fourcc::name(frame.fourcc());
                    set_status(shared, |s| {
                        s.error = Some(format!(
                            "{}x{} {what} pictures are not taken",
                            frame.width(),
                            frame.height()
                        ));
                    });
                    (shared.wake)();
                    continue;
                };
                let mut data = lock(&shared.spare).pop().unwrap_or_default();
                data.clear();
                data.extend_from_slice(bytes);
                let picture = Picture {
                    width: frame.width(),
                    height: frame.height(),
                    fourcc: frame.fourcc(),
                    stride: frame.stride(),
                    data,
                    field: frame.is_field(),
                };
                let rate = frame.frame_rate();
                drop(frame);
                let old = lock(&shared.picture).replace(picture);
                if let Some(old) = old {
                    let mut spare = lock(&shared.spare);
                    if spare.len() < 2 {
                        spare.push(old.data);
                    }
                }
                set_status(shared, |s| {
                    s.connected = true;
                    s.error = None;
                    s.frame_rate = rate;
                    s.frames += 1;
                });
                (shared.wake)();
            }
            Capture::Audio(frame) => {
                if !shared.want_audio.load(Ordering::Acquire) {
                    continue;
                }
                let rate = frame.sample_rate();
                let n = frame.channels();
                {
                    let mut q = lock(&shared.audio);
                    if q.rate != rate || q.channels.len() != n {
                        *q = AudioQueue { rate, channels: vec![VecDeque::new(); n] };
                    }
                    let cap = (f64::from(rate.max(1)) * MAX_AUDIO_SECONDS) as usize;
                    for (c, queue) in q.channels.iter_mut().enumerate() {
                        if let Some(samples) = frame.channel(c) {
                            queue.extend(samples.iter().map(|v| if v.is_finite() { *v } else { 0.0 }));
                        }
                        let over = queue.len().saturating_sub(cap);
                        queue.drain(..over);
                    }
                }
                if last_audio_wake.elapsed() >= AUDIO_WAKE {
                    last_audio_wake = Instant::now();
                    (shared.wake)();
                }
            }
            Capture::Lost => set_status(shared, |s| s.connected = false),
            Capture::Nothing | Capture::Other => {}
        }
        if last_check.elapsed() >= Duration::from_millis(500) {
            last_check = Instant::now();
            let connected = recv.connected();
            set_status(shared, |s| s.connected = connected);
        }
    }
}
