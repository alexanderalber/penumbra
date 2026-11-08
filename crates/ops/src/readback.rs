//! One wait per frame for the nodes that read the GPU back (`tex.blob_track`,
//! `chan.from_tex`, the picture inputs of `script.python`). Each waits a few milliseconds for
//! the readback it just started, so its result belongs to this frame. Ten of them waited ten
//! times as long, frame after frame. They now share [`FRAME_WAIT`]: the engine refills
//! [`ReadWaits`] before each pass, and a node whose turn finds it used up only looks whether
//! its readback is there; a late result shows a frame later.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_gpu::{Gpu, PendingRead, ReadState};
use penumbra_graph::CookContext;

/// Most a pass waits for readbacks, all nodes together, while the show runs in real time.
pub const FRAME_WAIT: Duration = Duration::from_millis(4);

/// What is left of this pass's wait for readbacks. A cooker resource; without it each node
/// waits as long as it asks.
pub struct ReadWaits {
    left: Mutex<Duration>,
}

impl Default for ReadWaits {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadWaits {
    pub fn new() -> Self {
        Self { left: Mutex::new(FRAME_WAIT) }
    }

    /// Gives the next pass its whole wait again.
    pub fn refill(&self) {
        *lock(&self.left) = FRAME_WAIT;
    }

    /// Takes up to `want` of what is left.
    fn take(&self, want: Duration) -> Duration {
        let mut left = lock(&self.left);
        let granted = want.min(*left);
        *left -= granted;
        granted
    }

    /// Gives back what a wait did not use.
    fn give_back(&self, unused: Duration) {
        let mut left = lock(&self.left);
        *left = left.saturating_add(unused).min(FRAME_WAIT);
    }
}

/// Waits for `read` at most `want`, and in a live show at most what is left of the pass's
/// shared wait. While every frame counts (stepping, rendering), the whole `want`.
pub(crate) fn wait(cx: &CookContext<'_>, gpu: &Gpu, read: &PendingRead, want: Duration) -> ReadState {
    let shared = if cx.exact_frames() { None } else { cx.resource::<ReadWaits>() };
    let Some(waits) = shared else { return read.wait(gpu, want) };
    let granted = waits.take(want);
    let started = Instant::now();
    let state = read.wait(gpu, granted);
    waits.give_back(granted.saturating_sub(started.elapsed()));
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waits_of_a_pass_share_one_budget() {
        let w = ReadWaits::new();
        assert_eq!(w.take(Duration::from_millis(3)), Duration::from_millis(3));
        assert_eq!(w.take(Duration::from_millis(3)), Duration::from_millis(1));
        assert_eq!(w.take(Duration::from_millis(3)), Duration::ZERO);
        // A wait that came back early leaves the rest to the next node.
        w.give_back(Duration::from_millis(2));
        assert_eq!(w.take(Duration::from_millis(3)), Duration::from_millis(2));
        w.give_back(Duration::from_secs(1));
        assert_eq!(w.take(Duration::from_secs(1)), FRAME_WAIT, "never more than one pass's wait");
        w.refill();
        assert_eq!(w.take(Duration::from_secs(1)), FRAME_WAIT);
    }
}
