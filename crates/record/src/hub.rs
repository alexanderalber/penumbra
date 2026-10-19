//! The recordings of a process.
//!
//! Each `out.video` node attaches itself ([`Hub::attach`]) with a way to wake it. Commands and
//! the UI ask for a recording to start or stop ([`Hub::request`]); the node takes the request
//! at its next cook ([`Hub::take_request`]) and starts the recording through the hub
//! ([`Hub::start`]), which keeps its thread. The status of a node's current or last recording
//! stays readable here ([`Hub::status`]), also after the node's handle is gone.
//!
//! [`Hub::shutdown`] stops every recording and waits (bounded) for the files to be finished:
//! the app calls it when it closes, after the cook thread has stopped.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, lock};
use serde::Serialize;

use crate::writer::{self, Recording, Settings, Shared};

/// What a command or the UI asks of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Start,
    Stop,
}

/// Wakes a node so it cooks at the next frame boundary.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A recording as far as it got.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    /// The node recording: its name here, its path from the engine's commands.
    pub node: String,
    /// Taking frames.
    pub recording: bool,
    /// Stopped; the queued frames are being encoded and the file finished.
    pub finishing: bool,
    /// Ended: the file is finished (or failed, see `error`).
    pub done: bool,
    pub file: String,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Frames per second of the file.
    pub fps: f64,
    /// Frames written.
    pub frames: u64,
    /// Frames that were due while the queue was full (the encoder behind): the frame before
    /// lasts longer instead.
    pub dropped: u64,
    /// Frames of the file's rate the show did not cook (slower than the file's rate).
    pub skipped: u64,
    /// Length of the file so far.
    pub seconds: f64,
    /// Bytes of coded pictures written.
    pub bytes: u64,
    /// Frames per second the encoder manages (from the time it takes per frame).
    pub encoder_fps: f64,
    /// Frames waiting for the encoder.
    pub queue: usize,
    pub error: Option<String>,
}

#[derive(Default)]
struct Entry {
    /// The node's label (name).
    path: String,
    request: Option<Request>,
    wake: Option<Wake>,
    /// The node's current or last recording.
    current: Option<Arc<Shared>>,
    /// The node is gone (deleted, its project closed); only its last recording's status stays.
    detached: bool,
}

#[derive(Default)]
struct Inner {
    nodes: BTreeMap<NodeId, Entry>,
    /// Recording threads not yet joined.
    threads: Vec<JoinHandle<()>>,
    /// Shut down: no new recordings.
    closed: bool,
}

/// Cheap to clone; every clone is the same hub.
#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<Inner>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `node` under `label` (its name; the engine shows paths) with a way to wake it.
    /// Called on every cook; cheap.
    pub fn attach(&self, node: NodeId, label: &str, wake: impl FnOnce() -> Wake) {
        let mut inner = lock(&self.inner);
        let e = inner.nodes.entry(node).or_default();
        if e.detached {
            // Ids are reused across projects: a new node starts without the old one's status.
            *e = Entry::default();
        }
        if e.path != label {
            e.path = label.to_string();
        }
        if e.wake.is_none() {
            e.wake = Some(wake());
        }
    }

    /// Forgets `node` (deleted, or its project closed): no more requests; the status of its last
    /// recording stays readable. Its recording, if any, ends when the node drops its handle.
    pub fn detach(&self, node: NodeId) {
        let mut inner = lock(&self.inner);
        match inner.nodes.get_mut(&node) {
            Some(e) if e.current.is_some() => {
                e.detached = true;
                e.wake = None;
                e.request = None;
            }
            Some(_) => {
                inner.nodes.remove(&node);
            }
            None => {}
        }
    }

    /// Asks `node` to start or stop recording and wakes it. Fails for a node that is not an
    /// attached `out.video` (not cooked yet, or something else).
    pub fn request(&self, node: NodeId, request: Request) -> Result<(), String> {
        let wake = {
            let mut inner = lock(&self.inner);
            if inner.closed {
                return Err("the app is closing".into());
            }
            let e = inner
                .nodes
                .get_mut(&node)
                .filter(|e| !e.detached)
                .ok_or("not a video output that has cooked")?;
            e.request = Some(request);
            e.wake.clone()
        };
        if let Some(w) = wake {
            w();
        }
        Ok(())
    }

    /// The request waiting for `node`, taken.
    pub fn take_request(&self, node: NodeId) -> Option<Request> {
        lock(&self.inner).nodes.get_mut(&node).and_then(|e| e.request.take())
    }

    /// Starts a recording for `node`. Refused once the hub is shut down.
    pub fn start(&self, node: NodeId, settings: Settings) -> Result<Recording, String> {
        let mut inner = lock(&self.inner);
        if inner.closed {
            return Err("the app is closing".into());
        }
        let (finished, running): (Vec<_>, Vec<_>) =
            std::mem::take(&mut inner.threads).into_iter().partition(JoinHandle::is_finished);
        inner.threads = running;
        for t in finished {
            if t.join().is_err() {
                log::error!("a recording thread panicked");
            }
        }
        let path = inner.nodes.get(&node).map(|e| e.path.clone()).unwrap_or_default();
        let (recording, shared, handle) = writer::start(&path, settings)?;
        inner.threads.push(handle);
        inner.nodes.entry(node).or_default().current = Some(shared);
        Ok(recording)
    }

    /// The status of `node`'s current or last recording; `None` when it has not recorded (or
    /// is not attached).
    pub fn status(&self, node: NodeId) -> Option<Status> {
        let inner = lock(&self.inner);
        let e = inner.nodes.get(&node)?;
        let mut s = e.current.as_ref()?.status();
        s.node.clone_from(&e.path);
        Some(s)
    }

    /// Every attached node with its current or last recording (a default status, with only the
    /// node's label, for one that has not recorded).
    pub fn all(&self) -> Vec<(NodeId, Status)> {
        let inner = lock(&self.inner);
        inner
            .nodes
            .iter()
            .map(|(id, e)| {
                let mut s = e.current.as_ref().map(|c| c.status()).unwrap_or_default();
                s.node.clone_from(&e.path);
                (*id, s)
            })
            .collect()
    }

    /// Stops every recording and waits up to `timeout` for their files to be finished; no new
    /// ones start afterwards. Returns how many were still busy at the deadline.
    pub fn shutdown(&self, timeout: Duration) -> usize {
        let threads = {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            for e in inner.nodes.values() {
                if let Some(c) = &e.current {
                    c.stop.store(true, Ordering::Release);
                }
            }
            std::mem::take(&mut inner.threads)
        };
        let deadline = Instant::now() + timeout;
        while threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut busy = 0;
        for t in threads {
            if t.is_finished() {
                if t.join().is_err() {
                    log::error!("a recording thread panicked");
                }
            } else {
                busy += 1;
                log::warn!("a recording did not finish its file in time");
            }
        }
        busy
    }
}
