//! `camera.exposure`: whether the camera's exposure suits a measurement, checked in seconds
//! before `projector.match`, `projector.flatfield` or `scan.capture` spend minutes on it.
//!
//! Each projector alone shows white (the others black) and the camera's frames are read where
//! it lights the picture: headroom, signal, steadiness ([`penumbra_calib::exposure`]). The
//! long measurements run the clipping part of the same check on their own white pictures and
//! stop before the ramps when the camera clips.

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_calib::colour::{self, Picture};
use penumbra_calib::exposure::{self, Reading, Verdict};
use penumbra_core::NodeId;
use penumbra_ops::out::PROJECTOR;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::jobs::Job;
use crate::projector::{
    CameraStream, Claim, MAX_WIDTH, Rig, keep_area, projector_list, settle_of, timeout_of,
};
use penumbra_core::NodePath;
use penumbra_project::Actor;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Camera frames read per projector by default: enough to see bands move.
pub const DEFAULT_FRAMES: usize = 4;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "camera.exposure",
        "Check the camera's exposure before a match, flat field or scan, in a few seconds: each projector \
         alone shows white, the camera's frames are read where it lights the picture. Reports per \
         projector the brightest pixels (peak, of 255; aim for about 200), the median, the black, the \
         share that clips and in which channels, the change from frame to frame (flicker) and of the rows \
         between frames (banding: a shutter shorter than the projector's frame), a verdict (ok, clipped, \
         dark, unsteady), stops to change by and advice; `ok` is false while any projector clips. \
         {camera (tex.video_in path), projectors? (paths, default every active out.projector), area? \
         ([x, y, w, h] of the camera picture, 0..1), frames? (camera frames per projector, 4), settle? \
         (s, 0.5), timeout? (s, 5), wait? (false: start and answer at once; job.status follows it and          keeps the reply; true)}",
        move |p| {
            let a: ExposureArgs = args(p)?;
            exposure_job(&e, a, Actor::Claude)
        },
    ));
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExposureArgs {
    pub camera: NodePath,
    pub projectors: Option<Vec<NodePath>>,
    pub area: Option<[f64; 4]>,
    pub frames: Option<usize>,
    pub settle: Option<f64>,
    pub timeout: Option<f64>,
    /// Answer when done (the default), or start and answer at once (`job.status` follows it).
    pub wait: Option<bool>,
}

/// Runs the check as a helper job (one at a time with the match, flat field and blend; the
/// Helpers window shows its progress and reply): on this thread when `wait` (the default),
/// else on its own.
pub fn exposure_job(e: &Arc<Engine>, a: ExposureArgs, actor: Actor) -> CmdResult<Json> {
    let wait = a.wait.unwrap_or(true);
    crate::jobs::run(e, "camera.exposure", actor, wait, move |e, job| run(e, &a, Some(job)))
}

pub fn run(e: &Engine, a: &ExposureArgs, job: Option<&Job>) -> CmdResult<Json> {
    let frames = a.frames.unwrap_or(DEFAULT_FRAMES);
    if !(1..=30).contains(&frames) {
        return Err("`frames` is 1 to 30".into());
    }
    if let Some(r) = a.area
        && !(r.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && r[2] > 0.0 && r[3] > 0.0)
    {
        return Err("`area` is [x, y, w, h] within 0..1 of the camera picture".into());
    }
    let settle = settle_of(a.settle, crate::projector::DEFAULT_SETTLE)?;
    let timeout = timeout_of(a.timeout)?;
    let session = e.session();
    let net = session.doc().snapshot();
    let projectors = projector_list(e, &net, a.projectors.as_deref())?;
    if projectors.is_empty() {
        return Err(format!("no active {PROJECTOR} to check").into());
    }
    let camera = CameraStream::open(e, &net, &a.camera)?;
    let _claim = Claim::new(e, projectors.iter().map(|(id, _)| *id).collect())?;
    let mut rig = Rig {
        engine: e,
        projectors: projectors.clone(),
        camera,
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        pass: e.output().pass,
        frames: 1,
        job,
    };
    if let Some(job) = job {
        job.expect(1 + projectors.len());
        job.step("each projector alone in white", Some(0.0));
    }
    let started = Instant::now();
    let black = rig.shot(|_| [0.0; 3])?;
    let mut out = Vec::with_capacity(projectors.len());
    let mut ok = true;
    for (k, (_, path)) in projectors.iter().enumerate() {
        let mut whites = vec![rig.shot(|j| if j == k { [1.0; 3] } else { [0.0; 3] })?];
        let mut last = Instant::now();
        for _ in 1..frames {
            let f = rig.camera.next(e, rig.timeout, |f| f.arrived > last)?;
            last = f.arrived;
            let step = (f.width as usize).div_ceil(MAX_WIDTH).max(1);
            let p = colour::linear_picture(&rig.camera.planes(&f), step)
                .map_err(|err| CommandError::from(format!("{}: {err}", rig.camera.name)))?;
            whites.push(p);
        }
        let Some(first) = whites.first() else { continue };
        let Some(mask) = spot(first, &black, a.area) else {
            ok = false;
            out.push(json!({
                "projector": path,
                "verdict": "unseen",
                "advice": "lights no camera pixel (inside `area`): is it on, in the camera's view, and the \
                           exposure far too low?",
            }));
            continue;
        };
        let Some(r) = exposure::read(&whites, &black, &mask) else {
            return Err(format!("{path}: the camera's frames changed size while checking").into());
        };
        ok &= r.verdict() != Verdict::Clipped;
        out.push(reading_json(path, &r));
    }
    let summary: Vec<String> = out
        .iter()
        .map(|r| {
            format!(
                "{} {}{}",
                r["projector"].as_str().unwrap_or("?"),
                r["verdict"].as_str().unwrap_or("?"),
                r["peak"].as_f64().map(|p| format!(" (peak {p:.0})")).unwrap_or_default()
            )
        })
        .collect();
    // What to do, per projector, where there is something to do.
    let warnings: Vec<String> = out
        .iter()
        .filter_map(|r| Some(format!("{}: {}", r["projector"].as_str()?, r["advice"].as_str()?)))
        .collect();
    Ok(json!({
        "warnings": warnings,
        "message": format!(
            "exposure {}: {}",
            if ok { "usable" } else { "not usable" },
            summary.join(", ")
        ),
        "ok": ok,
        "projectors": out,
        "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    }))
}

/// Where `white` lights the picture over `black`, within `area`; `None` when (almost) nowhere.
fn spot(white: &Picture, black: &Picture, area: Option<[f64; 4]>) -> Option<Vec<bool>> {
    let mut mask = colour::lit(white, black)?;
    if let Some(area) = area {
        keep_area(&mut mask, black.width, black.height, area);
    }
    (colour::count(&mask) >= colour::MIN_PIXELS).then_some(mask)
}

fn code(v: f64) -> f64 {
    (v * 255.0).round()
}

fn reading_json(path: &str, r: &Reading) -> Json {
    let channels: Vec<&str> = ["red", "green", "blue"]
        .iter()
        .zip(r.clipped_channels)
        .filter(|(_, c)| *c)
        .map(|(n, _)| *n)
        .collect();
    json!({
        "projector": path,
        "verdict": r.verdict().name(),
        "peak": code(r.peak),
        "median": code(r.median),
        "black": code(r.black),
        "clipped_percent": (r.clipped * 1000.0).round() / 10.0,
        "clipped_channels": channels,
        "flicker_percent": (r.flicker * 1000.0).round() / 10.0,
        "banding_percent": (r.banding * 1000.0).round() / 10.0,
        "stops": (r.stops() * 10.0).round() / 10.0,
        "pixels": r.pixels,
        "advice": r.advice(),
    })
}

/// For the long measurements: an error naming every projector whose white (one camera picture
/// each, of it alone) clips within its lit pixels, unless `check` is false.
pub(crate) fn refuse_clipped(
    check: Option<bool>,
    projectors: &[(NodeId, String)],
    whites: &[Picture],
    black: &Picture,
    masks: &[Vec<bool>],
) -> CmdResult<()> {
    if check == Some(false) {
        return Ok(());
    }
    let mut bad = Vec::new();
    for (((_, path), white), mask) in projectors.iter().zip(whites).zip(masks) {
        if let Some(r) = exposure::read(std::slice::from_ref(white), black, mask)
            && r.verdict() == Verdict::Clipped
        {
            bad.push(format!("{path} ({:.1} %)", r.clipped * 100.0));
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "stopped before measuring: the camera clips the white of {}; lower the camera's exposure \
         (camera.exposure tells by how much), or pass check_exposure: false",
        bad.join(", ")
    )
    .into())
}
