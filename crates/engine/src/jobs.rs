//! The long helper commands as jobs: `scan.blend`, `projector.match` and `projector.flatfield`.
//! Each run, from the control channel or from the Helpers window, takes the one job slot, says
//! what it is doing and how far it got, and leaves its reply behind when it ends. `job.status`
//! reports both; `job.cancel` stops a measurement before its next picture.
//!
//! A command answers when it is done unless asked not to wait (`wait: false`); then it runs on a
//! thread of its own and returns at once. The show keeps cooking and presenting either way.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use penumbra_core::lock;
use penumbra_project::Actor;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan_capture::CANCELLED;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// The job going on.
#[derive(Clone, Debug, Serialize)]
pub struct Running {
    pub command: String,
    /// `user` (the Helpers window) or `claude` (the control channel).
    pub by: &'static str,
    /// What it is doing now.
    pub step: String,
    /// 0 to 1, when it can tell.
    pub progress: Option<f32>,
    /// Seconds since it started.
    pub seconds: f64,
    #[serde(skip)]
    started: Instant,
}

/// How the last job of a command ended.
#[derive(Clone, Debug, Serialize)]
pub struct Finished {
    pub command: String,
    pub by: &'static str,
    pub ok: bool,
    pub cancelled: bool,
    /// The reply's message, or the error.
    pub message: String,
    pub warnings: Vec<String>,
    /// The command's whole reply; null when it failed.
    pub reply: Json,
    pub seconds: f64,
    /// When it ended, seconds since 1970.
    pub unix_time: f64,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// The last of each command, by command name.
    last: BTreeMap<String, Finished>,
}

/// The job slot of an engine.
#[derive(Default)]
pub struct Jobs {
    inner: Arc<Mutex<Inner>>,
    cancel: Arc<AtomicBool>,
}

/// What [`Jobs::status`] reports.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub running: Option<Running>,
    pub last: BTreeMap<String, Finished>,
}

fn by(actor: Actor) -> &'static str {
    match actor {
        Actor::User => "user",
        _ => "claude",
    }
}

impl Jobs {
    /// Takes the slot for `command`; fails while another job runs.
    pub fn begin(&self, command: &str, actor: Actor) -> CmdResult<Job> {
        let mut inner = lock(&self.inner);
        if let Some(r) = &inner.running {
            return Err(format!(
                "{} is running ({}); job.status follows it, job.cancel stops it",
                r.command, r.step
            )
            .into());
        }
        self.cancel.store(false, Ordering::Release);
        inner.running = Some(Running {
            command: command.to_string(),
            by: by(actor),
            step: "starting".into(),
            progress: None,
            seconds: 0.0,
            started: Instant::now(),
        });
        Ok(Job {
            inner: Arc::clone(&self.inner),
            cancel: Arc::clone(&self.cancel),
            shots: AtomicUsize::new(0),
            expected: AtomicUsize::new(0),
            ended: AtomicBool::new(false),
        })
    }

    /// The job going on and the last of each command.
    pub fn status(&self) -> Status {
        let inner = lock(&self.inner);
        let running = inner.running.clone().map(|mut r| {
            r.seconds = round(r.started.elapsed().as_secs_f64());
            r
        });
        Status { running, last: inner.last.clone() }
    }

    pub fn running(&self) -> bool {
        lock(&self.inner).running.is_some()
    }

    /// Asks the job going on to stop; false when none runs.
    pub fn cancel(&self) -> bool {
        let running = lock(&self.inner).running.is_some();
        if running {
            self.cancel.store(true, Ordering::Release);
        }
        running
    }
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// One job while it runs: reports its steps and progress, and its outcome when it ends. Dropped
/// without an outcome (a panic) it ends as failed.
pub struct Job {
    inner: Arc<Mutex<Inner>>,
    cancel: Arc<AtomicBool>,
    shots: AtomicUsize,
    expected: AtomicUsize,
    ended: AtomicBool,
}

impl Job {
    /// What it is doing now; `progress` 0 to 1 when known, else it stays as it was.
    pub fn step(&self, text: impl Into<String>, progress: Option<f32>) {
        let text = text.into();
        if let Some(r) = lock(&self.inner).running.as_mut() {
            r.step = text;
            if let Some(p) = progress {
                r.progress = Some(p.clamp(0.0, 1.0));
            }
        }
    }

    /// The camera pictures the whole job takes at most: the progress counts them.
    pub fn expect(&self, shots: usize) {
        self.expected.store(shots, Ordering::Relaxed);
    }

    /// One camera picture taken.
    pub fn shot(&self) {
        let done = self.shots.fetch_add(1, Ordering::Relaxed) + 1;
        let expected = self.expected.load(Ordering::Relaxed);
        if expected > 0
            && let Some(r) = lock(&self.inner).running.as_mut()
        {
            r.progress = Some((done as f32 / expected as f32).min(0.99));
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// An error when the job was asked to stop.
    pub fn check(&self) -> CmdResult<()> {
        if self.cancelled() { Err(CANCELLED.into()) } else { Ok(()) }
    }

    /// Ends the job with `outcome`.
    pub fn finish(&self, outcome: &CmdResult<Json>) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut inner = lock(&self.inner);
        let Some(r) = inner.running.take() else { return };
        let unix_time =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let seconds = round(r.started.elapsed().as_secs_f64());
        let finished = match outcome {
            Ok(reply) => Finished {
                command: r.command.clone(),
                by: r.by,
                ok: true,
                cancelled: false,
                message: reply.get("message").and_then(Json::as_str).unwrap_or("done").to_string(),
                warnings: reply
                    .get("warnings")
                    .and_then(Json::as_array)
                    .map(|w| w.iter().filter_map(Json::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
                reply: reply.clone(),
                seconds,
                unix_time,
            },
            Err(e) => Finished {
                command: r.command.clone(),
                by: r.by,
                ok: false,
                cancelled: e.0 == CANCELLED,
                message: e.0.clone(),
                warnings: Vec::new(),
                reply: Json::Null,
                seconds,
                unix_time,
            },
        };
        inner.last.insert(r.command, finished);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.finish(&Err("the job stopped on an internal error".into()));
    }
}

/// Runs `work` as the job `command`, edits made as `actor`: here when `wait`, else on a thread
/// of its own, answering at once.
pub fn run(
    e: &Arc<Engine>,
    command: &'static str,
    actor: Actor,
    wait: bool,
    work: impl FnOnce(&Engine, &Job) -> CmdResult<Json> + Send + 'static,
) -> CmdResult<Json> {
    if e.show_on() {
        return Err(crate::show::refusal(command).into());
    }
    let job = e.jobs().begin(command, actor)?;
    if wait {
        let outcome = catch_unwind(AssertUnwindSafe(|| work(e, &job)))
            .unwrap_or_else(|_| Err(format!("{command} stopped on an internal error").into()));
        job.finish(&outcome);
        return outcome;
    }
    let engine = Arc::clone(e);
    let spawned = std::thread::Builder::new().name(command.into()).spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| work(&engine, &job)))
            .unwrap_or_else(|_| Err(format!("{command} stopped on an internal error").into()));
        match &outcome {
            Ok(r) => log::info!("{command}: {}", r.get("message").and_then(Json::as_str).unwrap_or("done")),
            Err(err) => log::warn!("{command}: {}", err.0),
        }
        job.finish(&outcome);
    });
    match spawned {
        Ok(_) => Ok(json!({
            "started": true,
            "command": command,
            "message": format!("{command} started; job.status follows it, job.cancel stops it"),
        })),
        // The job went with the closure and ended as failed when it was dropped.
        Err(err) => Err(format!("cannot start {command}: {err}").into()),
    }
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "job.status",
        "How the helper jobs stand (scan.blend, projector.match, projector.flatfield, also when started \
         from the Helpers window): the one running (command, step, progress 0 to 1, seconds) and the \
         last of each command (ok, message, warnings, its whole reply); {}",
        move |p| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {}
            let _: A = args(p)?;
            serde_json::to_value(e.jobs().status()).map_err(|err| err.to_string().into())
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "job.cancel",
        "Stop the helper job going on (projector.match, projector.flatfield before their next picture; \
         scan.blend between its steps). Nothing is applied; {}",
        move |p| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {}
            let _: A = args(p)?;
            let cancelled = e.jobs().cancel();
            Ok(json!({
                "cancelled": cancelled,
                "message": if cancelled { "the job stops at its next step" } else { "no job is running" },
            }))
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_job_at_a_time_and_the_last_kept() {
        let jobs = Jobs::default();
        let job = jobs.begin("scan.blend", Actor::User).expect("first");
        assert!(jobs.begin("projector.match", Actor::Claude).is_err());
        job.expect(4);
        job.shot();
        job.step("blending", None);
        let s = jobs.status();
        let r = s.running.expect("running");
        assert_eq!((r.step.as_str(), r.progress, r.by), ("blending", Some(0.25), "user"));
        job.finish(&Ok(json!({"message": "done", "warnings": ["w"]})));
        drop(job);
        let s = jobs.status();
        assert!(s.running.is_none());
        let last = s.last.get("scan.blend").expect("kept");
        assert!(last.ok);
        assert_eq!(last.warnings, vec!["w".to_string()]);

        let job = jobs.begin("projector.match", Actor::Claude).expect("second");
        assert!(jobs.cancel());
        assert!(job.check().is_err());
        job.finish(&job.check().map(|()| Json::Null));
        let last = jobs.status().last.get("projector.match").cloned().expect("kept");
        assert!(last.cancelled && !last.ok);
        assert!(!jobs.cancel());

        // A job dropped without an outcome ends as failed.
        drop(jobs.begin("projector.flatfield", Actor::Claude).expect("third"));
        let last = jobs.status().last.get("projector.flatfield").cloned().expect("kept");
        assert!(!last.ok && last.message.contains("internal error"));
    }
}
