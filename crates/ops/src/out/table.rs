//! `out.table`: writes its input table to a CSV or TSV file.
//!
//! In mode `changes` the node writes whenever its input brings a table whose text differs from
//! what it wrote last; a file that already holds that text is not touched either. A
//! `table.file` reading the same file therefore settles after one round. In mode `manual` it
//! writes only when asked: `table_out.write` posts a request through [`TableOuts`], a cooker
//! resource, and wakes the node. With `overwrite` off every write goes to a new file, numbered
//! as `out.video` numbers its recordings ([`penumbra_core::files::create_numbered`]).
//!
//! The cook thread never touches the disk. A job turns the table into text, checks its size and
//! writes it to a temporary file beside the target, then renames that over the target, so a
//! reader never sees half a file. One write is in flight at a time; what arrives meanwhile is
//! written when it finishes.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use penumbra_core::{Error, NodeId, ParamSpec, Table, lock, table::Format};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator, Waker,
};
use serde::Serialize;

use crate::table::MAX_FILE_BYTES;

/// Kind of the table writing node.
pub const OUT_TABLE: &str = "out.table";
/// Where a table goes when `file` is empty, relative to the project.
pub const FOLDER: &str = "export";

const EXTENSIONS: &[&str] = &["csv", "tsv", "txt"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(OUT_TABLE, "Writes its input table to a CSV or TSV file")
            .param(ParamSpec::file("file", EXTENSIONS).doc(
                "The file to write, relative to the project; empty writes export/<node name>.csv (.tsv with \
                 format tsv). The node creates the export folder but no other",
            ))
            .param(ParamSpec::bool("overwrite", true).doc(
                "On: every write replaces the file, for a file another program keeps reading. Off: every \
                 write makes a new file with the next free name (_2, _3, ...); in mode changes that is a new \
                 file per change",
            ))
            .param(
                ParamSpec::choice("format", &["auto", "csv", "tsv"])
                    .doc("How the cells are written")
                    .choice_doc("auto", "by extension: csv for .csv, tsv for any other")
                    .choice_doc(
                        "csv",
                        "separated by commas; a cell with a comma, quote or line break in quotes",
                    )
                    .choice_doc(
                        "tsv",
                        "separated by tabs; a cell holding a tab or line break is an error, write it as csv",
                    ),
            )
            .param(
                ParamSpec::choice("mode", &["changes", "manual"])
                    .doc("When the node writes")
                    .choice_doc("changes", "whenever the table differs from what the file holds")
                    .choice_doc("manual", "only on the command table_out.write"),
            )
            .input("in", DataKind::Table)
            .describe_input("in", "The table to write"),
        || Box::new(TableOut::default()) as Box<dyn Operator>,
    ))
}

/// What `table_out.status` tells about one node.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    /// The node's path; the command fills it in.
    pub node: String,
    /// The file the node writes to.
    pub file: String,
    pub mode: String,
    /// Seconds since the node last wrote the file; `None` before its first write.
    pub written_seconds_ago: Option<f64>,
    /// Size of the table last written or found already in the file.
    pub rows: usize,
    pub cols: usize,
    pub bytes: u64,
    /// A write is under way.
    pub writing: bool,
    /// Why the last write failed.
    pub error: Option<String>,
}

/// Write requests from outside the cook (`table_out.write`) and the state of every `out.table`,
/// by node. Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct TableOuts(Arc<Mutex<BTreeMap<NodeId, Entry>>>);

struct Entry {
    waker: Waker,
    /// Requests made and requests answered (written or failed); a request is open while
    /// `requested > answered`.
    requested: u64,
    answered: u64,
    status: Status,
    written: Option<Instant>,
}

impl TableOuts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the node to write now, whatever its mode. Returns the request's number for
    /// [`TableOuts::answered`]; `None` when the node has not cooked yet.
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

    pub fn status(&self, node: NodeId) -> Option<Status> {
        lock(&self.0).get(&node).map(Entry::status)
    }

    /// Every node that has cooked, in id order.
    pub fn all(&self) -> Vec<(NodeId, Status)> {
        lock(&self.0).iter().map(|(id, e)| (*id, e.status())).collect()
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Registers the node's waker; returns the number of the newest request.
    fn check_in(&self, node: NodeId, waker: Waker) -> u64 {
        let mut entries = lock(&self.0);
        let e = entries.entry(node).or_insert_with(|| Entry {
            waker: waker.clone(),
            requested: 0,
            answered: 0,
            status: Status::default(),
            written: None,
        });
        e.waker = waker;
        e.requested
    }

    fn report(&self, node: NodeId, status: Status, written: Option<Instant>, answered: u64) {
        if let Some(e) = lock(&self.0).get_mut(&node) {
            e.status = status;
            e.written = written;
            e.answered = e.answered.max(answered);
        }
    }
}

impl Entry {
    fn status(&self) -> Status {
        let ago = self.written.map(|at| at.elapsed().as_secs_f64());
        Status { written_seconds_ago: ago, ..self.status.clone() }
    }
}

/// Where and how the node writes.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    path: PathBuf,
    format: Format,
    /// The default folder, which the job may create.
    folder: Option<PathBuf>,
    /// Replace the file; off, write a new numbered file each time.
    overwrite: bool,
}

/// What a finished job did.
#[derive(Debug)]
struct Wrote {
    /// The target's path, which `last` remembers.
    wanted: PathBuf,
    /// The file written: the target's path, or a numbered name beside it.
    path: PathBuf,
    rows: usize,
    cols: usize,
    bytes: u64,
    hash: u64,
    /// False when the file already held the text.
    wrote: bool,
}

#[derive(Default)]
struct TableOut {
    slot: AsyncSlot<Wrote>,
    /// The request the job in flight answers, if any.
    flight: Option<u64>,
    /// The input and target last handed to a job, so the same is not written twice.
    sent: Option<(Data, Target)>,
    /// The target path and hash of the text last written or found in the file.
    last: Option<(PathBuf, u64)>,
    /// The target path and the file last written for it.
    file: Option<(PathBuf, PathBuf)>,
    /// The newest request answered.
    answered: u64,
    status: Status,
    written: Option<Instant>,
}

fn target(cx: &CookContext<'_>) -> Result<Target, CookError> {
    let file = cx.text("file")?.trim();
    let format = cx.text("format")?;
    let (path, folder) = if file.is_empty() {
        let ext = if format == "tsv" { "tsv" } else { "csv" };
        let folder = cx.resolve_path(FOLDER)?;
        (folder.join(format!("{}.{ext}", cx.def().name.as_str())), Some(folder))
    } else {
        (cx.resolve_path(file)?, None)
    };
    let format = match format {
        "csv" => Format::Csv,
        "tsv" => Format::Tsv,
        "auto" => match Format::from_extension(path.extension().and_then(|e| e.to_str()).unwrap_or("")) {
            Format::Csv => Format::Csv,
            Format::Tsv | Format::Lines => Format::Tsv,
        },
        other => return Err(CookError::failed(format!("unknown format `{other}`"))),
    };
    Ok(Target { path, format, folder, overwrite: cx.bool("overwrite")? })
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Writes `table` to `target` unless `force` is off and the text is the one last written
/// (`last`) or, with `overwrite` on, the one the file holds.
fn write(table: &Table, target: &Target, last: Option<(PathBuf, u64)>, force: bool) -> Result<Wrote, String> {
    let name = target.path.display();
    let text = table.to_text(target.format).map_err(|e| format!("{name}: {e}"))?;
    let bytes = text.as_bytes();
    let len = bytes.len() as u64;
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "{name}: the table is {len} bytes as text, a table file may have {MAX_FILE_BYTES}"
        ));
    }
    let wrote = |path: PathBuf, wrote| Wrote {
        wanted: target.path.clone(),
        path,
        rows: table.num_rows(),
        cols: table.num_cols(),
        bytes: len,
        hash: hash(bytes),
        wrote,
    };
    if !force {
        if last == Some((target.path.clone(), hash(bytes))) {
            return Ok(wrote(target.path.clone(), false));
        }
        let same_len = std::fs::metadata(&target.path).is_ok_and(|m| m.is_file() && m.len() == len);
        if target.overwrite && same_len && std::fs::read(&target.path).is_ok_and(|old| old == bytes) {
            return Ok(wrote(target.path.clone(), false));
        }
    }
    if let Some(folder) = &target.folder
        && !folder.is_dir()
    {
        std::fs::create_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    }
    if target.overwrite {
        replace(&target.path, bytes).map_err(|e| format!("{name}: {e}"))?;
        return Ok(wrote(target.path.clone(), true));
    }
    // The new name is taken first, so a write running at the same time cannot take it too.
    let (file, path) = penumbra_core::files::create_numbered(&target.path)?;
    drop(file);
    if let Err(e) = replace(&path, bytes) {
        std::fs::remove_file(&path).ok();
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(wrote(path, true))
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

impl TableOut {
    fn start(&mut self, cx: &CookContext<'_>, data: Data, target: Target, force: bool) {
        let last = self.last.clone();
        let t = target.clone();
        let d = data.clone();
        self.slot.start(cx.spawn(move |_job| {
            let table = d.get::<Table>().ok_or("the input is not a table")?;
            write(table, &t, last, force)
        }));
        self.sent = Some((data, target));
    }

    /// Takes a finished job's result.
    fn finish(&mut self) {
        let busy = self.slot.busy();
        let good = self.slot.poll();
        if !busy || self.slot.busy() {
            return;
        }
        if let Some(ticket) = self.flight.take() {
            self.answered = self.answered.max(ticket);
        }
        if good && let Some(w) = self.slot.latest() {
            self.last = Some((w.wanted.clone(), w.hash));
            if w.wrote {
                self.written = Some(Instant::now());
                self.file = Some((w.wanted.clone(), w.path.clone()));
            }
            self.status.rows = w.rows;
            self.status.cols = w.cols;
            self.status.bytes = w.bytes;
        }
    }
}

impl Operator for TableOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.finish();
        let outs = cx.resource::<TableOuts>();
        let node = cx.node();
        let requested = outs.map_or(0, |o| o.check_in(node, cx.waker()));
        let result = self.decide(cx, requested);
        if let Some(outs) = outs {
            let mut status = self.status.clone();
            status.writing = self.slot.busy();
            status.error = match &result {
                Err(CookError::Failed(e)) => Some(e.clone()),
                _ => self.slot.error().map(str::to_string),
            };
            outs.report(node, status, self.written, self.answered);
        }
        result?;
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        if cx.input(0).is_none() {
            cx.warn("nothing connected: nothing is written");
        }
        Ok(Cooked::Outputs(Vec::new()))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

impl TableOut {
    /// Starts a write when one is due.
    fn decide(&mut self, cx: &CookContext<'_>, requested: u64) -> Result<(), CookError> {
        let open = requested > self.answered;
        let target = target(cx);
        let mode = cx.text("mode")?;
        self.status.mode = mode.to_string();
        if let Ok(t) = &target {
            let file = match &self.file {
                Some((wanted, written)) if *wanted == t.path => written,
                _ => &t.path,
            };
            self.status.file = file.display().to_string();
        }
        if self.slot.busy() {
            // The job's end recooks the node; a request waits until then.
            return Ok(());
        }
        let target = match target {
            Ok(t) => t,
            Err(e) => {
                // A request cannot be served: it is answered with the error.
                self.answered = requested;
                return Err(e);
            }
        };
        let data = cx.input(0).filter(|d| d.get::<Table>().is_some()).cloned();
        if open {
            let Some(data) = data else {
                self.answered = requested;
                return Err(CookError::failed("nothing connected: nothing to write"));
            };
            self.flight = Some(requested);
            self.start(cx, data, target, true);
            return Ok(());
        }
        if mode != "changes" {
            return Ok(());
        }
        let Some(data) = data else { return Ok(()) };
        let same = self.sent.as_ref().is_some_and(|(d, t)| d.same(&data) && *t == target);
        if !same {
            self.start(cx, data, target, false);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[&[&str]]) -> Table {
        Table::new(rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()).unwrap()
    }

    #[test]
    fn writes_only_what_differs() {
        let dir = penumbra_core::scratch::ScratchDir::new("table-out").unwrap();
        let folder = dir.join("export");
        let target = Target {
            path: folder.join("t.csv"),
            format: Format::Csv,
            folder: Some(folder.clone()),
            overwrite: true,
        };
        let t = table(&[&["a", "b, c"]]);
        let w = write(&t, &target, None, false).unwrap();
        assert!(w.wrote, "the export folder is created");
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), "a,\"b, c\"\n");
        // The file holds the text already: not written again, unless forced.
        assert!(!write(&t, &target, None, false).unwrap().wrote);
        assert!(!write(&t, &target, Some((w.path.clone(), w.hash)), false).unwrap().wrote);
        assert!(write(&t, &target, None, true).unwrap().wrote);
        // No temporary file is left.
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);

        // Only the default folder is created.
        let elsewhere = Target {
            path: dir.join("missing").join("t.csv"),
            format: Format::Csv,
            folder: None,
            overwrite: true,
        };
        assert!(write(&t, &elsewhere, None, false).is_err());
        assert!(!dir.join("missing").exists());
        // A cell TSV cannot hold.
        let tsv = Target { path: dir.join("t.tsv"), format: Format::Tsv, folder: None, overwrite: true };
        assert!(write(&table(&[&["a\tb"]]), &tsv, None, false).unwrap_err().contains("write it as csv"));

        // Overwrite off: each write a new numbered file, the text written last not again.
        let numbered =
            Target { path: dir.join("n.csv"), format: Format::Csv, folder: None, overwrite: false };
        std::fs::write(&numbered.path, "a,\"b, c\"\n").unwrap();
        let w = write(&t, &numbered, None, false).unwrap();
        assert_eq!(w.path, dir.join("n_2.csv"), "the file holding the same text is no reason to skip");
        assert!(!write(&t, &numbered, Some((w.wanted.clone(), w.hash)), false).unwrap().wrote);
        assert_eq!(write(&t, &numbered, Some((w.wanted, w.hash)), true).unwrap().path, dir.join("n_3.csv"));
        assert_eq!(std::fs::read_to_string(dir.join("n_3.csv")).unwrap(), "a,\"b, c\"\n");
    }

    use std::time::Duration;

    use penumbra_core::{NodePath, ParamPath, Rate, ShowClock, Value};
    use penumbra_graph::{Cooker, JobPool, Network, Op};

    fn create(path: &str, kind: &str) -> Op {
        Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
    }

    fn set(p: &str, v: impl Into<Value>) -> Op {
        Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
    }

    /// Cooks `/w` until `done` holds.
    fn cook_until(net: &Arc<Network>, cooker: &mut Cooker, what: &str, done: impl Fn() -> bool) {
        let id = net.resolve(&"/w".parse().unwrap()).unwrap();
        let clock = ShowClock::new(Rate::FPS_60, Default::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cooker.cook(net, clock.now(), &[id], &[]);
            if done() {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {:?}", cooker.status(id));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn writes_its_input() {
        let types = Arc::new(crate::builtin_types().unwrap());
        let dir = penumbra_core::scratch::ScratchDir::new("table-out-cook").unwrap();
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        cooker.set_base_dir(Some(dir.path().to_path_buf()));
        let outs = TableOuts::new();
        cooker.provide(outs.clone());
        let mut net = Network::new()
            .apply(
                &types,
                &[
                    create("/t", "table.text"),
                    set("/t.text", "name,message\nalex,\"hi, there\""),
                    set("/t.format", "csv"),
                    create("/w", OUT_TABLE),
                    Op::Connect {
                        from: "/t".parse().unwrap(),
                        output: 0,
                        to: "/w".parse().unwrap(),
                        input: 0,
                    },
                ],
            )
            .unwrap()
            .network;
        // Empty file: export/<node name>.csv, the folder created.
        let file = dir.join("export").join("w.csv");
        let read = |f: &Path| std::fs::read_to_string(f).unwrap_or_default();
        let arc = Arc::new(net.clone());
        cook_until(&arc, &mut cooker, "the default file", || {
            read(&file) == "name,message\nalex,\"hi, there\"\n"
        });
        let back = Table::parse(&read(&file), Format::Csv).unwrap();
        assert_eq!(back.cell(1, 1), "hi, there");
        let id = arc.resolve(&"/w".parse().unwrap()).unwrap();
        cook_until(&arc, &mut cooker, "the status", || outs.status(id).is_some_and(|s| !s.writing));
        let s = outs.status(id).unwrap();
        assert_eq!((s.rows, s.cols, s.bytes, s.error.clone()), (2, 2, 30, None), "{s:?}");

        // A change is written; tsv by extension.
        net = net.apply(&types, &[set("/t.text", "a,b"), set("/w.file", "out.tsv")]).unwrap().network;
        let tsv = dir.join("out.tsv");
        let arc = Arc::new(net.clone());
        cook_until(&arc, &mut cooker, "the tsv", || read(&tsv) == "a\tb\n");

        // Manual: a change is not written, a request is.
        net = net.apply(&types, &[set("/w.mode", "manual"), set("/t.text", "c,d")]).unwrap().network;
        let arc = Arc::new(net.clone());
        for _ in 0..20 {
            cook_until(&arc, &mut cooker, "a cook", || true);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(read(&tsv), "a\tb\n");
        let ticket = outs.request(id).unwrap();
        cook_until(&arc, &mut cooker, "the request", || outs.answered(id) >= ticket);
        assert_eq!(read(&tsv), "c\td\n");
        assert!(outs.status(id).unwrap().written_seconds_ago.is_some());

        // An error is the node's error and answers the request.
        net = net.apply(&types, &[set("/w.file", "missing/x.csv")]).unwrap().network;
        let arc = Arc::new(net.clone());
        let ticket = outs.request(id).unwrap();
        cook_until(&arc, &mut cooker, "the failed request", || outs.answered(id) >= ticket);
        let s = outs.status(id).unwrap();
        assert!(s.error.as_deref().is_some_and(|e| e.contains("x.csv")), "{s:?}");
        assert!(cooker.status(id).unwrap().error.is_some());
        assert!(!dir.join("missing").exists(), "no folder but export is created");
    }
}
