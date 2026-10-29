//! The output hub: one bus per output device, shared by every `out.audio` node that plays on it.
//!
//! - **Lanes:** each node hands its block of the frame to its lane on the bus ([`Outputs::submit`]),
//!   already levelled and limited. A lane keeps what it was given until the bus takes it.
//! - **After every cook pass** ([`Outputs::after_pass`]) each bus that got something sums its
//!   lanes and pushes the sum to its [`Player`]. A lane on a device clock may bring more or less
//!   than a frame; the bus takes as many frames as the shortest fresh lane has and keeps the
//!   rest (at most [`MAX_LEFTOVER_SECONDS`]) for the next pass.
//! - **Two or more lanes** go through a limiter of the bus as well, since two sounds each just
//!   under the ceiling add up to more. A single lane is limited by its node alone, so it pays
//!   for one look-ahead only. Every sum ends in a hard clip at the ceiling.
//! - **Mute** ([`Outputs::set_muted`]) silences every bus at once, ramped across one block.
//! - **Pause:** a bus that got nothing while the show stands still, while it renders offline, or
//!   for [`IDLE_PASSES`] passes in a row fades out without counting an underrun.
//! - **Opening and closing:** [`Outputs::set_wanted`] (after every change of the network) opens
//!   a bus for every device a node wants and closes the others: they fade out, and their player
//!   is dropped a little later, which closes the device on its own thread.
//!
//! [`Outputs::offline`] makes buses without a device, for tests: what each one was pushed is
//! kept, and [`Outputs::pull`] plays it through the bus's [`Engine`]. [`keep_off_devices`] makes
//! every bus of the process such a bus, for runs without a window that must not reach a speaker.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, lock};
use serde::Serialize;

use crate::limiter::Limiter;
use crate::playback::{Engine, EngineStatus, Player, PlayerStatus, Settings};

/// Most a lane keeps beyond what the bus took in a pass, seconds; older samples are dropped.
pub const MAX_LEFTOVER_SECONDS: f64 = 0.1;
/// Passes in a row without a block, while the show runs, before a bus fades out.
pub const IDLE_PASSES: u32 = 2;
/// How long a closing bus fades before its device closes.
const CLOSE_AFTER: Duration = Duration::from_millis(150);
/// Passes without a block after which a lane is forgotten (a node that stopped).
const STALE_PASSES: u64 = 60;
/// Set by [`keep_off_devices`].
static OFF_DEVICES: AtomicBool = AtomicBool::new(false);

/// From now on every bus opened plays into nothing, as an offline bus at 48 kHz stereo, instead
/// of a device. For runs without a window: their frames come slowly and irregularly, so a
/// speaker would only stutter.
pub fn keep_off_devices() {
    OFF_DEVICES.store(true, Ordering::Relaxed);
}
/// Most samples of history an offline bus keeps per channel (tests).
const HISTORY: usize = 48_000 * 120;

/// What a node wants of the hub: its device (part of a name, empty: the default output) and
/// the settings of its player.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    pub node: NodeId,
    pub key: String,
    pub settings: Settings,
}

/// One node's lane on a bus, as it stands.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LaneStatus {
    pub node: NodeId,
    pub rate: u32,
    pub channels: usize,
    /// The node's limiter: the largest reduction of its last block, dB.
    pub reduction_db: f64,
    /// The largest reduction of the last two seconds, dB.
    pub reduction_held_db: f64,
}

/// A bus as it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BusStatus {
    /// The device pattern (empty: the default output).
    pub key: String,
    pub lanes: Vec<LaneStatus>,
    /// The rate and channels of the sum pushed.
    pub rate: u32,
    pub channels: usize,
    /// Faded out on purpose: the show stands still or nothing feeds it.
    pub paused: bool,
    pub muted: bool,
    /// The bus limiter (two or more lanes), largest reduction of the last two seconds, dB.
    pub reduction_db: f64,
    /// Blocks refused: another lane plays at another rate.
    pub refused: u64,
    /// The device player; `None` for an offline bus.
    pub player: Option<PlayerStatus>,
    /// The engine of an offline bus.
    pub offline: Option<EngineStatus>,
}

enum Sink {
    Device(Player),
    Offline { engine: Box<Engine>, history: Vec<Vec<f32>> },
}

impl Sink {
    fn push(&mut self, rate: u32, channels: &[Vec<f64>]) -> Result<usize, String> {
        let slices: Vec<&[f64]> = channels.iter().map(Vec::as_slice).collect();
        match self {
            Sink::Device(p) => p.push(rate, &slices),
            Sink::Offline { engine, history } => {
                history.resize(channels.len().max(history.len()), Vec::new());
                for (h, c) in history.iter_mut().zip(channels) {
                    // `as`: samples to f32 as the engine keeps them.
                    h.extend(c.iter().map(|v| *v as f32));
                    let excess = h.len().saturating_sub(HISTORY);
                    if excess > 0 {
                        h.drain(..excess);
                    }
                }
                engine.push(rate, &slices)
            }
        }
    }

    fn pause(&mut self) {
        match self {
            Sink::Device(p) => p.pause(),
            Sink::Offline { engine, .. } => engine.pause(),
        }
    }

    fn set_settings(&mut self, settings: Settings) {
        match self {
            Sink::Device(p) => {
                if p.settings() != settings {
                    p.set_settings(settings);
                }
            }
            Sink::Offline { engine, .. } => {
                if engine.settings() != settings {
                    engine.set_settings(settings);
                }
            }
        }
    }
}

struct Lane {
    rate: u32,
    fifo: Vec<VecDeque<f64>>,
    /// Got a block in this pass.
    fresh: bool,
    last_pass: u64,
    settings: Settings,
    reduction_db: f64,
    held: (f64, Instant),
}

struct Bus {
    sink: Sink,
    rate: u32,
    lanes: BTreeMap<NodeId, Lane>,
    limiter: Option<Limiter>,
    /// The mute ramp's gain at the end of the last block.
    gain: f64,
    idle: u32,
    paused: bool,
    reduction: (f64, Instant),
    refused: u64,
    channels: usize,
}

impl Bus {
    fn new(sink: Sink) -> Self {
        Self {
            sink,
            rate: 0,
            lanes: BTreeMap::new(),
            limiter: None,
            gain: 1.0,
            idle: 0,
            paused: false,
            reduction: (0.0, Instant::now()),
            refused: 0,
            channels: 0,
        }
    }

    fn pause(&mut self) {
        if !self.paused {
            self.sink.pause();
            self.paused = true;
        }
        for lane in self.lanes.values_mut() {
            for c in &mut lane.fifo {
                c.clear();
            }
        }
        if let Some(l) = self.limiter.as_mut() {
            l.reset();
        }
    }

    /// Sums the lanes into one block and pushes it; pauses when nothing came.
    fn flush(&mut self, pass: u64, running: bool, muted: bool) {
        self.lanes.retain(|_, l| pass.saturating_sub(l.last_pass) <= STALE_PASSES);
        let fresh: Vec<&Lane> = self.lanes.values().filter(|l| l.fresh).collect();
        if fresh.is_empty() || !running {
            self.idle = self.idle.saturating_add(1);
            if !running || self.idle >= IDLE_PASSES {
                self.pause();
            }
            for l in self.lanes.values_mut() {
                l.fresh = false;
            }
            return;
        }
        self.idle = 0;
        let n = fresh.iter().map(|l| l.fifo.first().map_or(0, VecDeque::len)).min().unwrap_or(0);
        let channels = fresh.iter().map(|l| l.fifo.len()).max().unwrap_or(1).max(1);
        let settings = fresh.first().map(|l| l.settings).unwrap_or_default();
        let several =
            self.lanes.values().filter(|l| l.fifo.first().is_some_and(|f| !f.is_empty())).count() > 1;
        let mut sum = vec![vec![0.0f64; n]; channels];
        let keep = (f64::from(self.rate) * MAX_LEFTOVER_SECONDS) as usize;
        for lane in self.lanes.values_mut() {
            let lane_channels = lane.fifo.len();
            let take = lane.fifo.first().map_or(0, VecDeque::len).min(n);
            for (ci, fifo) in lane.fifo.iter_mut().enumerate() {
                let take = take.min(fifo.len());
                if lane_channels == 1 {
                    // Mono to every channel of the bus.
                    for (k, v) in fifo.drain(..take).enumerate() {
                        for out in &mut sum {
                            if let Some(o) = out.get_mut(k) {
                                *o += v;
                            }
                        }
                    }
                } else if let Some(out) = sum.get_mut(ci) {
                    for (o, v) in out.iter_mut().zip(fifo.drain(..take)) {
                        *o += v;
                    }
                } else {
                    fifo.drain(..take);
                }
                let excess = fifo.len().saturating_sub(keep);
                if excess > 0 {
                    fifo.drain(..excess);
                }
            }
            lane.fresh = false;
        }
        if several {
            if self.limiter.as_ref().is_none_or(|l| l.rate() != self.rate || l.channels() != channels) {
                self.limiter = Some(Limiter::new(self.rate, channels));
            }
            if let Some(l) = self.limiter.as_mut() {
                l.process(&mut sum);
                let db = l.take_reduction();
                if db >= self.reduction.0 || self.reduction.1.elapsed() > HOLD {
                    self.reduction = (db, Instant::now());
                }
            }
        } else {
            self.limiter = None;
            if self.reduction.1.elapsed() > HOLD {
                self.reduction = (0.0, Instant::now());
            }
        }
        let target = if muted { 0.0 } else { 1.0 };
        let ceiling = crate::limiter::db_to_gain(crate::limiter::CEILING_DB);
        for c in &mut sum {
            for (k, v) in c.iter_mut().enumerate() {
                // `as`: positions in a block.
                let t = (k + 1) as f64 / n.max(1) as f64;
                let g = self.gain + (target - self.gain) * t;
                let x = *v * g;
                *v = if x.is_finite() { x.clamp(-ceiling, ceiling) } else { 0.0 };
            }
        }
        self.gain = target;
        self.channels = channels;
        if n == 0 {
            return;
        }
        self.sink.set_settings(settings);
        match self.sink.push(self.rate, &sum) {
            Ok(_) => self.paused = false,
            Err(e) => log::warn!("audio output: {e}"),
        }
    }

    fn status(&self, key: &str, muted: bool) -> BusStatus {
        let (player, offline) = match &self.sink {
            Sink::Device(p) => (Some(p.status()), None),
            Sink::Offline { engine, .. } => (None, Some(engine.status())),
        };
        BusStatus {
            key: key.to_string(),
            lanes: self
                .lanes
                .iter()
                .map(|(id, l)| LaneStatus {
                    node: *id,
                    rate: l.rate,
                    channels: l.fifo.len(),
                    reduction_db: l.reduction_db,
                    reduction_held_db: l.held.0,
                })
                .collect(),
            rate: self.rate,
            channels: self.channels,
            paused: self.paused,
            muted,
            reduction_db: self.reduction.0,
            refused: self.refused,
            player,
            offline,
        }
    }
}

/// How long a reduction is held for the status.
const HOLD: Duration = Duration::from_secs(2);

struct Inner {
    buses: Mutex<BTreeMap<String, Bus>>,
    closing: Mutex<Vec<(Instant, Sink)>>,
    muted: AtomicBool,
    /// Device format of offline buses; `None`: real devices.
    offline: Option<(u32, usize)>,
    pass: Mutex<u64>,
}

/// The audio outputs (clones share them).
#[derive(Clone)]
pub struct Outputs {
    inner: Arc<Inner>,
}

impl Default for Outputs {
    fn default() -> Self {
        Self::new()
    }
}

impl Outputs {
    /// Buses that play on this machine's output devices.
    pub fn new() -> Self {
        Self::make(None)
    }

    /// Buses without a device, as if each had one at `rate` with `channels` (tests).
    pub fn offline(rate: u32, channels: usize) -> Self {
        Self::make(Some((rate, channels)))
    }

    fn make(offline: Option<(u32, usize)>) -> Self {
        Self {
            inner: Arc::new(Inner {
                buses: Mutex::new(BTreeMap::new()),
                closing: Mutex::new(Vec::new()),
                muted: AtomicBool::new(false),
                offline,
                pass: Mutex::new(0),
            }),
        }
    }

    fn sink(&self, key: &str, settings: Settings) -> Sink {
        let offline =
            self.inner.offline.or_else(|| OFF_DEVICES.load(Ordering::Relaxed).then_some((48_000, 2)));
        match offline {
            Some((rate, channels)) => {
                let mut engine = Box::new(Engine::new(settings));
                engine.set_device(rate, channels);
                Sink::Offline { engine, history: Vec::new() }
            }
            None => Sink::Device(Player::open(key, settings)),
        }
    }

    /// The nodes that want to play and where. Opens a bus for every device wanted and closes
    /// the others (they fade out first); forgets the lanes of nodes not listed.
    pub fn set_wanted(&self, wants: &[Want]) {
        let mut buses = lock(&self.inner.buses);
        let gone: Vec<String> =
            buses.keys().filter(|k| !wants.iter().any(|w| w.key.trim() == k.as_str())).cloned().collect();
        for key in gone {
            if let Some(mut bus) = buses.remove(&key) {
                bus.sink.pause();
                lock(&self.inner.closing).push((Instant::now() + CLOSE_AFTER, bus.sink));
            }
        }
        for w in wants {
            let key = w.key.trim().to_string();
            if !buses.contains_key(&key) {
                let sink = self.sink(&key, w.settings);
                buses.insert(key.clone(), Bus::new(sink));
            }
        }
        for (key, bus) in buses.iter_mut() {
            bus.lanes.retain(|id, _| wants.iter().any(|w| w.node == *id && w.key.trim() == key.as_str()));
        }
    }

    /// The devices with a bus.
    pub fn keys(&self) -> Vec<String> {
        lock(&self.inner.buses).keys().cloned().collect()
    }

    /// Hands node `node`'s block (one slice per channel, at `rate`, already limited) to the bus
    /// of `key`, which pushes it after the pass. `reduction_db` is what its limiter took off.
    /// Errors when another node plays on the same device at another rate.
    pub fn submit(
        &self,
        node: NodeId,
        key: &str,
        settings: Settings,
        rate: u32,
        block: &[Vec<f64>],
        reduction_db: f64,
    ) -> Result<(), String> {
        if block.is_empty() || block.len() > crate::playback::MAX_CHANNELS {
            return Err(format!("{} channels cannot be played", block.len()));
        }
        if !(crate::playback::MIN_RATE..=crate::playback::MAX_RATE).contains(&rate) {
            return Err(format!("a sample rate of {rate} Hz cannot be played"));
        }
        let pass = *lock(&self.inner.pass);
        let key = key.trim().to_string();
        let mut buses = lock(&self.inner.buses);
        if !buses.contains_key(&key) {
            let sink = self.sink(&key, settings);
            buses.insert(key.clone(), Bus::new(sink));
        }
        let Some(bus) = buses.get_mut(&key) else { return Err("no bus".into()) };
        if bus.rate != rate {
            let other = bus.lanes.iter().find(|(id, l)| {
                **id != node && l.rate != rate && pass.saturating_sub(l.last_pass) <= IDLE_PASSES.into()
            });
            if let Some((id, l)) = other {
                bus.refused += 1;
                return Err(format!(
                    "node {id} plays on this device at {} Hz, this one at {rate} Hz: give both the same rate",
                    l.rate
                ));
            }
            bus.rate = rate;
            bus.limiter = None;
            for l in bus.lanes.values_mut() {
                l.fifo.clear();
            }
        }
        let lane = bus.lanes.entry(node).or_insert_with(|| Lane {
            rate,
            fifo: Vec::new(),
            fresh: false,
            last_pass: pass,
            settings,
            reduction_db: 0.0,
            held: (0.0, Instant::now()),
        });
        if lane.rate != rate || lane.fifo.len() != block.len() {
            lane.rate = rate;
            lane.fifo = vec![VecDeque::new(); block.len()];
        }
        let frames = block.iter().map(Vec::len).max().unwrap_or(0);
        for (fifo, c) in lane.fifo.iter_mut().zip(block) {
            fifo.extend(c.iter().map(|v| if v.is_finite() { *v } else { 0.0 }));
            // A ragged block is padded with silence, so the channels stay aligned.
            fifo.extend(std::iter::repeat_n(0.0, frames - c.len()));
        }
        lane.fresh = true;
        lane.last_pass = pass;
        lane.settings = settings;
        lane.reduction_db = reduction_db;
        if reduction_db >= lane.held.0 || lane.held.1.elapsed() > HOLD {
            lane.held = (reduction_db, Instant::now());
        }
        Ok(())
    }

    /// After every cook pass: each bus sums what it got and pushes it, or fades out when it got
    /// nothing while the show stands still (`playing` false), renders offline (`realtime`
    /// false), or for [`IDLE_PASSES`] passes. Drops the players of closed buses whose fade is
    /// done (which closes their device on its own thread, without waiting).
    pub fn after_pass(&self, playing: bool, realtime: bool) {
        let pass = {
            let mut p = lock(&self.inner.pass);
            *p += 1;
            *p
        };
        let muted = self.muted();
        for bus in lock(&self.inner.buses).values_mut() {
            bus.flush(pass, playing && realtime, muted);
        }
        let now = Instant::now();
        lock(&self.inner.closing).retain(|(at, _)| *at > now);
    }

    /// Mutes every output at once (a panic button), or unmutes them; ramped across one block.
    pub fn set_muted(&self, muted: bool) {
        self.inner.muted.store(muted, Ordering::Release);
    }

    pub fn muted(&self) -> bool {
        self.inner.muted.load(Ordering::Acquire)
    }

    /// Every bus.
    pub fn status(&self) -> Vec<BusStatus> {
        let muted = self.muted();
        lock(&self.inner.buses).iter().map(|(k, b)| b.status(k, muted)).collect()
    }

    /// The bus of device `key`.
    pub fn bus_status(&self, key: &str) -> Option<BusStatus> {
        let muted = self.muted();
        lock(&self.inner.buses).get(key.trim()).map(|b| b.status(key.trim(), muted))
    }

    /// The lane of node `node`, with the key of its bus.
    pub fn lane_status(&self, node: NodeId) -> Option<(String, LaneStatus)> {
        let buses = lock(&self.inner.buses);
        buses.iter().find_map(|(k, b)| {
            b.lanes.get(&node).map(|l| {
                (
                    k.clone(),
                    LaneStatus {
                        node,
                        rate: l.rate,
                        channels: l.fifo.len(),
                        reduction_db: l.reduction_db,
                        reduction_held_db: l.held.0,
                    },
                )
            })
        })
    }

    /// What an offline bus was pushed so far, per channel (empty for a device bus).
    pub fn pushed(&self, key: &str) -> Vec<Vec<f32>> {
        match lock(&self.inner.buses).get(key.trim()).map(|b| &b.sink) {
            Some(Sink::Offline { history, .. }) => history.clone(),
            _ => Vec::new(),
        }
    }

    /// Plays an offline bus's next samples into `out` (interleaved, its channels); silence for a
    /// device bus or none.
    pub fn pull(&self, key: &str, out: &mut [f32]) {
        out.fill(0.0);
        if let Some(Sink::Offline { engine, .. }) =
            lock(&self.inner.buses).get_mut(key.trim()).map(|b| &mut b.sink)
        {
            engine.pull(out);
        }
    }

    /// Players closing, waiting for their fade.
    pub fn closing(&self) -> usize {
        lock(&self.inner.closing).len()
    }

    /// The app closes: every device closes now.
    pub fn shutdown(&self) {
        lock(&self.inner.buses).clear();
        lock(&self.inner.closing).clear();
    }
}
