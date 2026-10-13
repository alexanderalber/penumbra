//! Watching files that nodes read (shader sources, images), so an edit on disk recooks the node:
//! hot reload for any node that reads a file.
//!
//! One thread per [`Watcher`] polls the modification time and length of every watched file a
//! few times per second. Polling instead of OS notifications: a handful of files, no extra
//! dependency, the same behaviour on every platform and on network drives, and editors that
//! save by replacing the file are covered without special cases.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use penumbra_core::{NodeId, lock};

/// How often watched files are looked at.
pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Modification time and length; `None` while the file is missing.
type Stamp = Option<(SystemTime, u64)>;

struct Entry {
    key: u64,
    path: PathBuf,
    node: NodeId,
    stamp: Stamp,
    changed: Arc<AtomicBool>,
}

#[derive(Default)]
struct Shared {
    entries: Vec<Entry>,
    next_key: u64,
}

/// Polls files on its own thread and reports the node of a file that changed.
pub(crate) struct Watcher {
    shared: Arc<Mutex<Shared>>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// `changed` receives the node of every watched file that changed. Without a thread (the
    /// OS refused one) files are simply not watched; that is logged, not fatal.
    pub(crate) fn new(changed: Sender<NodeId>) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let weak = Arc::downgrade(&shared);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new().name("file-watch".into()).spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(WATCH_INTERVAL) {
                if !poll(&weak, &changed) {
                    return;
                }
            }
        });
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                log::error!("cannot start the file watcher, hot reload is off: {e}");
                None
            }
        };
        Self { shared, stop: Some(stop_tx), thread }
    }

    /// Starts watching `path` for `node` until the returned handle is dropped.
    pub(crate) fn watch(&self, path: &Path, node: NodeId) -> FileWatch {
        let mut shared = lock(&self.shared);
        let key = shared.next_key;
        shared.next_key += 1;
        let changed = Arc::new(AtomicBool::new(false));
        shared.entries.push(Entry {
            key,
            path: path.to_path_buf(),
            node,
            stamp: stamp(path),
            changed: changed.clone(),
        });
        FileWatch { shared: Arc::downgrade(&self.shared), key, path: path.to_path_buf(), changed }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            log::error!("the file watcher panicked");
        }
    }
}

/// One pass over the watched files; `false` once the watcher is gone.
fn poll(shared: &Weak<Mutex<Shared>>, changed: &Sender<NodeId>) -> bool {
    let Some(shared) = shared.upgrade() else { return false };
    // Stat outside the lock, so `watch` and dropping handles never wait for the disk.
    let paths: Vec<(u64, PathBuf)> = {
        let s = lock(&shared);
        s.entries.iter().map(|e| (e.key, e.path.clone())).collect()
    };
    let stamps: Vec<(u64, Stamp)> = paths.into_iter().map(|(k, p)| (k, stamp(&p))).collect();
    let mut s = lock(&shared);
    for (key, now) in stamps {
        if let Some(e) = s.entries.iter_mut().find(|e| e.key == key)
            && e.stamp != now
        {
            e.stamp = now;
            e.changed.store(true, Ordering::Release);
            // A closed channel means the cooker is gone; the next pass ends the thread.
            changed.send(e.node).ok();
        }
    }
    true
}

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Keeps a file watched; dropping it stops watching. Operators keep it in their state, so a
/// file is watched as long as the node that reads it exists.
pub struct FileWatch {
    shared: Weak<Mutex<Shared>>,
    key: u64,
    path: PathBuf,
    changed: Arc<AtomicBool>,
}

impl FileWatch {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file changed since the last call (or since watching began). Tells a node
    /// that recooks for several reasons whether to read the file again.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

impl Drop for FileWatch {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            lock(&shared).entries.retain(|e| e.key != self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_change_reports_the_node_until_the_handle_is_dropped() {
        let dir = std::env::temp_dir().join(format!("penumbra-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(tx);
        let handle = watcher.watch(&file, NodeId(7));
        std::fs::write(&file, "two, longer").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let got = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap();
        assert_eq!(got, NodeId(7));
        assert!(handle.take_changed());
        assert!(!handle.take_changed());
        drop(handle);
        std::fs::write(&file, "three, longer still").unwrap();
        assert!(rx.recv_timeout(WATCH_INTERVAL * 3).is_err());
        drop(watcher);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
