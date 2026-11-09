//! The cook scheduler.
//!
//! Pull based: once per show frame the caller names the nodes that are wanted (projector
//! outputs and nodes watched over the control channel at full priority, nodes visible in the
//! editor as thumbnails at low priority), and the cooker evaluates them and everything they
//! depend on. A node recooks only when it is dirty:
//!
//! - it is new, or its kind changed (fresh operator);
//! - its parameters changed;
//! - an input now points elsewhere or its source produced new outputs since this node last
//!   cooked (every successful cook stamps the outputs with a version unique in this cooker);
//! - it is time-dependent and the show time moved;
//! - its asynchronous job finished;
//! - one of its lazy outputs ([`NodeDesc::lazy_output`]) came to be read or stopped being read.
//!
//! A lazy output counts as read in a pass when it is wired into a node that the pass may cook
//! (anything upstream of the wanted nodes and, with thumbnails cooking, of the thumbnails;
//! input choosers are not consulted, so a branch a switch skips still counts), when its node is
//! named by a node reference param of such a node, or when the caller shows it
//! ([`Cooker::cook_showing`]).
//!
//! Failing nodes keep their last good outputs, so downstream (and the projectors) keep showing
//! the last working frame while the error is shown in the editor. A panicking operator is caught,
//! reported, and replaced by a fresh one the next time it cooks.
//!
//! Thumbnails: a thumbnail of a node that cooked for an output this frame costs nothing extra.
//! Otherwise it is cooked only when dirty, at most once every [`ThumbnailPolicy::min_interval`]
//! cook passes per node, and only while the frame has time left: until the pass has used
//! [`THUMB_FRAME_SHARE`] of the frame period, and at least [`ThumbnailPolicy::budget`] for
//! thumbnails however long the outputs took (round robin, so every thumbnail gets its turn). Pausing a
//! node's thumbnail means the caller stops requesting it; the global switch is
//! [`ThumbnailPolicy::mode`]; which pictures are still up to date without cooking is
//! [`Cooker::current`]. Not to be confused with locking a node
//! ([`crate::NodeDef::locked`]), which holds its outputs, not just its picture.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::bind as bindv;
use penumbra_core::param::MAX_TEXT;
use penumbra_core::{
    BindTarget, Chans, FrameTime, Name, NodeId, ParamKind, ParamPart, ParamSpec, Rate, Table, Tick, Value,
};
use serde::{Deserialize, Serialize};

use crate::bound::{self, BindRead, Resolved, short};
use crate::job::{Job, JobContext, JobPool, JobStatus, panic_message};
use crate::network::{Bind, Network, NodeDef, Port};
use crate::node::{
    CookError, Cooked, Data, DataKind, Density, InputChooser, NodeDesc, NodeType, NodeTypes, Operator,
    StateItem,
};
use crate::watch::{FileWatch, Watcher};

/// Passes without a cook after which a node's operator is trimmed ([`Operator::trim`]): two
/// seconds at 60 fps, long enough that a node cooking now and then keeps its spares.
pub const TRIM_AFTER_PASSES: u64 = 120;
/// How often the cooker looks for nodes to trim, in passes.
const TRIM_CHECK_EVERY: u64 = 30;

/// A node whose operator panicked is tried again after this long, even when nothing about it
/// changed (its params, its wiring, a finished job). Until then the error stays and the last
/// result is kept.
const PANIC_RETRY: Duration = Duration::from_secs(2);

/// What an operator sees while cooking.
pub struct CookContext<'a> {
    node: NodeId,
    def: &'a NodeDef,
    desc: &'a NodeDesc,
    time: FrameTime,
    time_read: Cell<bool>,
    inputs: &'a [Option<Data>],
    /// The wired inputs that deliver nothing until something changes ([`CookContext::input_stuck`]).
    stuck: &'a [usize],
    /// Outputs of the nodes named by node reference params, by param name.
    refs: &'a BTreeMap<String, Result<Option<Vec<Data>>, String>>,
    bound: &'a BTreeMap<Name, Value>,
    first_cook: bool,
    params_changed: bool,
    inputs_changed: bool,
    /// Per output: whether it is to be made ([`CookContext::output_wanted`]).
    wanted: &'a [bool],
    wanted_changed: bool,
    since_last_use: Option<f64>,
    exact: bool,
    spawner: &'a Spawner,
    services: &'a Services,
    warning: RefCell<Option<String>>,
}

impl<'a> CookContext<'a> {
    pub fn node(&self) -> NodeId {
        self.node
    }

    pub fn def(&self) -> &NodeDef {
        self.def
    }

    /// The show time. Reading it from a node not declared time-dependent earns the node a
    /// warning, because such a node would not recook when time moves.
    pub fn time(&self) -> FrameTime {
        self.time_read.set(true);
        self.time
    }

    /// The show's frame rate. Not the time, so reading it does not make a node time-dependent
    /// (a constant channel uses it as its sample rate).
    pub fn rate(&self) -> Rate {
        self.time.rate
    }

    /// Output `output` of whatever is connected to input `index`; `None` when nothing is
    /// connected or the source has no result yet.
    pub fn input(&self, index: usize) -> Option<&Data> {
        self.inputs.get(index).and_then(Option::as_ref)
    }

    pub fn input_as<T: 'static>(&self, index: usize) -> Option<&T> {
        self.input(index).and_then(Data::get)
    }

    /// Whether input `index` has no result and none is on its way: its source failed, something
    /// the source waits for failed, or the wire leads nowhere. `false` while the source only
    /// waits (a job running, a stream without a frame yet). A change of this answer recooks the
    /// node, so a node may wait on an input as long as this is `false`.
    pub fn input_stuck(&self, index: usize) -> bool {
        self.stuck.contains(&index)
    }

    /// The outputs of the node a node reference param names ([`ParamSpec::node`]): `Ok(None)`
    /// when the param is empty or the node has no result yet, an error when nothing has that
    /// path.
    pub fn referenced(&self, param: &str) -> Result<Option<&[Data]>, CookError> {
        match self.refs.get(param) {
            Some(Ok(outs)) => Ok(outs.as_deref()),
            Some(Err(e)) => Err(CookError::failed(e.clone())),
            None => Ok(None),
        }
    }

    /// A parameter value: from its channel binding, else explicit, else the declared default.
    pub fn param(&self, name: &str) -> Option<&Value> {
        self.bound.get(name).or_else(|| self.def.param(self.desc, name))
    }

    pub fn float(&self, name: &str) -> Result<f64, CookError> {
        self.param(name).and_then(Value::as_f64).ok_or_else(|| self.no_param(name, "float"))
    }

    pub fn int(&self, name: &str) -> Result<i64, CookError> {
        self.param(name).and_then(Value::as_i64).ok_or_else(|| self.no_param(name, "int"))
    }

    pub fn bool(&self, name: &str) -> Result<bool, CookError> {
        self.param(name).and_then(Value::as_bool).ok_or_else(|| self.no_param(name, "bool"))
    }

    pub fn text(&self, name: &str) -> Result<&str, CookError> {
        self.param(name).and_then(Value::as_str).ok_or_else(|| self.no_param(name, "text"))
    }

    fn no_param(&self, name: &str, ty: &str) -> CookError {
        CookError::Failed(format!("`{}` has no {ty} param `{name}`", self.desc.kind))
    }

    /// First cook of this operator (new node, changed kind, or after a panic).
    pub fn first_cook(&self) -> bool {
        self.first_cook
    }

    pub fn params_changed(&self) -> bool {
        self.params_changed
    }

    pub fn inputs_changed(&self) -> bool {
        self.inputs_changed
    }

    /// Show seconds since the node was last in use before this pass, when this pass uses it:
    /// one frame while it stays in use, more when it comes back (a switch or sequence that skipped
    /// it picks it again). In use means an output, a viewer or a snapshot pulls the node; a
    /// thumbnail alone does not count. `None` in a cook only for a thumbnail, and in the first
    /// pass that uses the node.
    pub fn since_last_use(&self) -> Option<f64> {
        self.since_last_use
    }

    /// Whether output `index` is to be made in this cook: always for an ordinary output; for a
    /// lazy one ([`NodeDesc::lazy_output`]) only while something reads it. An output not
    /// wanted is best given as [`Data::skipped`].
    pub fn output_wanted(&self, index: usize) -> bool {
        self.wanted.get(index).copied().unwrap_or(false)
    }

    /// Whether [`CookContext::output_wanted`] changed for some output since the last cook (the
    /// reason for this cook, when nothing else changed).
    pub fn outputs_wanted_changed(&self) -> bool {
        self.wanted_changed
    }

    /// Whether the show clock waits for this frame however long it takes (stepping, offline
    /// recording; [`Cooker::set_exact_frames`]). An operator that would hand on an older result
    /// when a fresh one is slow (a readback, a job) waits for the fresh one then, so the same
    /// frame gives the same picture on every run and every machine.
    pub fn exact_frames(&self) -> bool {
        self.exact
    }

    /// A shared service the host provided with [`Cooker::provide`], e.g. the GPU.
    pub fn resource<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.services.resources.get(&TypeId::of::<T>()).and_then(|r| r.downcast_ref())
    }

    /// A file parameter as a path: relative paths are relative to the project directory
    /// ([`Cooker::set_base_dir`]).
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf, CookError> {
        if path.trim().is_empty() {
            return Err(CookError::failed("no file set"));
        }
        let p = Path::new(path);
        Ok(match &self.services.base_dir {
            Some(base) if p.is_relative() => base.join(p),
            _ => p.to_path_buf(),
        })
    }

    /// Recooks this node whenever `path` changes on disk, as long as the handle lives (keep it
    /// in the operator). Hot reload for nodes that read files.
    pub fn watch_file(&self, path: &Path) -> FileWatch {
        self.services.watcher.watch(path, self.node)
    }

    /// A warning shown with the node until its next cook (an unknown parameter, a clamped
    /// value). The cook itself still counts as successful.
    pub fn warn(&self, message: impl Into<String>) {
        let mut w = self.warning.borrow_mut();
        match w.as_mut() {
            Some(w) => {
                w.push_str("; ");
                w.push_str(&message.into());
            }
            None => *w = Some(message.into()),
        }
    }

    /// A handle that marks this node dirty from another thread ([`Waker`]).
    pub fn waker(&self) -> Waker {
        Waker { done: self.spawner.done.clone(), node: self.node }
    }

    /// Starts `work` off the cook thread. When it finishes, this node is marked dirty and
    /// recooks at the next frame boundary, where it can take the result.
    pub fn spawn<T, F>(&self, work: F) -> Job<T>
    where
        T: Send + 'static,
        F: FnOnce(&JobContext) -> Result<T, String> + Send + 'static,
    {
        let tx = self.spawner.done.clone();
        let node = self.node;
        // A closed channel only means the cooker is gone; nothing to tell then.
        let notify = Box::new(move || {
            tx.send(node).ok();
        });
        self.spawner.pool.spawn(work, Some(notify))
    }
}

/// What an [`InputChooser`] sees: the node's parameters, bound ones evaluated, and which of
/// its inputs are wired. Nothing upstream of the inputs has cooked yet this frame.
pub struct InputChoice<'a> {
    def: &'a NodeDef,
    desc: &'a NodeDesc,
    bound: &'a BTreeMap<Name, Value>,
    /// Per input, the channels on it when it is a steering input with something on it.
    chans: Vec<Option<&'a Chans>>,
}

impl InputChoice<'_> {
    pub fn def(&self) -> &NodeDef {
        self.def
    }

    /// A parameter value, as [`CookContext::param`] will see it.
    pub fn param(&self, name: &str) -> Option<&Value> {
        self.bound.get(name).or_else(|| self.def.param(self.desc, name))
    }

    pub fn float(&self, name: &str) -> Option<f64> {
        self.param(name).and_then(Value::as_f64)
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        self.param(name).and_then(Value::as_bool)
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        self.param(name).and_then(Value::as_str)
    }

    /// Whether a wire leads into input `index`.
    pub fn connected(&self, index: usize) -> bool {
        self.def.inputs.get(index).is_some_and(Option::is_some)
    }

    /// The channels on input `index`, if it is a steering input ([`InputSpec::steers`](crate::node::InputSpec::steers)) and its
    /// source has made some (steering inputs cook before the choice).
    pub fn chans(&self, index: usize) -> Option<&Chans> {
        self.chans.get(index).copied().flatten()
    }
}

/// Marks one node dirty from any thread, as a finished job does: it recooks at the next frame
/// boundary. For operators with threads of their own (video decoding), so a frame that arrives
/// while the show time stands still is still picked up.
#[derive(Clone)]
pub struct Waker {
    done: Sender<NodeId>,
    node: NodeId,
}

impl Waker {
    pub fn wake(&self) {
        // A closed channel only means the cooker is gone.
        self.done.send(self.node).ok();
    }
}

struct Spawner {
    pool: Arc<JobPool>,
    done: Sender<NodeId>,
}

/// What the host provides to every operator.
struct Services {
    resources: BTreeMap<TypeId, Box<dyn Any + Send + Sync>>,
    base_dir: Option<PathBuf>,
    watcher: Watcher,
}

/// Which nodes show a live picture in the editor: the project setting
/// [`crate::network::THUMBNAILS`] (`cook.thumbnails`, the editor's options).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThumbnailMode {
    /// Every visible node shows a live picture (rate limited, see [`ThumbnailPolicy`]).
    #[default]
    Always,
    /// Nothing extra cooks; a node shows its picture only while it is up to date
    /// ([`Cooker::current`]): it cooked for an output or the viewer, or nothing changed.
    WhenFree,
    /// No thumbnails, nothing extra cooks.
    Never,
}

impl ThumbnailMode {
    pub const ALL: [ThumbnailMode; 3] =
        [ThumbnailMode::Always, ThumbnailMode::WhenFree, ThumbnailMode::Never];

    /// The name shown in the UI.
    pub fn name(self) -> &'static str {
        match self {
            ThumbnailMode::Always => "always",
            ThumbnailMode::WhenFree => "when free",
            ThumbnailMode::Never => "never",
        }
    }

    /// The name in the project setting and in `cook.thumbnails`.
    pub fn key(self) -> &'static str {
        match self {
            ThumbnailMode::Always => "always",
            ThumbnailMode::WhenFree => "when_free",
            ThumbnailMode::Never => "never",
        }
    }

    pub fn from_key(key: &str) -> Option<ThumbnailMode> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }
}

/// Rules for low-priority thumbnail cooks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ThumbnailPolicy {
    /// The global switch. Only [`ThumbnailMode::Always`] cooks for thumbnails; in the other
    /// modes thumbnails of nodes that cook for an output anyway are still reported.
    pub mode: ThumbnailMode,
    /// Cook passes between two thumbnail cooks of the same node (1 is every pass, 6 is 10 Hz at 60 fps).
    pub min_interval: u32,
    /// Time per pass thumbnail cooks always get after the outputs, even when the outputs used
    /// up the frame. With time left in the frame they get more ([`THUMB_FRAME_SHARE`]).
    pub budget: Duration,
}

impl Default for ThumbnailPolicy {
    fn default() -> Self {
        Self { mode: ThumbnailMode::Always, min_interval: 1, budget: Duration::from_millis(2) }
    }
}

/// Share of the frame period a pass may fill with thumbnail cooks: thumbnails stay smooth
/// while the whole pass fits in the frame, and the rest of the period is margin for a slow cook.
pub const THUMB_FRAME_SHARE: f64 = 0.75;

/// Cook-time statistics of one node (shown as a tint in the editor, reported by `node.info`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct CookStats {
    pub cooks: u64,
    pub last: Duration,
    /// Exponential moving average over roughly the last 10 cooks.
    pub average: Duration,
    pub max: Duration,
    /// Cook seconds, each weighted by how long ago it happened (`exp(-age / LOAD_TAU)`), as of
    /// `at`.
    #[serde(skip)]
    recent: f64,
    /// When the last cook ended.
    #[serde(skip)]
    at: Option<Instant>,
}

/// Time constant of [`CookStats::load`]: a node that stops cooking fades to no load in a few
/// of these.
const LOAD_TAU: f64 = 1.0;

impl CookStats {
    fn add(&mut self, d: Duration) {
        self.add_at(d, Instant::now());
    }

    fn add_at(&mut self, d: Duration, now: Instant) {
        self.cooks += 1;
        self.last = d;
        self.max = self.max.max(d);
        self.average = if self.cooks == 1 {
            d
        } else {
            Duration::from_secs_f64(self.average.as_secs_f64() * 0.9 + d.as_secs_f64() * 0.1)
        };
        self.recent = self.recent_at(now) + d.as_secs_f64();
        self.at = Some(now);
    }

    fn recent_at(&self, now: Instant) -> f64 {
        let Some(at) = self.at else { return 0.0 };
        self.recent * (-now.saturating_duration_since(at).as_secs_f64() / LOAD_TAU).exp()
    }

    /// Share of the recent time the node spent cooking: the number behind the editor tint.
    /// A node cooking every frame scores its cook time over the frame period. A node that
    /// cooked once and then holds its result fades to 0 within a few seconds, however long
    /// that one cook took.
    pub fn load(&self) -> f64 {
        self.load_at(Instant::now())
    }

    fn load_at(&self, now: Instant) -> f64 {
        self.recent_at(now) / LOAD_TAU
    }
}

/// Where one node's time went since the profile was last taken ([`Cooker::take_profile`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NodeSpent {
    /// Calls to the operator's cook.
    pub cooks: u64,
    /// Time in the operator's cook: its own work, GPU commands recorded and submitted.
    pub cook: Duration,
    pub cook_max: Duration,
    /// Times a pass looked at the node, whether it then cooked or not.
    pub visits: u64,
    /// Time those looks took, the operator's cook included: bound values read, inputs
    /// gathered, deciding whether to cook. Minus [`NodeSpent::cook`], what the node costs the
    /// scheduler.
    pub visit: Duration,
}

/// Where the cook passes' time went since the profile was last taken, by step of the pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PassSpent {
    pub passes: u64,
    /// Bringing the runtimes in line with a changed network.
    pub sync: Duration,
    /// Working out which lazy outputs are read.
    pub reads: Duration,
    /// Evaluating what the outputs, viewers and eager nodes need.
    pub outputs: Duration,
    /// Evaluating thumbnails.
    pub thumbnails: Duration,
    /// Trimming idle operators.
    pub trim: Duration,
}

/// What [`Cooker::take_profile`] hands back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub passes: PassSpent,
    /// Every node a pass looked at, in id order.
    pub nodes: Vec<(NodeId, NodeSpent)>,
}

/// Everything known about one node at runtime.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeStatus {
    /// Changes whenever the outputs change; 0 before the first result.
    pub version: u64,
    pub has_output: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub job: Option<JobStatus>,
    pub stats: CookStats,
    /// What the operator holds on the GPU ([`Operator::gpu_bytes`]) as of its last cook.
    pub gpu_bytes: u64,
    /// Parameters the instance declares beyond its type's ([`Operator::params`]).
    pub params: Option<Vec<ParamSpec>>,
    /// Values of bound parameters as of the last cook (whole values, also for a vector bound
    /// per component).
    pub bound: BTreeMap<Name, Value>,
    /// Whether the node recooks when time moves (the type's declaration or the instance's).
    pub time_dependent: bool,
    /// Inputs the instance reads whether wired or not, as known after its last cook
    /// ([`Operator::reads_inputs`]): bit `i` is input `i`.
    #[serde(skip_serializing_if = "is_zero")]
    pub reads: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// What one cook pass did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CookReport {
    pub pass: u64,
    /// Nodes whose operator ran, in cook order.
    pub cooked: Vec<NodeId>,
    /// Requested thumbnails that are up to date for this pass.
    pub thumbnails: Vec<NodeId>,
    /// Requested thumbnails left for a later pass (rate limit or budget).
    pub thumbnails_deferred: usize,
    pub elapsed: Duration,
}

/// What a state snapshot keeps of one node ([`Cooker::kept`], [`Cooker::restore`]).
#[derive(Clone, Debug)]
pub enum Kept {
    /// A locked node's outputs, which it holds on purpose and cannot make again.
    Outputs(Vec<Data>),
    /// What the operator hands out ([`Operator::save_state`]).
    State(Vec<StateItem>),
}

/// What [`Cooker::reset`] did with a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetOutcome {
    /// It starts over at its next cook.
    Reset,
    /// Locked: it keeps the outputs it holds.
    Locked,
    /// Its type keeps no state and its operator holds none: nothing to reset.
    Stateless,
    /// It has not cooked yet, so it holds nothing.
    NotCooked,
}

struct Runtime {
    def: Arc<NodeDef>,
    ty: Option<Arc<dyn NodeType>>,
    op: Option<Box<dyn Operator>>,
    outputs: Option<Vec<Data>>,
    version: u64,
    /// Per output, the version it last changed at: an output a cook hands on as it was (the
    /// same data, not merely equal) keeps its version, so its readers do not recook.
    output_versions: Vec<u64>,
    seen_inputs: Vec<Option<(Port, u64)>>,
    /// Values of bound parameters at the last cook; a change counts as a parameter change.
    seen_bound: BTreeMap<Name, Value>,
    /// Why bindings could not be applied at the last cook, apart from the operator's warning.
    bind_warning: Option<String>,
    seen_tick: Option<Tick>,
    fresh: bool,
    params_changed: bool,
    notified: bool,
    evaluated_pass: u64,
    /// The pass of the last call to the operator's cook, and whether the operator was trimmed
    /// since ([`Operator::trim`]).
    cooked_pass: u64,
    trimmed: bool,
    last_thumb_pass: Option<u64>,
    /// Show time of the last pass that used the node for more than a thumbnail
    /// ([`CookContext::since_last_use`]).
    used: Option<FrameTime>,
    error: Option<String>,
    warning: Option<String>,
    stats: CookStats,
    /// Time spent since the profile was last taken.
    spent: NodeSpent,
    /// [`Operator::gpu_bytes`] after the last cook or trim.
    gpu_bytes: u64,
    /// State restored before the operator existed, handed to it before its first cook.
    restored: Option<Vec<StateItem>>,
    /// A show place restored before the operator existed ([`Cooker::restore_show_place`]).
    restored_place: Option<String>,
    /// The next cook sees nothing on inputs that read a previous result, so a loop whose
    /// state was forgotten starts from its initial input ([`Cooker::forget_state`]).
    blank_previous: bool,
    /// Which outputs were wanted at the last cook ([`CookContext::output_wanted`]).
    seen_wanted: Vec<bool>,
    /// When the operator last panicked; it is made anew only when something about the node
    /// changed or after [`PANIC_RETRY`].
    panicked: Option<Instant>,
}

impl Runtime {
    /// The operator panicked in `what`: it is dropped, and the node shows the error.
    fn panicked_in(&mut self, what: &str, message: &str) {
        self.error = Some(format!("node panicked in {what}: {message}"));
        self.op = None;
        self.gpu_bytes = 0;
        self.panicked = Some(Instant::now());
    }

    /// Whether the operator holds state now (or state restored for it waits).
    fn holds_state(&self, id: NodeId) -> bool {
        self.restored.is_some()
            || self.op.as_ref().is_some_and(|op| match catch_unwind(AssertUnwindSafe(|| op.save_state())) {
                Ok(items) => items.is_some_and(|i| !i.is_empty()),
                Err(p) => {
                    log::error!("node {id}: save_state panicked: {}", panic_message(p.as_ref()));
                    true
                }
            })
    }

    /// Drops the operator and what was restored for it, so the next cook makes it anew, with
    /// nothing on its inputs that read a previous result.
    fn start_over(&mut self) {
        self.restored = None;
        self.restored_place = None;
        self.op = None;
        self.notified = true;
        self.blank_previous = true;
        self.gpu_bytes = 0;
    }

    /// Asks the operator what it holds on the GPU now ([`Operator::gpu_bytes`]); 0 without one.
    /// The operators add up a few sizes, so this runs after every cook.
    fn measure(&mut self, id: NodeId) {
        self.gpu_bytes = match self.op.as_ref() {
            None => 0,
            Some(op) => catch_unwind(AssertUnwindSafe(|| op.gpu_bytes())).unwrap_or_else(|p| {
                log::error!("node {id}: gpu_bytes panicked: {}", panic_message(p.as_ref()));
                0
            }),
        };
    }

    /// The version output `output` last changed at.
    fn output_version(&self, output: u32) -> u64 {
        self.output_versions.get(output as usize).copied().unwrap_or(self.version)
    }

    fn new(def: Arc<NodeDef>, types: &NodeTypes) -> Self {
        let ty = types.get(&def.kind).cloned();
        Self {
            def,
            ty,
            op: None,
            outputs: None,
            version: 0,
            output_versions: Vec::new(),
            seen_inputs: Vec::new(),
            seen_bound: BTreeMap::new(),
            bind_warning: None,
            seen_tick: None,
            fresh: true,
            params_changed: false,
            notified: false,
            evaluated_pass: 0,
            cooked_pass: 0,
            trimmed: false,
            last_thumb_pass: None,
            used: None,
            error: None,
            warning: None,
            stats: CookStats::default(),
            spent: NodeSpent::default(),
            gpu_bytes: 0,
            blank_previous: false,
            restored: None,
            restored_place: None,
            seen_wanted: Vec::new(),
            panicked: None,
        }
    }
}

/// Show places waiting for nodes the cooker has not seen yet, at most.
const MAX_PENDING_PLACES: usize = 10_000;

/// Owns the runtime state of every node and cooks on request. Lives on the cook thread.
pub struct Cooker {
    types: Arc<NodeTypes>,
    spawner: Spawner,
    services: Services,
    done: Receiver<NodeId>,
    runtimes: BTreeMap<NodeId, Runtime>,
    /// Show places restored for nodes the cooker has not seen yet ([`Cooker::restore_show_place`]).
    pending_places: BTreeMap<NodeId, String>,
    last_net: Option<Arc<Network>>,
    pass: u64,
    next_version: u64,
    thumbs: ThumbnailPolicy,
    thumb_cursor: usize,
    /// Whether the last synced network has a node with lazy outputs; while it has none,
    /// nothing is worked out about which outputs are read.
    lazy: bool,
    /// The ports read in this pass, and the nodes whose every output is (named by a node
    /// reference). Only kept while [`Cooker::lazy`] holds.
    read: BTreeSet<Port>,
    read_all: BTreeSet<NodeId>,
    /// The nodes cooked in every pass ([`NodeDesc::eager`]), as of the last sync.
    eager: Vec<NodeId>,
    /// Nodes are evaluated for thumbnails now, which does not count as use.
    for_thumbnails: bool,
    /// Time of the passes since the profile was last taken.
    spent: PassSpent,
    /// The clock waits for every frame ([`CookContext::exact_frames`]).
    exact: bool,
    /// Operators that panicked when asked something where they could not be dropped at once
    /// (a status, a density): node, what was asked, the message. Dropped at the next pass.
    asked_panics: Mutex<Vec<(NodeId, &'static str, String)>>,
}

impl Cooker {
    pub fn new(types: Arc<NodeTypes>, pool: Arc<JobPool>) -> Self {
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::new(tx.clone());
        Self {
            types,
            spawner: Spawner { pool, done: tx },
            services: Services { resources: BTreeMap::new(), base_dir: None, watcher },
            done: rx,
            runtimes: BTreeMap::new(),
            pending_places: BTreeMap::new(),
            last_net: None,
            pass: 0,
            next_version: 0,
            thumbs: ThumbnailPolicy::default(),
            thumb_cursor: 0,
            lazy: false,
            read: BTreeSet::new(),
            read_all: BTreeSet::new(),
            eager: Vec::new(),
            for_thumbnails: false,
            spent: PassSpent::default(),
            exact: false,
            asked_panics: Mutex::new(Vec::new()),
        }
    }

    /// Asks the operator of node `id` something outside a cook. A panic is logged and gives
    /// `None`; the operator is dropped at the start of the next pass.
    fn ask<T>(&self, id: NodeId, what: &'static str, f: impl FnOnce() -> T) -> Option<T> {
        match ask(f) {
            Ok(v) => Some(v),
            Err(message) => {
                self.asked_panicked(id, what, message);
                None
            }
        }
    }

    fn asked_panicked(&self, id: NodeId, what: &'static str, message: String) {
        log::error!("node {id}: {what} panicked: {message}");
        penumbra_core::lock(&self.asked_panics).push((id, what, message));
    }

    /// Drops the operators that panicked in [`Cooker::ask`] since the last pass.
    fn drop_asked_panics(&mut self) {
        let asked = std::mem::take(&mut *penumbra_core::lock(&self.asked_panics));
        for (id, what, message) in asked {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.panicked_in(what, &message);
            }
        }
    }

    /// Makes `value` available to every operator through [`CookContext::resource`], replacing
    /// an earlier value of the same type. Nodes that failed are tried again, since the failure
    /// may have been the missing resource (a `tex` node cooked for a projector before the GPU
    /// arrived).
    pub fn provide<T: Any + Send + Sync>(&mut self, value: T) {
        self.services.resources.insert(TypeId::of::<T>(), Box::new(value));
        for rt in self.runtimes.values_mut().filter(|rt| rt.error.is_some()) {
            rt.notified = true;
        }
    }

    pub fn resource<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.services.resources.get(&TypeId::of::<T>()).and_then(|r| r.downcast_ref())
    }

    /// The directory relative file parameters are resolved against (the project directory).
    pub fn set_base_dir(&mut self, dir: Option<PathBuf>) {
        self.services.base_dir = dir;
    }

    /// Whether the show clock waits for every frame from now on (stepping, offline recording),
    /// so operators wait for slow results instead of handing on older ones
    /// ([`CookContext::exact_frames`]).
    pub fn set_exact_frames(&mut self, on: bool) {
        self.exact = on;
    }

    pub fn thumbnail_policy(&self) -> ThumbnailPolicy {
        self.thumbs
    }

    pub fn set_thumbnail_policy(&mut self, policy: ThumbnailPolicy) {
        self.thumbs = policy;
    }

    /// One cook pass at `time` against snapshot `net`.
    pub fn cook(
        &mut self,
        net: &Arc<Network>,
        time: FrameTime,
        outputs: &[NodeId],
        thumbnails: &[NodeId],
    ) -> CookReport {
        self.cook_showing(net, time, outputs, thumbnails, &[])
    }

    /// [`Cooker::cook`], where `shown` are outputs someone looks at (a viewer, a snapshot):
    /// they count as read, so lazy ones among them are made ([`NodeDesc::lazy_output`]). Their
    /// nodes are cooked as `outputs` are.
    pub fn cook_showing(
        &mut self,
        net: &Arc<Network>,
        time: FrameTime,
        outputs: &[NodeId],
        thumbnails: &[NodeId],
        shown: &[Port],
    ) -> CookReport {
        let start = Instant::now();
        self.pass += 1;
        let mut report = CookReport { pass: self.pass, ..CookReport::default() };
        self.sync(net);
        self.drop_asked_panics();
        for id in self.done.try_iter() {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.notified = true;
            }
        }
        let synced = Instant::now();
        self.find_reads(outputs, thumbnails, shown);
        let found = Instant::now();
        for &id in outputs.iter().chain(shown.iter().map(|p| &p.node)) {
            self.evaluate(id, time, &mut report);
        }
        for id in self.eager.clone() {
            self.evaluate(id, time, &mut report);
        }
        let evaluated = Instant::now();
        self.for_thumbnails = true;
        self.cook_thumbnails(thumbnails, time, start, &mut report);
        self.for_thumbnails = false;
        let thumbs = Instant::now();
        if self.pass.is_multiple_of(TRIM_CHECK_EVERY) {
            self.trim_idle();
        }
        let end = Instant::now();
        let spent = &mut self.spent;
        spent.passes += 1;
        spent.sync += synced - start;
        spent.reads += found - synced;
        spent.outputs += evaluated - found;
        spent.thumbnails += thumbs - evaluated;
        spent.trim += end - thumbs;
        report.elapsed = end - start;
        report
    }

    /// Where the time of the passes went since the last call with `reset` (or since the
    /// cooker started): per step of the pass, and per node that a pass looked at. With
    /// `reset`, counting starts afresh.
    pub fn take_profile(&mut self, reset: bool) -> Profile {
        let passes = self.spent;
        let nodes = self
            .runtimes
            .iter_mut()
            .filter(|(_, rt)| rt.spent.visits > 0)
            .map(|(id, rt)| {
                let spent = rt.spent;
                if reset {
                    rt.spent = NodeSpent::default();
                }
                (*id, spent)
            })
            .collect();
        if reset {
            self.spent = PassSpent::default();
        }
        Profile { passes, nodes }
    }

    /// Trims the operators of nodes that have not cooked for [`TRIM_AFTER_PASSES`]
    /// ([`Operator::trim`]), each once until it cooks again.
    fn trim_idle(&mut self) {
        let pass = self.pass;
        for (id, rt) in &mut self.runtimes {
            if rt.trimmed || pass.saturating_sub(rt.cooked_pass) < TRIM_AFTER_PASSES {
                continue;
            }
            rt.trimmed = true;
            if let Some(op) = rt.op.as_mut()
                && let Err(p) = catch_unwind(AssertUnwindSafe(|| op.trim()))
            {
                log::error!("node {id}: trim panicked: {}", panic_message(p.as_ref()));
                rt.op = None;
                rt.notified = true;
            }
            rt.measure(*id);
        }
    }

    fn cook_thumbnails(
        &mut self,
        thumbnails: &[NodeId],
        time: FrameTime,
        pass_start: Instant,
        report: &mut CookReport,
    ) {
        let n = thumbnails.len();
        if n == 0 {
            return;
        }
        let start = Instant::now();
        let headroom = time.rate.period().mul_f64(THUMB_FRAME_SHARE);
        let first = self.thumb_cursor % n;
        for k in 0..n {
            let i = (first + k) % n;
            let Some(&id) = thumbnails.get(i) else { continue };
            let Some(rt) = self.runtimes.get(&id) else { continue };
            if rt.evaluated_pass == self.pass {
                report.thumbnails.push(id);
                continue;
            }
            if self.thumbs.mode != ThumbnailMode::Always {
                continue;
            }
            let interval = u64::from(self.thumbs.min_interval);
            if rt.last_thumb_pass.is_some_and(|p| self.pass.saturating_sub(p) < interval) {
                report.thumbnails_deferred += 1;
                continue;
            }
            if start.elapsed() >= self.thumbs.budget && pass_start.elapsed() >= headroom {
                // Start here next pass, so the ones at the back are not starved.
                self.thumb_cursor = i;
                report.thumbnails_deferred += n - k;
                return;
            }
            let before = report.cooked.len();
            self.evaluate(id, time, report);
            // Only an actual cook uses up the node's slot; a clean check is free.
            if report.cooked.len() > before
                && let Some(rt) = self.runtimes.get_mut(&id)
            {
                rt.last_thumb_pass = Some(self.pass);
            }
            report.thumbnails.push(id);
        }
        self.thumb_cursor = first.wrapping_add(1);
    }

    /// Bring runtimes in line with the snapshot: drop deleted nodes, create new ones, flag
    /// changed parameters. Unchanged nodes keep their operator and outputs.
    fn sync(&mut self, net: &Arc<Network>) {
        if self.last_net.as_ref().is_some_and(|last| Arc::ptr_eq(last, net)) {
            return;
        }
        self.runtimes.retain(|id, _| net.node(*id).is_some());
        for def in net.nodes() {
            match self.runtimes.get_mut(&def.id) {
                Some(rt) if Arc::ptr_eq(&rt.def, def) => {}
                Some(rt) if rt.def.kind == def.kind => {
                    if rt.def.params != def.params {
                        rt.params_changed = true;
                    }
                    rt.def = def.clone();
                }
                _ => {
                    let mut rt = Runtime::new(def.clone(), &self.types);
                    rt.restored_place = self.pending_places.remove(&def.id);
                    self.runtimes.insert(def.id, rt);
                }
            }
        }
        self.pending_places.retain(|id, _| net.node(*id).is_some());
        self.eager = self
            .runtimes
            .iter()
            .filter(|(_, rt)| rt.ty.as_ref().is_some_and(|t| t.desc().eager))
            .map(|(id, _)| *id)
            .collect();
        self.lazy = self
            .runtimes
            .values()
            .any(|rt| rt.ty.as_ref().is_some_and(|t| t.desc().outputs.iter().any(|o| o.lazy)));
        self.last_net = Some(net.clone());
    }

    /// Works out which ports this pass reads ([`Cooker::read`]): everything wired into a node
    /// upstream of the roots, plus what is shown. Choosers are not asked (their choice depends
    /// on bound values not cooked yet), so this may count a branch they skip as read.
    fn find_reads(&mut self, outputs: &[NodeId], thumbnails: &[NodeId], shown: &[Port]) {
        self.read.clear();
        self.read_all.clear();
        if !self.lazy {
            return;
        }
        let mut read: BTreeSet<Port> = shown.iter().copied().collect();
        let mut read_all = BTreeSet::new();
        let mut stack: Vec<NodeId> =
            outputs.iter().chain(shown.iter().map(|p| &p.node)).chain(&self.eager).copied().collect();
        if self.thumbs.mode == ThumbnailMode::Always {
            stack.extend(thumbnails.iter().copied());
        }
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(rt) = self.runtimes.get(&id) else { continue };
            for port in rt.def.inputs.iter().flatten() {
                read.insert(*port);
                stack.push(port.node);
            }
            stack.extend(rt.def.binds.values().map(|b| b.node));
            for target in self.current_refs(rt) {
                read_all.insert(target);
                stack.push(target);
            }
        }
        self.read = read;
        self.read_all = read_all;
    }

    /// Per output of node `id`: whether it is to be made in this pass.
    fn wanted(&self, id: NodeId, desc: &NodeDesc) -> Vec<bool> {
        let all = self.read_all.contains(&id);
        desc.outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                !o.lazy
                    || all
                    || u32::try_from(i).is_ok_and(|output| self.read.contains(&Port { node: id, output }))
            })
            .collect()
    }

    /// Cook `root` and whatever it depends on, upstream first. A node whose type chooses its
    /// inputs ([`NodeType::input_chooser`]) is taken in two steps: first what its parameters
    /// depend on (bindings, node references) cooks, then, with the bound values known, it
    /// picks its inputs and only those are followed upstream.
    fn evaluate(&mut self, root: NodeId, time: FrameTime, report: &mut CookReport) {
        let mut stack = vec![(root, Visit::Expand)];
        let mut expanded = BTreeSet::new();
        while let Some((id, visit)) = stack.pop() {
            let Some(rt) = self.runtimes.get(&id) else { continue };
            let upstream: Vec<NodeId> = match visit {
                Visit::Cook => {
                    let started = Instant::now();
                    self.cook_one(id, time, report);
                    let took = started.elapsed();
                    if let Some(rt) = self.runtimes.get_mut(&id) {
                        rt.spent.visits += 1;
                        rt.spent.visit += took;
                    }
                    continue;
                }
                Visit::Expand => {
                    if rt.evaluated_pass == self.pass || !expanded.insert(id) {
                        continue;
                    }
                    if chooser(rt).is_some() {
                        stack.push((id, Visit::Choose));
                        let steering = steering(rt).filter_map(|i| rt.def.inputs.get(i).copied().flatten());
                        rt.def
                            .binds
                            .values()
                            .map(|b| b.node)
                            .chain(self.current_refs(rt))
                            .chain(steering.map(|p| p.node))
                            .collect()
                    } else {
                        stack.push((id, Visit::Cook));
                        rt.def.upstream().chain(self.current_refs(rt)).collect()
                    }
                }
                Visit::Choose => {
                    stack.push((id, Visit::Cook));
                    let needed = self.needed(rt, &self.resolve_binds(rt));
                    rt.def
                        .inputs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !rt.def.is_previous(*i) && is_needed(needed.as_deref(), *i))
                        .filter_map(|(_, p)| p.map(|p| p.node))
                        .collect()
                }
            };
            // Reversed, so the first input is cooked first.
            stack.extend(upstream.into_iter().rev().map(|n| (n, Visit::Expand)));
        }
    }

    /// Which inputs of a node are needed with parameters `bound` ([`NodeType::input_chooser`]),
    /// one flag per input; `None` for all. A chooser that panics needs every input.
    fn needed(&self, rt: &Runtime, bound: &BTreeMap<Name, Value>) -> Option<Vec<bool>> {
        let choose = chooser(rt)?;
        let desc = rt.ty.as_ref()?.desc();
        let mut chans = vec![None; rt.def.inputs.len()];
        for i in steering(rt) {
            let data = rt.def.inputs.get(i).copied().flatten().and_then(|port| {
                self.runtimes.get(&port.node)?.outputs.as_ref()?.get(port.output as usize)?.get::<Chans>()
            });
            if let Some(slot) = chans.get_mut(i) {
                *slot = data;
            }
        }
        let cx = InputChoice { def: &rt.def, desc, bound, chans };
        let picked = match catch_unwind(AssertUnwindSafe(|| choose(&cx))) {
            Ok(picked) => picked?,
            Err(p) => {
                log::error!("node {}: choosing inputs panicked: {}", rt.def.id, panic_message(p.as_ref()));
                return None;
            }
        };
        let mut mask = vec![false; rt.def.inputs.len()];
        for i in steering(rt) {
            if let Some(m) = mask.get_mut(i) {
                *m = true;
            }
        }
        for i in picked {
            if let Some(m) = mask.get_mut(i) {
                *m = true;
            }
        }
        Some(mask)
    }

    /// Whether `port` has no result and none is on its way ([`CookContext::input_stuck`]): walks up
    /// from a source without a result, through what it needs, to a failure or to a node that
    /// waits for something of its own.
    fn stuck(&self, port: Port) -> bool {
        let mut seen = BTreeSet::new();
        let mut todo = vec![port];
        while let Some(port) = todo.pop() {
            let Some(s) = self.runtimes.get(&port.node) else { return true };
            if let Some(outputs) = &s.outputs {
                if outputs.get(port.output as usize).is_none() {
                    return true;
                }
                continue;
            }
            if s.error.is_some() {
                return true;
            }
            if !seen.insert(port.node) {
                continue;
            }
            let needed = self.needed(s, &s.seen_bound);
            for (i, slot) in s.def.inputs.iter().enumerate() {
                if let Some(up) = slot
                    && !s.def.is_previous(i)
                    && is_needed(needed.as_deref(), i)
                {
                    todo.push(*up);
                }
            }
        }
        false
    }

    fn cook_one(&mut self, id: NodeId, time: FrameTime, report: &mut CookReport) {
        let Some(rt) = self.runtimes.get_mut(&id) else { return };
        if rt.evaluated_pass == self.pass {
            return;
        }
        // Taken out and put back below, so the bound values are brought up to date in place
        // and an unchanged text costs no allocation.
        let mut bound = std::mem::take(&mut rt.seen_bound);
        let Some(rt) = self.runtimes.get(&id) else { return };
        let mut bind_warnings = Vec::new();
        let bound_changed = self.update_bound(rt, &mut bound, Some(&mut bind_warnings));
        let bind_warning = (!bind_warnings.is_empty()).then(|| bind_warnings.join("; "));
        let needed = self.needed(rt, &bound);
        let mut inputs = Vec::with_capacity(rt.def.inputs.len());
        let mut signature = Vec::with_capacity(rt.def.inputs.len());
        let mut stuck = Vec::new();
        // Sources on a loop back to this node: everything a node needs is evaluated before it
        // in a pass, except what waits for the node itself. A node reference can close such a
        // loop (also onto the node itself). Their new results do not recook the node, or the
        // loop would cook every frame.
        let mut loops = Vec::new();
        for (i, slot) in rt.def.inputs.iter().enumerate() {
            let Some(port) = slot else {
                inputs.push(None);
                signature.push(None);
                continue;
            };
            // An input the node does not need now reads as nothing, and what its source does
            // meanwhile does not recook the node. Which inputs are needed follows from params
            // and wiring only, so a change there recooks the node anyway.
            if !is_needed(needed.as_deref(), i) {
                inputs.push(None);
                signature.push(Some((*port, 0)));
                continue;
            }
            let src = self.runtimes.get(&port.node);
            let data =
                src.and_then(|s| s.outputs.as_ref()).and_then(|o| o.get(port.output as usize)).cloned();
            let previous = rt.def.is_previous(i);
            let looped = !previous && src.is_some_and(|s| s.evaluated_pass != self.pass);
            if looped {
                let name =
                    rt.ty.as_ref().and_then(|t| t.desc().inputs.get(i)).map_or("?", |s| s.name.as_str());
                loops.push(format!("input `{name}`"));
            }
            let is_stuck = !previous && data.is_none() && self.stuck(*port);
            if is_stuck {
                stuck.push(i);
            }
            inputs.push(data.filter(|_| !(rt.blank_previous && previous)));
            // A previous-result input does not recook the node when its source changes (the
            // node is time-dependent; otherwise a loop would recook itself forever).
            let version = if previous || looped {
                0
            } else if is_stuck {
                // A source that goes from waiting to failed keeps its version, and a node waiting
                // on it must hear of that.
                u64::MAX
            } else {
                src.map_or(0, |s| s.output_version(port.output))
            };
            signature.push(Some((*port, version)));
        }
        let refs = self.refs(rt);
        let mut ref_data = BTreeMap::new();
        for (name, target) in refs {
            let data = target.map(|t| {
                let t = t?;
                let src = self.runtimes.get(&t);
                let looped = src.is_some_and(|s| s.evaluated_pass != self.pass);
                if looped {
                    loops.push(format!("`{name}`"));
                }
                // A new result of a referenced node recooks this one, as an input would.
                let version = if looped { 0 } else { src.map_or(0, |s| s.version) };
                signature.push(Some((Port { node: t, output: u32::MAX }, version)));
                src.and_then(|s| s.outputs.clone())
            });
            ref_data.insert(name, data);
        }

        let loop_warning = (!loops.is_empty()).then(|| {
            format!(
                "{} leads back to this node through a node reference; new results there do not recook it",
                loops.join(", ")
            )
        });

        let wanted = match rt.ty.as_ref().map(|t| t.desc()) {
            Some(desc) if self.lazy && desc.outputs.iter().any(|o| o.lazy) => self.wanted(id, desc),
            Some(desc) => vec![true; desc.outputs.len()],
            None => Vec::new(),
        };

        let Some(rt) = self.runtimes.get_mut(&id) else { return };
        rt.evaluated_pass = self.pass;
        rt.seen_bound = bound;
        if rt.bind_warning != bind_warning {
            rt.bind_warning = bind_warning;
        }
        let since_last_use = if self.for_thumbnails {
            None
        } else {
            let last = rt.used.replace(time);
            last.map(|t| time.seconds() - t.seconds())
        };
        // A locked node holds what it has. Its seen inputs stay as they were, so after
        // unlocking it recooks exactly when something changed meanwhile. A node locked before
        // it ever produced anything cooks once to have something to hold.
        if rt.def.locked && rt.outputs.is_some() {
            // The bound values were brought up to date; a change recooks the node once it is
            // unlocked.
            rt.params_changed |= bound_changed;
            return;
        }
        let Some(ty) = rt.ty.clone() else {
            rt.error = Some(format!("unknown node kind `{}`", rt.def.kind));
            return;
        };
        let desc = ty.desc();
        let follows_clock = match time_dependent(desc, rt.op.as_deref()) {
            Ok(t) => t,
            Err(message) => {
                rt.panicked_in("time_dependent", &message);
                return;
            }
        };
        let time_moved = follows_clock && rt.seen_tick != Some(time.tick);
        let inputs_changed = rt.seen_inputs != signature;
        let wanted_changed = !rt.fresh && rt.seen_wanted != wanted;
        // A channel driving a parameter is a parameter change, but only when the value moved:
        // a source that recooked to the same number does not recook this node.
        let params_changed = rt.params_changed || bound_changed;
        if !(rt.fresh || params_changed || rt.notified || time_moved || inputs_changed || wanted_changed) {
            return;
        }
        // A node whose operator panicked is made anew only after an edit of its params, a change
        // of its wiring, a finished job or a pause, so a deterministic panic does not repeat
        // every frame. The clock and new results upstream are no reason.
        if let Some(at) = rt.panicked {
            let rewired = !same_ports(&rt.seen_inputs, &signature);
            if !(rt.fresh || rt.params_changed || rt.notified || rewired || at.elapsed() >= PANIC_RETRY) {
                return;
            }
        }

        let first_cook = rt.op.is_none();
        let op = rt.op.get_or_insert_with(|| ty.create());
        let restore_warning = rt.restored.take().and_then(|items| {
            match catch_unwind(AssertUnwindSafe(|| op.restore_state(items))) {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("the saved state was not taken: {e}")),
                Err(p) => Some(format!("restoring the saved state panicked: {}", panic_message(p.as_ref()))),
            }
        });
        if let Some(place) = rt.restored_place.take() {
            match catch_unwind(AssertUnwindSafe(|| op.restore_show_place(&place))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    log::info!("node {id}: the place kept from before the restart was not taken: {e}")
                }
                Err(p) => {
                    log::error!("node {id}: restore_show_place panicked: {}", panic_message(p.as_ref()))
                }
            }
        }
        let cx = CookContext {
            node: id,
            def: &rt.def,
            desc,
            time,
            time_read: Cell::new(false),
            inputs: &inputs,
            stuck: &stuck,
            refs: &ref_data,
            bound: &rt.seen_bound,
            first_cook,
            params_changed,
            inputs_changed,
            wanted: &wanted,
            wanted_changed,
            since_last_use,
            exact: self.exact,
            spawner: &self.spawner,
            services: &self.services,
            warning: RefCell::new(None),
        };
        rt.cooked_pass = self.pass;
        rt.trimmed = false;
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| op.cook(&cx)));
        let panicked = result.is_err();
        let elapsed = started.elapsed();
        let read_time = cx.time_read.get();
        let op_warning = cx.warning.take();

        match result {
            Ok(Ok(Cooked::Outputs(outs))) => match check_outputs(desc, &outs) {
                Ok(()) => {
                    self.next_version += 1;
                    let version = self.next_version;
                    let before = rt.outputs.as_deref().unwrap_or(&[]);
                    rt.output_versions = outs
                        .iter()
                        .enumerate()
                        .map(|(i, d)| match (before.get(i), rt.output_versions.get(i)) {
                            (Some(old), Some(&v)) if old.same(d) => v,
                            _ => version,
                        })
                        .collect();
                    rt.outputs = Some(outs);
                    rt.version = version;
                    rt.error = None;
                }
                Err(e) => rt.error = Some(e),
            },
            Ok(Ok(Cooked::Unchanged) | Err(CookError::Pending)) => rt.error = None,
            Ok(Err(CookError::Failed(message))) => rt.error = Some(message),
            Err(payload) => {
                rt.error = Some(format!("node panicked: {}", panic_message(payload.as_ref())));
                rt.op = None;
            }
        }
        rt.panicked = panicked.then(Instant::now);
        // Asked again after the cook, which is when an operator learns what it depends on.
        let declared = !read_time
            || match time_dependent(desc, rt.op.as_deref()) {
                Ok(t) => t,
                Err(message) => {
                    rt.panicked_in("time_dependent", &message);
                    true
                }
            };
        let time_warning = (!declared)
            .then(|| format!("`{}` reads the show time but is not declared time-dependent", desc.kind));
        // Values the type does not declare, kept from the project file (`Params::Keep`).
        let stray: Vec<&str> = if desc.extra_params {
            Vec::new()
        } else {
            rt.def.params.keys().map(|n| n.as_str()).filter(|n| desc.param_spec(n).is_none()).collect()
        };
        let stray_warning = (!stray.is_empty()).then(|| {
            format!("`{}` has no param {}; the value is ignored, remove it", desc.kind, quoted(&stray))
        });
        rt.warning = [restore_warning, time_warning, op_warning, stray_warning, loop_warning]
            .into_iter()
            .flatten()
            .reduce(|a, b| format!("{a}; {b}"));
        rt.seen_inputs = signature;
        rt.seen_tick = Some(time.tick);
        rt.seen_wanted = wanted;
        rt.fresh = false;
        rt.params_changed = false;
        rt.notified = false;
        rt.blank_previous = false;
        rt.stats.add(elapsed);
        rt.spent.cooks += 1;
        rt.spent.cook += elapsed;
        rt.spent.cook_max = rt.spent.cook_max.max(elapsed);
        rt.measure(id);
        report.cooked.push(id);
    }

    /// Which of `ids` hold outputs that are up to date at `time` without cooking: nothing
    /// about the node or anything upstream changed since it last cooked (parameters, inputs,
    /// a finished job, the clock for time-dependent nodes). Locked nodes count as current, they
    /// hold their outputs on purpose. Meant for pictures shown without cooking (thumbnails that
    /// only show what cooked anyway): an outdated picture is worse than none. Call after
    /// [`Self::cook`] for the same time.
    pub fn current(&self, ids: &[NodeId], time: FrameTime) -> BTreeSet<NodeId> {
        let mut known: BTreeMap<NodeId, bool> = BTreeMap::new();
        // Node references can form loops the network cannot see; each node is expanded once.
        let mut expanded: BTreeSet<NodeId> = BTreeSet::new();
        for &root in ids {
            // Upstream first, iteratively (chains can be long).
            let mut stack = vec![(root, false)];
            while let Some((id, done)) = stack.pop() {
                if known.contains_key(&id) || (!done && !expanded.insert(id)) {
                    continue;
                }
                let Some(rt) = self.runtimes.get(&id) else {
                    known.insert(id, false);
                    continue;
                };
                if !done {
                    stack.push((id, true));
                    let upstream = rt.def.upstream().chain(self.current_refs(rt));
                    stack.extend(upstream.filter(|n| !known.contains_key(n)).map(|n| (n, false)));
                    continue;
                }
                let ok = self.is_current(rt, time, &known);
                known.insert(id, ok);
            }
        }
        ids.iter().copied().filter(|id| known.get(id) == Some(&true)).collect()
    }

    /// One node, its inputs already judged in `known`.
    fn is_current(&self, rt: &Runtime, time: FrameTime, known: &BTreeMap<NodeId, bool>) -> bool {
        if rt.outputs.is_none() {
            return false;
        }
        if rt.def.locked || rt.evaluated_pass == self.pass {
            return true;
        }
        let Some(ty) = rt.ty.as_ref() else { return false };
        let follows_clock = match time_dependent(ty.desc(), rt.op.as_deref()) {
            Ok(t) => t,
            Err(message) => {
                self.asked_panicked(rt.def.id, "time_dependent", message);
                return false;
            }
        };
        let time_moved = follows_clock && rt.seen_tick != Some(time.tick);
        if rt.fresh || rt.params_changed || rt.notified || time_moved {
            return false;
        }
        let needed = if chooser(rt).is_some() { self.needed(rt, &self.resolve_binds(rt)) } else { None };
        // A node naming itself does not wait for itself.
        let refs_same = self.current_refs(rt).filter(|t| *t != rt.def.id).all(|t| {
            known.get(&t) == Some(&true)
                && rt.seen_inputs.iter().flatten().any(|(p, v)| {
                    p.node == t
                        && p.output == u32::MAX
                        && self.runtimes.get(&t).is_some_and(|s| s.version == *v)
                })
        });
        let inputs_same = refs_same
            && rt
                .def
                .inputs
                .iter()
                .zip(rt.seen_inputs.iter().chain(std::iter::repeat(&None)))
                .enumerate()
                .all(|(i, (slot, seen))| match (slot, seen) {
                    (None, None) => true,
                    (Some(port), Some((seen_port, _)))
                        if rt.def.is_previous(i) || !is_needed(needed.as_deref(), i) =>
                    {
                        port == seen_port
                    }
                    (Some(port), Some((seen_port, seen_version))) => {
                        port == seen_port
                            && known.get(&port.node) == Some(&true)
                            && self
                                .runtimes
                                .get(&port.node)
                                .is_some_and(|s| s.output_version(port.output) == *seen_version)
                    }
                    _ => false,
                });
        let binds_same =
            rt.def.binds.values().all(|b| known.get(&b.node) == Some(&true)) && self.bound_same(rt);
        inputs_same && binds_same && rt.seen_inputs.len() >= rt.def.inputs.len()
    }

    /// The node reference params of a node and the node each names: `Ok(None)` for an empty
    /// param, an error for a path nothing has.
    fn refs(&self, rt: &Runtime) -> Vec<(String, Result<Option<NodeId>, String>)> {
        let Some(desc) = rt.ty.as_ref().map(|t| t.desc()) else { return Vec::new() };
        let Some(net) = self.last_net.as_ref() else { return Vec::new() };
        desc.params
            .iter()
            .filter(|spec| spec.node_ref)
            .map(|spec| {
                let text = rt.def.param(desc, &spec.name).and_then(Value::as_str).unwrap_or("").trim();
                let target =
                    if text.is_empty() { Ok(None) } else { net.resolve_from(rt.def.id, text).map(Some) };
                (spec.name.clone(), target)
            })
            .collect()
    }

    /// The nodes a node names through node reference params: cooked before it.
    fn current_refs(&self, rt: &Runtime) -> impl Iterator<Item = NodeId> + use<> {
        self.refs(rt).into_iter().filter_map(|(_, t)| t.ok().flatten())
    }

    /// The values bound parameters take now, as a new map ([`Self::update_bound`] on the values
    /// in effect at the last cook).
    fn resolve_binds(&self, rt: &Runtime) -> BTreeMap<Name, Value> {
        let mut bound = if rt.def.binds.is_empty() { BTreeMap::new() } else { rt.seen_bound.clone() };
        self.update_bound(rt, &mut bound, None);
        bound
    }

    /// Brings `bound` from the values in effect at the last cook to the values bound
    /// parameters take now, in place, so an unchanged text costs no allocation; whether a value
    /// changed. A warning per binding that cannot be applied goes into `warnings` (the
    /// parameter falls back to its stored value then).
    fn update_bound(
        &self,
        rt: &Runtime,
        bound: &mut BTreeMap<Name, Value>,
        warnings: Option<&mut Vec<String>>,
    ) -> bool {
        let before = bound.len();
        bound.retain(|name, _| rt.def.is_bound(name.as_str()));
        let mut changed = bound.len() != before;
        self.each_bound(rt, warnings, |name, r| changed |= bound::apply(bound, name, r));
        changed
    }

    /// Whether the bindings of `rt` give the values in effect at its last cook. Does not
    /// allocate while they do.
    fn bound_same(&self, rt: &Runtime) -> bool {
        if rt.seen_bound.keys().any(|name| !rt.def.is_bound(name.as_str())) {
            return false;
        }
        let mut same = true;
        self.each_bound(rt, None, |name, r| same &= bound::same(rt.seen_bound.get(name), &r));
        same
    }

    /// Calls `f` with what each bound parameter of `rt` comes out as now; a warning per binding
    /// that cannot be applied goes into `warnings` when given.
    fn each_bound(
        &self,
        rt: &Runtime,
        mut warnings: Option<&mut Vec<String>>,
        mut f: impl FnMut(&Name, Resolved<'_>),
    ) {
        if rt.def.binds.is_empty() {
            return;
        }
        let Some(desc) = rt.ty.as_ref().map(|t| t.desc()) else { return };
        // Params a shader declares in its header; asked for only when the type lacks one.
        let mut instance: Option<Vec<ParamSpec>> = None;
        let mut warn = |w: String| {
            if let Some(ws) = warnings.as_deref_mut() {
                ws.push(w);
            }
        };
        let mut parts = rt.def.binds.iter().peekable();
        while let Some((first, _)) = parts.peek() {
            let name = &first.param;
            let spec = match desc.param_spec(name.as_str()) {
                Some(spec) => Some(spec),
                None => instance
                    .get_or_insert_with(|| {
                        rt.op
                            .as_ref()
                            .and_then(|op| self.ask(rt.def.id, "params", || op.params()).flatten())
                            .unwrap_or_default()
                    })
                    .iter()
                    .find(|s| s.name == name.as_str()),
            };
            let Some(spec) = spec else {
                while let Some((part, _)) = parts.next_if(|(p, _)| &p.param == name) {
                    warn(format!("`{part}` is bound, but the node has no such param"));
                }
                continue;
            };
            let stored = rt.def.params.get(name.as_str()).unwrap_or(&spec.default);
            let mut value: Option<Value> = None;
            let mut resolved = None;
            // All parts of one param: a vector bound per component builds one value.
            while let Some((part, bind)) = parts.next_if(|(p, _)| &p.param == name) {
                let target = match spec.bind_target(part.component) {
                    Ok(t) => t,
                    Err(e) => {
                        warn(format!("`{part}`: {e}"));
                        continue;
                    }
                };
                let read = match self.read_bind(part, bind, target) {
                    Ok(read) => read,
                    Err(w) => {
                        warn(w);
                        continue;
                    }
                };
                let source = || self.path(bind.node);
                match (target, read) {
                    (BindTarget::Number, read) => {
                        let x = match read {
                            BindRead::Number(x) => x,
                            BindRead::Text(t) => match bindv::cell_number(t) {
                                Some(x) => x,
                                None => {
                                    warn(format!(
                                        "`{part}`: {} gives \"{}\", which is no number",
                                        source(),
                                        short(t)
                                    ));
                                    continue;
                                }
                            },
                        };
                        match spec.bind_value(value.as_ref().unwrap_or(stored), part.component, x) {
                            Ok(v) => value = Some(v),
                            Err(e) => warn(format!("`{part}`: {e}")),
                        }
                    }
                    (BindTarget::Text, BindRead::Text(t)) if t.len() > MAX_TEXT => {
                        warn(format!("`{part}`: the cell of {} is longer than {MAX_TEXT} bytes", source()));
                    }
                    (BindTarget::Text, BindRead::Text(t)) => resolved = Some(Resolved::Text(t)),
                    (BindTarget::Text, BindRead::Number(x)) => {
                        let format = bind.format.as_ref().unwrap_or(&bindv::DEFAULT_FORMAT);
                        resolved = Some(Resolved::Formatted(x, format));
                    }
                    (BindTarget::Choice, read) => {
                        let ParamKind::Choice(choices) = &spec.kind else { continue };
                        let entry = match read {
                            BindRead::Number(x) => bindv::choice_at(choices, x),
                            BindRead::Text(t) => bindv::choice_named(choices, t),
                        };
                        resolved = Some(match (entry, read) {
                            (Some(e), _) => Resolved::Text(e),
                            (None, BindRead::Text(t)) => {
                                warn(format!(
                                    "`{part}`: {} gives \"{}\", which is none of {}; it keeps its last entry",
                                    source(),
                                    short(t),
                                    choices.join(", ")
                                ));
                                Resolved::Keep
                            }
                            (None, BindRead::Number(_)) => Resolved::Stored,
                        });
                    }
                }
            }
            let r = match (value, resolved) {
                (Some(v), _) => Resolved::Value(v),
                (None, Some(r)) => r,
                (None, None) => Resolved::Stored,
            };
            f(name, r);
        }
    }

    /// What binding `bind` (of `part`, driving a `target`) reads now: a channel's number or a
    /// table cell's text; a warning when there is none. A text or choice bound to a node that
    /// [binds by name](crate::NodeDesc::binds_by_name), naming neither a channel nor a cell,
    /// reads that node's first cell.
    fn read_bind<'s>(
        &'s self,
        part: &ParamPart,
        bind: &Bind,
        target: BindTarget,
    ) -> Result<BindRead<'s>, String> {
        let output = |kind: DataKind| {
            let src = self.runtimes.get(&bind.node)?;
            let i = src.ty.as_ref()?.desc().outputs.iter().position(|o| o.kind == kind)?;
            src.outputs.as_ref()?.get(i)
        };
        let by_name = bind.channel.is_none()
            && bind.cell.is_none()
            && target != BindTarget::Number
            && self
                .runtimes
                .get(&bind.node)
                .and_then(|src| src.ty.as_ref())
                .is_some_and(|t| t.desc().binds_by_name);
        let first = bindv::Cell::default();
        let cell = if by_name { Some(&first) } else { bind.cell.as_ref() };
        if let Some(cell) = cell {
            let Some(table) = output(DataKind::Table).and_then(|d| d.get::<Table>()) else {
                return Err(format!("`{part}` is bound to {}, which has no table yet", self.path(bind.node)));
            };
            return cell
                .find(table)
                .map(BindRead::Text)
                .map_err(|e| format!("`{part}`: {} has no {cell}: {e}", self.path(bind.node)));
        }
        let Some(chans) = output(DataKind::Chan).and_then(|d| d.get::<Chans>()) else {
            return Err(format!("`{part}` is bound to {}, which has no channels yet", self.path(bind.node)));
        };
        chans.value(bind.channel.as_ref().map(Name::as_str)).map(BindRead::Number).ok_or_else(|| {
            let names: Vec<&str> = chans.names().map(Name::as_str).collect();
            let has = if names.is_empty() { "none".to_string() } else { names.join(", ") };
            let which =
                bind.channel.as_ref().map_or_else(|| "any channel".into(), |c| format!("channel `{c}`"));
            format!("`{part}`: {} has no {which} (it has {has})", self.path(bind.node))
        })
    }

    /// The path of node `id` for a message.
    fn path(&self, id: NodeId) -> String {
        self.last_net
            .as_ref()
            .and_then(|n| n.path_of(id))
            .map_or_else(|| format!("node {id}"), |p| p.to_string())
    }

    /// What a state snapshot should keep, per node: the outputs of locked nodes and whatever
    /// operators hand out ([`Operator::save_state`]), including state restored for an operator
    /// that has not cooked since (so a loop nobody looks at keeps its state from file to file).
    /// Cheap: handles, no copies.
    pub fn kept(&self) -> Vec<(NodeId, Kept)> {
        let mut kept = Vec::new();
        for (&id, rt) in &self.runtimes {
            if rt.def.locked {
                if let Some(outs) = &rt.outputs {
                    kept.push((id, Kept::Outputs(outs.clone())));
                }
                continue;
            }
            if let Some(items) = &rt.restored {
                kept.push((id, Kept::State(items.clone())));
                continue;
            }
            let Some(op) = rt.op.as_ref() else { continue };
            match catch_unwind(AssertUnwindSafe(|| op.save_state())) {
                Ok(Some(items)) if !items.is_empty() => kept.push((id, Kept::State(items))),
                Ok(_) => {}
                Err(p) => log::error!("node {id}: save_state panicked: {}", panic_message(p.as_ref())),
            }
        }
        kept
    }

    /// Where the operators that keep one stand in a show ([`Operator::show_place`]). Cheap: one
    /// call per operator, most of which answer `None`.
    pub fn show_places(&self) -> Vec<(NodeId, String)> {
        let mut places = Vec::new();
        for (&id, rt) in &self.runtimes {
            if let Some(place) = &rt.restored_place {
                places.push((id, place.clone()));
                continue;
            }
            let Some(op) = rt.op.as_ref() else { continue };
            match catch_unwind(AssertUnwindSafe(|| op.show_place())) {
                Ok(Some(place)) => places.push((id, place)),
                Ok(None) => {}
                Err(p) => log::error!("node {id}: show_place panicked: {}", panic_message(p.as_ref())),
            }
        }
        places
    }

    /// Gives node `id` back its place in a show from before a restart
    /// ([`Operator::restore_show_place`]): now, or before its first cook when it has not cooked
    /// yet (also when the cooker has not seen the node yet). The node cooks again at the next
    /// pass.
    pub fn restore_show_place(&mut self, id: NodeId, place: String) -> Result<(), String> {
        let Some(rt) = self.runtimes.get_mut(&id) else {
            if self.pending_places.len() < MAX_PENDING_PLACES {
                self.pending_places.insert(id, place);
            }
            return Ok(());
        };
        if rt.def.locked {
            return Err("the node is locked".into());
        }
        match rt.op.as_mut() {
            None => rt.restored_place = Some(place),
            Some(op) => match catch_unwind(AssertUnwindSafe(|| op.restore_show_place(&place))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(p) => {
                    rt.op = None;
                    return Err(format!("restoring panicked: {}", panic_message(p.as_ref())));
                }
            },
        }
        rt.notified = true;
        Ok(())
    }

    /// Forgets what operators hold beyond their params (a feedback loop's frame, a held cache,
    /// state restored but not taken yet): each such operator starts over at its next cook, as
    /// if it were new. Locked nodes keep their outputs. Returns how many nodes started over.
    pub fn forget_state(&mut self) -> usize {
        let mut count = 0;
        for (&id, rt) in &mut self.runtimes {
            if !rt.def.locked && rt.holds_state(id) {
                rt.start_over();
                count += 1;
            }
        }
        count
    }

    /// Starts node `id` over, as if it were new: its operator is dropped (with whatever it held:
    /// a loop's frame, a cache, a learned background) and made again at the next cook. Only for
    /// a node whose type is [`NodeDesc::stateful`] or whose operator holds state now; a locked
    /// node keeps its held outputs.
    pub fn reset(&mut self, id: NodeId) -> ResetOutcome {
        let Some(rt) = self.runtimes.get_mut(&id) else { return ResetOutcome::NotCooked };
        if rt.def.locked {
            return ResetOutcome::Locked;
        }
        let stateful = rt.ty.as_ref().is_some_and(|t| t.desc().stateful);
        if !stateful && !rt.holds_state(id) {
            return ResetOutcome::Stateless;
        }
        rt.start_over();
        ResetOutcome::Reset
    }

    /// Puts back what [`Cooker::kept`] returned, maybe in an earlier run: a locked node's
    /// outputs replace what it holds; an operator's state goes to the operator (now, or before
    /// its first cook if it has none yet), and the node cooks again at the next pass. Errors
    /// say why the state does not fit; the node is left as it was then.
    pub fn restore(&mut self, id: NodeId, kept: Kept) -> Result<(), String> {
        let rt = self.runtimes.get_mut(&id).ok_or("no such node")?;
        match kept {
            Kept::Outputs(outs) => {
                if !rt.def.locked {
                    return Err("held outputs, but the node is not locked any more".into());
                }
                let ty = rt.ty.as_ref().ok_or_else(|| format!("unknown node kind `{}`", rt.def.kind))?;
                check_outputs(ty.desc(), &outs)?;
                self.next_version += 1;
                rt.output_versions = vec![self.next_version; outs.len()];
                rt.outputs = Some(outs);
                rt.version = self.next_version;
                rt.error = None;
                Ok(())
            }
            Kept::State(items) => {
                if rt.def.locked {
                    return Err("operator state, but the node is locked now".into());
                }
                match rt.op.as_mut() {
                    None => rt.restored = Some(items),
                    Some(op) => match catch_unwind(AssertUnwindSafe(|| op.restore_state(items))) {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(p) => {
                            rt.op = None;
                            return Err(format!("restoring panicked: {}", panic_message(p.as_ref())));
                        }
                    },
                }
                rt.notified = true;
                Ok(())
            }
        }
    }

    /// The last good outputs of a node.
    pub fn outputs(&self, id: NodeId) -> Option<&[Data]> {
        self.runtimes.get(&id)?.outputs.as_deref()
    }

    pub fn output(&self, port: Port) -> Option<&Data> {
        self.outputs(port.node)?.get(port.output as usize)
    }

    pub fn status(&self, id: NodeId) -> Option<NodeStatus> {
        self.status_with(id, true)
    }

    /// [`Cooker::status`] without the instance's params, which can be long (a shader's header):
    /// cheap enough for every node every frame, as the editor needs it.
    pub fn status_brief(&self, id: NodeId) -> Option<NodeStatus> {
        self.status_with(id, false)
    }

    fn status_with(&self, id: NodeId, params: bool) -> Option<NodeStatus> {
        let rt = self.runtimes.get(&id)?;
        Some(NodeStatus {
            version: rt.version,
            has_output: rt.outputs.is_some(),
            error: rt.error.clone(),
            warning: match (&rt.warning, &rt.bind_warning) {
                (Some(a), Some(b)) => Some(format!("{a}; {b}")),
                (a, b) => a.clone().or_else(|| b.clone()),
            },
            job: rt.op.as_ref().and_then(|op| self.ask(id, "job_status", || op.job_status()).flatten()),
            stats: rt.stats,
            gpu_bytes: rt.gpu_bytes,
            params: if params {
                rt.op.as_ref().and_then(|op| self.ask(id, "params", || op.params()).flatten())
            } else {
                None
            },
            bound: rt.seen_bound.clone(),
            reads: rt
                .op
                .as_ref()
                .and_then(|op| self.ask(id, "reads_inputs", || op.reads_inputs()))
                .unwrap_or(0),
            time_dependent: rt.ty.as_ref().is_some_and(|ty| {
                match time_dependent(ty.desc(), rt.op.as_deref()) {
                    Ok(t) => t,
                    Err(message) => {
                        self.asked_panicked(id, "time_dependent", message);
                        ty.desc().time_dependent
                    }
                }
            }),
        })
    }

    /// What node `id`'s operator holds on the GPU now ([`Operator::gpu_bytes`]), asked anew
    /// rather than as of its last cook; `None` for a node the cooker does not know.
    pub fn gpu_bytes(&self, id: NodeId) -> Option<u64> {
        let rt = self.runtimes.get(&id)?;
        let Some(op) = rt.op.as_ref() else { return Some(0) };
        Some(catch_unwind(AssertUnwindSafe(|| op.gpu_bytes())).unwrap_or_else(|p| {
            log::error!("node {id}: gpu_bytes panicked: {}", panic_message(p.as_ref()));
            0
        }))
    }

    /// How input `input` of node `id` relates to its output ([`Operator::density`]); `None`
    /// before the node has an operator, or when asking it panicked.
    pub fn density(&self, id: NodeId, input: usize) -> Option<Density> {
        let rt = self.runtimes.get(&id)?;
        let (op, desc) = (rt.op.as_ref()?, rt.ty.as_ref()?.desc());
        self.ask(id, "density", || op.density(&rt.def, desc, input))
    }

    /// Whether node `id`'s `width` and `height` set its resolution ([`Operator::sets_size`]).
    pub fn sets_size(&self, id: NodeId) -> bool {
        self.runtimes
            .get(&id)
            .and_then(|rt| {
                let (op, desc) = (rt.op.as_ref()?, rt.ty.as_ref()?.desc());
                self.ask(id, "sets_size", || op.sets_size(&rt.def, desc))
            })
            .unwrap_or(false)
    }
}

/// A step of [`Cooker::evaluate`] for one node.
#[derive(Clone, Copy)]
enum Visit {
    /// Follow what it depends on.
    Expand,
    /// Its parameters' sources have cooked: pick its inputs and follow them.
    Choose,
    /// Everything it needs has cooked: cook it.
    Cook,
}

fn chooser(rt: &Runtime) -> Option<InputChooser> {
    rt.ty.as_ref()?.input_chooser()
}

/// The indices of a node's steering inputs ([`InputSpec::steers`](crate::node::InputSpec::steers)): its chooser reads them, so
/// they cook before it chooses and are always needed.
fn steering(rt: &Runtime) -> impl Iterator<Item = usize> + '_ {
    let inputs = rt.ty.as_ref().map(|t| t.desc().inputs.as_slice()).unwrap_or_default();
    inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| i.steers && i.kind == DataKind::Chan && !i.previous)
        .map(|(i, _)| i)
}

/// Whether input `i` is needed by a mask from [`Cooker::needed`] (`None`: all are).
fn is_needed(mask: Option<&[bool]>, i: usize) -> bool {
    mask.is_none_or(|m| m.get(i).copied().unwrap_or(true))
}

/// Whether a node follows the clock, as its operator says or else its type; the message when
/// asking the operator panicked.
fn time_dependent(desc: &NodeDesc, op: Option<&dyn Operator>) -> Result<bool, String> {
    let Some(op) = op else { return Ok(desc.time_dependent) };
    ask(|| op.time_dependent()).map(|t| t.unwrap_or(desc.time_dependent))
}

/// Runs `f`, a question to an operator outside its cook; a panic comes back as its message.
fn ask<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| panic_message(p.as_ref()))
}

/// Whether two input signatures name the same sources, whatever their versions.
fn same_ports(a: &[Option<(Port, u64)>], b: &[Option<(Port, u64)>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.map(|(p, _)| p) == y.map(|(p, _)| p))
}

fn check_outputs(desc: &NodeDesc, outs: &[Data]) -> Result<(), String> {
    if outs.len() != desc.outputs.len() {
        return Err(format!(
            "`{}` produced {} outputs but declares {}",
            desc.kind,
            outs.len(),
            desc.outputs.len()
        ));
    }
    for (spec, data) in desc.outputs.iter().zip(outs) {
        if spec.kind != data.kind() {
            return Err(format!(
                "`{}` output `{}` is declared {:?} but produced {:?}",
                desc.kind,
                spec.name,
                spec.kind,
                data.kind()
            ));
        }
    }
    Ok(())
}

/// `a`, `b` and `c` as `` `a`, `b`, `c` ``.
fn quoted(names: &[&str]) -> String {
    names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Op;
    use crate::testing::{create, test_types};
    use penumbra_core::{NodePath, ParamPath, ShowClock};

    struct Fixture {
        types: Arc<NodeTypes>,
        net: Arc<Network>,
        cooker: Cooker,
        clock: ShowClock,
    }

    impl Fixture {
        fn new(ops: &[Op]) -> Self {
            let types = Arc::new(test_types());
            let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
            let pool = Arc::new(JobPool::new(2).unwrap());
            let cooker = Cooker::new(types.clone(), pool);
            Self { types, net, cooker, clock: ShowClock::new(Rate::FPS_60, Default::default()) }
        }

        fn id(&self, path: &str) -> NodeId {
            self.net.resolve(&NodePath::parse(path).unwrap()).unwrap()
        }

        fn keep(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply_with(&self.types, ops, crate::Params::Keep).unwrap().network);
        }

        fn edit(&mut self, ops: &[Op]) {
            self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        }

        fn set(&mut self, param: &str, value: Value) {
            self.edit(&[Op::Set { param: ParamPath::parse(param).unwrap(), value }]);
        }

        fn cook(&mut self, outputs: &[&str]) -> Vec<String> {
            self.cook_with(outputs, &[]).0
        }

        fn cook_with(&mut self, outputs: &[&str], thumbs: &[&str]) -> (Vec<String>, CookReport) {
            let outs: Vec<NodeId> = outputs.iter().map(|p| self.id(p)).collect();
            let th: Vec<NodeId> = thumbs.iter().map(|p| self.id(p)).collect();
            let report = self.cooker.cook(&self.net, self.clock.now(), &outs, &th);
            let names = report.cooked.iter().map(|id| self.net.path_of(*id).unwrap().to_string()).collect();
            (names, report)
        }

        fn value(&self, path: &str) -> Option<f64> {
            self.cooker.output(Port { node: self.id(path), output: 0 })?.get::<Chans>()?.value(None)
        }

        fn status(&self, path: &str) -> NodeStatus {
            self.cooker.status(self.id(path)).unwrap()
        }
    }

    fn pp(s: &str) -> NodePath {
        NodePath::parse(s).unwrap()
    }

    fn connect(from: &str, to: &str, input: u32) -> Op {
        Op::Connect { from: pp(from), output: 0, to: pp(to), input }
    }

    /// `/a` and `/b` into `/sum`, `/sum` into `/out`.
    fn sum_network() -> Fixture {
        Fixture::new(&[
            create("/a", "test.const"),
            create("/b", "test.const"),
            create("/sum", "test.add"),
            create("/out", "test.sink"),
            connect("/a", "/sum", 0),
            connect("/b", "/sum", 1),
            connect("/sum", "/out", 0),
        ])
    }

    #[test]
    fn current_tells_which_pictures_are_up_to_date_without_cooking() {
        let mut f = Fixture::new(&[
            create("/k", "test.const"),
            create("/t", "test.time"),
            create("/sum", "test.add"),
            create("/k2", "test.add"),
            connect("/t", "/sum", 0),
            connect("/k", "/sum", 1),
            connect("/k", "/k2", 0),
        ]);
        let all = ["/k", "/t", "/sum", "/k2"];
        f.cook(&all);
        let ids: Vec<NodeId> = all.iter().map(|p| f.id(p)).collect();
        assert_eq!(f.cooker.current(&ids, f.clock.now()).len(), 4, "everything just cooked");
        // The clock moves and only /k is cooked: the time-dependent branch is outdated.
        f.clock.advance(1);
        f.cook(&["/k"]);
        let current = f.cooker.current(&ids, f.clock.now());
        assert_eq!(current, [f.id("/k"), f.id("/k2")].into());
        // A param edit outdates the node and what reads it, until it cooks.
        f.set("/k.value", Value::Float(5.0));
        f.cook(&[]);
        let current = f.cooker.current(&ids, f.clock.now());
        assert!(!current.contains(&f.id("/k")) && !current.contains(&f.id("/k2")), "{current:?}");
        f.cook(&["/k2"]);
        assert!(f.cooker.current(&[f.id("/k2")], f.clock.now()).contains(&f.id("/k2")));
    }

    #[test]
    fn a_lazy_output_is_made_only_while_read() {
        let mut f = Fixture::new(&[
            create("/l", "test.lazy"),
            create("/use_out", "test.sink"),
            create("/use_extra", "test.sink"),
            connect("/l", "/use_out", 0),
        ]);
        let extra = |f: &Fixture| {
            let d = f.cooker.output(Port { node: f.id("/l"), output: 1 }).unwrap().clone();
            (d.is_skipped(), d.get::<Chans>().and_then(|c| c.value(None)))
        };
        // Only `out` is wired: `extra` is skipped.
        assert_eq!(f.cook(&["/use_out"]), ["/l", "/use_out"]);
        assert_eq!(extra(&f), (true, None));
        // A thumbnail of the node shows its first output and does not read `extra`.
        f.cooker.set_thumbnail_policy(ThumbnailPolicy { mode: ThumbnailMode::Always, ..Default::default() });
        assert!(f.cook_with(&["/use_out"], &["/l"]).0.is_empty());
        // Wired into a node the pass cooks: made, with nothing else changed.
        f.edit(&[Op::Connect { from: pp("/l"), output: 1, to: pp("/use_extra"), input: 0 }]);
        assert_eq!(f.cook(&["/use_out", "/use_extra"]), ["/l", "/use_out", "/use_extra"]);
        assert_eq!(extra(&f), (false, Some(100.0)));
        assert!(f.cook(&["/use_out", "/use_extra"]).is_empty(), "nothing changed");
        // The reader no longer cooks: skipped again.
        assert_eq!(f.cook(&["/use_out"]), ["/l", "/use_out"]);
        assert_eq!(extra(&f), (true, None));
        // Shown (a viewer, a snapshot): made.
        let shown = [Port { node: f.id("/l"), output: 1 }];
        let report = f.cooker.cook_showing(&f.net, f.clock.now(), &[f.id("/use_out")], &[], &shown);
        // (The reader of `out` recooks too: a node's outputs share one version.)
        assert_eq!(report.cooked, [f.id("/l"), f.id("/use_out")]);
        assert_eq!(extra(&f), (false, Some(100.0)));
    }

    #[test]
    fn an_output_handed_on_unchanged_does_not_recook_its_readers() {
        let mut f = Fixture::new(&[
            create("/p", "test.pair"),
            create("/use_a", "test.sink"),
            create("/use_b", "test.sink"),
            connect("/p", "/use_a", 0),
            Op::Connect { from: pp("/p"), output: 1, to: pp("/use_b"), input: 0 },
        ]);
        f.cook(&["/use_a", "/use_b"]);
        f.set("/p.b", Value::Float(2.0));
        assert_eq!(f.cook(&["/use_a", "/use_b"]), ["/p", "/use_b"]);
        f.set("/p.a", Value::Float(3.0));
        assert_eq!(f.cook(&["/use_a", "/use_b"]), ["/p", "/use_a"]);
        assert!(f.cooker.current(&[f.id("/use_a"), f.id("/use_b")], f.clock.now()).len() == 2);
    }

    #[test]
    fn a_stray_param_is_a_warning() {
        let mut f = Fixture::new(&[]);
        f.keep(&[Op::Create {
            path: NodePath::parse("/k").unwrap(),
            kind: "test.const".into(),
            params: [("opacity".parse().unwrap(), Value::Float(0.7))].into(),
        }]);
        assert_eq!(f.cook(&["/k"]), ["/k"]);
        let w = f.status("/k").warning.unwrap();
        assert!(w.contains("`test.const` has no param `opacity`"), "{w}");
        assert_eq!(f.status("/k").error, None);
    }

    #[test]
    fn instance_params_resources_files_and_time() {
        let dir = penumbra_core::scratch::ScratchDir::new("cook-open").unwrap();
        std::fs::write(dir.join("f.txt"), "abcd").unwrap();
        let mut f = Fixture::new(&[create("/o", "test.open")]);
        f.cooker.provide(2.0f64);
        f.cooker.set_base_dir(Some(dir.to_path_buf()));
        f.set("/o.file", "f.txt".into());
        f.set("/o.gain", Value::Float(3.0));
        f.set("/o.bogus", Value::Int(1));
        assert_eq!(f.cook(&["/o"]), ["/o"]);
        assert_eq!(f.value("/o"), Some(24.0));
        let s = f.status("/o");
        assert_eq!(s.warning.as_deref(), Some("unknown param `bogus`"));
        assert_eq!(s.params.unwrap().len(), 2);
        assert!(!s.time_dependent);
        assert_eq!(f.cooker.density(f.id("/o"), 0), Some(Density::needs([2.0; 2])));
        // Undeclared values must still be finite; resetting one is allowed.
        let nan = Op::Set { param: ParamPath::parse("/o.x").unwrap(), value: Value::Float(f64::NAN) };
        assert!(f.net.apply(&f.types, &[nan]).is_err());
        f.edit(&[Op::Reset { param: ParamPath::parse("/o.bogus").unwrap() }]);

        // Not time-dependent: time moves, nothing cooks. Animated: it follows the clock.
        f.cook(&["/o"]);
        f.clock.advance(1);
        assert!(f.cook(&["/o"]).is_empty());
        f.set("/o.animate", Value::Bool(true));
        f.cook(&["/o"]);
        assert!(f.status("/o").time_dependent);
        assert_eq!(f.status("/o").warning, None);
        f.clock.advance(1);
        assert_eq!(f.cook(&["/o"]), ["/o"]);
        f.set("/o.animate", Value::Bool(false));
        f.cook(&["/o"]);

        // The watched file changes on disk: the node recooks without any edit. File times follow a
        // coarse clock, up to 16 ms behind on Windows, so an edit right after the watch began would
        // look older than the watch and go unreported.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.join("f.txt"), "abcdefgh").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while f.value("/o") != Some(48.0) {
            assert!(Instant::now() < deadline, "file change not picked up");
            std::thread::sleep(Duration::from_millis(20));
            f.cook(&["/o"]);
        }
    }

    #[test]
    fn pulls_upstream_first_and_caches() {
        let mut f = sum_network();
        f.set("/a.value", Value::Float(2.0));
        f.set("/b.value", Value::Float(3.0));
        assert_eq!(f.cook(&["/out"]), ["/a", "/b", "/sum", "/out"]);
        assert_eq!(f.value("/sum"), Some(5.0));
        // Nothing changed: nothing cooks, even as time moves (no node is time-dependent).
        f.clock.advance(1);
        assert!(f.cook(&["/out"]).is_empty());
    }

    #[test]
    fn the_profile_counts_visits_and_cooks_until_reset() {
        let mut f = sum_network();
        f.cook(&["/out"]);
        f.cook(&["/out"]);
        let profile = f.cooker.take_profile(true);
        assert_eq!(profile.passes.passes, 2);
        assert_eq!(profile.nodes.len(), 4, "{profile:?}");
        let sum = profile.nodes.iter().find(|(id, _)| *id == f.id("/sum")).map(|(_, s)| *s).unwrap();
        assert_eq!((sum.visits, sum.cooks), (2, 1), "cooked once, looked at in both passes");
        assert!(sum.visit >= sum.cook);
        let after = f.cooker.take_profile(false);
        assert_eq!(after, Profile::default(), "reset");
        f.set("/b.value", Value::Float(1.0));
        f.cook(&["/out"]);
        let a = f.cooker.take_profile(false).nodes.iter().find(|(id, _)| *id == f.id("/a")).map(|(_, s)| *s);
        assert_eq!(a.map(|s| (s.visits, s.cooks)), Some((1, 0)), "clean, only looked at");
    }

    #[test]
    fn operators_learn_whether_the_clock_waits_for_every_frame() {
        let mut f = Fixture::new(&[create("/e", "test.exact")]);
        f.cook(&["/e"]);
        assert_eq!(f.value("/e"), Some(0.0), "live by default");
        f.cooker.set_exact_frames(true);
        f.clock.advance(1);
        f.cook(&["/e"]);
        assert_eq!(f.value("/e"), Some(1.0));
    }

    #[test]
    fn param_change_recooks_only_downstream() {
        let mut f = sum_network();
        f.cook(&["/out"]);
        f.set("/a.value", Value::Float(1.0));
        assert_eq!(f.cook(&["/out"]), ["/a", "/sum", "/out"]);
        assert_eq!(f.value("/sum"), Some(1.0));
    }

    #[test]
    fn unrequested_nodes_do_not_cook() {
        let mut f = sum_network();
        f.edit(&[create("/lonely", "test.const")]);
        let cooked = f.cook(&["/out"]);
        assert!(!cooked.contains(&"/lonely".to_string()));
        assert!(f.cooker.outputs(f.id("/lonely")).is_none());
    }

    #[test]
    fn time_dependent_nodes_follow_the_clock() {
        let mut f = Fixture::new(&[
            create("/t", "test.time"),
            create("/k", "test.const"),
            create("/sum", "test.add"),
            connect("/t", "/sum", 0),
            connect("/k", "/sum", 1),
        ]);
        f.cook(&["/sum"]);
        f.clock.advance(60);
        assert_eq!(f.cook(&["/sum"]), ["/t", "/sum"]);
        assert_eq!(f.value("/sum"), Some(1.0));
        // Same time again: clean.
        assert!(f.cook(&["/sum"]).is_empty());
    }

    #[test]
    fn idle_nodes_are_trimmed_once() {
        let mut f = Fixture::new(&[create("/x", "test.trims"), create("/k", "test.const")]);
        f.cook(&["/x"]);
        assert_eq!(f.value("/x"), Some(0.0));
        assert_eq!(f.status("/x").gpu_bytes, 1000, "measured after the cook");
        // Asked for but clean, then not asked for at all: both count as not cooking.
        for i in 0..TRIM_AFTER_PASSES * 3 {
            f.cook(if i % 2 == 0 { &["/x"] } else { &["/k"] });
        }
        f.set("/x.value", Value::Float(1.0));
        assert_eq!(f.status("/x").gpu_bytes, 0, "measured again after the trim");
        assert_eq!(f.cook(&["/x"]), ["/x"]);
        assert_eq!(f.value("/x"), Some(1.0), "trimmed once, not again while idle");
        // Cooking every pass keeps it from being trimmed.
        for i in 0..TRIM_AFTER_PASSES * 2 {
            f.set("/x.value", Value::Float(i as f64 + 2.0));
            f.cook(&["/x"]);
        }
        assert_eq!(f.value("/x"), Some(1.0));
    }

    #[test]
    fn undeclared_time_reads_are_flagged() {
        let mut f = Fixture::new(&[create("/s", "test.sneaky"), create("/t", "test.time")]);
        f.cook(&["/s", "/t"]);
        assert!(f.status("/s").warning.unwrap().contains("not declared time-dependent"));
        assert_eq!(f.status("/t").warning, None);
        // And, as the warning says, it does not follow the clock.
        f.clock.advance(1);
        assert_eq!(f.cook(&["/s", "/t"]), ["/t"]);
    }

    #[test]
    fn state_is_kept_and_restored() {
        let mut f = Fixture::new(&[create("/c", "test.counter"), create("/k", "test.const")]);
        assert!(f.cooker.kept().is_empty(), "nothing cooked, nothing kept");
        for _ in 0..3 {
            f.cook(&["/c", "/k"]);
            f.clock.advance(1);
        }
        assert_eq!(f.value("/c"), Some(3.0));
        let kept = f.cooker.kept();
        assert_eq!(kept.len(), 1, "only the counter has state");
        let Some((id, Kept::State(items))) = kept.first().cloned() else { panic!("{kept:?}") };
        assert_eq!(id, f.id("/c"));

        // Another cooker (a restart): the state arrives before the first cook.
        let mut g = Fixture::new(&[create("/c", "test.counter")]);
        g.cooker.cook(&g.net.clone(), g.clock.now(), &[], &[]);
        g.cooker.restore(g.id("/c"), Kept::State(items.clone())).unwrap();
        // Not cooked yet, so the next snapshot still has it.
        assert!(matches!(g.cooker.kept().as_slice(), [(_, Kept::State(_))]));
        g.cook(&["/c"]);
        assert_eq!(g.value("/c"), Some(4.0));
        assert_eq!(g.status("/c").warning, None);

        // Arriving later: taken at once, and the node cooks again even with the clock stopped.
        g.cook(&["/c"]);
        assert!(g.cook(&["/c"]).is_empty());
        g.cooker.restore(g.id("/c"), Kept::State(items)).unwrap();
        assert_eq!(g.cook(&["/c"]), ["/c"]);
        assert_eq!(g.value("/c"), Some(4.0));

        // Hostile state is an error, before or after the first cook, and never a panic.
        let bad = Kept::State(vec![StateItem::Bytes(b"lots".to_vec())]);
        assert!(g.cooker.restore(g.id("/c"), bad.clone()).unwrap_err().contains("count"));
        let mut h = Fixture::new(&[create("/c", "test.counter"), create("/k", "test.const")]);
        h.cooker.cook(&h.net.clone(), h.clock.now(), &[], &[]);
        h.cooker.restore(h.id("/c"), bad).unwrap();
        h.cook(&["/c"]);
        assert!(h.status("/c").warning.unwrap().contains("not taken"));
        assert_eq!(h.value("/c"), Some(1.0));
        // A node without state refuses it.
        let k = h.id("/k");
        h.cook(&["/k"]);
        assert!(h.cooker.restore(k, Kept::State(vec![])).unwrap_err().contains("no state"));
        assert!(h.cooker.restore(NodeId(9999), Kept::State(vec![])).is_err());
    }

    #[test]
    fn locked_outputs_are_kept_and_restored() {
        let mut f = Fixture::new(&[create("/k", "test.const")]);
        f.set("/k.value", Value::Float(5.0));
        f.cook(&["/k"]);
        f.edit(&[Op::Lock { path: pp("/k"), locked: true }]);
        f.cook(&["/k"]);
        let kept = f.cooker.kept();
        let Some((_, Kept::Outputs(outs))) = kept.first().cloned() else { panic!("{kept:?}") };

        // A restart: the locked node gets its outputs back without cooking.
        let mut g = Fixture::new(&[create("/k", "test.const")]);
        g.edit(&[Op::Lock { path: pp("/k"), locked: true }]);
        g.cooker.cook(&g.net.clone(), g.clock.now(), &[], &[]);
        g.cooker.restore(g.id("/k"), Kept::Outputs(outs.clone())).unwrap();
        assert!(g.cook(&["/k"]).is_empty());
        assert_eq!(g.value("/k"), Some(5.0));
        // Outputs that do not fit the node, or a node no longer locked, are refused.
        let wrong = Kept::Outputs(vec![Data::new(DataKind::Tex, ())]);
        assert!(g.cooker.restore(g.id("/k"), wrong).unwrap_err().contains("declared"));
        g.edit(&[Op::Lock { path: pp("/k"), locked: false }]);
        g.cooker.cook(&g.net.clone(), g.clock.now(), &[], &[]);
        assert!(g.cooker.restore(g.id("/k"), Kept::Outputs(outs)).unwrap_err().contains("not locked"));
    }

    #[test]
    fn rewiring_recooks() {
        let mut f = sum_network();
        f.cook(&["/out"]);
        f.edit(&[connect("/a", "/sum", 1)]);
        assert_eq!(f.cook(&["/out"]), ["/sum", "/out"]);
        f.edit(&[Op::Disconnect { to: pp("/sum"), input: 1 }]);
        assert_eq!(f.cook(&["/out"]), ["/sum", "/out"]);
    }

    /// What `test.stuck` makes of its input: 2 a result, 1 stuck, 0 waiting.
    #[test]
    fn a_stuck_input_is_told_from_a_waiting_one() {
        let settle = |f: &mut Fixture, want: f64| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                f.clock.advance(1);
                f.cook(&["/s"]);
                if f.value("/s") == Some(want) {
                    return;
                }
                assert!(Instant::now() < deadline, "/s stays at {:?}, wanted {want}", f.value("/s"));
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        // A job on its way waits, through a node that waits on it, and then delivers.
        let mut f = Fixture::new(&[
            create("/slow", "test.slow"),
            create("/via", "test.pick"),
            create("/s", "test.stuck"),
            connect("/slow", "/via", 0),
            connect("/via", "/s", 0),
        ]);
        f.set("/slow.ms", Value::Int(50));
        f.cook(&["/s"]);
        assert_eq!(f.value("/s"), Some(0.0));
        settle(&mut f, 2.0);

        // A job that fails turns the wait into stuck, with no new version upstream to announce it.
        let mut f = Fixture::new(&[
            create("/slow", "test.slow"),
            create("/via", "test.pick"),
            create("/s", "test.stuck"),
            connect("/slow", "/via", 0),
            connect("/via", "/s", 0),
        ]);
        f.set("/slow.ms", Value::Int(50));
        f.set("/slow.value", Value::Float(-1.0));
        f.cook(&["/s"]);
        assert_eq!(f.value("/s"), Some(0.0));
        settle(&mut f, 1.0);

        // A plain failure two nodes up is stuck at once, and fixing it delivers.
        let mut f = Fixture::new(&[
            create("/f", "test.fail"),
            create("/via", "test.pick"),
            create("/s", "test.stuck"),
            connect("/f", "/via", 0),
            connect("/via", "/s", 0),
        ]);
        f.set("/f.fail", Value::Bool(true));
        f.cook(&["/s"]);
        assert_eq!(f.value("/s"), Some(1.0));
        f.set("/f.fail", Value::Bool(false));
        settle(&mut f, 2.0);
    }

    #[test]
    fn failure_keeps_last_good_result() {
        let mut f = Fixture::new(&[
            create("/f", "test.fail"),
            create("/k", "test.const"),
            create("/sum", "test.add"),
            connect("/f", "/sum", 0),
            connect("/k", "/sum", 1),
        ]);
        f.set("/f.value", Value::Float(4.0));
        f.cook(&["/sum"]);
        assert_eq!(f.value("/sum"), Some(4.0));
        let v = f.status("/f").version;

        f.set("/f.value", Value::Float(9.0));
        f.set("/f.fail", Value::Bool(true));
        // The failing node cooks, but downstream is not disturbed.
        assert_eq!(f.cook(&["/sum"]), ["/f"]);
        assert_eq!(f.value("/f"), Some(4.0));
        assert_eq!(f.value("/sum"), Some(4.0));
        let st = f.status("/f");
        assert_eq!(st.error.as_deref(), Some("asked to fail"));
        assert_eq!(st.version, v);
        // It does not retry every frame.
        assert!(f.cook(&["/sum"]).is_empty());
        // A new resource is a reason to try again (it may have been what was missing).
        f.cooker.provide(7u32);
        assert_eq!(f.cook(&["/sum"]), ["/f"]);
        assert!(f.cook(&["/sum"]).is_empty());

        f.set("/f.fail", Value::Bool(false));
        f.cook(&["/sum"]);
        assert_eq!(f.value("/sum"), Some(9.0));
        assert_eq!(f.status("/f").error, None);
    }

    #[test]
    fn a_panicking_node_is_caught_and_recreated() {
        let mut f = Fixture::new(&[create("/p", "test.panic")]);
        f.set("/p.boom", Value::Bool(true));
        f.cook(&["/p"]);
        assert!(f.status("/p").error.unwrap().contains("node panicked: boom"));
        assert!(f.cook(&["/p"]).is_empty(), "no retry without a change");
        f.set("/p.boom", Value::Bool(false));
        f.cook(&["/p"]);
        let st = f.status("/p");
        assert_eq!(st.error, None);
        assert!(st.has_output);
    }

    #[test]
    fn a_panicking_node_waits_for_a_change_not_the_clock() {
        let mut f =
            Fixture::new(&[create("/t", "test.time"), create("/p", "test.panic"), connect("/t", "/p", 0)]);
        f.set("/p.boom", Value::Bool(true));
        assert_eq!(f.cook(&["/p"]), ["/t", "/p"]);
        assert!(f.status("/p").error.unwrap().contains("node panicked: boom"));
        // The clock moves and its input has a new result every frame: no retry.
        for _ in 0..5 {
            f.clock.advance(1);
            assert_eq!(f.cook(&["/p"]), ["/t"]);
        }
        // Rewired: tried again (and it panics again).
        f.edit(&[Op::Disconnect { to: pp("/p"), input: 0 }]);
        assert_eq!(f.cook(&["/p"]), ["/p"]);
        assert!(f.cook(&["/p"]).is_empty());
        f.edit(&[connect("/t", "/p", 0)]);
        f.clock.advance(1);
        assert_eq!(f.cook(&["/p"]), ["/t", "/p"]);
        f.clock.advance(1);
        assert_eq!(f.cook(&["/p"]), ["/t"]);
        // After a pause, a new result upstream is reason enough again.
        if let Some(rt) = f.cooker.runtimes.get_mut(&f.id("/p")) {
            rt.panicked = rt.panicked.and_then(|t| t.checked_sub(PANIC_RETRY));
        }
        f.clock.advance(1);
        assert_eq!(f.cook(&["/p"]), ["/t", "/p"]);
    }

    #[test]
    fn an_operator_that_panics_when_asked_is_dropped_and_the_pass_goes_on() {
        let mut f = Fixture::new(&[
            create("/q", "test.asks_time"),
            create("/k", "test.const"),
            create("/a", "test.asks"),
        ]);
        // Asked about the clock right after its cook: the pass goes on to /k.
        assert_eq!(f.cook(&["/q", "/k", "/a"]), ["/q", "/k", "/a"]);
        let q = f.status("/q");
        assert!(q.error.unwrap().contains("panicked in time_dependent: asked about time"));
        assert!(q.has_output, "the result of the cook stays");
        assert_eq!(f.value("/k"), Some(0.0));
        // Asked for a status, a density or a size: answered without the operator's part, and
        // the operator is dropped at the next pass.
        let a = f.status("/a");
        assert_eq!((a.error, a.job, a.params), (None, None, None));
        assert_eq!(f.cooker.density(f.id("/a"), 0), None);
        assert!(!f.cooker.sets_size(f.id("/a")));
        f.cook(&["/a"]);
        assert!(f.status("/a").error.unwrap().contains("panicked in"), "{:?}", f.status("/a").error);
        assert!(f.cook(&["/a"]).is_empty(), "not made anew every frame");
    }

    #[test]
    fn a_node_naming_itself_or_a_loop_does_not_cook_every_frame() {
        let mut f =
            Fixture::new(&[create("/r", "test.ref"), create("/x", "test.ref"), create("/y", "test.ref")]);
        f.set("/r.node", Value::Text("/r".into()));
        f.set("/x.node", Value::Text("/y".into()));
        f.set("/y.node", Value::Text("/x".into()));
        f.cook(&["/r", "/x"]);
        assert!(f.cook(&["/r", "/x"]).is_empty());
        let w = f.status("/r").warning.unwrap();
        assert!(w.contains("`node` leads back to this node"), "{w}");
        assert!(f.status("/y").warning.is_some_and(|w| w.contains("leads back")));
        // The node it names still recooks it when it changes for a reason of its own.
        f.set("/y.node", Value::Text("".into()));
        assert_eq!(f.cook(&["/x"]), ["/y", "/x"]);
    }

    #[test]
    fn wrong_output_count_is_an_error() {
        let mut f = Fixture::new(&[create("/l", "test.liar")]);
        f.cook(&["/l"]);
        assert!(f.status("/l").error.unwrap().contains("produced 2 outputs but declares 1"));
        assert!(!f.status("/l").has_output);
    }

    #[test]
    fn operators_survive_unrelated_edits() {
        let mut f = sum_network();
        f.cook(&["/out"]);
        let before = f.status("/b").stats.cooks;
        // Renaming and an edit elsewhere keep /b's runtime; it does not even recook.
        f.edit(&[Op::Rename { path: pp("/b"), name: "bee".parse().unwrap() }]);
        f.set("/a.value", Value::Float(1.0));
        assert_eq!(f.cook(&["/out"]), ["/a", "/sum", "/out"]);
        assert_eq!(f.status("/bee").stats.cooks, before);
    }

    #[test]
    fn locked_node_holds_its_output_without_its_input() {
        let mut f = Fixture::new(&[
            create("/t", "test.time"),
            create("/hold", "test.add"),
            create("/out", "test.add"),
            connect("/t", "/hold", 0),
            connect("/hold", "/out", 0),
        ]);
        f.clock.advance(60);
        f.cook(&["/out"]);
        assert_eq!(f.value("/out"), Some(1.0));
        f.edit(&[Op::Lock { path: pp("/hold"), locked: true }]);
        // Time moves and the source goes away: the lock keeps the value, downstream stays clean.
        f.clock.advance(60);
        f.edit(&[Op::Delete { path: pp("/t") }]);
        assert!(f.cook(&["/out"]).is_empty());
        assert_eq!(f.value("/out"), Some(1.0));
        // Unlocking cooks again because the input changed while locked.
        f.edit(&[Op::Lock { path: pp("/hold"), locked: false }]);
        assert_eq!(f.cook(&["/out"]), ["/hold", "/out"]);
        assert_eq!(f.value("/out"), Some(0.0));
    }

    #[test]
    fn locked_before_first_cook_cooks_once() {
        let mut f = Fixture::new(&[create("/k", "test.const")]);
        f.set("/k.value", Value::Float(2.0));
        f.edit(&[Op::Lock { path: pp("/k"), locked: true }]);
        assert_eq!(f.cook(&["/k"]), ["/k"]);
        f.set("/k.value", Value::Float(3.0));
        assert!(f.cook(&["/k"]).is_empty());
        assert_eq!(f.value("/k"), Some(2.0));
    }

    #[test]
    fn deleted_nodes_leave_no_runtime() {
        let mut f = sum_network();
        f.cook(&["/out"]);
        let a = f.id("/a");
        f.edit(&[Op::Delete { path: pp("/a") }]);
        assert_eq!(f.cook(&["/out"]), ["/sum", "/out"]);
        assert!(f.cooker.status(a).is_none());
        assert_eq!(f.value("/sum"), Some(0.0));
    }

    #[test]
    fn async_node_never_blocks_and_swaps_in_later() {
        let mut f = Fixture::new(&[
            create("/slow", "test.slow"),
            create("/k", "test.const"),
            create("/sum", "test.add"),
            connect("/slow", "/sum", 0),
            connect("/k", "/sum", 1),
        ]);
        f.set("/slow.value", Value::Float(5.0));
        f.set("/slow.ms", Value::Int(30));
        let started = Instant::now();
        f.cook(&["/sum"]);
        assert!(started.elapsed() < Duration::from_millis(25), "the cook waited for the job");
        assert_eq!(f.value("/slow"), None);
        assert_eq!(f.status("/slow").error, None);
        assert!(matches!(f.status("/slow").job, Some(JobStatus::Queued | JobStatus::Running { .. })));

        // Frames go on; once the job is done the result appears at a frame boundary.
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.value("/sum") != Some(10.0) {
            assert!(Instant::now() < deadline, "result never arrived");
            std::thread::sleep(Duration::from_millis(2));
            f.clock.advance(1);
            f.cook(&["/sum"]);
        }
        assert_eq!(f.status("/slow").job, Some(JobStatus::Done));

        // A new request keeps showing the old result until the new one is ready.
        f.set("/slow.value", Value::Float(1.0));
        f.cook(&["/sum"]);
        assert_eq!(f.value("/sum"), Some(10.0));
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.value("/sum") != Some(2.0) {
            assert!(Instant::now() < deadline, "second result never arrived");
            std::thread::sleep(Duration::from_millis(2));
            f.cook(&["/sum"]);
        }
    }

    #[test]
    fn thumbnail_of_a_node_cooked_for_output_is_free() {
        let mut f = sum_network();
        let (_, r) = f.cook_with(&["/out"], &["/a", "/sum"]);
        assert_eq!(r.thumbnails.len(), 2);
        assert_eq!(r.cooked.len(), 4);
    }

    #[test]
    fn thumbnails_cook_only_when_dirty_and_rate_limited() {
        let mut f = Fixture::new(&[create("/t", "test.time"), create("/k", "test.const")]);
        f.cooker.set_thumbnail_policy(ThumbnailPolicy { min_interval: 6, ..Default::default() });
        let (cooked, _) = f.cook_with(&[], &["/t", "/k"]);
        assert_eq!(cooked, ["/t", "/k"]);
        // Next passes: /k is clean (free check), /t is dirty but rate limited.
        for _ in 1..6 {
            f.clock.advance(1);
            let (cooked, r) = f.cook_with(&[], &["/t", "/k"]);
            assert!(cooked.is_empty(), "{cooked:?}");
            assert_eq!(r.thumbnails_deferred, 2);
        }
        f.clock.advance(1);
        assert_eq!(f.cook_with(&[], &["/t", "/k"]).0, ["/t"]);
    }

    #[test]
    fn paused_clock_still_shows_param_edits_in_thumbnails() {
        let mut f = Fixture::new(&[create("/k", "test.const")]);
        f.cook_with(&[], &["/k"]);
        f.set("/k.value", Value::Float(3.0));
        // Same frame, but the edit is a reason to cook, and the rate limit counts passes.
        for _ in 0..6 {
            f.cook_with(&[], &["/k"]);
        }
        assert_eq!(f.value("/k"), Some(3.0));
    }

    #[test]
    fn thumbnails_when_free_cook_outputs_only() {
        let mut f = sum_network();
        f.edit(&[create("/other", "test.const")]);
        f.cooker
            .set_thumbnail_policy(ThumbnailPolicy { mode: ThumbnailMode::WhenFree, ..Default::default() });
        let (cooked, r) = f.cook_with(&["/out"], &["/sum", "/other"]);
        assert!(!cooked.contains(&"/other".to_string()));
        assert_eq!(r.thumbnails, [f.id("/sum")]);
    }

    #[test]
    fn thumbnail_budget_is_round_robin() {
        let mut ops = Vec::new();
        for i in 0..4 {
            ops.push(create(&format!("/s{i}"), "test.slowcook"));
        }
        let mut f = Fixture::new(&ops);
        f.cooker.set_thumbnail_policy(ThumbnailPolicy {
            mode: ThumbnailMode::Always,
            min_interval: 0,
            budget: Duration::from_millis(1),
        });
        // At 1000 fps the frame has no headroom left, so only the budget counts.
        f.clock.set_rate(Rate::fps(1000).unwrap());
        let names = ["/s0", "/s1", "/s2", "/s3"];
        let mut seen = BTreeSet::new();
        for pass in 0..8 {
            f.edit(&names.map(|n| Op::Set {
                param: ParamPath::parse(&format!("{n}.value")).unwrap(),
                value: Value::Float(f64::from(pass) + 1.0),
            }));
            let (cooked, r) = f.cook_with(&[], &names);
            // Each test.slowcook takes 2 ms, so the 1 ms budget allows one per pass.
            assert_eq!(cooked.len(), 1, "{cooked:?}");
            assert_eq!(r.thumbnails_deferred, 3);
            seen.extend(cooked);
        }
        assert_eq!(seen.len(), 4, "every thumbnail got a turn: {seen:?}");
    }

    #[test]
    fn thumbnails_use_the_frame_headroom_beyond_the_budget() {
        let mut ops = Vec::new();
        for i in 0..4 {
            ops.push(create(&format!("/s{i}"), "test.slowcook"));
        }
        let mut f = Fixture::new(&ops);
        f.cooker.set_thumbnail_policy(ThumbnailPolicy {
            mode: ThumbnailMode::Always,
            min_interval: 0,
            budget: Duration::from_millis(1),
        });
        let names = ["/s0", "/s1", "/s2", "/s3"];
        for pass in 0..3 {
            f.edit(&names.map(|n| Op::Set {
                param: ParamPath::parse(&format!("{n}.value")).unwrap(),
                value: Value::Float(f64::from(pass) + 1.0),
            }));
            let (cooked, r) = f.cook_with(&[], &names);
            // Four 2 ms cooks fit in 75 % of a 60 fps frame, past the 1 ms budget.
            assert_eq!(cooked.len(), 4, "{cooked:?}");
            assert_eq!(r.thumbnails_deferred, 0);
        }
    }

    #[test]
    fn diamond_cooks_shared_upstream_once() {
        let mut f = Fixture::new(&[
            create("/a", "test.const"),
            create("/l", "test.add"),
            create("/r", "test.add"),
            create("/d", "test.add"),
            connect("/a", "/l", 0),
            connect("/a", "/r", 0),
            connect("/l", "/d", 0),
            connect("/r", "/d", 1),
        ]);
        f.set("/a.value", Value::Float(1.0));
        let cooked = f.cook(&["/d", "/l"]);
        assert_eq!(cooked.len(), 4);
        assert_eq!(cooked.first().map(String::as_str), Some("/a"));
        assert_eq!(f.value("/d"), Some(2.0));
    }

    #[test]
    fn long_chains_do_not_overflow_the_stack() {
        let mut ops = vec![create("/n0", "test.const")];
        for i in 1..2000 {
            ops.push(create(&format!("/n{i}"), "test.add"));
            ops.push(connect(&format!("/n{}", i - 1), &format!("/n{i}"), 0));
        }
        let mut f = Fixture::new(&ops);
        f.set("/n0.value", Value::Float(1.0));
        assert_eq!(f.cook(&["/n1999"]).len(), 2000);
        assert_eq!(f.value("/n1999"), Some(1.0));
    }

    #[test]
    fn cook_stats_accumulate() {
        let mut f = Fixture::new(&[create("/s", "test.slowcook")]);
        f.cook(&["/s"]);
        let st = f.status("/s").stats;
        assert_eq!(st.cooks, 1);
        assert!(st.last >= Duration::from_millis(2));
        assert_eq!(st.average, st.last);
        assert!(st.load() > 0.0);
    }

    #[test]
    fn load_counts_cooks_per_time_and_fades() {
        let t0 = Instant::now();
        let period = Duration::from_secs_f64(1.0 / 60.0);
        // Every frame at a quarter of the frame: about a quarter of the time.
        let mut busy = CookStats::default();
        for i in 0..600 {
            busy.add_at(period / 4, t0 + period * i);
        }
        let now = t0 + period * 599;
        assert!((busy.load_at(now) - 0.25).abs() < 0.01, "{}", busy.load_at(now));
        // One cook of a whole second and nothing after: gone a few seconds later.
        let mut once = CookStats::default();
        once.add_at(Duration::from_secs(1), t0);
        assert!(once.load_at(t0 + Duration::from_secs(5)) < 0.01);
        assert_eq!(once.average, Duration::from_secs(1));
    }

    #[test]
    fn bound_params_follow_their_channel() {
        let bind = |param: &str, from: &str, channel: Option<&str>| Op::Bind {
            param: ParamPath::parse(param).unwrap(),
            component: None,
            from: pp(from),
            channel: channel.map(|c| Name::new(c).unwrap()),
            cell: None,
            format: None,
        };
        let mut f = Fixture::new(&[create("/src", "test.const"), create("/dst", "test.const")]);
        f.set("/dst.value", Value::Float(7.0));
        f.set("/src.value", Value::Float(2.0));
        f.edit(&[bind("/dst.value", "/src", None)]);
        // The source cooks first, without being asked for.
        assert_eq!(f.cook(&["/dst"]), ["/src", "/dst"]);
        assert_eq!(f.value("/dst"), Some(2.0));
        assert_eq!(f.status("/dst").bound.get("value"), Some(&Value::Float(2.0)));
        assert_eq!(f.cooker.current(&[f.id("/dst")], f.clock.now()).len(), 1);
        // A source change reaches the bound node.
        f.set("/src.value", Value::Float(3.0));
        assert_eq!(f.cook(&["/dst"]), ["/src", "/dst"]);
        assert_eq!(f.value("/dst"), Some(3.0));
        // An edit elsewhere in the source that leaves the channel's value alone does not
        // recook the bound node (here: setting the same value again recooks nothing at all).
        f.set("/src.value", Value::Float(3.0));
        assert!(f.cook(&["/dst"]).is_empty());
        // A missing channel falls back to the stored value, with a warning.
        f.edit(&[bind("/dst.value", "/src", Some("nope"))]);
        assert_eq!(f.cook(&["/dst"]), ["/dst"]);
        assert_eq!(f.value("/dst"), Some(7.0));
        let w = f.status("/dst").warning.unwrap();
        assert!(w.contains("no channel `nope` (it has value)"), "{w}");
        // A named channel; unbinding goes back to the stored value.
        f.edit(&[bind("/dst.value", "/src", Some("value"))]);
        f.cook(&["/dst"]);
        assert_eq!(f.value("/dst"), Some(3.0));
        assert!(f.status("/dst").warning.is_none());
        f.edit(&[Op::Unbind { param: ParamPath::parse("/dst.value").unwrap(), component: None }]);
        assert_eq!(f.cook(&["/dst"]), ["/dst"]);
        assert_eq!(f.value("/dst"), Some(7.0));
    }

    #[test]
    fn a_time_dependent_source_drives_a_static_node() {
        let mut f = Fixture::new(&[create("/t", "test.time"), create("/dst", "test.const")]);
        f.edit(&[Op::Bind {
            param: ParamPath::parse("/dst.value").unwrap(),
            component: None,
            from: pp("/t"),
            channel: None,
            cell: None,
            format: None,
        }]);
        f.cook(&["/dst"]);
        f.clock.advance(30);
        assert_eq!(f.cook(&["/dst"]), ["/t", "/dst"]);
        assert_eq!(f.value("/dst"), Some(0.5));
        assert!(!f.status("/dst").time_dependent);
    }

    /// `/a` (2) and `/b` (time) into `/pick`, which skips what it does not show.
    fn pick_network(skip: bool) -> Fixture {
        let mut f = Fixture::new(&[
            create("/a", "test.const"),
            create("/b", "test.time"),
            create("/pick", "test.pick"),
            connect("/a", "/pick", 0),
            connect("/b", "/pick", 2),
        ]);
        f.set("/a.value", Value::Float(2.0));
        f.set("/pick.skip", Value::Bool(skip));
        f.clock.advance(60);
        f
    }

    #[test]
    fn a_node_that_needs_all_inputs_cooks_them_all() {
        let mut f = pick_network(false);
        assert_eq!(f.cook(&["/pick"]), ["/a", "/b", "/pick"]);
        assert_eq!(f.value("/pick"), Some(2.0));
        f.clock.advance(1);
        // The hidden input's source moves: everything recooks, as it always did.
        assert_eq!(f.cook(&["/pick"]), ["/b", "/pick"]);
    }

    #[test]
    fn skipped_inputs_do_not_cook() {
        let mut f = pick_network(true);
        assert_eq!(f.cook(&["/pick"]), ["/a", "/pick"]);
        assert_eq!(f.value("/pick"), Some(2.0));
        assert!(f.cooker.outputs(f.id("/b")).is_none(), "the hidden branch never cooked");
        f.clock.advance(1);
        assert!(f.cook(&["/pick"]).is_empty(), "nothing it shows moved");
        f.clock.advance(1);
        f.cook(&[]);
        assert!(f.cooker.current(&[f.id("/pick")], f.clock.now()).contains(&f.id("/pick")));
        // Switching cooks the other branch in the same pass.
        f.set("/pick.index", Value::Float(1.0));
        assert_eq!(f.cook(&["/pick"]), ["/b", "/pick"]);
        assert_eq!(f.value("/pick"), Some(62.0 / 60.0));
        // Past the last wired input: the last one, as without skipping.
        f.set("/pick.index", Value::Float(5.0));
        assert_eq!(f.cook(&["/pick"]), ["/pick"]);
        assert_eq!(f.value("/pick"), Some(62.0 / 60.0));
        // Back to a branch that kept its result: only the node itself cooks.
        f.set("/pick.index", Value::Float(0.0));
        assert_eq!(f.cook(&["/pick"]), ["/pick"]);
        assert_eq!(f.value("/pick"), Some(2.0));
        // Cooking everything again.
        f.clock.advance(1);
        f.set("/pick.skip", Value::Bool(false));
        assert_eq!(f.cook(&["/pick"]), ["/b", "/pick"]);
    }

    #[test]
    fn a_skipped_node_still_cooks_for_whoever_else_asks() {
        let mut f = pick_network(true);
        f.edit(&[create("/other", "test.add"), connect("/b", "/other", 0)]);
        // Wanted elsewhere: it cooks once, the picker ignores it.
        assert_eq!(f.cook(&["/pick", "/other"]), ["/a", "/pick", "/b", "/other"]);
        f.clock.advance(1);
        assert_eq!(f.cook(&["/other", "/pick"]), ["/b", "/other"]);
        // Viewed directly (a thumbnail, a snapshot): it cooks.
        f.clock.advance(1);
        let (cooked, report) = f.cook_with(&["/pick"], &["/b"]);
        assert_eq!(cooked, ["/b"]);
        assert_eq!(report.thumbnails, [f.id("/b")]);
    }

    #[test]
    fn since_last_use_counts_outputs_not_thumbnails() {
        let mut f = Fixture::new(&[
            create("/a", "test.const"),
            create("/s", "test.since"),
            create("/pick", "test.pick"),
            connect("/a", "/pick", 0),
            connect("/s", "/pick", 1),
        ]);
        f.set("/pick.skip", Value::Bool(true));
        f.set("/pick.index", Value::Float(1.0));
        f.cook(&["/pick"]);
        assert_eq!(f.value("/s"), Some(-1.0), "first use");
        f.clock.advance(1);
        f.cook(&["/pick"]);
        assert_eq!(f.value("/s"), Some(1.0 / 60.0), "in use: one frame");
        f.set("/pick.index", Value::Float(0.0));
        for _ in 0..30 {
            f.clock.advance(1);
            f.cook(&["/pick"]);
        }
        // A thumbnail cooks it meanwhile: no use, and it learns nothing.
        f.clock.advance(1);
        f.cook_with(&["/pick"], &["/s"]);
        assert_eq!(f.value("/s"), Some(-1.0));
        f.set("/pick.index", Value::Float(1.0));
        f.clock.advance(1);
        f.cook(&["/pick"]);
        assert_eq!(f.value("/s"), Some(32.0 / 60.0), "back after 32 frames");
    }

    #[test]
    fn state_in_a_skipped_branch_pauses() {
        let mut f = Fixture::new(&[
            create("/a", "test.const"),
            create("/n", "test.counter"),
            create("/pick", "test.pick"),
            connect("/a", "/pick", 0),
            connect("/n", "/pick", 1),
        ]);
        f.set("/pick.skip", Value::Bool(true));
        f.set("/pick.index", Value::Float(1.0));
        f.cook(&["/pick"]);
        f.clock.advance(1);
        f.cook(&["/pick"]);
        assert_eq!(f.value("/pick"), Some(2.0));
        f.set("/pick.index", Value::Float(0.0));
        for _ in 0..3 {
            f.clock.advance(1);
            assert!(!f.cook(&["/pick"]).contains(&"/n".to_string()));
        }
        f.set("/pick.index", Value::Float(1.0));
        f.clock.advance(1);
        f.cook(&["/pick"]);
        assert_eq!(f.value("/pick"), Some(3.0), "carries on where it stopped");
        assert_eq!(f.status("/n").stats.cooks, 3);
        assert_eq!(f.cooker.reset(f.id("/n")), ResetOutcome::Reset);
        f.clock.advance(1);
        f.cook(&["/pick"]);
        assert_eq!(f.value("/pick"), Some(1.0));
    }

    #[test]
    fn a_source_shared_by_shown_and_skipped_inputs_cooks_once() {
        let mut f = Fixture::new(&[
            create("/s", "test.time"),
            create("/a", "test.add"),
            create("/b", "test.add"),
            create("/pick", "test.pick"),
            connect("/s", "/a", 0),
            connect("/s", "/b", 0),
            connect("/a", "/pick", 0),
            connect("/b", "/pick", 1),
        ]);
        f.set("/pick.skip", Value::Bool(true));
        assert_eq!(f.cook(&["/pick"]), ["/s", "/a", "/pick"]);
        f.set("/pick.index", Value::Float(1.0));
        f.clock.advance(1);
        assert_eq!(f.cook(&["/pick"]), ["/s", "/b", "/pick"]);
    }

    #[test]
    fn a_bound_index_is_known_before_the_inputs_are_chosen() {
        let mut f = pick_network(true);
        f.edit(&[
            create("/idx", "test.const"),
            Op::Bind {
                param: ParamPath::parse("/pick.index").unwrap(),
                component: None,
                from: pp("/idx"),
                channel: None,
                cell: None,
                format: None,
            },
        ]);
        assert_eq!(f.cook(&["/pick"]), ["/idx", "/a", "/pick"]);
        assert_eq!(f.value("/pick"), Some(2.0));
        // The channel moves the index: the other branch cooks in the same pass, after it.
        f.set("/idx.value", Value::Float(1.0));
        assert_eq!(f.cook(&["/pick"]), ["/idx", "/b", "/pick"]);
        assert_eq!(f.value("/pick"), Some(1.0));
        assert!(f.cooker.current(&[f.id("/pick")], f.clock.now()).contains(&f.id("/pick")));
        f.clock.advance(1);
        f.cook(&[]);
        assert!(!f.cooker.current(&[f.id("/pick")], f.clock.now()).contains(&f.id("/pick")));
        // Back: the hidden time source no longer outdates it.
        f.set("/idx.value", Value::Float(0.0));
        f.cook(&["/pick"]);
        f.clock.advance(1);
        f.cook(&[]);
        assert!(f.cooker.current(&[f.id("/pick")], f.clock.now()).contains(&f.id("/pick")));
    }
}
