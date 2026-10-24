//! Outputs, one per target, each with a thread that sends the newest frame at a steady rate.
//!
//! A node asks the [`Hub`] for an [`Output`] to a [`Target`] and puts its slots into it
//! ([`Output::set`]); several outputs to the same target share one link, each owning the slot
//! range it last wrote (a later writer wins where ranges overlap, and the status says so). The
//! link's thread composes the frame from all of them and sends it whether or not anything
//! changed: the Open DMX USB has no memory of its own, and sACN receivers drop a source after
//! 2.5 s of silence. When the last output to a target is dropped, its thread stops.
//!
//! Nothing here blocks the caller: sockets are non-blocking, serial writes happen on the link's
//! thread, and a port that cannot be opened (not plugged in, taken by another program) is
//! tried again every second.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

use crate::serial::{self, Line};
use crate::{SLOTS, artnet, ddp, sacn};

/// Universes one output may span (Art-Net, sACN).
pub const MAX_UNIVERSES: usize = 64;
/// Bytes one DDP output may send (21845 RGB pixels).
pub const MAX_DDP_BYTES: usize = 65536;
/// How often a port that failed is tried again.
const RETRY: Duration = Duration::from_secs(1);
/// Frames the timing statistics cover.
const WINDOW: usize = 120;
/// The longest sleep between looking at the stop flag.
const NAP: Duration = Duration::from_millis(50);

/// Where an output goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum Target {
    /// An Enttec Open DMX USB on a serial port; an empty `port` is the first FT232R found, looked
    /// up again whenever the port has to be opened.
    OpenDmx { port: String },
    /// Art-Net to an address (a node, or a broadcast address).
    ArtNet { to: SocketAddr },
    /// sACN to an address (a receiver), or to each universe's multicast group when `to` is
    /// `None`.
    Sacn { to: Option<SocketAddr> },
    /// DDP to a pixel controller.
    Ddp { to: SocketAddr },
}

impl Target {
    /// How slots are laid out: universes of 512 (spilling into the next), one universe, or one
    /// flat buffer.
    fn layout(&self) -> Layout {
        match self {
            Target::OpenDmx { .. } => Layout::Single,
            Target::ArtNet { .. } => Layout::Universes(artnet::MAX_UNIVERSE),
            Target::Sacn { .. } => Layout::Universes(sacn::MAX_UNIVERSE),
            Target::Ddp { .. } => Layout::Flat,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Single,
    /// Up to this universe.
    Universes(u16),
    Flat,
}

/// Switches for all outputs, for this run.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Config {
    /// Off: no output sends anything (the Open DMX USB's port is closed), so a test run cannot
    /// reach the lights.
    pub enabled: bool,
    /// Every output sends zeros in place of its slots.
    pub blackout: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true, blackout: false }
    }
}

/// Minimum, mean and maximum of a timing, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    pub min: f64,
    pub mean: f64,
    pub max: f64,
}

impl Stats {
    fn of(samples: &VecDeque<f64>) -> Stats {
        if samples.is_empty() {
            return Stats::default();
        }
        let (mut min, mut max, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for &v in samples {
            min = min.min(v);
            max = max.max(v);
            sum += v;
        }
        // `as`: a count of at most WINDOW.
        Stats { min, mean: sum / samples.len() as f64, max }
    }
}

/// What a link did lately.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub target: Target,
    /// The writers: their labels (node paths), in the order they came.
    pub writers: Vec<String>,
    /// The port actually open (the Open DMX USB), or the address sent to.
    pub device: Option<String>,
    /// Whether the last frame went out.
    pub open: bool,
    /// The last error, cleared by the next frame that goes out.
    pub error: Option<String>,
    /// Frames sent since the link started.
    pub frames: u64,
    /// Frames (or packets) that failed.
    pub failures: u64,
    /// How often the port was opened.
    pub opens: u64,
    /// The rate asked for (the highest of the writers'), in Hz.
    pub rate: f64,
    /// Frames per second over the last frames.
    pub measured: f64,
    /// Start to start of consecutive frames, over the last frames.
    pub period: Stats,
    /// The Open DMX USB: break calls; zero for the network.
    pub brk: Stats,
    /// The Open DMX USB: writing a frame until the driver had sent it; the network: sending all
    /// of a frame's packets.
    pub write: Stats,
    /// The universes sent (Art-Net, sACN); `[0]` for the Open DMX USB and DDP.
    pub universes: Vec<u16>,
    /// Slots written by more than one writer (the later wins).
    pub overlap: usize,
    /// Slots that fell outside what the target can carry, and were left out.
    pub dropped: usize,
}

/// The slots one writer put in.
struct Slice {
    label: String,
    /// The first slot: see [`Output::set`].
    start: usize,
    data: Vec<u8>,
    rate: f64,
    order: u64,
}

/// One target's writers and what its thread found out.
struct Link {
    target: Target,
    writers: Mutex<BTreeMap<u64, Slice>>,
    /// Counts changes of `writers`, so the thread composes only after one.
    version: AtomicU64,
    stop: AtomicBool,
    status: Mutex<Status>,
    /// Called when `open` or `error` changes, so a node can show it.
    wakers: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
}

/// A running link and its thread.
type Running = (Arc<Link>, Option<JoinHandle<()>>);

struct Inner {
    links: Mutex<BTreeMap<Target, Running>>,
    /// Links whose last output was dropped, until their thread has ended.
    stopping: Mutex<Vec<(Target, JoinHandle<()>)>>,
    config: Mutex<Config>,
    next: AtomicU64,
    /// This run's sACN source id.
    cid: [u8; 16],
}

/// All DMX outputs of the process. Cheap to clone.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                links: Mutex::new(BTreeMap::new()),
                stopping: Mutex::new(Vec::new()),
                config: Mutex::new(Config::default()),
                next: AtomicU64::new(1),
                cid: source_id(),
            }),
        }
    }

    pub fn config(&self) -> Config {
        *lock(&self.inner.config)
    }

    pub fn set_config(&self, config: Config) {
        *lock(&self.inner.config) = config;
    }

    /// An output to `target`, starting the link's thread if it is the first. `label` names the
    /// writer in the status (a node path); `wake` is called when the link opens, closes or fails.
    pub fn output(
        &self,
        label: &str,
        target: Target,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Output, String> {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let mut links = lock(&self.inner.links);
        let link = match links.get(&target) {
            Some((link, _)) => link.clone(),
            None => {
                // A link to the same target that is still ending holds the port: wait for it
                // (it looks at its stop flag at least every 50 ms, a frame takes 25).
                let old: Vec<JoinHandle<()>> = {
                    let mut stopping = lock(&self.inner.stopping);
                    let (same, rest): (Vec<_>, Vec<_>) = stopping.drain(..).partition(|(t, _)| *t == target);
                    *stopping = rest;
                    same.into_iter().map(|(_, h)| h).collect()
                };
                for h in old {
                    h.join().ok();
                }
                let link = Arc::new(Link::new(target.clone()));
                let (inner, l) = (Arc::downgrade(&self.inner), link.clone());
                let handle = std::thread::Builder::new()
                    .name(format!("dmx {}", describe(&target)))
                    .spawn(move || run(&inner, &l))
                    .map_err(|e| format!("cannot start the DMX thread: {e}"))?;
                links.insert(target.clone(), (link.clone(), Some(handle)));
                link
            }
        };
        lock(&link.wakers).insert(id, wake);
        let order = link.version.fetch_add(1, Ordering::AcqRel);
        lock(&link.writers)
            .insert(id, Slice { label: label.to_string(), start: 0, data: Vec::new(), rate: 0.0, order });
        Ok(Output { link, id, hub: Arc::downgrade(&self.inner) })
    }

    /// Every link's status, by target.
    pub fn status(&self) -> Vec<Status> {
        lock(&self.inner.links).values().map(|(l, _)| lock(&l.status).clone()).collect()
    }

    /// Stops every link and waits for the threads.
    pub fn shutdown(&self) {
        let links = std::mem::take(&mut *lock(&self.inner.links));
        let stopping = std::mem::take(&mut *lock(&self.inner.stopping));
        for (link, _) in links.values() {
            link.stop.store(true, Ordering::Release);
        }
        for (_, h) in links.into_values() {
            if let Some(h) = h {
                h.join().ok();
            }
        }
        for (_, h) in stopping {
            h.join().ok();
        }
    }
}

/// One writer's way into a link. Dropping it takes its slots out; the last one stops the link.
pub struct Output {
    link: Arc<Link>,
    id: u64,
    hub: Weak<Inner>,
}

impl Output {
    pub fn target(&self) -> &Target {
        &self.link.target
    }

    /// Puts `data` in from slot `start`, to be sent at `rate` Hz (the link takes the highest of
    /// its writers'). Art-Net and sACN count slots over all universes, `universe * 512 +
    /// address - 1`, and spill into the following universes (at most 64); the Open DMX USB
    /// and DDP count from 0. Unchanged data costs a comparison.
    pub fn set(&self, start: usize, data: &[u8], rate: f64) {
        let mut writers = lock(&self.link.writers);
        let Some(s) = writers.get_mut(&self.id) else { return };
        if s.start == start && s.data == data && s.rate == rate {
            return;
        }
        s.start = start;
        s.data = data.to_vec();
        s.rate = rate;
        self.link.version.fetch_add(1, Ordering::Release);
    }

    /// The link's status.
    pub fn status(&self) -> Status {
        lock(&self.link.status).clone()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        lock(&self.link.wakers).remove(&self.id);
        let empty = {
            let mut writers = lock(&self.link.writers);
            writers.remove(&self.id);
            self.link.version.fetch_add(1, Ordering::Release);
            writers.is_empty()
        };
        if !empty {
            return;
        }
        let Some(inner) = self.hub.upgrade() else { return };
        let mut links = lock(&inner.links);
        // Another output may have joined between the two locks; only an empty link stops.
        if lock(&self.link.writers).is_empty()
            && let Some((link, handle)) = links.remove(&self.link.target)
        {
            link.stop.store(true, Ordering::Release);
            if let Some(h) = handle {
                lock(&inner.stopping).push((link.target.clone(), h));
            }
        }
    }
}

impl Link {
    fn new(target: Target) -> Self {
        let status = Status {
            target: target.clone(),
            writers: Vec::new(),
            device: None,
            open: false,
            error: None,
            frames: 0,
            failures: 0,
            opens: 0,
            rate: 0.0,
            measured: 0.0,
            period: Stats::default(),
            brk: Stats::default(),
            write: Stats::default(),
            universes: Vec::new(),
            overlap: 0,
            dropped: 0,
        };
        Self {
            target,
            writers: Mutex::new(BTreeMap::new()),
            version: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            status: Mutex::new(status),
            wakers: Mutex::new(BTreeMap::new()),
        }
    }

    fn wake(&self) {
        let wakers: Vec<_> = lock(&self.wakers).values().cloned().collect();
        for w in wakers {
            w();
        }
    }
}

/// A frame as the link sends it.
#[derive(Default)]
struct Frame {
    /// Universe and its 512 slots (Art-Net, sACN); for the Open DMX USB and DDP one entry, 0,
    /// with the whole buffer.
    parts: BTreeMap<u16, Vec<u8>>,
    rate: f64,
    writers: Vec<String>,
    overlap: usize,
    dropped: usize,
}

/// The writers' slices laid out for `layout`, the earliest writer first so later ones win.
fn compose(writers: &BTreeMap<u64, Slice>, layout: Layout) -> Frame {
    let mut order: Vec<&Slice> = writers.values().collect();
    order.sort_by_key(|s| s.order);
    let mut f = Frame::default();
    let mut owner: BTreeMap<usize, usize> = BTreeMap::new();
    for (w, s) in order.iter().enumerate() {
        f.rate = f.rate.max(s.rate);
        f.writers.push(s.label.clone());
        for (i, v) in s.data.iter().enumerate() {
            let slot = s.start + i;
            let universe = u16::try_from(slot / SLOTS).ok();
            let (part, at) = match (layout, universe) {
                (Layout::Single, _) if slot < SLOTS => (0, slot),
                (Layout::Universes(last), Some(u)) if u <= last && i < SLOTS * MAX_UNIVERSES => {
                    (u, slot % SLOTS)
                }
                (Layout::Flat, _) if slot < MAX_DDP_BYTES => (0, slot),
                _ => {
                    f.dropped += 1;
                    continue;
                }
            };
            if owner.insert(slot, w).is_some_and(|o| o != w) {
                f.overlap += 1;
            }
            let buf = f
                .parts
                .entry(part)
                .or_insert_with(|| vec![0; if matches!(layout, Layout::Flat) { 0 } else { SLOTS }]);
            if buf.len() <= at {
                buf.resize(at + 1, 0);
            }
            if let Some(d) = buf.get_mut(at) {
                *d = *v;
            }
        }
    }
    f
}

/// The transport a link's thread holds.
enum Wire {
    Serial(Option<Line>),
    Udp(Option<UdpSocket>),
}

fn run(hub: &Weak<Inner>, link: &Arc<Link>) {
    // A panic in here must not take the process along; the link then stays closed.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_link(hub, link)));
    if outcome.is_err() {
        let mut st = lock(&link.status);
        st.open = false;
        st.error = Some("the DMX thread failed; remove and add the node to restart it".to_string());
    }
}

fn run_link(hub: &Weak<Inner>, link: &Arc<Link>) {
    let Some(cid) = hub.upgrade().map(|h| h.cid) else { return };
    let mut wire = match link.target {
        Target::OpenDmx { .. } => Wire::Serial(None),
        _ => Wire::Udp(None),
    };
    let mut seen = u64::MAX;
    let mut frame = Frame::default();
    let mut sequence: u8 = 0;
    let mut next = Instant::now();
    let mut retry_at: Option<Instant> = None;
    let mut starts: VecDeque<Instant> = VecDeque::new();
    let (mut brk, mut write) = (VecDeque::new(), VecDeque::new());
    let mut last_shown: (bool, Option<String>) = (false, None);
    while !link.stop.load(Ordering::Acquire) {
        let Some(config) = hub.upgrade().map(|h| *lock(&h.config)) else { return };
        let version = link.version.load(Ordering::Acquire);
        if version != seen {
            seen = version;
            frame = compose(&lock(&link.writers), link.target.layout());
            let mut st = lock(&link.status);
            st.rate = frame.rate;
            st.writers = frame.writers.clone();
            st.overlap = frame.overlap;
            st.dropped = frame.dropped;
            st.universes = frame.parts.keys().copied().collect();
        }
        let period = Duration::from_secs_f64(1.0 / frame.rate.clamp(0.5, 1000.0));
        let mut outcome: Result<Option<(Duration, Duration)>, String> = Ok(None);
        let mut device = None;
        if !config.enabled {
            wire = match wire {
                Wire::Serial(_) => Wire::Serial(None),
                Wire::Udp(_) => Wire::Udp(None),
            };
            outcome = Err("DMX output is switched off (dmx.config enabled)".to_string());
        } else if retry_at.is_some_and(|t| Instant::now() < t) {
            outcome = Err(lock(&link.status).error.clone().unwrap_or_default());
        } else {
            retry_at = None;
            let blank = |v: &Vec<u8>| if config.blackout { vec![0; v.len()] } else { v.clone() };
            match (&mut wire, &link.target) {
                (Wire::Serial(line), Target::OpenDmx { port }) => {
                    if line.is_none() {
                        let name =
                            if port.trim().is_empty() { serial::find_open_dmx() } else { Some(port.clone()) };
                        match name {
                            None => outcome = Err("no Open DMX USB (FTDI FT232R) is plugged in".to_string()),
                            Some(name) => match Line::open(&name) {
                                Ok(l) => {
                                    *line = Some(l);
                                    lock(&link.status).opens += 1;
                                }
                                Err(e) => outcome = Err(e),
                            },
                        }
                    }
                    if let Some(l) = line.as_mut() {
                        device = Some(l.name().to_string());
                        let slots = frame.parts.get(&0).map(blank).unwrap_or_default();
                        match l.send(&slots) {
                            Ok(t) => outcome = Ok(Some((t.brk, t.write))),
                            Err(e) => {
                                // Unplugged or taken: close, and open again after a pause.
                                *line = None;
                                outcome = Err(e);
                            }
                        }
                    }
                }
                (Wire::Udp(socket), target) => {
                    if socket.is_none() {
                        match open_socket(target) {
                            Ok(s) => {
                                *socket = Some(s);
                                lock(&link.status).opens += 1;
                            }
                            Err(e) => outcome = Err(e),
                        }
                    }
                    if let Some(s) = socket.as_ref() {
                        sequence = sequence.wrapping_add(1);
                        let t0 = Instant::now();
                        let (sent, to) = send_packets(s, target, &cid, sequence, &frame, &blank);
                        device = to;
                        outcome = sent.map(|()| Some((Duration::ZERO, t0.elapsed())));
                    }
                }
                _ => outcome = Err("internal: transport and target disagree".to_string()),
            }
        }
        let now = Instant::now();
        {
            let mut st = lock(&link.status);
            st.device = device.or(st.device.take());
            match outcome {
                Ok(Some((b, w))) => {
                    st.open = true;
                    st.error = None;
                    st.frames += 1;
                    push(&mut starts, now, WINDOW + 1);
                    push(&mut brk, b.as_secs_f64() * 1e3, WINDOW);
                    push(&mut write, w.as_secs_f64() * 1e3, WINDOW);
                    st.brk = Stats::of(&brk);
                    st.write = Stats::of(&write);
                    let gaps: VecDeque<f64> = starts
                        .iter()
                        .zip(starts.iter().skip(1))
                        .map(|(a, b)| (*b - *a).as_secs_f64() * 1e3)
                        .collect();
                    st.period = Stats::of(&gaps);
                    st.measured = if st.period.mean > 0.0 { 1e3 / st.period.mean } else { 0.0 };
                }
                Ok(None) => {}
                Err(e) => {
                    if st.open || st.error.as_deref() != Some(e.as_str()) {
                        st.failures += 1;
                    }
                    st.open = false;
                    st.error = Some(e);
                    st.measured = 0.0;
                    starts.clear();
                    if retry_at.is_none() {
                        retry_at = Some(now + RETRY);
                    }
                }
            }
            let shown = (st.open, st.error.clone());
            if shown != last_shown {
                last_shown = shown;
                drop(st);
                link.wake();
            }
        }
        // Steady pacing: the next frame a period after the last one was due, unless that fell
        // behind by a whole period (a slow write, a pause), then from now.
        next += period;
        let now = Instant::now();
        if next + period < now {
            next = now;
        }
        while !link.stop.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= next {
                break;
            }
            std::thread::sleep((next - now).min(NAP));
        }
    }
}

fn push<T>(q: &mut VecDeque<T>, v: T, cap: usize) {
    q.push_back(v);
    while q.len() > cap {
        q.pop_front();
    }
}

fn open_socket(target: &Target) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("cannot open a socket: {e}"))?;
    s.set_nonblocking(true).map_err(|e| format!("cannot open a socket: {e}"))?;
    if let Target::ArtNet { to: SocketAddr::V4(a) } = target
        && (a.ip().is_broadcast() || a.ip().octets().last() == Some(&255))
    {
        s.set_broadcast(true).map_err(|e| format!("cannot send broadcasts: {e}"))?;
    }
    Ok(s)
}

/// Sends one frame's packets; the address sent to, for the status.
fn send_packets(
    s: &UdpSocket,
    target: &Target,
    cid: &[u8; 16],
    sequence: u8,
    frame: &Frame,
    blank: &dyn Fn(&Vec<u8>) -> Vec<u8>,
) -> (Result<(), String>, Option<String>) {
    let mut first_error = None;
    let mut send = |p: &[u8], to: SocketAddr| {
        if let Err(e) = s.send_to(p, to)
            && first_error.is_none()
        {
            first_error = Some(format!("sending to {to}: {e}"));
        }
    };
    let device = match target {
        Target::ArtNet { to } => {
            for (universe, data) in &frame.parts {
                send(&artnet::dmx_packet(*universe, sequence.max(1), &blank(data)), *to);
            }
            Some(to.to_string())
        }
        Target::Sacn { to } => {
            for (&universe, data) in &frame.parts {
                let addr =
                    to.unwrap_or_else(|| SocketAddr::new(IpAddr::V4(sacn::multicast(universe)), sacn::PORT));
                let p = sacn::data_packet(
                    cid,
                    "penumbra",
                    sacn::DEFAULT_PRIORITY,
                    universe,
                    sequence,
                    &blank(data),
                );
                send(&p, addr);
            }
            Some(to.map_or_else(|| "multicast".to_string(), |a| a.to_string()))
        }
        Target::Ddp { to } => {
            let data = frame.parts.get(&0).map(blank).unwrap_or_default();
            for p in ddp::packets((sequence % 15) + 1, &data) {
                send(&p, *to);
            }
            Some(to.to_string())
        }
        Target::OpenDmx { .. } => None,
    };
    (first_error.map_or(Ok(()), Err), device)
}

fn describe(target: &Target) -> String {
    match target {
        Target::OpenDmx { port } if port.is_empty() => "open-dmx auto".to_string(),
        Target::OpenDmx { port } => format!("open-dmx {port}"),
        Target::ArtNet { to } => format!("art-net {to}"),
        Target::Sacn { to: Some(a) } => format!("sacn {a}"),
        Target::Sacn { to: None } => "sacn multicast".to_string(),
        Target::Ddp { to } => format!("ddp {to}"),
    }
}

/// A source id that differs between runs and machines, from the clock, the process id and the
/// address of a stack value; a receiver only needs it unique among the sources it sees.
fn source_id() -> [u8; 16] {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let here = 0u8;
    // `as`: only the bits matter.
    let mut x = nanos ^ (u128::from(std::process::id()) << 64) ^ ((&raw const here as usize as u128) << 32);
    let mut id = [0u8; 16];
    for b in &mut id {
        // xorshift, so neighbouring runs do not share most bytes.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 64) as u8;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(start: usize, data: &[u8], order: u64) -> Slice {
        Slice { label: format!("w{order}"), start, data: data.to_vec(), rate: 30.0, order }
    }

    #[test]
    fn writers_merge_by_range_the_later_wins() {
        let mut w = BTreeMap::new();
        w.insert(1, slice(0, &[1, 1, 1], 0));
        w.insert(2, slice(2, &[2, 2], 5));
        let f = compose(&w, Layout::Single);
        let u = &f.parts[&0];
        assert_eq!(u.len(), SLOTS);
        assert_eq!(&u[..5], &[1, 1, 2, 2, 0]);
        assert_eq!(f.overlap, 1);
        assert_eq!(f.writers, vec!["w0", "w5"]);
    }

    #[test]
    fn universes_spill_and_limits_drop() {
        let mut w = BTreeMap::new();
        // Universe 3, address 511: two slots there, three in universe 4.
        w.insert(1, slice(3 * SLOTS + 510, &[9; 5], 0));
        let f = compose(&w, Layout::Universes(artnet::MAX_UNIVERSE));
        assert_eq!(f.parts.keys().copied().collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(&f.parts[&3][510..], &[9, 9]);
        assert_eq!(&f.parts[&4][..4], &[9, 9, 9, 0]);
        let f = compose(&w, Layout::Single);
        assert_eq!(f.dropped, 5);
        let mut w = BTreeMap::new();
        w.insert(1, slice(usize::from(sacn::MAX_UNIVERSE) * SLOTS, &[1; 600], 0));
        let f = compose(&w, Layout::Universes(sacn::MAX_UNIVERSE));
        assert_eq!((f.parts.len(), f.dropped), (1, 88));
        let f = compose(&w, Layout::Flat);
        assert_eq!(f.dropped, 600);
    }

    fn receiver() -> (UdpSocket, SocketAddr) {
        let r = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        r.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let a = r.local_addr().unwrap();
        (r, a)
    }

    fn nothing() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    #[test]
    fn art_net_frames_keep_coming_and_stop_with_the_last_output() {
        let hub = Hub::new();
        let (r, to) = receiver();
        let a = hub.output("/a", Target::ArtNet { to }, nothing()).unwrap();
        let b = hub.output("/b", Target::ArtNet { to }, nothing()).unwrap();
        a.set(SLOTS, &[10, 20], 100.0);
        b.set(SLOTS + 2, &[30], 50.0);
        let mut buf = [0u8; 1024];
        let mut got = None;
        let start = Instant::now();
        let mut packets = 0;
        // The same frame over and over, nothing changed: at 100 Hz, 30 in 300 ms. A busy machine
        // wakes the sender late, so only that they keep coming is checked, not how evenly.
        while (packets < 30 || got.is_none()) && start.elapsed() < Duration::from_secs(5) {
            let n = r.recv(&mut buf).unwrap();
            packets += 1;
            let p = &buf[..n];
            if p[14..16] == [1, 0] && p[18..21] == [10, 20, 30] {
                got = Some(p.len());
            }
        }
        assert_eq!(got, Some(artnet::HEADER + SLOTS));
        assert!(packets >= 30, "{packets} packets in {:?} at 100 Hz", start.elapsed());
        let st = &hub.status()[0];
        assert_eq!(st.writers, vec!["/a", "/b"]);
        assert_eq!(st.universes, vec![1]);
        assert!(st.open && st.rate == 100.0, "{st:?}");
        drop(a);
        assert_eq!(hub.status().len(), 1);
        drop(b);
        assert!(hub.status().is_empty());
        std::thread::sleep(Duration::from_millis(100));
        r.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        while r.recv(&mut buf).is_ok() {}
        r.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
        assert!(r.recv(&mut buf).is_err(), "a stopped link still sends");
        hub.shutdown();
    }

    #[test]
    fn blackout_and_switching_off() {
        let hub = Hub::new();
        let (r, to) = receiver();
        let out = hub.output("/x", Target::Ddp { to }, nothing()).unwrap();
        out.set(0, &[200; 3000], 60.0);
        let mut buf = [0u8; 2048];
        let n = r.recv(&mut buf).unwrap();
        assert_eq!(buf[ddp::HEADER], 200);
        assert_eq!(n, ddp::HEADER + ddp::CHUNK);
        hub.set_config(Config { enabled: true, blackout: true });
        let start = Instant::now();
        loop {
            let n = r.recv(&mut buf).unwrap();
            if n > ddp::HEADER && buf[ddp::HEADER] == 0 {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(1), "no blackout");
        }
        hub.set_config(Config { enabled: false, blackout: false });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!out.status().open);
        assert!(out.status().error.unwrap().contains("switched off"));
        hub.shutdown();
    }

    #[test]
    fn a_missing_port_is_an_error_not_a_crash_and_wakes_the_node() {
        let hub = Hub::new();
        let woken = Arc::new(AtomicU64::new(0));
        let w = woken.clone();
        let out = hub
            .output(
                "/dmx",
                Target::OpenDmx { port: "COM_DOES_NOT_EXIST".to_string() },
                Arc::new(move || {
                    w.fetch_add(1, Ordering::Relaxed);
                }),
            )
            .unwrap();
        out.set(0, &[255; 512], 30.0);
        let start = Instant::now();
        while out.status().error.is_none() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let st = out.status();
        assert!(!st.open);
        assert!(st.error.as_deref().unwrap_or("").contains("COM_DOES_NOT_EXIST"), "{st:?}");
        assert_eq!(st.failures, 1, "retried once a second, counted once");
        assert!(woken.load(Ordering::Relaxed) >= 1);
        drop(out);
        hub.shutdown();
    }

    /// Needs an Open DMX USB plugged in (nothing need be connected to it): `cargo test -p
    /// penumbra-dmx -- --ignored open_dmx`.
    #[test]
    #[ignore]
    fn open_dmx_usb_holds_its_rate() {
        let hub = Hub::new();
        let out = hub.output("/dmx", Target::OpenDmx { port: String::new() }, nothing()).unwrap();
        out.set(0, &[128; 512], 30.0);
        std::thread::sleep(Duration::from_secs(3));
        let st = out.status();
        println!("{st:#?}");
        assert!(st.open, "{st:?}");
        assert!((st.measured - 30.0).abs() < 1.0, "{st:?}");
        assert!(st.period.max < 40.0, "{st:?}");
        drop(out);
        hub.shutdown();
    }
}
