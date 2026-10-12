//! The Python thread, the watchdog, and who may run when.
//!
//! One thread runs every request; Python executes one script at a time anyway (the GIL), and a
//! thread of its own gives the watchdog a fixed target. It starts on first use: loading the
//! libraries and initialising Python cost tens of milliseconds, which a project without script
//! nodes never pays.
//!
//! - **Calls** come from the cook thread and must not wait for anything but themselves: when the
//!   thread is busy (loading a script, starting up) a call returns [`ScriptError::Busy`] at once
//!   and its waker runs when the thread is free again. A call waits at most its limit plus
//!   [`GRACE`].
//! - **Loads** come from job threads and wait until the thread is free.
//! - **The watchdog** raises `Timeout` in the Python thread once a request runs past its limit,
//!   and again every [`RETRY`] until it returns. A script stuck in C code or catching
//!   `BaseException` in a loop does not stop: the waiting caller gives up after the grace period
//!   and every script reports [`ScriptError::Stuck`] (without waiting) until it returns.
//! - A `gate` mutex holds the sequence number of the request running in Python. The watchdog
//!   interrupts only while holding it and seeing its own request, and the Python thread changes
//!   it only between requests, so a late interrupt cannot land in the next request; pyhost also
//!   clears any pending one at the start and end of every request.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ffi::Library;
use crate::locate::Install;
use crate::protocol::{self, Call, CallOutput, Load, Loaded};

/// How long a caller waits beyond a request's limit for the interrupted script to return.
pub const GRACE: Duration = Duration::from_millis(100);
/// How often the watchdog repeats its interrupt while a request runs past its limit.
pub const RETRY: Duration = Duration::from_millis(50);
/// Limit for loading a script (its top level runs: imports, setup).
pub const LOAD_LIMIT: Duration = Duration::from_secs(10);
/// How long a load waits for the thread to be free.
const LOAD_WAIT: Duration = Duration::from_secs(60);
/// The Python thread's stack: deep recursion in a script should hit Python's recursion limit,
/// not the end of the stack.
const STACK: usize = 16 << 20;

/// Why a script did not run or did not finish.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptError {
    /// No usable Python, or it could not be started. Lasts until penumbra restarts.
    Unavailable(String),
    /// The Python thread is doing something else (starting, loading a script); the waker given
    /// to [`Host::call`] runs once it is free.
    Busy,
    /// Another script ignored the watchdog and still holds the Python thread; nothing can run
    /// until it returns.
    Stuck(String),
    /// This request ran past its limit and did not stop when interrupted; it still holds the
    /// Python thread.
    Hung(String),
    /// This request ran past its limit and was stopped.
    Timeout(Duration),
    /// The script raised an exception or produced something unusable.
    Script(String),
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptError::Unavailable(e) => write!(f, "Python is not available: {e}"),
            ScriptError::Busy => write!(f, "Python is busy"),
            ScriptError::Stuck(e) | ScriptError::Hung(e) => write!(f, "{e}"),
            ScriptError::Timeout(limit) => {
                write!(f, "stopped after {} ms (the node's timeout)", limit.as_millis())
            }
            ScriptError::Script(e) => write!(f, "{e}"),
        }
    }
}

/// The host's state for `script.info`.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// The library found (`python3.dll`, `libpython3.XY.so`).
    pub library: Option<String>,
    /// How it was found.
    pub source: Option<String>,
    /// Why there is none, or why it failed to start.
    pub error: Option<String>,
    /// `Py_GetVersion()` once started.
    pub version: Option<String>,
    pub pyhost: Option<String>,
    /// `not_started`, `starting`, `idle`, `busy`, `stuck`, `failed`.
    pub state: &'static str,
    /// What runs right now, and since how many milliseconds.
    pub running: Option<String>,
    pub running_ms: Option<f64>,
    /// Script calls so far, their average and longest time as the caller saw it (microseconds).
    pub calls: u64,
    pub call_avg_us: f64,
    pub call_max_us: f64,
    /// Requests that ran past their limit (stopped by the watchdog or not), and the interrupts
    /// it raised.
    pub timeouts: u64,
    pub interrupts: u64,
}

/// Measured costs, `script.bench`.
#[derive(Clone, Debug, Serialize)]
pub struct Bench {
    pub runs: u32,
    /// Attaching to and detaching from the interpreter on the Python thread (the GIL alone).
    pub attach_ns: f64,
    /// A request that only reaches the bootstrap: thread handoff, JSON, GIL, Python dispatch.
    pub ping_us: f64,
    /// `cook(cx)` writing one channel, with one channel in.
    pub call_trivial_us: f64,
    /// `cook(cx)` doubling 200 channels of one sample each.
    pub call_200_us: f64,
}

enum Phase {
    NotStarted,
    Starting,
    Idle,
    Busy,
    Failed(String),
}

struct Running {
    label: String,
    since: Instant,
}

#[derive(Default)]
struct Stats {
    calls: u64,
    call_total_us: f64,
    call_max_us: f64,
    timeouts: u64,
}

struct State {
    phase: Phase,
    running: Option<Running>,
    /// A caller gave up waiting for the running request.
    stuck: bool,
    seq: u64,
    /// Module slots of deleted nodes, freed with the next request.
    drops: Vec<u64>,
    /// Run when the thread is free again (calls that met [`ScriptError::Busy`]).
    wakers: Vec<Box<dyn FnOnce() + Send>>,
    work: Option<Sender<Work>>,
    stats: Stats,
}

struct Work {
    seq: u64,
    request: Request,
    limit: Duration,
    /// `None` for housekeeping sent ahead of a request (freeing module slots): its end does
    /// not end the busy phase.
    reply: Option<Sender<Result<String, String>>>,
}

#[derive(Clone)]
enum Request {
    Json(String),
    BenchAttach(u32),
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when the phase changes (loads wait for idle).
    changed: Condvar,
    /// The watchdog's assignment: request and deadline.
    watch: Mutex<Option<(u64, Instant)>>,
    watch_changed: Condvar,
    /// The request running in Python right now (0: none); see the module docs.
    gate: Mutex<u64>,
    library: OnceLock<Library>,
    /// The Python thread's ident.
    thread: AtomicU64,
    interrupts: AtomicU64,
    version: OnceLock<String>,
}

/// The script host. One per process (an interpreter cannot be restarted): [`crate::host`].
pub struct Host {
    install: Result<Install, String>,
    shared: Arc<Shared>,
    next_key: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Host {
    /// A host for `install` (or the reason there is none). Starts nothing yet.
    pub fn new(install: Result<Install, String>) -> Host {
        Host {
            install,
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    phase: Phase::NotStarted,
                    running: None,
                    stuck: false,
                    seq: 0,
                    drops: Vec::new(),
                    wakers: Vec::new(),
                    work: None,
                    stats: Stats::default(),
                }),
                changed: Condvar::new(),
                watch: Mutex::new(None),
                watch_changed: Condvar::new(),
                gate: Mutex::new(0),
                library: OnceLock::new(),
                thread: AtomicU64::new(0),
                interrupts: AtomicU64::new(0),
                version: OnceLock::new(),
            }),
            next_key: AtomicU64::new(1),
        }
    }

    /// The Python found at startup, or why there is none.
    pub fn install(&self) -> Result<&Install, &str> {
        self.install.as_ref().map_err(String::as_str)
    }

    /// Whether scripts can run as far as is known: a Python was found and has not failed.
    pub fn available(&self) -> bool {
        self.install.is_ok() && !matches!(lock(&self.shared.state).phase, Phase::Failed(_))
    }

    /// A fresh module slot for one node.
    pub fn new_key(&self) -> u64 {
        self.next_key.fetch_add(1, Ordering::Relaxed)
    }

    /// Frees a node's module slot (with the next request; never waits).
    pub fn forget(&self, key: u64) {
        let mut st = lock(&self.shared.state);
        if !matches!(st.phase, Phase::NotStarted | Phase::Failed(_)) {
            st.drops.push(key);
        }
    }

    pub fn status(&self) -> Status {
        let st = lock(&self.shared.state);
        let (library, source, mut error) = match &self.install {
            Ok(i) => (Some(i.library.display().to_string()), Some(i.source.to_string()), None),
            Err(e) => (None, None, Some(e.clone())),
        };
        let state = match &st.phase {
            Phase::NotStarted => "not_started",
            Phase::Starting => "starting",
            Phase::Idle => "idle",
            Phase::Busy if st.stuck => "stuck",
            Phase::Busy => "busy",
            Phase::Failed(e) => {
                error = Some(e.clone());
                "failed"
            }
        };
        let s = &st.stats;
        Status {
            library,
            source,
            error,
            version: self.shared.version.get().cloned(),
            pyhost: self.shared.library.get().map(|l| l.host_path.display().to_string()),
            state,
            running: st.running.as_ref().map(|r| r.label.clone()),
            running_ms: st.running.as_ref().map(|r| r.since.elapsed().as_secs_f64() * 1e3),
            calls: s.calls,
            call_avg_us: if s.calls > 0 { s.call_total_us / s.calls as f64 } else { 0.0 },
            call_max_us: s.call_max_us,
            timeouts: s.timeouts,
            interrupts: self.shared.interrupts.load(Ordering::Relaxed),
        }
    }

    /// Loads (or replaces) a script. Waits for the thread to be free (giving up when `cancelled`
    /// says so), then for at most [`LOAD_LIMIT`].
    pub fn load(&self, load: &Load<'_>, cancelled: &dyn Fn() -> bool) -> Result<Loaded, ScriptError> {
        let request = protocol::load_json(load).map_err(ScriptError::Script)?;
        let deadline = Instant::now() + LOAD_WAIT;
        loop {
            if cancelled() {
                return Err(ScriptError::Script("cancelled".into()));
            }
            match self.begin(load.label, Request::Json(request.clone()), LOAD_LIMIT, None) {
                Err(ScriptError::Busy) => {}
                Err(e) => return Err(e),
                Ok(rx) => {
                    let reply = self.wait(rx, LOAD_LIMIT)?;
                    if reply.ok {
                        return reply.loaded().map_err(ScriptError::Script);
                    }
                    return Err(failure(reply, LOAD_LIMIT));
                }
            }
            if Instant::now() > deadline {
                return Err(ScriptError::Busy);
            }
            let st = lock(&self.shared.state);
            if matches!(st.phase, Phase::Busy | Phase::Starting) {
                let _unused = self.shared.changed.wait_timeout(st, Duration::from_millis(100));
            }
        }
    }

    /// Calls a loaded script's `cook(cx)`, for at most `limit`. Never waits for other work: when
    /// the thread is busy this returns [`ScriptError::Busy`] and `wake` runs once it is free.
    pub fn call(
        &self,
        label: &str,
        call: &Call<'_>,
        limit: Duration,
        wake: Box<dyn FnOnce() + Send>,
    ) -> Result<CallOutput, ScriptError> {
        let request = protocol::call_json(call).map_err(ScriptError::Script)?;
        let start = Instant::now();
        let rx = self.begin(label, Request::Json(request), limit, Some(wake))?;
        let reply = self.wait(rx, limit);
        let us = start.elapsed().as_secs_f64() * 1e6;
        {
            let mut st = lock(&self.shared.state);
            st.stats.calls += 1;
            st.stats.call_total_us += us;
            st.stats.call_max_us = st.stats.call_max_us.max(us);
        }
        let reply = reply?;
        if reply.ok {
            reply.output(call.rate).map_err(ScriptError::Script)
        } else {
            Err(failure(reply, limit))
        }
    }

    /// Measures what a script call costs (`runs` of each); see [`Bench`].
    pub fn bench(&self, runs: u32) -> Result<Bench, ScriptError> {
        let runs = runs.clamp(1, 100_000);
        let limit = Duration::from_secs(30);
        // Wait for a free thread (and start it).
        let never = || false;
        let key = self.new_key();
        let source = "def cook(cx):\n    cx.out['y'] = cx.chans[0].value() + 1.0\n";
        self.load(&Load { key, generation: 1, source, label: "bench", folder: None }, &never)?;
        let rx = self.begin_waiting(Request::BenchAttach(runs), limit)?;
        let ns = self.wait_raw(rx, limit)?;
        let attach_ns = ns.trim().parse::<f64>().unwrap_or(0.0) / f64::from(runs);

        let timed = |request: &dyn Fn() -> Result<String, String>| -> Result<f64, ScriptError> {
            let text = request().map_err(ScriptError::Script)?;
            let start = Instant::now();
            for _ in 0..runs {
                let rx = self.begin_waiting(Request::Json(text.clone()), limit)?;
                let reply = self.wait(rx, limit)?;
                if !reply.ok {
                    return Err(failure(reply, limit));
                }
            }
            Ok(start.elapsed().as_secs_f64() * 1e6 / f64::from(runs))
        };
        let ping_us = timed(&|| Ok(r#"{"op": "ping"}"#.to_string()))?;
        let one = penumbra_core::Chans::single(
            penumbra_core::Name::new("x").map_err(|e| ScriptError::Script(e.0))?,
            1.0,
            60.0,
        )
        .map_err(|e| ScriptError::Script(e.0))?;
        let params = Default::default();
        let call = |chans: &penumbra_core::Chans| {
            protocol::call_json(&Call {
                key,
                time: None,
                rate: 60.0,
                params: &params,
                chans: vec![Some(chans), None],
                tables: vec![None, None],
            })
        };
        let call_trivial_us = timed(&|| call(&one))?;
        let many = (0..200)
            .map(|i| {
                let name = penumbra_core::Name::new(&format!("c{i}"))?;
                Ok(penumbra_core::Channel { name, samples: vec![f64::from(i)] })
            })
            .collect::<Result<Vec<_>, penumbra_core::Error>>()
            .and_then(|channels| penumbra_core::Chans::new(60.0, channels))
            .map_err(|e| ScriptError::Script(e.0))?;
        let source = "def cook(cx):\n    for k, v in cx.chans[0].items():\n        cx.out[k] = v[-1] * 2\n";
        self.load(&Load { key, generation: 2, source, label: "bench", folder: None }, &never)?;
        let call_200_us = timed(&|| call(&many))?;
        self.forget(key);
        Ok(Bench { runs, attach_ns, ping_us, call_trivial_us, call_200_us })
    }

    /// [`Host::begin`] for the benchmark, which shares the thread with the show's scripts:
    /// waits while it is busy with them.
    fn begin_waiting(
        &self,
        request: Request,
        limit: Duration,
    ) -> Result<Receiver<Result<String, String>>, ScriptError> {
        let deadline = Instant::now() + LOAD_WAIT;
        loop {
            match self.begin("bench", request.clone(), limit, None) {
                Err(ScriptError::Busy) if Instant::now() < deadline => {
                    let st = lock(&self.shared.state);
                    if matches!(st.phase, Phase::Busy | Phase::Starting) {
                        let _unused = self.shared.changed.wait_timeout(st, Duration::from_millis(10));
                    }
                }
                r => return r,
            }
        }
    }

    /// Hands `request` to the Python thread if it is free (starting it on first use).
    fn begin(
        &self,
        label: &str,
        request: Request,
        limit: Duration,
        wake: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<Receiver<Result<String, String>>, ScriptError> {
        let install = self.install.as_ref().map_err(|e| ScriptError::Unavailable(e.clone()))?;
        let mut st = lock(&self.shared.state);
        if matches!(st.phase, Phase::NotStarted) {
            self.start(&mut st, install.clone());
        }
        match &st.phase {
            Phase::Failed(e) => return Err(ScriptError::Unavailable(e.clone())),
            Phase::Idle => {}
            Phase::NotStarted | Phase::Starting | Phase::Busy => {
                if let Some(w) = wake {
                    st.wakers.push(w);
                }
                if st.stuck
                    && let Some(r) = &st.running
                {
                    return Err(ScriptError::Stuck(format!(
                        "Python is stuck in `{}` for {:.0} s: it ignored the watchdog, so no script runs until it \
                         returns (or penumbra restarts)",
                        r.label,
                        r.since.elapsed().as_secs_f64()
                    )));
                }
                return Err(ScriptError::Busy);
            }
        }
        // Freed module slots go first, in a request of their own.
        let drops = std::mem::take(&mut st.drops);
        st.seq += 1;
        let seq = st.seq;
        let (tx, rx) = mpsc::channel();
        let Some(work) = st.work.clone() else {
            return Err(ScriptError::Unavailable("the Python thread is gone".into()));
        };
        if !drops.is_empty() {
            let text = serde_json::json!({"op": "ping", "drops": drops}).to_string();
            work.send(Work { seq, request: Request::Json(text), limit: LOAD_LIMIT, reply: None }).ok();
        }
        st.phase = Phase::Busy;
        st.running = Some(Running { label: label.to_string(), since: Instant::now() });
        if work.send(Work { seq, request, limit, reply: Some(tx) }).is_err() {
            st.phase = Phase::Failed("the Python thread ended".into());
            st.running = None;
            return Err(ScriptError::Unavailable("the Python thread ended".into()));
        }
        Ok(rx)
    }

    /// Waits for a reply, at most `limit` plus [`GRACE`].
    fn wait(
        &self,
        rx: Receiver<Result<String, String>>,
        limit: Duration,
    ) -> Result<protocol::Reply, ScriptError> {
        let text = self.wait_raw(rx, limit)?;
        let reply = protocol::parse(&text).map_err(ScriptError::Script)?;
        if reply.timeout {
            lock(&self.shared.state).stats.timeouts += 1;
        }
        Ok(reply)
    }

    fn wait_raw(&self, rx: Receiver<Result<String, String>>, limit: Duration) -> Result<String, ScriptError> {
        match rx.recv_timeout(limit + GRACE) {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => Err(ScriptError::Script(e)),
            Err(RecvTimeoutError::Timeout) => {
                let mut st = lock(&self.shared.state);
                st.stuck = true;
                st.stats.timeouts += 1;
                let label = st.running.as_ref().map(|r| r.label.clone()).unwrap_or_default();
                Err(ScriptError::Hung(format!(
                    "`{label}` ran past its timeout of {} ms and ignored the watchdog (C code, a sleep, or a loop \
                     catching BaseException); no script runs until it returns",
                    limit.as_millis()
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(ScriptError::Unavailable("the Python thread ended".into()))
            }
        }
    }

    /// Starts the Python thread and the watchdog.
    fn start(&self, st: &mut State, install: Install) {
        let (tx, rx) = mpsc::channel::<Work>();
        st.work = Some(tx);
        st.phase = Phase::Starting;
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("python".into())
            .stack_size(STACK)
            .spawn(move || python_thread(&shared, &install, &rx));
        if let Err(e) = spawned {
            st.phase = Phase::Failed(format!("cannot start the Python thread: {e}"));
            st.work = None;
        }
    }
}

/// The error a failed reply stands for.
fn failure(reply: protocol::Reply, limit: Duration) -> ScriptError {
    if reply.timeout { ScriptError::Timeout(limit) } else { ScriptError::Script(reply.error) }
}

fn python_thread(shared: &Arc<Shared>, install: &Install, rx: &Receiver<Work>) {
    let started = Library::load(install).and_then(|lib| {
        let info = lib.init()?;
        let info: serde_json::Value =
            serde_json::from_str(&info).map_err(|e| format!("unreadable reply from Python's start: {e}"))?;
        Ok((lib, info))
    });
    let (lib, info) = match started {
        Ok(x) => x,
        Err(e) => {
            log::warn!("script node: {e}");
            finish(shared, Phase::Failed(e));
            return;
        }
    };
    let version = lib.version.split_whitespace().next().unwrap_or("?").to_string();
    log::info!(
        "script node: Python {version} from {}, pyhost {}",
        install.library.display(),
        lib.host_path.display()
    );
    shared
        .thread
        .store(info.get("thread").and_then(serde_json::Value::as_u64).unwrap_or(0), Ordering::Relaxed);
    shared.version.set(lib.version.clone()).ok();
    shared.library.set(lib).ok();
    let Some(lib) = shared.library.get() else { return };
    let watchdog_shared = shared.clone();
    if let Err(e) =
        std::thread::Builder::new().name("python-watchdog".into()).spawn(move || watchdog(&watchdog_shared))
    {
        finish(shared, Phase::Failed(format!("cannot start the watchdog: {e}")));
        return;
    }
    finish(shared, Phase::Idle);

    for work in rx {
        *lock(&shared.gate) = work.seq;
        arm(shared, Some((work.seq, Instant::now() + work.limit)));
        let result = match &work.request {
            Request::Json(text) => lib.request(text),
            Request::BenchAttach(n) => Ok(lib.bench_attach(*n).to_string()),
        };
        arm(shared, None);
        *lock(&shared.gate) = 0;
        if let Some(reply) = work.reply {
            // Free before the caller hears back, so its next request finds the thread idle.
            finish(shared, Phase::Idle);
            reply.send(result).ok();
        }
    }
}

/// Sets the phase (and ends a running request), then runs the wakers.
fn finish(shared: &Shared, phase: Phase) {
    let wakers = {
        let mut st = lock(&shared.state);
        st.phase = phase;
        st.running = None;
        st.stuck = false;
        std::mem::take(&mut st.wakers)
    };
    shared.changed.notify_all();
    for w in wakers {
        w();
    }
}

fn arm(shared: &Shared, to: Option<(u64, Instant)>) {
    *lock(&shared.watch) = to;
    shared.watch_changed.notify_all();
}

fn watchdog(shared: &Shared) {
    let Some(lib) = shared.library.get() else { return };
    loop {
        let seq = {
            let mut w = lock(&shared.watch);
            loop {
                match *w {
                    None => {
                        w = shared.watch_changed.wait(w).unwrap_or_else(std::sync::PoisonError::into_inner)
                    }
                    Some((seq, deadline)) => {
                        let now = Instant::now();
                        if now >= deadline {
                            *w = Some((seq, now + RETRY));
                            break seq;
                        }
                        w = shared
                            .watch_changed
                            .wait_timeout(w, deadline - now)
                            .map(|(g, _)| g)
                            .unwrap_or_else(|p| p.into_inner().0);
                    }
                }
            }
        };
        let gate = lock(&shared.gate);
        if *gate == seq {
            lib.interrupt(shared.thread.load(Ordering::Relaxed));
            shared.interrupts.fetch_add(1, Ordering::Relaxed);
        }
        drop(gate);
    }
}
