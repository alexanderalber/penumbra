//! `state.*` (persistent state, [`crate::state`]), `cook.stats` (pass timings over a stretch of
//! time) and `system.process` (memory, handles, threads, GPU memory), the last two for the soak
//! test and for looking at a long-running show; `cook.meter` and `system.load` (the last seconds'
//! pass times, CPU and GPU load), what the status bar's cook time shows; `node.memory` (video
//! memory per node against what wgpu has allocated); `cook.profile` (where the frame time goes,
//! by step and by node).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::no_args;
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
        "cook.profile",
        "Where the frame time went since the last reset, in ms per frame: the steps of a frame, the steps of the cook pass, and the nodes under {under?} (default /) summed by its children, by kind and one by one (the {top?} most, default 30). A node's `cook_ms` is its operator's work, `visit_ms` adds what the scheduler spends on it; {reset?}",
        cook_profile,
    );
    add(
        "cook.meter",
        "Pass times of the last 8 s: last, smoothed (1 s lag), median, 1 % high, max, cook thread busy; {samples?: [seconds ago, ms] per pass}",
        cook_meter,
    );
    add(
        "system.load",
        "CPU over all cores and the busiest GPU engine in percent, sampled once a second while asked for (the first call waits for a sample). On NVIDIA GPUs `nvidia`: power_w, power_limit_w, temperature_c, clock_mhz, limited_by of the GPU penumbra renders on (`matched`), else the first",
        system_load,
    );
    add(
        "node.memory",
        "Video memory per node, largest first. Also what the editor holds (pictures of geometry, display copies) and the render attachments that renderers of one size share. All of it against what wgpu has allocated, and wgpu's allocations by label; {limit?} (default 30 of each)",
        node_memory,
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
    no_args(p)?;
    let (session, generation) = e.current();
    let dir = crate::state::dir_of(session.dir());
    let now = e.on_cook_in(generation, move |st| {
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
    no_args(p)?;
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

/// Nodes `cook.profile` lists one by one when not told otherwise.
const PROFILE_TOP: usize = 30;

/// One line of `cook.profile`: a node, a group under the asked-for path, or a kind.
#[derive(Default)]
struct Spent {
    nodes: u64,
    cooks: u64,
    cook: f64,
    cook_max: f64,
    visit: f64,
}

impl Spent {
    fn add(&mut self, s: &penumbra_graph::NodeSpent) {
        self.nodes += 1;
        self.cooks += s.cooks;
        self.cook += s.cook.as_secs_f64();
        self.cook_max = self.cook_max.max(s.cook_max.as_secs_f64());
        self.visit += s.visit.as_secs_f64();
    }

    fn json(&self, passes: f64) -> serde_json::Map<String, Json> {
        let mut o = serde_json::Map::new();
        o.insert("cook_ms".into(), json!(round(self.cook * 1000.0 / passes)));
        o.insert("visit_ms".into(), json!(round(self.visit * 1000.0 / passes)));
        o.insert("cooks_per_frame".into(), json!(round(self.cooks as f64 / passes)));
        o
    }
}

/// Milliseconds to three decimals: microseconds are the finest step worth reading.
fn round(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

fn cook_profile(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        reset: Option<bool>,
        under: Option<String>,
        top: Option<usize>,
    }
    let a: A = args(p)?;
    let reset = a.reset.unwrap_or(false);
    let top = a.top.unwrap_or(PROFILE_TOP);
    let under = a.under.unwrap_or_else(|| "/".into());
    let under = under.trim_end_matches('/').to_string();
    let session = e.session().clone();
    let net = session.doc().snapshot();
    if !under.is_empty() {
        let path: penumbra_core::NodePath =
            under.parse().map_err(|err| CommandError(format!("`{under}`: {err}")))?;
        if net.resolve(&path).is_none() {
            return Err(CommandError(format!("no node `{under}`")));
        }
    }
    let (frames, profile) = e.on_cook(move |st| {
        let frames = st.spent;
        if reset {
            st.spent = crate::engine::FrameSpent::default();
        }
        (frames, st.cooker.take_profile(reset))
    })?;
    let passes = profile.passes.passes.max(1) as f64;
    let per_frame = |d: Duration| json!(round(d.as_secs_f64() * 1000.0 / frames.frames.max(1) as f64));
    let per_pass = |d: Duration| json!(round(d.as_secs_f64() * 1000.0 / passes));

    let prefix = format!("{under}/");
    let mut groups: std::collections::BTreeMap<String, Spent> = Default::default();
    let mut kinds: std::collections::BTreeMap<String, Spent> = Default::default();
    let mut nodes: Vec<(String, String, penumbra_graph::NodeSpent)> = Vec::new();
    let mut visit_all = Duration::ZERO;
    let mut cook_all = Duration::ZERO;
    for (id, spent) in &profile.nodes {
        visit_all += spent.visit;
        cook_all += spent.cook;
        let (Some(path), Some(def)) = (net.path_of(*id), net.node(*id)) else { continue };
        let path = path.to_string();
        let Some(rest) = path.strip_prefix(&prefix) else { continue };
        let child = rest.split('/').next().unwrap_or(rest);
        groups.entry(format!("{prefix}{child}")).or_default().add(spent);
        kinds.entry(def.kind.clone()).or_default().add(spent);
        nodes.push((path, def.kind.clone(), *spent));
    }
    let mut groups: Vec<(String, Spent)> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.visit.total_cmp(&a.1.visit));
    let mut kinds: Vec<(String, Spent)> = kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.visit.total_cmp(&a.1.visit));
    nodes.sort_by_key(|a| std::cmp::Reverse(a.2.visit));
    let listed = nodes.len();
    let groups: Vec<Json> = groups
        .iter()
        .map(|(path, s)| {
            let mut o = s.json(passes);
            o.insert("path".into(), json!(path));
            o.insert("nodes".into(), json!(s.nodes));
            Json::Object(o)
        })
        .collect();
    let kinds: Vec<Json> = kinds
        .iter()
        .take(top)
        .map(|(kind, s)| {
            let mut o = s.json(passes);
            o.insert("kind".into(), json!(kind));
            o.insert("nodes".into(), json!(s.nodes));
            Json::Object(o)
        })
        .collect();
    let nodes: Vec<Json> = nodes
        .iter()
        .take(top)
        .map(|(path, kind, spent)| {
            let mut s = Spent::default();
            s.add(spent);
            let mut o = s.json(passes);
            o.insert("path".into(), json!(path));
            o.insert("kind".into(), json!(kind));
            o.insert("cook_ms_max".into(), json!(round(s.cook_max * 1000.0)));
            Json::Object(o)
        })
        .collect();
    let pass = profile.passes;
    let evaluating = pass.outputs + pass.thumbnails;
    Ok(json!({
        "seconds": frames.since.map(|t| t.elapsed().as_secs_f64()),
        "frames": frames.frames,
        "passes": pass.passes,
        "frame_ms": {
            "total": per_frame(frames.total),
            "network": per_frame(frames.network),
            "cook": per_frame(frames.cook),
            "audio": per_frame(frames.audio),
            "outputs": per_frame(frames.outputs),
            "prune": per_frame(frames.prune),
            "resolution": per_frame(frames.resolution),
            "gpu_poll": per_frame(frames.gpu_poll),
            "state": per_frame(frames.state),
            "free_views": per_frame(frames.free_views),
            "publish": per_frame(frames.publish),
        },
        "pass_ms": {
            "sync": per_pass(pass.sync),
            "reads": per_pass(pass.reads),
            "outputs": per_pass(pass.outputs),
            "thumbnails": per_pass(pass.thumbnails),
            "trim": per_pass(pass.trim),
            "nodes_cook": per_pass(cook_all),
            "nodes_visit": per_pass(visit_all),
            "walking": per_pass(evaluating.saturating_sub(visit_all)),
        },
        "under": if under.is_empty() { "/" } else { under.as_str() },
        "groups": groups,
        "kinds": kinds,
        "nodes": nodes,
        "nodes_seen": listed,
        "not_counted": "GPU execution time (a node's cook_ms holds recording and submitting its GPU work, not the GPU running it); the editor's own drawing; output windows presenting",
    }))
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
    no_args(p)?;
    // Two readings make a rate, so a sampler that was asleep needs a little over a second.
    let load = e.load().wait_newer(None, penumbra_system::load::INTERVAL * 3);
    let age = load.at.map(|at| at.elapsed().as_secs_f64());
    let mut out = serde_json::to_value(&load).map_err(|e| e.to_string())?;
    if let Some(o) = out.as_object_mut() {
        o.insert("age_s".into(), json!(age));
    }
    Ok(out)
}

/// Nodes `node.memory` lists when not told otherwise.
const MEMORY_LIMIT: usize = 30;

fn node_memory(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        limit: Option<usize>,
    }
    let a: A = args(p)?;
    let limit = a.limit.unwrap_or(MEMORY_LIMIT);
    let (session, generation) = e.current();
    let (mut nodes, views) = e.on_cook_in(generation, move |st| {
        let net = session.doc().snapshot();
        let nodes: Vec<(String, String, u64)> = net
            .nodes()
            .filter_map(|n| {
                let bytes = st.cooker.gpu_bytes(n.id).filter(|b| *b > 0)?;
                Some((net.path_of(n.id)?.to_string(), n.kind.clone(), bytes))
            })
            .collect();
        (nodes, st.view_gpu_bytes())
    })?;
    nodes.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let total: u64 = nodes.iter().map(|n| n.2).sum();
    let (free_views, geo_pictures) = views;
    let gpu = e.gpu();
    let attachments = gpu.map_or(0, penumbra_gpu::Gpu::render_attachment_bytes);
    let display = e.display_bytes();
    let counted = total + free_views + geo_pictures + attachments + display;
    let memory = gpu.and_then(|g| g.memory());
    let vram = gpu.and_then(|g| g.vram());
    let labels: Option<Vec<Json>> = gpu.and_then(|g| g.allocations_by_label()).map(|v| {
        v.into_iter()
            .take(limit)
            .map(|(label, bytes, count)| json!({"label": label, "bytes": bytes, "count": count}))
            .collect()
    });
    let listed: Vec<Json> = nodes
        .iter()
        .take(limit)
        .map(|(path, kind, bytes)| json!({"path": path, "kind": kind, "bytes": bytes}))
        .collect();
    Ok(json!({
        "nodes": listed,
        "nodes_holding": nodes.len(),
        "nodes_bytes": total,
        "free_views_bytes": free_views,
        "geometry_pictures_bytes": geo_pictures,
        "render_attachments_bytes": attachments,
        "display_copies_bytes": display,
        "counted_bytes": counted,
        "allocated": memory.map(|m| m.allocated),
        "reserved": memory.map(|m| m.reserved),
        "not_counted_bytes": memory.map(|m| m.allocated.saturating_sub(counted)),
        "vram_usage": vram.map(|v| v.usage),
        "vram_budget": vram.map(|v| v.budget),
        "allocations_by_label": labels,
        "not_counted": "output windows, the window's own textures, shader pipelines, uniform buffers; allocations_by_label shows them by the label they were created with",
    }))
}

fn system_process(e: &Engine, p: &Json) -> Result<Json> {
    no_args(p)?;
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
