//! Audio input: one stream per device the network wants, its samples in a ring, and the
//! hand-over to whoever reads them.
//!
//! - **Streams** are wanted by the network ([`Hub::set_wanted`]: one device pattern per
//!   `chan.audio_in` node; empty is the system's default input). Each wanted device gets a thread
//!   that owns its cpal stream: it opens the device in its default format (shared mode on
//!   Windows, so other programs keep using it), and reopens it two seconds after an error, a
//!   lost device or a second without samples (a pulled USB cable does not always report an
//!   error).
//! - **Samples** go into a ring per stream ([`RING_SECONDS`], interleaved, as `f32`). Readers
//!   keep a [`Cursor`] and get what arrived since ([`Hub::read`]); a reader that falls more than
//!   the ring behind loses the oldest part and is told how much. [`Hub::watch`] calls back
//!   when samples arrive, at most once until the watcher acknowledges.
//! - **Devices** are picked by name: the first input whose name contains the pattern (case
//!   ignored; an exact name wins). [`devices`] lists what this machine has.
//! - [`Hub::feed`] puts samples into a stream without a device, for tests and tools; a stream
//!   fed that way is not opened as a device.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use penumbra_core::lock;
use serde::{Deserialize, Serialize};

/// Seconds of samples kept per stream.
pub const RING_SECONDS: f64 = 2.0;
/// How long a stream may deliver nothing before it is reopened.
pub const STALL: Duration = Duration::from_secs(1);
/// How long a stream waits before it tries a device again.
pub const RETRY: Duration = Duration::from_secs(2);
/// How often a stream thread looks at its stream.
const TICK: Duration = Duration::from_millis(100);
/// Most channels taken from one device.
pub const MAX_CHANNELS: usize = 64;
/// How fast the level meters fall, per second (as a factor).
const METER_FALL: f32 = 0.05;

/// Settings for all streams. Not stored: every start has the defaults (`audio.config`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Off closes every stream (the wants are kept for when it is on again).
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// One input device of this machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    /// The system's default input.
    pub default: bool,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
    pub format: Option<String>,
}

/// The input devices of this machine (asks the system: some milliseconds, not for every frame).
pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    catch_unwind(|| {
        let host = cpal::default_host();
        let default = host.default_input_device().and_then(|d| name_of(&d));
        let list = host.input_devices().map_err(|e| format!("cannot list audio inputs: {e}"))?;
        Ok(list
            .map(|d| {
                let config = d.default_input_config().ok();
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
    .unwrap_or_else(|_| Err("listing audio inputs failed".into()))
}

fn name_of(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|x| x.name().to_string())
}

/// A stream as it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StreamStatus {
    /// The device pattern the network asked for (empty: the default input).
    pub key: String,
    /// The device opened.
    pub device: Option<String>,
    pub rate: u32,
    pub channels: u16,
    pub format: Option<String>,
    pub open: bool,
    pub error: Option<String>,
    /// Sample frames received since the start.
    pub frames: u64,
    /// Callbacks from the system.
    pub callbacks: u64,
    /// Times the device was opened.
    pub opens: u64,
    /// Overruns and other glitches the system reported.
    pub glitches: u64,
    /// Recent peak per channel, dBFS (falling slowly), -120 for silence.
    pub peak_db: Vec<f64>,
    /// Fed by [`Hub::feed`], not by a device.
    pub fed: bool,
}

/// Where a reader stands in a stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    generation: u64,
    frame: Option<u64>,
}

/// Samples that arrived since a reader's last read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub rate: u32,
    /// One vector per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
    /// Frames lost because the reader fell more than the ring behind.
    pub lost: u64,
}

impl Block {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}

/// The samples of one stream.
#[derive(Default)]
struct Ring {
    /// Interleaved.
    data: VecDeque<f32>,
    channels: usize,
    rate: u32,
    /// Frames written since the stream (re)opened.
    total: u64,
    /// Changes whenever the format changes; readers start over.
    generation: u64,
    peak: Vec<f32>,
    last_meter: Option<Instant>,
}

impl Ring {
    fn reset(&mut self, rate: u32, channels: usize) {
        self.data.clear();
        self.rate = rate;
        self.channels = channels;
        self.total = 0;
        self.generation += 1;
        self.peak = vec![0.0; channels];
    }

    fn cap(&self) -> usize {
        // `as`: seconds of frames to a sample count.
        ((f64::from(self.rate) * RING_SECONDS) as usize).max(1) * self.channels.max(1)
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>) {
        let channels = self.channels.max(1);
        let now = Instant::now();
        let fall =
            self.last_meter.map_or(1.0, |t| METER_FALL.powf(now.saturating_duration_since(t).as_secs_f32()));
        self.last_meter = Some(now);
        for p in &mut self.peak {
            *p *= fall;
        }
        let mut n = 0usize;
        for (i, s) in samples.enumerate() {
            let s = if s.is_finite() { s } else { 0.0 };
            if let Some(p) = self.peak.get_mut(i % channels) {
                *p = p.max(s.abs());
            }
            self.data.push_back(s);
            n += 1;
        }
        let cap = self.cap();
        while self.data.len() > cap {
            self.data.pop_front();
        }
        self.total += (n / channels) as u64;
    }
}

struct Stream {
    key: String,
    ring: Mutex<Ring>,
    status: Mutex<StreamStatus>,
    stop: AtomicBool,
    /// Set by the error callback: the stream is broken and must be reopened.
    failed: Mutex<Option<String>>,
    /// Milliseconds since `born` at the last callback.
    last_data: AtomicU64,
    born: Instant,
}

impl Stream {
    fn new(key: &str, fed: bool) -> Self {
        Self {
            key: key.to_string(),
            ring: Mutex::new(Ring::default()),
            status: Mutex::new(StreamStatus { key: key.to_string(), fed, ..StreamStatus::default() }),
            stop: AtomicBool::new(false),
            failed: Mutex::new(None),
            last_data: AtomicU64::new(0),
            born: Instant::now(),
        }
    }

    fn mark_data(&self) {
        // `as`: milliseconds since the start fit easily.
        self.last_data.store(self.born.elapsed().as_millis() as u64, Ordering::Release);
    }

    fn since_data(&self) -> Duration {
        let last = Duration::from_millis(self.last_data.load(Ordering::Acquire));
        self.born.elapsed().saturating_sub(last)
    }
}

type Wake = Arc<dyn Fn() + Send + Sync>;
/// A stream and its thread (none for a fed stream).
type Entry = (Arc<Stream>, Option<JoinHandle<()>>);
/// A watcher: stream key, id, whether a callback is pending, the callback.
type Watcher = (String, u64, Arc<AtomicBool>, Wake);

struct Inner {
    config: Mutex<Config>,
    wanted: Mutex<BTreeSet<String>>,
    streams: Mutex<BTreeMap<String, Entry>>,
    watches: Mutex<Vec<Watcher>>,
    next_id: AtomicU64,
}

/// The audio hub, shared by the engine, the nodes and the commands (clones share it).
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps a [`Hub::watch`] callback registered; dropping it unregisters.
pub struct Watch {
    key: String,
    id: u64,
    pending: Arc<AtomicBool>,
    hub: Weak<Inner>,
}

impl Watch {
    /// Allows the next callback (call before reading).
    pub fn ack(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.watches).retain(|(_, id, _, _)| *id != self.id);
        }
    }
}

fn wake(inner: &Inner, key: &str) {
    let watches: Vec<Wake> = lock(&inner.watches)
        .iter()
        .filter(|(k, _, pending, _)| k == key && !pending.swap(true, Ordering::AcqRel))
        .map(|(_, _, _, w)| w.clone())
        .collect();
    for w in watches {
        w();
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                config: Mutex::new(Config::default()),
                wanted: Mutex::new(BTreeSet::new()),
                streams: Mutex::new(BTreeMap::new()),
                watches: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// Changes the settings; turning it off closes every stream, on opens what is wanted.
    pub fn set_config(&self, config: Config) {
        *lock(&self.inner.config) = config;
        self.apply();
    }

    /// The devices the network wants, by pattern (empty: the default input).
    pub fn set_wanted(&self, keys: &[String]) {
        let keys: BTreeSet<String> = keys.iter().map(|k| k.trim().to_string()).collect();
        {
            let mut wanted = lock(&self.inner.wanted);
            if *wanted == keys {
                return;
            }
            *wanted = keys;
        }
        self.apply();
    }

    pub fn wanted(&self) -> Vec<String> {
        lock(&self.inner.wanted).iter().cloned().collect()
    }

    fn apply(&self) {
        let wanted = if self.config().enabled { lock(&self.inner.wanted).clone() } else { BTreeSet::new() };
        let mut streams = lock(&self.inner.streams);
        let gone: Vec<String> = streams
            .iter()
            .filter(|(k, (s, _))| !wanted.contains(*k) && !lock(&s.status).fed)
            .map(|(k, _)| k.clone())
            .collect();
        let mut touched = Vec::new();
        for key in gone {
            if let Some((s, _thread)) = streams.remove(&key) {
                // The thread ends within a tick; it is not joined here, so the cook thread
                // never waits for it.
                s.stop.store(true, Ordering::Release);
            }
            touched.push(key);
        }
        for key in wanted {
            if streams.contains_key(&key) {
                continue;
            }
            let s = Arc::new(Stream::new(&key, false));
            let (inner, stream) = (Arc::downgrade(&self.inner), s.clone());
            let thread = std::thread::Builder::new()
                .name(format!("audio-in {}", if key.is_empty() { "default" } else { key.as_str() }))
                .spawn(move || run(&inner, &stream));
            let thread = match thread {
                Ok(t) => Some(t),
                Err(e) => {
                    lock(&s.status).error = Some(format!("cannot start a thread: {e}"));
                    None
                }
            };
            touched.push(key.clone());
            streams.insert(key, (s, thread));
        }
        drop(streams);
        for key in touched {
            wake(&self.inner, &key);
        }
    }

    fn stream(&self, key: &str) -> Option<Arc<Stream>> {
        lock(&self.inner.streams).get(key.trim()).map(|(s, _)| s.clone())
    }

    /// What arrived on stream `key` since `cursor`, at most `max_frames` (the newest); `None`
    /// when there is no such stream or it has no format yet. A fresh cursor starts at the
    /// newest sample, not at the start of the ring.
    pub fn read(&self, key: &str, cursor: &mut Cursor, max_frames: usize) -> Option<Block> {
        let s = self.stream(key)?;
        let ring = lock(&s.ring);
        if ring.channels == 0 || ring.rate == 0 {
            return None;
        }
        let held = (ring.data.len() / ring.channels) as u64;
        let oldest = ring.total.saturating_sub(held);
        let from = match cursor.frame {
            Some(f) if cursor.generation == ring.generation => f,
            _ => ring.total,
        };
        let mut lost = oldest.saturating_sub(from);
        let mut start = from.max(oldest);
        let available = ring.total.saturating_sub(start);
        let take = available.min(max_frames as u64);
        lost += available - take;
        start += available - take;
        let skip = usize::try_from(start - oldest).unwrap_or(usize::MAX).saturating_mul(ring.channels);
        let mut channels = vec![Vec::with_capacity(usize::try_from(take).unwrap_or(0)); ring.channels];
        for (i, v) in
            ring.data.iter().skip(skip).take(usize::try_from(take).unwrap_or(0) * ring.channels).enumerate()
        {
            if let Some(c) = channels.get_mut(i % ring.channels) {
                c.push(*v);
            }
        }
        *cursor = Cursor { generation: ring.generation, frame: Some(start + take) };
        Some(Block { rate: ring.rate, channels, lost })
    }

    /// The stream's status, `None` when nothing is wanted or fed under `key`.
    pub fn stream_status(&self, key: &str) -> Option<StreamStatus> {
        let s = self.stream(key)?;
        Some(status_of(&s))
    }

    /// Every stream's status.
    pub fn status(&self) -> Vec<StreamStatus> {
        let streams: Vec<Arc<Stream>> = lock(&self.inner.streams).values().map(|(s, _)| s.clone()).collect();
        streams.iter().map(|s| status_of(s)).collect()
    }

    /// Calls `wake` when samples arrive on stream `key` or it opens, closes or fails, at most
    /// once until [`Watch::ack`]. Works before the stream exists.
    pub fn watch(&self, key: &str, wake: Wake) -> Watch {
        let key = key.trim().to_string();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(AtomicBool::new(false));
        lock(&self.inner.watches).push((key.clone(), id, pending.clone(), wake));
        Watch { key, id, pending, hub: Arc::downgrade(&self.inner) }
    }

    /// Puts interleaved samples into stream `key` as if a device had sent them (for tests and
    /// tools). The stream is made when missing; a different rate or channel count starts it
    /// over.
    pub fn feed(&self, key: &str, rate: u32, channels: usize, samples: &[f32]) -> Result<(), String> {
        if rate == 0 || channels == 0 || channels > MAX_CHANNELS {
            return Err(format!("{channels} channels at {rate} Hz cannot be fed"));
        }
        let key = key.trim().to_string();
        let s = {
            let mut streams = lock(&self.inner.streams);
            match streams.get(&key) {
                Some((s, _)) => s.clone(),
                None => {
                    let s = Arc::new(Stream::new(&key, true));
                    streams.insert(key.clone(), (s.clone(), None));
                    s
                }
            }
        };
        {
            let mut ring = lock(&s.ring);
            if ring.rate != rate || ring.channels != channels {
                ring.reset(rate, channels);
                let mut st = lock(&s.status);
                st.rate = rate;
                st.channels = u16::try_from(channels).unwrap_or(u16::MAX);
                st.open = true;
                st.opens += 1;
            }
            ring.push(samples.iter().copied());
        }
        {
            let mut st = lock(&s.status);
            st.frames += (samples.len() / channels) as u64;
            st.callbacks += 1;
        }
        s.mark_data();
        wake(&self.inner, &key);
        Ok(())
    }

    /// Closes every stream (at shutdown) and waits for their threads.
    pub fn shutdown(&self) {
        let all = std::mem::take(&mut *lock(&self.inner.streams));
        for (s, thread) in all.into_values() {
            s.stop.store(true, Ordering::Release);
            if let Some(t) = thread
                && t.join().is_err()
            {
                log::error!("an audio input thread panicked");
            }
        }
    }
}

fn status_of(s: &Stream) -> StreamStatus {
    let mut st = lock(&s.status).clone();
    let ring = lock(&s.ring);
    // Meters fall even while no callback comes.
    let fall = ring.last_meter.map_or(1.0, |t| METER_FALL.powf(t.elapsed().as_secs_f32()));
    st.peak_db = ring
        .peak
        .iter()
        .map(|p| {
            let p = f64::from(p * fall);
            if p > 1e-6 { 20.0 * p.log10() } else { -120.0 }
        })
        .collect();
    st
}

/// The stream thread: opens the device, watches it, reopens it after trouble.
fn run(inner: &Weak<Inner>, s: &Arc<Stream>) {
    let stopped = || s.stop.load(Ordering::Acquire);
    while !stopped() {
        *lock(&s.failed) = None;
        let opened = catch_unwind(AssertUnwindSafe(|| open(inner, s)))
            .unwrap_or_else(|_| Err("opening the device panicked".to_string()));
        match opened {
            Ok(stream) => {
                {
                    let mut st = lock(&s.status);
                    st.open = true;
                    st.error = None;
                    st.opens += 1;
                }
                s.mark_data();
                if let Some(i) = inner.upgrade() {
                    wake(&i, &s.key);
                }
                let trouble = loop {
                    std::thread::sleep(TICK);
                    if stopped() {
                        break None;
                    }
                    if let Some(e) = lock(&s.failed).take() {
                        break Some(e);
                    }
                    if s.since_data() > STALL {
                        break Some("no samples for a second".to_string());
                    }
                };
                // Dropping the stream stops it; on its own thread, where it was made.
                drop(stream);
                let mut st = lock(&s.status);
                st.open = false;
                if let Some(e) = trouble {
                    log::warn!("audio input `{}`: {e}; reopening", s.key);
                    st.error = Some(e);
                }
            }
            Err(e) => {
                let mut st = lock(&s.status);
                if st.error.as_deref() != Some(e.as_str()) {
                    log::warn!("audio input `{}`: {e}", s.key);
                }
                st.open = false;
                st.error = Some(e);
            }
        }
        if let Some(i) = inner.upgrade() {
            wake(&i, &s.key);
        }
        let until = Instant::now() + RETRY;
        while !stopped() && Instant::now() < until {
            std::thread::sleep(TICK);
        }
    }
}

/// The input device a pattern names: the default for an empty one, else an exact name, else
/// the first name containing it (case ignored).
fn find(host: &cpal::Host, pattern: &str) -> Result<cpal::Device, String> {
    if pattern.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| "this machine has no default audio input".to_string());
    }
    let list: Vec<cpal::Device> =
        host.input_devices().map_err(|e| format!("cannot list audio inputs: {e}"))?.collect();
    let lower = pattern.to_lowercase();
    let names: Vec<String> = list.iter().map(|d| name_of(d).unwrap_or_default()).collect();
    let exact = names.iter().position(|n| n.eq_ignore_ascii_case(pattern));
    let partial = || names.iter().position(|n| n.to_lowercase().contains(&lower));
    match exact.or_else(partial) {
        Some(i) => list.into_iter().nth(i).ok_or_else(|| "the device list changed".to_string()),
        None => Err(format!("no audio input matches `{pattern}`; inputs: {}", names.join(", "))),
    }
}

fn open(inner: &Weak<Inner>, s: &Arc<Stream>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = find(&host, &s.key)?;
    let name = name_of(&device);
    let config = device.default_input_config().map_err(|e| format!("cannot read the input format: {e}"))?;
    let channels = usize::from(config.channels());
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(format!("the device has {channels} channels (at most {MAX_CHANNELS} are read)"));
    }
    let rate = config.sample_rate();
    let format = config.sample_format();
    lock(&s.ring).reset(rate, channels);
    {
        let mut st = lock(&s.status);
        st.device = name;
        st.rate = rate;
        st.channels = config.channels();
        st.format = Some(format.to_string());
    }
    let data_stream = s.clone();
    let data_inner = inner.clone();
    let mut scratch: Vec<f32> = Vec::new();
    let on_data = move |data: &cpal::Data, _: &cpal::InputCallbackInfo| {
        // A panic must not unwind into the system's audio thread.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            scratch.clear();
            if !convert(data, &mut scratch) {
                *lock(&data_stream.failed) =
                    Some(format!("samples in an unexpected format ({})", data.sample_format()));
                return;
            }
            lock(&data_stream.ring).push(scratch.iter().copied());
            {
                let mut st = lock(&data_stream.status);
                st.frames += (scratch.len() / channels) as u64;
                st.callbacks += 1;
            }
            data_stream.mark_data();
            if let Some(i) = data_inner.upgrade() {
                wake(&i, &data_stream.key);
            }
        }));
    };
    let err_stream = s.clone();
    let on_error = move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => {
            lock(&err_stream.status).glitches += 1;
        }
        _ => *lock(&err_stream.failed) = Some(e.to_string()),
    };
    let stream = device
        .build_input_stream_raw(config.config(), format, on_data, on_error, Some(Duration::from_secs(5)))
        .map_err(|e| format!("cannot open the input: {e}"))?;
    stream.play().map_err(|e| format!("cannot start the input: {e}"))?;
    Ok(stream)
}

/// Appends `data`'s samples as `f32`; false for a format it does not know.
fn convert(data: &cpal::Data, out: &mut Vec<f32>) -> bool {
    fn take<T: SizedSample>(data: &cpal::Data, out: &mut Vec<f32>) -> bool
    where
        f32: FromSample<T>,
    {
        match data.as_slice::<T>() {
            Some(s) => {
                out.extend(s.iter().map(|v| v.to_sample::<f32>()));
                true
            }
            None => false,
        }
    }
    match data.sample_format() {
        SampleFormat::F32 => take::<f32>(data, out),
        SampleFormat::F64 => take::<f64>(data, out),
        SampleFormat::I8 => take::<i8>(data, out),
        SampleFormat::I16 => take::<i16>(data, out),
        SampleFormat::I32 => take::<i32>(data, out),
        SampleFormat::U8 => take::<u8>(data, out),
        SampleFormat::U16 => take::<u16>(data, out),
        SampleFormat::U32 => take::<u32>(data, out),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_get_what_arrived_since() {
        let hub = Hub::new();
        let mut a = Cursor::default();
        assert!(hub.read("t", &mut a, 1000).is_none());
        hub.feed("t", 48_000, 2, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        // A fresh cursor starts at the newest sample.
        assert_eq!(hub.read("t", &mut a, 1000).unwrap().frames(), 0);
        hub.feed("t", 48_000, 2, &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]).unwrap();
        let b = hub.read("t", &mut a, 1000).unwrap();
        assert_eq!(b.channels, vec![vec![1.0, 2.0, 3.0], vec![-1.0, -2.0, -3.0]]);
        assert_eq!(b.lost, 0);
        assert_eq!(hub.read("t", &mut a, 1000).unwrap().frames(), 0);
        // At most `max_frames`, the newest; the rest counts as lost.
        hub.feed("t", 48_000, 2, &[4.0, 0.0, 5.0, 0.0, 6.0, 0.0]).unwrap();
        let b = hub.read("t", &mut a, 2).unwrap();
        assert_eq!(b.channels[0], vec![5.0, 6.0]);
        assert_eq!(b.lost, 1);
    }

    #[test]
    fn a_slow_reader_loses_the_oldest() {
        let hub = Hub::new();
        let mut c = Cursor::default();
        hub.feed("t", 1000, 1, &[0.0]).unwrap();
        hub.read("t", &mut c, 10_000).unwrap();
        // The ring holds two seconds: 2000 frames.
        let samples: Vec<f32> = (0..2500).map(|i| i as f32).collect();
        hub.feed("t", 1000, 1, &samples).unwrap();
        let b = hub.read("t", &mut c, 10_000).unwrap();
        assert_eq!(b.frames(), 2000);
        assert_eq!(b.lost, 500);
        assert_eq!(b.channels[0][0], 500.0);
    }

    #[test]
    fn a_new_format_starts_readers_over() {
        let hub = Hub::new();
        let mut c = Cursor::default();
        hub.feed("t", 48_000, 1, &[0.0; 10]).unwrap();
        hub.read("t", &mut c, 100).unwrap();
        hub.feed("t", 96_000, 2, &[0.5; 10]).unwrap();
        let b = hub.read("t", &mut c, 100).unwrap();
        assert_eq!((b.rate, b.frames()), (96_000, 0));
        assert!(hub.feed("t", 0, 1, &[]).is_err());
        assert!(hub.feed("t", 48_000, MAX_CHANNELS + 1, &[]).is_err());
    }

    #[test]
    fn watchers_are_woken_once_until_they_acknowledge() {
        let hub = Hub::new();
        let count = Arc::new(AtomicU64::new(0));
        let c = count.clone();
        let w = hub.watch(
            "t",
            Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }),
        );
        hub.feed("t", 48_000, 1, &[0.0]).unwrap();
        hub.feed("t", 48_000, 1, &[0.0]).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        w.ack();
        hub.feed("t", 48_000, 1, &[0.0]).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 2);
        drop(w);
        hub.feed("t", 48_000, 1, &[0.0]).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn meters_follow_the_peaks() {
        let hub = Hub::new();
        hub.feed("t", 48_000, 2, &[0.5, 0.0, -1.0, f32::NAN]).unwrap();
        let st = hub.stream_status("t").unwrap();
        assert!((st.peak_db[0] - 0.0).abs() < 0.1, "{:?}", st.peak_db);
        assert_eq!(st.peak_db[1], -120.0);
        assert!(st.fed && st.open);
    }
}
