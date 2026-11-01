//! The HTTP hub: one TCP listener per port, the connections on it, what arrived, and the state
//! sent back.
//!
//! - **Listeners** are wanted by the network ([`Hub::set_wanted`]: one [`Want`] per
//!   `chan.http_in`); the hub opens one accept thread per port, binds `127.0.0.1` unless a want
//!   asks for the LAN (`0.0.0.0`), rebinds when that changes and retries a taken port every
//!   second. Port 0 binds any free port ([`ListenerStatus::bound`] says which).
//! - **Connections** get a thread each, at most [`Config::max_connections`] per port and
//!   [`Config::max_per_client`] per client address; more are answered 503 and closed. A request
//!   head and body must arrive within [`Config::timeout`] (slow senders are cut off), a body is at
//!   most [`Config::max_body`] bytes. Every response closes the connection.
//! - **Routes:** `GET /ws` upgrades to a WebSocket (JSON text frames in, the state out);
//!   `POST /set` takes a JSON body, `GET /set?a=1` a query; `GET /state` answers the state once;
//!   any other `GET` is a file from the node's folder ([`crate::files`]).
//! - **What arrives** ([`crate::message`]) is kept per port as the latest values per address
//!   ([`Store`], as OSC does) and tables per address, and handed to the OSC hub
//!   ([`penumbra_osc::Hub::feed`]), which queues it for params by address and offers it to
//!   learn. Clients have token buckets ([`Config::rate`], [`Config::burst`]; all clients of a
//!   port together ten times that): a connection or a WebSocket message over the rate is dropped.
//! - **The state** a WebSocket client gets, at most every [`Want::interval_ms`] and only when it
//!   changed: `{"in": {address: value}, "chans": {name: value}, "controls": [...]}`: what
//!   arrived, the channels the nodes publish ([`Publisher`]) and the show screen's controls
//!   (from [`Hub::set_controls`]).
//! - **Presses** (`{"press": {"/go": true}}`, [`crate::message`]) hold momentary buttons
//!   through the engine's [`Presser`]. On a WebSocket a press is held until the page lets go,
//!   the connection ends, or the page stops renewing it for [`LEASE`] (a phone that lost the
//!   network). A renewal of a press that was let go that way does not press again: the page has
//!   to let go first. Over `POST /set` a press is a tap (one frame).
//! - **The built-in page** ([`PAGE`]) answers `/` when the node's folder has no `index.html`
//!   and no node on the port switched it off: the show screen's controls, for a phone.
//! - **Requests from other sites** are refused: a `Host` that is not an IP address, `localhost`,
//!   a `.local` name or a single-label name (a page of a DNS-rebinding site), and for `/set`,
//!   `/ws` and `/state` an `Origin` other than this server or a `Sec-Fetch-Site` other than
//!   `same-origin` (a page elsewhere that tries to drive the show from the user's browser).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_osc::hub::{BUSY, Hub as OscHub, Store, Via};
use penumbra_osc::pattern::is_pattern;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::files::{self, Refused};
use crate::message::{self, Caps, Parsed};
use crate::request::{self, Request, error_response, json_response, response, send};
use crate::ws::{self, Assembler, Whole};

/// Client addresses tracked per listener.
pub const MAX_CLIENTS: usize = 256;
/// Table addresses kept per listener.
pub const MAX_TABLES: usize = 16;
/// Channels a node publishes to the clients.
pub const MAX_PUBLISHED: usize = 1024;
/// Addresses listed in [`ListenerStatus::last_addresses`].
const LAST_ADDRESSES: usize = 8;
/// How long the accept thread sleeps when nobody connects (and so how fast it notices a stop).
const ACCEPT_POLL: Duration = Duration::from_millis(10);
/// How long a connection blocks in one read.
const READ_POLL: Duration = Duration::from_millis(10);
/// How long one write may block; a client that reads slower is cut off.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a listener retries a port it could not bind.
const RETRY: Duration = Duration::from_secs(1);
/// A silent WebSocket client gets a ping this often.
const PING: Duration = Duration::from_secs(20);
/// How long a closing connection is read from (and its bytes dropped) so the response arrives.
const LINGER: Duration = Duration::from_millis(200);
/// A state built this recently is sent again as it is.
const STATE_CACHE: Duration = Duration::from_millis(8);
/// A press on a WebSocket not renewed for this long is let go. The built-in page renews every
/// 250 ms while a finger is down.
pub const LEASE: Duration = Duration::from_millis(1000);
/// Buttons one WebSocket connection may hold at once.
const MAX_HOLDS: usize = 64;

/// The control page served at `/` when the folder has none: every control flagged `show`, as
/// the show screen draws it, for a phone.
pub const PAGE: &str = include_str!("page.html");

/// Presses the momentary buttons a page names. The engine provides it ([`Hub::set_presser`]);
/// without one, presses are refused.
pub trait Presser: Send + Sync {
    /// The momentary `chan.button` at node path `path` that a page may press, as a key for
    /// [`Presser::hold`]. `any`: any momentary button (the listener's `params` is `all`);
    /// else only one flagged `show`. An error says why not (no such node, a toggle, locked).
    fn find(&self, path: &str, any: bool) -> Result<u64, String>;
    /// Holds the button `key` for page connection `holder`, or lets go.
    fn hold(&self, key: u64, holder: u64, down: bool);
    /// Presses and lets go at once (one frame).
    fn tap(&self, key: u64, holder: u64);
}

/// Settings for all listeners. Not stored: every start has the defaults (`http.config`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Off closes every listener and connection (the wants are kept for when it is on again).
    pub enabled: bool,
    /// Bytes of a request body or a WebSocket message; larger ones are refused.
    pub max_body: usize,
    /// Connections and WebSocket messages per second per client address, on average.
    pub rate: f64,
    /// How many a client may send at once before the rate applies.
    pub burst: f64,
    /// Open connections per port.
    pub max_connections: usize,
    /// Open connections per client address.
    pub max_per_client: usize,
    /// Addresses kept per port; new ones beyond are dropped.
    pub max_addresses: usize,
    /// Seconds a request's head and body may take to arrive.
    pub timeout: f64,
    /// Seconds a WebSocket client may stay silent (it answers pings, so only a dead one does).
    pub idle: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_body: 65_536,
            rate: 200.0,
            burst: 400.0,
            max_connections: 32,
            max_per_client: 8,
            max_addresses: 1024,
            timeout: 5.0,
            idle: 60.0,
        }
    }
}

impl Config {
    /// Brings every number into a sane range.
    pub fn sanitized(mut self) -> Self {
        let finite = |x: f64, d: f64| if x.is_finite() { x } else { d };
        self.max_body = self.max_body.clamp(256, 16 * 1024 * 1024);
        self.rate = finite(self.rate, 200.0).clamp(1.0, 100_000.0);
        self.burst = finite(self.burst, 400.0).clamp(1.0, 100_000.0);
        self.max_connections = self.max_connections.clamp(1, 1024);
        self.max_per_client = self.max_per_client.clamp(1, 1024);
        self.max_addresses = self.max_addresses.clamp(1, 65_536);
        self.timeout = finite(self.timeout, 5.0).clamp(0.1, 120.0);
        self.idle = finite(self.idle, 60.0).clamp(1.0, 3600.0);
        self
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }
}

/// Which params a listener's messages may set by address.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Params {
    /// None: the messages only make channels.
    #[default]
    Off,
    /// The `value` of the controls on the show screen (`chan.slider`, `chan.button` flagged
    /// `show`).
    Controls,
    /// Any param the address names (live, outside the history), as OSC's `params`.
    All,
}

/// A listener as the network wants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Want {
    pub port: u16,
    /// Listen on every interface, not only this machine.
    pub lan: bool,
    pub params: Params,
    /// Milliseconds between two state messages to a WebSocket client, at least.
    pub interval_ms: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClientStatus {
    pub from: String,
    /// Connections it opened (an HTTP request is one).
    pub connections: u64,
    /// Addresses it sent.
    pub messages: u64,
    /// Connections and WebSocket messages over its rate, or over the caps.
    pub dropped: u64,
    /// Connections open now.
    pub open: usize,
    pub seconds_ago: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SocketStatus {
    pub from: String,
    /// Seconds since it connected.
    pub seconds: f64,
    /// WebSocket messages it sent.
    pub messages: u64,
    /// Its messages over the rate.
    pub dropped: u64,
    /// State messages sent to it.
    pub sent: u64,
    #[serde(skip)]
    since: Option<Instant>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ListenerStatus {
    pub port: u16,
    /// The address actually bound, `None` while not bound.
    pub bound: Option<String>,
    pub lan: bool,
    pub params: Params,
    /// Why binding failed (retried every second).
    pub error: Option<String>,
    /// The folder files are served from, if any.
    pub folder: Option<String>,
    /// Connections open now.
    pub connections: usize,
    pub websockets: Vec<SocketStatus>,
    pub clients: Vec<ClientStatus>,
    /// Requests read (WebSocket upgrades included).
    pub requests: u64,
    /// Addresses taken in.
    pub messages: u64,
    /// Addresses per second, over the last second.
    pub messages_per_second: f64,
    /// Bytes of bodies and WebSocket messages.
    pub bytes: u64,
    /// Files served.
    pub files: u64,
    pub not_found: u64,
    /// Requests and WebSocket messages that did not parse, with the last reason.
    pub malformed: u64,
    pub last_malformed: Option<String>,
    /// Bodies and WebSocket messages over [`Config::max_body`].
    pub oversize: u64,
    /// Connections and WebSocket messages over a client's rate or the rate of all.
    pub rate_dropped: u64,
    /// Connections over [`Config::max_connections`] or [`Config::max_per_client`].
    pub refused: u64,
    /// Requests from another site, or for a path outside the folder, with the last reason.
    pub forbidden: u64,
    pub last_forbidden: Option<String>,
    /// Requests that did not arrive within [`Config::timeout`].
    pub timeouts: u64,
    /// New addresses dropped because the store was full.
    pub store_full: u64,
    pub addresses: usize,
    /// The addresses that sent a table.
    pub tables: Vec<String>,
    /// The latest addresses that arrived, newest first.
    pub last_addresses: Vec<String>,
    /// State messages sent to WebSocket clients.
    pub sent: u64,
    /// Button presses taken (a renewal of a held press is not one).
    pub presses: u64,
    /// Buttons held by pages now.
    pub held: usize,
    /// Presses let go because their page stopped renewing them (a phone off the network).
    pub lapsed: u64,
    /// Presses refused, with the last reason.
    pub press_refused: u64,
    pub last_press_refused: Option<String>,
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

struct Client {
    bucket: Bucket,
    open: usize,
    connections: u64,
    messages: u64,
    dropped: u64,
    last: Instant,
}

/// Addresses per second.
struct PerSecond {
    start: Instant,
    count: u64,
    rate: f64,
}

impl PerSecond {
    fn add(&mut self, n: u64, now: Instant) {
        let dt = now.saturating_duration_since(self.start).as_secs_f64();
        if dt >= 1.0 {
            self.rate = self.count as f64 / dt;
            self.start = now;
            self.count = 0;
        }
        self.count += n;
    }

    fn now(&self, now: Instant) -> f64 {
        if now.saturating_duration_since(self.start) > Duration::from_secs(2) { 0.0 } else { self.rate }
    }
}

/// Tables by address, in the order the addresses first came.
#[derive(Clone, Debug, Default)]
pub struct Tables {
    pub version: u64,
    pub list: Vec<(String, Vec<Vec<String>>)>,
}

/// What a node publishes on its port.
struct Published {
    port: u16,
    folder: Option<PathBuf>,
    /// The built-in page may answer `/` when the folder has no `index.html`.
    page: bool,
    chans: Vec<(String, f64)>,
}

type Controls = Arc<dyn Fn() -> Json + Send + Sync>;
/// A watcher: port, id, "called and not yet acknowledged", the callback.
type WatchEntry = (u16, u64, Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);
type Running = (Arc<Listener>, Option<JoinHandle<()>>);

/// One port.
struct Listener {
    want: Mutex<Want>,
    stop: AtomicBool,
    store: Mutex<Store>,
    tables: Mutex<Tables>,
    status: Mutex<ListenerStatus>,
    clients: Mutex<BTreeMap<IpAddr, Client>>,
    global: Mutex<Bucket>,
    sockets: Mutex<BTreeMap<u64, SocketStatus>>,
    open: AtomicUsize,
    per_second: Mutex<PerSecond>,
    last: Mutex<VecDeque<String>>,
    /// The learn generation and the addresses busy when it was armed.
    busy: Mutex<(u64, BTreeSet<String>)>,
    cache: Mutex<Option<(Instant, String)>>,
    /// Buttons held by this port's WebSocket connections now.
    held: AtomicUsize,
}

struct Inner {
    osc: OscHub,
    config: Mutex<Config>,
    wanted: Mutex<Vec<Want>>,
    listeners: Mutex<BTreeMap<u16, Running>>,
    watches: Mutex<Vec<WatchEntry>>,
    published: Mutex<BTreeMap<u64, Published>>,
    controls: Mutex<Option<Controls>>,
    presser: Mutex<Option<Arc<dyn Presser>>>,
    next_id: AtomicU64,
}

/// The HTTP hub, shared by the engine, the nodes and the commands (clones share it).
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
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

/// What one node gives its port: the folder to serve and the channels for the clients. Dropping
/// it takes both back.
pub struct Publisher {
    port: u16,
    id: u64,
    hub: Weak<Inner>,
}

impl Publisher {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The folder files are served from (`None`: no files), whether the built-in page may answer
    /// `/` when that folder has no `index.html`, and the channels sent to clients.
    pub fn set(&self, folder: Option<PathBuf>, page: bool, mut chans: Vec<(String, f64)>) {
        chans.truncate(MAX_PUBLISHED);
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.published).insert(self.id, Published { port: self.port, folder, page, chans });
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.published).remove(&self.id);
        }
    }
}

impl Hub {
    /// A hub handing its messages to `osc` (for params by address and learn).
    pub fn new(osc: OscHub) -> Self {
        Self {
            inner: Arc::new(Inner {
                osc,
                config: Mutex::new(Config::default()),
                wanted: Mutex::new(Vec::new()),
                listeners: Mutex::new(BTreeMap::new()),
                watches: Mutex::new(Vec::new()),
                published: Mutex::new(BTreeMap::new()),
                controls: Mutex::new(None),
                presser: Mutex::new(None),
                next_id: AtomicU64::new(1),
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

    /// Where the show screen's controls come from, for the state sent to clients.
    pub fn set_controls(&self, controls: Controls) {
        *lock(&self.inner.controls) = Some(controls);
    }

    /// Who presses the buttons pages name.
    pub fn set_presser(&self, presser: Arc<dyn Presser>) {
        *lock(&self.inner.presser) = Some(presser);
    }

    /// The listeners the network wants. Wants on the same port merge: the LAN if any asks for
    /// it, the widest params, the shortest interval.
    pub fn set_wanted(&self, wants: &[Want]) {
        let mut merged: BTreeMap<u16, Want> = BTreeMap::new();
        for w in wants {
            merged
                .entry(w.port)
                .and_modify(|m| {
                    m.lan |= w.lan;
                    m.params = m.params.max(w.params);
                    m.interval_ms = m.interval_ms.min(w.interval_ms);
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
        let gone: Vec<u16> = listeners.keys().filter(|p| !keep.contains_key(p)).copied().collect();
        let mut touched: Vec<u16> = Vec::new();
        for port in gone {
            touched.push(port);
            if let Some((l, _thread)) = listeners.remove(&port) {
                // Not joined here, so the cook thread never waits; it ends within a poll.
                l.stop.store(true, Ordering::Release);
            }
        }
        for (port, want) in keep {
            match listeners.get(&port) {
                Some((l, _)) => *lock(&l.want) = want,
                None => {
                    touched.push(port);
                    let now = Instant::now();
                    let l = Arc::new(Listener {
                        want: Mutex::new(want),
                        stop: AtomicBool::new(false),
                        store: Mutex::new(Store::default()),
                        tables: Mutex::new(Tables::default()),
                        status: Mutex::new(ListenerStatus { port, ..ListenerStatus::default() }),
                        clients: Mutex::new(BTreeMap::new()),
                        global: Mutex::new(Bucket::new(0.0, now)),
                        sockets: Mutex::new(BTreeMap::new()),
                        open: AtomicUsize::new(0),
                        per_second: Mutex::new(PerSecond { start: now, count: 0, rate: 0.0 }),
                        last: Mutex::new(VecDeque::new()),
                        busy: Mutex::new((0, BTreeSet::new())),
                        cache: Mutex::new(None),
                        held: AtomicUsize::new(0),
                    });
                    {
                        let config = self.config();
                        *lock(&l.global) = Bucket::new(config.burst * 10.0, now);
                    }
                    let (inner, listener) = (Arc::downgrade(&self.inner), l.clone());
                    let thread = std::thread::Builder::new()
                        .name(format!("http-in-{port}"))
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

    fn listener_of(&self, port: u16) -> Option<Arc<Listener>> {
        lock(&self.inner.listeners).get(&port).map(|(l, _)| l.clone())
    }

    /// What port `port` (as the want named it) received; `None` when nothing listens there.
    pub fn read(&self, port: u16) -> Option<Store> {
        self.listener_of(port).map(|l| lock(&l.store).clone())
    }

    /// The tables port `port` received.
    pub fn tables(&self, port: u16) -> Option<Tables> {
        self.listener_of(port).map(|l| lock(&l.tables).clone())
    }

    /// The listener's status, `None` when nothing listens on `port`.
    pub fn listener(&self, port: u16) -> Option<ListenerStatus> {
        let l = self.listener_of(port)?;
        Some(status_of(&self.inner, &l, port))
    }

    /// Every listener's status.
    pub fn status(&self) -> Vec<ListenerStatus> {
        let all: Vec<(u16, Arc<Listener>)> =
            lock(&self.inner.listeners).iter().map(|(p, (l, _))| (*p, l.clone())).collect();
        all.iter().map(|(p, l)| status_of(&self.inner, l, *p)).collect()
    }

    /// Calls `wake` when `port`'s store or tables change, or its listener opens, closes, binds
    /// or fails, at most once until [`Watch::ack`]. Works before anything listens on `port`.
    pub fn watch(&self, port: u16, wake: Arc<dyn Fn() + Send + Sync>) -> Watch {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(AtomicBool::new(false));
        lock(&self.inner.watches).push((port, id, pending.clone(), wake));
        Watch { port, id, pending, hub: Arc::downgrade(&self.inner) }
    }

    /// A handle for one node's folder and channels on `port`.
    pub fn publisher(&self, port: u16) -> Publisher {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        Publisher { port, id, hub: Arc::downgrade(&self.inner) }
    }

    /// Takes in a JSON message on `port` as if a client on this machine had sent it (for
    /// `http.send`, so Claude and tests do what a page does); how many addresses it had.
    pub fn inject(&self, port: u16, text: &str) -> Result<usize, String> {
        let l = self.listener_of(port).ok_or_else(|| format!("nothing listens on port {port}"))?;
        let parsed = message::parse_json(text, &self.config().caps())?;
        lock(&l.status).bytes += text.len() as u64;
        let refused = take_in(&self.inner, &l, (Ipv4Addr::LOCALHOST, 0).into(), &parsed, None);
        if !refused.is_empty() {
            return Err(refused.join("; "));
        }
        Ok(parsed.len())
    }

    /// The state a WebSocket client on `port` gets now.
    pub fn state(&self, port: u16) -> Option<Json> {
        let l = self.listener_of(port)?;
        serde_json::from_str(&state_text(&self.inner, &l, port)).ok()
    }

    /// Closes every listener (at shutdown); open connections end within a poll.
    pub fn shutdown(&self) {
        let all = std::mem::take(&mut *lock(&self.inner.listeners));
        for (l, thread) in all.into_values() {
            l.stop.store(true, Ordering::Release);
            if let Some(t) = thread
                && t.join().is_err()
            {
                log::error!("an HTTP listener thread panicked");
            }
        }
    }
}

fn status_of(inner: &Inner, l: &Listener, port: u16) -> ListenerStatus {
    let now = Instant::now();
    let mut s = lock(&l.status).clone();
    s.addresses = lock(&l.store).entries.len();
    s.tables = lock(&l.tables).list.iter().map(|(a, _)| a.clone()).collect();
    s.connections = l.open.load(Ordering::Acquire);
    s.held = l.held.load(Ordering::Acquire);
    s.messages_per_second = lock(&l.per_second).now(now);
    s.last_addresses = lock(&l.last).iter().rev().cloned().collect();
    s.folder = folder_of(inner, port).map(|f| f.display().to_string());
    s.websockets = lock(&l.sockets)
        .values()
        .map(|w| {
            let mut w = w.clone();
            w.seconds = w.since.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
            w
        })
        .collect();
    s.clients = lock(&l.clients)
        .iter()
        .map(|(ip, c)| ClientStatus {
            from: ip.to_string(),
            connections: c.connections,
            messages: c.messages,
            dropped: c.dropped,
            open: c.open,
            seconds_ago: now.saturating_duration_since(c.last).as_secs_f64(),
        })
        .collect();
    s
}

/// The folder the first node on `port` (by when it registered) serves.
fn folder_of(inner: &Inner, port: u16) -> Option<PathBuf> {
    lock(&inner.published).values().filter(|p| p.port == port).find_map(|p| p.folder.clone())
}

/// Whether the built-in page may answer on `port`: a node publishes there and none switched
/// the page off.
fn page_of(inner: &Inner, port: u16) -> bool {
    let published = lock(&inner.published);
    let mut on_port = published.values().filter(|p| p.port == port).peekable();
    on_port.peek().is_some() && on_port.all(|p| p.page)
}

/// Calls the watchers of `port`.
fn wake(inner: &Inner, port: u16) {
    for (p, _, pending, wake) in lock(&inner.watches).iter() {
        if *p == port && !pending.swap(true, Ordering::AcqRel) {
            wake();
        }
    }
}

fn catch(f: impl FnOnce()) -> std::thread::Result<()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
}

// ---------------------------------------------------------------------------------------------
// Taking messages in, building the state

/// Takes in what a message says; returns the presses refused, with their reasons. `holds`: the
/// WebSocket connection's held buttons (a press holds); `None` over plain HTTP (a press taps).
fn take_in(
    inner: &Inner,
    l: &Listener,
    from: SocketAddr,
    parsed: &Parsed,
    holds: Option<&mut Holds<'_>>,
) -> Vec<String> {
    let want = *lock(&l.want);
    let config = lock(&inner.config).clone();
    let now = Instant::now();
    let generation = inner.osc.learn_generation();
    if inner.osc.learning() {
        let mut busy = lock(&l.busy);
        if busy.0 != generation {
            let store = lock(&l.store);
            let heard = store
                .entries
                .iter()
                .filter(|e| e.at.is_some_and(|at| now.saturating_duration_since(at) < BUSY));
            *busy = (generation, heard.map(|e| e.address.clone()).collect());
        }
    }
    let mut changed = false;
    let mut full = 0u64;
    {
        let mut store = lock(&l.store);
        for m in &parsed.messages {
            // A pattern sets params; as a channel it would be a channel named after the pattern.
            if is_pattern(&m.address) {
                continue;
            }
            if store.set(&m.address, &m.args, config.max_addresses) {
                changed = true;
            } else {
                full += 1;
            }
        }
    }
    if !parsed.tables.is_empty() {
        let mut tables = lock(&l.tables);
        for (address, rows) in &parsed.tables {
            if let Some(t) = tables.list.iter_mut().find(|(a, _)| a == address) {
                t.1 = rows.clone();
            } else if tables.list.len() < MAX_TABLES {
                tables.list.push((address.clone(), rows.clone()));
            } else {
                full += 1;
                continue;
            }
            changed = true;
        }
        tables.version += 1;
    }
    {
        let mut st = lock(&l.status);
        st.messages += parsed.len() as u64;
        st.store_full += full;
    }
    lock(&l.per_second).add(parsed.len() as u64, now);
    {
        let mut last = lock(&l.last);
        let addresses =
            parsed.messages.iter().map(|m| &m.address).chain(parsed.tables.iter().map(|(a, _)| a));
        for a in addresses {
            last.retain(|x| x != a);
            if last.len() >= LAST_ADDRESSES {
                last.pop_front();
            }
            last.push_back(a.clone());
        }
    }
    if let Some(c) = lock(&l.clients).get_mut(&from.ip()) {
        c.messages += parsed.len() as u64;
    }
    let params = match want.params {
        Params::Off => None,
        Params::Controls => Some(true),
        Params::All => Some(false),
    };
    let busy = lock(&l.busy).clone();
    let is_busy = |a: &str| busy.0 == generation && busy.1.contains(a);
    inner.osc.feed(Via::Http, want.port, from, &parsed.messages, params, &is_busy);
    if changed {
        wake(inner, want.port);
    }
    if parsed.presses.is_empty() {
        return Vec::new();
    }
    let presser = lock(&inner.presser).clone();
    let refused = match (presser, want.params) {
        (None, _) => {
            parsed.presses.iter().map(|(p, _)| format!("press {p}: nothing presses buttons here")).collect()
        }
        (Some(_), Params::Off) => parsed
            .presses
            .iter()
            .map(|(p, _)| format!("press {p}: this listener's `params` is off"))
            .collect(),
        (Some(presser), _) => {
            let any = want.params == Params::All;
            match holds {
                Some(h) => h.take(&presser, &parsed.presses, any),
                None => tap_all(inner, l, &*presser, &parsed.presses, any),
            }
        }
    };
    if !refused.is_empty() {
        let mut st = lock(&l.status);
        st.press_refused += refused.len() as u64;
        st.last_press_refused = refused.last().map(|r| format!("from {from}: {r}"));
    }
    refused
}

/// Presses over plain HTTP: a tap each (a request has no connection that could let go later).
fn tap_all(
    inner: &Inner,
    l: &Listener,
    presser: &dyn Presser,
    presses: &[(String, bool)],
    any: bool,
) -> Vec<String> {
    let holder = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let mut refused = Vec::new();
    for (path, down) in presses {
        if !*down {
            continue;
        }
        match presser.find(path, any) {
            Ok(key) => {
                presser.tap(key, holder);
                lock(&l.status).presses += 1;
            }
            Err(why) => refused.push(format!("press {path}: {why}")),
        }
    }
    refused
}

/// The buttons one WebSocket connection holds, by node path: the presser's key and when the
/// page last renewed the press. Dropping it lets go of all of them, so a connection that ends
/// in any way leaves no button held.
struct Holds<'a> {
    l: &'a Listener,
    holder: u64,
    presser: Option<Arc<dyn Presser>>,
    held: BTreeMap<String, (u64, Instant)>,
    /// Presses let go for want of renewal; renewals are ignored until the page lets go.
    lapsed: BTreeSet<String>,
}

impl<'a> Holds<'a> {
    fn new(l: &'a Listener, holder: u64) -> Self {
        Self { l, holder, presser: None, held: BTreeMap::new(), lapsed: BTreeSet::new() }
    }

    fn take(&mut self, presser: &Arc<dyn Presser>, presses: &[(String, bool)], any: bool) -> Vec<String> {
        let now = Instant::now();
        let mut refused = Vec::new();
        for (path, down) in presses {
            if !*down {
                self.lapsed.remove(path);
                if let Some((key, _)) = self.held.remove(path) {
                    presser.hold(key, self.holder, false);
                    self.l.held.fetch_sub(1, Ordering::AcqRel);
                }
                continue;
            }
            if let Some((_, renewed)) = self.held.get_mut(path) {
                *renewed = now;
                continue;
            }
            if self.lapsed.contains(path) {
                continue;
            }
            if self.held.len() >= MAX_HOLDS {
                refused.push(format!("press {path}: more than {MAX_HOLDS} buttons held at once"));
                continue;
            }
            match presser.find(path, any) {
                Ok(key) => {
                    presser.hold(key, self.holder, true);
                    self.held.insert(path.clone(), (key, now));
                    self.l.held.fetch_add(1, Ordering::AcqRel);
                    lock(&self.l.status).presses += 1;
                    self.presser = Some(presser.clone());
                }
                Err(why) => refused.push(format!("press {path}: {why}")),
            }
        }
        refused
    }

    /// Lets go of the presses not renewed within [`LEASE`].
    fn expire(&mut self, now: Instant) {
        let Some(presser) = &self.presser else { return };
        let old: Vec<String> = self
            .held
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) > LEASE)
            .map(|(p, _)| p.clone())
            .collect();
        for path in old {
            if let Some((key, _)) = self.held.remove(&path) {
                presser.hold(key, self.holder, false);
                self.l.held.fetch_sub(1, Ordering::AcqRel);
                lock(&self.l.status).lapsed += 1;
                self.lapsed.insert(path);
            }
        }
    }
}

impl Drop for Holds<'_> {
    fn drop(&mut self) {
        let Some(presser) = self.presser.take() else { return };
        for (key, _) in std::mem::take(&mut self.held).into_values() {
            presser.hold(key, self.holder, false);
            self.l.held.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// The state as JSON text, built at most every [`STATE_CACHE`].
fn state_text(inner: &Inner, l: &Listener, port: u16) -> String {
    let now = Instant::now();
    if let Some((at, text)) = lock(&l.cache).as_ref()
        && now.saturating_duration_since(*at) < STATE_CACHE
    {
        return text.clone();
    }
    let mut received = serde_json::Map::new();
    for e in &lock(&l.store).entries {
        let v = match (e.values.as_slice(), &e.text) {
            ([x], _) => json!(finite(*x)),
            ([], Some(t)) => json!(t),
            ([], None) => Json::Null,
            (xs, _) => json!(xs.iter().map(|x| finite(*x)).collect::<Vec<_>>()),
        };
        received.insert(e.address.clone(), v);
    }
    let mut chans = serde_json::Map::new();
    for p in lock(&inner.published).values().filter(|p| p.port == port) {
        for (name, v) in &p.chans {
            chans.insert(name.clone(), json!(finite(*v)));
        }
    }
    let controls = lock(&inner.controls).clone();
    let controls = controls.map_or(Json::Array(Vec::new()), |c| c());
    let params = lock(&l.want).params;
    let text = json!({"in": received, "chans": chans, "controls": controls, "params": params}).to_string();
    *lock(&l.cache) = Some((now, text.clone()));
    text
}

fn finite(x: f64) -> f64 {
    if x.is_finite() { x } else { 0.0 }
}

// ---------------------------------------------------------------------------------------------
// The accept thread

fn bind(want: Want) -> std::io::Result<TcpListener> {
    let ip = if want.lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
    let s = TcpListener::bind((ip, want.port))?;
    s.set_nonblocking(true)?;
    Ok(s)
}

fn listen(inner: &Weak<Inner>, l: &Arc<Listener>) {
    let mut socket: Option<(TcpListener, bool)> = None;
    let mut retry_at: Option<Instant> = None;
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
                std::thread::sleep(ACCEPT_POLL * 10);
                continue;
            }
            match bind(want) {
                Ok(s) => {
                    let mut st = lock(&l.status);
                    st.bound = s.local_addr().ok().map(|a| a.to_string());
                    st.error = None;
                    log::info!("HTTP: listening on {}", st.bound.as_deref().unwrap_or("?"));
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
                        log::warn!("HTTP: {msg}");
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
        match sock.accept() {
            Ok((stream, from)) => {
                let Some(inner) = inner.upgrade() else { return };
                if catch(|| admit(&inner, l, stream, from)).is_err() {
                    log::error!("HTTP: taking a connection panicked; dropped");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(e) => {
                log::debug!("HTTP: accept: {e}");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
    let port = lock(&l.status).port;
    log::info!("HTTP: stopped listening on port {port}");
}

/// Answers a connection that is not taken, without waiting for it.
fn turn_away(mut stream: TcpStream, status: u16, why: &str) {
    if stream.set_nonblocking(true).is_ok() {
        send(&mut stream, &error_response(status, why));
    }
}

/// Lets a connection in (a thread of its own) or turns it away: over the caps, over the rate.
fn admit(inner: &Arc<Inner>, l: &Arc<Listener>, stream: TcpStream, from: SocketAddr) {
    let config = lock(&inner.config).clone();
    let now = Instant::now();
    let ip = from.ip();
    let mut clients = lock(&l.clients);
    if !clients.contains_key(&ip) && clients.len() >= MAX_CLIENTS {
        let oldest = clients.iter().filter(|(_, c)| c.open == 0).min_by_key(|(_, c)| c.last).map(|(a, _)| *a);
        if let Some(a) = oldest {
            clients.remove(&a);
        }
    }
    let total = l.open.load(Ordering::Acquire);
    let global_ok = lock(&l.global).take(now, config.rate * 10.0, config.burst * 10.0);
    let c = clients.entry(ip).or_insert_with(|| Client {
        bucket: Bucket::new(config.burst, now),
        open: 0,
        connections: 0,
        messages: 0,
        dropped: 0,
        last: now,
    });
    c.last = now;
    c.connections += 1;
    if total >= config.max_connections || c.open >= config.max_per_client {
        c.dropped += 1;
        drop(clients);
        lock(&l.status).refused += 1;
        turn_away(stream, 503, "too many connections");
        return;
    }
    if !(c.bucket.take(now, config.rate, config.burst) && global_ok) {
        c.dropped += 1;
        drop(clients);
        lock(&l.status).rate_dropped += 1;
        turn_away(stream, 429, "too many requests");
        return;
    }
    c.open += 1;
    drop(clients);
    l.open.fetch_add(1, Ordering::AcqRel);
    let guard = Open { l: l.clone(), ip };
    let weak = Arc::downgrade(inner);
    let port = lock(&l.status).port;
    let spawned = std::thread::Builder::new().name(format!("http-{port}-conn")).spawn(move || {
        let guard = guard;
        if catch(|| serve(&weak, &guard.l, stream, from)).is_err() {
            log::error!("HTTP: a connection panicked; closed");
        }
    });
    if let Err(e) = spawned {
        // The closure, and with it the guard, is dropped: the counts go back.
        log::warn!("HTTP: cannot start a connection thread: {e}");
    }
}

/// Counts a connection as open while it lives.
struct Open {
    l: Arc<Listener>,
    ip: IpAddr,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.l.open.fetch_sub(1, Ordering::AcqRel);
        if let Some(c) = lock(&self.l.clients).get_mut(&self.ip) {
            c.open = c.open.saturating_sub(1);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// One connection

fn malformed(l: &Listener, from: SocketAddr, why: &str) {
    let mut st = lock(&l.status);
    st.malformed += 1;
    st.last_malformed = Some(format!("from {from}: {why}"));
}

fn forbidden(l: &Listener, from: SocketAddr, why: &str) {
    let mut st = lock(&l.status);
    st.forbidden += 1;
    st.last_forbidden = Some(format!("from {from}: {why}"));
}

/// Whether the `Host` names this server the way a person or a program would: an IP address,
/// `localhost`, a `.local` name or a single-label machine name. A page of another site that
/// points its own name at this machine (DNS rebinding) sends its name and is refused.
pub fn host_ok(host: Option<&str>) -> bool {
    let Some(host) = host else { return true };
    let host = host.trim().to_ascii_lowercase();
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, _)) => return ip.parse::<std::net::Ipv6Addr>().is_ok(),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((n, p)) if p.bytes().all(|b| b.is_ascii_digit()) => n.to_string(),
            _ => host.clone(),
        }
    };
    if name.is_empty() {
        return false;
    }
    name.parse::<IpAddr>().is_ok() || name == "localhost" || name.ends_with(".local") || !name.contains('.')
}

/// Whether a request that acts (`/set`, `/ws`, `/state`) comes from this server's own page or
/// from a program, not from a page of another site in the user's browser.
pub fn origin_ok(req: &Request) -> Result<(), &'static str> {
    if let Some(site) = req.header("sec-fetch-site")
        && site != "same-origin"
        && site != "none"
    {
        return Err("a request from another site's page");
    }
    if let Some(origin) = req.header("origin") {
        let authority = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://"));
        let same = authority.zip(req.header("host")).is_some_and(|(a, h)| a.eq_ignore_ascii_case(h));
        if !same {
            return Err("a request from a page of another origin");
        }
    }
    Ok(())
}

/// Reads until `buf` holds `want` bytes; an error when the client stopped, the deadline passed
/// or the listener stops.
fn read_until(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    want: usize,
    deadline: Instant,
    l: &Listener,
) -> Result<(), &'static str> {
    let mut chunk = [0u8; 4096];
    while buf.len() < want {
        if Instant::now() > deadline {
            return Err("timeout");
        }
        if l.stop.load(Ordering::Acquire) {
            return Err("stopped");
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err("closed"),
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or(&[])),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("closed"),
        }
    }
    Ok(())
}

/// Serves one connection, then closes it so that the response arrives: closing a socket with
/// unread bytes in it resets the connection, and the client may lose what was sent.
fn serve(inner: &Weak<Inner>, l: &Listener, mut stream: TcpStream, from: SocketAddr) {
    handle(inner, l, &mut stream, from);
    stream.shutdown(std::net::Shutdown::Write).ok();
    let until = Instant::now() + LINGER;
    let mut chunk = [0u8; 4096];
    let mut drained = 0usize;
    while Instant::now() < until && drained < 1 << 20 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => drained += n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
}

fn handle(inner: &Weak<Inner>, l: &Listener, stream: &mut TcpStream, from: SocketAddr) {
    let Some(config) = inner.upgrade().map(|i| lock(&i.config).clone()) else { return };
    let setup = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(READ_POLL)))
        .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)));
    if setup.is_err() {
        return;
    }
    stream.set_nodelay(true).ok();
    let deadline = Instant::now() + Duration::from_secs_f64(config.timeout);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_len = loop {
        let end = request::head_end(&buf);
        if end.is_none_or(|n| n > request::MAX_HEAD) && buf.len() > request::MAX_HEAD {
            malformed(l, from, "the request head is too long");
            send(stream, &error_response(431, "the request head is too long"));
            return;
        }
        if let Some(n) = end {
            break n;
        }
        if Instant::now() > deadline {
            lock(&l.status).timeouts += 1;
            if !buf.is_empty() {
                send(stream, &error_response(408, "the request took too long"));
            }
            return;
        }
        if l.stop.load(Ordering::Acquire) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or(&[])),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    };
    let req = match request::parse_head(buf.get(..head_len).unwrap_or(&[])) {
        Ok(r) => r,
        Err(why) => {
            malformed(l, from, why);
            send(stream, &error_response(400, why));
            return;
        }
    };
    lock(&l.status).requests += 1;
    let rest: Vec<u8> = buf.get(head_len..).unwrap_or(&[]).to_vec();
    if !host_ok(req.header("host")) {
        forbidden(l, from, "a Host header naming another site");
        send(stream, &error_response(403, "this server answers to its IP address or localhost"));
        return;
    }
    let path = match request::percent_decode(&req.path, false) {
        Ok(p) => p,
        Err(why) => {
            malformed(l, from, why);
            send(stream, &error_response(400, why));
            return;
        }
    };
    let acts = matches!(path.as_str(), "/set" | "/ws" | "/state");
    if acts && let Err(why) = origin_ok(&req) {
        forbidden(l, from, why);
        send(stream, &error_response(403, why));
        return;
    }
    let Some(inner) = inner.upgrade() else { return };
    let port = lock(&l.status).port;
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/ws") => websocket(&inner, l, port, stream, from, &req, rest, &config),
        ("POST", "/set") => {
            let len = match req.body_length() {
                Ok(n) => n,
                Err((status, why)) => {
                    malformed(l, from, why);
                    send(stream, &error_response(status, why));
                    return;
                }
            };
            if len > config.max_body {
                lock(&l.status).oversize += 1;
                let why = format!("a body of {len} bytes is over the cap of {}", config.max_body);
                send(stream, &error_response(413, &why));
                return;
            }
            let mut body = rest;
            if let Err(why) = read_until(stream, &mut body, len, deadline, l) {
                if why == "timeout" {
                    lock(&l.status).timeouts += 1;
                    send(stream, &error_response(408, "the body took too long"));
                }
                return;
            }
            body.truncate(len);
            lock(&l.status).bytes += len as u64;
            let reply = match std::str::from_utf8(&body) {
                Ok(text) => take_text(&inner, l, from, text, false, None),
                Err(_) => Err("the body is not UTF-8".to_string()),
            };
            answer(stream, l, from, reply);
        }
        ("GET", "/set") => {
            let reply = take_text(&inner, l, from, req.query.as_deref().unwrap_or(""), true, None);
            answer(stream, l, from, reply);
        }
        ("GET" | "HEAD", "/state") => {
            let text = state_text(&inner, l, port);
            send(stream, &response(200, "application/json", text.as_bytes(), req.method == "HEAD"));
        }
        ("GET" | "HEAD", _) => serve_file(&inner, l, port, stream, from, &path, req.method == "HEAD"),
        _ => send(stream, &error_response(405, "GET or HEAD for files, POST /set, GET /set?..., GET /ws")),
    }
}

/// Parses and takes in one message (JSON, or a query when `query`): how many addresses it had
/// and the presses refused.
fn take_text(
    inner: &Inner,
    l: &Listener,
    from: SocketAddr,
    text: &str,
    query: bool,
    holds: Option<&mut Holds<'_>>,
) -> Result<(usize, Vec<String>), String> {
    let caps = lock(&inner.config).caps();
    let parsed = if query { message::parse_query(text, &caps) } else { message::parse_json(text, &caps) }?;
    let refused = take_in(inner, l, from, &parsed, holds);
    Ok((parsed.len(), refused))
}

fn answer(
    stream: &mut TcpStream,
    l: &Listener,
    from: SocketAddr,
    reply: Result<(usize, Vec<String>), String>,
) {
    match reply {
        Ok((n, refused)) if refused.is_empty() => {
            send(stream, &json_response(200, &json!({"ok": true, "addresses": n})));
        }
        Ok((n, refused)) => {
            send(stream, &json_response(200, &json!({"ok": true, "addresses": n, "refused": refused})));
        }
        Err(why) => {
            malformed(l, from, &why);
            send(stream, &error_response(400, &why));
        }
    }
}

fn serve_file(
    inner: &Inner,
    l: &Listener,
    port: u16,
    stream: &mut TcpStream,
    from: SocketAddr,
    path: &str,
    head: bool,
) {
    let not_found = |stream: &mut TcpStream, why: &str| {
        lock(&l.status).not_found += 1;
        send(stream, &response(404, "text/plain; charset=utf-8", why.as_bytes(), head));
    };
    let front = path == "/" || path == "/index.html";
    let builtin = |stream: &mut TcpStream| {
        lock(&l.status).files += 1;
        send(stream, &response(200, "text/html; charset=utf-8", PAGE.as_bytes(), head));
    };
    let Some(folder) = folder_of(inner, port) else {
        if front && page_of(inner, port) {
            return builtin(stream);
        }
        return not_found(
            stream,
            "Not found. This chan.http_in serves no folder (its `folder` param is empty).",
        );
    };
    match files::resolve(&folder, path) {
        Err(Refused::NotFound) if front && page_of(inner, port) => builtin(stream),
        Ok(file) => match std::fs::read(&file) {
            Ok(bytes) => {
                lock(&l.status).files += 1;
                send(stream, &response(200, files::content_type(&file), &bytes, head));
            }
            Err(_) => not_found(stream, "Not found."),
        },
        Err(Refused::NotFound) if path == "/" || path.ends_with('/') => not_found(
            stream,
            "Not found. Put an index.html into the chan.http_in's folder (`www` in the project by default).",
        ),
        Err(Refused::NotFound) => not_found(stream, "Not found."),
        Err(Refused::Forbidden(why)) => {
            forbidden(l, from, why);
            send(stream, &response(403, "text/plain; charset=utf-8", why.as_bytes(), head));
        }
        Err(Refused::TooBig(n)) => {
            let why = format!("the file has {n} bytes, more than the {} served", files::MAX_FILE);
            send(stream, &response(413, "text/plain; charset=utf-8", why.as_bytes(), head));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// WebSocket

/// Takes a message token of the client and of all clients.
fn admit_message(inner: &Inner, l: &Listener, ip: IpAddr) -> bool {
    let config = lock(&inner.config).clone();
    let now = Instant::now();
    let global_ok = lock(&l.global).take(now, config.rate * 10.0, config.burst * 10.0);
    let mut clients = lock(&l.clients);
    let Some(c) = clients.get_mut(&ip) else { return global_ok };
    c.last = now;
    let ok = c.bucket.take(now, config.rate, config.burst) && global_ok;
    if !ok {
        c.dropped += 1;
    }
    ok
}

/// Removes a socket's entry when the connection ends.
struct SocketEntry<'a> {
    l: &'a Listener,
    id: u64,
}

impl Drop for SocketEntry<'_> {
    fn drop(&mut self) {
        lock(&self.l.sockets).remove(&self.id);
    }
}

#[allow(clippy::too_many_arguments)]
fn websocket(
    inner: &Inner,
    l: &Listener,
    port: u16,
    stream: &mut TcpStream,
    from: SocketAddr,
    req: &Request,
    rest: Vec<u8>,
    config: &Config,
) {
    let key = req.header("sec-websocket-key").filter(|k| !k.is_empty());
    let upgrade = req.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let (Some(key), true, true) = (key, upgrade, req.lists("connection", "upgrade")) else {
        malformed(l, from, "GET /ws without a WebSocket upgrade");
        send(stream, &error_response(400, "/ws is a WebSocket; connect with new WebSocket(...)"));
        return;
    };
    if req.header("sec-websocket-version") != Some("13") {
        send(stream, &error_response(400, "only WebSocket version 13 is spoken"));
        return;
    }
    let accept = ws::accept_key(key);
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if std::io::Write::write_all(stream, head.as_bytes()).is_err() {
        return;
    }
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    lock(&l.sockets).insert(
        id,
        SocketStatus {
            from: from.to_string(),
            seconds: 0.0,
            messages: 0,
            dropped: 0,
            sent: 0,
            since: Some(Instant::now()),
        },
    );
    let _entry = SocketEntry { l, id };
    // Declared after the entry, so dropped first: the buttons are let go before the socket
    // leaves the status.
    let mut holds = Holds::new(l, id);
    let close = |stream: &mut TcpStream, code: u16, why: &str| send(stream, &ws::close_frame(code, why));
    let mut buf = rest;
    let mut chunk = vec![0u8; 16_384];
    let mut assembler = Assembler::default();
    let idle = Duration::from_secs_f64(config.idle);
    let max = config.max_body;
    let mut last_heard = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_sent: Option<(Instant, String)> = None;
    loop {
        if l.stop.load(Ordering::Acquire) {
            return close(stream, ws::CLOSE_GOING_AWAY, "the listener stopped");
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
                last_heard = Instant::now();
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
        loop {
            let (frame, used) = match ws::parse_frame(&buf, max) {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    if matches!(e, ws::FrameError::TooBig(_)) {
                        lock(&l.status).oversize += 1;
                    } else {
                        malformed(l, from, &e.to_string());
                    }
                    return close(stream, e.code(), &e.to_string());
                }
            };
            buf.drain(..used.min(buf.len()));
            match frame.opcode {
                ws::OP_PING => send(stream, &ws::frame(ws::OP_PONG, &frame.payload)),
                ws::OP_PONG => {}
                ws::OP_CLOSE => return close(stream, ws::CLOSE_NORMAL, ""),
                _ => match assembler.take(frame, max) {
                    Ok(None) => {}
                    Ok(Some(Whole::Text(text))) => {
                        lock(&l.status).bytes += text.len() as u64;
                        let ok = admit_message(inner, l, from.ip());
                        if let Some(s) = lock(&l.sockets).get_mut(&id) {
                            s.messages += 1;
                            s.dropped += u64::from(!ok);
                        }
                        if !ok {
                            lock(&l.status).rate_dropped += 1;
                            continue;
                        }
                        let why = match take_text(inner, l, from, &text, false, Some(&mut holds)) {
                            Ok((_, refused)) if refused.is_empty() => None,
                            Ok((_, refused)) => Some(refused.join("; ")),
                            Err(why) => {
                                malformed(l, from, &why);
                                Some(why)
                            }
                        };
                        if let Some(why) = why {
                            send(
                                stream,
                                &ws::frame(ws::OP_TEXT, json!({"error": why}).to_string().as_bytes()),
                            );
                        }
                    }
                    Ok(Some(Whole::Binary(_))) => {
                        malformed(l, from, "a binary WebSocket message (send JSON as text)");
                    }
                    Err(e) => {
                        if matches!(e, ws::FrameError::TooBig(_)) {
                            lock(&l.status).oversize += 1;
                        } else {
                            malformed(l, from, &e.to_string());
                        }
                        return close(stream, e.code(), &e.to_string());
                    }
                },
            }
        }
        let now = Instant::now();
        holds.expire(now);
        if now.saturating_duration_since(last_heard) > idle {
            return close(stream, ws::CLOSE_GOING_AWAY, "silent for too long");
        }
        if now.saturating_duration_since(last_ping) > PING {
            last_ping = now;
            send(stream, &ws::frame(ws::OP_PING, b""));
        }
        let interval = Duration::from_millis(u64::from(lock(&l.want).interval_ms));
        if last_sent.as_ref().is_none_or(|(at, _)| now.saturating_duration_since(*at) >= interval) {
            let text = state_text(inner, l, port);
            if last_sent.as_ref().is_none_or(|(_, t)| *t != text) {
                let frame = ws::frame(ws::OP_TEXT, text.as_bytes());
                if std::io::Write::write_all(stream, &frame).is_err() {
                    return;
                }
                lock(&l.status).sent += 1;
                if let Some(s) = lock(&l.sockets).get_mut(&id) {
                    s.sent += 1;
                }
                last_sent = Some((now, text));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        for ok in
            ["localhost:8090", "127.0.0.1:8090", "192.168.1.20", "[::1]:8090", "my-pc", "my-pc.local:80"]
        {
            assert!(host_ok(Some(ok)), "{ok}");
        }
        for bad in ["evil.example.com:8090", "localhost.evil.com", "[::1", ""] {
            assert!(!host_ok(Some(bad)), "{bad}");
        }
        assert!(host_ok(None));
    }

    #[test]
    fn origins() {
        let req = |h: &str| {
            request::parse_head(format!("GET /ws HTTP/1.1\r\nHost: 10.0.0.2:8090\r\n{h}\r\n\r\n").as_bytes())
                .unwrap()
        };
        assert!(origin_ok(&req("X: y")).is_ok(), "a program sends no Origin");
        assert!(origin_ok(&req("Origin: http://10.0.0.2:8090")).is_ok());
        assert!(origin_ok(&req("Origin: http://evil.com")).is_err());
        assert!(origin_ok(&req("Origin: null")).is_err(), "a page opened from a file");
        assert!(origin_ok(&req("Sec-Fetch-Site: cross-site")).is_err());
        assert!(origin_ok(&req("Sec-Fetch-Site: same-site")).is_err());
        assert!(origin_ok(&req("Sec-Fetch-Site: same-origin")).is_ok());
    }
}
