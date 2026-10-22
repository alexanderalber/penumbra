//! `scan.run`: the scan button of `tex.scan` nodes. Every node of the run names a projector, the
//! camera and a folder; the run scans the projectors one after another (`scan.capture`, each
//! into its node's folder), lines them up when every node names its mapper (`scan.align`) and
//! writes blend weights and smoothed maps into the scan folders (`scan.blend`), where the nodes
//! read them.
//!
//! The run has a thread of its own: the command returns at once (unless asked to wait), the
//! show keeps cooking and presenting, `scan.status` and the nodes report the progress, and
//! `scan.cancel` stops it before the next picture. A failed step after the scans (align, blend)
//! is a warning: the scans themselves are good.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::scan::Options;
use penumbra_core::{NodeId, NodePath, Value};
use penumbra_graph::Network;
use penumbra_ops::out::PROJECTOR;
use penumbra_ops::tex::mapper::MAPPER;
use penumbra_ops::tex::scan::{RunTicket, SCAN, ScanState, folder_name};
use penumbra_ops::tex::video_in::VIDEO_IN;
use penumbra_project::Actor;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::require;
use crate::engine::Engine;
use crate::scan::{BlendArgs, blend};
use crate::scan_align::{AlignArgs, align_as};
use crate::scan_capture::{CANCELLED, CaptureArgs, Watch, capture_watched};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// The share of a run's progress the scans take; aligning and blending the rest.
const SCANS_SHARE: f32 = 0.9;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.run",
        "The scan button of tex.scan nodes. Scans their projectors one after another with their camera \
         (scan.capture, each into the node's folder), lines them up if every node names its mapper \
         (scan.align, one undo step) and writes blend weights and smoothed maps into the scan folders \
         (scan.blend); the nodes then show them. Runs in the background (scan.status, scan.cancel); \
         {nodes? (tex.scan paths, default every tex.scan; all must name the same camera), wait? (return \
         when done, with the report, false)}",
        move |p| {
            let a: RunArgs = args(p)?;
            start(&e, &a, Actor::Claude)
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.status",
        "How the scans of the tex.scan nodes stand: running, progress (0 to 1), step, the last run's \
         error and warnings; {}",
        move |p| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {}
            let _: A = args(p)?;
            Ok(status(&e))
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.cancel",
        "Stop the scan going on (scan.run) before its next picture. The projectors show their pictures \
         again; the files of earlier scans stay; {}",
        move |p| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {}
            let _: A = args(p)?;
            let cancelled = e.scans().cancel();
            Ok(json!({
                "cancelled": cancelled,
                "message": if cancelled { "the scan stops before its next picture" } else { "no scan is running" },
            }))
        },
    ));
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    pub nodes: Option<Vec<NodePath>>,
    pub wait: Option<bool>,
}

/// One node of a run.
struct Item {
    node: NodeId,
    path: String,
    projector: NodePath,
    /// The scan root (relative to the project) and the projector's folder inside.
    root: PathBuf,
    scan: PathBuf,
    mapper: Option<NodePath>,
    settle: f64,
    level: f32,
    fill: u32,
}

struct Plan {
    camera: NodePath,
    items: Vec<Item>,
    /// The project directory the run belongs to.
    dir: PathBuf,
}

fn text<'n>(net: &'n Network, e: &'n Engine, id: NodeId, name: &str) -> &'n str {
    let (Some(node), Some(desc)) = (net.node(id), e.types().desc(SCAN)) else { return "" };
    node.param(desc, name).and_then(Value::as_str).map_or("", str::trim)
}

fn number(net: &Network, e: &Engine, id: NodeId, name: &str) -> Option<f64> {
    let (node, desc) = (net.node(id)?, e.types().desc(SCAN)?);
    node.param(desc, name).and_then(Value::as_f64)
}

/// The node `text` names from `id` (a path, or a name beside it), of kind `kind`.
fn named(net: &Network, id: NodeId, path: &str, param: &str, text: &str, kind: &str) -> CmdResult<NodePath> {
    if text.is_empty() {
        return Err(format!("{path}: `{param}` names no {kind}").into());
    }
    let target = net.resolve_from(id, text).map_err(|e| format!("{path}: `{param}`: {e}"))?;
    if net.node(target).is_none_or(|n| n.kind != kind) {
        return Err(format!("{path}: `{param}` {text} is not a {kind}").into());
    }
    net.path_of(target).ok_or_else(|| format!("{path}: `{param}` {text} has no path").into())
}

/// The `tex.scan` nodes naming the same camera as `node` (itself included), in network order:
/// what its scan button scans.
pub fn rig_of(e: &Engine, node: NodeId) -> Vec<NodePath> {
    let net = e.session().doc().snapshot();
    let camera = |id: NodeId| net.resolve_from(id, text(&net, e, id, "camera")).ok();
    let own = camera(node);
    net.nodes()
        .filter(|n| n.kind == SCAN)
        .filter(|n| n.id == node || (own.is_some() && camera(n.id) == own))
        .filter_map(|n| net.path_of(n.id))
        .collect()
}

fn plan(e: &Engine, nodes: Option<&[NodePath]>) -> CmdResult<Plan> {
    let net = e.session().doc().snapshot();
    let ids: Vec<NodeId> = match nodes {
        Some(paths) => {
            let mut ids = Vec::new();
            for p in paths {
                let id = require(&net, p)?;
                if net.node(id).is_none_or(|n| n.kind != SCAN) {
                    return Err(format!("{p} is not a {SCAN}").into());
                }
                if ids.contains(&id) {
                    return Err(format!("{p} is listed twice").into());
                }
                ids.push(id);
            }
            ids
        }
        None => net.nodes().filter(|n| n.kind == SCAN).map(|n| n.id).collect(),
    };
    if ids.is_empty() {
        return Err(format!("no {SCAN} to scan").into());
    }
    let mut camera: Option<NodePath> = None;
    let mut items: Vec<Item> = Vec::new();
    for id in ids {
        let path = net.path_of(id).map(|p| p.to_string()).unwrap_or_default();
        let projector = named(&net, id, &path, "projector", text(&net, e, id, "projector"), PROJECTOR)?;
        let cam = named(&net, id, &path, "camera", text(&net, e, id, "camera"), VIDEO_IN)?;
        match &camera {
            Some(c) if *c != cam => {
                return Err(format!(
                    "the {SCAN} nodes name different cameras ({c}, {cam}); list the nodes of one camera in `nodes`"
                )
                .into());
            }
            Some(_) => {}
            None => camera = Some(cam),
        }
        let mapper = match text(&net, e, id, "mapper") {
            "" => None,
            m => Some(named(&net, id, &path, "mapper", m, MAPPER)?),
        };
        let root = PathBuf::from(match text(&net, e, id, "folder") {
            "" => penumbra_ops::tex::scan::DEFAULT_FOLDER,
            f => f,
        });
        let scan = root.join(folder_name(&projector.to_string()));
        if let Some(other) = items.iter().find(|i| i.projector == projector) {
            return Err(format!("{} and {path} both scan {projector}", other.path).into());
        }
        if let Some(other) = items.iter().find(|i| i.scan == scan) {
            return Err(
                format!("{} and {path} would write the same folder {}", other.path, scan.display()).into()
            );
        }
        items.push(Item {
            node: id,
            path,
            projector,
            root,
            scan,
            mapper,
            settle: number(&net, e, id, "settle").unwrap_or(crate::scan_capture::DEFAULT_SETTLE),
            level: number(&net, e, id, "level").unwrap_or(1.0) as f32,
            fill: number(&net, e, id, "fill")
                .map_or(crate::scan::DEFAULT_FILL, |f| f.clamp(0.0, 1024.0) as u32),
        });
    }
    let camera = camera.ok_or("no camera")?;
    Ok(Plan { camera, items, dir: e.session().dir().to_path_buf() })
}

/// Starts a run of `a.nodes` (or every `tex.scan`) as `actor` (whose the mappers' edit is).
pub fn start(e: &Arc<Engine>, a: &RunArgs, actor: Actor) -> CmdResult<Json> {
    let plan = plan(e, a.nodes.as_deref())?;
    let ids: Vec<NodeId> = plan.items.iter().map(|i| i.node).collect();
    let ticket = e.scans().begin(&ids).map_err(CommandError)?;
    let names: Vec<&str> = plan.items.iter().map(|i| i.path.as_str()).collect();
    let started = json!({
        "started": true,
        "nodes": names,
        "camera": plan.camera.to_string(),
        "message": format!(
            "scanning {} projector(s) with {}; scan.status follows it, scan.cancel stops it",
            plan.items.len(),
            plan.camera
        ),
    });
    let wait = a.wait.unwrap_or(false);
    let engine = Arc::clone(e);
    let t = ticket.clone();
    let spawned = std::thread::Builder::new().name("scan-run".into()).spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&engine, &plan, &t, actor)))
            .unwrap_or_else(|_| Err("the scan stopped on an internal error".into()));
        finish(&engine, &plan, &t, &outcome);
        outcome
    });
    match spawned {
        Ok(handle) if wait => match handle.join() {
            Ok(r) => r,
            Err(_) => Err("the scan stopped on an internal error".into()),
        },
        Ok(_) => Ok(started),
        Err(err) => {
            let message = format!("cannot start the scan: {err}");
            for id in &ids {
                e.scans().update(&ticket, *id, |s| s.error = Some(message.clone()));
            }
            e.scans().end(&ticket, &ids);
            Err(message.into())
        }
    }
}

/// Every node of the run gets the outcome; logged.
fn finish(e: &Engine, plan: &Plan, ticket: &RunTicket, outcome: &CmdResult<Json>) {
    let ids: Vec<NodeId> = plan.items.iter().map(|i| i.node).collect();
    match outcome {
        Ok(r) => {
            let warnings: Vec<String> = r
                .get("warnings")
                .and_then(Json::as_array)
                .map(|w| w.iter().filter_map(Json::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            log::info!("scan: {}", r.get("message").and_then(Json::as_str).unwrap_or("done"));
            for w in &warnings {
                log::warn!("scan: {w}");
            }
            for (k, item) in plan.items.iter().enumerate() {
                let measured = r.pointer(&format!("/scans/{k}/projector_pixels")).and_then(Json::as_u64);
                let seconds = r.get("seconds").and_then(Json::as_f64).unwrap_or(0.0);
                let step = match measured {
                    Some(m) => format!("scanned in {seconds:.0} s, {m} projector px measured"),
                    None => format!("scanned in {seconds:.0} s"),
                };
                let warnings = warnings.clone();
                e.scans().update(ticket, item.node, |s| {
                    s.step = step;
                    s.error = None;
                    s.warnings = warnings;
                });
            }
        }
        Err(err) => {
            if err.0 == CANCELLED {
                log::info!("scan: cancelled");
            } else {
                log::warn!("scan failed: {}", err.0);
            }
            for id in &ids {
                e.scans().update(ticket, *id, |s| {
                    s.step = if err.0 == CANCELLED { "cancelled".into() } else { "failed".into() };
                    s.error = Some(err.0.clone());
                });
            }
        }
    }
    e.scans().end(ticket, &ids);
}

fn run(e: &Engine, plan: &Plan, ticket: &RunTicket, actor: Actor) -> CmdResult<Json> {
    let started = Instant::now();
    let n = plan.items.len();
    let set_all = |f: &dyn Fn(&mut ScanState)| {
        for item in &plan.items {
            e.scans().update(ticket, item.node, |s| f(s));
        }
    };
    let check = || -> CmdResult<()> {
        if ticket.cancelled() {
            return Err(CANCELLED.into());
        }
        if e.session().dir() != plan.dir {
            return Err("another project was opened".into());
        }
        Ok(())
    };
    let mut warnings: Vec<String> = Vec::new();
    let mut scans: Vec<Json> = Vec::new();
    for (k, item) in plan.items.iter().enumerate() {
        check()?;
        let step = format!("scanning {} ({} of {n})", item.projector, k + 1);
        let at = k as f32 / n as f32 * SCANS_SHARE;
        set_all(&|s| {
            s.step.clone_from(&step);
            s.progress = Some(at);
        });
        let progress = |_: usize, f: f32| {
            let p = (k as f32 + f.clamp(0.0, 1.0)) / n as f32 * SCANS_SHARE;
            set_all(&|s| s.progress = Some(p));
        };
        let watch = Watch { cancel: &ticket.cancel, progress: &progress };
        let args = CaptureArgs {
            camera: plan.camera.clone(),
            projectors: Some(vec![item.projector.clone()]),
            out: Some(item.root.clone()),
            settle: Some(item.settle),
            timeout: None,
            level: Some(item.level),
            frames: None,
            verify: None,
            retries: None,
            fill: Some(item.fill),
            options: Options::default(),
        };
        let r = capture_watched(e, &args, Some(&watch)).map_err(|err| {
            if err.0 == CANCELLED { err } else { CommandError(format!("{}: {}", item.path, err.0)) }
        })?;
        if let Some(w) = r.get("warnings").and_then(Json::as_array) {
            warnings.extend(w.iter().filter_map(Json::as_str).map(str::to_string));
        }
        let p = r.pointer("/projectors/0").cloned().unwrap_or(Json::Null);
        scans.push(json!({
            "node": item.path,
            "projector": item.projector.to_string(),
            "folder": item.scan.display().to_string().replace('\\', "/"),
            "camera_pixels": p.pointer("/camera_pixels/valid"),
            "projector_pixels": p.pointer("/projector_pixels/measured"),
            "latency_ms": p.get("latency_ms"),
        }));
    }
    let folders: Vec<PathBuf> = plan.items.iter().map(|i| i.scan.clone()).collect();

    let mappers: Option<Vec<NodePath>> = plan.items.iter().map(|i| i.mapper.clone()).collect();
    let mut aligned = Json::Null;
    if let Some(mappers) = mappers {
        check()?;
        set_all(&|s| {
            s.step = "lining the projectors up".into();
            s.progress = Some(SCANS_SHARE);
        });
        let a = AlignArgs { scans: folders.clone(), mappers: Some(mappers), apply: Some(true) };
        match align_as(e, &a, actor) {
            Ok(r) => {
                if let Some(w) = r.get("warnings").and_then(Json::as_array) {
                    warnings.extend(w.iter().filter_map(Json::as_str).map(|w| format!("scan.align: {w}")));
                }
                aligned = json!({"aspect": r.pointer("/content/aspect"), "message": r.get("message")});
            }
            Err(err) => warnings.push(format!("scan.align: {}", err.0)),
        }
    }

    check()?;
    set_all(&|s| {
        s.step = "blending".into();
        s.progress = Some(SCANS_SHARE + (1.0 - SCANS_SHARE) / 2.0);
    });
    let b = BlendArgs {
        scans: folders,
        masks: None,
        out: None,
        ramp: None,
        blur: None,
        reach: None,
        sigma: None,
        into_scans: Some(true),
    };
    let blended = match blend(&plan.dir, &b) {
        Ok(r) => {
            if let Some(w) = r.get("warnings").and_then(Json::as_array) {
                warnings.extend(w.iter().filter_map(Json::as_str).map(|w| format!("scan.blend: {w}")));
            }
            r.get("overlap").cloned().unwrap_or(Json::Null)
        }
        Err(err) => {
            warnings.push(format!("scan.blend: {}", err.0));
            Json::Null
        }
    };
    let seconds = (started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    Ok(json!({
        "message": format!(
            "{n} projector(s) scanned in {seconds:.0} s{}",
            if aligned.is_null() { "" } else { ", lined up" }
        ),
        "scans": scans,
        "align": aligned,
        "blend": blended,
        "warnings": warnings,
        "seconds": seconds,
    }))
}

/// Every `tex.scan` and how its scan stands.
pub fn status(e: &Engine) -> Json {
    let net = e.session().doc().snapshot();
    let nodes: Vec<Json> = net
        .nodes()
        .filter(|n| n.kind == SCAN)
        .map(|n| {
            json!({
                "path": net.path_of(n.id).map(|p| p.to_string()),
                "projector": text(&net, e, n.id, "projector"),
                "state": e.scans().state(n.id).unwrap_or_default(),
            })
        })
        .collect();
    json!({"running": e.scans().running(), "nodes": nodes})
}
