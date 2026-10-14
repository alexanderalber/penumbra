//! The OSC hub: one UDP listener per port, what arrived on it, and the hand-over to whoever
//! uses it.
//!
//! - **Listeners** are wanted by the network ([`Hub::set_wanted`]: one [`Want`] per `chan.oscin`
//!   node); the hub opens one thread per port, binds `127.0.0.1` unless a want asks for the LAN
//!   (`0.0.0.0`), rebinds when that changes and retries a taken port every second. Port 0 binds
//!   any free port ([`ListenerStatus::bound`] says which).
//! - **What arrives** is parsed with [`crate::codec::parse`] and kept per port as the latest
//!   numbers per address ([`Store`], at most [`Config::max_addresses`] addresses, in the order
//!   they first came). Readers ([`Hub::read`]) get a snapshot; [`Hub::watch`] calls back when the
//!   store changed, at most once until the watcher acknowledges.
//! - **Params:** on a port whose want has `params`, every message is also queued for the
//!   engine, the latest per address ([`Hub::take_params`]), which turns addresses into param
//!   edits.
//! - **Learn:** while armed ([`Hub::learn`]), the next message with a number (and a plain
//!   address) on any port is kept for whoever armed it ([`Hub::take_heard`]). Addresses that
//!   were busy when learn was armed (a message in the last [`BUSY`]: an accelerometer, a
//!   feedback loop, a sender streaming all the time) are passed over, so learn catches the
//!   control the user moves and not the one that never stops.
//! - **Hostile input:** datagrams over [`Config::max_packet`] are dropped, malformed ones are
//!   counted and dropped, every source (address and port) has a token bucket
//!   ([`Config::rate`], [`Config::burst`]) and so do all sources together (ten times that);
//!   messages with patterns in their address cost more to match and have a bucket of their own
//!   ([`Config::pattern_rate`]) plus a step budget each. At most [`MAX_SOURCES`] sources are
//!   tracked; the one heard from least recently makes room.
//! - **Timetags:** messages in a bundle with a future timetag are applied at once like the
//!   rest and counted ([`ListenerStatus::future`]); see the plan for why.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

use crate::codec::{self, Arg, Limits, Message};
use crate::pattern::{Budget, Pattern, is_pattern};

/// Sources tracked for rate limits per listener.
pub const MAX_SOURCES: usize = 256;
/// Messages kept for [`Status::recent`].
pub const RECENT: usize = 32;
/// Numbers kept per address.
pub const MAX_VALUES: usize = 64;
/// Steps one message with a pattern address may take to match.
pub const PATTERN_STEPS: u64 = 200_000;
/// Distinct addresses waiting for the engine ([`Hub::take_params`]).
pub const MAX_QUEUED: usize = 1024;
/// An address heard this recently when learn is armed is passed over by learn.
pub const BUSY: Duration = Duration::from_millis(1500);
/// How long a listener thread blocks in one receive (and so how fast it notices a stop).
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
/// How often a listener retries a port it could not bind.
const RETRY: Duration = Duration::from_secs(1);

/// Settings for all listeners. Not stored: every start has the defaults (`osc.config`).
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Off closes every listener (the wants are kept for when it is on again).
    pub enabled: bool,
    /// Larger datagrams are dropped unread.
    pub max_packet: usize,
    /// Packets per second per source, on average.
    pub rate: f64,
    /// Packets a source may send at once before the rate applies.
    pub burst: f64,
    /// Messages per second with pattern characters in their address, all sources together.
    pub pattern_rate: f64,
    /// Addresses kept per port; new ones beyond are dropped.
    pub max_addresses: usize,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_packet: 16_384,
            rate: 2000.0,
            burst: 2000.0,
            pattern_rate: 100.0,
            max_addresses: 1024,
            limits: Limits::default(),
        }
    }
}

impl Config {
    /// Brings every number into a sane range.
    pub fn sanitized(mut self) -> Self {
        let finite = |x: f64, d: f64| if x.is_finite() { x } else { d };
        self.max_packet = self.max_packet.clamp(64, codec::MAX_PACKET);
        self.rate = finite(self.rate, 2000.0).clamp(1.0, 1_000_000.0);
        self.burst = finite(self.burst, 2000.0).clamp(1.0, 1_000_000.0);
        self.pattern_rate = finite(self.pattern_rate, 100.0).clamp(0.0, 100_000.0);
        self.max_addresses = self.max_addresses.clamp(1, 65_536);
        self.limits.max_messages = self.limits.max_messages.clamp(1, 4096);
        self.limits.max_args = self.limits.max_args.clamp(1, 4096);
        self.limits.max_depth = self.limits.max_depth.clamp(1, 32);
        self.limits.max_address = self.limits.max_address.clamp(2, 4096);
        self
    }
}

/// A listener as the network wants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Want {
    pub port: u16,
    /// Listen on every interface, not only this machine.
    pub lan: bool,
    /// Queue messages for param edits ([`Hub::take_params`]).
    pub params: bool,
}

/// The latest numbers of one address.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub address: String,
    /// The numeric args in order (strings and blobs left out), at most [`MAX_VALUES`].
    pub values: Vec<f64>,
    /// The first string arg, if any.
    pub text: Option<String>,
    /// Messages received for it.
    pub count: u64,
    #[serde(skip)]
    pub at: Option<Instant>,
}

/// What a port received: the latest per address, in the order the addresses first came.
#[derive(Clone, Debug, Default)]
pub struct Store {
    /// Changes with every message taken in.
    pub version: u64,
    pub entries: Vec<Entry>,
    index: BTreeMap<String, usize>,
}

impl Store {
    fn set(&mut self, address: &str, args: &[Arg], max: usize) -> bool {
        let values: Vec<f64> = args.iter().filter_map(Arg::as_f64).take(MAX_VALUES).collect();
        let text = args.iter().find_map(|a| a.as_str().map(str::to_string));
        let i = match self.index.get(address) {
            Some(i) => *i,
            None => {
                if self.entries.len() >= max {
                    return false;
                }
                self.index.insert(address.to_string(), self.entries.len());
                self.entries.push(Entry {
                    address: address.to_string(),
                    values: Vec::new(),
                    text: None,
                    count: 0,
                    at: None,
                });
                self.entries.len() - 1
            }
        };
        if let Some(e) = self.entries.get_mut(i) {
            e.values = values;
            e.text = text;
            e.count += 1;
            e.at = Some(Instant::now());
        }
        self.version += 1;
        true
    }

    /// Sets every known address `pattern` matches; how many.
    fn set_matching(&mut self, pattern: &Pattern, args: &[Arg], budget: &mut Budget) -> usize {
        let hits: Vec<String> = self
            .entries
            .iter()
            .filter(|e| pattern.matches(&e.address, budget))
            .map(|e| e.address.clone())
            .collect();
        for a in &hits {
            self.set(a, args, usize::MAX);
        }
        hits.len()
    }
}

/// A message waiting to become a param edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    pub port: u16,
    pub address: String,
    pub args: Vec<Arg>,
    pub from: SocketAddr,
}

/// What learn heard.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Heard {
    pub port: u16,
    pub address: String,
    /// Numeric args.
    pub values: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceStatus {
    pub from: String,
    pub packets: u64,
    /// Packets over the source's rate.
    pub dropped: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ListenerStatus {
    pub port: u16,
    /// The address actually bound, `None` while not bound.
    pub bound: Option<String>,
    pub lan: bool,
    pub params: bool,
    /// Why binding failed (retried every second).
    pub error: Option<String>,
    pub packets: u64,
    pub messages: u64,
    pub bytes: u64,
    pub bundles: u64,
    /// Packets that did not parse, with the last reason.
    pub malformed: u64,
    pub last_malformed: Option<String>,
    /// Datagrams over the size limit.
    pub oversize: u64,
    /// Packets over a source's rate or over the rate of all sources.
    pub rate_dropped: u64,
    /// Pattern messages over their rate or their step budget.
    pub pattern_dropped: u64,
    /// New addresses dropped because the store was full.
    pub store_full: u64,
    /// Messages in bundles timed in the future (applied at once all the same).
    pub future: u64,
    pub addresses: usize,
    pub sources: Vec<SourceStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Recent {
    pub port: u16,
    pub from: String,
    pub address: String,
    pub args: String,
    /// Seconds ago.
    pub age: f64,
    #[serde(skip)]
    at: Option<Instant>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SendStatus {
    pub label: String,
    pub target: String,
    pub packets: u64,
    pub messages: u64,
    pub bytes: u64,
    pub errors: u64,
    pub last_error: Option<String>,
}

/// `osc.status`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub listeners: Vec<ListenerStatus>,
    pub senders: Vec<SendStatus>,
    pub recent: Vec<Recent>,
    pub learning: bool,
    pub queued: usize,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(tokens: f64, now: Instant) -> Self {
        Self { tokens, last: now }
    }

    fn take(&mut self, now: Instant, rate: f64, burst: f64) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * rate).min(burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Source {
    bucket: Bucket,
    packets: u64,
    dropped: u64,
}

/// A watcher: port, id, "called and not yet acknowledged", the callback.
type WatchEntry = (u16, u64, Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);
/// A listener and its thread.
type Running = (Arc<Listener>, Option<JoinHandle<()>>);

/// One port.
struct Listener {
    want: Mutex<Want>,
    stop: AtomicBool,
    store: Mutex<Store>,
    status: Mutex<ListenerStatus>,
    sources: Mutex<BTreeMap<SocketAddr, Source>>,
}

struct Inner {
    config: Mutex<Config>,
    wanted: Mutex<Vec<Want>>,
    listeners: Mutex<BTreeMap<u16, Running>>,
    /// Watchers by port, also of ports nothing listens on yet.
    watches: Mutex<Vec<WatchEntry>>,
    queue: Mutex<BTreeMap<(u16, String), Incoming>>,
    queued: Condvar,
    learning: AtomicBool,
    heard: Mutex<Option<Heard>>,
    /// Addresses busy when learn was armed, by port.
    busy: Mutex<std::collections::BTreeSet<(u16, String)>>,
    recent: Mutex<VecDeque<Recent>>,
    senders: Mutex<BTreeMap<u64, Arc<Mutex<SendStatus>>>>,
    next_id: AtomicU64,
    /// All sources together, per port.
    global: Mutex<BTreeMap<u16, Bucket>>,
    pattern_bucket: Mutex<Bucket>,
}

/// The OSC hub, shared by the engine, the nodes and the commands (clones share it).
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
    port: u16,
    id: u64,
    pending: Arc<AtomicBool>,
    hub: Weak<Inner>,
}

impl Watch {
    /// Allows the next callback (call before reading).
    pub fn ack(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.watches).retain(|(_, id, _, _)| *id != self.id);
        }
    }
}

impl Hub {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            inner: Arc::new(Inner {
                config: Mutex::new(Config::default()),
                wanted: Mutex::new(Vec::new()),
                listeners: Mutex::new(BTreeMap::new()),
                watches: Mutex::new(Vec::new()),
                queue: Mutex::new(BTreeMap::new()),
                queued: Condvar::new(),
                learning: AtomicBool::new(false),
                heard: Mutex::new(None),
                busy: Mutex::new(std::collections::BTreeSet::new()),
                recent: Mutex::new(VecDeque::new()),
                senders: Mutex::new(BTreeMap::new()),
                next_id: AtomicU64::new(1),
                global: Mutex::new(BTreeMap::new()),
                pattern_bucket: Mutex::new(Bucket::new(0.0, now)),
            }),
        }
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// Changes the settings; turning it off closes every listener, on opens what is wanted.
    pub fn set_config(&self, config: Config) {
        *lock(&self.inner.config) = config.sanitized();
        self.apply();
    }

    /// The listeners the network wants. Wants on the same port merge: the LAN if any asks for
    /// it, params if any does.
    pub fn set_wanted(&self, wants: &[Want]) {
        let mut merged: BTreeMap<u16, Want> = BTreeMap::new();
        for w in wants {
            merged
                .entry(w.port)
                .and_modify(|m| {
                    m.lan |= w.lan;
                    m.params |= w.params;
                })
                .or_insert(*w);
        }
        let merged: Vec<Want> = merged.into_values().collect();
        {
            let mut wanted = lock(&self.inner.wanted);
            if *wanted == merged {
                return;
            }
            *wanted = merged;
        }
        self.apply();
    }

    /// The wants after merging.
    pub fn wanted(&self) -> Vec<Want> {
        lock(&self.inner.wanted).clone()
    }

    fn apply(&self) {
        let wanted = if self.config().enabled { self.wanted() } else { Vec::new() };
        let mut listeners = lock(&self.inner.listeners);
        let keep: BTreeMap<u16, Want> = wanted.iter().map(|w| (w.port, *w)).collect();
        // Close what is no longer wanted.
        let gone: Vec<u16> = listeners.keys().filter(|p| !keep.contains_key(p)).copied().collect();
        let mut touched: Vec<u16> = Vec::new();
        for port in gone {
            touched.push(port);
            if let Some((l, _thread)) = listeners.remove(&port) {
                // The thread ends within a receive timeout; it is not joined here, so the cook
                // thread never waits for it.
                l.stop.store(true, Ordering::Release);
            }
        }
        for (port, want) in keep {
            match listeners.get(&port) {
                Some((l, _)) => *lock(&l.want) = want,
                None => {
                    touched.push(port);
                    let l = Arc::new(Listener {
                        want: Mutex::new(want),
                        stop: AtomicBool::new(false),
                        store: Mutex::new(Store::default()),
                        status: Mutex::new(ListenerStatus { port, ..ListenerStatus::default() }),
                        sources: Mutex::new(BTreeMap::new()),
                    });
                    let (inner, listener) = (Arc::downgrade(&self.inner), l.clone());
                    let thread = std::thread::Builder::new()
                        .name(format!("osc-in-{port}"))
                        .spawn(move || listen(&inner, &listener));
                    let thread = match thread {
                        Ok(t) => Some(t),
                        Err(e) => {
                            lock(&l.status).error = Some(format!("cannot start a thread: {e}"));
                            None
                        }
                    };
                    listeners.insert(port, (l, thread));
                }
            }
        }
        drop(listeners);
        for port in touched {
            wake(&self.inner, port);
        }
    }

    /// What port `port` (as the want named it) received; `None` when nothing listens there.
    pub fn read(&self, port: u16) -> Option<Store> {
        let listeners = lock(&self.inner.listeners);
        let (l, _) = listeners.get(&port)?;
        Some(lock(&l.store).clone())
    }

    /// The version of `port`'s store, cheaper than [`Hub::read`].
    pub fn version(&self, port: u16) -> Option<u64> {
        let listeners = lock(&self.inner.listeners);
        let (l, _) = listeners.get(&port)?;
        Some(lock(&l.store).version)
    }

    /// The listener's status, `None` when nothing listens on `port`.
    pub fn listener(&self, port: u16) -> Option<ListenerStatus> {
        let listeners = lock(&self.inner.listeners);
        let (l, _) = listeners.get(&port)?;
        let mut s = lock(&l.status).clone();
        s.addresses = lock(&l.store).entries.len();
        Some(s)
    }

    /// Calls `wake` when `port`'s store changes, its listener opens, closes, binds or fails, at
    /// most once until [`Watch::ack`]. Works before anything listens on `port`.
    pub fn watch(&self, port: u16, wake: Arc<dyn Fn() + Send + Sync>) -> Watch {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(AtomicBool::new(false));
        lock(&self.inner.watches).push((port, id, pending.clone(), wake));
        Watch { port, id, pending, hub: Arc::downgrade(&self.inner) }
    }

    /// Messages waiting to become param edits, the latest per address, oldest port first.
    pub fn take_params(&self) -> Vec<Incoming> {
        std::mem::take(&mut *lock(&self.inner.queue)).into_values().collect()
    }

    /// Waits up to `timeout` for a message for the params (or learn); whether one is there.
    pub fn wait_params(&self, timeout: Duration) -> bool {
        let q = lock(&self.inner.queue);
        let (q, _) = self
            .inner
            .queued
            .wait_timeout_while(q, timeout, |q| q.is_empty() && lock(&self.inner.heard).is_none())
            .unwrap_or_else(PoisonError::into_inner);
        !q.is_empty() || lock(&self.inner.heard).is_some()
    }

    /// Wakes [`Hub::wait_params`] (for stopping).
    pub fn notify(&self) {
        self.inner.queued.notify_all();
    }

    /// Arms (or disarms) learn: the next message with a number on a plain address is kept.
    pub fn learn(&self, on: bool) {
        *lock(&self.inner.heard) = None;
        let mut busy = std::collections::BTreeSet::new();
        if on {
            let now = Instant::now();
            for (port, (l, _)) in lock(&self.inner.listeners).iter() {
                for e in &lock(&l.store).entries {
                    if e.at.is_some_and(|at| now.saturating_duration_since(at) < BUSY) {
                        busy.insert((*port, e.address.clone()));
                    }
                }
            }
        }
        *lock(&self.inner.busy) = busy;
        self.inner.learning.store(on, Ordering::Release);
    }

    pub fn learning(&self) -> bool {
        self.inner.learning.load(Ordering::Acquire)
    }

    /// What learn heard, once.
    pub fn take_heard(&self) -> Option<Heard> {
        lock(&self.inner.heard).take()
    }

    /// Opens a socket for sending to `target`, counted in [`Status::senders`] under `label`
    /// while the [`Sender`] lives.
    pub fn sender(&self, label: &str, target: SocketAddr) -> std::io::Result<Sender> {
        let local: SocketAddr = match target.ip() {
            IpAddr::V4(ip) if ip.is_loopback() => (Ipv4Addr::LOCALHOST, 0).into(),
            IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(ip) if ip.is_loopback() => (std::net::Ipv6Addr::LOCALHOST, 0).into(),
            IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(local)?;
        socket.set_nonblocking(true)?;
        if let IpAddr::V4(ip) = target.ip()
            && (ip.is_broadcast() || ip.octets().last() == Some(&255))
        {
            socket.set_broadcast(true)?;
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let stats = Arc::new(Mutex::new(SendStatus {
            label: label.to_string(),
            target: target.to_string(),
            ..SendStatus::default()
        }));
        lock(&self.inner.senders).insert(id, stats.clone());
        Ok(Sender { socket, target, id, stats, hub: Arc::downgrade(&self.inner) })
    }

    /// `osc.status`.
    pub fn status(&self) -> Status {
        let listeners: Vec<ListenerStatus> = {
            let ls = lock(&self.inner.listeners);
            ls.values()
                .map(|(l, _)| {
                    let mut s = lock(&l.status).clone();
                    s.addresses = lock(&l.store).entries.len();
                    s.sources = lock(&l.sources)
                        .iter()
                        .map(|(a, src)| SourceStatus {
                            from: a.to_string(),
                            packets: src.packets,
                            dropped: src.dropped,
                        })
                        .collect();
                    s
                })
                .collect()
        };
        let now = Instant::now();
        let recent = lock(&self.inner.recent)
            .iter()
            .rev()
            .map(|r| {
                let mut r = r.clone();
                r.age = r.at.map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f64());
                r
            })
            .collect();
        Status {
            enabled: self.config().enabled,
            listeners,
            senders: lock(&self.inner.senders).values().map(|s| lock(s).clone()).collect(),
            recent,
            learning: self.learning(),
            queued: lock(&self.inner.queue).len(),
        }
    }

    /// Closes every listener (at shutdown).
    pub fn shutdown(&self) {
        let all = std::mem::take(&mut *lock(&self.inner.listeners));
        for (l, thread) in all.into_values() {
            l.stop.store(true, Ordering::Release);
            if let Some(t) = thread
                && t.join().is_err()
            {
                log::error!("an OSC listener thread panicked");
            }
        }
        self.notify();
    }
}

/// A socket sending to one target ([`Hub::sender`]).
pub struct Sender {
    socket: UdpSocket,
    target: SocketAddr,
    id: u64,
    stats: Arc<Mutex<SendStatus>>,
    hub: Weak<Inner>,
}

impl Sender {
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Sends one packet without blocking; a full send buffer or an unreachable host is an
    /// error, counted.
    pub fn send(&self, packet: &[u8], messages: usize) -> std::io::Result<()> {
        let r = self.socket.send_to(packet, self.target).map(|_| ());
        let mut s = lock(&self.stats);
        match &r {
            Ok(()) => {
                s.packets += 1;
                s.messages += messages as u64;
                s.bytes += packet.len() as u64;
            }
            Err(e) => {
                s.errors += 1;
                s.last_error = Some(e.to_string());
            }
        }
        r
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.senders).remove(&self.id);
        }
    }
}

/// `host` as an address to send to: an IP address or `localhost`. Names are not looked up, so
/// nothing ever waits for DNS.
pub fn target(host: &str, port: u16) -> Result<SocketAddr, String> {
    let host = host.trim();
    let ip: IpAddr = if host.eq_ignore_ascii_case("localhost") {
        Ipv4Addr::LOCALHOST.into()
    } else {
        host.parse().map_err(|_| format!("`{host}` is not an IP address (names are not looked up)"))?
    };
    if port == 0 {
        return Err("port 0 cannot be sent to".into());
    }
    Ok(SocketAddr::new(ip, port))
}

/// This machine's address on the network that reaches beyond it (for setting up a phone), found
/// without sending anything (connecting a UDP socket only picks a route).
pub fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

// ---------------------------------------------------------------------------------------------
// The listener thread

fn bind(want: Want) -> std::io::Result<UdpSocket> {
    let ip = if want.lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
    let s = UdpSocket::bind((ip, want.port))?;
    s.set_read_timeout(Some(RECV_TIMEOUT))?;
    Ok(s)
}

fn listen(inner: &Weak<Inner>, l: &Arc<Listener>) {
    let mut socket: Option<(UdpSocket, bool)> = None;
    let mut retry_at: Option<Instant> = None;
    let mut buf = vec![0u8; 65_536];
    // Ends when told to, and when the hub itself is gone (a cooker dropped without shutdown).
    while !l.stop.load(Ordering::Acquire) && inner.strong_count() > 0 {
        let want = *lock(&l.want);
        {
            let mut s = lock(&l.status);
            s.lan = want.lan;
            s.params = want.params;
        }
        if socket.as_ref().is_some_and(|(_, lan)| *lan != want.lan) {
            socket = None;
        }
        let Some((sock, _)) = &socket else {
            if retry_at.is_some_and(|at| Instant::now() < at) {
                std::thread::sleep(RECV_TIMEOUT);
                continue;
            }
            match bind(want) {
                Ok(s) => {
                    let mut st = lock(&l.status);
                    st.bound = s.local_addr().ok().map(|a| a.to_string());
                    st.error = None;
                    log::info!("OSC: listening on {}", st.bound.as_deref().unwrap_or("?"));
                    drop(st);
                    socket = Some((s, want.lan));
                    retry_at = None;
                    if let Some(inner) = inner.upgrade() {
                        wake(&inner, want.port);
                    }
                }
                Err(e) => {
                    let mut st = lock(&l.status);
                    let msg = format!("cannot listen on port {}: {e}", want.port);
                    if st.error.as_deref() != Some(msg.as_str()) {
                        log::warn!("OSC: {msg}");
                    }
                    let first = st.error.is_none();
                    st.bound = None;
                    st.error = Some(msg);
                    drop(st);
                    retry_at = Some(Instant::now() + RETRY);
                    if first && let Some(inner) = inner.upgrade() {
                        wake(&inner, want.port);
                    }
                }
            }
            continue;
        };
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let Some(inner) = inner.upgrade() else { return };
                let data = buf.get(..n).unwrap_or(&[]);
                if catch(|| receive(&inner, l, want, data, from)).is_err() {
                    log::error!("OSC: handling a packet panicked; dropped");
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => {
                // A datagram larger than the buffer (WSAEMSGSIZE on Windows) or an ICMP error
                // reported on the socket: count it and go on.
                let mut st = lock(&l.status);
                if e.raw_os_error() == Some(10040) {
                    st.oversize += 1;
                } else {
                    st.malformed += 1;
                    st.last_malformed = Some(e.to_string());
                }
            }
        }
    }
    let port = lock(&l.status).port;
    log::info!("OSC: stopped listening on port {port}");
}

fn catch(f: impl FnOnce()) -> std::thread::Result<()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
}

/// Takes one datagram in.
fn receive(inner: &Inner, l: &Listener, want: Want, data: &[u8], from: SocketAddr) {
    let config = lock(&inner.config).clone();
    let now = Instant::now();
    if data.len() > config.max_packet {
        lock(&l.status).oversize += 1;
        return;
    }
    if !admit(inner, l, &config, want.port, from, now) {
        lock(&l.status).rate_dropped += 1;
        return;
    }
    let packet = match codec::parse(data, &config.limits) {
        Ok(p) => p,
        Err(e) => {
            let mut st = lock(&l.status);
            st.malformed += 1;
            st.last_malformed = Some(format!("from {from}: {e}"));
            return;
        }
    };
    let ntp = codec::ntp_now();
    let mut future = 0u64;
    let mut pattern_dropped = 0u64;
    let mut changed = false;
    let mut full = 0u64;
    let mut queue: Vec<Incoming> = Vec::new();
    for (time, msg) in &packet.messages {
        if codec::is_future(*time, ntp, 10) {
            future += 1;
        }
        let pattern = is_pattern(&msg.address);
        if pattern {
            let ok = lock(&inner.pattern_bucket).take(now, config.pattern_rate, config.pattern_rate.max(1.0));
            let compiled = Pattern::compile(&msg.address);
            match (ok, compiled) {
                (true, Ok(p)) => {
                    let mut budget = Budget(PATTERN_STEPS);
                    let hits = lock(&l.store).set_matching(&p, &msg.args, &mut budget);
                    if budget.exhausted() {
                        pattern_dropped += 1;
                    }
                    changed |= hits > 0;
                }
                _ => {
                    pattern_dropped += 1;
                    continue;
                }
            }
        } else if lock(&l.store).set(&msg.address, &msg.args, config.max_addresses) {
            changed = true;
        } else {
            full += 1;
        }
        if want.params {
            queue.push(Incoming {
                port: want.port,
                address: msg.address.clone(),
                args: msg.args.clone(),
                from,
            });
        }
        if !pattern
            && inner.learning.load(Ordering::Acquire)
            && msg.args.iter().any(|a| a.as_f64().is_some())
            && !lock(&inner.busy).contains(&(want.port, msg.address.clone()))
        {
            let mut heard = lock(&inner.heard);
            if heard.is_none() && inner.learning.swap(false, Ordering::AcqRel) {
                *heard = Some(Heard {
                    port: want.port,
                    address: msg.address.clone(),
                    values: msg.args.iter().filter(|a| a.as_f64().is_some()).count(),
                });
                inner.queued.notify_all();
            }
        }
        remember(inner, want.port, from, msg, now);
    }
    {
        let mut st = lock(&l.status);
        st.packets += 1;
        st.messages += packet.messages.len() as u64;
        st.bytes += data.len() as u64;
        st.bundles += packet.bundles as u64;
        st.future += future;
        st.pattern_dropped += pattern_dropped;
        st.store_full += full;
    }
    if !queue.is_empty() {
        let mut q = lock(&inner.queue);
        for m in queue {
            let key = (m.port, m.address.clone());
            if q.len() < MAX_QUEUED || q.contains_key(&key) {
                q.insert(key, m);
            }
        }
        drop(q);
        inner.queued.notify_all();
    }
    if changed {
        wake(inner, want.port);
    }
}

/// Calls the watchers of `port`.
fn wake(inner: &Inner, port: u16) {
    for (p, _, pending, wake) in lock(&inner.watches).iter() {
        if *p == port && !pending.swap(true, Ordering::AcqRel) {
            wake();
        }
    }
}

/// The token buckets of the source and of all sources on this port.
fn admit(inner: &Inner, l: &Listener, config: &Config, port: u16, from: SocketAddr, now: Instant) -> bool {
    let global_ok = lock(&inner.global)
        .entry(port)
        .or_insert_with(|| Bucket::new(config.burst * 10.0, now))
        .take(now, config.rate * 10.0, config.burst * 10.0);
    let mut sources = lock(&l.sources);
    if !sources.contains_key(&from) && sources.len() >= MAX_SOURCES {
        let oldest = sources.iter().min_by_key(|(_, s)| s.bucket.last).map(|(a, _)| *a);
        if let Some(a) = oldest {
            sources.remove(&a);
        }
    }
    let src = sources.entry(from).or_insert_with(|| Source {
        bucket: Bucket::new(config.burst, now),
        packets: 0,
        dropped: 0,
    });
    src.packets += 1;
    let ok = src.bucket.take(now, config.rate, config.burst) && global_ok;
    if !ok {
        src.dropped += 1;
    }
    ok
}

fn remember(inner: &Inner, port: u16, from: SocketAddr, msg: &Message, now: Instant) {
    let args: Vec<String> = msg.args.iter().take(8).map(ToString::to_string).collect();
    let mut args = args.join(" ");
    if msg.args.len() > 8 {
        args.push_str(" ...");
    }
    // The latest per address, so one address streaming does not push the others out.
    let mut recent = lock(&inner.recent);
    recent.retain(|r| !(r.port == port && r.address == msg.address));
    if recent.len() >= RECENT {
        recent.pop_front();
    }
    recent.push_back(Recent {
        port,
        from: from.to_string(),
        address: msg.address.clone(),
        args,
        age: 0.0,
        at: Some(now),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Message, message_packet, packets};

    fn hub_on_any_port(params: bool) -> (Hub, u16) {
        let hub = Hub::new();
        hub.set_wanted(&[Want { port: 0, lan: false, params }]);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(b) = hub.listener(0).and_then(|s| s.bound) {
                let port = b.rsplit(':').next().unwrap().parse().unwrap();
                return (hub, port);
            }
            assert!(Instant::now() < deadline, "listener did not bind");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn client() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").unwrap()
    }

    #[test]
    fn messages_and_bundles_arrive_in_the_store() {
        let (hub, port) = hub_on_any_port(false);
        let woke = Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        let watch = hub.watch(
            0,
            Arc::new(move || {
                w.fetch_add(1, Ordering::Relaxed);
            }),
        );
        let c = client();
        let to = ("127.0.0.1", port);
        c.send_to(&message_packet(&Message::new("/fader1", vec![Arg::Float(0.25)])), to).unwrap();
        let ms = vec![
            Message::new("/xy", vec![Arg::Float(0.1), Arg::Float(0.9)]),
            Message::new("/label", vec![Arg::Str("hi".into())]),
        ];
        c.send_to(&packets(&ms, 1400, true)[0], to).unwrap();
        until("three addresses", || hub.read(0).is_some_and(|s| s.entries.len() == 3));
        let s = hub.read(0).unwrap();
        assert_eq!(s.entries[0].address, "/fader1");
        assert_eq!(s.entries[0].values, vec![0.25]);
        assert_eq!(s.entries[1].values, vec![0.10000000149011612, 0.8999999761581421]);
        assert_eq!(s.entries[2].text.as_deref(), Some("hi"));
        assert_eq!(woke.load(Ordering::Relaxed), 1, "once until acknowledged");
        watch.ack();
        c.send_to(&message_packet(&Message::new("/fader1", vec![Arg::Float(0.5)])), to).unwrap();
        until("second wake", || woke.load(Ordering::Relaxed) == 2);
        // A pattern updates the known addresses it matches.
        c.send_to(&message_packet(&Message::new("/fader?", vec![Arg::Int(1)])), to).unwrap();
        until("pattern", || hub.read(0).unwrap().entries[0].values == vec![1.0]);
        let st = hub.listener(0).unwrap();
        assert_eq!((st.packets, st.messages, st.bundles), (4, 5, 1));
        hub.shutdown();
    }

    #[test]
    fn hostile_input_is_counted_and_dropped() {
        let (hub, port) = hub_on_any_port(false);
        hub.set_config(Config { max_packet: 256, rate: 1.0, burst: 5.0, ..Config::default() });
        let c = client();
        let to = ("127.0.0.1", port);
        c.send_to(b"garbage", to).unwrap();
        c.send_to(&[0u8; 1000], to).unwrap();
        for i in 0..20 {
            c.send_to(&message_packet(&Message::new(format!("/a{i}"), vec![Arg::Int(i)])), to).unwrap();
        }
        until("counted", || {
            let s = hub.listener(0).unwrap();
            s.malformed == 1 && s.oversize == 1 && s.rate_dropped >= 14
        });
        let s = hub.listener(0).unwrap();
        assert_eq!(s.addresses, 4, "the burst of five minus the garbage packet");
        assert!(s.last_malformed.unwrap().contains("from 127.0.0.1"));
        assert_eq!(hub.status().listeners[0].sources.len(), 1);
        hub.shutdown();
    }

    #[test]
    fn params_queue_keeps_the_latest_and_learn_hears_once() {
        let (hub, port) = hub_on_any_port(true);
        let c = client();
        let to = ("127.0.0.1", port);
        // An address streaming when learn is armed is passed over.
        c.send_to(&message_packet(&Message::new("/accel", vec![Arg::Float(0.0)])), to).unwrap();
        until("the stream", || hub.read(0).is_some_and(|s| s.entries.len() == 1));
        hub.learn(true);
        c.send_to(&message_packet(&Message::new("/accel", vec![Arg::Float(0.5)])), to).unwrap();
        for v in [0.1f32, 0.2, 0.3] {
            c.send_to(&message_packet(&Message::new("/n/scale", vec![Arg::Float(v)])), to).unwrap();
        }
        c.send_to(&message_packet(&Message::new("/other", vec![Arg::Str("x".into())])), to).unwrap();
        until("queued", || hub.listener(0).unwrap().messages == 6);
        assert!(hub.wait_params(Duration::from_millis(10)));
        let q = hub.take_params();
        assert_eq!(q.len(), 3);
        assert_eq!(q[1].address, "/n/scale");
        assert_eq!(q[1].args, vec![Arg::Float(0.3)]);
        assert_eq!(hub.take_heard(), Some(Heard { port: 0, address: "/n/scale".into(), values: 1 }));
        assert_eq!(hub.take_heard(), None);
        assert!(!hub.learning());
        hub.shutdown();
    }

    #[test]
    fn wants_merge_and_close() {
        let hub = Hub::new();
        hub.set_wanted(&[
            Want { port: 0, lan: false, params: false },
            Want { port: 0, lan: false, params: true },
        ]);
        assert_eq!(hub.wanted(), vec![Want { port: 0, lan: false, params: true }]);
        hub.set_config(Config { enabled: false, ..Config::default() });
        assert!(hub.listener(0).is_none());
        hub.set_config(Config::default());
        assert!(hub.listener(0).is_some());
        hub.set_wanted(&[]);
        assert!(hub.read(0).is_none());
        hub.shutdown();
    }

    #[test]
    fn a_taken_port_is_an_error_not_a_crash() {
        let taken = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let hub = Hub::new();
        hub.set_wanted(&[Want { port, lan: false, params: false }]);
        until("error", || hub.listener(port).unwrap().error.is_some());
        drop(taken);
        until("bound after the retry", || hub.listener(port).unwrap().bound.is_some());
        hub.shutdown();
    }

    #[test]
    fn senders_send_and_count() {
        let (hub, port) = hub_on_any_port(false);
        let s = hub.sender("test", target("localhost", port).unwrap()).unwrap();
        s.send(&message_packet(&Message::new("/x", vec![Arg::Float(2.0)])), 1).unwrap();
        until("arrived", || hub.read(0).is_some_and(|st| st.entries.len() == 1));
        let st = hub.status();
        assert_eq!(st.senders.len(), 1);
        assert_eq!(st.senders[0].packets, 1);
        assert_eq!(st.recent[0].address, "/x");
        drop(s);
        assert!(hub.status().senders.is_empty());
        assert!(target("example.com", 1).is_err());
        assert!(target("10.0.0.2", 0).is_err());
        hub.shutdown();
    }
}
