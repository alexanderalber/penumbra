//! What the file writing nodes share (`out.table`, `out.mesh`): requests and status by node
//! ([`Writers`], a cooker resource), and writing bytes to a file without a reader ever seeing
//! half of it ([`write_bytes`]).

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use penumbra_core::{NodeId, lock};
use penumbra_graph::Waker;

/// Where a table or mesh goes when `file` is empty, relative to the project.
pub const FOLDER: &str = "export";

/// What a writer's status says beside its own numbers.
pub trait Report: Clone + Default + Send + 'static {
    /// The node's path; the command fills it in.
    fn set_node(&mut self, node: String);
    fn set_written_seconds_ago(&mut self, seconds: Option<f64>);
    /// Why the last write failed.
    fn error(&self) -> Option<&str>;
}

/// Write requests from outside the cook (`table_out.write`, `mesh_out.write`) and the state of
/// every writer of one kind, by node. Provided by the engine as a cooker resource, one per
/// kind; cheap to clone.
pub struct Writers<S>(Arc<Mutex<BTreeMap<NodeId, Entry<S>>>>);

impl<S> Clone for Writers<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<S> Default for Writers<S> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

struct Entry<S> {
    waker: Waker,
    /// Requests made and requests answered (written or failed); a request is open while
    /// `requested > answered`.
    requested: u64,
    answered: u64,
    status: S,
    written: Option<Instant>,
}

impl<S: Report> Entry<S> {
    fn status(&self) -> S {
        let mut s = self.status.clone();
        s.set_written_seconds_ago(self.written.map(|at| at.elapsed().as_secs_f64()));
        s
    }
}

impl<S: Report> Writers<S> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the node to write now, whatever its mode. Returns the request's number for
    /// [`Writers::answered`]; `None` when the node has not cooked yet.
    pub fn request(&self, node: NodeId) -> Option<u64> {
        let mut entries = lock(&self.0);
        let e = entries.get_mut(&node)?;
        e.requested += 1;
        e.waker.wake();
        Some(e.requested)
    }

    /// The number of the last request the node has answered.
    pub fn answered(&self, node: NodeId) -> u64 {
        lock(&self.0).get(&node).map_or(0, |e| e.answered)
    }

    pub fn status(&self, node: NodeId) -> Option<S> {
        lock(&self.0).get(&node).map(Entry::status)
    }

    /// Every node that has cooked, in id order.
    pub fn all(&self) -> Vec<(NodeId, S)> {
        lock(&self.0).iter().map(|(id, e)| (*id, e.status())).collect()
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Registers the node's waker; returns the number of the newest request.
    pub(crate) fn check_in(&self, node: NodeId, waker: Waker) -> u64 {
        let mut entries = lock(&self.0);
        let e = entries.entry(node).or_insert_with(|| Entry {
            waker: waker.clone(),
            requested: 0,
            answered: 0,
            status: S::default(),
            written: None,
        });
        e.waker = waker;
        e.requested
    }

    pub(crate) fn report(&self, node: NodeId, status: S, written: Option<Instant>, answered: u64) {
        if let Some(e) = lock(&self.0).get_mut(&node) {
            e.status = status;
            e.written = written;
            e.answered = e.answered.max(answered);
        }
    }
}

/// Where a writer puts its file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Dest {
    pub path: PathBuf,
    /// The default folder, which the write may create.
    pub folder: Option<PathBuf>,
    /// Replace the file; off, write a new numbered file each time.
    pub overwrite: bool,
}

/// What [`write_bytes`] did.
#[derive(Debug)]
pub(crate) struct Written {
    /// The file written: the target's path, or a numbered name beside it.
    pub path: PathBuf,
    pub hash: u64,
    /// False when the bytes were the ones last written, or the file held them already.
    pub wrote: bool,
}

pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Writes `bytes` to `dest` unless `force` is off and they are the ones last written (`last`:
/// the target path and the hash) or, with `overwrite` on, the ones the file holds. With
/// `overwrite` off every write takes the next free numbered name
/// ([`penumbra_core::files::create_numbered`]).
pub(crate) fn write_bytes(
    bytes: &[u8],
    dest: &Dest,
    last: Option<&(PathBuf, u64)>,
    force: bool,
) -> Result<Written, String> {
    let name = dest.path.display();
    let len = bytes.len() as u64;
    let hash = hash(bytes);
    let kept = || Written { path: dest.path.clone(), hash, wrote: false };
    if !force {
        if last.is_some_and(|(p, h)| *p == dest.path && *h == hash) {
            return Ok(kept());
        }
        let same_len = std::fs::metadata(&dest.path).is_ok_and(|m| m.is_file() && m.len() == len);
        if dest.overwrite && same_len && std::fs::read(&dest.path).is_ok_and(|old| old == bytes) {
            return Ok(kept());
        }
    }
    if let Some(folder) = &dest.folder
        && !folder.is_dir()
    {
        std::fs::create_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    }
    if dest.overwrite {
        replace(&dest.path, bytes).map_err(|e| format!("{name}: {e}"))?;
        return Ok(Written { path: dest.path.clone(), hash, wrote: true });
    }
    // The new name is taken first, so a write running at the same time cannot take it too.
    let (file, path) = penumbra_core::files::create_numbered(&dest.path)?;
    drop(file);
    if let Err(e) = replace(&path, bytes) {
        std::fs::remove_file(&path).ok();
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(Written { path, hash, wrote: true })
}

/// Writes `bytes` to a temporary file beside `path` and renames it over `path`.
fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if file_name.is_empty() {
        return Err(std::io::Error::other("not a file name"));
    }
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        // Whatever is left of the temporary file; it may not exist.
        std::fs::remove_file(&tmp).ok();
    }
    result
}
