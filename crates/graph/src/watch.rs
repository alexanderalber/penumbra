//! Watching files that nodes read (shader sources, images), so an edit on disk recooks the node:
//! hot reload for any node that reads a file.
//!
//! One thread per [`Watcher`] polls the modification time and length of every watched file a
//! few times per second. Polling instead of OS notifications: a handful of files, no extra
//! dependency, the same behaviour on every platform and on network drives, and editors that
//! save by replacing the file are covered without special cases.
//!
//! Only that thread touches the disk, so a slow or unreachable drive never holds up the cook
//! thread that asks for a watch. It takes a file's first stamp at its next pass; a file
//! modified since the watch began counts as changed then.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
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
    /// `None` until the poll thread first looked at the file.
    stamp: Option<Stamp>,
    /// When the watch began.
    since: SystemTime,
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
    /// Dropping it ends the thread. The thread is not joined: it may wait for a slow drive,
    /// and it holds nothing that needs it to finish.
    _stop: Sender<()>,
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
        if let Err(e) = thread {
            log::error!("cannot start the file watcher, hot reload is off: {e}");
        }
        Self { shared, _stop: stop_tx }
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
            stamp: None,
            since: SystemTime::now(),
            changed: changed.clone(),
        });
        FileWatch { shared: Arc::downgrade(&self.shared), key, path: path.to_path_buf(), changed }
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
        let Some(e) = s.entries.iter_mut().find(|e| e.key == key) else { continue };
        let report = match e.stamp {
            // File clocks can lag wall time: do not lose an edit just after the watch began.
            None => now.is_some_and(|(modified, _)| modified >= e.since - Duration::from_millis(100)),
            Some(before) => before != now,
        };
        e.stamp = Some(now);
        if report {
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
    fn an_edit_before_the_first_poll_survives_a_coarse_file_clock() {
        let dir = penumbra_core::scratch::ScratchDir::new("watch-coarse").unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        // Poll explicitly so the first look always happens after the edit.
        let (stop, _) = mpsc::channel();
        let watcher = Watcher { shared: Arc::default(), _stop: stop };
        let handle = watcher.watch(&file, NodeId(7));
        let since = lock(&watcher.shared).entries[0].since;
        std::fs::write(&file, "two, longer").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(since - Duration::from_millis(5))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        assert!(poll(&Arc::downgrade(&watcher.shared), &tx));
        assert_eq!(rx.try_recv().unwrap(), NodeId(7));
        assert!(handle.take_changed());
    }

    #[test]
    fn a_change_reports_the_node_until_the_handle_is_dropped() {
        let dir = penumbra_core::scratch::ScratchDir::new("watch").unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(tx);
        let handle = watcher.watch(&file, NodeId(7));
        // File times follow a coarse clock; an edit a moment after the watch began is newer.
        std::thread::sleep(Duration::from_millis(50));
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
    }

    #[test]
    fn an_old_file_is_not_reported_at_the_first_look_but_its_edit_is() {
        let dir = penumbra_core::scratch::ScratchDir::new("watch-old").unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(old).unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(tx);
        let handle = watcher.watch(&file, NodeId(3));
        assert!(rx.recv_timeout(WATCH_INTERVAL * 4).is_err(), "the first look is no change");
        assert!(!handle.take_changed());
        std::fs::write(&file, "two, longer").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), NodeId(3));
        assert!(handle.take_changed());
    }
}
