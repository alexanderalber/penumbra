//! Asynchronous jobs.
//!
//! The cook never waits. A node with slow work (a file load, an ML model, a request to
//! ComfyUI) starts a [`Job`] on the [`JobPool`] and keeps delivering its last good result. When
//! the job finishes, the pool tells the cooker, which recooks the node at the next frame
//! boundary; the node then takes the result and swaps it in. [`AsyncSlot`] bundles that pattern.
//!
//! Jobs run inside `catch_unwind`, so a panic in foreign code (a decoder from a dependency)
//! fails the job, not the show. Memory exhaustion is not caught this way: untrusted files go
//! to a memory-limited worker process (later).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use penumbra_core::Error;
use serde::Serialize;

/// What a node's job is doing, reported in `node.info` and (later) as channels.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobStatus {
    Idle,
    Queued,
    /// `progress` from 0 to 1 if the job reports it.
    Running {
        progress: Option<f32>,
    },
    Done,
    Failed {
        error: String,
    },
}

type Task = Box<dyn FnOnce() + Send>;

/// A fixed set of worker threads that run jobs in order of submission.
pub struct JobPool {
    tx: Sender<Task>,
    threads: usize,
}

impl JobPool {
    /// `threads` workers (at least 1). Workers are not joined on drop: a hanging job must not
    /// hang the app on exit; they end when the pool is gone and their current job returns.
    pub fn new(threads: usize) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        let threads = threads.max(1);
        for i in 0..threads {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("pn-job-{i}"))
                .spawn(move || worker(&rx))
                .map_err(|e| Error::new(format!("cannot start job thread: {e}")))?;
        }
        Ok(Self { tx, threads })
    }

    /// One worker per spare core, at most 4.
    pub fn with_default_threads() -> Result<Self, Error> {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Self::new(cores.saturating_sub(1).clamp(1, 4))
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Runs `work` on a worker. `notify` runs after the job finished (successfully or not),
    /// on the worker thread.
    pub fn spawn<T, F>(&self, work: F, notify: Option<Box<dyn FnOnce() + Send>>) -> Job<T>
    where
        T: Send + 'static,
        F: FnOnce(&JobContext) -> Result<T, String> + Send + 'static,
    {
        let control = Arc::new(Control::default());
        let state = Arc::new(Mutex::new(State::Queued));
        let job = Job { control: control.clone(), state: state.clone() };
        let task: Task = Box::new(move || {
            if control.cancelled.load(Ordering::Relaxed) {
                return;
            }
            set(&state, State::Running);
            let cx = JobContext { control };
            let result = match catch_unwind(AssertUnwindSafe(|| work(&cx))) {
                Ok(r) => r,
                Err(payload) => Err(format!("job panicked: {}", panic_message(payload.as_ref()))),
            };
            set(&state, State::Finished(result));
            if let Some(notify) = notify {
                notify();
            }
        });
        if let Err(mpsc::SendError(_)) = self.tx.send(task) {
            set(&job.state, State::Finished(Err("job pool stopped".into())));
        }
        job
    }
}

fn worker(rx: &Mutex<Receiver<Task>>) {
    loop {
        let task = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        match task {
            Ok(task) => task(),
            Err(_) => return,
        }
    }
}

pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown payload".into()
    }
}

struct Control {
    cancelled: AtomicBool,
    /// Progress as `f32` bits; `u32::MAX` (a NaN) means not reported.
    progress: AtomicU32,
}

impl Default for Control {
    fn default() -> Self {
        Self { cancelled: AtomicBool::new(false), progress: AtomicU32::new(u32::MAX) }
    }
}

enum State<T> {
    Queued,
    Running,
    Finished(Result<T, String>),
    Taken,
}

fn set<T>(state: &Mutex<State<T>>, value: State<T>) {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

/// Handed to the running job.
pub struct JobContext {
    control: Arc<Control>,
}

impl JobContext {
    /// Whether the job is no longer wanted; long jobs should check this and return early.
    pub fn cancelled(&self) -> bool {
        self.control.cancelled.load(Ordering::Relaxed)
    }

    /// Report progress from 0 to 1.
    pub fn progress(&self, fraction: f32) {
        let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
        self.control.progress.store(f.to_bits(), Ordering::Relaxed);
    }
}

/// Handle to a submitted job. Dropping it cancels the job (a queued job never starts; a running
/// one sees [`JobContext::cancelled`]).
pub struct Job<T> {
    control: Arc<Control>,
    state: Arc<Mutex<State<T>>>,
}

impl<T> Job<T> {
    pub fn status(&self) -> JobStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            State::Queued => JobStatus::Queued,
            State::Running => {
                let p = f32::from_bits(self.control.progress.load(Ordering::Relaxed));
                JobStatus::Running { progress: p.is_finite().then_some(p) }
            }
            State::Finished(Ok(_)) | State::Taken => JobStatus::Done,
            State::Finished(Err(e)) => JobStatus::Failed { error: e.clone() },
        }
    }

    /// The result, once, when the job has finished.
    pub fn take(&self) -> Option<Result<T, String>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(*state, State::Finished(_)) {
            return None;
        }
        match std::mem::replace(&mut *state, State::Taken) {
            State::Finished(r) => Some(r),
            _ => None,
        }
    }

    pub fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::Relaxed);
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The usual shape of an asynchronous node: at most one job in flight, the last good result,
/// and the last error.
pub struct AsyncSlot<T> {
    job: Option<Job<T>>,
    latest: Option<T>,
    error: Option<String>,
}

impl<T> Default for AsyncSlot<T> {
    fn default() -> Self {
        Self { job: None, latest: None, error: None }
    }
}

impl<T> AsyncSlot<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the job in flight (the previous one is cancelled). The last good result stays.
    pub fn start(&mut self, job: Job<T>) {
        self.job = Some(job);
    }

    /// Takes a finished result. Returns true when a new good result was swapped in.
    pub fn poll(&mut self) -> bool {
        let Some(result) = self.job.as_ref().and_then(Job::take) else {
            return false;
        };
        self.job = None;
        match result {
            Ok(v) => {
                self.latest = Some(v);
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    pub fn latest(&self) -> Option<&T> {
        self.latest.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn status(&self) -> JobStatus {
        match (&self.job, &self.error, &self.latest) {
            (Some(job), _, _) => job.status(),
            (None, Some(e), _) => JobStatus::Failed { error: e.clone() },
            (None, None, Some(_)) => JobStatus::Done,
            (None, None, None) => JobStatus::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait<T>(job: &Job<T>) -> Result<T, String> {
        let start = Instant::now();
        loop {
            if let Some(r) = job.take() {
                return r;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "job did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn runs_and_notifies() {
        let pool = JobPool::new(2).unwrap();
        let (tx, rx) = mpsc::channel();
        let job = pool.spawn(|_| Ok(21 * 2), Some(Box::new(move || tx.send(()).unwrap())));
        assert_eq!(wait(&job), Ok(42));
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(job.status(), JobStatus::Done);
        assert!(job.take().is_none());
    }

    #[test]
    fn a_panic_fails_the_job_not_the_process() {
        let pool = JobPool::new(1).unwrap();
        let job = pool.spawn::<(), _>(|_| panic!("decoder exploded"), None);
        let err = wait(&job).unwrap_err();
        assert!(err.contains("decoder exploded"), "{err}");
        // The worker survived.
        assert_eq!(wait(&pool.spawn(|_| Ok(1), None)), Ok(1));
    }

    #[test]
    fn dropped_queued_job_never_runs() {
        let pool = JobPool::new(1).unwrap();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let blocker = pool.spawn(
            move |_| {
                gate_rx.recv_timeout(Duration::from_secs(10)).ok();
                Ok(())
            },
            None,
        );
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = ran.clone();
        let queued = pool.spawn(
            move |_| {
                ran2.store(true, Ordering::SeqCst);
                Ok(())
            },
            None,
        );
        assert_eq!(queued.status(), JobStatus::Queued);
        drop(queued);
        gate_tx.send(()).unwrap();
        wait(&blocker).unwrap();
        // A job after it runs, so the dropped one had its turn.
        wait(&pool.spawn(|_| Ok(()), None)).unwrap();
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn running_job_sees_cancel_and_reports_progress() {
        let pool = JobPool::new(1).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let job = pool.spawn(
            move |cx| {
                cx.progress(0.5);
                started_tx.send(()).unwrap();
                let start = Instant::now();
                while !cx.cancelled() {
                    if start.elapsed() > Duration::from_secs(10) {
                        return Err("never cancelled".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err::<(), _>("cancelled".into())
            },
            None,
        );
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(job.status(), JobStatus::Running { progress: Some(0.5) });
        job.cancel();
        assert_eq!(wait(&job), Err("cancelled".into()));
    }

    #[test]
    fn slot_keeps_last_good_result() {
        let pool = JobPool::new(1).unwrap();
        let mut slot = AsyncSlot::new();
        assert_eq!(slot.status(), JobStatus::Idle);
        slot.start(pool.spawn(|_| Ok(1), None));
        let start = Instant::now();
        while !slot.poll() {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(slot.latest(), Some(&1));
        slot.start(pool.spawn(|_| Err::<i32, _>("bad file".into()), None));
        let start = Instant::now();
        while slot.busy() {
            slot.poll();
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(slot.latest(), Some(&1));
        assert_eq!(slot.status(), JobStatus::Failed { error: "bad file".into() });
    }
}
