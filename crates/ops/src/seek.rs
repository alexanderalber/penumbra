//! Seek requests from outside the cook (`video_file.seek`, `audio.seek`), by node: `tex.video_file`
//! and `audio.file` jump to the seconds asked for at their next cook and play on from there.
//! Provided by the engine as a cooker resource; cheap to clone.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_core::{NodeId, lock};
use penumbra_graph::Waker;

#[derive(Clone, Default)]
pub struct Seeks(Arc<Mutex<BTreeMap<NodeId, Entry>>>);

struct Entry {
    waker: Waker,
    /// Seconds into the file asked for and not taken yet.
    pending: Option<f64>,
}

impl Seeks {
    pub fn new() -> Self {
        Self::default()
    }

    /// The node jumps to `seconds` into its file at its next cook. A later request before that
    /// cook wins. False when the node has not cooked yet.
    pub fn request(&self, node: NodeId, seconds: f64) -> bool {
        let mut entries = lock(&self.0);
        let Some(e) = entries.get_mut(&node) else { return false };
        e.pending = Some(if seconds.is_finite() { seconds.max(0.0) } else { 0.0 });
        e.waker.wake();
        true
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Registers the node's waker and takes the seek asked for since its last cook.
    pub fn take(&self, node: NodeId, waker: Waker) -> Option<f64> {
        let mut entries = lock(&self.0);
        let e = entries.entry(node).or_insert(Entry { waker: waker.clone(), pending: None });
        e.waker = waker;
        e.pending.take()
    }
}

/// Where a jump to `seconds` into a file of `duration` seconds (0: not known yet) lands on an
/// unwrapped playhead now at `head`: in the same pass while looping, so the count of loops goes
/// on; else the seconds themselves.
pub fn in_pass(head: f64, seconds: f64, duration: f64, looping: bool) -> f64 {
    let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    if looping && duration > 0.0 && head.is_finite() {
        let pass = (head.max(0.0) / duration).floor();
        pass * duration + seconds.min(duration)
    } else {
        seconds
    }
}
