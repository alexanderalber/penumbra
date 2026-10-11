//! `pn soak`: runs the engine headless on a generated network for a while and watches it for
//! leaks and stalls.
//!
//! The network has what a long installation has: a feedback loop (with its state saved to disk
//! every `--state-interval` seconds), blur, transform, text, a held cache, a shader with hot
//! reload, a render node with a sphere, a mapper, a channel bound to a param. While it runs the
//! tool edits a param every 2 s (history), creates and deletes a node every 20 s (operators,
//! textures, pipelines), takes a PNG every minute and restores the saved state every
//! `--restore-every` seconds. Every `--sample` seconds it records memory, handles, threads, GPU
//! memory, cook pass timings and the state snapshots in `samples.csv`; at the end it fits a
//! line through everything after the warm-up and judges it against the thresholds in
//! [`Thresholds`] (`summary.json`, exit code 1 on a failure).
//!
//! The process caps its own memory with a job object (`--mem-cap-mb`, default 4096; 0 for
//! none), so a leak cannot take the machine down.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_engine::{Engine, Registry};
use penumbra_gpu::{AdapterChoice, Gpu};
use penumbra_project::{Content, Session};
use serde::Serialize;
use serde_json::{Value as Json, json};

pub const USAGE: &str = "pn soak [--minutes N | --seconds N] [--out DIR] [--sample S] [--state-interval S] \
[--restore-every S] [--size WxH] [--software] [--mem-cap-mb M]";

const SHADER: &str = "// Soak test shader: ripples over the input.
// @param amount float = 0.02 0..0.2
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let d = v.uv - 0.5;
    let r = length(d);
    let uv = v.uv + d * params.amount * sin(r * 40.0 - pn_time.seconds * 3.0);
    return textureSampleLevel(in0, pn_clamp, uv, 0.0);
}
";

#[derive(Clone, Debug, Serialize)]
struct Options {
    seconds: f64,
    out: PathBuf,
    sample: f64,
    state_interval: f64,
    restore_every: f64,
    width: u32,
    height: u32,
    software: bool,
    mem_cap_mb: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            seconds: 600.0,
            out: std::env::temp_dir().join("penumbra").join("soak"),
            sample: 10.0,
            state_interval: 30.0,
            restore_every: 300.0,
            width: 1920,
            height: 1080,
            software: false,
            mem_cap_mb: 4096,
        }
    }
}

/// What counts as a failure, stated up front.
#[derive(Clone, Debug, Serialize)]
struct Thresholds {
    /// Growth of the Rust heap after the warm-up (our code, so judged strictly): this much plus
    /// `heap_mb_per_hour` per hour of the measured span.
    heap_mb: f64,
    heap_mb_per_hour: f64,
    /// Growth of private bytes, most of which the GPU driver holds and which moves in steps
    /// when another program uses the GPU (seen while the machine was shared): above
    /// `memory_warn_mb` (plus per hour) a warning, above `memory_mb` (plus per hour) a failure.
    memory_warn_mb: f64,
    memory_mb: f64,
    memory_mb_per_hour: f64,
    /// The same for GPU memory held through wgpu.
    gpu_mb: f64,
    gpu_mb_per_hour: f64,
    handles: f64,
    threads: f64,
    /// Gaps of more than 100 ms between cook passes after the warm-up: this many plus one per
    /// ten minutes (the machine is shared, so a few are not the engine's).
    stalls: u64,
    stalls_per_10_min: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            heap_mb: 4.0,
            heap_mb_per_hour: 4.0,
            memory_warn_mb: 32.0,
            memory_mb: 256.0,
            memory_mb_per_hour: 64.0,
            gpu_mb: 32.0,
            gpu_mb_per_hour: 16.0,
            handles: 50.0,
            threads: 4.0,
            stalls: 2,
            stalls_per_10_min: 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
struct Sample {
    t: f64,
    private_mb: f64,
    /// Bytes the Rust heap holds (`heap.rs`): a leak in our code grows this.
    heap_mb: f64,
    working_set_mb: f64,
    handles: u64,
    threads: u64,
    gpu_allocated_mb: f64,
    gpu_reserved_mb: f64,
    gpu_allocations: u64,
    passes: u64,
    cook_ms_mean: f64,
    cook_ms_max: f64,
    gap_ms_max: f64,
    slow: u64,
    late: u64,
    stalls: u64,
    state_saves: u64,
    state_failures: u64,
    state_bytes: u64,
    state_write_ms: f64,
    state_gpu_ms: f64,
    state_collect_ms: f64,
    node_errors: u64,
    gpu_errors: u64,
}

const CSV_HEADER: &str = "t_s,private_mb,heap_mb,working_set_mb,handles,threads,gpu_allocated_mb,gpu_reserved_mb,\
gpu_allocations,passes,cook_ms_mean,cook_ms_max,gap_ms_max,slow,late,stalls,state_saves,state_failures,\
state_bytes,state_write_ms,state_gpu_ms,state_collect_ms,node_errors,gpu_errors";

impl Sample {
    fn csv(&self) -> String {
        format!(
            "{:.1},{:.2},{:.2},{:.2},{},{},{:.2},{:.2},{},{},{:.3},{:.2},{:.2},{},{},{},{},{},{},{:.2},{:.2},{:.2},{},{}",
            self.t,
            self.private_mb,
            self.heap_mb,
            self.working_set_mb,
            self.handles,
            self.threads,
            self.gpu_allocated_mb,
            self.gpu_reserved_mb,
            self.gpu_allocations,
            self.passes,
            self.cook_ms_mean,
            self.cook_ms_max,
            self.gap_ms_max,
            self.slow,
            self.late,
            self.stalls,
            self.state_saves,
            self.state_failures,
            self.state_bytes,
            self.state_write_ms,
            self.state_gpu_ms,
            self.state_collect_ms,
            self.node_errors,
            self.gpu_errors
        )
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    let num = |v: Option<&String>, name: &str| -> Result<f64, String> {
        let v = v.ok_or_else(|| format!("{name} needs a value"))?;
        let x: f64 = v.parse().map_err(|_| format!("{name}: `{v}` is not a number"))?;
        if !(x.is_finite() && x > 0.0) {
            return Err(format!("{name} must be positive"));
        }
        Ok(x)
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" => o.seconds = num(it.next(), a)? * 60.0,
            "--seconds" => o.seconds = num(it.next(), a)?,
            "--sample" => o.sample = num(it.next(), a)?,
            "--state-interval" => o.state_interval = num(it.next(), a)?.clamp(1.0, 86_400.0),
            "--restore-every" => o.restore_every = num(it.next(), a)?,
            "--out" => o.out = PathBuf::from(it.next().ok_or("--out needs a directory")?),
            "--software" => o.software = true,
            "--mem-cap-mb" => {
                let v = it.next().ok_or("--mem-cap-mb needs a value")?;
                o.mem_cap_mb = v.parse().map_err(|_| format!("--mem-cap-mb: `{v}` is not a number"))?;
            }
            "--size" => {
                let v = it.next().ok_or("--size needs WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH, e.g. 1920x1080")?;
                o.width = w.parse().map_err(|_| "--size: bad width")?;
                o.height = h.parse().map_err(|_| "--size: bad height")?;
                if !(16..=8192).contains(&o.width) || !(16..=8192).contains(&o.height) {
                    return Err("--size: 16 to 8192 per side".into());
                }
            }
            other => return Err(format!("unknown argument `{other}`; usage: {USAGE}")),
        }
    }
    Ok(o)
}

struct Rig {
    engine: Arc<Engine>,
    registry: Registry,
}

impl Rig {
    fn call(&self, method: &str, params: Json) -> Result<Json, String> {
        let r = self.registry.dispatch(method, &params);
        if r.get("ok") == Some(&Json::Bool(true)) {
            Ok(r.get("result").cloned().unwrap_or(Json::Null))
        } else {
            Err(format!("{method} {params}: {}", r.get("error").cloned().unwrap_or(Json::Null)))
        }
    }
}

/// Runs the soak test with `args` (after `soak`).
pub fn run(args: &[String]) -> Result<ExitCode, String> {
    let o = parse(args)?;
    if o.mem_cap_mb > 0 {
        penumbra_system::process::cap_memory(o.mem_cap_mb << 20).map_err(|e| e.0)?;
    }
    let project = o.out.join("project");
    // A fresh project every run (the soak tool's own scratch directory).
    if project.exists() {
        std::fs::remove_dir_all(&project).map_err(|e| format!("{}: {e}", project.display()))?;
    }
    std::fs::create_dir_all(&project).map_err(|e| format!("{}: {e}", project.display()))?;
    std::fs::write(project.join("soak.wgsl"), SHADER).map_err(|e| format!("soak.wgsl: {e}"))?;
    let types = Arc::new(penumbra_ops::builtin_types().map_err(|e| e.0)?);
    let session = Arc::new(Session::open(&project, types, &Content::default()).map_err(|e| e.0)?);
    let engine = Engine::start(session).map_err(|e| e.0)?;
    let registry = penumbra_engine::engine_registry(&engine);
    let rig = Rig { engine: engine.clone(), registry };
    let result = soak(&rig, &o);
    engine.shutdown();
    result
}

fn soak(rig: &Rig, o: &Options) -> Result<ExitCode, String> {
    let choice = if o.software { AdapterChoice::Software } else { AdapterChoice::Hardware };
    let gpu = Gpu::headless(choice).map_err(|e| e.0)?;
    eprintln!("pn soak: {} for {:.0} s, output in {}", gpu.name(), o.seconds, o.out.display());
    rig.engine.attach_gpu(gpu).map_err(|e| e.0)?;
    wait_until(Duration::from_secs(10), || !rig.engine.state().holding());
    rig.call("state.config", json!({"interval": o.state_interval}))?;
    build(rig, o)?;

    let mut csv = String::from(CSV_HEADER);
    csv.push('\n');
    let csv_path = o.out.join("samples.csv");
    let mut samples: Vec<Sample> = Vec::new();
    let start = Instant::now();
    let mut next_sample = o.sample;
    let mut next_edit = 2.0;
    let mut next_churn = 20.0;
    let mut next_png = 60.0;
    let mut next_restore = o.restore_every;
    let (mut edits, mut churns, mut restores, mut pngs) = (0u64, 0u64, 0u64, 0u64);
    let mut problems: Vec<String> = Vec::new();
    rig.call("cook.stats", json!({"reset": true}))?;
    loop {
        let t = start.elapsed().as_secs_f64();
        if t >= o.seconds {
            break;
        }
        if t >= next_edit {
            next_edit += 2.0;
            edits += 1;
            let text = format!("soak {edits}");
            if let Err(e) = rig.call("node.set", json!({"path": "/text", "params": {"text": text}})) {
                problems.push(e);
            }
        }
        if t >= next_churn {
            next_churn += 20.0;
            churns += 1;
            let r = if churns % 2 == 1 {
                rig.call(
                    "node.create",
                    json!({"path": "/churn", "kind": "tex.noise", "params": {"width": 512, "height": 512, "seed": churns}}),
                )
                .and_then(|_| rig.call("node.watch", json!({"path": "/churn"})))
            } else {
                rig.call("node.delete", json!({"path": "/churn"}))
            };
            if let Err(e) = r {
                problems.push(e);
            }
        }
        if t >= next_png {
            next_png += 60.0;
            pngs += 1;
            for (node, file) in [("/end", "end.png"), ("/map", "map.png")] {
                if let Err(e) = rig.call("node.snapshot", json!({"path": node, "file": o.out.join(file)})) {
                    problems.push(e);
                }
            }
        }
        if t >= next_restore {
            next_restore += o.restore_every;
            restores += 1;
            match rig.call("state.restore", json!({})) {
                Ok(r) if r.get("restored").and_then(Json::as_array).is_some_and(|a| !a.is_empty()) => {}
                Ok(r) => problems.push(format!("state.restore restored nothing: {r}")),
                Err(e) => problems.push(e),
            }
        }
        if t >= next_sample {
            next_sample += o.sample;
            let s = sample(rig, t)?;
            eprintln!(
                "  {:>6.0} s  private {:>7.1} MB  heap {:>6.1} MB  gpu {:>7.1} MB  handles {:>5}  threads {:>3}  cook {:>5.2}/{:>6.2} ms  gap {:>6.1} ms  stalls {}  saves {}",
                s.t,
                s.private_mb,
                s.heap_mb,
                s.gpu_allocated_mb,
                s.handles,
                s.threads,
                s.cook_ms_mean,
                s.cook_ms_max,
                s.gap_ms_max,
                s.stalls,
                s.state_saves
            );
            csv.push_str(&s.csv());
            csv.push('\n');
            std::fs::write(&csv_path, &csv).map_err(|e| format!("{}: {e}", csv_path.display()))?;
            samples.push(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let roundtrip = roundtrip(rig, o);
    let summary = judge(&samples, o, &problems, roundtrip, [edits, churns, restores, pngs]);
    let text = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
    let path = o.out.join("summary.json");
    std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{text}");
    Ok(if summary.get("pass") == Some(&Json::Bool(true)) { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !done() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The generated network, built through commands as Claude would.
fn build(rig: &Rig, o: &Options) -> Result<(), String> {
    let size = json!({"width": o.width, "height": o.height});
    let with = |extra: Json| {
        let mut p = size.clone();
        if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        p
    };
    let create = |path: &str, kind: &str, params: Json| {
        rig.call("node.create", json!({"path": path, "kind": kind, "params": params}))
    };
    let connect = |from: &str, to: &str, input: Json| {
        rig.call("node.connect", json!({"from": from, "to": to, "input": input}))
    };
    create("/noise", "tex.noise", with(json!({"color": true, "speed": 0.3})))?;
    create("/fb", "tex.feedback", json!({}))?;
    create("/turn", "tex.transform", json!({"scale": [0.995, 0.995]}))?;
    create("/blur", "tex.blur", json!({"size": 2.0}))?;
    create("/text", "tex.text", with(json!({"text": "soak", "size": 160.0})))?;
    create("/mix", "tex.composite", json!({"op": "maximum"}))?;
    create("/end", "tex.level", json!({"brightness": 0.96}))?;
    create("/fresh", "tex.composite", json!({"op": "average"}))?;
    create("/cache", "tex.cache", json!({"length": 4, "record": false}))?;
    create("/ripple", "tex.shader", json!({"file": "soak.wgsl"}))?;
    create("/geo", "geo.primitive", json!({"shape": "sphere"}))?;
    create("/render", "tex.render", with(json!({})))?;
    create("/map", "tex.mapper", with(json!({})))?;
    create("/lfo", "chan.lfo", json!({"rate": 0.1, "amplitude": 0.5}))?;
    connect("/noise", "/fb", json!("initial"))?;
    connect("/fb", "/turn", json!(0))?;
    connect("/turn", "/blur", json!(0))?;
    connect("/blur", "/mix", json!("bottom"))?;
    connect("/text", "/mix", json!("top"))?;
    connect("/mix", "/end", json!(0))?;
    connect("/end", "/fb", json!("previous"))?;
    connect("/end", "/fresh", json!("bottom"))?;
    connect("/noise", "/fresh", json!("top"))?;
    connect("/fresh", "/cache", json!(0))?;
    connect("/end", "/ripple", json!(0))?;
    connect("/geo", "/render", json!("geo"))?;
    connect("/ripple", "/render", json!("texture"))?;
    connect("/render", "/map", json!(0))?;
    rig.call("node.bind", json!({"param": "/turn.rotate", "from": "/lfo"}))?;
    for node in ["/end", "/map", "/cache"] {
        rig.call("node.watch", json!({"path": node}))?;
    }
    // Fill the undo history to its limit (1000 entries) first: it grows by a few KB per edit until
    // then, which a 40-minute run showed as a heap growth that stopped at the limit. Measured from
    // here, the history only folds old entries away.
    for i in 0..1000 {
        rig.call("node.set", json!({"path": "/text", "params": {"text": format!("history {i}")}}))?;
    }
    // The cache holds what it has (state) after recording a moment.
    rig.call("node.set", json!({"path": "/cache", "params": {"record": true}}))?;
    std::thread::sleep(Duration::from_millis(500));
    rig.call("node.set", json!({"path": "/cache", "params": {"record": false}}))?;
    // Let the startup compile shaders and settle before anything is measured.
    std::thread::sleep(Duration::from_secs(2));
    let errors = node_errors(rig);
    if errors > 0 {
        let info = rig.call("node.list", json!({"recursive": true})).unwrap_or(Json::Null);
        return Err(format!("{errors} nodes report errors after building: {info}"));
    }
    Ok(())
}

fn node_errors(rig: &Rig) -> u64 {
    rig.engine.view().status.values().filter(|s| s.error.is_some()).count() as u64
}

fn sample(rig: &Rig, t: f64) -> Result<Sample, String> {
    let mb = |b: u64| b as f64 / (1 << 20) as f64;
    let p = penumbra_system::process::stats().map_err(|e| e.0)?;
    let stats = rig.call("cook.stats", json!({"reset": true}))?;
    let gpu = rig.engine.gpu();
    let mem = gpu.and_then(Gpu::memory).unwrap_or_default();
    let state = rig.engine.state().status();
    let f = |k: &str| stats.get(k).and_then(Json::as_f64).unwrap_or(0.0);
    let u = |k: &str| stats.get(k).and_then(Json::as_u64).unwrap_or(0);
    Ok(Sample {
        t,
        private_mb: mb(p.private_bytes),
        heap_mb: mb(crate::heap::live()),
        working_set_mb: mb(p.working_set),
        handles: p.handles,
        threads: p.threads,
        gpu_allocated_mb: mb(mem.allocated),
        gpu_reserved_mb: mb(mem.reserved),
        gpu_allocations: mem.allocations,
        passes: u("passes"),
        cook_ms_mean: f("cook_ms_mean"),
        cook_ms_max: f("cook_ms_max"),
        gap_ms_max: f("gap_ms_max"),
        slow: u("slow"),
        late: u("late"),
        stalls: u("stalls"),
        state_saves: state.saves,
        state_failures: state.failures,
        state_bytes: state.last_save.as_ref().map_or(0, |r| r.bytes),
        state_write_ms: state.last_save.as_ref().map_or(0.0, |r| r.write_ms),
        state_gpu_ms: state.last_save.as_ref().map_or(0.0, |r| r.gpu_ms),
        state_collect_ms: state.last_save.as_ref().map_or(0.0, |r| r.collect_ms),
        node_errors: node_errors(rig),
        gpu_errors: gpu.map_or(0, |g| g.errors().count),
    })
}

/// Pauses, saves, restores and checks that the feedback node shows the same picture.
fn roundtrip(rig: &Rig, o: &Options) -> Result<bool, String> {
    rig.call("cook.play", json!({"playing": false}))?;
    let before = rig.call("node.snapshot", json!({"path": "/fb", "file": o.out.join("fb_before.png")}))?;
    rig.call("state.save", json!({}))?;
    rig.call("state.restore", json!({}))?;
    let after = rig.call("node.snapshot", json!({"path": "/fb", "file": o.out.join("fb_after.png")}))?;
    rig.call("cook.play", json!({"playing": true}))?;
    let a = std::fs::read(o.out.join("fb_before.png")).map_err(|e| e.to_string())?;
    let b = std::fs::read(o.out.join("fb_after.png")).map_err(|e| e.to_string())?;
    Ok(before.get("stats") == after.get("stats") && a == b)
}

/// Least-squares line through `(t, y)`: the slope per second.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mt = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let num: f64 = points.iter().map(|(t, y)| (t - mt) * (y - my)).sum();
    let den: f64 = points.iter().map(|(t, _)| (t - mt) * (t - mt)).sum();
    if den > 0.0 { num / den } else { 0.0 }
}

fn judge(
    samples: &[Sample],
    o: &Options,
    problems: &[String],
    roundtrip: Result<bool, String>,
    [edits, churns, restores, pngs]: [u64; 4],
) -> Json {
    let th = Thresholds::default();
    let warmup = (o.seconds * 0.15).max(60.0).min(o.seconds * 0.5);
    let after: Vec<&Sample> = samples.iter().filter(|s| s.t >= warmup).collect();
    let span = match (after.first(), after.last()) {
        (Some(a), Some(b)) => b.t - a.t,
        _ => 0.0,
    };
    let hours = span / 3600.0;
    let growth = |f: &dyn Fn(&Sample) -> f64| {
        let pts: Vec<(f64, f64)> = after.iter().map(|s| (s.t, f(s))).collect();
        slope(&pts) * span
    };
    let memory = growth(&|s| s.private_mb);
    let heap = growth(&|s| s.heap_mb);
    let gpu = growth(&|s| s.gpu_allocated_mb);
    let handles = growth(&|s| s.handles as f64);
    let threads = growth(&|s| s.threads as f64);
    let stalls: u64 = after.iter().map(|s| s.stalls).sum();
    let late: u64 = after.iter().map(|s| s.late).sum();
    let passes: u64 = after.iter().map(|s| s.passes).sum();
    let gap_max = after.iter().map(|s| s.gap_ms_max).fold(0.0, f64::max);
    let cook_max = after.iter().map(|s| s.cook_ms_max).fold(0.0, f64::max);
    let cook_mean = if after.is_empty() {
        0.0
    } else {
        after.iter().map(|s| s.cook_ms_mean).sum::<f64>() / after.len() as f64
    };
    let last = samples.last().cloned().unwrap_or_default();
    let expected_saves = (o.seconds / o.state_interval).floor() as u64;
    let mut fails = Vec::new();
    let mut warnings = Vec::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            fails.push(what);
        }
    };
    let heap_limit = th.heap_mb + th.heap_mb_per_hour * hours;
    let mem_limit = th.memory_mb + th.memory_mb_per_hour * hours;
    let mem_warn = th.memory_warn_mb + th.memory_mb_per_hour * hours;
    let gpu_limit = th.gpu_mb + th.gpu_mb_per_hour * hours;
    let stall_limit = th.stalls + (span / 600.0 * th.stalls_per_10_min).floor() as u64;
    check(after.len() >= 3, format!("only {} samples after the warm-up", after.len()));
    check(
        heap <= heap_limit,
        format!("the Rust heap grew {heap:.1} MB (limit {heap_limit:.1}): a leak in our code"),
    );
    check(
        memory <= mem_limit,
        format!(
            "private memory grew {memory:.1} MB (limit {mem_limit:.1}; the Rust heap {heap:.1} MB of it)"
        ),
    );
    if memory > mem_warn && memory <= mem_limit {
        warnings.push(format!(
            "private memory grew {memory:.1} MB (warning above {mem_warn:.1}), the Rust heap {heap:.1} MB of it: \
             outside our heap, driver-side or another GPU user; rerun on an idle machine"
        ));
    }
    check(gpu <= gpu_limit, format!("GPU memory grew {gpu:.1} MB (limit {gpu_limit:.1})"));
    check(handles <= th.handles, format!("handles grew {handles:.0} (limit {})", th.handles));
    check(threads <= th.threads, format!("threads grew {threads:.1} (limit {})", th.threads));
    check(stalls <= stall_limit, format!("{stalls} stalls over 100 ms (limit {stall_limit})"));
    check(last.state_failures == 0, format!("{} state snapshots failed", last.state_failures));
    check(
        last.state_saves + 1 >= expected_saves,
        format!("{} state snapshots, expected about {expected_saves}", last.state_saves),
    );
    check(last.node_errors == 0, format!("{} nodes report errors", last.node_errors));
    check(last.gpu_errors == 0, format!("{} GPU errors", last.gpu_errors));
    check(problems.is_empty(), format!("{} commands failed", problems.len()));
    match &roundtrip {
        Ok(true) => {}
        Ok(false) => check(false, "the feedback frame changed through save and restore".into()),
        Err(e) => check(false, format!("the save and restore round trip failed: {e}")),
    }
    let mut report = String::new();
    for f in &fails {
        let _ = writeln!(report, "FAIL {f}");
    }
    for w in &warnings {
        let _ = writeln!(report, "WARN {w}");
    }
    json!({
        "pass": fails.is_empty(),
        "failures": fails,
        "warnings": warnings,
        "options": o,
        "thresholds": th,
        "warmup_s": warmup,
        "measured_span_s": span,
        "growth_after_warmup": {
            "private_mb": memory,
            "rust_heap_mb": heap,
            "gpu_allocated_mb": gpu,
            "handles": handles,
            "threads": threads,
        },
        "first_after_warmup": after.first().map(|s| (*s).clone()),
        "last": last,
        "cook": {
            "passes": passes,
            "mean_ms": cook_mean,
            "max_ms": cook_max,
            "gap_max_ms": gap_max,
            "late": late,
            "stalls_over_100ms": stalls,
        },
        "activity": {"edits": edits, "node_churns": churns, "restores": restores, "pngs": pngs},
        "roundtrip_identical": roundtrip.as_ref().ok(),
        "problems": problems.iter().take(20).collect::<Vec<_>>(),
        "report": report,
        "files": {"samples": o.out.join("samples.csv"), "summary": o.out.join("summary.json"), "project": o.out.join("project")},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_slope() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let o =
            parse(&s(&["--minutes", "2", "--size", "640x360", "--mem-cap-mb", "0", "--software"])).unwrap();
        assert_eq!((o.seconds, o.width, o.height, o.mem_cap_mb, o.software), (120.0, 640, 360, 0, true));
        assert!(parse(&s(&["--minutes", "-1"])).is_err());
        assert!(parse(&s(&["--size", "1x1"])).is_err());
        assert!(parse(&s(&["--bogus"])).is_err());
        let line: Vec<(f64, f64)> = (0..10).map(|i| (i as f64, 3.0 + 2.0 * i as f64)).collect();
        assert!((slope(&line) - 2.0).abs() < 1e-9);
        assert_eq!(slope(&[(1.0, 1.0)]), 0.0);
    }
}
