//! Persistent state: what nodes hold beyond their params (a feedback loop's last frame, a held
//! cache, a locked node's outputs, later a tracker's ids) is written to `state/latest.pnstate` in
//! the project directory every [`StateConfig::interval`] and when the app closes, and read back
//! when the GPU arrives at the next start.
//!
//! Operators' state is kept only in projects that switch it on (the project setting
//! [`PERSIST_STATE`], off by default). Locked nodes' outputs are kept regardless, since that is
//! what a lock promises; with the setting off, a snapshot is only written when they changed. A
//! fresh start (`--fresh`, [`StateShared::start_fresh`]) and the Helpers button
//! ([`crate::Engine::discard_state`]) discard the operators' state.
//!
//! Nothing here may stall the show:
//! - **On the cook thread** a snapshot only collects handles ([`Cooker::kept`]) and records one
//!   GPU copy per texture into buffers that map later ([`Gpu::read_async`]); the cook thread's
//!   per-frame poll sees them arrive.
//! - **On the writer thread** channels and tables are encoded and, with the mapped rows, go
//!   into `latest.pnstate.tmp`, which is synced and renamed over `latest.pnstate` after the old
//!   one became `previous.pnstate`. A crash mid-write leaves the last good files untouched; a
//!   crash between the two renames leaves the new file as `latest.pnstate.tmp`, which reading
//!   takes when there is no `latest.pnstate`.
//! - **Reading** happens on the thread that asks (the startup restore thread, a command), and so
//!   does the upload of the textures; the cook thread only hands the results to the operators.
//!
//! The file is hostile input when read: every count and size is checked against
//! [`StateLimits`] before anything is allocated for it, a checksum covers the whole file, a file
//! from another format version is refused. A file whose content is wrong is moved aside to
//! `broken.pnstate` (so the next save cannot rotate it into `previous`) and the previous one is
//! tried. A file that cannot be opened or read now (another program holds it) stays where it
//! is.
//!
//! Format (version 1, little endian): `PNSTATE\0`, `u32` version, `u32` manifest length, the
//! manifest (JSON: which nodes, what kind of state, which items with their sizes), the items'
//! bytes one after the other, and a `u64` checksum of everything before it. Textures are stored
//! as they live on the GPU (RGBA half floats, or 32-bit floats for a simulation that keeps its
//! state in them; linear, premultiplied, rows top to bottom), so a restored loop continues bit
//! for bit; channels and tables as JSON; operator bytes as they are.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use penumbra_core::{Channel, Chans, Error, Name, NodeId, Table, lock};
use penumbra_gpu::{Gpu, PendingRead, ReadPool, ReadState, RowsError, Tex};
use penumbra_graph::{Cooker, Data, DataKind, Kept, Network, PERSIST_STATE, StateItem};
use serde::{Deserialize, Serialize};

/// Directory inside the project.
pub const DIR: &str = "state";
pub const LATEST: &str = "latest.pnstate";
pub const PREVIOUS: &str = "previous.pnstate";
pub const BROKEN: &str = "broken.pnstate";
pub const DISCARDED: &str = "discarded.pnstate";
const TMP: &str = "latest.pnstate.tmp";
const MAGIC: &[u8; 8] = b"PNSTATE\0";
pub const VERSION: u32 = 1;
/// How long a snapshot may wait for the GPU before it is given up.
const GPU_TIMEOUT: Duration = Duration::from_secs(10);

/// Budgets for reading (and writing) a state file. A file over one is refused before the
/// memory for it is allocated; a snapshot over one leaves out its largest nodes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateLimits {
    pub max_file_bytes: u64,
    pub max_manifest_bytes: u32,
    pub max_nodes: usize,
    pub max_items: usize,
    /// Largest side of a stored texture (the GPU's limit is checked again on upload).
    pub max_side: u32,
    /// Largest channel, table or operator item.
    pub max_blob_bytes: u64,
}

impl Default for StateLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 30,
            max_manifest_bytes: 4 << 20,
            max_nodes: 4096,
            max_items: 1 << 16,
            max_side: 16384,
            max_blob_bytes: 64 << 20,
        }
    }
}

/// When state is kept (`state.config`). Not stored in the project: every start begins with the
/// defaults. Whether operators' state is kept at all is the project setting [`PERSIST_STATE`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateConfig {
    #[serde(serialize_with = "seconds")]
    pub interval: Duration,
    /// Read `latest.pnstate` back when the GPU arrives.
    pub restore_on_start: bool,
    pub limits: StateLimits,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self { interval: Duration::from_secs(60), restore_on_start: true, limits: StateLimits::default() }
    }
}

fn seconds<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

/// The result of one save.
#[derive(Clone, Debug, Serialize)]
pub struct SaveReport {
    pub seq: u64,
    pub file: String,
    pub bytes: u64,
    /// Paths of the nodes in the file.
    pub nodes: Vec<String>,
    /// Nodes left out, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Seconds since 1970 when it was written.
    pub unix_time: f64,
    /// Time on the cook thread to collect and record the copies.
    pub collect_ms: f64,
    /// From the copies to the mapped buffers.
    pub gpu_ms: f64,
    /// Writing the file on the writer thread.
    pub write_ms: f64,
}

/// The result of one restore.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RestoreReport {
    /// The file read, if any.
    pub file: Option<String>,
    /// Paths whose state was put back.
    pub restored: Vec<String>,
    /// Nodes in the file that were not, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Files that did not read (moved aside to `broken.pnstate`), with the reason.
    pub broken: Vec<(String, String)>,
    /// Files that could not be opened or read now, left where they are, with the reason.
    pub unreadable: Vec<(String, String)>,
    /// Why nothing was restored, when nothing was.
    pub note: Option<String>,
    pub ms: f64,
}

/// What the commands and the UI see.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StateStatus {
    /// Saves asked for so far (periodic, `state.save`, closing).
    pub requested: u64,
    /// The last save that finished, successfully or not.
    pub finished: u64,
    pub saves: u64,
    pub failures: u64,
    pub last_save: Option<SaveReport>,
    /// The last failure, with the save it belongs to.
    pub last_error: Option<(u64, String)>,
    pub restore: Option<RestoreReport>,
    /// The startup restore has not finished, so nothing is saved yet (it would overwrite the
    /// file before it was read).
    pub holding: bool,
}

/// Shared between the engine, the cook thread and the writer thread.
#[derive(Default)]
pub struct StateShared {
    config: Mutex<StateConfig>,
    status: Mutex<StateStatus>,
    changed: Condvar,
    /// The start restore leaves the operators' state out (`--fresh`).
    fresh: AtomicBool,
}

impl StateShared {
    pub fn config(&self) -> StateConfig {
        lock(&self.config).clone()
    }

    pub fn set_config(&self, config: StateConfig) {
        *lock(&self.config) = config;
    }

    pub fn status(&self) -> StateStatus {
        lock(&self.status).clone()
    }

    /// Makes the start restore leave the operators' state out, as `--fresh` asks. Call before
    /// the GPU arrives.
    pub fn start_fresh(&self) {
        self.fresh.store(true, Ordering::Release);
    }

    /// Whether the start restore leaves the operators' state out (once: it clears the flag).
    pub(crate) fn take_fresh(&self) -> bool {
        self.fresh.swap(false, Ordering::AcqRel)
    }

    /// [`StateStatus::finished`], without copying the rest.
    pub fn finished(&self) -> u64 {
        lock(&self.status).finished
    }

    /// [`StateStatus::holding`], without copying the rest.
    pub fn holding(&self) -> bool {
        lock(&self.status).holding
    }

    pub(crate) fn update(&self, f: impl FnOnce(&mut StateStatus)) {
        f(&mut lock(&self.status));
        self.changed.notify_all();
    }

    /// Records that save `seq` failed, logging why.
    fn failed(&self, seq: u64, why: String) {
        log::warn!("state snapshot {seq}: {why}");
        self.update(|s| {
            s.finished = s.finished.max(seq);
            s.failures += 1;
            s.last_error = Some((seq, why));
        });
    }

    /// Waits until save `seq` finished; its report or its error.
    pub fn wait_for(&self, seq: u64, timeout: Duration) -> Result<SaveReport, Error> {
        let guard = lock(&self.status);
        let (status, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |s| s.finished < seq)
            .unwrap_or_else(PoisonError::into_inner);
        if status.finished < seq {
            return Err(Error::new("the state snapshot did not finish in time"));
        }
        if let Some((s, e)) = &status.last_error
            && *s == seq
        {
            return Err(Error::new(e.clone()));
        }
        match &status.last_save {
            Some(r) if r.seq == seq => Ok(r.clone()),
            _ => Err(Error::new("the snapshot was replaced by a newer one")),
        }
    }
}

/// The state directory of a project.
pub fn dir_of(project: &Path) -> PathBuf {
    project.join(DIR)
}

// ---------------------------------------------------------------------------------------------
// The file

/// What kind of state a node entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum What {
    Outputs,
    State,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ItemEntry {
    /// RGBA half floats, or 32-bit floats with `float` (a simulation's state). Files written
    /// before 32-bit floats existed have no `float` and hold half floats.
    Tex {
        width: u32,
        height: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        float: bool,
    },
    Chan {
        len: u64,
    },
    Table {
        len: u64,
    },
    Bytes {
        len: u64,
    },
}

impl ItemEntry {
    fn len(&self) -> u64 {
        match self {
            ItemEntry::Tex { width, height, float } => u64::from(*width)
                .saturating_mul(u64::from(*height))
                .saturating_mul(if *float { 16 } else { 8 }),
            ItemEntry::Chan { len } | ItemEntry::Table { len } | ItemEntry::Bytes { len } => *len,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEntry {
    pub path: String,
    pub kind: String,
    pub what: What,
    pub items: Vec<ItemEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The app that wrote it (`penumbra 0.1.0`).
    pub app: String,
    /// Show frame and seconds since 1970 when the snapshot was taken.
    pub frame: i64,
    pub unix_time: f64,
    pub nodes: Vec<NodeEntry>,
}

/// An item read from a file.
#[derive(Clone, Debug, PartialEq)]
pub enum SavedItem {
    /// `width * height * 8` bytes of RGBA half floats, or `width * height * 16` bytes of 32-bit
    /// floats with `float`.
    Tex {
        width: u32,
        height: u32,
        float: bool,
        bytes: Vec<u8>,
    },
    Chan(Chans),
    Table(Table),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedNode {
    pub path: String,
    pub kind: String,
    pub what: What,
    pub items: Vec<SavedItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub app: String,
    pub frame: i64,
    pub unix_time: f64,
    pub nodes: Vec<SavedNode>,
}

/// A checksum over a byte stream, eight bytes at a time (FNV-style multiply, then a final
/// avalanche). Not cryptographic: it catches truncation and corruption, not intent.
struct Hasher {
    h: u64,
    tail: [u8; 8],
    n: usize,
    total: u64,
}

impl Hasher {
    fn new() -> Self {
        Self { h: 0xcbf2_9ce4_8422_2325, tail: [0; 8], n: 0, total: 0 }
    }

    fn word(&mut self, w: u64) {
        self.h = (self.h ^ w).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23);
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        while self.n > 0 && !bytes.is_empty() {
            let Some((first, rest)) = bytes.split_first() else { break };
            if let Some(slot) = self.tail.get_mut(self.n) {
                *slot = *first;
            }
            self.n += 1;
            bytes = rest;
            if self.n == 8 {
                self.word(u64::from_le_bytes(self.tail));
                self.n = 0;
            }
        }
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.word(u64::from_le_bytes(*w));
        }
        for b in rest {
            if let Some(slot) = self.tail.get_mut(self.n) {
                *slot = *b;
            }
            self.n += 1;
        }
    }

    fn finish(mut self) -> u64 {
        let mut last = [0u8; 8];
        for (d, s) in last.iter_mut().zip(self.tail.iter().take(self.n)) {
            *d = *s;
        }
        self.word(u64::from_le_bytes(last));
        self.word(self.total);
        let mut h = self.h;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        h
    }
}

/// Writes through to `inner` and hashes what passes.
struct Hashing<W> {
    inner: W,
    hash: Hasher,
}

impl<W: Write> Hashing<W> {
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.hash.update(bytes);
        self.inner.write_all(bytes)
    }
}

/// Reads from `inner` and hashes what passes.
struct Hashed<R> {
    inner: R,
    hash: Hasher,
    left: u64,
}

impl<R: Read> Hashed<R> {
    /// Exactly `n` bytes; `n` was checked against the budgets and the bytes left in the file.
    fn take(&mut self, n: u64) -> Result<Vec<u8>, Error> {
        if n > self.left {
            return Err(Error::new("the file is shorter than its manifest says (truncated?)"));
        }
        let len = usize::try_from(n).map_err(|_| Error::new("item too large"))?;
        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf).map_err(|e| Error::new(format!("reading: {e}")))?;
        self.left -= n;
        self.hash.update(&buf);
        Ok(buf)
    }
}

/// Writes a state file to `w`: the manifest, then each item's bytes from `payload(node, item,
/// sink)` in manifest order. The payload callback must write exactly the item's length.
fn write_file<W: Write>(
    w: W,
    manifest: &Manifest,
    mut payload: impl FnMut(usize, usize, &mut dyn FnMut(&[u8]) -> std::io::Result<()>) -> Result<(), Error>,
) -> Result<W, Error> {
    let json = serde_json::to_vec(manifest).map_err(|e| Error::new(format!("manifest: {e}")))?;
    let json_len = u32::try_from(json.len()).map_err(|_| Error::new("manifest too large"))?;
    let io = |e: std::io::Error| Error::new(format!("writing: {e}"));
    let mut out = Hashing { inner: w, hash: Hasher::new() };
    out.put(MAGIC).map_err(io)?;
    out.put(&VERSION.to_le_bytes()).map_err(io)?;
    out.put(&json_len.to_le_bytes()).map_err(io)?;
    out.put(&json).map_err(io)?;
    for (n, node) in manifest.nodes.iter().enumerate() {
        for (i, item) in node.items.iter().enumerate() {
            let mut written = 0u64;
            payload(n, i, &mut |bytes| {
                written += bytes.len() as u64;
                out.put(bytes)
            })?;
            if written != item.len() {
                return Err(Error::new(format!(
                    "{}: item {i} has {written} bytes, the manifest says {}",
                    node.path,
                    item.len()
                )));
            }
        }
    }
    let sum = out.hash.finish();
    let mut inner = out.inner;
    inner.write_all(&sum.to_le_bytes()).map_err(io)?;
    Ok(inner)
}

/// Reads a state file of `len` bytes from `r`, checking everything against `limits`.
pub fn read_file<R: Read>(r: R, len: u64, limits: &StateLimits) -> Result<Saved, Error> {
    if len > limits.max_file_bytes {
        return Err(Error::new(format!("{len} bytes, more than the {} allowed", limits.max_file_bytes)));
    }
    if len < 24 {
        return Err(Error::new("too short for a state file"));
    }
    // Everything but the trailing checksum is hashed.
    let mut f = Hashed { inner: r, hash: Hasher::new(), left: len - 8 };
    let head = f.take(16)?;
    let (magic, rest) = head.split_at(8);
    if magic != MAGIC {
        return Err(Error::new("not a penumbra state file"));
    }
    let word = |b: &[u8]| b.try_into().map(u32::from_le_bytes).map_err(|_| Error::new("bad header"));
    let version = word(rest.get(0..4).unwrap_or_default())?;
    if version != VERSION {
        return Err(Error::new(format!("format version {version}; this build reads {VERSION}")));
    }
    let manifest_len = word(rest.get(4..8).unwrap_or_default())?;
    if manifest_len > limits.max_manifest_bytes {
        return Err(Error::new(format!("manifest of {manifest_len} bytes is over the budget")));
    }
    let json = f.take(u64::from(manifest_len))?;
    let manifest: Manifest =
        serde_json::from_slice(&json).map_err(|e| Error::new(format!("manifest: {e}")))?;
    if manifest.nodes.len() > limits.max_nodes {
        return Err(Error::new(format!("{} nodes, more than {}", manifest.nodes.len(), limits.max_nodes)));
    }
    let items: usize = manifest.nodes.iter().map(|n| n.items.len()).sum();
    if items > limits.max_items {
        return Err(Error::new(format!("{items} items, more than {}", limits.max_items)));
    }
    // All sizes first, so nothing is allocated for a file that cannot hold what it claims.
    let mut total = 0u64;
    for node in &manifest.nodes {
        for item in &node.items {
            match item {
                ItemEntry::Tex { width, height, .. } => {
                    if *width == 0 || *height == 0 || *width > limits.max_side || *height > limits.max_side {
                        return Err(Error::new(format!("{}: a {width}x{height} texture", node.path)));
                    }
                }
                other if other.len() > limits.max_blob_bytes => {
                    return Err(Error::new(format!("{}: an item of {} bytes", node.path, other.len())));
                }
                _ => {}
            }
            total = total.saturating_add(item.len());
        }
    }
    if total != f.left {
        return Err(Error::new(format!(
            "the manifest lists {total} bytes of items, the file has {} (truncated or padded)",
            f.left
        )));
    }
    let mut nodes = Vec::with_capacity(manifest.nodes.len());
    for node in &manifest.nodes {
        let mut items = Vec::with_capacity(node.items.len());
        for item in &node.items {
            let bytes = f.take(item.len())?;
            items.push(match item {
                ItemEntry::Tex { width, height, float } => {
                    SavedItem::Tex { width: *width, height: *height, float: *float, bytes }
                }
                ItemEntry::Bytes { .. } => SavedItem::Bytes(bytes),
                // Parsed after the checksum (below): a corrupt file should say so first.
                ItemEntry::Chan { .. } | ItemEntry::Table { .. } => SavedItem::Bytes(bytes),
            });
        }
        nodes.push(items);
    }
    let expected = f.hash.finish();
    let mut sum = [0u8; 8];
    f.inner.read_exact(&mut sum).map_err(|e| Error::new(format!("reading the checksum: {e}")))?;
    if u64::from_le_bytes(sum) != expected {
        return Err(Error::new("checksum mismatch: the file is damaged"));
    }
    let mut out = Vec::with_capacity(nodes.len());
    for (entry, items) in manifest.nodes.into_iter().zip(nodes) {
        let mut parsed = Vec::with_capacity(items.len());
        for (spec, item) in entry.items.iter().zip(items) {
            parsed.push(match (spec, item) {
                (ItemEntry::Chan { .. }, SavedItem::Bytes(b)) => {
                    SavedItem::Chan(chans_from_json(&b).map_err(|e| e.context(entry.path.clone()))?)
                }
                (ItemEntry::Table { .. }, SavedItem::Bytes(b)) => {
                    SavedItem::Table(table_from_json(&b).map_err(|e| e.context(entry.path.clone()))?)
                }
                (_, item) => item,
            });
        }
        out.push(SavedNode { path: entry.path, kind: entry.kind, what: entry.what, items: parsed });
    }
    Ok(Saved { app: manifest.app, frame: manifest.frame, unix_time: manifest.unix_time, nodes: out })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChansJson {
    rate: f64,
    channels: Vec<(String, Vec<f64>)>,
}

fn chans_to_json(c: &Chans) -> Result<Vec<u8>, Error> {
    let j = ChansJson {
        rate: c.rate(),
        channels: c.channels().iter().map(|ch| (ch.name.as_str().to_string(), ch.samples.clone())).collect(),
    };
    serde_json::to_vec(&j).map_err(|e| Error::new(format!("channels: {e}")))
}

fn chans_from_json(b: &[u8]) -> Result<Chans, Error> {
    let j: ChansJson = serde_json::from_slice(b).map_err(|e| Error::new(format!("channels: {e}")))?;
    let mut channels = Vec::with_capacity(j.channels.len());
    for (name, samples) in j.channels {
        channels.push(Channel { name: Name::new(&name)?, samples });
    }
    Chans::new(j.rate, channels)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableJson {
    rows: Vec<Vec<String>>,
}

fn table_to_json(t: &Table) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&TableJson { rows: t.rows().to_vec() }).map_err(|e| Error::new(format!("table: {e}")))
}

fn table_from_json(b: &[u8]) -> Result<Table, Error> {
    let j: TableJson = serde_json::from_slice(b).map_err(|e| Error::new(format!("table: {e}")))?;
    Table::new(j.rows)
}

/// Writes `file` as a complete state file from items already on the CPU (tests, tools).
pub fn encode(saved: &Saved) -> Result<Vec<u8>, Error> {
    let mut manifest =
        Manifest { app: saved.app.clone(), frame: saved.frame, unix_time: saved.unix_time, nodes: vec![] };
    let mut blobs: Vec<Vec<Vec<u8>>> = Vec::new();
    for node in &saved.nodes {
        let mut entries = Vec::new();
        let mut bytes = Vec::new();
        for item in &node.items {
            let (entry, b) = match item {
                SavedItem::Tex { width, height, float, bytes } => {
                    (ItemEntry::Tex { width: *width, height: *height, float: *float }, bytes.clone())
                }
                SavedItem::Chan(c) => {
                    let b = chans_to_json(c)?;
                    (ItemEntry::Chan { len: b.len() as u64 }, b)
                }
                SavedItem::Table(t) => {
                    let b = table_to_json(t)?;
                    (ItemEntry::Table { len: b.len() as u64 }, b)
                }
                SavedItem::Bytes(b) => (ItemEntry::Bytes { len: b.len() as u64 }, b.clone()),
            };
            entries.push(entry);
            bytes.push(b);
        }
        manifest.nodes.push(NodeEntry {
            path: node.path.clone(),
            kind: node.kind.clone(),
            what: node.what,
            items: entries,
        });
        blobs.push(bytes);
    }
    write_file(Vec::new(), &manifest, |n, i, sink| {
        let b = blobs.get(n).and_then(|b| b.get(i)).ok_or_else(|| Error::new("no such item"))?;
        sink(b).map_err(|e| Error::new(e.to_string()))
    })
}

/// Reads a state file from disk.
pub fn read_path(path: &Path, limits: &StateLimits) -> Result<Saved, Error> {
    read_checked(path, limits).map_err(|f| match f {
        ReadFailure::Io(e) | ReadFailure::Content(e) => e,
    })
}

/// Why a state file did not read.
enum ReadFailure {
    /// Opening or reading failed. The file itself may be fine: another program may hold it
    /// open (a virus scanner, a backup), and the next try may read it.
    Io(Error),
    /// The content is wrong: damaged, truncated, another format.
    Content(Error),
}

/// Passes reads through and notes whether one failed.
struct Watched<R> {
    inner: R,
    failed: bool,
}

impl<R: Read> Read for Watched<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let r = self.inner.read(buf);
        if r.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::Interrupted) {
            self.failed = true;
        }
        r
    }
}

fn read_checked(path: &Path, limits: &StateLimits) -> Result<Saved, ReadFailure> {
    let io = |e: std::io::Error| ReadFailure::Io(Error::new(format!("{}: {e}", path.display())));
    let file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    let mut source = Watched { inner: BufReader::with_capacity(1 << 20, file), failed: false };
    read_file(&mut source, len, limits).map_err(|e| {
        let e = e.context(path.display().to_string());
        if source.failed { ReadFailure::Io(e) } else { ReadFailure::Content(e) }
    })
}

// ---------------------------------------------------------------------------------------------
// Saving: cook thread and writer thread

/// An item of a snapshot on its way to the writer thread.
enum ItemOut {
    /// Texture `index` of the readback.
    Tex(usize),
    /// Channels or a table: a handle, encoded as JSON on the writer thread.
    Data(Data),
    Bytes(Vec<u8>),
}

/// A node of a snapshot: its manifest entry (the lengths of channels and tables are filled in
/// on the writer thread), its items and the bytes known so far.
type JobNode = (NodeEntry, Vec<ItemOut>, u64);

struct WriteJob {
    seq: u64,
    frame: i64,
    unix_time: f64,
    nodes: Vec<JobNode>,
    limits: StateLimits,
    read: Option<PendingRead>,
    skipped: Vec<(String, String)>,
    collect: Duration,
    gpu: Duration,
}

struct Pending {
    job: WriteJob,
    started: Instant,
}

/// The cook thread's side: decides when to save, collects, waits for the GPU without blocking,
/// hands over to the writer thread.
pub struct StateKeeper {
    shared: Arc<StateShared>,
    dir: PathBuf,
    writer: Option<(Sender<WriteJob>, JoinHandle<()>)>,
    pending: Option<Pending>,
    /// When the last periodic snapshot started (or the keeper did): the next is due one
    /// interval later, so a changed interval applies at once.
    last: Instant,
    /// Explicit requests (`state.save`, closing) not yet started.
    asked: Vec<u64>,
    /// Readback buffers the writer gave back, for the next snapshot.
    pool: Arc<Mutex<ReadPool>>,
    /// The last snapshot handed to the writer; the next one waits until it is written (one
    /// set of readback buffers, reused).
    sent: u64,
    /// With [`PERSIST_STATE`] off: the locked outputs the last snapshot held, so an unchanged
    /// set is not written again. `None`: unknown, the next periodic snapshot writes.
    written: Option<Vec<(NodeId, Vec<usize>)>>,
}

impl StateKeeper {
    pub fn new(shared: Arc<StateShared>, project_dir: &Path) -> Self {
        let holding = shared.config().restore_on_start;
        shared.update(|s| s.holding = holding);
        Self {
            shared,
            dir: dir_of(project_dir),
            writer: None,
            pending: None,
            last: Instant::now(),
            asked: Vec::new(),
            pool: Arc::new(Mutex::new(ReadPool::default())),
            sent: 0,
            // No file yet: nothing to write until something is locked.
            written: (!dir_of(project_dir).join(LATEST).exists()).then(Vec::new),
        }
    }

    /// Whether a snapshot now would write something new: always with [`PERSIST_STATE`] on,
    /// else only when the locked outputs changed since the last one.
    pub fn due(&self, cooker: &Cooker, net: &Network) -> bool {
        net.switch(PERSIST_STATE) || self.written.as_ref() != Some(&locked_outputs(cooker))
    }

    /// Forgets what was written, so the next periodic snapshot writes (after a discard).
    pub fn invalidate(&mut self) {
        self.written = None;
    }

    /// Queues an explicit save; returns its number, for [`StateShared::wait_for`].
    pub fn request(&mut self) -> Result<u64, Error> {
        if self.shared.holding() {
            return Err(Error::new(
                "the saved state has not been read back yet (no GPU so far?); saving now would overwrite it",
            ));
        }
        let mut seq = 0;
        self.shared.update(|s| {
            s.requested += 1;
            seq = s.requested;
        });
        self.asked.push(seq);
        Ok(seq)
    }

    /// After every cook pass: moves a pending snapshot along and starts one when due.
    /// `periodic` false leaves out the periodic snapshot; requested ones still run.
    pub fn after_pass(
        &mut self,
        cooker: &Cooker,
        net: &Network,
        gpu: Option<&Gpu>,
        frame: i64,
        periodic: bool,
    ) {
        let now = Instant::now();
        if let Some(p) = &self.pending {
            let state = p.job.read.as_ref().map_or(ReadState::Ready, PendingRead::state);
            let timed_out = now.duration_since(p.started) > GPU_TIMEOUT;
            match (state, timed_out) {
                (ReadState::Ready, _) => {
                    if let Some(mut p) = self.pending.take() {
                        p.job.gpu = now.duration_since(p.started);
                        self.send(p.job);
                    }
                }
                (ReadState::Failed, _) | (ReadState::Waiting, true) => {
                    if let Some(p) = self.pending.take() {
                        let why = if state == ReadState::Failed {
                            "the GPU could not map the readback"
                        } else {
                            "the GPU did not deliver the readback in 10 s"
                        };
                        self.fail(p.job.seq, why.into());
                    }
                }
                (ReadState::Waiting, false) => {}
            }
            return;
        }
        if self.shared.finished() < self.sent {
            return;
        }
        let config = self.shared.config();
        let periodic = periodic && now >= self.last + config.interval && !self.shared.holding();
        if !periodic && self.asked.is_empty() {
            return;
        }
        self.last = now;
        if self.asked.is_empty() && !self.due(cooker, net) {
            return;
        }
        self.written = (!net.switch(PERSIST_STATE)).then(|| locked_outputs(cooker));
        let seq = match self.asked.pop() {
            Some(seq) => {
                // Older requests are answered by this one too.
                for older in std::mem::take(&mut self.asked) {
                    self.fail(older, "replaced by a newer snapshot".into());
                }
                seq
            }
            None => {
                let mut seq = 0;
                self.shared.update(|s| {
                    s.requested += 1;
                    seq = s.requested;
                });
                seq
            }
        };
        let mut pool = std::mem::take(&mut *lock(&self.pool));
        let collected = collect(cooker, net, gpu, frame, &config.limits, seq, &mut pool);
        // Buffers of sizes no longer wanted are let go.
        drop(pool);
        match collected {
            Ok(job) => self.pending = Some(Pending { job, started: Instant::now() }),
            Err(e) => self.fail(seq, e.0),
        }
    }

    fn fail(&mut self, seq: u64, why: String) {
        self.written = None;
        self.shared.failed(seq, why);
    }

    fn send(&mut self, job: WriteJob) {
        if self.writer.is_none() {
            let (tx, rx) = mpsc::channel::<WriteJob>();
            let (shared, pool) = (self.shared.clone(), self.pool.clone());
            let dir = self.dir.clone();
            match std::thread::Builder::new()
                .name("state-writer".into())
                .spawn(move || writer(&rx, &shared, &dir, &pool))
            {
                Ok(handle) => self.writer = Some((tx, handle)),
                Err(e) => return self.fail(job.seq, format!("cannot start the writer thread: {e}")),
            }
        }
        let seq = job.seq;
        let sent = self.writer.as_ref().is_some_and(|(tx, _)| tx.send(job).is_ok());
        if sent {
            self.sent = seq;
        } else {
            self.writer = None;
            self.fail(seq, "the writer thread has stopped".into());
        }
    }

    /// Whether a snapshot is on its way to the file.
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.shared.finished() < self.sent
    }
}

impl Drop for StateKeeper {
    fn drop(&mut self) {
        if let Some((tx, handle)) = self.writer.take() {
            drop(tx);
            if handle.join().is_err() {
                log::error!("the state writer panicked");
            }
        }
    }
}

/// What a snapshot holds now, without touching the GPU: per node its path, kind and items
/// (`state.info`).
pub fn summary(cooker: &Cooker, net: &Network) -> Vec<(String, String, What, usize, u64)> {
    cooker
        .kept()
        .into_iter()
        .filter_map(|(id, kept)| {
            let def = net.node(id)?;
            let path = net.path_of(id)?.to_string();
            let (what, items) = match &kept {
                Kept::Outputs(d) => (What::Outputs, d.iter().map(data_bytes).sum::<u64>()),
                Kept::State(s) => (
                    What::State,
                    s.iter()
                        .map(|i| match i {
                            StateItem::Data(d) => data_bytes(d),
                            StateItem::Bytes(b) => b.len() as u64,
                        })
                        .sum(),
                ),
            };
            let n = match &kept {
                Kept::Outputs(d) => d.len(),
                Kept::State(s) => s.len(),
            };
            Some((path, def.kind.clone(), what, n, items))
        })
        .collect()
}

/// The locked nodes and the addresses of the outputs they hold, to tell whether they changed.
fn locked_outputs(cooker: &Cooker) -> Vec<(NodeId, Vec<usize>)> {
    cooker
        .kept()
        .into_iter()
        .filter_map(|(id, k)| match k {
            Kept::Outputs(d) => Some((id, d.iter().map(Data::address).collect())),
            Kept::State(_) => None,
        })
        .collect()
}

fn data_bytes(d: &Data) -> u64 {
    d.get::<Tex>().map_or(0, Tex::bytes)
}

/// Turns the cooker's kept state into a write job, recording the texture copies.
fn collect(
    cooker: &Cooker,
    net: &Network,
    gpu: Option<&Gpu>,
    frame: i64,
    limits: &StateLimits,
    seq: u64,
    pool: &mut ReadPool,
) -> Result<WriteJob, Error> {
    let started = Instant::now();
    let mut nodes: Vec<JobNode> = Vec::new();
    let mut skipped = Vec::new();
    let mut kept = cooker.kept();
    if !net.switch(PERSIST_STATE) {
        kept.retain(|(_, k)| matches!(k, Kept::Outputs(_)));
    }
    for (id, kept) in kept {
        let (Some(def), Some(path)) = (net.node(id), net.path_of(id)) else { continue };
        let path = path.to_string();
        let (what, list): (What, Vec<StateItem>) = match kept {
            Kept::Outputs(d) => (What::Outputs, d.into_iter().map(StateItem::Data).collect()),
            Kept::State(s) => (What::State, s),
        };
        match items_of(list, limits) {
            Ok(items) => {
                let bytes = items.iter().map(|(e, _)| e.len()).sum();
                let (entries, items): (Vec<ItemEntry>, Vec<ItemOut>) = items.into_iter().unzip();
                nodes.push((NodeEntry { path, kind: def.kind.clone(), what, items: entries }, items, bytes));
            }
            Err(why) => skipped.push((path, why)),
        }
    }
    // Channels and tables count once they are encoded, on the writer thread, which checks the
    // budget again.
    trim(&mut nodes, limits, &mut skipped);
    let textures: Vec<&Tex> = nodes
        .iter()
        .flat_map(|(_, items, _)| items.iter())
        .filter_map(|item| match item {
            ItemOut::Data(d) => d.get::<Tex>(),
            _ => None,
        })
        .collect();
    let read = if textures.is_empty() {
        None
    } else {
        let gpu = gpu.ok_or_else(|| Error::new("textures to keep but no GPU"))?;
        Some(gpu.read_async_exact(&textures, pool)?)
    };
    // The textures become their place in the readback, in the order they were listed.
    let mut index = 0;
    for (_, items, _) in &mut nodes {
        for item in items.iter_mut() {
            if matches!(item, ItemOut::Data(d) if d.get::<Tex>().is_some()) {
                *item = ItemOut::Tex(index);
                index += 1;
            }
        }
    }
    let unix_time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    Ok(WriteJob {
        seq,
        frame,
        unix_time,
        nodes,
        limits: limits.clone(),
        read,
        skipped,
        collect: started.elapsed(),
        gpu: Duration::ZERO,
    })
}

/// Over the budget: leaves out the largest nodes first, keeping the rest in path order.
fn trim<T>(nodes: &mut Vec<(NodeEntry, T, u64)>, limits: &StateLimits, skipped: &mut Vec<(String, String)>) {
    let budget = limits.max_file_bytes.saturating_sub(limits.max_manifest_bytes.into()).saturating_sub(64);
    let mut total: u64 = nodes.iter().map(|(_, _, b)| *b).fold(0, u64::saturating_add);
    if nodes.len() <= limits.max_nodes && total <= budget {
        return;
    }
    nodes.sort_by_key(|(_, _, b)| std::cmp::Reverse(*b));
    while (total > budget || nodes.len() > limits.max_nodes) && !nodes.is_empty() {
        let (entry, _, b) = nodes.remove(0);
        total = total.saturating_sub(b);
        skipped.push((entry.path, format!("{b} bytes would exceed the state budget")));
    }
    nodes.sort_by(|a, b| a.0.path.cmp(&b.0.path));
}

/// The manifest entries and items of one node. Channels and tables get their length when the
/// writer thread encodes them.
fn items_of(list: Vec<StateItem>, limits: &StateLimits) -> Result<Vec<(ItemEntry, ItemOut)>, String> {
    let mut out = Vec::with_capacity(list.len());
    for s in list {
        let (entry, item) = match s {
            StateItem::Data(d) => match d.kind() {
                DataKind::Tex => {
                    let t = d.get::<Tex>().ok_or("a texture output without a texture")?;
                    let float = t.format() == penumbra_gpu::FLOAT_FORMAT;
                    (ItemEntry::Tex { width: t.width(), height: t.height(), float }, ItemOut::Data(d))
                }
                DataKind::Chan => {
                    d.get::<Chans>().ok_or("a channel output without channels")?;
                    (ItemEntry::Chan { len: 0 }, ItemOut::Data(d))
                }
                DataKind::Table => {
                    d.get::<Table>().ok_or("a table output without a table")?;
                    (ItemEntry::Table { len: 0 }, ItemOut::Data(d))
                }
                DataKind::Geo => return Err("geometry is not kept (it comes from its file again)".into()),
                DataKind::Audio => return Err("audio is not kept (it streams, the next block is new)".into()),
                DataKind::Vol => {
                    return Err("volumes are not kept as such (a node keeps them as pictures)".into());
                }
                DataKind::Bodies => {
                    return Err("groups of bodies are not kept (they come from their table again)".into());
                }
                DataKind::Mat | DataKind::Light | DataKind::View => {
                    return Err(
                        "materials, lights and cameras are not kept (they come from their params again)"
                            .into(),
                    );
                }
            },
            StateItem::Bytes(b) => (ItemEntry::Bytes { len: b.len() as u64 }, ItemOut::Bytes(b)),
        };
        check_blob(&entry, limits)?;
        out.push((entry, item));
    }
    Ok(out)
}

fn check_blob(entry: &ItemEntry, limits: &StateLimits) -> Result<(), String> {
    if !matches!(entry, ItemEntry::Tex { .. }) && entry.len() > limits.max_blob_bytes {
        return Err(format!("an item of {} bytes is over the budget", entry.len()));
    }
    Ok(())
}

/// What the writer puts into the file for one item.
enum Payload {
    /// Texture `index` of the readback.
    Tex(usize),
    Blob(Vec<u8>),
}

/// On the writer thread: encodes the channels and tables, leaves out the nodes that are then
/// over a budget and makes the manifest.
fn finish(
    job_nodes: Vec<JobNode>,
    job: &WriteJob,
    skipped: &mut Vec<(String, String)>,
) -> (Manifest, Vec<Vec<Payload>>) {
    let mut nodes: Vec<(NodeEntry, Vec<Payload>, u64)> = Vec::with_capacity(job_nodes.len());
    'nodes: for (mut entry, items, _) in job_nodes {
        let mut payloads = Vec::with_capacity(items.len());
        for (spec, item) in entry.items.iter_mut().zip(items) {
            let payload = match item {
                ItemOut::Tex(i) => Payload::Tex(i),
                ItemOut::Bytes(b) => Payload::Blob(b),
                ItemOut::Data(d) => {
                    let encoded = match (d.get::<Chans>(), d.get::<Table>()) {
                        (Some(c), _) => chans_to_json(c),
                        (None, Some(t)) => table_to_json(t),
                        (None, None) => Err(Error::new("neither channels nor a table")),
                    };
                    let b = match encoded {
                        Ok(b) => b,
                        Err(e) => {
                            skipped.push((entry.path, e.0));
                            continue 'nodes;
                        }
                    };
                    match spec {
                        ItemEntry::Chan { len } | ItemEntry::Table { len } | ItemEntry::Bytes { len } => {
                            *len = b.len() as u64;
                        }
                        ItemEntry::Tex { .. } => {}
                    }
                    Payload::Blob(b)
                }
            };
            if let Err(why) = check_blob(spec, &job.limits) {
                skipped.push((entry.path, why));
                continue 'nodes;
            }
            payloads.push(payload);
        }
        let bytes = entry.items.iter().map(ItemEntry::len).fold(0, u64::saturating_add);
        nodes.push((entry, payloads, bytes));
    }
    trim(&mut nodes, &job.limits, skipped);
    let (entries, payloads) = nodes.into_iter().map(|(e, p, _)| (e, p)).unzip();
    let manifest = Manifest {
        app: format!("penumbra {}", crate::VERSION),
        frame: job.frame,
        unix_time: job.unix_time,
        nodes: entries,
    };
    (manifest, payloads)
}

fn writer(rx: &Receiver<WriteJob>, shared: &StateShared, dir: &Path, pool: &Mutex<ReadPool>) {
    while let Ok(mut job) = rx.recv() {
        let seq = job.seq;
        let started = Instant::now();
        let job_nodes = std::mem::take(&mut job.nodes);
        let mut skipped = std::mem::take(&mut job.skipped);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (manifest, payloads) = finish(job_nodes, &job, &mut skipped);
            let written = write_job(&manifest, &payloads, job.read.as_ref(), dir)?;
            Ok::<_, Error>((written, manifest.nodes.into_iter().map(|n| n.path).collect::<Vec<_>>()))
        }));
        let write = started.elapsed();
        if let Some(read) = job.read.take() {
            lock(pool).append(read.recycle());
        }
        match result {
            Ok(Ok(((file, bytes), nodes))) => {
                let report = SaveReport {
                    seq,
                    file: file.display().to_string(),
                    bytes,
                    nodes,
                    skipped,
                    unix_time: job.unix_time,
                    collect_ms: job.collect.as_secs_f64() * 1000.0,
                    gpu_ms: job.gpu.as_secs_f64() * 1000.0,
                    write_ms: write.as_secs_f64() * 1000.0,
                };
                shared.update(|s| {
                    s.finished = s.finished.max(seq);
                    s.saves += 1;
                    s.last_save = Some(report);
                });
            }
            Ok(Err(e)) => shared.failed(seq, e.0),
            Err(_) => shared.failed(seq, "writing the state panicked".into()),
        }
    }
}

/// Writes the snapshot to `latest.pnstate.tmp`, then rotates: `latest` becomes `previous`, the
/// temporary file becomes `latest`.
fn write_job(
    manifest: &Manifest,
    payloads: &[Vec<Payload>],
    read: Option<&PendingRead>,
    dir: &Path,
) -> Result<(PathBuf, u64), Error> {
    let io = |what: &Path, e: std::io::Error| Error::new(format!("{}: {e}", what.display()));
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        // Runtime state, not content: keep it out of a project's git repository.
        std::fs::write(&ignore, "# penumbra runtime state, rewritten every minute\n*\n")
            .map_err(|e| io(&ignore, e))?;
    }
    let tmp = dir.join(TMP);
    let file = File::create(&tmp).map_err(|e| io(&tmp, e))?;
    let result = write_file(BufWriter::with_capacity(1 << 20, file), manifest, |n, i, sink| {
        match payloads.get(n).and_then(|v| v.get(i)) {
            Some(Payload::Blob(b)) => sink(b).map_err(|e| Error::new(format!("writing: {e}"))),
            Some(Payload::Tex(t)) => {
                let read = read.ok_or_else(|| Error::new("no readback"))?;
                read.rows(*t, |row| sink(row)).map_err(|e| match e {
                    RowsError::Gpu(e) => e,
                    RowsError::Sink(e) => Error::new(format!("writing: {e}")),
                })
            }
            None => Err(Error::new("no such item")),
        }
    })
    .and_then(|w| {
        let file = w.into_inner().map_err(|e| Error::new(format!("writing: {}", e.error())))?;
        file.sync_all().map_err(|e| io(&tmp, e))
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let bytes = std::fs::metadata(&tmp).map_err(|e| io(&tmp, e))?.len();
    let latest = dir.join(LATEST);
    if latest.exists() {
        let previous = dir.join(PREVIOUS);
        std::fs::rename(&latest, &previous).map_err(|e| io(&previous, e))?;
    }
    std::fs::rename(&tmp, &latest).map_err(|e| io(&latest, e))?;
    Ok((latest, bytes))
}

// ---------------------------------------------------------------------------------------------
// Restoring

/// Reads the newest state file that reads (`file`, or `latest`, then the temporary file when
/// there is no `latest`, then `previous` in `dir`), moving one whose content is wrong aside.
/// Runs on the caller's thread.
pub fn load(
    dir: &Path,
    file: Option<&Path>,
    limits: &StateLimits,
    report: &mut RestoreReport,
) -> Option<Saved> {
    if let Some(f) = file {
        return match read_path(f, limits) {
            Ok(s) => {
                report.file = Some(f.display().to_string());
                Some(s)
            }
            Err(e) => {
                report.broken.push((f.display().to_string(), e.0));
                None
            }
        };
    }
    let latest = dir.join(LATEST);
    for name in [LATEST, TMP, PREVIOUS] {
        let path = dir.join(name);
        // The temporary file counts only without a `latest`: then a save stopped between its
        // two renames, and the temporary file is the newest snapshot. Next to a `latest` it is
        // a write in progress or one that failed.
        if !path.exists() || (name == TMP && latest.exists()) {
            continue;
        }
        match read_checked(&path, limits) {
            Ok(s) if name == TMP => {
                // Back in the rotation, so the next save keeps it as `previous`.
                let used = match std::fs::rename(&path, &latest) {
                    Ok(()) => latest.clone(),
                    Err(e) => {
                        log::warn!("state: cannot rename {} to {}: {e}", path.display(), latest.display());
                        path
                    }
                };
                log::info!("state: the last save had not finished its renames; using {}", used.display());
                report.file = Some(used.display().to_string());
                return Some(s);
            }
            Ok(s) => {
                report.file = Some(path.display().to_string());
                return Some(s);
            }
            // An unfinished write: the next save writes over it.
            Err(ReadFailure::Content(e) | ReadFailure::Io(e)) if name == TMP => {
                log::info!("state: an unfinished save is not used: {e}");
            }
            Err(ReadFailure::Content(e)) => {
                log::warn!("state: {e}");
                report.broken.push((path.display().to_string(), e.0));
                set_aside(&path, &dir.join(BROKEN));
            }
            Err(ReadFailure::Io(e)) => {
                log::warn!("state: {e}; the file stays where it is");
                report.unreadable.push((path.display().to_string(), e.0));
            }
        }
    }
    if report.broken.is_empty() {
        report.note = Some(if report.unreadable.is_empty() {
            "no saved state".into()
        } else {
            "the saved state could not be read now (held by another program?)".into()
        });
    }
    None
}

/// Takes a file that does not read out of the rotation, kept as `broken` for a look. Should
/// the move fail, the file is deleted instead: left in place, the next save would rotate a
/// broken `latest` into `previous`.
fn set_aside(path: &Path, broken: &Path) {
    let Err(e) = std::fs::rename(path, broken) else { return };
    log::warn!("state: cannot move {} to {}: {e}; deleting it instead", path.display(), broken.display());
    if let Err(e) = std::fs::remove_file(path) {
        log::error!(
            "state: cannot delete {} either: {e}; the next save may keep it as the previous state",
            path.display()
        );
    }
}

/// The saved nodes turned into what the cooker takes, textures uploaded (on the caller's
/// thread). Nodes that do not fit the network are listed in the report.
pub fn prepare(
    saved: Saved,
    net: &Network,
    gpu: Option<&Gpu>,
    take_state: bool,
    report: &mut RestoreReport,
) -> Vec<(NodeId, String, Kept)> {
    let mut out = Vec::new();
    let mut seen = BTreeMap::new();
    for node in saved.nodes {
        let skip = |report: &mut RestoreReport, why: String| report.skipped.push((node.path.clone(), why));
        let Ok(path) = penumbra_core::NodePath::parse(&node.path) else {
            skip(report, "not a node path".into());
            continue;
        };
        let Some(id) = net.resolve(&path) else {
            skip(report, "no such node now".into());
            continue;
        };
        if seen.insert(id, ()).is_some() {
            skip(report, "listed twice".into());
            continue;
        }
        let Some(def) = net.node(id) else { continue };
        if node.what == What::State && !take_state {
            let why = if net.switch(PERSIST_STATE) {
                "left out: a fresh start"
            } else {
                "left out: persistent state is off in this project"
            };
            skip(report, why.into());
            continue;
        }
        if def.kind != node.kind {
            skip(report, format!("now a `{}`, saved as a `{}`", def.kind, node.kind));
            continue;
        }
        let mut datas = Vec::with_capacity(node.items.len());
        let mut failed = None;
        for item in node.items {
            let item = match item {
                SavedItem::Tex { width, height, float, mut bytes } => {
                    let Some(gpu) = gpu else {
                        failed = Some("no GPU to upload the texture to".to_string());
                        break;
                    };
                    let uploaded = if float {
                        gpu.upload_rgba32f(width, height, &bytes, "restored")
                    } else {
                        gpu.upload_rgba16f(width, height, &mut bytes, "restored")
                    };
                    match uploaded {
                        Ok(t) => StateItem::Data(Data::new(DataKind::Tex, t)),
                        Err(e) => {
                            failed = Some(e.0);
                            break;
                        }
                    }
                }
                SavedItem::Chan(c) => StateItem::Data(Data::new(DataKind::Chan, c)),
                SavedItem::Table(t) => StateItem::Data(Data::new(DataKind::Table, t)),
                SavedItem::Bytes(b) => StateItem::Bytes(b),
            };
            datas.push(item);
        }
        if let Some(why) = failed {
            skip(report, why);
            continue;
        }
        let kept = match node.what {
            What::State => Kept::State(datas),
            What::Outputs => {
                let mut outs = Vec::with_capacity(datas.len());
                for d in datas {
                    match d {
                        StateItem::Data(d) => outs.push(d),
                        StateItem::Bytes(_) => {
                            failed = Some("raw bytes as an output".to_string());
                            break;
                        }
                    }
                }
                if let Some(why) = failed {
                    skip(report, why);
                    continue;
                }
                Kept::Outputs(outs)
            }
        };
        out.push((id, node.path, kept));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use half::f16;

    fn sample() -> Saved {
        let tex: Vec<u8> =
            (0..3 * 2 * 4).flat_map(|i| f16::from_f32(i as f32 * 0.25).to_le_bytes()).collect();
        Saved {
            app: "test".into(),
            frame: 42,
            unix_time: 1.5,
            nodes: vec![
                SavedNode {
                    path: "/fb".into(),
                    kind: "tex.feedback".into(),
                    what: What::State,
                    items: vec![SavedItem::Tex { width: 3, height: 2, float: false, bytes: tex }],
                },
                SavedNode {
                    path: "/held".into(),
                    kind: "chan.constant".into(),
                    what: What::Outputs,
                    items: vec![SavedItem::Chan(
                        Chans::new(
                            60.0,
                            vec![Channel { name: Name::new("x").unwrap(), samples: vec![1.0, 2.5] }],
                        )
                        .unwrap(),
                    )],
                },
                SavedNode {
                    path: "/t".into(),
                    kind: "table.text".into(),
                    what: What::Outputs,
                    items: vec![SavedItem::Table(Table::new(vec![vec!["a".into(), "b".into()]]).unwrap())],
                },
                SavedNode {
                    path: "/track".into(),
                    kind: "x.y".into(),
                    what: What::State,
                    items: vec![SavedItem::Bytes(b"{\"ids\": [1, 2]}".to_vec()), SavedItem::Bytes(vec![])],
                },
            ],
        }
    }

    fn read(bytes: &[u8]) -> Result<Saved, Error> {
        read_file(bytes, bytes.len() as u64, &StateLimits::default())
    }

    #[test]
    fn round_trip() {
        let s = sample();
        let bytes = encode(&s).unwrap();
        assert_eq!(read(&bytes).unwrap(), s);
        let empty = Saved { nodes: vec![], ..s };
        assert_eq!(read(&encode(&empty).unwrap()).unwrap(), empty);
    }

    #[test]
    fn hostile_files_are_errors() {
        let good = encode(&sample()).unwrap();
        // Every truncation.
        for n in 0..good.len() {
            assert!(read(&good[..n]).is_err(), "truncated to {n}");
        }
        // Padding.
        let mut long = good.clone();
        long.push(0);
        assert!(read(&long).is_err());
        // Every single flipped bit in a stretch of the file, and every byte set to 0xff.
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 1 << (i % 8);
            assert!(read(&b).is_err(), "bit flip at {i}");
            let mut b = good.clone();
            if b[i] != 0xff {
                b[i] = 0xff;
                assert!(read(&b).is_err(), "0xff at {i}");
            }
        }
        // Another version, another magic.
        let mut v = good.clone();
        v[8] = 2;
        assert!(read(&v).unwrap_err().0.contains("version 2"));
        assert!(read(b"PNG\r\n\x1a\n\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap_err().0.contains("not a"));
        // A file claiming more than its budget is refused before reading it.
        let small = StateLimits { max_file_bytes: 100, ..StateLimits::default() };
        assert!(read_file(&good[..], good.len() as u64, &small).unwrap_err().0.contains("allowed"));
        // A manifest claiming a huge texture, with a correct checksum: refused by the budget,
        // not by an allocation failure.
        let manifest = Manifest {
            app: "x".into(),
            frame: 0,
            unix_time: 0.0,
            nodes: vec![NodeEntry {
                path: "/fb".into(),
                kind: "tex.feedback".into(),
                what: What::State,
                items: vec![ItemEntry::Tex { width: 100_000, height: 100_000, float: false }],
            }],
        };
        let forged = write_file(Vec::new(), &manifest, |_, _, sink| {
            // The writer refuses an item that is not as long as declared; build it by hand.
            let _ = sink;
            Ok(())
        });
        assert!(forged.is_err());
        let json = serde_json::to_vec(&manifest).unwrap();
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(json.len() as u32).to_le_bytes());
        b.extend_from_slice(&json);
        let mut h = Hasher::new();
        h.update(&b);
        b.extend_from_slice(&h.finish().to_le_bytes());
        assert!(read(&b).unwrap_err().0.contains("100000x100000"));
        // Items whose bytes do not parse as what they claim, with a correct checksum.
        let manifest = Manifest {
            app: "x".into(),
            frame: 0,
            unix_time: 0.0,
            nodes: vec![NodeEntry {
                path: "/c".into(),
                kind: "chan.constant".into(),
                what: What::Outputs,
                items: vec![ItemEntry::Chan { len: 5 }],
            }],
        };
        let b = write_file(Vec::new(), &manifest, |_, _, sink| {
            sink(b"hello").map_err(|e| Error::new(e.to_string()))
        })
        .unwrap();
        assert!(read(&b).unwrap_err().0.contains("channels"));
    }

    #[test]
    fn a_save_cut_off_between_its_renames_is_found() {
        let scratch = penumbra_core::scratch::ScratchDir::unborn("state-tmp");
        let dir = scratch.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let newest = sample();
        let older = Saved { frame: 1, ..sample() };
        // `latest` became `previous`, then the app stopped before the temporary file was renamed.
        std::fs::write(dir.join(PREVIOUS), encode(&older).unwrap()).unwrap();
        std::fs::write(dir.join(TMP), encode(&newest).unwrap()).unwrap();
        let mut report = RestoreReport::default();
        let limits = StateLimits::default();
        assert_eq!(load(&dir, None, &limits, &mut report), Some(newest.clone()));
        assert!(report.file.as_deref().unwrap().ends_with(LATEST), "{report:?}");
        assert!(dir.join(LATEST).exists() && !dir.join(TMP).exists(), "back in the rotation");
        assert!(report.broken.is_empty());

        // Next to a `latest`, a temporary file is a write in progress and is not read.
        std::fs::write(dir.join(TMP), encode(&older).unwrap()).unwrap();
        let mut report = RestoreReport::default();
        assert_eq!(load(&dir, None, &limits, &mut report), Some(newest));

        // An unfinished write without a `latest`: skipped, not set aside.
        std::fs::remove_file(dir.join(LATEST)).unwrap();
        let half = encode(&older).unwrap();
        std::fs::write(dir.join(TMP), &half[..half.len() / 2]).unwrap();
        let mut report = RestoreReport::default();
        assert_eq!(load(&dir, None, &limits, &mut report), Some(older));
        assert!(report.file.as_deref().unwrap().ends_with(PREVIOUS), "{report:?}");
        assert!(report.broken.is_empty() && !dir.join(BROKEN).exists(), "{report:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_another_program_holds_is_left_in_place() {
        use std::os::windows::fs::OpenOptionsExt;
        let scratch = penumbra_core::scratch::ScratchDir::unborn("state-held");
        let dir = scratch.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let older = Saved { frame: 1, ..sample() };
        std::fs::write(dir.join(LATEST), encode(&sample()).unwrap()).unwrap();
        std::fs::write(dir.join(PREVIOUS), encode(&older).unwrap()).unwrap();
        // No sharing: opening it again fails with a sharing violation, as with a virus scanner.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.join(LATEST)).unwrap();
        let mut report = RestoreReport::default();
        assert_eq!(load(&dir, None, &StateLimits::default(), &mut report), Some(older));
        drop(held);
        assert_eq!(report.unreadable.len(), 1, "{report:?}");
        assert!(report.broken.is_empty() && !dir.join(BROKEN).exists(), "{report:?}");
        assert_eq!(read_path(&dir.join(LATEST), &StateLimits::default()).unwrap(), sample());
    }

    #[test]
    fn channels_and_tables_are_encoded_on_the_writer_within_the_budget() {
        let chans =
            Chans::new(60.0, vec![Channel { name: Name::new("x").unwrap(), samples: vec![0.5; 100] }])
                .unwrap();
        let table = Table::new(vec![vec!["a".into()]]).unwrap();
        let node = |path: &str, item: ItemOut, entry: ItemEntry| {
            (
                NodeEntry { path: path.into(), kind: "k".into(), what: What::Outputs, items: vec![entry] },
                vec![item],
                0,
            )
        };
        let job = |limits: StateLimits| WriteJob {
            seq: 1,
            frame: 7,
            unix_time: 2.0,
            nodes: vec![
                node(
                    "/c",
                    ItemOut::Data(Data::new(DataKind::Chan, chans.clone())),
                    ItemEntry::Chan { len: 0 },
                ),
                node(
                    "/t",
                    ItemOut::Data(Data::new(DataKind::Table, table.clone())),
                    ItemEntry::Table { len: 0 },
                ),
            ],
            limits,
            read: None,
            skipped: vec![],
            collect: Duration::ZERO,
            gpu: Duration::ZERO,
        };
        let mut j = job(StateLimits::default());
        let mut skipped = vec![];
        let (manifest, payloads) = finish(std::mem::take(&mut j.nodes), &j, &mut skipped);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(
            manifest.nodes[0].items,
            [ItemEntry::Chan { len: chans_to_json(&chans).unwrap().len() as u64 }]
        );
        let bytes = write_file(Vec::new(), &manifest, |n, i, sink| match &payloads[n][i] {
            Payload::Blob(b) => sink(b).map_err(|e| Error::new(e.to_string())),
            Payload::Tex(_) => Err(Error::new("no texture here")),
        })
        .unwrap();
        let back = read(&bytes).unwrap();
        assert_eq!(back.nodes[0].items, [SavedItem::Chan(chans.clone())]);
        assert_eq!(back.nodes[1].items, [SavedItem::Table(table.clone())]);

        // Over the item budget once encoded: the node is left out, the rest is written.
        let mut j = job(StateLimits { max_blob_bytes: 100, ..StateLimits::default() });
        let mut skipped = vec![];
        let (manifest, _) = finish(std::mem::take(&mut j.nodes), &j, &mut skipped);
        assert_eq!(manifest.nodes.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), ["/t"]);
        assert_eq!(skipped[0].0, "/c", "{skipped:?}");
    }

    #[test]
    fn the_hash_does_not_depend_on_how_bytes_arrive() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut a = Hasher::new();
        a.update(&data);
        let mut b = Hasher::new();
        for chunk in data.chunks(13) {
            b.update(chunk);
        }
        assert_eq!(a.finish(), b.finish());
        let mut c = Hasher::new();
        c.update(&data[..999]);
        let mut d = Hasher::new();
        d.update(&data[..999]);
        d.update(&[0]);
        assert_ne!(c.finish(), d.finish(), "a trailing zero changes the hash");
    }
}
