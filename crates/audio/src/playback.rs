//! Audio output: a [`Player`] per output device, fed one block per show frame.
//!
//! - **Devices** are picked by name as on the input side: the first output whose name contains
//!   the pattern (case ignored; an exact name wins), empty is the system's default output.
//!   [`output_devices`] lists what this machine has.
//! - **The device thread** owns the cpal stream: it opens the device in its default format
//!   (shared mode on Windows), and reopens it two seconds after an error, a lost device or a
//!   second without callbacks. The callback converts to whatever sample format the device
//!   wants and runs inside `catch_unwind`; it never allocates.
//! - **Pushing** ([`Player::push`]) copies a block into the [`Engine`]'s ring under a short
//!   lock and returns at once; the device callback takes the same lock to pull. The engine
//!   resamples to the device rate, keeps the latency at its target against clock drift, and
//!   fades at gaps and drops. [`Engine`] works without a device, for tests and offline use.

mod engine;
#[cfg(test)]
mod tests;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, I24, SampleFormat, SizedSample, U24};
use penumbra_core::lock;
use serde::Serialize;

pub use engine::{
    Engine, EngineStatus, FADE_SECONDS, MAX_CHANNELS, MAX_CORRECTION, MAX_LATENCY_MS, MAX_RATE,
    MIN_LATENCY_MS, MIN_RATE, PHASES, Settings, State, TAPS,
};

use crate::capture::DeviceInfo;

/// How long an output may call back nothing before it is reopened.
pub const STALL: Duration = Duration::from_secs(1);
/// How long an output waits before it tries a device again.
pub const RETRY: Duration = Duration::from_secs(2);
/// How often the device thread looks at its stream.
const TICK: Duration = Duration::from_millis(100);
/// Device frames converted per step inside one callback (the scratch buffer's size).
const CHUNK_FRAMES: usize = 1024;

/// The output devices of this machine (asks the system: some milliseconds, not for every frame).
pub fn output_devices() -> Result<Vec<DeviceInfo>, String> {
    catch_unwind(|| {
        let host = cpal::default_host();
        let default = host.default_output_device().and_then(|d| name_of(&d));
        let list = host.output_devices().map_err(|e| format!("cannot list audio outputs: {e}"))?;
        Ok(list
            .map(|d| {
                let config = d.default_output_config().ok();
                let name = name_of(&d).unwrap_or_default();
                DeviceInfo {
                    default: default.as_deref() == Some(name.as_str()),
                    name,
                    rate: config.as_ref().map(|c| c.sample_rate()),
                    channels: config.as_ref().map(|c| c.channels()),
                    format: config.as_ref().map(|c| c.sample_format().to_string()),
                }
            })
            .collect())
    })
    .unwrap_or_else(|_| Err("listing audio outputs failed".into()))
}

fn name_of(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|x| x.name().to_string())
}

/// A player as it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlayerStatus {
    /// The device pattern asked for (empty: the default output).
    pub key: String,
    /// The device opened.
    pub device: Option<String>,
    pub open: bool,
    pub error: Option<String>,
    /// The device's format.
    pub rate: u32,
    pub channels: u16,
    pub format: Option<String>,
    /// The device's own buffer: from a callback to its samples reaching the converter, ms.
    pub device_ms: f64,
    /// The buffer asked of the device (frames; 0: the system's default).
    pub buffer_frames: u32,
    /// Frames the device asked for in its last callback.
    pub callback_frames: usize,
    /// Total delay from a push to the speaker: the queued fill plus the device's buffer, ms.
    pub latency_ms: f64,
    /// Callbacks from the system.
    pub callbacks: u64,
    /// Times the device was opened.
    pub opens: u64,
    /// Underruns and other glitches the system reported.
    pub glitches: u64,
    /// The engine: source format, fill, ratio, underruns and overruns.
    pub engine: EngineStatus,
}

#[derive(Default)]
struct DeviceState {
    device: Option<String>,
    open: bool,
    error: Option<String>,
    rate: u32,
    channels: u16,
    format: Option<String>,
    device_ms: f64,
    buffer_frames: u32,
    callback_frames: usize,
    callbacks: u64,
    opens: u64,
    glitches: u64,
}

struct Shared {
    key: String,
    engine: Mutex<Engine>,
    device: Mutex<DeviceState>,
    stop: AtomicBool,
    /// Set by the error callback: the stream is broken and must be reopened.
    failed: Mutex<Option<String>>,
    /// Milliseconds since `born` at the last callback.
    last_callback: AtomicU64,
    born: Instant,
}

impl Shared {
    fn mark_callback(&self) {
        // `as`: milliseconds since the start fit easily.
        self.last_callback.store(self.born.elapsed().as_millis() as u64, Ordering::Release);
    }

    fn since_callback(&self) -> Duration {
        let last = Duration::from_millis(self.last_callback.load(Ordering::Acquire));
        self.born.elapsed().saturating_sub(last)
    }
}

/// Plays pushed blocks on one output device. Dropping it closes the device within a tenth of a
/// second without waiting; [`Player::close`] waits.
pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Starts playing on the output whose name contains `device` (empty: the default output).
    /// Never fails: a missing device shows in [`Player::status`] and is tried again every
    /// [`RETRY`].
    pub fn open(device: &str, settings: Settings) -> Player {
        let key = device.trim().to_string();
        let shared = Arc::new(Shared {
            key: key.clone(),
            engine: Mutex::new(Engine::new(settings)),
            device: Mutex::new(DeviceState::default()),
            stop: AtomicBool::new(false),
            failed: Mutex::new(None),
            last_callback: AtomicU64::new(0),
            born: Instant::now(),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("audio-out {}", if key.is_empty() { "default" } else { key.as_str() }))
            .spawn(move || run(&s));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                lock(&shared.device).error = Some(format!("cannot start a thread: {e}"));
                None
            }
        };
        Player { shared, thread }
    }

    /// The device pattern this player was opened with.
    pub fn key(&self) -> &str {
        &self.shared.key
    }

    /// Queues one block: one slice per channel (`f32` or `f64`), all of one length, at `rate`.
    /// Returns at once. Errors for a rate or channel count that cannot be played; non-numbers
    /// play as silence.
    pub fn push<T: Copy + Into<f64>>(&self, rate: u32, channels: &[&[T]]) -> Result<usize, String> {
        lock(&self.shared.engine).push(rate, channels)
    }

    /// Queues interleaved samples, `channels` per frame.
    pub fn push_interleaved(&self, rate: u32, channels: usize, samples: &[f32]) -> Result<usize, String> {
        lock(&self.shared.engine).push_interleaved(rate, channels, samples)
    }

    pub fn settings(&self) -> Settings {
        lock(&self.shared.engine).settings()
    }

    /// A new latency takes effect at once; a new device buffer reopens the device.
    pub fn set_settings(&self, settings: Settings) {
        let old = {
            let mut engine = lock(&self.shared.engine);
            let old = engine.settings();
            engine.set_settings(settings);
            old
        };
        if old.device_buffer_ms.to_bits() != settings.device_buffer_ms.to_bits() {
            *lock(&self.shared.failed) = Some("the device buffer changed".to_string());
        }
    }

    /// Fades out and stays silent until the next push; not counted as an underrun.
    pub fn pause(&self) {
        lock(&self.shared.engine).pause();
    }

    pub fn status(&self) -> PlayerStatus {
        let engine = lock(&self.shared.engine).status();
        let d = lock(&self.shared.device);
        PlayerStatus {
            key: self.shared.key.clone(),
            device: d.device.clone(),
            open: d.open,
            error: d.error.clone(),
            rate: d.rate,
            channels: d.channels,
            format: d.format.clone(),
            device_ms: d.device_ms,
            buffer_frames: d.buffer_frames,
            callback_frames: d.callback_frames,
            latency_ms: engine.fill_ms + if d.open { d.device_ms } else { 0.0 },
            callbacks: d.callbacks,
            opens: d.opens,
            glitches: d.glitches,
            engine,
        }
    }

    /// Delay from a push to the speaker (the queued fill plus the device's buffer).
    pub fn latency(&self) -> Duration {
        Duration::from_secs_f64((self.status().latency_ms / 1000.0).clamp(0.0, 60.0))
    }

    /// Closes the device and waits for its thread.
    pub fn close(mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            log::error!("an audio output thread panicked");
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The thread ends within a tick; it is not joined here, so the cook thread never waits.
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// The device thread: opens the device, watches it, reopens it after trouble.
fn run(s: &Arc<Shared>) {
    let stopped = || s.stop.load(Ordering::Acquire);
    while !stopped() {
        *lock(&s.failed) = None;
        let opened = catch_unwind(AssertUnwindSafe(|| open(s)))
            .unwrap_or_else(|_| Err("opening the device panicked".to_string()));
        match opened {
            Ok(stream) => {
                {
                    let mut d = lock(&s.device);
                    d.open = true;
                    d.error = None;
                    d.opens += 1;
                }
                s.mark_callback();
                let trouble = loop {
                    std::thread::sleep(TICK);
                    if stopped() {
                        break None;
                    }
                    if let Some(e) = lock(&s.failed).take() {
                        break Some(e);
                    }
                    if s.since_callback() > STALL {
                        break Some("no callbacks for a second".to_string());
                    }
                };
                // Dropping the stream stops it; on its own thread, where it was made.
                drop(stream);
                lock(&s.engine).detach();
                let mut d = lock(&s.device);
                d.open = false;
                if let Some(e) = trouble {
                    log::warn!("audio output `{}`: {e}; reopening", s.key);
                    d.error = Some(e);
                }
            }
            Err(e) => {
                lock(&s.engine).detach();
                let mut d = lock(&s.device);
                if d.error.as_deref() != Some(e.as_str()) {
                    log::warn!("audio output `{}`: {e}", s.key);
                }
                d.open = false;
                d.error = Some(e);
            }
        }
        let until = Instant::now() + RETRY;
        while !stopped() && Instant::now() < until {
            std::thread::sleep(TICK);
        }
    }
}

/// The output device a pattern names: the default for an empty one, else an exact name, else
/// the first name containing it (case ignored).
pub(crate) fn find(host: &cpal::Host, pattern: &str) -> Result<cpal::Device, String> {
    if pattern.is_empty() {
        return host
            .default_output_device()
            .ok_or_else(|| "this machine has no default audio output".to_string());
    }
    let list: Vec<cpal::Device> =
        host.output_devices().map_err(|e| format!("cannot list audio outputs: {e}"))?.collect();
    let lower = pattern.to_lowercase();
    let names: Vec<String> = list.iter().map(|d| name_of(d).unwrap_or_default()).collect();
    let exact = names.iter().position(|n| n.eq_ignore_ascii_case(pattern));
    let partial = || names.iter().position(|n| n.to_lowercase().contains(&lower));
    match exact.or_else(partial) {
        Some(i) => list.into_iter().nth(i).ok_or_else(|| "the device list changed".to_string()),
        None => Err(format!("no audio output matches `{pattern}`; outputs: {}", names.join(", "))),
    }
}

fn open(s: &Arc<Shared>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = find(&host, &s.key)?;
    let name = name_of(&device);
    let config = device.default_output_config().map_err(|e| format!("cannot read the output format: {e}"))?;
    let channels = usize::from(config.channels());
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(format!("the device has {channels} channels (at most {MAX_CHANNELS} are played)"));
    }
    let rate = config.sample_rate();
    let format = config.sample_format();
    if !writable(format) {
        return Err(format!("the device wants samples as {format}, which cannot be written"));
    }
    let mut stream_config = config.config();
    let buffer_ms = lock(&s.engine).settings().device_buffer_ms;
    let buffer_frames = if buffer_ms.is_finite() && buffer_ms > 0.0 {
        // `as`: a few milliseconds of frames, well inside `u32`.
        (f64::from(rate) * buffer_ms.min(MAX_LATENCY_MS) / 1000.0).round().max(16.0) as u32
    } else {
        0
    };
    if buffer_frames > 0 {
        stream_config.buffer_size = cpal::BufferSize::Fixed(buffer_frames);
    }
    lock(&s.engine).set_device(rate, channels);
    {
        let mut d = lock(&s.device);
        d.device = name;
        d.rate = rate;
        d.channels = config.channels();
        d.format = Some(format.to_string());
        d.device_ms = 0.0;
        d.buffer_frames = buffer_frames;
    }
    let data_shared = s.clone();
    let mut scratch = vec![0.0f32; CHUNK_FRAMES * channels];
    let on_data = move |data: &mut cpal::Data, info: &cpal::OutputCallbackInfo| {
        // A panic must not unwind into the system's audio thread.
        let done = catch_unwind(AssertUnwindSafe(|| {
            let ts = info.timestamp();
            let buffered = ts.playback.duration_since(ts.callback).as_secs_f64() * 1000.0;
            let ok = {
                let mut engine = lock(&data_shared.engine);
                fill(data, &mut engine, &mut scratch, channels)
            };
            {
                let mut d = lock(&data_shared.device);
                d.callbacks += 1;
                d.callback_frames = data.len() / channels.max(1);
                // Smoothed: the prediction jitters from callback to callback.
                d.device_ms =
                    if d.device_ms == 0.0 { buffered } else { d.device_ms + (buffered - d.device_ms) * 0.05 };
            }
            data_shared.mark_callback();
            if !ok {
                *lock(&data_shared.failed) =
                    Some(format!("the device wants samples as {}", data.sample_format()));
            }
        }));
        if done.is_err() {
            silence(data);
        }
    };
    let err_shared = s.clone();
    let on_error = move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => {
            lock(&err_shared.device).glitches += 1;
        }
        _ => *lock(&err_shared.failed) = Some(e.to_string()),
    };
    let stream = device
        .build_output_stream_raw(stream_config, format, on_data, on_error, Some(Duration::from_secs(5)))
        .map_err(|e| format!("cannot open the output: {e}"))?;
    stream.play().map_err(|e| format!("cannot start the output: {e}"))?;
    Ok(stream)
}

fn writable(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U24
            | SampleFormat::U32
            | SampleFormat::U64
    )
}

/// Pulls from the engine into the device's buffer in its format; false for a format it does
/// not know (the buffer is then silent).
fn fill(data: &mut cpal::Data, engine: &mut Engine, scratch: &mut [f32], channels: usize) -> bool {
    fn put<T: SizedSample + FromSample<f32>>(
        data: &mut cpal::Data,
        engine: &mut Engine,
        scratch: &mut [f32],
        channels: usize,
    ) -> bool {
        let Some(out) = data.as_slice_mut::<T>() else {
            return false;
        };
        // Whole frames per step, so the engine never sees half a frame.
        let step = (scratch.len() / channels.max(1)).max(1) * channels.max(1);
        for chunk in out.chunks_mut(step) {
            let Some(s) = scratch.get_mut(..chunk.len()) else {
                return false;
            };
            engine.pull(s);
            for (o, v) in chunk.iter_mut().zip(s.iter()) {
                *o = T::from_sample(*v);
            }
        }
        true
    }
    let ok = match data.sample_format() {
        SampleFormat::F32 => put::<f32>(data, engine, scratch, channels),
        SampleFormat::F64 => put::<f64>(data, engine, scratch, channels),
        SampleFormat::I8 => put::<i8>(data, engine, scratch, channels),
        SampleFormat::I16 => put::<i16>(data, engine, scratch, channels),
        SampleFormat::I24 => put::<I24>(data, engine, scratch, channels),
        SampleFormat::I32 => put::<i32>(data, engine, scratch, channels),
        SampleFormat::I64 => put::<i64>(data, engine, scratch, channels),
        SampleFormat::U8 => put::<u8>(data, engine, scratch, channels),
        SampleFormat::U16 => put::<u16>(data, engine, scratch, channels),
        SampleFormat::U24 => put::<U24>(data, engine, scratch, channels),
        SampleFormat::U32 => put::<u32>(data, engine, scratch, channels),
        SampleFormat::U64 => put::<u64>(data, engine, scratch, channels),
        _ => false,
    };
    if !ok {
        silence(data);
    }
    ok
}

/// Writes silence in the buffer's format (for unsigned formats that is the middle value).
fn silence(data: &mut cpal::Data) {
    fn zero<T: SizedSample>(data: &mut cpal::Data) {
        if let Some(out) = data.as_slice_mut::<T>() {
            out.fill(T::EQUILIBRIUM);
        }
    }
    match data.sample_format() {
        SampleFormat::F32 => zero::<f32>(data),
        SampleFormat::F64 => zero::<f64>(data),
        SampleFormat::I8 => zero::<i8>(data),
        SampleFormat::I16 => zero::<i16>(data),
        SampleFormat::I24 => zero::<I24>(data),
        SampleFormat::I32 => zero::<i32>(data),
        SampleFormat::I64 => zero::<i64>(data),
        SampleFormat::U8 => zero::<u8>(data),
        SampleFormat::U16 => zero::<u16>(data),
        SampleFormat::U24 => zero::<U24>(data),
        SampleFormat::U32 => zero::<u32>(data),
        SampleFormat::U64 => zero::<u64>(data),
        _ => data.bytes_mut().fill(0),
    }
}
