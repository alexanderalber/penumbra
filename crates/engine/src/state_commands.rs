//! `state.*` (persistent state, [`crate::state`]), `cook.stats` (pass timings over a stretch of
//! time) and `system.process` (memory, handles, threads, GPU memory), the last two for the soak
//! test and for looking at a long-running show; `cook.meter` and `system.load` (the last seconds'
//! pass times, CPU and GPU load), what the status bar's cook time shows.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::state::{BROKEN, DISCARDED, LATEST, PREVIOUS};

type Result<T> = std::result::Result<T, CommandError>;

/// How long `state.save` waits for the file by default.
const SAVE_WAIT: Duration = Duration::from_secs(20);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> Result<Json>| {
        let engine = engine.clone();
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    };
    add(
        "state.info",
        "Persistent state: settings, the files in the project's state directory, the last save and restore, what a snapshot would keep now",
        state_info,
    );
    add("state.save", "Take a state snapshot now and wait for the file; {timeout?} in seconds", state_save);
    add(
        "state.restore",
        "Give the nodes their saved state back (latest, else previous, or {file}); replaces what they hold",
        state_restore,
    );
    add(
        "state.discard",
        "Discard what feedback loops, caches and other nodes hold, now and in the state files (locked nodes keep theirs)",
        state_discard,
    );
    add(
        "state.config",
        "Snapshot timing (not stored, every start has the defaults); {interval?, restore_on_start?}. Whether node state is kept at all is the project setting persist_state",
        state_config,
    );
    add(
        "cook.stats",
        "Cook pass timings since the last reset: passes, slowest pass, longest gap, late frames, stalls; {reset?}",
        cook_stats,
    );
    add(
        "cook.meter",
        "Pass times of the last 8 s: last, smoothed (1 s lag), median, 1 % high, max, cook thread busy; {samples?: [seconds ago, ms] per pass}",
        cook_meter,
    );
    add(
        "system.load",
        "CPU over all cores and the busiest GPU engine in percent, sampled once a second while asked for (the first call waits for a sample)",
        system_load,
    );
    add(
        "system.process",
        "This process: working set, private bytes, handles, threads; GPU memory through wgpu's allocator",
        system_process,
    );
}

fn file_info(path: &std::path::Path) -> Json {
    match std::fs::metadata(path) {
        Ok(m) => {
            let age = m.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok());
            json!({"file": path.display().to_string(), "bytes": m.len(), "age_s": age.map(|a| a.as_secs_f64())})
        }
        Err(_) => Json::Null,
    }
}

fn state_info(e: &Engine, p: &Json) -> Result<Json> {
    args::<Empty>(p)?;
    let dir = e.state_dir();
    let session = e.session().clone();
    let now = e.on_cook(move |st| {
        let net = session.doc().snapshot();
        let summary = crate::state::summary(&st.cooker, &net);
        (summary, st.state.busy())
    })?;
    let (kept, busy) = now;
    let kept: Vec<Json> = kept
        .into_iter()
        .map(|(path, kind, what, items, gpu_bytes)| {
            json!({"path": path, "kind": kind, "what": what, "items": items, "texture_bytes": gpu_bytes})
        })
        .collect();
    let status = e.state().status();
    let age = status.last_save.as_ref().and_then(|r| {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(r.unix_time.max(0.0));
        SystemTime::now().duration_since(t).ok().map(|d| d.as_secs_f64())
    });
    let persist = e.session().doc().snapshot().switch(penumbra_graph::PERSIST_STATE);
    Ok(json!({
        "persist": persist,
        "config": e.state().config(),
        "dir": dir.display().to_string(),
        "files": {
            "latest": file_info(&dir.join(LATEST)),
            "previous": file_info(&dir.join(PREVIOUS)),
            "broken": file_info(&dir.join(BROKEN)),
            "discarded": file_info(&dir.join(DISCARDED)),
        },
        "status": status,
        "last_save_age_s": age,
        "saving": busy,
        "kept_now": kept,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn state_save(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        timeout: Option<f64>,
    }
    let a: A = args(p)?;
    let wait = a
        .timeout
        .filter(|t| t.is_finite() && *t > 0.0)
        .map_or(SAVE_WAIT, |t| Duration::from_secs_f64(t.min(600.0)));
    let report = e.save_state(wait)?;
    Ok(serde_json::to_value(report).map_err(|e| e.to_string())?)
}

fn state_restore(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        file: Option<PathBuf>,
    }
    let a: A = args(p)?;
    if e.gpu().is_none() {
        return Err("no GPU yet: textures in the state could not be uploaded".into());
    }
    let report = e.restore_state(a.file.as_deref())?;
    if let Some((_, why)) = report.broken.first()
        && report.file.is_none()
    {
        // The reason names the file.
        return Err(why.clone().into());
    }
    Ok(serde_json::to_value(report).map_err(|e| e.to_string())?)
}

fn state_discard(e: &Engine, p: &Json) -> Result<Json> {
    args::<Empty>(p)?;
    Ok(json!({"done": e.discard_state()?}))
}

fn state_config(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        /// Seconds between snapshots.
        interval: Option<f64>,
        restore_on_start: Option<bool>,
    }
    let a: A = args(p)?;
    let mut config = e.state().config();
    if let Some(s) = a.interval {
        if !(s.is_finite() && (1.0..=86_400.0).contains(&s)) {
            return Err("interval is in seconds, 1 to 86400".into());
        }
        config.interval = Duration::from_secs_f64(s);
    }
    if let Some(on) = a.restore_on_start {
        config.restore_on_start = on;
    }
    e.state().set_config(config.clone());
    if a.restore_on_start == Some(false) {
        // Nothing will be read back, so saving may start.
        e.state().update(|s| s.holding = false);
    }
    Ok(serde_json::to_value(config).map_err(|e| e.to_string())?)
}

fn cook_stats(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        reset: Option<bool>,
    }
    let a: A = args(p)?;
    let reset = a.reset.unwrap_or(false);
    let (stats, nodes, pass) = e.on_cook(move |st| {
        let stats = if reset { st.stats.take() } else { st.stats.clone() };
        (stats, st.last.cooked.len(), st.last.pass)
    })?;
    let mut out = serde_json::to_value(&stats).map_err(|e| e.to_string())?;
    if let Some(o) = out.as_object_mut() {
        let mean = if stats.passes > 0 { stats.cook_ms_total / stats.passes as f64 } else { 0.0 };
        o.insert("cook_ms_mean".into(), json!(mean));
        o.insert("cooked_last_pass".into(), json!(nodes));
        o.insert("pass".into(), json!(pass));
    }
    Ok(out)
}

fn cook_meter(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        samples: Option<bool>,
    }
    let a: A = args(p)?;
    let meter = e.cook_meter();
    let now = std::time::Instant::now();
    let mut out = serde_json::to_value(meter.summary(now)).map_err(|e| e.to_string())?;
    if let Some(o) = out.as_object_mut() {
        o.insert("window_s".into(), json!(crate::meter::WINDOW.as_secs_f64()));
        if a.samples.unwrap_or(false) {
            let samples: Vec<Json> = meter
                .samples(now)
                .map(|s| json!([now.saturating_duration_since(s.at).as_secs_f64(), s.ms]))
                .collect();
            o.insert("samples".into(), Json::Array(samples));
        }
    }
    Ok(out)
}

fn system_load(e: &Engine, p: &Json) -> Result<Json> {
    args::<Empty>(p)?;
    // Two readings make a rate, so a sampler that was asleep needs a little over a second.
    let load = e.load().wait_newer(None, penumbra_system::load::INTERVAL * 3);
    let age = load.at.map(|at| at.elapsed().as_secs_f64());
    let mut out = serde_json::to_value(&load).map_err(|e| e.to_string())?;
    if let Some(o) = out.as_object_mut() {
        o.insert("age_s".into(), json!(age));
    }
    Ok(out)
}

fn system_process(e: &Engine, p: &Json) -> Result<Json> {
    args::<Empty>(p)?;
    let process = penumbra_system::process::stats()?;
    let gpu = e.gpu().and_then(|g| g.memory());
    let gpu_errors = e.gpu().map(|g| g.errors().count);
    Ok(json!({
        "process": process,
        "gpu": gpu.map(|m| json!({
            "allocated": m.allocated,
            "reserved": m.reserved,
            "allocations": m.allocations,
            "blocks": m.blocks,
        })),
        "gpu_errors": gpu_errors,
    }))
}
