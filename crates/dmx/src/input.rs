//! DMX in: listeners for Art-Net and sACN, each with a thread that keeps the latest frame of
//! every universe somebody asked for.
//!
//! A node asks the [`Inputs`] for an [`Input`] on a [`Listen`] (protocol, interface, port) for
//! some universes and reads their slots ([`Input::read`]); inputs on the same listen share one
//! socket and thread, and the last one dropped stops it. The thread calls an input's waker when
//! one of its universes changes, so a node cooks when something arrived and not every frame.
//!
//! - **Merging:** per universe every source (the sender's address for Art-Net, its CID for
//!   sACN) keeps its latest frame. The universe is the highest takes precedence merge of the live
//!   sources with the highest priority (Art-Net has no priorities; its sources count as
//!   100). A source is live until it falls silent for [`SACN_TIMEOUT`] (sACN) or
//!   [`ART_NET_TIMEOUT`] (Art-Net's merge timeout), or says it terminates (sACN). With no live
//!   source the last frame is held, as most fixtures do on lost signal.
//! - **Order:** packets whose sequence number lies up to 20 behind the source's last are late
//!   and dropped, as E1.31 asks; Art-Net's sequence 0 switches that off.
//! - **Hostile input:** anything that is not a data packet is counted and dropped, universes
//!   nobody asked for are not kept, and a universe tracks at most [`MAX_SOURCES`] sources.
//! - **sACN multicast:** the thread joins the group of every universe asked for, on the
//!   listen's interface (the system's default one for all interfaces); unicast packets arrive
//!   either way.
//!
//! Nothing here blocks the caller. A port that cannot be bound (another program holds it) is
//! tried again every second.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

use crate::{SLOTS, artnet, sacn};

/// Sources tracked per universe; further ones are ignored until one falls silent.
pub const MAX_SOURCES: usize = 16;
/// Universes one input may ask for.
pub const MAX_UNIVERSES: usize = 64;
/// An sACN source counts as gone after this long without a packet (E1.31's data loss timeout).
pub const SACN_TIMEOUT: Duration = Duration::from_millis(2500);
/// An Art-Net source counts as gone after this long without a packet (Art-Net's merge timeout).
pub const ART_NET_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the thread blocks in one receive (and so how fast it notices a stop or a timeout).
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
/// How often a port that could not be bound is tried again.
const RETRY: Duration = Duration::from_secs(1);

/// The protocols that can be received.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    ArtNet,
    Sacn,
}

impl Protocol {
    /// The protocol's own UDP port.
    pub fn port(self) -> u16 {
        match self {
            Protocol::ArtNet => artnet::PORT,
            Protocol::Sacn => sacn::PORT,
        }
    }

    /// The first and the last universe, as the protocol counts.
    pub fn universes(self) -> (u16, u16) {
        match self {
            Protocol::ArtNet => (0, artnet::MAX_UNIVERSE),
            Protocol::Sacn => (1, sacn::MAX_UNIVERSE),
        }
    }

    fn timeout(self) -> Duration {
        match self {
            Protocol::ArtNet => ART_NET_TIMEOUT,
            Protocol::Sacn => SACN_TIMEOUT,
        }
    }
}

/// Where a listener listens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Listen {
    pub protocol: Protocol,
    /// The address of the interface to bind; unspecified (0.0.0.0) is every interface.
    pub interface: Ipv4Addr,
    pub port: u16,
}

/// One sender of a universe, as the status shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceStatus {
    /// Where its last packet came from.
    pub from: String,
    pub priority: u8,
    /// Its frames count (live and among the highest priority).
    pub active: bool,
}

/// One universe, as the status shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UniverseStatus {
    pub universe: u16,
    /// Packets taken in.
    pub packets: u64,
    /// The live sources.
    pub sources: Vec<SourceStatus>,
}

/// What a listener did lately.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub listen: Listen,
    /// The readers: their labels (node paths), in the order they came.
    pub readers: Vec<String>,
    /// Whether the socket is bound.
    pub open: bool,
    /// The last error binding the socket, cleared when it is bound.
    pub error: Option<String>,
    /// Joining a multicast group failed (sACN); unicast still arrives.
    pub multicast: Option<String>,
    /// Packets taken in, for universes asked for.
    pub packets: u64,
    /// Packets dropped: not data, late, for universes nobody asked for, or from a source too many.
    pub dropped: u64,
    pub universes: Vec<UniverseStatus>,
}

/// What an input reads of one universe.
#[derive(Clone, Debug, PartialEq)]
pub struct Universe {
    pub universe: u16,
    /// The merged slots (always 512), or `None` while nothing arrived.
    pub slots: Option<Vec<u8>>,
    /// Some source is live; `false` with slots means the last frame is held.
    pub live: bool,
}

/// What [`Input::read`] returns.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub open: bool,
    pub error: Option<String>,
    pub multicast: Option<String>,
    /// The universes asked for, in order.
    pub universes: Vec<Universe>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceKey {
    Addr(SocketAddr),
    Cid([u8; 16]),
}

struct Source {
    key: SourceKey,
    from: SocketAddr,
    priority: u8,
    sequence: u8,
    slots: Vec<u8>,
    at: Instant,
}

#[derive(Default)]
struct UniverseState {
    sources: Vec<Source>,
    /// The merged slots, held when every source fell silent.
    merged: Option<Vec<u8>>,
    packets: u64,
}

impl UniverseState {
    fn live(&self) -> bool {
        !self.sources.is_empty()
    }

    /// Merges again; whether the slots or liveness changed.
    fn remerge(&mut self, was_live: bool) -> bool {
        let Some(top) = self.sources.iter().map(|s| s.priority).max() else {
            return was_live;
        };
        let mut merged = vec![0u8; SLOTS];
        for s in self.sources.iter().filter(|s| s.priority == top) {
            for (m, v) in merged.iter_mut().zip(&s.slots) {
                *m = (*m).max(*v);
            }
        }
        let changed = self.merged.as_ref() != Some(&merged) || !was_live;
        self.merged = Some(merged);
        changed
    }
}

/// One reader's wish.
struct Want {
    label: String,
    universes: BTreeSet<u16>,
    wake: Arc<dyn Fn() + Send + Sync>,
    order: u64,
}

struct Listener {
    listen: Listen,
    wants: Mutex<BTreeMap<u64, Want>>,
    /// Counts changes of `wants`, so the thread joins and leaves groups only after one.
    version: AtomicU64,
    universes: Mutex<BTreeMap<u16, UniverseState>>,
    stop: AtomicBool,
    status: Mutex<Status>,
}

type Running = (Arc<Listener>, Option<JoinHandle<()>>);

struct Inner {
    listeners: Mutex<BTreeMap<Listen, Running>>,
    /// Listeners whose last input was dropped, until their thread has ended.
    stopping: Mutex<Vec<(Listen, JoinHandle<()>)>>,
    next: AtomicU64,
}

/// All DMX inputs of the process. Cheap to clone.
#[derive(Clone)]
pub struct Inputs {
    inner: Arc<Inner>,
}

impl Default for Inputs {
    fn default() -> Self {
        Self::new()
    }
}

impl Inputs {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                listeners: Mutex::new(BTreeMap::new()),
                stopping: Mutex::new(Vec::new()),
                next: AtomicU64::new(1),
            }),
        }
    }

    /// An input on `listen`, starting the listener's thread if it is the first. `label` names
    /// the reader in the status (a node path); `wake` is called when one of its universes
    /// changes or the listener opens or fails. Ask for the universes with [`Input::want`].
    pub fn input(
        &self,
        label: &str,
        listen: Listen,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Input, String> {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let mut listeners = lock(&self.inner.listeners);
        let listener = match listeners.get(&listen) {
            Some((l, _)) => l.clone(),
            None => {
                // A listener on the same port that is still ending holds it: wait for it (it
                // looks at its stop flag at least every 100 ms).
                let old: Vec<JoinHandle<()>> = {
                    let mut stopping = lock(&self.inner.stopping);
                    let (same, rest): (Vec<_>, Vec<_>) = stopping.drain(..).partition(|(l, _)| *l == listen);
                    *stopping = rest;
                    same.into_iter().map(|(_, h)| h).collect()
                };
                for h in old {
                    h.join().ok();
                }
                let l = Arc::new(Listener::new(listen));
                let run_l = l.clone();
                let handle = std::thread::Builder::new()
                    .name(format!("dmx in {}", describe(&listen)))
                    .spawn(move || run(&run_l))
                    .map_err(|e| format!("cannot start the DMX input thread: {e}"))?;
                listeners.insert(listen, (l.clone(), Some(handle)));
                l
            }
        };
        lock(&listener.wants)
            .insert(id, Want { label: label.to_string(), universes: BTreeSet::new(), wake, order: id });
        listener.version.fetch_add(1, Ordering::Release);
        Ok(Input { listener, id, hub: Arc::downgrade(&self.inner) })
    }

    /// Every listener's status, by listen.
    pub fn status(&self) -> Vec<Status> {
        lock(&self.inner.listeners).values().map(|(l, _)| l.status()).collect()
    }

    /// Stops every listener and waits for the threads.
    pub fn shutdown(&self) {
        let listeners = std::mem::take(&mut *lock(&self.inner.listeners));
        let stopping = std::mem::take(&mut *lock(&self.inner.stopping));
        for (l, _) in listeners.values() {
            l.stop.store(true, Ordering::Release);
        }
        for (_, h) in listeners.into_values() {
            if let Some(h) = h {
                h.join().ok();
            }
        }
        for (_, h) in stopping {
            h.join().ok();
        }
    }
}

/// One reader's way into a listener. Dropping it forgets its universes; the last one stops the
/// listener.
pub struct Input {
    listener: Arc<Listener>,
    id: u64,
    hub: Weak<Inner>,
}

impl Input {
    pub fn listen(&self) -> Listen {
        self.listener.listen
    }

    /// Asks for these universes (at most [`MAX_UNIVERSES`], the first ones kept).
    pub fn want(&self, universes: &BTreeSet<u16>) {
        let mut wants = lock(&self.listener.wants);
        let Some(w) = wants.get_mut(&self.id) else { return };
        if w.universes == *universes {
            return;
        }
        w.universes = universes.iter().copied().take(MAX_UNIVERSES).collect();
        drop(wants);
        self.listener.version.fetch_add(1, Ordering::Release);
    }

    /// The universes asked for, as they are now.
    pub fn read(&self) -> Reading {
        let wanted: Vec<u16> = lock(&self.listener.wants)
            .get(&self.id)
            .map(|w| w.universes.iter().copied().collect())
            .unwrap_or_default();
        let universes = {
            let states = lock(&self.listener.universes);
            wanted
                .into_iter()
                .map(|u| {
                    let s = states.get(&u);
                    Universe {
                        universe: u,
                        slots: s.and_then(|s| s.merged.clone()),
                        live: s.is_some_and(UniverseState::live),
                    }
                })
                .collect()
        };
        let st = lock(&self.listener.status);
        Reading { open: st.open, error: st.error.clone(), multicast: st.multicast.clone(), universes }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let empty = {
            let mut wants = lock(&self.listener.wants);
            wants.remove(&self.id);
            wants.is_empty()
        };
        self.listener.version.fetch_add(1, Ordering::Release);
        if !empty {
            return;
        }
        let Some(inner) = self.hub.upgrade() else { return };
        let mut listeners = lock(&inner.listeners);
        // Another input may have joined between the two locks; only an empty listener stops.
        if lock(&self.listener.wants).is_empty()
            && let Some((l, handle)) = listeners.remove(&self.listener.listen)
        {
            l.stop.store(true, Ordering::Release);
            if let Some(h) = handle {
                lock(&inner.stopping).push((l.listen, h));
            }
        }
    }
}

impl Listener {
    fn new(listen: Listen) -> Self {
        let status = Status {
            listen,
            readers: Vec::new(),
            open: false,
            error: None,
            multicast: None,
            packets: 0,
            dropped: 0,
            universes: Vec::new(),
        };
        Self {
            listen,
            wants: Mutex::new(BTreeMap::new()),
            version: AtomicU64::new(0),
            universes: Mutex::new(BTreeMap::new()),
            stop: AtomicBool::new(false),
            status: Mutex::new(status),
        }
    }

    fn status(&self) -> Status {
        let mut st = lock(&self.status).clone();
        let mut readers: Vec<(u64, String)> =
            lock(&self.wants).values().map(|w| (w.order, w.label.clone())).collect();
        readers.sort();
        st.readers = readers.into_iter().map(|(_, l)| l).collect();
        st.universes = lock(&self.universes)
            .iter()
            .map(|(&universe, s)| {
                let top = s.sources.iter().map(|s| s.priority).max();
                UniverseStatus {
                    universe,
                    packets: s.packets,
                    sources: s
                        .sources
                        .iter()
                        .map(|src| SourceStatus {
                            from: src.from.to_string(),
                            priority: src.priority,
                            active: Some(src.priority) == top,
                        })
                        .collect(),
                }
            })
            .collect();
        st
    }

    /// Calls the wakers of the readers that asked for any of `universes` (all of them for
    /// `None`).
    fn wake(&self, universes: Option<&BTreeSet<u16>>) {
        let wakers: Vec<_> = lock(&self.wants)
            .values()
            .filter(|w| universes.is_none_or(|u| !w.universes.is_disjoint(u)))
            .map(|w| w.wake.clone())
            .collect();
        for w in wakers {
            w();
        }
    }
}

fn run(l: &Arc<Listener>) {
    // A panic in here must not take the process along; the listener then stays closed.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listen(l)));
    if outcome.is_err() {
        {
            let mut st = lock(&l.status);
            st.open = false;
            st.error = Some("the DMX input thread failed; remove and add the node to restart it".to_string());
        }
        l.wake(None);
    }
}

fn listen(l: &Arc<Listener>) {
    let mut socket: Option<UdpSocket> = None;
    let mut retry_at: Option<Instant> = None;
    let mut seen = u64::MAX;
    let mut wanted: BTreeSet<u16> = BTreeSet::new();
    let mut joined: BTreeSet<u16> = BTreeSet::new();
    let mut buf = vec![0u8; 2048];
    while !l.stop.load(Ordering::Acquire) {
        if socket.is_none() && retry_at.is_none_or(|t| Instant::now() >= t) {
            match bind(&l.listen) {
                Ok(s) => {
                    socket = Some(s);
                    retry_at = None;
                    joined.clear();
                    seen = u64::MAX;
                    {
                        let mut st = lock(&l.status);
                        st.open = true;
                        st.error = None;
                    }
                    l.wake(None);
                }
                Err(e) => {
                    retry_at = Some(Instant::now() + RETRY);
                    let changed = {
                        let mut st = lock(&l.status);
                        let changed = st.error.as_deref() != Some(e.as_str());
                        st.open = false;
                        st.error = Some(e);
                        changed
                    };
                    if changed {
                        l.wake(None);
                    }
                }
            }
        }
        let Some(s) = socket.as_ref() else {
            std::thread::sleep(RECV_TIMEOUT);
            continue;
        };
        let version = l.version.load(Ordering::Acquire);
        if version != seen {
            seen = version;
            wanted = lock(&l.wants).values().flat_map(|w| w.universes.iter().copied()).collect();
            // Forget universes nobody asks for any more.
            lock(&l.universes).retain(|u, _| wanted.contains(u));
            if l.listen.protocol == Protocol::Sacn {
                let error = rejoin(s, &l.listen, &wanted, &mut joined);
                lock(&l.status).multicast = error;
            }
        }
        match s.recv_from(&mut buf) {
            Ok((n, from)) => {
                let packet = buf.get(..n).unwrap_or_default();
                let changed = take(l, &wanted, packet, from);
                if let Some(u) = changed {
                    l.wake(Some(&BTreeSet::from([u])));
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            // Windows reports an ICMP "port unreachable" for an earlier send on the next receive;
            // nothing here sends, but a stray one must not close the socket.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => {
                socket = None;
                retry_at = Some(Instant::now() + RETRY);
                {
                    let mut st = lock(&l.status);
                    st.open = false;
                    st.error = Some(format!("receiving failed: {e}"));
                }
                l.wake(None);
                continue;
            }
        }
        let gone = expire(l, Instant::now());
        if !gone.is_empty() {
            l.wake(Some(&gone));
        }
    }
}

fn bind(listen: &Listen) -> Result<UdpSocket, String> {
    let s = UdpSocket::bind((listen.interface, listen.port)).map_err(|e| {
        let what = match listen.protocol {
            Protocol::ArtNet => "Art-Net",
            Protocol::Sacn => "sACN",
        };
        format!(
            "cannot listen for {what} on {}:{} ({e}); another program may hold the port",
            listen.interface, listen.port
        )
    })?;
    s.set_read_timeout(Some(RECV_TIMEOUT)).map_err(|e| format!("cannot set up the socket: {e}"))?;
    Ok(s)
}

/// Joins the multicast groups of `wanted` and leaves the rest; the first error, if any. Not on
/// the loopback interface, which carries no multicast.
fn rejoin(
    s: &UdpSocket,
    listen: &Listen,
    wanted: &BTreeSet<u16>,
    joined: &mut BTreeSet<u16>,
) -> Option<String> {
    if listen.interface.is_loopback() {
        return None;
    }
    let mut error = None;
    for &u in joined.difference(wanted) {
        s.leave_multicast_v4(&sacn::multicast(u), &listen.interface).ok();
    }
    joined.retain(|u| wanted.contains(u));
    for &u in wanted {
        if joined.contains(&u) {
            continue;
        }
        match s.join_multicast_v4(&sacn::multicast(u), &listen.interface) {
            Ok(()) => {
                joined.insert(u);
            }
            Err(e) => {
                if error.is_none() {
                    error = Some(format!("cannot join the multicast group of universe {u}: {e}"));
                }
            }
        }
    }
    error
}

/// What one packet says, whatever the protocol.
struct Packet<'a> {
    key: SourceKey,
    universe: u16,
    priority: u8,
    sequence: Option<u8>,
    terminated: bool,
    data: &'a [u8],
}

fn parse(protocol: Protocol, p: &[u8], from: SocketAddr) -> Option<Packet<'_>> {
    match protocol {
        Protocol::ArtNet => {
            let d = artnet::parse_dmx(p)?;
            Some(Packet {
                key: SourceKey::Addr(from),
                universe: d.universe,
                priority: sacn::DEFAULT_PRIORITY,
                sequence: (d.sequence != 0).then_some(d.sequence),
                terminated: false,
                data: d.data,
            })
        }
        Protocol::Sacn => {
            let d = sacn::parse_data(p)?;
            if d.options & sacn::PREVIEW != 0 {
                return None;
            }
            Some(Packet {
                key: SourceKey::Cid(d.cid),
                universe: d.universe,
                priority: d.priority,
                sequence: Some(d.sequence),
                terminated: d.options & sacn::TERMINATED != 0,
                data: d.data,
            })
        }
    }
}

/// Whether `new` comes too late after `last`: up to 20 behind, or the same (E1.31 6.7.2).
fn late(last: u8, new: u8) -> bool {
    let diff = i8::from_ne_bytes(new.wrapping_sub(last).to_ne_bytes());
    diff <= 0 && diff > -20
}

/// Takes one datagram in; the universe whose slots or liveness changed, if any.
fn take(l: &Listener, wanted: &BTreeSet<u16>, packet: &[u8], from: SocketAddr) -> Option<u16> {
    let now = Instant::now();
    let dropped = || lock(&l.status).dropped += 1;
    let Some(p) = parse(l.listen.protocol, packet, from) else {
        dropped();
        return None;
    };
    if !wanted.contains(&p.universe) {
        dropped();
        return None;
    }
    let mut universes = lock(&l.universes);
    let u = universes.entry(p.universe).or_default();
    let was_live = u.live();
    let at = u.sources.iter().position(|s| s.key == p.key);
    if p.terminated {
        if let Some(i) = at {
            u.sources.remove(i);
        }
        lock(&l.status).packets += 1;
        return u.remerge(was_live).then_some(p.universe);
    }
    let full = u.sources.len() >= MAX_SOURCES;
    match at.and_then(|i| u.sources.get_mut(i)) {
        Some(s) => {
            if let Some(seq) = p.sequence
                && late(s.sequence, seq)
            {
                drop(universes);
                dropped();
                return None;
            }
            s.from = from;
            s.priority = p.priority;
            s.sequence = p.sequence.unwrap_or(0);
            s.slots.clear();
            s.slots.extend_from_slice(p.data);
            s.at = now;
        }
        None if full => {
            drop(universes);
            dropped();
            return None;
        }
        None => u.sources.push(Source {
            key: p.key,
            from,
            priority: p.priority,
            sequence: p.sequence.unwrap_or(0),
            slots: p.data.to_vec(),
            at: now,
        }),
    }
    u.packets += 1;
    let changed = u.remerge(was_live);
    drop(universes);
    lock(&l.status).packets += 1;
    changed.then_some(p.universe)
}

/// Drops the sources that fell silent; the universes that changed.
fn expire(l: &Listener, now: Instant) -> BTreeSet<u16> {
    let timeout = l.listen.protocol.timeout();
    let mut changed = BTreeSet::new();
    for (&n, u) in lock(&l.universes).iter_mut() {
        let before = u.sources.len();
        if before == 0 {
            continue;
        }
        u.sources.retain(|s| now.saturating_duration_since(s.at) < timeout);
        if u.sources.len() != before && u.remerge(true) {
            changed.insert(n);
        }
    }
    changed
}

fn describe(listen: &Listen) -> String {
    let p = match listen.protocol {
        Protocol::ArtNet => "art-net",
        Protocol::Sacn => "sacn",
    };
    format!("{p} {}:{}", listen.interface, listen.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port()
    }

    fn listen(protocol: Protocol) -> Listen {
        Listen { protocol, interface: Ipv4Addr::LOCALHOST, port: free_port() }
    }

    fn counter() -> (Arc<AtomicU64>, Arc<dyn Fn() + Send + Sync>) {
        let n = Arc::new(AtomicU64::new(0));
        let m = n.clone();
        (
            n,
            Arc::new(move || {
                m.fetch_add(1, Ordering::Relaxed);
            }),
        )
    }

    fn until(what: &str, mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(3), "waited in vain for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn sender() -> UdpSocket {
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
    }

    #[test]
    fn sequence_numbers() {
        assert!(late(10, 10));
        assert!(late(10, 9));
        assert!(late(5, 250));
        assert!(!late(10, 11));
        assert!(!late(250, 3));
        assert!(!late(30, 5));
    }

    #[test]
    fn art_net_sources_merge_highest_and_wake_only_on_change() {
        let hub = Inputs::new();
        let at = listen(Protocol::ArtNet);
        let (woken, wake) = counter();
        let input = hub.input("/in", at, wake).unwrap();
        input.want(&BTreeSet::from([2]));
        until("the socket", || input.read().open);
        let to = SocketAddr::from((Ipv4Addr::LOCALHOST, at.port));
        let (a, b) = (sender(), sender());
        a.send_to(&artnet::dmx_packet(2, 1, &[10, 200, 0]), to).unwrap();
        b.send_to(&artnet::dmx_packet(2, 0, &[50, 100]), to).unwrap();
        // Universes nobody asked for are dropped.
        a.send_to(&artnet::dmx_packet(3, 2, &[1]), to).unwrap();
        until("two sources", || hub.status()[0].universes.first().is_some_and(|u| u.sources.len() == 2));
        let r = input.read();
        let u = &r.universes[0];
        assert!(u.live);
        assert_eq!(&u.slots.as_ref().unwrap()[..4], &[50, 200, 0, 0]);
        until("the drop counted", || hub.status()[0].dropped == 1);
        // The same frame again (a later sequence) changes nothing and wakes nobody.
        let before = woken.load(Ordering::Relaxed);
        a.send_to(&artnet::dmx_packet(2, 2, &[10, 200, 0]), to).unwrap();
        until("the packet", || hub.status()[0].packets == 3);
        assert_eq!(woken.load(Ordering::Relaxed), before);
        // A late packet is dropped.
        a.send_to(&artnet::dmx_packet(2, 1, &[255]), to).unwrap();
        until("the late drop", || hub.status()[0].dropped == 2);
        assert_eq!(input.read().universes[0].slots.as_ref().unwrap()[0], 50);
        assert_eq!(hub.status()[0].readers, vec!["/in"]);
        drop(input);
        assert!(hub.status().is_empty());
        hub.shutdown();
    }

    #[test]
    fn sacn_priority_termination_and_hold() {
        let hub = Inputs::new();
        let at = listen(Protocol::Sacn);
        let (_, wake) = counter();
        let input = hub.input("/in", at, wake).unwrap();
        input.want(&BTreeSet::from([1]));
        until("the socket", || input.read().open);
        assert_eq!(input.read().universes[0].slots, None);
        let to = SocketAddr::from((Ipv4Addr::LOCALHOST, at.port));
        let s = sender();
        let low = sacn::data_packet(&[1; 16], "low", 100, 1, 1, &[255, 255]);
        let high = sacn::data_packet(&[2; 16], "high", 150, 1, 1, &[7]);
        s.send_to(&low, to).unwrap();
        s.send_to(&high, to).unwrap();
        until("the high source", || input.read().universes[0].slots.as_ref().is_some_and(|v| v[0] == 7));
        assert_eq!(
            input.read().universes[0].slots.as_ref().unwrap()[1],
            0,
            "only the highest priority counts"
        );
        // The high one terminates: the low one shows.
        let mut bye = sacn::data_packet(&[2; 16], "high", 150, 1, 2, &[7]);
        bye[112] = sacn::TERMINATED;
        s.send_to(&bye, to).unwrap();
        until("the low source", || input.read().universes[0].slots.as_ref().is_some_and(|v| v[0] == 255));
        // Preview data is ignored.
        let mut preview = sacn::data_packet(&[3; 16], "preview", 200, 1, 1, &[1]);
        preview[112] = sacn::PREVIEW;
        s.send_to(&preview, to).unwrap();
        // The low one falls silent: the frame is held.
        let start = Instant::now();
        until("silence", || !input.read().universes[0].live);
        assert!(start.elapsed() > Duration::from_secs(2));
        let r = input.read();
        assert_eq!(&r.universes[0].slots.as_ref().unwrap()[..2], &[255, 255]);
        drop(input);
        hub.shutdown();
    }

    #[test]
    fn a_taken_port_is_an_error_and_shared_listeners_stay_until_the_last() {
        let hub = Inputs::new();
        let taken = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let at = Listen { protocol: Protocol::ArtNet, interface: Ipv4Addr::LOCALHOST, port };
        let (woken, wake) = counter();
        let a = hub.input("/a", at, wake.clone()).unwrap();
        let b = hub.input("/b", at, wake).unwrap();
        until("the error", || a.read().error.is_some());
        assert!(a.read().error.unwrap().contains("another program"), "{:?}", a.read());
        assert!(woken.load(Ordering::Relaxed) >= 1);
        assert_eq!(hub.status().len(), 1);
        drop(taken);
        until("the socket", || b.read().open);
        drop(a);
        assert_eq!(hub.status()[0].readers, vec!["/b"]);
        drop(b);
        assert!(hub.status().is_empty());
        hub.shutdown();
    }
}
