//! Links: one per port or peer, each with a thread of its own, shared by the readers and writers
//! that name it.
//!
//! A node asks the [`Hub`] for a [`Reader`] or a [`Writer`] on an [`Endpoint`]. Readers and
//! writers on the same endpoint share one link: one socket, one open serial port, one TCP
//! connection, so a node that writes requests and one that reads the answers talk to the same
//! peer. The last handle dropped stops the link.
//!
//! - **UDP in** ([`Endpoint::Udp`]): a socket bound to the port, on this machine only or on
//!   every interface. Each datagram is one message for bytes; lines and JSON in it are split at
//!   the delimiter.
//! - **UDP out** ([`Endpoint::UdpTo`]): a socket of its own, no thread; sending never blocks.
//! - **TCP listen** ([`Endpoint::TcpListen`]): accepts up to [`MAX_CLIENTS`] clients, a thread
//!   each. Readers take what every client sends; writers send to every client.
//! - **TCP connect** ([`Endpoint::TcpConnect`]) and **serial** ([`Endpoint::Serial`]): one
//!   connection, opened again with backoff ([`BACKOFF_MIN`] doubling up to [`BACKOFF_MAX`]) when
//!   it fails or the peer goes away.
//!
//! Every reader cuts its own messages ([`Decode`]) and keeps its own [`Store`]; the link's
//! thread parses (before it locks the readers), so a reader's cook only copies the store. A new connection (a client
//! connecting, a reconnect, a serial port opened again) first gets each writer's latest message.
//! Sending while nothing is connected drops the message and counts it. What waits to be written
//! to one connection is capped at [`MAX_QUEUED`] bytes, for a serial port at about a second of
//! bytes at its bit rate; beyond, messages are dropped and counted. Nothing here blocks the
//! caller: a link that ends is joined by the thread of the next link on the same endpoint, never
//! by the caller. A panic on a link's thread ends only that link.
//!
//! A serial port opens at the bit rate of the first handle. When no handle asks for that rate
//! any more (the handle that asked for it left), the port opens again at the rate of the oldest
//! handle left.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

use crate::frame::{Framer, Frames};
use crate::parse::{self, Decode, Parsed, Store};

/// Clients one TCP listener takes at once; more are closed at once and counted.
pub const MAX_CLIENTS: usize = 16;
/// Bytes waiting to be written to one connection at most.
pub const MAX_QUEUED: usize = 1 << 20;
/// Largest UDP datagram.
pub const MAX_DATAGRAM: usize = 65_507;
/// The first wait before connecting again.
pub const BACKOFF_MIN: Duration = Duration::from_millis(500);
/// The longest wait before connecting again.
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How long a connection's thread waits for bytes before it looks at what to write and at its
/// stop flag.
const POLL: Duration = Duration::from_millis(10);
/// How long a UDP socket waits for a datagram before it looks at its stop flag.
const UDP_POLL: Duration = Duration::from_millis(100);
/// How long connecting to a TCP peer may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// How long one write to a TCP connection may block.
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often a port that could not be bound is tried again.
const RETRY: Duration = Duration::from_secs(1);
/// Bytes read at once.
const READ_CHUNK: usize = 16 * 1024;
/// How long one pass of a connection's loop may spend writing before it reads again.
const WRITE_BUDGET: Duration = Duration::from_millis(20);
/// Rows of a table one writer sends per call at most.
pub const MAX_MESSAGES: usize = 1024;

/// Where a link reads or writes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    /// Listens for UDP datagrams on `port` (0: any free port), on every interface with `lan`.
    Udp { lan: bool, port: u16 },
    /// Sends UDP datagrams to this address.
    UdpTo(SocketAddr),
    /// Listens for TCP clients on `port` (0: any free port), on every interface with `lan`.
    TcpListen { lan: bool, port: u16 },
    /// Connects to a TCP server.
    TcpConnect(SocketAddr),
    /// A serial port (`COM3`, `/dev/ttyACM0`; empty: the only one there is) at a bit rate, 8N1.
    Serial { name: String, baud: u32 },
}

impl Endpoint {
    /// Whether each message is a datagram of its own (no delimiter between them).
    pub fn datagrams(&self) -> bool {
        matches!(self, Endpoint::Udp { .. } | Endpoint::UdpTo(_))
    }

    /// The link this endpoint shares: a serial port is one link whatever bit rate is asked.
    /// Windows port names do not depend on case, so `com3` is `COM3` there.
    fn key(&self) -> Endpoint {
        match self {
            Endpoint::Serial { name, .. } => {
                let name = name.trim();
                let name = if cfg!(windows) { name.to_ascii_uppercase() } else { name.to_string() };
                Endpoint::Serial { name, baud: 0 }
            }
            other => other.clone(),
        }
    }

    fn baud(&self) -> u32 {
        match self {
            Endpoint::Serial { baud, .. } => *baud,
            _ => 0,
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reach = |lan: bool| if lan { "the network" } else { "this machine" };
        match self {
            Endpoint::Udp { lan, port } => write!(f, "udp port {port} ({})", reach(*lan)),
            Endpoint::UdpTo(a) => write!(f, "udp to {a}"),
            Endpoint::TcpListen { lan, port } => write!(f, "tcp port {port} ({})", reach(*lan)),
            Endpoint::TcpConnect(a) => write!(f, "tcp to {a}"),
            Endpoint::Serial { name, .. } if name.is_empty() => write!(f, "serial (the only port)"),
            Endpoint::Serial { name, .. } => write!(f, "serial {name}"),
        }
    }
}

/// What a link did lately.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    pub endpoint: String,
    /// The readers' and writers' labels (the nodes' names), in the order they came.
    pub readers: Vec<String>,
    pub writers: Vec<String>,
    /// Bound (listening), connected, or the serial port open.
    pub open: bool,
    /// The address bound, for a listener (with the port picked for port 0).
    pub local: Option<String>,
    /// Who is connected now: a TCP listener's clients, a TCP connection's peer, the serial port
    /// opened; for UDP in the last sender.
    pub peers: Vec<String>,
    /// The serial port's bit rate in use.
    pub baud: Option<u32>,
    /// The last error, cleared when the link opens again.
    pub error: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Messages handed over by writers and sent (a TCP listener counts each once).
    pub messages_out: u64,
    /// Messages dropped on the way out: nothing connected, the queue full, a datagram too large.
    pub dropped_out: u64,
    pub last_drop: Option<String>,
    /// Connections opened (for a listener: clients taken).
    pub connects: u64,
    /// Clients closed at once because the listener had as many as it takes.
    pub refused: u64,
}

struct ReaderSlot {
    label: String,
    order: u64,
    decode: Decode,
    /// Per connection, the message begun on it.
    framers: BTreeMap<u64, Framer>,
    store: Store,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// A wake is on its way and the reader has not read since: no second one.
    woken: Arc<AtomicBool>,
    /// The serial bit rate asked for (0 elsewhere).
    baud: u32,
}

struct WriterSlot {
    label: String,
    order: u64,
    baud: u32,
    /// The last message sent, with its delimiter, for a connection that opens later.
    latest: Vec<u8>,
}

#[derive(Default)]
struct Outbox {
    bytes: VecDeque<u8>,
}

struct Link {
    endpoint: Endpoint,
    readers: Mutex<BTreeMap<u64, ReaderSlot>>,
    writers: Mutex<BTreeMap<u64, WriterSlot>>,
    /// Per open connection, what waits to be written.
    outboxes: Mutex<BTreeMap<u64, Outbox>>,
    /// The socket of a UDP out link.
    udp_out: Option<UdpSocket>,
    stop: AtomicBool,
    status: Mutex<Status>,
    next_conn: AtomicU64,
    /// The serial bit rate in use.
    baud: AtomicU32,
    /// The serial port has to open again, at a new bit rate.
    reopen: AtomicBool,
}

type Running = (Arc<Link>, Option<JoinHandle<()>>);

struct Inner {
    links: Mutex<BTreeMap<Endpoint, Running>>,
    /// Links whose last handle was dropped, until their thread has ended.
    stopping: Mutex<Vec<(Endpoint, JoinHandle<()>)>>,
    next: AtomicU64,
}

/// Every link of the process. Cheap to clone.
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
        Hub {
            inner: Arc::new(Inner {
                links: Mutex::new(BTreeMap::new()),
                stopping: Mutex::new(Vec::new()),
                next: AtomicU64::new(1),
            }),
        }
    }

    /// A reader on `endpoint`, starting the link if it is the first handle. `label` names it in
    /// the status (a node's name); `wake` is called when a message arrived or the link opened,
    /// closed or failed, at most once between two [`Reader::store`] calls.
    pub fn reader(
        &self,
        label: &str,
        endpoint: Endpoint,
        decode: Decode,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Reader, String> {
        if matches!(endpoint, Endpoint::UdpTo(_)) {
            return Err("UDP to an address only sends".to_string());
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let slot = ReaderSlot {
            label: label.to_string(),
            order: id,
            decode,
            framers: BTreeMap::new(),
            store: Store::default(),
            wake,
            woken: Arc::new(AtomicBool::new(false)),
            baud: endpoint.baud(),
        };
        let link = self.link(&endpoint, |l| {
            lock(&l.readers).insert(id, slot);
        })?;
        Ok(Reader { link, id, endpoint, hub: Arc::downgrade(&self.inner) })
    }

    /// A writer on `endpoint`, starting the link if it is the first handle.
    pub fn writer(&self, label: &str, endpoint: Endpoint) -> Result<Writer, String> {
        if matches!(endpoint, Endpoint::Udp { .. }) {
            return Err("a UDP port to listen on only receives; send to an address".to_string());
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let slot =
            WriterSlot { label: label.to_string(), order: id, baud: endpoint.baud(), latest: Vec::new() };
        let link = self.link(&endpoint, |l| {
            lock(&l.writers).insert(id, slot);
        })?;
        Ok(Writer { link, id, endpoint, hub: Arc::downgrade(&self.inner) })
    }

    /// The link for `endpoint`, started if there is none; `add` puts the handle's slot in while
    /// the links are locked, so a handle dropped at the same time cannot stop the link under it.
    fn link(&self, endpoint: &Endpoint, add: impl FnOnce(&Link)) -> Result<Arc<Link>, String> {
        let key = endpoint.key();
        let mut links = lock(&self.inner.links);
        if let Some((l, _)) = links.get(&key) {
            add(l);
            return Ok(l.clone());
        }
        // A listener asked for port 0 is found by the port it got, too.
        if let Endpoint::Udp { lan, port } | Endpoint::TcpListen { lan, port } = key
            && port != 0
            && let Some((l, _)) = links.values().find(|(l, _)| {
                let same_kind = match l.endpoint {
                    Endpoint::Udp { lan: a, port: 0 } => matches!(key, Endpoint::Udp { .. }) && a == lan,
                    Endpoint::TcpListen { lan: a, port: 0 } => {
                        matches!(key, Endpoint::TcpListen { .. }) && a == lan
                    }
                    _ => false,
                };
                same_kind && l.bound_port() == Some(port)
            })
        {
            add(l);
            return Ok(l.clone());
        }
        // A link on the same endpoint that is still ending holds its port. The new link's thread
        // waits for it before it binds or opens (the old one looks at its stop flag at least every
        // 100 ms); the caller never does.
        let old: Vec<JoinHandle<()>> = {
            let mut stopping = lock(&self.inner.stopping);
            stopping.retain(|(_, h)| !h.is_finished());
            let (same, rest): (Vec<_>, Vec<_>) = stopping.drain(..).partition(|(e, _)| *e == key);
            *stopping = rest;
            same.into_iter().map(|(_, h)| h).collect()
        };
        let udp_out = match endpoint {
            Endpoint::UdpTo(to) => Some(udp_sender(*to)?),
            _ => None,
        };
        let link = Arc::new(Link {
            endpoint: endpoint.clone(),
            readers: Mutex::new(BTreeMap::new()),
            writers: Mutex::new(BTreeMap::new()),
            outboxes: Mutex::new(BTreeMap::new()),
            udp_out,
            stop: AtomicBool::new(false),
            status: Mutex::new(Status { endpoint: key.to_string(), ..Status::default() }),
            next_conn: AtomicU64::new(1),
            baud: AtomicU32::new(endpoint.baud()),
            reopen: AtomicBool::new(false),
        });
        let handle = if link.udp_out.is_some() {
            // A UDP sender holds no port others want; its old threads are none.
            lock(&link.status).open = true;
            None
        } else {
            let run_link = link.clone();
            Some(
                std::thread::Builder::new()
                    .name(format!("net {key}"))
                    .spawn(move || {
                        for h in old {
                            h.join().ok();
                        }
                        run(&run_link);
                    })
                    .map_err(|e| format!("cannot start a thread for {key}: {e}"))?,
            )
        };
        add(&link);
        links.insert(key, (link.clone(), handle));
        Ok(link)
    }

    /// Every link's status.
    pub fn status(&self) -> Vec<Status> {
        lock(&self.inner.links).values().map(|(l, _)| l.status()).collect()
    }

    /// Stops every link and waits for the threads.
    pub fn shutdown(&self) {
        let links = std::mem::take(&mut *lock(&self.inner.links));
        let stopping = std::mem::take(&mut *lock(&self.inner.stopping));
        for (l, _) in links.values() {
            l.stop.store(true, Ordering::Release);
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

fn udp_sender(to: SocketAddr) -> Result<UdpSocket, String> {
    let local: SocketAddr = match to.ip() {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let s = UdpSocket::bind(local).map_err(|e| format!("cannot open a socket to send to {to}: {e}"))?;
    s.set_nonblocking(true).map_err(|e| format!("cannot open a socket to send to {to}: {e}"))?;
    if let IpAddr::V4(ip) = to.ip()
        && (ip.is_broadcast() || ip.octets().get(3) == Some(&255))
    {
        s.set_broadcast(true).ok();
    }
    Ok(s)
}

/// Drops a handle's entry; the last one stops the link.
fn release(hub: &Weak<Inner>, link: &Arc<Link>) {
    let empty = || lock(&link.readers).is_empty() && lock(&link.writers).is_empty();
    if !empty() {
        link.rebalance_baud();
        return;
    }
    let Some(inner) = hub.upgrade() else {
        // The hub is gone: nobody will join the thread, but it must still end.
        link.stop.store(true, Ordering::Release);
        return;
    };
    let key = link.endpoint.key();
    let mut links = lock(&inner.links);
    // Another handle may have joined in between; only an empty link stops, and only this one.
    if empty()
        && links.get(&key).is_some_and(|(l, _)| Arc::ptr_eq(l, link))
        && let Some((l, handle)) = links.remove(&key)
    {
        l.stop.store(true, Ordering::Release);
        let mut stopping = lock(&inner.stopping);
        stopping.retain(|(_, h)| !h.is_finished());
        if let Some(h) = handle {
            stopping.push((key, h));
        }
    }
}

/// A reader's way into a link. Dropping it forgets the reader; the last handle stops the link.
pub struct Reader {
    link: Arc<Link>,
    id: u64,
    endpoint: Endpoint,
    hub: Weak<Inner>,
}

impl Reader {
    /// The endpoint as asked for (a serial port with the bit rate this reader asked).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Reads from now on as `decode` says; a change starts the store over.
    pub fn configure(&self, decode: &Decode) {
        let mut readers = lock(&self.link.readers);
        let Some(r) = readers.get_mut(&self.id) else { return };
        if r.decode == *decode {
            return;
        }
        r.decode = decode.clone();
        r.framers.clear();
        let version = r.store.version + 1;
        r.store = Store::default();
        r.store.version = version;
    }

    /// The store's version: it changes with every message (and every message dropped). Lets
    /// the next message wake the reader again.
    pub fn version(&self) -> u64 {
        let readers = lock(&self.link.readers);
        let Some(r) = readers.get(&self.id) else { return 0 };
        r.woken.store(false, Ordering::Release);
        r.store.version
    }

    /// A copy of the store. Lets the next message wake the reader again.
    pub fn store(&self) -> Store {
        let readers = lock(&self.link.readers);
        let Some(r) = readers.get(&self.id) else { return Store::default() };
        r.woken.store(false, Ordering::Release);
        r.store.clone()
    }

    pub fn status(&self) -> Status {
        self.link.status()
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        lock(&self.link.readers).remove(&self.id);
        release(&self.hub, &self.link);
    }
}

/// A writer's way into a link. Dropping it forgets the writer; the last handle stops the link.
pub struct Writer {
    link: Arc<Link>,
    id: u64,
    endpoint: Endpoint,
    hub: Weak<Inner>,
}

/// What one [`Writer::send`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    /// Messages on their way (to at least one connection).
    pub sent: usize,
    /// Messages dropped, with the last reason.
    pub dropped: usize,
    pub reason: Option<String>,
    /// Some connection's queue was full and did not get the messages: send them again.
    pub full: bool,
    /// Nothing was connected (normal for a TCP listener without clients).
    pub unconnected: bool,
}

impl Writer {
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn status(&self) -> Status {
        self.link.status()
    }

    /// Sends `messages`: each a datagram over UDP, else each followed by `delimiter`. Never
    /// blocks: a stream's bytes go to the link's thread.
    pub fn send(&self, messages: &[Vec<u8>], delimiter: &[u8]) -> Sent {
        let mut out = Sent::default();
        if messages.is_empty() {
            return out;
        }
        if let Some(socket) = &self.link.udp_out {
            let Endpoint::UdpTo(to) = self.link.endpoint else { return out };
            let mut bytes = 0u64;
            for m in messages {
                let r = if m.len() > MAX_DATAGRAM {
                    Err(format!("a message of {} bytes does not fit a datagram", m.len()))
                } else {
                    socket.send_to(m, to).map_err(|e| {
                        if e.kind() == ErrorKind::WouldBlock {
                            "the network is busy: a datagram was not sent".to_string()
                        } else {
                            e.to_string()
                        }
                    })
                };
                match r {
                    Ok(n) => {
                        out.sent += 1;
                        bytes += n as u64;
                    }
                    Err(e) => {
                        out.dropped += 1;
                        out.reason = Some(e);
                    }
                }
            }
            let mut st = lock(&self.link.status);
            st.bytes_out += bytes;
            st.messages_out += out.sent as u64;
            st.dropped_out += out.dropped as u64;
            if out.reason.is_some() {
                st.last_drop.clone_from(&out.reason);
            }
            return out;
        }
        let mut payload = Vec::new();
        for m in messages {
            payload.extend_from_slice(m);
            payload.extend_from_slice(delimiter);
        }
        let cap = self.link.queue_cap();
        if payload.len() > cap {
            out.dropped = messages.len();
            out.reason = Some(match self.link.endpoint {
                Endpoint::Serial { .. } => format!(
                    "{} bytes at once take more than a second at {} baud",
                    payload.len(),
                    self.link.baud.load(Ordering::Relaxed)
                ),
                _ => format!("{} bytes at once is more than a connection queues", payload.len()),
            });
        } else {
            let mut boxes = lock(&self.link.outboxes);
            let mut reached = 0;
            let mut full = 0;
            for b in boxes.values_mut() {
                if b.bytes.len() + payload.len() > cap {
                    full += 1;
                } else {
                    b.bytes.extend(payload.iter().copied());
                    reached += 1;
                }
            }
            drop(boxes);
            if reached > 0 {
                out.sent = messages.len();
            } else {
                out.dropped = messages.len();
            }
            if full > 0 {
                out.full = true;
                out.reason = Some(if reached > 0 {
                    format!(
                        "{full} of {} connections do not read fast enough: messages dropped there",
                        full + reached
                    )
                } else {
                    "the other side does not read fast enough: messages dropped".to_string()
                });
            } else if reached == 0 {
                out.unconnected = true;
                out.reason = Some("not connected".to_string());
            }
            if let Some(w) = lock(&self.link.writers).get_mut(&self.id) {
                w.latest = payload;
            }
        }
        let mut st = lock(&self.link.status);
        st.messages_out += out.sent as u64;
        st.dropped_out += out.dropped as u64;
        if out.reason.is_some() {
            st.last_drop.clone_from(&out.reason);
        }
        out
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        lock(&self.link.writers).remove(&self.id);
        release(&self.hub, &self.link);
    }
}

impl Link {
    /// Bytes one connection may queue: about a second of bytes for a serial port (10 bits a
    /// byte), else [`MAX_QUEUED`].
    fn queue_cap(&self) -> usize {
        match self.endpoint {
            Endpoint::Serial { .. } => usize::try_from(self.baud.load(Ordering::Relaxed) / 10)
                .unwrap_or(MAX_QUEUED)
                .clamp(64, MAX_QUEUED),
            _ => MAX_QUEUED,
        }
    }

    /// A serial port opens again at the rate of the oldest handle when none asks for the rate in
    /// use any more.
    fn rebalance_baud(&self) {
        if !matches!(self.endpoint, Endpoint::Serial { .. }) {
            return;
        }
        let mut asked: Vec<(u64, u32)> = lock(&self.readers).values().map(|r| (r.order, r.baud)).collect();
        asked.extend(lock(&self.writers).values().map(|w| (w.order, w.baud)));
        asked.sort();
        let current = self.baud.load(Ordering::Acquire);
        if asked.is_empty() || asked.iter().any(|(_, b)| *b == current) {
            return;
        }
        if let Some((_, b)) = asked.first() {
            self.baud.store(*b, Ordering::Release);
            self.reopen.store(true, Ordering::Release);
        }
    }

    /// The port a listener got.
    fn bound_port(&self) -> Option<u16> {
        let st = lock(&self.status);
        st.local.as_deref().and_then(|l| l.parse::<SocketAddr>().ok()).map(|a| a.port())
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn status(&self) -> Status {
        let mut st = lock(&self.status).clone();
        let mut readers: Vec<(u64, String)> =
            lock(&self.readers).values().map(|r| (r.order, r.label.clone())).collect();
        readers.sort();
        st.readers = readers.into_iter().map(|(_, l)| l).collect();
        let mut writers: Vec<(u64, String)> =
            lock(&self.writers).values().map(|w| (w.order, w.label.clone())).collect();
        writers.sort();
        st.writers = writers.into_iter().map(|(_, l)| l).collect();
        st
    }

    /// Changes the status; wakes the readers when `open` or the error changed.
    fn set_status(&self, f: impl FnOnce(&mut Status)) {
        let changed = {
            let mut st = lock(&self.status);
            let before = (st.open, st.error.clone(), st.peers.len());
            f(&mut st);
            before != (st.open, st.error.clone(), st.peers.len())
        };
        if changed {
            self.wake_all();
        }
    }

    fn wake_all(&self) {
        let wakers: Vec<_> = lock(&self.readers).values().map(|r| r.wake.clone()).collect();
        for w in wakers {
            w();
        }
    }

    /// Sleeps up to `d`, waking early on stop. Whether to go on.
    fn wait(&self, d: Duration) -> bool {
        let until = Instant::now() + d;
        while Instant::now() < until {
            if self.stopped() {
                return false;
            }
            std::thread::sleep(
                Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
            );
        }
        !self.stopped()
    }

    /// Bytes from connection `conn` (a datagram when `datagram`). Framing (cheap) happens under
    /// the readers' lock, parsing outside it, so a cook reading a store never waits for a parse.
    fn incoming(&self, conn: u64, bytes: &[u8], datagram: bool) {
        // Per reader: its decode as it was, and the messages cut for it.
        let mut cut: Vec<(u64, Decode, Vec<Vec<u8>>, u64)> = Vec::new();
        {
            let mut readers = lock(&self.readers);
            for (id, r) in readers.iter_mut() {
                if datagram {
                    cut.push((*id, r.decode.clone(), vec![bytes.to_vec()], 0));
                } else {
                    let decode = &r.decode;
                    let framer = r.framers.entry(conn).or_insert_with(|| decode.framer());
                    let mut frames = Frames::default();
                    framer.push(bytes, &mut frames);
                    if !frames.messages.is_empty() || frames.too_long > 0 {
                        cut.push((*id, r.decode.clone(), frames.messages, frames.too_long));
                    }
                }
            }
        }
        let parsed: Vec<(u64, Decode, Vec<Parsed>, u64)> = cut
            .into_iter()
            .map(|(id, d, messages, too_long)| {
                let p = if datagram {
                    messages.iter().flat_map(|m| parse::parse_datagram(m, &d)).collect()
                } else {
                    messages.iter().map(|m| parse::parse(m, &d)).collect()
                };
                (id, d, p, too_long)
            })
            .collect();
        let mut wakers = Vec::new();
        {
            let mut readers = lock(&self.readers);
            for (id, d, messages, too_long) in parsed {
                // A reader that left or changed its decode meanwhile does not take these.
                let Some(r) = readers.get_mut(&id).filter(|r| r.decode == d) else { continue };
                let before = r.store.version;
                for p in messages {
                    r.store.apply_parsed(p, &d);
                }
                r.store.too_long(too_long);
                if r.store.version != before && !r.woken.swap(true, Ordering::AcqRel) {
                    wakers.push(r.wake.clone());
                }
            }
        }
        lock(&self.status).bytes_in += bytes.len() as u64;
        for w in wakers {
            w();
        }
    }

    /// Opens an outbox for a new connection, holding each writer's latest message. The outboxes
    /// are locked first, so a send at the same time reaches either the latest or the new box.
    fn open_conn(&self) -> u64 {
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let cap = self.queue_cap();
        let mut boxes = lock(&self.outboxes);
        let mut b = Outbox::default();
        for w in lock(&self.writers).values() {
            if b.bytes.len() + w.latest.len() <= cap {
                b.bytes.extend(w.latest.iter().copied());
            }
        }
        boxes.insert(conn, b);
        conn
    }

    /// Forgets a connection: its outbox and the messages begun on it.
    fn close_conn(&self, conn: u64) {
        lock(&self.outboxes).remove(&conn);
        for r in lock(&self.readers).values_mut() {
            r.framers.remove(&conn);
        }
    }
}

fn run(link: &Arc<Link>) {
    // A panic in here must not take the process along; the link then stays closed.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &link.endpoint {
        Endpoint::Udp { lan, port } => udp_in(link, *lan, *port),
        Endpoint::UdpTo(_) => {}
        Endpoint::TcpListen { lan, port } => tcp_listen(link, *lan, *port),
        Endpoint::TcpConnect(to) => tcp_connect(link, *to),
        Endpoint::Serial { name, .. } => serial(link, name),
    }));
    if outcome.is_err() {
        link.set_status(|st| {
            st.open = false;
            st.error = Some(
                "this connection failed; it starts again when every node using it changes its params or is removed"
                    .to_string(),
            );
        });
    }
}

fn bind_ip(lan: bool) -> Ipv4Addr {
    if lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST }
}

fn udp_in(link: &Link, lan: bool, port: u16) {
    let mut buf = vec![0u8; MAX_DATAGRAM + 1];
    while !link.stopped() {
        let socket = match UdpSocket::bind((bind_ip(lan), port)) {
            Ok(s) => s,
            Err(e) => {
                link.set_status(|st| {
                    st.open = false;
                    st.error = Some(format!("cannot listen on UDP port {port}: {e}"));
                });
                if !link.wait(RETRY) {
                    return;
                }
                continue;
            }
        };
        socket.set_read_timeout(Some(UDP_POLL)).ok();
        let local = socket.local_addr().ok().map(|a| a.to_string());
        link.set_status(|st| {
            st.open = true;
            st.error = None;
            st.local = local;
            st.connects += 1;
        });
        while !link.stopped() {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    link.incoming(0, buf.get(..n).unwrap_or_default(), true);
                    let mut st = lock(&link.status);
                    if st.peers.first() != Some(&from.to_string()) {
                        st.peers = vec![from.to_string()];
                    }
                }
                Err(e) if is_timeout(&e) => {}
                // Windows reports an earlier send's "port unreachable" on the next receive.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => {}
                Err(e) => {
                    link.set_status(|st| {
                        st.open = false;
                        st.error = Some(format!("receiving on UDP port {port}: {e}"));
                    });
                    break;
                }
            }
        }
        if !link.wait(RETRY) {
            return;
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

/// Pumps one connection until it ends or the link stops: reads into the readers, writes what
/// the writers queued. `Ok` when the link stopped.
fn pump<S: Read + Write>(link: &Link, conn: u64, s: &mut S) -> Result<(), String> {
    let mut buf = vec![0u8; READ_CHUNK];
    let mut chunk: Vec<u8> = Vec::new();
    loop {
        if link.stopped() || link.reopen.load(Ordering::Acquire) {
            return Ok(());
        }
        // Write what waits, for a short while at most, so reading goes on even while a slow
        // serial port has a full queue.
        let started = Instant::now();
        while started.elapsed() < WRITE_BUDGET {
            if chunk.is_empty() {
                let mut boxes = lock(&link.outboxes);
                let Some(b) = boxes.get_mut(&conn) else { break };
                let n = b.bytes.len().min(READ_CHUNK);
                chunk.extend(b.bytes.drain(..n));
            }
            if chunk.is_empty() {
                break;
            }
            match s.write(&chunk) {
                Ok(0) => return Err("the other side closed the connection".to_string()),
                Ok(n) => {
                    chunk.drain(..n.min(chunk.len()));
                    lock(&link.status).bytes_out += n as u64;
                }
                Err(e) if is_timeout(&e) => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        match s.read(&mut buf) {
            Ok(0) => return Err("the other side closed the connection".to_string()),
            Ok(n) => link.incoming(conn, buf.get(..n).unwrap_or_default(), false),
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn tcp_listen(link: &Arc<Link>, lan: bool, port: u16) {
    let mut clients: Vec<(u64, String, JoinHandle<()>)> = Vec::new();
    'bind: while !link.stopped() {
        let listener = match TcpListener::bind((bind_ip(lan), port)).and_then(|l| {
            l.set_nonblocking(true)?;
            Ok(l)
        }) {
            Ok(l) => l,
            Err(e) => {
                link.set_status(|st| {
                    st.open = false;
                    st.error = Some(format!("cannot listen on TCP port {port}: {e}"));
                });
                if !link.wait(RETRY) {
                    break 'bind;
                }
                continue;
            }
        };
        let local = listener.local_addr().ok().map(|a| a.to_string());
        link.set_status(|st| {
            st.open = true;
            st.error = None;
            st.local = local;
        });
        while !link.stopped() {
            let before = clients.len();
            clients.retain(|(_, _, h)| !h.is_finished());
            if clients.len() != before {
                let peers: Vec<String> = clients.iter().map(|(_, p, _)| p.clone()).collect();
                link.set_status(|st| st.peers = peers);
            }
            match listener.accept() {
                Ok((stream, peer)) => {
                    if clients.len() >= MAX_CLIENTS {
                        drop(stream);
                        lock(&link.status).refused += 1;
                        continue;
                    }
                    let conn = link.open_conn();
                    let l = link.clone();
                    let spawned =
                        std::thread::Builder::new().name(format!("net client {peer}")).spawn(move || {
                            let mut stream = stream;
                            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if setup(&stream).is_ok() {
                                    pump(&l, conn, &mut stream).ok();
                                }
                            }));
                            drop(outcome);
                            l.close_conn(conn);
                        });
                    match spawned {
                        Ok(h) => {
                            clients.push((conn, peer.to_string(), h));
                            let peers: Vec<String> = clients.iter().map(|(_, p, _)| p.clone()).collect();
                            link.set_status(|st| {
                                st.peers = peers;
                                st.connects += 1;
                            });
                        }
                        Err(_) => link.close_conn(conn),
                    }
                }
                Err(e) if is_timeout(&e) => std::thread::sleep(POLL),
                // A client that went away before it was taken; others keep coming.
                Err(_) => std::thread::sleep(POLL),
            }
        }
    }
    for (_, _, h) in clients {
        h.join().ok();
    }
}

fn setup(s: &TcpStream) -> std::io::Result<()> {
    s.set_nonblocking(false)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(POLL))?;
    s.set_write_timeout(Some(WRITE_TIMEOUT))?;
    Ok(())
}

/// Keeps one connection open: `open` makes it, [`pump`] runs it, and after it ends or fails to
/// open the next try waits with backoff.
fn reconnecting<S: Read + Write>(
    link: &Link,
    mut open: impl FnMut() -> Result<(S, String, Option<u32>), String>,
) {
    let mut backoff = BACKOFF_MIN;
    while !link.stopped() {
        match open() {
            Ok((mut s, peer, baud)) => {
                let conn = link.open_conn();
                link.set_status(|st| {
                    st.open = true;
                    st.error = None;
                    st.peers = vec![peer];
                    st.baud = baud;
                    st.connects += 1;
                });
                let outcome = pump(link, conn, &mut s);
                link.close_conn(conn);
                drop(s);
                link.set_status(|st| {
                    st.open = false;
                    st.peers.clear();
                    if let Err(e) = outcome {
                        st.error = Some(e);
                    }
                });
                // A connection that worked starts the backoff over; a new bit rate opens at once.
                backoff = BACKOFF_MIN;
                if link.reopen.swap(false, Ordering::AcqRel) {
                    continue;
                }
            }
            Err(e) => {
                link.set_status(|st| {
                    st.open = false;
                    st.error = Some(e);
                });
            }
        }
        if !link.wait(backoff) {
            return;
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn tcp_connect(link: &Link, to: SocketAddr) {
    reconnecting(link, || {
        let s = TcpStream::connect_timeout(&to, CONNECT_TIMEOUT)
            .map_err(|e| format!("cannot connect to {to}: {e}"))?;
        setup(&s).map_err(|e| format!("cannot set up the connection to {to}: {e}"))?;
        Ok((s, to.to_string(), None))
    });
}

/// The serial ports of this machine by name.
pub fn serial_ports() -> Result<Vec<String>, String> {
    // Without libudev, the Linux listing `expect`s `/sys/class/tty` to exist.
    let list = std::panic::catch_unwind(serialport::available_ports)
        .map_err(|_| "cannot list the serial ports".to_string())?
        .map_err(|e| format!("cannot list the serial ports: {e}"))?;
    let mut names: Vec<String> = list.into_iter().map(|p| p.port_name).collect();
    names.sort();
    Ok(names)
}

/// The port to open: `name`, or the only one there is when it is empty.
fn serial_name(name: &str) -> Result<String, String> {
    if !name.is_empty() {
        return Ok(name.to_string());
    }
    let ports = serial_ports()?;
    match ports.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("no serial port found: plug in the device".to_string()),
        many => Err(format!("{} serial ports ({}): name one in `serial`", many.len(), many.join(", "))),
    }
}

fn serial(link: &Link, name: &str) {
    reconnecting(link, || {
        link.reopen.store(false, Ordering::Release);
        let baud = link.baud.load(Ordering::Acquire);
        let name = serial_name(name.trim())?;
        let mut port = serialport::new(&name, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(POLL)
            .open()
            .map_err(|e| format!("cannot open {name}: {e}"))?;
        // Boards with native USB (Arduino Leonardo, many ESP32) only send while DTR is set.
        port.write_data_terminal_ready(true).ok();
        Ok((port, name, Some(baud)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_share_a_serial_port_whatever_the_rate() {
        let a = Endpoint::Serial { name: " COM3".into(), baud: 9600 };
        let b = Endpoint::Serial { name: "COM3".into(), baud: 115_200 };
        assert_eq!(a.key(), b.key());
        if cfg!(windows) {
            assert_eq!(Endpoint::Serial { name: "com3".into(), baud: 1 }.key(), b.key());
        }
        assert!(Endpoint::Udp { lan: false, port: 1 }.datagrams());
        assert!(!Endpoint::TcpConnect("127.0.0.1:1".parse().unwrap()).datagrams());
        assert_eq!(b.to_string(), "serial COM3");
    }

    #[test]
    fn a_serial_link_takes_the_rate_of_the_oldest_handle_when_its_own_leaves() {
        let hub = Hub::new();
        let w = |name: &str, baud| {
            hub.writer(name, Endpoint::Serial { name: "COM_PENUMBRA_NONE".into(), baud }).unwrap()
        };
        let a = w("a", 9600);
        let b = w("b", 115_200);
        let c = w("c", 115_200);
        assert_eq!(a.link.baud.load(Ordering::Relaxed), 9600);
        // A serial queue holds about a second of bytes.
        assert_eq!(a.link.queue_cap(), 960);
        drop(a);
        assert_eq!(b.link.baud.load(Ordering::Relaxed), 115_200);
        drop(b);
        assert_eq!(c.link.baud.load(Ordering::Relaxed), 115_200);
        drop(c);
        hub.shutdown();
    }
}
