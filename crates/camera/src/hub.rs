//! The camera hub: one capture thread per camera and mode the network wants, the newest frame
//! of each, and the hand-over to whoever shows it.
//!
//! - **Streams** are wanted by the network ([`Hub::set_wanted`]: a device pattern and a mode
//!   text per `tex.videoin` node). Each gets a thread that opens the camera, reads frames as
//!   they come, and after an error reopens it two seconds later. Reading blocks until the camera
//!   delivers; a stream that is no longer wanted ends after its next frame and is never waited
//!   for, except briefly at shutdown.
//! - **Frames** replace each other: a stream keeps only the newest ([`Hub::latest`]), because a
//!   live picture wants the freshest frame, not every frame. [`Hub::watch`] calls back when one
//!   arrives, at most once until the watcher acknowledges.
//! - [`Hub::feed`] puts a frame into a stream without a camera, for tests and tools.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::{Deserialize, Serialize};

use crate::backend::{self, Capture};
use crate::frame::CameraFrame;
use crate::mode::{DeviceInfo, Mode, ModeInfo, Wanted};

/// How long a stream waits before it tries a camera again.
pub const RETRY: Duration = Duration::from_secs(2);
/// How long shutdown waits for capture threads (one may be blocked in a read that never ends).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(50);
/// The window the frame rate is measured over.
const RATE_WINDOW: Duration = Duration::from_secs(2);

/// What a node asks for: a camera by part of its name (empty: the first) and a mode text
/// (see [`Wanted::parse`]; empty: the largest, fastest).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Key {
    pub device: String,
    pub mode: String,
}

impl Key {
    pub fn new(device: &str, mode: &str) -> Self {
        Self { device: device.trim().to_string(), mode: mode.trim().to_string() }
    }

    fn label(&self) -> String {
        let d = if self.device.is_empty() { "first camera" } else { self.device.as_str() };
        if self.mode.is_empty() { d.to_string() } else { format!("{d} {}", self.mode) }
    }
}

/// Settings for all streams. Not stored: every start has the defaults (`videoin.config`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Off closes every camera (the wants are kept for when it is on again).
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// A stream as it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StreamStatus {
    pub key: Key,
    /// The camera opened.
    pub device: Option<String>,
    /// The mode it runs in, as a label (`1920x1080@60 NV12`).
    pub mode: Option<String>,
    pub open: bool,
    pub error: Option<String>,
    /// Frames received since the stream was made.
    pub frames: u64,
    /// Times the camera was opened.
    pub opens: u64,
    /// Frames the camera's timestamps say were lost before they reached penumbra.
    pub dropped: u64,
    /// Frames per second arriving, measured over the last two seconds.
    pub fps: f64,
    /// Milliseconds from the camera's timestamp to the capture thread having the frame,
    /// smoothed; `None` when the camera's clock is not the system's.
    pub delay_ms: Option<f64>,
    /// Milliseconds the capture thread spends rearranging a frame, smoothed.
    pub copy_ms: Option<f64>,
    /// Milliseconds since the last frame.
    pub since_frame_ms: Option<f64>,
    /// The modes of the camera opened.
    pub modes: Vec<ModeInfo>,
    /// Fed by [`Hub::feed`], not by a camera.
    pub fed: bool,
}

#[derive(Default)]
struct Meter {
    arrivals: VecDeque<Instant>,
    last_stamp: Option<i64>,
    delay: Option<f64>,
    copy: Option<f64>,
}

struct Stream {
    key: Key,
    latest: Mutex<Option<Arc<CameraFrame>>>,
    status: Mutex<StreamStatus>,
    meter: Mutex<Meter>,
    stop: AtomicBool,
}

impl Stream {
    fn new(key: &Key, fed: bool) -> Self {
        Self {
            key: key.clone(),
            latest: Mutex::new(None),
            status: Mutex::new(StreamStatus { key: key.clone(), fed, ..StreamStatus::default() }),
            meter: Mutex::new(Meter::default()),
            stop: AtomicBool::new(false),
        }
    }

    /// Takes a frame in: counts it, measures it, makes it the newest.
    fn put(&self, mut frame: CameraFrame, stamp: Option<i64>, fps: f64, copy_ms: Option<f64>) {
        let mut dropped = 0;
        {
            let mut m = lock(&self.meter);
            let now = frame.arrived;
            m.arrivals.push_back(now);
            while m.arrivals.front().is_some_and(|t| now.saturating_duration_since(*t) > RATE_WINDOW) {
                m.arrivals.pop_front();
            }
            if let (Some(s), Some(prev)) = (stamp, m.last_stamp)
                && fps > 0.0
            {
                let periods = (s - prev) as f64 * fps / 1e7;
                if periods > 1.5 && periods < 1e6 {
                    // `as`: a rounded, bounded count.
                    dropped = periods.round() as u64 - 1;
                }
            }
            if stamp.is_some() {
                m.last_stamp = stamp;
            }
            let smooth = |old: Option<f64>, new: f64| Some(old.map_or(new, |old| old + 0.1 * (new - old)));
            if let Some(d) = frame.delay_ms {
                m.delay = smooth(m.delay, d);
            }
            if let Some(c) = copy_ms {
                m.copy = smooth(m.copy, c);
            }
        }
        let mut st = lock(&self.status);
        st.frames += 1;
        st.dropped += dropped;
        frame.sequence = st.frames;
        drop(st);
        *lock(&self.latest) = Some(Arc::new(frame));
    }
}

type Wake = Arc<dyn Fn() + Send + Sync>;
type Entry = (Arc<Stream>, Option<JoinHandle<()>>);
/// A watcher: stream key, id, whether a callback is pending, the callback.
type Watcher = (Key, u64, Arc<AtomicBool>, Wake);

struct Inner {
    config: Mutex<Config>,
    wanted: Mutex<BTreeSet<Key>>,
    streams: Mutex<BTreeMap<Key, Entry>>,
    watches: Mutex<Vec<Watcher>>,
    next_id: AtomicU64,
}

/// The camera hub, shared by the engine, the nodes and the commands (clones share it).
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
    key: Key,
    id: u64,
    pending: Arc<AtomicBool>,
    hub: Weak<Inner>,
}

impl Watch {
    /// Allows the next callback (call before looking at the newest frame).
    pub fn ack(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn key(&self) -> &Key {
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

fn wake(inner: &Inner, key: &Key) {
    let watches: Vec<Wake> = lock(&inner.watches)
        .iter()
        .filter(|(k, _, pending, _)| k == key && !pending.swap(true, Ordering::AcqRel))
        .map(|(_, _, _, w)| w.clone())
        .collect();
    for w in watches {
        w();
    }
}

/// The cameras of this machine and their modes (asks the system and opens each camera
/// briefly: tens of milliseconds, not for every frame). A camera a stream holds open may not
/// list its modes; [`Hub::devices`] fills them in from the stream.
pub fn devices() -> Result<Vec<DeviceInfo>, String> {
    backend::devices()
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

    /// Changes the settings; off closes every camera, on opens what is wanted.
    pub fn set_config(&self, config: Config) {
        *lock(&self.inner.config) = config;
        self.apply();
    }

    /// The streams the network wants.
    pub fn set_wanted(&self, keys: &[Key]) {
        let keys: BTreeSet<Key> = keys.iter().map(|k| Key::new(&k.device, &k.mode)).collect();
        {
            let mut wanted = lock(&self.inner.wanted);
            if *wanted == keys {
                return;
            }
            *wanted = keys;
        }
        self.apply();
    }

    pub fn wanted(&self) -> Vec<Key> {
        lock(&self.inner.wanted).iter().cloned().collect()
    }

    fn apply(&self) {
        let wanted = if self.config().enabled { lock(&self.inner.wanted).clone() } else { BTreeSet::new() };
        let mut streams = lock(&self.inner.streams);
        let gone: Vec<Key> = streams
            .iter()
            .filter(|(k, (s, _))| !wanted.contains(*k) && !lock(&s.status).fed)
            .map(|(k, _)| k.clone())
            .collect();
        let mut touched = Vec::new();
        for key in gone {
            if let Some((s, _thread)) = streams.remove(&key) {
                // Not joined: the thread ends after its current read, and the cook thread
                // never waits for a camera.
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
                .name(format!("camera {}", key.label()))
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

    fn stream(&self, key: &Key) -> Option<Arc<Stream>> {
        lock(&self.inner.streams).get(key).map(|(s, _)| s.clone())
    }

    /// The newest frame of stream `key`, if it has one.
    pub fn latest(&self, key: &Key) -> Option<Arc<CameraFrame>> {
        lock(&self.stream(key)?.latest).clone()
    }

    pub fn stream_status(&self, key: &Key) -> Option<StreamStatus> {
        let s = self.stream(key)?;
        Some(status_of(&s))
    }

    pub fn status(&self) -> Vec<StreamStatus> {
        let streams: Vec<Arc<Stream>> = lock(&self.inner.streams).values().map(|(s, _)| s.clone()).collect();
        streams.iter().map(|s| status_of(s)).collect()
    }

    /// [`devices`], with the modes of cameras this hub holds open taken from their streams.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        let mut list = devices()?;
        let status = self.status();
        for d in &mut list {
            if !d.modes.is_empty() {
                continue;
            }
            if let Some(st) =
                status.iter().find(|s| s.device.as_deref() == Some(d.name.as_str()) && !s.modes.is_empty())
            {
                d.modes = st.modes.clone();
                d.error = None;
            }
        }
        Ok(list)
    }

    /// Calls `wake` when a frame arrives on stream `key` or it opens, closes or fails, at most
    /// once until [`Watch::ack`]. Works before the stream exists.
    pub fn watch(&self, key: &Key, wake: Wake) -> Watch {
        let key = Key::new(&key.device, &key.mode);
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(AtomicBool::new(false));
        lock(&self.inner.watches).push((key.clone(), id, pending.clone(), wake));
        Watch { key, id, pending, hub: Arc::downgrade(&self.inner) }
    }

    /// Puts a frame into stream `key` as if a camera had sent it (for tests and tools).
    pub fn feed(&self, key: &Key, frame: CameraFrame) {
        let key = Key::new(&key.device, &key.mode);
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
            let mut st = lock(&s.status);
            if !st.open {
                st.open = true;
                st.opens += 1;
            }
            st.mode = Some(format!("{}x{} {}", frame.width, frame.height, frame.format.name()));
        }
        s.put(frame, None, 0.0, None);
        wake(&self.inner, &key);
    }

    /// Closes every camera (at shutdown) and waits briefly for the threads.
    pub fn shutdown(&self) {
        let all = std::mem::take(&mut *lock(&self.inner.streams));
        let threads: Vec<JoinHandle<()>> = all
            .into_values()
            .filter_map(|(s, t)| {
                s.stop.store(true, Ordering::Release);
                t
            })
            .collect();
        let until = Instant::now() + SHUTDOWN_WAIT;
        for t in threads {
            while !t.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !t.is_finished() {
                log::warn!("a camera thread is still reading; left behind");
                continue;
            }
            if t.join().is_err() {
                log::error!("a camera thread panicked");
            }
        }
    }
}

fn status_of(s: &Stream) -> StreamStatus {
    let mut st = lock(&s.status).clone();
    let m = lock(&s.meter);
    let now = Instant::now();
    let recent: Vec<&Instant> =
        m.arrivals.iter().filter(|t| now.saturating_duration_since(**t) <= RATE_WINDOW).collect();
    st.fps = match (recent.first(), recent.last()) {
        (Some(a), Some(b)) if recent.len() > 1 => {
            let span = b.saturating_duration_since(**a).as_secs_f64();
            if span > 0.0 { (recent.len() - 1) as f64 / span } else { 0.0 }
        }
        _ => 0.0,
    };
    st.delay_ms = m.delay;
    st.copy_ms = m.copy;
    st.since_frame_ms = m.arrivals.back().map(|t| now.saturating_duration_since(*t).as_secs_f64() * 1000.0);
    st
}

/// The stream thread: opens the camera, reads it, reopens it after trouble.
fn run(inner: &Weak<Inner>, s: &Arc<Stream>) {
    let stopped = || s.stop.load(Ordering::Acquire);
    while !stopped() {
        let trouble = catch_unwind(AssertUnwindSafe(|| capture(inner, s)))
            .unwrap_or_else(|_| Some("the camera thread panicked".to_string()));
        {
            let mut st = lock(&s.status);
            st.open = false;
            if let Some(e) = trouble {
                if st.error.as_deref() != Some(e.as_str()) {
                    log::warn!("camera `{}`: {e}", s.key.label());
                }
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

/// One opening of the camera, read until it fails or the stream stops. The error, if any.
fn capture(inner: &Weak<Inner>, s: &Arc<Stream>) -> Option<String> {
    let wanted = match Wanted::parse(&s.key.mode) {
        Ok(w) => w,
        Err(e) => return Some(e),
    };
    let (mut cam, opened) = match Capture::open(&s.key.device, &wanted) {
        Ok(c) => c,
        Err(e) => return Some(e),
    };
    let mode: Mode = opened.mode;
    {
        let mut st = lock(&s.status);
        st.device = Some(opened.device.clone());
        st.mode = Some(mode.label());
        st.modes = opened.modes.iter().map(Mode::info).collect();
        st.open = true;
        st.error = None;
        st.opens += 1;
    }
    log::info!("camera `{}`: {} in {}", s.key.label(), opened.device, mode.label());
    lock(&s.meter).last_stamp = None;
    if let Some(i) = inner.upgrade() {
        wake(&i, &s.key);
    }
    while !s.stop.load(Ordering::Acquire) {
        match cam.read() {
            Ok(Some(sample)) => {
                let (sx, sy) = mode.format.shift();
                let frame = CameraFrame {
                    width: mode.width,
                    height: mode.height,
                    format: mode.format,
                    y: sample.planes.y,
                    cb: sample.planes.cb,
                    cr: sample.planes.cr,
                    shift: (sx, sy),
                    matrix: sample.matrix,
                    full_range: sample.full_range,
                    sequence: 0,
                    arrived: Instant::now(),
                    delay_ms: sample.delay_ms,
                };
                s.put(frame, sample.stamp, mode.fps(), Some(sample.copy_ms));
                let i = inner.upgrade()?;
                wake(&i, &s.key);
            }
            Ok(None) => {}
            Err(e) => return Some(e),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Matrix, PixelFormat};

    fn frame(w: u32, h: u32) -> CameraFrame {
        CameraFrame {
            width: w,
            height: h,
            format: PixelFormat::Nv12,
            y: vec![16; (w * h) as usize],
            cb: vec![128; (w * h / 4) as usize],
            cr: vec![128; (w * h / 4) as usize],
            shift: (1, 1),
            matrix: Matrix::Bt709,
            full_range: false,
            sequence: 0,
            arrived: Instant::now(),
            delay_ms: Some(20.0),
        }
    }

    #[test]
    fn the_newest_frame_wins_and_watchers_wake_once() {
        let hub = Hub::new();
        let key = Key::new(" cam ", "");
        let count = Arc::new(AtomicU64::new(0));
        let c = count.clone();
        let w = hub.watch(
            &key,
            Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }),
        );
        assert!(hub.latest(&key).is_none());
        hub.feed(&key, frame(4, 2));
        hub.feed(&key, frame(8, 4));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let f = hub.latest(&Key::new("cam", "")).unwrap();
        assert_eq!((f.width, f.sequence), (8, 2));
        w.ack();
        hub.feed(&key, frame(4, 2));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        let st = hub.stream_status(&key).unwrap();
        assert!(st.fed && st.open);
        assert_eq!((st.frames, st.opens), (3, 1));
        assert_eq!(st.delay_ms, Some(20.0));
        drop(w);
        hub.feed(&key, frame(4, 2));
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn gaps_in_the_cameras_timestamps_count_as_dropped() {
        let s = Stream::new(&Key::default(), true);
        // 60 fps: a period is 166_667 units of 100 ns.
        for stamp in [0, 166_667, 333_333, 833_333, 1_000_000] {
            s.put(frame(2, 2), Some(stamp), 60.0, None);
        }
        assert_eq!(lock(&s.status).dropped, 2);
    }

    #[test]
    fn a_stream_without_a_camera_reports_why() {
        let hub = Hub::new();
        let key = Key::new("no such camera anywhere", "");
        hub.set_wanted(std::slice::from_ref(&key));
        let until = Instant::now() + Duration::from_secs(10);
        while hub.stream_status(&key).and_then(|s| s.error).is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let st = hub.stream_status(&key).unwrap();
        assert!(!st.open);
        assert!(st.error.is_some(), "{st:?}");
        hub.set_wanted(&[]);
        assert!(hub.stream_status(&key).is_none());
        hub.shutdown();
    }
}
