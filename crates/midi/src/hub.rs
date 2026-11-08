//! The MIDI hub: the devices the network wants, opened and closed by one thread, what they
//! sent, and the bytes `out.midi` sends.
//!
//! - **Devices by name.** A want names a part of a device's name (`easy control`), empty for
//!   every input. The hub's thread lists the ports every [`POLL`] (and at once when the wants
//!   change), opens what is wanted and closes what is unplugged or no longer wanted, so a device
//!   that is plugged in again during a show comes back by itself. A device that cannot be
//!   opened (another program holds it; WinMM gives a port to one program at a time unless
//!   Windows MIDI Services is installed) is retried at every poll and its error shown.
//! - **What arrives** is read on the driver's thread ([`crate::message::Reader`]) into the
//!   device's [`Store`] and [`Clock`]; readers get a snapshot ([`Hub::inputs`]), and
//!   [`Hub::watch`] calls back when something changed. What a device sent stays when it is
//!   unplugged, so bindings hold their last value.
//! - **Learn:** every control change, note on, bend and pressure is also offered to the OSC
//!   hub's learn as `/midi/<device>/cc/<channel>/<number>` and so on ([`address`]), with the
//!   addresses busy when learn was armed passed over, as for OSC.
//! - **Hostile input:** a device is trusted no more than the network: the reader checks every
//!   byte, and each device may send at most [`MAX_RATE`] messages a second; more are dropped
//!   and counted.
//! - **Out:** [`Hub::send`] queues bytes for an output (at most [`MAX_QUEUED`]); the thread
//!   sends them. An output is opened while a want names it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use penumbra_core::lock;
use penumbra_osc::codec::{Arg, Message};
use penumbra_osc::hub::{BUSY, Hub as OscHub, Via};
use serde::Serialize;

use crate::clock::{Clock, ClockState};
use crate::message::{Event, Reader};
use crate::store::{Control, Pairs, Store, control_of};

/// How often the ports are listed for devices plugged in or out.
pub const POLL: Duration = Duration::from_secs(1);
/// Messages per second one device may send; more are dropped.
pub const MAX_RATE: u64 = 20_000;
/// Messages waiting to be sent, all outputs together.
pub const MAX_QUEUED: usize = 4096;
/// Messages kept for [`Status::recent`].
pub const RECENT: usize = 32;
/// The client name the driver sees.
const CLIENT: &str = "penumbra";

/// Settings for this run. Not stored: every start has the defaults (`midi.config`).
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Off closes every device (the wants are kept for when it is on again).
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// An input as the network wants it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Want {
    /// A part of the device's name, any case; empty: every input.
    pub device: String,
    /// Control changes 32 to 63 refine 0 to 31 (14-bit pairs).
    pub pairs: bool,
}

/// Whether `pattern` (a part of a name, any case; empty for all) names `name`.
pub fn matches(pattern: &str, name: &str) -> bool {
    let p = pattern.trim().to_lowercase();
    p.is_empty() || name.to_lowercase().contains(&p)
}

/// A device's name as it appears in addresses: lower case, anything but letters and digits a
/// single `_` (`WORLDE easy control` is `worlde_easy_control`).
pub fn device_key(name: &str) -> String {
    let mut key = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c.to_ascii_lowercase());
        } else if !key.ends_with('_') && !key.is_empty() {
            key.push('_');
        }
    }
    let key = key.trim_end_matches('_').to_string();
    if key.is_empty() { "device".to_string() } else { key }
}

/// The OSC address of a device's control, as learn hears it: `/midi/<device>/cc/1/7`.
pub fn address(device_key: &str, control: &Control) -> String {
    format!("/midi/{device_key}{}", control.path())
}

/// Reads an address as [`address`] writes it: the device key and the control.
pub fn parse_address(address: &str) -> Option<(String, Control)> {
    let rest = address.strip_prefix("/midi/")?;
    let (key, path) = rest.split_once('/')?;
    Some((key.to_string(), Control::from_path(path)?))
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InputStatus {
    pub name: String,
    /// The device's part of the addresses.
    pub key: String,
    pub connected: bool,
    /// Made by `midi.inject`, not a device.
    pub injected: bool,
    /// Why it could not be opened (retried at every poll).
    pub error: Option<String>,
    pub messages: u64,
    /// Bytes the reader skipped (system exclusive, garbage).
    pub skipped: u64,
    /// Messages over [`MAX_RATE`].
    pub dropped: u64,
    pub controls: usize,
    /// The 14-bit pairs are on.
    pub pairs: bool,
    pub clock: Option<ClockState>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OutputStatus {
    pub name: String,
    pub connected: bool,
    pub error: Option<String>,
    /// Messages sent.
    pub sent: u64,
    /// Messages that failed or found the queue full.
    pub dropped: u64,
    pub last_error: Option<String>,
    /// Changes every time the output is opened, so a sender can send everything again.
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Recent {
    pub device: String,
    pub event: Event,
    /// Seconds ago.
    pub age: f64,
    #[serde(skip)]
    at: Option<Instant>,
}

/// The devices this machine has, as the last poll found them.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Ports {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Why listing failed.
    pub error: Option<String>,
}

/// `midi.status`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub ports: Ports,
    pub wanted_inputs: Vec<Want>,
    pub wanted_outputs: Vec<String>,
    pub inputs: Vec<InputStatus>,
    pub outputs: Vec<OutputStatus>,
    pub recent: Vec<Recent>,
    pub queued_dropped: u64,
}

/// One input as a reader gets it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub name: String,
    pub key: String,
    pub connected: bool,
    pub store: Store,
    pub clock: ClockState,
}

struct InputState {
    reader: Reader,
    store: Store,
    clock: Clock,
    pairs: Option<Pairs>,
    status: InputStatus,
    /// The learn generation and the addresses busy when it was armed.
    busy: (u64, BTreeSet<String>),
    /// The current second and the messages in it.
    window: (Option<Instant>, u64),
}

struct Input {
    name: String,
    key: String,
    state: Mutex<InputState>,
}

/// Bytes for the output of that name.
type Outgoing = (String, Vec<u8>);

type WatchEntry = (u64, Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);

struct Inner {
    osc: OscHub,
    config: Mutex<Config>,
    wanted: Mutex<(Vec<Want>, Vec<String>)>,
    /// Set when the wants or the config changed: the thread polls at once.
    dirty: AtomicBool,
    inputs: Mutex<BTreeMap<String, Arc<Input>>>,
    outputs: Mutex<BTreeMap<String, OutputStatus>>,
    /// Outputs that are no device: what is sent to them is kept ([`Hub::capture`]).
    captured: Mutex<BTreeMap<String, VecDeque<Vec<u8>>>>,
    ports: Mutex<Ports>,
    watches: Mutex<Vec<WatchEntry>>,
    recent: Mutex<VecDeque<Recent>>,
    queue: Mutex<Option<SyncSender<Outgoing>>>,
    queued_dropped: AtomicU64,
    thread: Mutex<Option<JoinHandle<()>>>,
    stop: AtomicBool,
    next_id: AtomicU64,
}

/// The MIDI hub, shared by the engine, the nodes and the commands (clones share it).
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

/// Keeps a [`Hub::watch`] callback registered; dropping it unregisters.
pub struct Watch {
    id: u64,
    pending: Arc<AtomicBool>,
    hub: Weak<Inner>,
}

impl Watch {
    /// Allows the next callback (call before reading).
    pub fn ack(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(inner) = self.hub.upgrade() {
            lock(&inner.watches).retain(|(id, _, _)| *id != self.id);
        }
    }
}

fn wake(inner: &Inner) {
    let watches: Vec<_> = lock(&inner.watches)
        .iter()
        .filter(|(_, pending, _)| !pending.swap(true, Ordering::AcqRel))
        .map(|(_, _, f)| f.clone())
        .collect();
    for f in watches {
        f();
    }
}

impl Hub {
    /// A hub whose controls are offered to `osc`'s learn.
    pub fn new(osc: OscHub) -> Self {
        Self {
            inner: Arc::new(Inner {
                osc,
                config: Mutex::new(Config::default()),
                wanted: Mutex::new((Vec::new(), Vec::new())),
                dirty: AtomicBool::new(false),
                inputs: Mutex::new(BTreeMap::new()),
                outputs: Mutex::new(BTreeMap::new()),
                captured: Mutex::new(BTreeMap::new()),
                ports: Mutex::new(Ports::default()),
                watches: Mutex::new(Vec::new()),
                recent: Mutex::new(VecDeque::new()),
                queue: Mutex::new(None),
                queued_dropped: AtomicU64::new(0),
                thread: Mutex::new(None),
                stop: AtomicBool::new(false),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    /// Changes the settings; off closes every device at the next poll, which comes at once.
    pub fn set_config(&self, config: Config) {
        *lock(&self.inner.config) = config;
        self.inner.dirty.store(true, Ordering::Release);
        self.start();
    }

    /// The inputs and outputs the network wants. The thread starts with the first want.
    pub fn set_wanted(&self, inputs: &[Want], outputs: &[String]) {
        let mut inputs = inputs.to_vec();
        inputs.sort();
        inputs.dedup();
        let mut outputs: Vec<String> =
            outputs.iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
        outputs.sort();
        outputs.dedup();
        {
            let mut w = lock(&self.inner.wanted);
            if w.0 == inputs && w.1 == outputs {
                return;
            }
            *w = (inputs, outputs);
        }
        // Pairs follow the wants at once, also for injected inputs.
        self.apply_pairs();
        self.inner.dirty.store(true, Ordering::Release);
        let any = {
            let w = lock(&self.inner.wanted);
            !w.0.is_empty() || !w.1.is_empty()
        };
        if any {
            self.start();
        }
    }

    fn apply_pairs(&self) {
        let wants = lock(&self.inner.wanted).0.clone();
        for input in lock(&self.inner.inputs).values() {
            let pairs = wants.iter().any(|w| w.pairs && matches(&w.device, &input.name));
            let mut st = lock(&input.state);
            if pairs != st.pairs.is_some() {
                st.pairs = pairs.then(Pairs::default);
                st.status.pairs = pairs;
            }
        }
    }

    /// Starts the thread if it does not run.
    fn start(&self) {
        let mut thread = lock(&self.inner.thread);
        if thread.is_some() {
            return;
        }
        let (tx, rx) = sync_channel(MAX_QUEUED);
        *lock(&self.inner.queue) = Some(tx);
        let weak = Arc::downgrade(&self.inner);
        match std::thread::Builder::new().name("midi".into()).spawn(move || run(&weak, &rx)) {
            Ok(t) => *thread = Some(t),
            Err(e) => lock(&self.inner.ports).error = Some(format!("cannot start the MIDI thread: {e}")),
        }
    }

    /// Calls `wake` when an input's store or clock changes, or a device comes or goes, at
    /// most once until [`Watch::ack`].
    pub fn watch(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Watch {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let pending = Arc::new(AtomicBool::new(false));
        lock(&self.inner.watches).push((id, pending.clone(), wake));
        Watch { id, pending, hub: Arc::downgrade(&self.inner) }
    }

    /// The inputs `pattern` names that were ever opened or injected, in name order.
    pub fn inputs(&self, pattern: &str) -> Vec<Snapshot> {
        let now = Instant::now();
        lock(&self.inner.inputs)
            .values()
            .filter(|i| matches(pattern, &i.name))
            .map(|i| {
                let st = lock(&i.state);
                Snapshot {
                    name: i.name.clone(),
                    key: i.key.clone(),
                    connected: st.status.connected,
                    store: st.store.clone(),
                    clock: st.clock.state(now),
                }
            })
            .collect()
    }

    /// The name of the input whose address key is `key`.
    pub fn input_named_by_key(&self, key: &str) -> Option<String> {
        lock(&self.inner.inputs).values().find(|i| i.key == key).map(|i| i.name.clone())
    }

    /// The devices as the last poll found them.
    pub fn ports(&self) -> Ports {
        lock(&self.inner.ports).clone()
    }

    /// The output a want `pattern` opens: the first port it names.
    pub fn output_for(&self, pattern: &str) -> Option<String> {
        if pattern.trim().is_empty() {
            return None;
        }
        if let Some(name) = lock(&self.inner.captured).keys().find(|o| matches(pattern, o)) {
            return Some(name.clone());
        }
        lock(&self.inner.ports).outputs.iter().find(|o| matches(pattern, o)).cloned()
    }

    pub fn output(&self, name: &str) -> Option<OutputStatus> {
        lock(&self.inner.outputs).get(name).cloned()
    }

    /// Queues `bytes` for the output `name`; `false` when the queue is full or nothing runs.
    pub fn send(&self, name: &str, bytes: Vec<u8>) -> bool {
        if let Some(kept) = lock(&self.inner.captured).get_mut(name) {
            if kept.len() >= MAX_QUEUED {
                kept.pop_front();
            }
            kept.push_back(bytes);
            if let Some(s) = lock(&self.inner.outputs).get_mut(name) {
                s.sent += 1;
            }
            return true;
        }
        let sent = match lock(&self.inner.queue).as_ref() {
            Some(tx) => match tx.try_send((name.to_string(), bytes)) {
                Ok(()) => true,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            },
            None => false,
        };
        if !sent {
            self.inner.queued_dropped.fetch_add(1, Ordering::Relaxed);
        }
        sent
    }

    /// Makes an output `name` that is no device and keeps what is sent to it (the last
    /// [`MAX_QUEUED`] messages), for tests and for trying without a device.
    pub fn capture(&self, name: &str) {
        lock(&self.inner.captured).entry(name.to_string()).or_default();
        let mut outputs = lock(&self.inner.outputs);
        let s = outputs
            .entry(name.to_string())
            .or_insert_with(|| OutputStatus { name: name.to_string(), ..Default::default() });
        if !s.connected {
            s.connected = true;
            s.generation += 1;
        }
    }

    /// What was sent to the captured output `name` since the last call.
    pub fn captured(&self, name: &str) -> Vec<Vec<u8>> {
        lock(&self.inner.captured).get_mut(name).map(|k| k.drain(..).collect()).unwrap_or_default()
    }

    /// Takes in `bytes` as if the input `name` had sent them (the input is made if it does not
    /// exist; it is not a device and no poll closes it). For tests and for trying without a
    /// device.
    pub fn inject(&self, name: &str, bytes: &[u8], stamp: Option<u64>) {
        let input = input_of(&self.inner, name);
        {
            let mut st = lock(&input.state);
            st.status.injected = true;
            st.status.connected = true;
        }
        let stamp = stamp.unwrap_or_else(micros_since_start);
        take(&self.inner, &input, stamp, bytes);
    }

    pub fn status(&self) -> Status {
        let now = Instant::now();
        let (wanted_inputs, wanted_outputs) = lock(&self.inner.wanted).clone();
        let inputs = lock(&self.inner.inputs)
            .values()
            .map(|i| {
                let st = lock(&i.state);
                let mut s = st.status.clone();
                s.controls = st.store.entries.len();
                s.clock = (st.clock.clocks > 0).then(|| st.clock.state(now));
                s
            })
            .collect();
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
            ports: self.ports(),
            wanted_inputs,
            wanted_outputs,
            inputs,
            outputs: lock(&self.inner.outputs).values().cloned().collect(),
            recent,
            queued_dropped: self.inner.queued_dropped.load(Ordering::Relaxed),
        }
    }

    /// Lists the ports now (for a menu), without waiting for the next poll.
    pub fn list_now(&self) -> Ports {
        let ports = list();
        *lock(&self.inner.ports) = ports.clone();
        ports
    }

    /// Stops the thread, which closes every device.
    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::Release);
        *lock(&self.inner.queue) = None;
        if let Some(t) = lock(&self.inner.thread).take() {
            let _ = t.join();
        }
    }
}

/// Microseconds on one clock for stamps that do not come from a driver.
fn micros_since_start() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    u64::try_from(Instant::now().saturating_duration_since(*start).as_micros()).unwrap_or(u64::MAX)
}

fn input_of(inner: &Inner, name: &str) -> Arc<Input> {
    let mut inputs = lock(&inner.inputs);
    if let Some(i) = inputs.get(name) {
        return i.clone();
    }
    let pairs = lock(&inner.wanted).0.iter().any(|w| w.pairs && matches(&w.device, name));
    let key = device_key(name);
    let input = Arc::new(Input {
        name: name.to_string(),
        key: key.clone(),
        state: Mutex::new(InputState {
            reader: Reader::new(),
            store: Store::default(),
            clock: Clock::new(),
            pairs: pairs.then(Pairs::default),
            status: InputStatus { name: name.to_string(), key, pairs, ..InputStatus::default() },
            busy: (0, BTreeSet::new()),
            window: (None, 0),
        }),
    });
    inputs.insert(name.to_string(), input.clone());
    input
}

/// Takes in what an input sent: into its store and clock, and to learn.
fn take(inner: &Inner, input: &Input, stamp: u64, bytes: &[u8]) {
    let now = Instant::now();
    let generation = inner.osc.learn_generation();
    let learning = inner.osc.learning();
    let mut events = Vec::new();
    let mut learn: Vec<Message> = Vec::new();
    let busy;
    {
        let mut st = lock(&input.state);
        if learning && st.busy.0 != generation {
            let set = st
                .store
                .entries
                .iter()
                .filter(|e| e.at.is_some_and(|at| now.saturating_duration_since(at) < BUSY))
                .map(|e| address(&input.key, &e.control))
                .collect();
            st.busy = (generation, set);
        }
        let InputState { reader, .. } = &mut *st;
        reader.read(bytes, &mut |e| events.push(e));
        // The rate cap: a second's messages.
        let in_window =
            st.window.0.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(1));
        if !in_window {
            st.window = (Some(now), 0);
        }
        let room = MAX_RATE.saturating_sub(st.window.1);
        // `as`: a count of events from one call, far below 2^64.
        let n = events.len() as u64;
        if n > room {
            st.status.dropped += n - room;
            events.truncate(usize::try_from(room).unwrap_or(0));
        }
        st.window.1 += events.len() as u64;
        st.status.messages += events.len() as u64;
        st.status.skipped = st.reader.skipped;
        for e in &events {
            match *e {
                Event::Clock => st.clock.clock(stamp, now),
                Event::Start => st.clock.start(),
                Event::Continue => st.clock.resume(),
                Event::Stop => st.clock.stop(),
                Event::SongPosition(p) => st.clock.song_position(p),
                _ => {
                    let InputState { pairs, store, .. } = &mut *st;
                    let Some((control, value, note)) = control_of(e, pairs.as_mut()) else { continue };
                    match note {
                        Some(velocity) => store.note(control, velocity, now),
                        None => store.set(control, value, now),
                    }
                    // A note's release and a program change bind nothing.
                    let offered = match note {
                        Some(v) => v.is_some(),
                        None => !matches!(control, Control::Program { .. }),
                    };
                    if learning && offered {
                        learn.push(Message::new(address(&input.key, &control), vec![Arg::Float(1.0)]));
                    }
                }
            }
        }
        busy = st.busy.clone();
    }
    if events.is_empty() {
        return;
    }
    {
        let mut recent = lock(&inner.recent);
        for e in events.iter().filter(|e| !matches!(e, Event::Clock)) {
            if recent.len() >= RECENT {
                recent.pop_front();
            }
            recent.push_back(Recent { device: input.name.clone(), event: *e, age: 0.0, at: Some(now) });
        }
    }
    if !learn.is_empty() {
        let is_busy = |a: &str| busy.0 == generation && busy.1.contains(a);
        let from = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        inner.osc.feed(Via::Midi, 0, from, &learn, None, &is_busy);
    }
    wake(inner);
}

/// The ports of this machine, listed now (for a menu).
pub fn list() -> Ports {
    let mut ports = Ports::default();
    match MidiInput::new(CLIENT) {
        Ok(mi) => {
            ports.inputs = mi.ports().iter().filter_map(|p| mi.port_name(p).ok()).collect();
        }
        Err(e) => ports.error = Some(format!("cannot list MIDI inputs: {e}")),
    }
    match MidiOutput::new(CLIENT) {
        Ok(mo) => {
            ports.outputs = mo.ports().iter().filter_map(|p| mo.port_name(p).ok()).collect();
        }
        Err(e) => ports.error = Some(format!("cannot list MIDI outputs: {e}")),
    }
    ports
}

/// What the thread holds open.
#[derive(Default)]
struct Open {
    inputs: BTreeMap<String, MidiInputConnection<()>>,
    outputs: BTreeMap<String, MidiOutputConnection>,
}

fn run(weak: &Weak<Inner>, rx: &Receiver<Outgoing>) {
    let mut open = Open::default();
    let mut last_poll: Option<Instant> = None;
    loop {
        let Some(inner) = weak.upgrade() else { break };
        if inner.stop.load(Ordering::Acquire) {
            break;
        }
        let due = inner.dirty.swap(false, Ordering::AcqRel) || last_poll.is_none_or(|t| t.elapsed() >= POLL);
        if due {
            // A panic in a driver must not take the show down: the poll is tried again.
            let polled =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll(&inner, weak, &mut open)));
            if polled.is_err() {
                lock(&inner.ports).error = Some("listing the MIDI devices failed (a driver panicked)".into());
            }
            last_poll = Some(Instant::now());
        }
        drop(inner);
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(first) => {
                let Some(inner) = weak.upgrade() else { break };
                send_one(&inner, &mut open, first);
                // Whatever else waits, without blocking.
                for _ in 0..MAX_QUEUED {
                    match rx.try_recv() {
                        Ok(m) => send_one(&inner, &mut open, m),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Dropping the connections closes the devices.
    drop(open);
}

fn send_one(inner: &Inner, open: &mut Open, (name, bytes): (String, Vec<u8>)) {
    let mut outputs = lock(&inner.outputs);
    let Some(status) = outputs.get_mut(&name) else {
        inner.queued_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match open.outputs.get_mut(&name) {
        Some(c) => match c.send(&bytes) {
            Ok(()) => status.sent += 1,
            Err(e) => {
                status.dropped += 1;
                status.last_error = Some(e.to_string());
            }
        },
        None => status.dropped += 1,
    }
}

fn poll(inner: &Arc<Inner>, weak: &Weak<Inner>, open: &mut Open) {
    let enabled = lock(&inner.config).enabled;
    let (wants, out_wants) = lock(&inner.wanted).clone();
    let ports = list();
    *lock(&inner.ports) = ports.clone();
    let mut changed = false;

    // Inputs: every port a want names.
    let wanted: BTreeSet<String> = if enabled {
        ports.inputs.iter().filter(|p| wants.iter().any(|w| matches(&w.device, p))).cloned().collect()
    } else {
        BTreeSet::new()
    };
    let gone: Vec<String> = open.inputs.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
    for name in gone {
        open.inputs.remove(&name);
        if let Some(i) = lock(&inner.inputs).get(&name) {
            lock(&i.state).status.connected = false;
        }
        log::info!("MIDI input {name} closed");
        changed = true;
    }
    // A device not wanted any more (or unplugged) but never closed by us, as after a failed
    // open, shows as not connected.
    for (name, i) in lock(&inner.inputs).iter() {
        let mut st = lock(&i.state);
        if !st.status.injected && !open.inputs.contains_key(name) && st.status.connected {
            st.status.connected = false;
            changed = true;
        }
    }
    for name in wanted {
        if open.inputs.contains_key(&name) {
            continue;
        }
        let input = input_of(inner, &name);
        match connect_input(weak, &input) {
            Ok(c) => {
                open.inputs.insert(name.clone(), c);
                let mut st = lock(&input.state);
                st.status.connected = true;
                st.status.error = None;
                log::info!("MIDI input {name} opened");
                changed = true;
            }
            Err(e) => {
                let mut st = lock(&input.state);
                if st.status.error.as_deref() != Some(e.as_str()) {
                    log::warn!("MIDI input {name}: {e}");
                    st.status.error = Some(e);
                    changed = true;
                }
            }
        }
    }

    // Outputs: the first port each want names.
    let wanted: BTreeSet<String> = if enabled {
        out_wants.iter().filter_map(|w| ports.outputs.iter().find(|p| matches(w, p)).cloned()).collect()
    } else {
        BTreeSet::new()
    };
    let gone: Vec<String> = open.outputs.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
    for name in gone {
        open.outputs.remove(&name);
        if let Some(s) = lock(&inner.outputs).get_mut(&name) {
            s.connected = false;
        }
        log::info!("MIDI output {name} closed");
        changed = true;
    }
    for name in wanted {
        if open.outputs.contains_key(&name) {
            continue;
        }
        let result = connect_output(&name);
        let mut outputs = lock(&inner.outputs);
        let status = outputs
            .entry(name.clone())
            .or_insert_with(|| OutputStatus { name: name.clone(), ..Default::default() });
        match result {
            Ok(c) => {
                open.outputs.insert(name.clone(), c);
                status.connected = true;
                status.error = None;
                status.generation += 1;
                log::info!("MIDI output {name} opened");
                changed = true;
            }
            Err(e) => {
                if status.error.as_deref() != Some(e.as_str()) {
                    log::warn!("MIDI output {name}: {e}");
                    status.error = Some(e);
                    changed = true;
                }
            }
        }
    }
    if changed {
        wake(inner);
    }
}

fn connect_input(weak: &Weak<Inner>, input: &Arc<Input>) -> Result<MidiInputConnection<()>, String> {
    let mut mi = MidiInput::new(CLIENT).map_err(|e| e.to_string())?;
    mi.ignore(Ignore::SysexAndActiveSense);
    let port = mi
        .ports()
        .into_iter()
        .find(|p| mi.port_name(p).is_ok_and(|n| n == input.name))
        .ok_or("the device is gone")?;
    let (weak, target) = (weak.clone(), Arc::downgrade(input));
    let callback = move |stamp: u64, bytes: &[u8], _: &mut ()| {
        // The driver calls this on its own thread, where a panic would end the process.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let (Some(inner), Some(input)) = (weak.upgrade(), target.upgrade()) {
                take(&inner, &input, stamp, bytes);
            }
        }));
    };
    mi.connect(&port, "penumbra-in", callback, ()).map_err(|e| {
        format!("{e} (another program may hold it: under Windows a device is open in one program at a time)")
    })
}

fn connect_output(name: &str) -> Result<MidiOutputConnection, String> {
    let mo = MidiOutput::new(CLIENT).map_err(|e| e.to_string())?;
    let port = mo
        .ports()
        .into_iter()
        .find(|p| mo.port_name(p).is_ok_and(|n| n == name))
        .ok_or("the device is gone")?;
    mo.connect(&port, "penumbra-out").map_err(|e| {
        format!("{e} (another program may hold it: under Windows a device is open in one program at a time)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_addresses() {
        assert_eq!(device_key("WORLDE easy control"), "worlde_easy_control");
        assert_eq!(device_key("2- nanoKONTROL2 (1)"), "2_nanokontrol2_1");
        assert_eq!(device_key("  ***  "), "device");
        let c = Control::Cc { channel: 1, number: 7 };
        let a = address("worlde_easy_control", &c);
        assert_eq!(a, "/midi/worlde_easy_control/cc/1/7");
        assert_eq!(parse_address(&a), Some(("worlde_easy_control".to_string(), c)));
        assert!(matches("", "x") && matches("EASY", "WORLDE easy control") && !matches("nano", "easy"));
    }

    #[test]
    fn injected_messages_reach_the_store_the_clock_and_learn() {
        let osc = OscHub::new();
        let hub = Hub::new(osc.clone());
        let woke = Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        let watch = hub.watch(Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }));
        hub.inject("Test Device", &[0xB0, 7, 127], None);
        let snap = hub.inputs("test");
        assert_eq!(snap.len(), 1);
        let e = snap.first().and_then(|s| s.store.get(&Control::Cc { channel: 1, number: 7 })).cloned();
        assert_eq!(e.map(|e| e.value), Some(1.0));
        assert_eq!(woke.load(Ordering::Relaxed), 1);
        // Learn hears the next control moved, not the busy one.
        osc.learn(true);
        hub.inject("Test Device", &[0xB0, 7, 100], None);
        assert!(osc.take_heard().is_none(), "cc 7 was busy when learn was armed");
        hub.inject("Test Device", &[0x90, 36, 0, 0x90, 36, 90], None);
        let heard = osc.take_heard();
        assert_eq!(
            heard.map(|h| (h.via, h.address)),
            Some((Via::Midi, "/midi/test_device/note/1/36".to_string()))
        );
        watch.ack();
        // Clocks at 120 BPM.
        for i in 0..30u64 {
            hub.inject("Test Device", &[0xF8], Some(i * 20_833));
        }
        let clock = hub.inputs("test").first().map(|s| s.clock).unwrap_or_default();
        assert!((clock.bpm - 120.0).abs() < 0.05, "{}", clock.bpm);
    }

    #[test]
    fn pairs_follow_the_wants() {
        let hub = Hub::new(OscHub::new());
        hub.inject("Fine", &[0xB0, 1, 64, 0xB0, 33, 64], None);
        let n = hub.inputs("fine").first().map(|s| s.store.entries.len());
        assert_eq!(n, Some(2), "without pairs, cc 33 is a control of its own");
        // A want with pairs; the thread is not needed for an injected input.
        hub.set_wanted(&[Want { device: "fine".into(), pairs: true }], &[]);
        hub.inject("Fine", &[0xB0, 1, 64, 0xB0, 33, 64], None);
        let snap = hub.inputs("fine");
        let v =
            snap.first().and_then(|s| s.store.get(&Control::Cc { channel: 1, number: 1 })).map(|e| e.value);
        assert_eq!(v, Some(f64::from((64u16 << 7) | 64) / 16383.0));
        hub.shutdown();
    }

    #[test]
    fn a_flood_is_capped() {
        let hub = Hub::new(OscHub::new());
        let bytes: Vec<u8> = (0..30_000).flat_map(|i| [0xB0, 1, (i % 128) as u8]).collect();
        hub.inject("Flood", &bytes, None);
        let st = hub.status();
        let i = st.inputs.first().cloned().unwrap_or_default();
        assert_eq!((i.messages, i.dropped), (MAX_RATE, 30_000 - MAX_RATE));
    }
}
