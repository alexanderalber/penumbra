//! Watching a sender from a thread, to wake its receiver when there is something to receive.
//!
//! Every [`POLL`] the thread looks up the sender. It wakes the receiver when the sender appears,
//! goes or changes its texture, and when a frame arrives: by the sender's frame counter where it
//! keeps one, otherwise every second poll, as Spout's own receivers copy every frame. Wakes
//! coalesce into one cook per frame, so waking more often than the show runs costs nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::d3d::Semaphore;
use crate::directory;
use crate::protocol::Info;

/// How often the thread looks.
const POLL: Duration = Duration::from_millis(2);

/// Stops its thread when dropped.
pub struct Watch {
    stop: Arc<AtomicBool>,
}

impl Watch {
    /// Watches sender `name` (empty: the default sender) and calls `wake` when there is
    /// something new.
    pub fn new(name: &str, wake: impl Fn() + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let watched = name.to_string();
        let spawned = std::thread::Builder::new().name("spout watch".into()).spawn(move || {
            let mut last: Option<(String, Info)> = None;
            let mut counter: Option<Semaphore> = None;
            let mut count = None;
            let mut tick = 0u64;
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(POLL);
                tick += 1;
                let resolved =
                    if watched.is_empty() { directory::default_sender() } else { Some(watched.clone()) };
                let now = resolved.and_then(|n| directory::info(&n).map(|i| (n, i)));
                if now != last {
                    counter =
                        now.as_ref().and_then(|(n, _)| Semaphore::open(&format!("{n}_Count_Semaphore")));
                    count = None;
                    last = now;
                    wake();
                    continue;
                }
                if last.is_none() {
                    continue;
                }
                match &counter {
                    Some(c) => {
                        let c = c.count();
                        if c != count {
                            count = c;
                            wake();
                        }
                    }
                    None if tick.is_multiple_of(2) => wake(),
                    None => {}
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("cannot start watching Spout sender `{}`: {e}", name);
        }
        Self { stop }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
