//! The engine: an open project session plus the threads that keep it alive.
//!
//! - The **cook thread** owns the [`Cooker`] and the show clock. It is paced by the vsync of
//!   one projector output ([`Engine::vsync`]) or, without one, by a timer ([`Pacer`]), takes a
//!   snapshot of the document per frame and cooks the sinks and the nodes watched over the
//!   control channel. Anything that
//!   needs the cooker (node status, the thumbnail switch) is sent to it as a task and runs
//!   between two frames ([`Engine::on_cook`]); this is the dispatch onto the engine that control
//!   channel handlers, which run on their connection's thread, need.
//! - The **project thread** calls [`Session::poll`] a few times per second: external edits of
//!   the project file and autosave. File IO therefore never runs on the cook thread.
//!
//! Edits do not go through either thread: the session and its document are safe to call from
//! any thread.
//!
//! The GPU arrives after the start ([`Engine::attach_gpu`]): the app gets its device from
//! eframe once the window is open. Until then `tex` nodes report that there is no GPU.
//!
//! **The UI's view:** the editor says what it shows ([`Engine::set_view`]: viewer nodes cooked
//! every frame, thumbnails cooked as low-priority sinks, one node whose instance params it
//! needs), and after every pass the cook thread publishes a [`ViewFrame`] with the status of
//! every node and the outputs of the requested ones ([`Engine::view`]). Textures stay on the
//! GPU; the frame holds handles to them, so a node's texture ring does not render into a
//! texture the UI still shows.
//!
//! **The outputs' view:** after every pass the cook thread also publishes an [`OutputFrame`]:
//! every `out.projector` with the texture feeding it. The output threads (crate
//! `penumbra-output`) wait for it ([`Engine::wait_output`]) and draw it; what they find out
//! about displays and windows comes back through the [`OutputBoard`], which projector nodes
//! read as a resource.
//!
//! **Mappers:** `tex.mapper` nodes find the others on their surface through a [`Mappers`]
//! resource, which the cook thread prunes after every pass; a mapper among the viewers is
//! published with its mapping, for the editor's handles.
//!
//! **Resolution analysis** (`res.*`): while the overlay is on or the UI asks for it
//! ([`ViewRequest::resolution`]), the cook thread repeats the analysis every
//! [`RES_INTERVAL`] passes and whenever the network changes, publishes it in the
//! [`ViewFrame`] and hands each mapper and render node its overlay. Meanwhile render nodes
//! measure what their camera sees of their texture ([`Renders`]); the measurements come back
//! without waiting (the cook thread polls the GPU after every pass) and are taken up before the
//! next analysis, which then runs at once.
//!
//! **Persistent state** ([`crate::state`]): after every pass the cook thread lets its
//! [`StateKeeper`] move a state snapshot along (collect handles, record GPU copies, hand mapped
//! buffers to the writer thread) and start one when due. The saved state is read back on a
//! thread of its own when the GPU arrives ([`Engine::attach_gpu`]); until then nothing is saved,
//! so a start without a GPU cannot overwrite it. [`Engine::shutdown`] saves once more.
//!
//! **OSC** ([`crate::osc`]): the cook thread tells the [`Hub`] which ports the network's
//! `chan.oscin` nodes want whenever the network changes; the hub is a resource of the cooker
//! (the nodes read what arrived); the **osc thread** turns messages into param edits and ends
//! learns.
//!
//! **Audio:** likewise, the cook thread tells the audio [`AudioHub`] which input devices the
//! network's `chan.audioin` nodes want; the hub keeps a stream thread per device, and the
//! nodes read what arrived. **Cameras** the same way: the [`CameraHub`] opens what the
//! `tex.videoin` nodes want, a capture thread per camera, and keeps each one's newest frame.
//!
//! **Another project** ([`Engine::open_project`]): the threads above stop (state saved and
//! changes written as at closing) and start again for the new session with a fresh cook state.
//! What does not belong to a project stays: the GPU, the output board and windows, the OSC
//! audio and camera hubs, the component library, the clipboard. [`Engine::generation`] tells the UI.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_audio::Hub as AudioHub;
use penumbra_camera::Hub as CameraHub;
use penumbra_core::{Error, FrameTime, LatePolicy, NodeId, PaceSource, Pacer, ParamSpec, Rate, ShowClock};
use penumbra_gpu::Gpu;
use penumbra_graph::{
    CookReport, Cooker, Data, JobPool, Network, NodeStatus, NodeTypes, PERSIST_STATE, ThumbnailMode,
};
use penumbra_ops::mapping::Mapping;
use penumbra_ops::out::{Mappings, OutputBoard, ProjectorFrame};
use penumbra_ops::res::{Report, Scene};
use penumbra_ops::tex::mapper::Mappers;
use penumbra_ops::tex::render::Renders;
use penumbra_osc::Hub;
use penumbra_project::Session;
use serde::Serialize;

use crate::osc::OscShared;
use crate::state::{RestoreReport, SaveReport, StateKeeper, StateShared};

/// How often the project thread looks at the file and autosaves.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long a command waits for the cook thread before giving up.
pub const COOK_TASK_TIMEOUT: Duration = Duration::from_secs(5);
/// Passes between two resolution analyses while they are wanted and the network stays the
/// same (a second at 60 Hz; sizes change without an edit only when a file loads). An analysis
/// costs about a millisecond per mapper with a mesh.
pub const RES_INTERVAL: u64 = 60;

type CookTask = Box<dyn FnOnce(&mut CookState) + Send>;

enum CookMsg {
    Run(CookTask),
    /// The pacing output presented a frame at this moment.
    Vsync(Instant),
    Stop,
}

/// What the UI shows, so the cook thread knows what to cook and publish ([`Engine::set_view`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewRequest {
    /// Cooked every frame, like an output (the viewer).
    pub viewers: Vec<NodeId>,
    /// Cooked as low-priority sinks (the editor's visible nodes) in [`ThumbnailMode::Always`].
    pub thumbnails: Vec<NodeId>,
    /// Visible nodes whose thumbnail is paused: never cooked for, shown only while current.
    pub passive: Vec<NodeId>,
    /// The node whose instance params are published (the selected one, for the param panel).
    pub detail: Option<NodeId>,
    /// Publish the resolution analysis ([`ViewFrame::resolution`]).
    pub resolution: bool,
}

/// What the cook thread publishes after every pass for the UI.
#[derive(Clone, Default)]
pub struct ViewFrame {
    pub pass: u64,
    pub time: Option<FrameTime>,
    pub playing: bool,
    /// What drives the show clock.
    pub pace: Option<PaceSource>,
    /// The global thumbnail switch (`cook.thumbnails`).
    pub thumbnails: ThumbnailMode,
    /// Duration of the last pass.
    pub elapsed: Duration,
    /// Status of every node the cooker knows, without instance params.
    pub status: BTreeMap<NodeId, NodeStatus>,
    /// Outputs of the viewers and of the nodes in `pictures`.
    pub outputs: BTreeMap<NodeId, Vec<Data>>,
    /// The thumbnails to draw. An outdated picture is worse than none, so a picture nobody cooks is
    /// only shown while [`Cooker::current`] says it is up to date: [`ThumbnailMode::Always`] shows
    /// every requested thumbnail (cooked, rate limited), plus current passive ones;
    /// [`ThumbnailMode::WhenFree`] only current ones (those that cooked for an output or the
    /// viewer, or that nothing changed for); [`ThumbnailMode::Never`] none.
    pub pictures: BTreeSet<NodeId>,
    /// Instance params of [`ViewRequest::detail`].
    pub detail_params: Option<(NodeId, Vec<ParamSpec>)>,
    /// The mappings of the `tex.mapper` nodes among the viewers.
    pub mappings: BTreeMap<NodeId, Arc<Mapping>>,
    /// The latest resolution analysis, while the overlay is on or the view asks for it.
    pub resolution: Option<Arc<Report>>,
    /// The resolution overlay is on (`res.overlay`).
    pub overlay: bool,
}

/// How the cook passes went since the last reset (`cook.stats`): the numbers a soak test
/// watches for stalls.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PassStats {
    pub passes: u64,
    /// Show frames that passed (more than `passes` when frames were skipped).
    pub frames: u64,
    pub cook_ms_total: f64,
    pub cook_ms_max: f64,
    /// Longest time between the starts of two passes.
    pub gap_ms_max: f64,
    /// Passes that took longer than a frame period.
    pub slow: u64,
    /// Gaps longer than 1.5 frame periods (a frame shown twice).
    pub late: u64,
    /// Gaps longer than 100 ms (a visible stall).
    pub stalls: u64,
    /// Seconds covered.
    pub seconds: f64,
    #[serde(skip)]
    since: Option<Instant>,
    #[serde(skip)]
    last_start: Option<Instant>,
}

impl PassStats {
    fn add(&mut self, start: Instant, cook: Duration, frames: u64, period: Duration) {
        let since = *self.since.get_or_insert(start);
        self.seconds = start.saturating_duration_since(since).as_secs_f64();
        self.passes += 1;
        self.frames += frames;
        let ms = cook.as_secs_f64() * 1000.0;
        self.cook_ms_total += ms;
        self.cook_ms_max = self.cook_ms_max.max(ms);
        if cook > period {
            self.slow += 1;
        }
        if let Some(last) = self.last_start {
            let gap = start.saturating_duration_since(last);
            self.gap_ms_max = self.gap_ms_max.max(gap.as_secs_f64() * 1000.0);
            if gap > period.mul_f64(1.5) {
                self.late += 1;
            }
            if gap > Duration::from_millis(100) {
                self.stalls += 1;
            }
        }
        self.last_start = Some(start);
    }

    /// The numbers so far; starts counting afresh, keeping the time of the last pass so the
    /// next gap is still measured.
    pub fn take(&mut self) -> PassStats {
        let out = self.clone();
        *self = PassStats { last_start: self.last_start, ..PassStats::default() };
        out
    }
}

/// What the cook thread publishes after every pass for the projector outputs.
#[derive(Clone, Default)]
pub struct OutputFrame {
    pub pass: u64,
    pub time: Option<FrameTime>,
    pub projectors: Vec<ProjectorFrame>,
}

/// Everything the cook thread owns. Tasks sent with [`Engine::on_cook`] get it mutably.
pub struct CookState {
    pub cooker: Cooker,
    pub clock: ShowClock,
    pacer: Pacer,
    /// Nodes cooked every frame because a client asked to watch them (`node.watch`).
    pub watched: BTreeSet<NodeId>,
    /// The last cook pass.
    pub last: CookReport,
    /// Sinks of the last snapshot looked at, recomputed when the snapshot changes.
    sinks: Option<(Arc<Network>, Vec<NodeId>)>,
    gpu: Option<Gpu>,
    view: Arc<ViewShared>,
    mappings: Mappings,
    mappers: Mappers,
    /// Render nodes' measured fields and overlays.
    pub renders: Renders,
    /// The resolution overlay is on (`res.overlay`).
    overlay: bool,
    /// The latest analysis with the network and pass it was made for.
    resolution: Option<(Arc<Network>, u64, Arc<Report>)>,
    /// State snapshots ([`crate::state`]).
    pub state: StateKeeper,
    /// Cook pass timings since the last `cook.stats` reset.
    pub stats: PassStats,
    /// Told which ports to listen on when the network changes.
    osc: Hub,
    /// Told which audio inputs to open when the network changes.
    audio: AudioHub,
    /// Told which cameras to open when the network changes.
    camera: CameraHub,
    /// Added to the cooker's pass numbers in what is published, so they keep growing when
    /// another project opens (the output threads wait for a pass newer than the last they drew).
    pass_base: u64,
}

/// Shared between the cook thread, the UI and the outputs.
#[derive(Default)]
struct ViewShared {
    request: Mutex<ViewRequest>,
    frame: Mutex<Arc<ViewFrame>>,
    output: Mutex<Arc<OutputFrame>>,
    output_ready: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CookState {
    /// What drives the show clock now.
    pub fn pace(&self) -> PaceSource {
        self.pacer.source(Instant::now())
    }

    /// What the projectors need of every node they show, after the last pass.
    pub fn analyze_resolution(&self, net: &Network, types: &NodeTypes) -> Report {
        let board = self.cooker.resource::<OutputBoard>().map(OutputBoard::state);
        let max_side = self.gpu.as_ref().map_or(penumbra_ops::tex::MAX_SIDE as u32, Gpu::max_texture_size);
        penumbra_ops::res::analyze(&Scene {
            net,
            types,
            cooker: &self.cooker,
            board: board.as_ref(),
            max_side,
        })
    }

    /// Turns the resolution overlay on the mappers and render nodes on or off.
    pub fn set_overlay(&mut self, on: bool, net: &Arc<Network>, types: &NodeTypes) {
        self.overlay = on;
        self.renders.set_active(on);
        if on {
            self.renders.collect();
            let report = Arc::new(self.analyze_resolution(net, types));
            self.show_overlays(&report);
            self.resolution = Some((net.clone(), self.last.pass, report));
        } else {
            self.mappers.set_overlays(BTreeMap::new());
            self.renders.set_overlays(BTreeMap::new());
        }
    }

    fn show_overlays(&self, report: &Report) {
        self.mappers.set_overlays(report.overlays.clone());
        self.renders.set_overlays(report.overlays.clone());
    }

    /// Repeats the analysis when it is wanted and due: every [`RES_INTERVAL`] passes, when the
    /// network changed, and when a render node's measurement arrived.
    fn refresh_resolution(&mut self, net: &Arc<Network>, types: &NodeTypes, wanted: bool) {
        self.renders.set_active(wanted || self.overlay);
        if !(wanted || self.overlay) {
            self.resolution = None;
            return;
        }
        let due = self.resolution.as_ref().is_none_or(|(n, pass, _)| {
            !Arc::ptr_eq(n, net) || self.last.pass.saturating_sub(*pass) >= RES_INTERVAL
        });
        let arrived = self.renders.collect();
        if !(due || arrived) {
            return;
        }
        let report = Arc::new(self.analyze_resolution(net, types));
        if self.overlay {
            self.show_overlays(&report);
        }
        self.resolution = Some((net.clone(), self.last.pass, report));
    }

    fn frame(&mut self, session: &Session, due: u64, now: Instant) {
        if due == 0 {
            return;
        }
        self.clock.advance(due);
        let started = Instant::now();
        let net = session.doc().snapshot();
        let fresh = !self.sinks.as_ref().is_some_and(|(n, _)| Arc::ptr_eq(n, &net));
        if fresh {
            let types = session.types();
            let sinks = net
                .nodes()
                .filter(|n| types.desc(&n.kind).is_some_and(|d| d.outputs.is_empty() && !d.container))
                .map(|n| n.id)
                .collect();
            self.sinks = Some((net.clone(), sinks));
            self.osc.set_wanted(&penumbra_ops::osc::wants(&net, types));
            self.audio.set_wanted(&penumbra_ops::audio::wants(&net, types));
            self.camera.set_wanted(&penumbra_ops::tex::videoin::wants(&net, types));
        }
        let mut wanted: Vec<NodeId> = self.sinks.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        self.watched.retain(|id| net.node(*id).is_some());
        wanted.extend(self.watched.iter().copied());
        let request = lock(&self.view.request).clone();
        wanted.extend(request.viewers.iter().copied().filter(|id| net.node(*id).is_some()));
        self.last = self.cooker.cook(&net, self.clock.now(), &wanted, &request.thumbnails);
        let output = OutputFrame {
            pass: self.pass_base + self.last.pass,
            time: Some(self.clock.now()),
            projectors: penumbra_ops::out::collect(&net, session.types(), &self.cooker, &mut self.mappings),
        };
        *lock(&self.view.output) = Arc::new(output);
        self.view.output_ready.notify_all();
        self.mappers.prune(&net);
        self.renders.prune(&net);
        self.refresh_resolution(&net, session.types(), request.resolution);
        if let Some(gpu) = &self.gpu {
            gpu.poll();
        }
        self.state.after_pass(&self.cooker, &net, self.gpu.as_ref(), self.clock.now().frame);
        self.publish(&net, &request, now);
        self.stats.add(started, self.last.elapsed, due, self.clock.now().rate.period());
    }

    fn publish(&self, net: &Network, request: &ViewRequest, wall: Instant) {
        let mut frame = ViewFrame {
            pass: self.pass_base + self.last.pass,
            time: Some(self.clock.now()),
            playing: self.clock.is_playing(),
            pace: Some(self.pacer.source(wall)),
            thumbnails: self.cooker.thumbnail_policy().mode,
            elapsed: self.last.elapsed,
            resolution: self.resolution.as_ref().map(|(_, _, r)| r.clone()),
            overlay: self.overlay,
            ..ViewFrame::default()
        };
        for node in net.nodes() {
            if let Some(status) = self.cooker.status_brief(node.id) {
                frame.status.insert(node.id, status);
            }
        }
        let now = self.clock.now();
        frame.pictures = match frame.thumbnails {
            ThumbnailMode::Always => {
                // Cooked for (rate limited, so at most a few frames old).
                let mut p: BTreeSet<NodeId> = request.thumbnails.iter().copied().collect();
                p.extend(self.cooker.current(&request.passive, now));
                p
            }
            ThumbnailMode::WhenFree => {
                let all: Vec<NodeId> = request.thumbnails.iter().chain(&request.passive).copied().collect();
                self.cooker.current(&all, now)
            }
            ThumbnailMode::Never => BTreeSet::new(),
        };
        for &id in request.viewers.iter().chain(&frame.pictures) {
            if let Some(outputs) = self.cooker.outputs(id) {
                frame.outputs.insert(id, outputs.to_vec());
            }
        }
        for &id in &request.viewers {
            if let Some(m) = self.mappers.mapping(id) {
                frame.mappings.insert(id, m);
            }
        }
        frame.pictures.retain(|id| frame.outputs.contains_key(id));
        frame.detail_params = request.detail.and_then(|id| Some((id, self.cooker.status(id)?.params?)));
        *lock(&self.view.frame) = Arc::new(frame);
    }
}

pub struct Engine {
    /// The open project; another one replaces it in [`Engine::open_project`].
    session: Mutex<Arc<Session>>,
    /// Counts the projects opened in this run, so the UI notices a switch.
    generation: AtomicU64,
    /// The node types; the same for every project.
    types: Arc<NodeTypes>,
    pool: Arc<JobPool>,
    gpu: OnceLock<Gpu>,
    board: OutputBoard,
    view: Arc<ViewShared>,
    cook: Mutex<Sender<CookMsg>>,
    stop_project: Mutex<Option<Sender<()>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Held while a project closes and another opens, and by the shutdown.
    switching: Mutex<()>,
    /// The user's component library (`comp.*`); `None` until set or asked for.
    library: Mutex<Option<std::path::PathBuf>>,
    /// Where the recently opened projects are listed; `None` lists nothing (tests, headless).
    recent: Mutex<Option<std::path::PathBuf>>,
    /// The nodes copied last (`node.copy`, Ctrl+C).
    clipboard: Mutex<Option<crate::clipboard::Clipboard>>,
    state: Arc<StateShared>,
    osc: Arc<OscShared>,
    audio: AudioHub,
    camera: CameraHub,
    shut: AtomicBool,
}

impl Engine {
    /// Starts the cook and project threads for `session`.
    pub fn start(session: Arc<Session>) -> Result<Arc<Engine>, Error> {
        // Look for Python now, so the log says at startup whether script nodes can run; Python
        // itself starts with the first script.
        penumbra_script::host();
        let (cook, _) = mpsc::channel();
        let engine = Arc::new(Engine {
            session: Mutex::new(session.clone()),
            generation: AtomicU64::new(0),
            types: session.types().clone(),
            pool: Arc::new(JobPool::with_default_threads()?),
            gpu: OnceLock::new(),
            board: OutputBoard::new(),
            view: Arc::new(ViewShared::default()),
            cook: Mutex::new(cook),
            stop_project: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            switching: Mutex::new(()),
            library: Mutex::new(None),
            recent: Mutex::new(None),
            clipboard: Mutex::new(None),
            state: Arc::new(StateShared::default()),
            osc: Arc::new(OscShared::new(Hub::new())),
            audio: AudioHub::new(),
            camera: CameraHub::new(),
            shut: AtomicBool::new(false),
        });
        engine.launch(&session, 0)?;
        Ok(engine)
    }

    /// A fresh cook state for `session`, with the resources that outlive a project.
    fn cook_state(&self, session: &Session, pass_base: u64) -> CookState {
        let rate = Rate::FPS_60;
        let mut cooker = Cooker::new(session.types().clone(), self.pool.clone());
        cooker.set_base_dir(Some(session.dir().to_path_buf()));
        cooker.provide(self.board.clone());
        let mappers = Mappers::new();
        cooker.provide(mappers.clone());
        let renders = Renders::new();
        cooker.provide(renders.clone());
        cooker.provide(self.osc.hub().clone());
        cooker.provide(self.audio.clone());
        cooker.provide(self.camera.clone());
        let gpu = self.gpu.get().cloned();
        if let Some(gpu) = &gpu {
            cooker.provide(gpu.clone());
        }
        CookState {
            cooker,
            clock: ShowClock::new(rate, LatePolicy::Realtime),
            pacer: Pacer::new(rate, Instant::now()),
            watched: BTreeSet::new(),
            last: CookReport::default(),
            sinks: None,
            gpu,
            view: self.view.clone(),
            mappings: Mappings::default(),
            mappers,
            renders,
            overlay: false,
            resolution: None,
            state: StateKeeper::new(self.state.clone(), session.dir()),
            stats: PassStats::default(),
            osc: self.osc.hub().clone(),
            audio: self.audio.clone(),
            camera: self.camera.clone(),
            pass_base,
        }
    }

    /// Starts the cook, project and osc threads for `session`.
    fn launch(&self, session: &Arc<Session>, pass_base: u64) -> Result<(), Error> {
        let state = self.cook_state(session, pass_base);
        let spawn_error = |e: std::io::Error| Error::new(format!("cannot start a thread: {e}"));

        let (cook_tx, cook_rx) = mpsc::channel();
        let cook_session = session.clone();
        let cook = std::thread::Builder::new()
            .name("cook".into())
            .spawn(move || cook_loop(&cook_session, state, &cook_rx))
            .map_err(spawn_error)?;
        *lock(&self.cook) = cook_tx;
        lock(&self.threads).push(cook);

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let project_session = session.clone();
        let project = std::thread::Builder::new()
            .name("project".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(POLL_INTERVAL) {
                    project_session.poll(Instant::now());
                }
            })
            .map_err(spawn_error)?;
        *lock(&self.stop_project) = Some(stop_tx);
        lock(&self.threads).push(project);

        self.osc.resume();
        let (osc_session, osc_shared) = (session.clone(), self.osc.clone());
        let osc_thread = std::thread::Builder::new()
            .name("osc".into())
            .spawn(move || crate::osc::run(&osc_session, &osc_shared))
            .map_err(spawn_error)?;
        lock(&self.threads).push(osc_thread);
        Ok(())
    }

    /// Saves the state if due, stops the project's threads and writes its unsaved changes.
    fn stop_threads(&self) {
        let session = self.session();
        let snapshot = session.clone();
        let due =
            self.on_cook(move |st| st.state.due(&st.cooker, &snapshot.doc().snapshot())).unwrap_or(false);
        if due
            && !self.state.holding()
            && let Err(e) = self.save_state(Duration::from_secs(10))
        {
            log::warn!("state at closing: {e}");
        }
        lock(&self.cook).send(CookMsg::Stop).ok();
        self.osc.stop();
        if let Some(stop) = lock(&self.stop_project).take() {
            drop(stop);
        }
        let threads = std::mem::take(&mut *lock(&self.threads));
        for t in threads {
            if t.join().is_err() {
                log::error!("a worker thread panicked");
            }
        }
        session.flush(Instant::now());
    }

    /// Closes the open project and opens `session` instead; the GPU, the outputs and the hubs
    /// stay. The old project's state is saved and its unsaved changes written first, as at
    /// closing. That can take seconds, so the UI calls this from a thread of its own.
    pub fn open_project(self: &Arc<Self>, session: Arc<Session>) -> Result<(), Error> {
        let _switch = lock(&self.switching);
        if self.shut.load(Ordering::Acquire) {
            return Err(Error::new("the app is closing"));
        }
        let pass_base = self.view().pass.max(self.output().pass);
        self.stop_threads();
        *lock(&self.view.request) = ViewRequest::default();
        *lock(&self.view.frame) = Arc::new(ViewFrame { pass: pass_base, ..ViewFrame::default() });
        *lock(&self.view.output) = Arc::new(OutputFrame { pass: pass_base, ..OutputFrame::default() });
        self.state.update(|s| {
            s.restore = None;
            s.last_error = None;
        });
        *lock(&self.session) = session.clone();
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.launch(&session, pass_base)?;
        if self.gpu().is_some() {
            self.restore_in_background();
        }
        self.remember(session.dir());
        Ok(())
    }

    /// The open project.
    pub fn session(&self) -> Arc<Session> {
        lock(&self.session).clone()
    }

    /// The node types (every project's session shares them).
    pub fn types(&self) -> &Arc<NodeTypes> {
        &self.types
    }

    /// Grows by one whenever another project opens ([`Engine::open_project`]).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Where recently opened projects are listed (`None`: nowhere). The app sets it; tests and
    /// headless runs leave it unset, so they never touch the user's list.
    pub fn set_recent_file(&self, file: Option<std::path::PathBuf>) {
        *lock(&self.recent) = file;
    }

    /// The recently opened projects, newest first, only those that still have a project file.
    pub fn recent(&self) -> Vec<std::path::PathBuf> {
        let file = lock(&self.recent).clone();
        file.map(|f| crate::projects::read_recent(&f)).unwrap_or_default()
    }

    /// Puts `dir` on top of the recent projects.
    pub fn remember(&self, dir: &std::path::Path) {
        let file = lock(&self.recent).clone();
        if let Some(file) = file
            && let Err(e) = crate::projects::remember(&file, dir)
        {
            log::warn!("recent projects: {e}");
        }
    }

    /// The directory of the user's component library, shared by all projects: set by
    /// [`Engine::set_library`], else `PENUMBRA_LIBRARY`, else `components` beside the demo
    /// project (`%LOCALAPPDATA%\penumbra\components`, `$XDG_DATA_HOME/penumbra/components`).
    pub fn library(&self) -> Option<std::path::PathBuf> {
        let set = self.library.lock().unwrap_or_else(PoisonError::into_inner).clone();
        set.or_else(crate::components::default_library)
    }

    /// The nodes copied last, with what their locked nodes held.
    pub(crate) fn clipboard(&self) -> &Mutex<Option<crate::clipboard::Clipboard>> {
        &self.clipboard
    }

    /// Points the component library somewhere else (tests, a project that brings its own).
    pub fn set_library(&self, dir: Option<std::path::PathBuf>) {
        *self.library.lock().unwrap_or_else(PoisonError::into_inner) = dir;
    }

    /// Hands the GPU to the cook thread. Only the first call counts. Then reads the saved state
    /// back on a thread of its own ([`crate::state`]), unless `state.config` says not to.
    pub fn attach_gpu(self: &Arc<Self>, gpu: Gpu) -> Result<(), Error> {
        if self.gpu.set(gpu.clone()).is_err() {
            return Err(Error::new("the engine has a GPU already"));
        }
        self.on_cook(move |st| {
            st.cooker.provide(gpu.clone());
            st.gpu = Some(gpu);
        })?;
        self.restore_in_background();
        Ok(())
    }

    /// Reads the open project's saved state back on a thread of its own, unless `state.config`
    /// says not to; until it is done, nothing is saved ([`StateStatus::holding`]).
    ///
    /// [`StateStatus::holding`]: crate::state::StateStatus::holding
    fn restore_in_background(self: &Arc<Self>) {
        if !self.state.config().restore_on_start {
            return;
        }
        let engine = Arc::downgrade(self);
        let generation = self.generation();
        let spawned = std::thread::Builder::new().name("state-restore".into()).spawn(move || {
            let Some(engine) = engine.upgrade() else { return };
            // No project switch while the state is read back, so it cannot land in the next
            // project or end the next project's holding early.
            let _switch = lock(&engine.switching);
            if engine.generation() != generation || engine.shut.load(Ordering::Acquire) {
                return;
            }
            let fresh = engine.state.take_fresh();
            match engine.restore(None, fresh) {
                Ok(r) if !r.restored.is_empty() => {
                    log::info!(
                        "state: restored {} nodes from {}",
                        r.restored.len(),
                        r.file.as_deref().unwrap_or("?")
                    )
                }
                Ok(r) => log::info!("state: {}", r.note.as_deref().unwrap_or("nothing restored")),
                Err(e) => log::warn!("state: {e}"),
            }
            engine.state.update(|s| s.holding = false);
            if fresh {
                match engine.discard_state() {
                    Ok(note) => log::info!("state: {note}"),
                    Err(e) => log::warn!("state: fresh start: {e}"),
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("cannot start the state restore thread: {e}");
            self.state.update(|s| s.holding = false);
        }
    }

    /// State snapshots: configuration and status.
    pub fn state(&self) -> &Arc<StateShared> {
        &self.state
    }

    /// OSC: the hub, params set by address, learn.
    pub fn osc(&self) -> &Arc<OscShared> {
        &self.osc
    }

    /// The audio inputs.
    pub fn audio(&self) -> &AudioHub {
        &self.audio
    }

    /// The cameras.
    pub fn camera(&self) -> &CameraHub {
        &self.camera
    }

    /// The project's state directory.
    pub fn state_dir(&self) -> std::path::PathBuf {
        crate::state::dir_of(self.session().dir())
    }

    /// Takes a state snapshot now and waits up to `timeout` for the file. The cook thread only
    /// starts it; the waiting happens here.
    pub fn save_state(&self, timeout: Duration) -> Result<SaveReport, Error> {
        let seq = self.on_cook(|st| st.state.request())??;
        self.state.wait_for(seq, timeout)
    }

    /// Reads the saved state (`file`, else the project's latest, else its previous) and gives
    /// it back to the nodes. Reading and uploading run on this thread; the cook thread only
    /// hands the results over between two frames.
    pub fn restore_state(&self, file: Option<&std::path::Path>) -> Result<RestoreReport, Error> {
        self.restore(file, false)
    }

    /// [`Engine::restore_state`]; operators' state is taken only with the project setting
    /// [`PERSIST_STATE`] on and not on a `fresh` start. Locked outputs are taken always.
    fn restore(&self, file: Option<&std::path::Path>, fresh: bool) -> Result<RestoreReport, Error> {
        let started = Instant::now();
        let limits = self.state.config().limits;
        let mut report = RestoreReport::default();
        if let Some(saved) = crate::state::load(&self.state_dir(), file, &limits, &mut report) {
            let net = self.session().doc().snapshot();
            let take_state = !fresh && net.switch(PERSIST_STATE);
            let prepared = crate::state::prepare(saved, &net, self.gpu(), take_state, &mut report);
            let results = self.on_cook(move |st| {
                // Bring the cooker up to this network first, so new nodes can take state.
                st.cooker.cook(&net, st.clock.now(), &[], &[]);
                prepared
                    .into_iter()
                    .map(|(id, path, kept)| (path, st.cooker.restore(id, kept)))
                    .collect::<Vec<_>>()
            })?;
            for (path, r) in results {
                match r {
                    Ok(()) => report.restored.push(path),
                    Err(e) => report.skipped.push((path, e)),
                }
            }
        }
        report.ms = started.elapsed().as_secs_f64() * 1000.0;
        self.state.update(|s| s.restore = Some(report.clone()));
        Ok(report)
    }

    /// Discards what operators hold beyond their params, now and on disk (the Helpers button,
    /// `state.discard`, `--fresh`): every operator with state starts over, `latest.pnstate`
    /// becomes `discarded.pnstate` (one kept, in case it was a mistake), `previous.pnstate` is
    /// deleted, and a snapshot of what is left (the locked nodes' outputs) is written. Returns
    /// what happened, for a message.
    pub fn discard_state(&self) -> Result<String, Error> {
        if self.state.holding() {
            // The start restore has not run yet: let it leave the state out instead.
            self.state.start_fresh();
            return Ok("the saved state will not be read back".into());
        }
        let nodes = self.on_cook(|st| {
            st.state.invalidate();
            st.cooker.forget_state()
        })?;
        let dir = self.state_dir();
        let latest = dir.join(crate::state::LATEST);
        if latest.exists() {
            std::fs::rename(&latest, dir.join(crate::state::DISCARDED))
                .map_err(|e| Error::new(format!("cannot move {} aside: {e}", latest.display())))?;
        }
        let previous = dir.join(crate::state::PREVIOUS);
        if previous.exists() {
            std::fs::remove_file(&previous)
                .map_err(|e| Error::new(format!("cannot delete {}: {e}", previous.display())))?;
        }
        self.save_state(Duration::from_secs(20))?;
        Ok(format!("started fresh: {nodes} nodes forgot what they held; locked nodes keep their outputs"))
    }

    pub fn gpu(&self) -> Option<&Gpu> {
        self.gpu.get()
    }

    /// Tells the cook thread what the UI shows; used from the next pass on.
    pub fn set_view(&self, request: ViewRequest) {
        let mut current = lock(&self.view.request);
        if *current != request {
            *current = request;
        }
    }

    /// The view request in effect.
    pub fn view_request(&self) -> ViewRequest {
        lock(&self.view.request).clone()
    }

    /// The latest published frame (empty before the first pass).
    pub fn view(&self) -> Arc<ViewFrame> {
        lock(&self.view.frame).clone()
    }

    /// What the output side reports (displays, windows, projectors) and reads.
    pub fn board(&self) -> &OutputBoard {
        &self.board
    }

    /// The latest output frame (empty before the first pass).
    pub fn output(&self) -> Arc<OutputFrame> {
        lock(&self.view.output).clone()
    }

    /// Waits until a pass newer than `after` is published, at most `timeout`; returns the
    /// latest frame either way.
    pub fn wait_output(&self, after: u64, timeout: Duration) -> Arc<OutputFrame> {
        let guard = lock(&self.view.output);
        let (guard, _) = self
            .view
            .output_ready
            .wait_timeout_while(guard, timeout, |f| f.pass <= after)
            .unwrap_or_else(PoisonError::into_inner);
        guard.clone()
    }

    /// The pacing output presented a frame at `at` (called from its thread): the cook thread
    /// cooks the next frame now instead of waiting for its timer.
    pub fn vsync(&self, at: Instant) {
        lock(&self.cook).send(CookMsg::Vsync(at)).ok();
    }

    /// Runs `task` on the cook thread between two frames and returns its result.
    pub fn on_cook<R, F>(&self, task: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&mut CookState) -> R + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let run: CookTask = Box::new(move |state| {
            // The caller may have given up waiting; then nobody wants the result.
            tx.send(task(state)).ok();
        });
        lock(&self.cook).send(CookMsg::Run(run)).map_err(|_| Error::new("the cook thread has stopped"))?;
        rx.recv_timeout(COOK_TASK_TIMEOUT).map_err(|e| match e {
            RecvTimeoutError::Timeout => Error::new("the cook thread did not answer in time"),
            RecvTimeoutError::Disconnected => Error::new("the cook thread dropped the request"),
        })
    }

    /// Runs `task` on the cook thread between two frames without waiting for it, for the UI
    /// (play, pause, the thumbnail switch), which must not stall on the cook thread.
    pub fn post_cook<F>(&self, task: F)
    where
        F: FnOnce(&mut CookState) + Send + 'static,
    {
        if lock(&self.cook).send(CookMsg::Run(Box::new(task))).is_err() {
            log::warn!("the cook thread has stopped");
        }
    }

    /// Stops both threads and writes unsaved changes (unless the file holds an edit that has not
    /// merged, see [`Session::flush`]). Called when the app closes.
    pub fn shutdown(&self) {
        if self.shut.swap(true, Ordering::AcqRel) {
            return;
        }
        let _switch = lock(&self.switching);
        self.stop_threads();
        self.osc.hub().shutdown();
        self.audio.shutdown();
        self.camera.shutdown();
    }
}

fn cook_loop(session: &Session, mut state: CookState, rx: &Receiver<CookMsg>) {
    loop {
        let now = Instant::now();
        let deadline = state.pacer.next_deadline();
        if now >= deadline {
            let due = state.pacer.due(now);
            run_frame(session, &mut state, due, now);
            // Tasks run between frames even when every frame is late.
            while let Ok(msg) = rx.try_recv() {
                if !handle(msg, session, &mut state) {
                    return;
                }
            }
            continue;
        }
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(msg) => {
                if !handle(msg, session, &mut state) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn run_frame(session: &Session, state: &mut CookState, due: u64, now: Instant) {
    // The cooker catches operator panics itself; this guards the loop's own code.
    if catch_unwind(AssertUnwindSafe(|| state.frame(session, due, now))).is_err() {
        log::error!("a cook pass panicked; continuing with the next frame");
    }
}

/// Runs one message; `false` means stop.
fn handle(msg: CookMsg, session: &Session, state: &mut CookState) -> bool {
    match msg {
        CookMsg::Run(task) => {
            if catch_unwind(AssertUnwindSafe(|| task(state))).is_err() {
                log::error!("a cook-thread task panicked");
            }
            true
        }
        CookMsg::Vsync(at) => {
            let due = state.pacer.vsync(at);
            run_frame(session, state, due, Instant::now());
            true
        }
        CookMsg::Stop => false,
    }
}
