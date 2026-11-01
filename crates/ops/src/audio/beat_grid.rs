//! `chan.beat_grid`: the beats and bars of the sound file an `audio.file` plays, read at its
//! playhead. The beats come from the Beat This! network ([`penumbra_audio::beats`]), found once
//! per file in the background and kept in a cache on disk.
//!
//! **Wiring:** the node's `audio` input takes an `audio.file`'s `out`, the same wire a
//! `chan.beat_analysis` takes. The node does not listen to the samples: it asks [`SoundFiles`]
//! which file the node at the other end plays and where its playhead stands.
//!
//! **Analyses** belong to [`BeatFiles`], one per sound file, shared by every node and command
//! that wants it. Each runs on a thread of its own and hands on the beats that are final so far
//! while it runs ([`beats::pick_final`]), so the grid can be read long before a two-hour mix is
//! done. An analysis stops when the last node or command that wanted it lets go, when it is
//! cancelled, and when the app closes. A finished one stays in memory for the next node that
//! wants the same file, until the file changes on disk.
//!
//! **Bars** are voted over 64 beats from the network's downbeat logits ([`beats::bars`]), so
//! they hold on DJ mixes, where the downbeats the network marks one by one do not.
//!
//! **Time:** the grid is known ahead, so `offset` moves the lookup itself: a tick comes exactly
//! `offset` early, with no prediction. A looped file's grid runs from its last beat into the
//! first beat of the next pass ([`BeatGrid::at_looped`]). Per cook the node costs one binary
//! search.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime};

use penumbra_audio::beats::{self, BeatError, BeatGrid, BeatModel, Beats, Partial};
use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, Table, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator, Waker,
};

use super::file::SoundFiles;

/// Kind of the grid node.
pub const BEAT_GRID: &str = "chan.beat_grid";
/// The index of the `grid` output.
const GRID: usize = 1;
/// A lookup that moved further than this since the last cook is a jump, not a beat passing.
const JUMP_SECONDS: f64 = 0.5;
/// Finished analyses kept for nodes to come.
const KEEP_DONE: usize = 32;
/// Progress steps that wake the nodes.
const WAKE_STEP: f64 = 0.01;
/// Cores the analysis leaves free for sound, pictures and the rest of the app.
const SPARE_CORES: usize = 4;

/// The threads every analysis runs its network on: fewer than the cores and at the lowest
/// priority, so a long analysis never starves the sound output. `None` when the pool cannot be
/// built; the analysis then runs on the calling thread alone.
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores.saturating_sub(SPARE_CORES).max(1))
            .thread_name(|i| format!("beats-{i}"))
            .start_handler(|_| {
                penumbra_system::process::lower_thread_priority();
            })
            .build()
            .map_err(|e| log::warn!("beat analysis threads: {e}"))
            .ok()
    })
    .as_ref()
}

/// The channels `chan.beat_grid` outputs, in order. The first eight mean what they mean on
/// `chan.beat_analysis`, so a binding keeps working when one node replaces the other.
pub const CHANNELS: [&str; 14] = [
    "beat",
    "tick",
    "bar",
    "bar_beat",
    "bpm",
    "beats_per_bar",
    "locked",
    "coasting",
    "downbeat",
    "beat_index",
    "bar_index",
    "ready",
    "progress",
    "analyzing",
];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            BEAT_GRID,
            "The beats and bars of the sound file an audio.file plays, found ahead of time, as numbers to animate with",
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .clamp(-5000.0, 5000.0)
                .range(-200.0, 200.0)
                .doc("Milliseconds to move beat and bar ahead by, to make up for the projector's delay. Ticks come exactly this much early. Negative values move them later"),
        )
        .input("audio", DataKind::Audio)
        .describe_input("audio", "The out of an audio.file, wired straight from it")
        .output("out", DataKind::Chan)
        .output("grid", DataKind::Table)
        .lazy_output("grid")
        .describe_output(
            "out",
            "Channels to bind: beat and bar rise from 0 to 1, tick and downbeat are 1 in the frame a beat or bar begins, bar_beat counts the beats in the bar from 0, \
             beat_index and bar_index count from the first beat and the first downbeat; also bpm, beats_per_bar, locked, coasting. \
             ready is 1 where the grid is final, progress goes from 0 to 1 while the file is analysed, analyzing is 1 meanwhile",
        )
        .describe_output(
            "grid",
            "One row per beat found so far, under a header row: time (seconds into the file), beat, bar, beat_in_bar, downbeat (1 or 0)",
        ),
        || Box::new(GridNode::default()) as Box<dyn Operator>,
    ))
}

/// The beats of a file as far as they are known.
#[derive(Debug)]
pub struct Grid {
    pub beats: Beats,
    pub grid: BeatGrid,
    /// The grid's bar starts, seconds into the file: voted from the network's downbeat
    /// logits, so they need not be `beats.downbeats`.
    pub bars: Vec<f64>,
    /// Seconds into the file before which the grid is final; infinite once the analysis ended,
    /// minus infinity before two bars are known.
    pub final_until: f64,
}

impl Grid {
    fn new(beats: Beats, final_until: f64) -> Self {
        let grid = BeatGrid::from_beats(&beats);
        let bars = grid.downbeats();
        Grid { beats, grid, bars, final_until }
    }

    /// The analysis ended and this is all of it.
    pub fn complete(&self) -> bool {
        self.final_until == f64::INFINITY
    }

    /// The grid as a table: a header row, then one row per beat.
    pub fn table(&self) -> Result<Table, Error> {
        let mut rows = Vec::with_capacity(self.beats.beats.len() + 1);
        rows.push(["time", "beat", "bar", "beat_in_bar", "downbeat"].map(str::to_string).to_vec());
        for t in &self.beats.beats {
            let Some(p) = self.grid.at(*t) else { continue };
            rows.push(vec![
                format!("{t:.3}"),
                p.beat.to_string(),
                p.bar.to_string(),
                p.beat_in_bar.to_string(),
                if p.beat_in_bar == 0 { "1" } else { "0" }.to_string(),
            ]);
        }
        Table::new(rows)
    }
}

/// How one file's analysis stands.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// The analysis is running.
    pub running: bool,
    /// 0 to 1.
    pub progress: f64,
    /// The beats known so far; complete once the analysis ended.
    pub grid: Option<Arc<Grid>>,
    pub error: Option<String>,
    pub cancelled: bool,
    /// Seconds it ran (so far).
    pub seconds: f64,
}

/// What the thread and the handles share.
struct Shared {
    state: Mutex<State>,
    wakers: Mutex<BTreeMap<NodeId, Waker>>,
    started: Instant,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = lock(&self.state);
        f(&mut s);
        s.seconds = self.started.elapsed().as_secs_f64();
    }

    fn wake(&self) {
        for w in lock(&self.wakers).values() {
            w.wake();
        }
    }
}

/// One sound file's analysis, shared by the nodes and commands that want it. Dropping the last
/// handle stops it.
pub struct Analysis {
    file: PathBuf,
    /// The file's size and modification time when the analysis began.
    stamp: Option<(u64, SystemTime)>,
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
    /// A newer analysis of the file took this one's place.
    replaced: AtomicBool,
}

impl Analysis {
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn state(&self) -> State {
        lock(&self.shared.state).clone()
    }

    /// The grid so far, the progress, whether it runs, and the error once it ended in one.
    fn grid(&self) -> (Option<Arc<Grid>>, f64, bool, Option<String>) {
        let s = lock(&self.shared.state);
        (s.grid.clone(), s.progress, s.running, s.error.clone().filter(|_| !s.running))
    }

    /// Stops it before its next chunk.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether a newer analysis of the same file took this one's place.
    pub fn replaced(&self) -> bool {
        self.replaced.load(Ordering::Relaxed)
    }

    /// Recooks `node` whenever the analysis moves on.
    fn wake_on_change(&self, node: NodeId, waker: Waker) {
        lock(&self.shared.wakers).insert(node, waker);
    }

    fn job_status(&self) -> JobStatus {
        let s = lock(&self.shared.state);
        if s.running {
            // `as`: a fraction for the progress bar.
            JobStatus::Running { progress: Some(s.progress as f32) }
        } else if let Some(e) = &s.error {
            JobStatus::Failed { error: e.clone() }
        } else if s.grid.is_some() {
            JobStatus::Done
        } else {
            JobStatus::Idle
        }
    }

    fn ended(&self) -> bool {
        !lock(&self.shared.state).running
    }
}

impl Drop for Analysis {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn stamp(file: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(file).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The beat analyses of sound files, one per file, for the `chan.beat_grid` nodes and the
/// `beats.*` commands. Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone)]
pub struct BeatFiles(Arc<Inner>);

struct Inner {
    /// Where results are kept on disk; `None` keeps none.
    cache: Option<PathBuf>,
    /// Every analysis some handle still holds.
    files: Mutex<BTreeMap<PathBuf, Weak<Analysis>>>,
    /// Analyses held without a node: by commands until they end, and the last finished ones.
    held: Mutex<Vec<Arc<Analysis>>>,
    /// The network, loaded while an analysis needs it.
    model: Mutex<Weak<BeatModel>>,
    stopped: AtomicBool,
}

impl BeatFiles {
    /// Results are kept in `cache` (a directory, made when needed).
    pub fn new(cache: Option<PathBuf>) -> Self {
        BeatFiles(Arc::new(Inner {
            cache,
            files: Mutex::new(BTreeMap::new()),
            held: Mutex::new(Vec::new()),
            model: Mutex::new(Weak::new()),
            stopped: AtomicBool::new(false),
        }))
    }

    /// The analysis of `file`: the one running or finished, else a new one. A finished or
    /// failed one is begun anew when the file changed on disk since.
    pub fn want(&self, file: &Path) -> Arc<Analysis> {
        let mut files = lock(&self.0.files);
        if let Some(a) = files.get(file).and_then(Weak::upgrade)
            && !(a.ended() && a.stamp != stamp(file))
        {
            return a;
        }
        self.begin(&mut files, file)
    }

    /// Retires the analysis of `file` (after a cancel or a failure): it stops, and its nodes
    /// and the next [`BeatFiles::want`] begin a new one.
    pub fn restart(&self, file: &Path) {
        let old = lock(&self.0.files).remove(file).and_then(|w| w.upgrade());
        if let Some(old) = old {
            old.replaced.store(true, Ordering::Relaxed);
            old.cancel();
            lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &old));
            old.shared.wake();
        }
    }

    /// [`BeatFiles::want`], and kept going without a node until it ends.
    pub fn hold(&self, file: &Path) -> Arc<Analysis> {
        let a = self.want(file);
        let mut held = lock(&self.0.held);
        if !held.iter().any(|h| Arc::ptr_eq(h, &a)) {
            held.push(a.clone());
        }
        a
    }

    /// The analysis of `file`, if some node or command holds it.
    pub fn get(&self, file: &Path) -> Option<Arc<Analysis>> {
        lock(&self.0.files).get(file).and_then(Weak::upgrade)
    }

    /// Every analysis held, by file.
    pub fn list(&self) -> Vec<(PathBuf, State)> {
        lock(&self.0.files).iter().filter_map(|(f, w)| w.upgrade().map(|a| (f.clone(), a.state()))).collect()
    }

    /// Stops `file`'s analysis. Its nodes keep the beats found so far and do not begin again
    /// until [`BeatFiles::restart`]. Whether one was running.
    pub fn cancel(&self, file: &Path) -> bool {
        let Some(a) = self.get(file) else { return false };
        let running = !a.ended();
        a.cancel();
        lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &a) || h.ended());
        running
    }

    /// Stops every analysis; none begins after this. For the app's end.
    pub fn shutdown(&self) {
        self.0.stopped.store(true, Ordering::Relaxed);
        for w in lock(&self.0.files).values() {
            if let Some(a) = w.upgrade() {
                a.cancel();
            }
        }
        lock(&self.0.held).clear();
    }

    fn begin(&self, files: &mut BTreeMap<PathBuf, Weak<Analysis>>, file: &Path) -> Arc<Analysis> {
        if let Some(old) = files.get(file).and_then(Weak::upgrade) {
            // The file changed on disk since it ended.
            old.replaced.store(true, Ordering::Relaxed);
            lock(&self.0.held).retain(|h| !Arc::ptr_eq(h, &old));
        }
        files.retain(|_, w| w.strong_count() > 0);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { running: true, ..State::default() }),
            wakers: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
        });
        let a = Arc::new(Analysis {
            file: file.to_path_buf(),
            stamp: stamp(file),
            cancel: Arc::new(AtomicBool::new(false)),
            shared: shared.clone(),
            replaced: AtomicBool::new(false),
        });
        files.insert(file.to_path_buf(), Arc::downgrade(&a));
        if self.0.stopped.load(Ordering::Relaxed) {
            shared.update(|s| {
                s.running = false;
                s.cancelled = true;
            });
            return a;
        }
        let (inner, path, cancel) = (self.0.clone(), file.to_path_buf(), a.cancel.clone());
        let spawned = std::thread::Builder::new().name("beats".into()).spawn(move || {
            penumbra_system::process::lower_thread_priority();
            let work = || run(&inner, &path, &cancel, &shared);
            let result = catch_unwind(AssertUnwindSafe(|| match pool() {
                Some(p) => p.install(work),
                None => work(),
            }))
            .unwrap_or_else(|_| Err(BeatError::Network("the beat analysis failed".into())));
            shared.update(|s| {
                s.running = false;
                match result {
                    Ok(b) => {
                        s.grid = Some(Arc::new(Grid::new(b, f64::INFINITY)));
                        s.progress = 1.0;
                    }
                    Err(BeatError::Cancelled) => s.cancelled = true,
                    Err(e) => s.error = Some(e.to_string()),
                }
            });
            shared.wake();
            inner.keep(&path, &shared);
        });
        if let Err(e) = spawned {
            a.shared.update(|s| {
                s.running = false;
                s.error = Some(format!("cannot start the beat analysis: {e}"));
            });
        }
        a
    }
}

impl Inner {
    /// The network, loaded once for the analyses running at the same time.
    fn model(&self) -> Result<Arc<BeatModel>, BeatError> {
        let mut slot = lock(&self.model);
        if let Some(m) = slot.upgrade() {
            return Ok(m);
        }
        let m = Arc::new(BeatModel::shipped()?);
        *slot = Arc::downgrade(&m);
        Ok(m)
    }

    /// A finished analysis stays for the next node; a command's hold ends with it.
    fn keep(&self, file: &Path, shared: &Arc<Shared>) {
        let Some(a) = lock(&self.files).get(file).and_then(Weak::upgrade) else { return };
        if !Arc::ptr_eq(&a.shared, shared) {
            return;
        }
        let mut held = lock(&self.held);
        held.retain(|h| !Arc::ptr_eq(h, &a));
        if lock(&shared.state).grid.as_ref().is_some_and(|g| g.complete()) {
            held.push(a);
            let done = held.iter().filter(|h| h.ended()).count();
            if done > KEEP_DONE
                && let Some(i) = held.iter().position(|h| h.ended())
            {
                held.remove(i);
            }
        }
    }
}

/// The analysis itself, on its thread.
fn run(inner: &Inner, file: &Path, cancel: &AtomicBool, shared: &Shared) -> Result<Beats, BeatError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(BeatError::Cancelled);
    }
    let model = inner.model()?;
    let mut woke = 0.0;
    let mut progress = |p: &beats::Progress| {
        let f = p.fraction();
        shared.update(|s| s.progress = f);
        if f - woke >= WAKE_STEP {
            woke = f;
            shared.wake();
        }
    };
    let mut partial = |p: &Partial| {
        let grid = Arc::new(Grid::new(p.beats.clone(), p.final_until.unwrap_or(f64::NEG_INFINITY)));
        shared.update(|s| s.grid = Some(grid));
        shared.wake();
    };
    beats::analyze_file_with(file, &model, inner.cache.as_deref(), Some(cancel), &mut progress, &mut partial)
}

#[derive(Default)]
struct GridNode {
    analysis: Option<Arc<Analysis>>,
    watch: Option<FileWatch>,
    /// The pass through a looped file, beat and bar at the last lookup and its time, for the
    /// ticks.
    last: Option<(i64, i64, i64, f64)>,
    names: Vec<Name>,
    /// The `grid` output and the grid it was made from.
    table: Option<(Arc<Grid>, Data)>,
}

impl GridNode {
    /// The analysis of `file`, anew when the file is another, changed on disk or was analysed
    /// again.
    fn follow(&mut self, cx: &CookContext<'_>, file: &Path) -> Result<Arc<Analysis>, CookError> {
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if let Some(a) = &self.analysis
            && a.file() == file
            && !changed
            && !a.replaced()
        {
            return Ok(a.clone());
        }
        let files = cx
            .resource::<BeatFiles>()
            .ok_or_else(|| CookError::failed("beat analysis is not available in this process"))?;
        if self.watch.as_ref().is_none_or(|w| w.path() != file) {
            self.watch = Some(cx.watch_file(file));
        }
        let a = files.want(file);
        a.wake_on_change(cx.node(), cx.waker());
        self.analysis = Some(a.clone());
        self.last = None;
        self.table = None;
        Ok(a)
    }

    fn table(&mut self, cx: &CookContext<'_>, grid: Option<&Arc<Grid>>) -> Data {
        if !cx.output_wanted(GRID) {
            self.table = None;
            return Data::skipped(DataKind::Table);
        }
        let Some(grid) = grid else {
            return Data::new(DataKind::Table, Table::default());
        };
        if let Some((g, d)) = &self.table
            && Arc::ptr_eq(g, grid)
        {
            return d.clone();
        }
        let data = match grid.table() {
            Ok(t) => Data::new(DataKind::Table, t),
            Err(e) => {
                cx.warn(format!("the grid output is empty: {e}"));
                Data::new(DataKind::Table, Table::default())
            }
        };
        self.table = Some((grid.clone(), data.clone()));
        data
    }
}

impl Operator for GridNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(port) = cx.def().inputs.first().copied().flatten() else {
            self.analysis = None;
            return Err(CookError::failed("wire the out of an audio.file into audio"));
        };
        let sounds = cx
            .resource::<SoundFiles>()
            .ok_or_else(|| CookError::failed("sound files are not available here"))?;
        let Some(status) = sounds.get(port.node) else {
            return Err(if cx.input(0).is_none() {
                CookError::Pending
            } else {
                CookError::failed(
                    "audio must come straight from an audio.file: its file and playhead give the grid",
                )
            });
        };
        if status.file.is_empty() {
            self.analysis = None;
            return Err(CookError::failed("the audio.file has no file"));
        }
        let analysis = self.follow(cx, Path::new(&status.file))?;
        let (grid, progress, running, error) = analysis.grid();
        if let Some(e) = error {
            return Err(CookError::failed(format!("no beats: {e}")));
        }
        if !running && grid.as_ref().is_none_or(|g| !g.complete()) {
            cx.warn("the beat analysis was cancelled: analyze starts it again");
        }

        let offset = cx.float("offset")?;
        let offset = if offset.is_finite() { offset.clamp(-5000.0, 5000.0) / 1000.0 } else { 0.0 };
        // A looped file's grid runs on from its last beat into the first beat of the next pass,
        // once all of it is known; `t` counts from the start of the first pass then.
        let looped = status.looping && status.duration > 0.0 && grid.as_ref().is_some_and(|g| g.complete());
        let (t, lookup) = if looped {
            let t = status.position + offset;
            (t, grid.as_ref().and_then(|g| g.grid.at_looped(t, status.duration)))
        } else {
            let t = status.in_file + offset;
            (t, grid.as_ref().and_then(|g| g.grid.at(t)).map(|p| (p, 0)))
        };
        let pos = lookup.map(|(p, _)| p);
        let ready = pos.is_some() && grid.as_ref().is_some_and(|g| t < g.final_until);
        let (mut tick, mut downbeat) = (false, false);
        if let (Some((p, pass)), Some((last_pass, beat, bar, before))) = (lookup, self.last) {
            let forward = t > before && t - before <= JUMP_SECONDS;
            tick = forward && (pass, p.beat) > (last_pass, beat);
            downbeat = forward && (pass, p.bar) > (last_pass, bar);
        }
        self.last = lookup.map(|(p, pass)| (pass, p.beat, p.bar, t));

        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let values = match pos {
            Some(p) => [
                p.beat_phase,
                flag(tick),
                p.bar_phase,
                p.beat_in_bar as f64,
                p.bpm,
                p.beats_in_bar as f64,
                flag(ready && p.inside),
                flag(!(ready && p.inside)),
                flag(downbeat),
                p.beat as f64,
                p.bar as f64,
                flag(ready),
                progress,
                flag(running),
            ],
            None => {
                let mut v = [0.0; CHANNELS.len()];
                if let Some(x) = v.get_mut(12) {
                    *x = progress;
                }
                if let Some(x) = v.get_mut(13) {
                    *x = flag(running);
                }
                v
            }
        };
        if self.names.len() != CHANNELS.len() {
            self.names = CHANNELS.iter().filter_map(|n| Name::new(n).ok()).collect();
        }
        let channels = self
            .names
            .iter()
            .zip(values)
            .map(|(name, v)| Channel { name: name.clone(), samples: vec![v] })
            .collect();
        let out = Data::new(DataKind::Chan, Chans::new(cx.rate().as_f64(), channels)?);
        let table = self.table(cx, grid.as_ref());
        Ok(Cooked::Outputs(vec![out, table]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        self.analysis.as_ref().map(|a| a.job_status())
    }
}
