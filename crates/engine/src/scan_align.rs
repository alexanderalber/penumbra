//! `scan.align`: lines projectors up from their scans, as one picture across all of them.
//!
//! Per projector a homography from camera pixels to projector positions is fitted robustly to
//! its `cam_to_proj.exr` ([`penumbra_calib::Homography::fit_robust`]); its inverse is the
//! projector's frame in the camera's picture. The projectors stand in a row (side by side) or a
//! column (one above the other), whichever way their frames' centres spread more; the content is
//! the rectangle (in the camera's axes) along the row from the near edge of the first frame to the
//! far edge of the last, and across it the band all frames cover. Each projector shows the part
//! of it between its own edges along the row (the outer ones to the content's ends): that part is
//! its mapper's `region`, and the four corners of that part, through the projector's homography,
//! are where the mapper's corners go (`corner_*`, offsets from the frame's corners, `fit`
//! stretch).
//!
//! That assumes the camera looks at the surface roughly square-on: the content is a rectangle
//! in the camera's picture. A camera turned by a quarter, upside down or mirrored is fine; the
//! first projector's frame decides which way is up.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::Homography;
use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_ops::tex::mapper::{CORNERS, MAPPER};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{edit, outcome, require};
use crate::engine::Engine;
use crate::scan::{CAMERA_MAP, PROJECTOR_MAP, ms, read_map};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Pairs within this many projector pixels always count as fitting.
const FLOOR_PX: f64 = 1.5;
/// Fewest decoded camera pixels a scan must have.
const MIN_POINTS: usize = 100;
/// Decimals of the params written.
const DECIMALS: f64 = 1e5;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.align",
        "Line projectors in a row or a column up from their scans (folders scan.capture or scan.decode wrote): a \
         homography camera -> projector per scan, the content as the camera-axis rectangle spanning all \
         their frames, and per projector its mapper's region (its share of the content) and corners \
         (where that share lands in the projector), fit stretch, written as one undo step; {scans: \
         [folder, ...], mappers? ([tex.mapper path, ...], one per scan; without them only the report), \
         apply? (true; false: report only)}",
        move |p| {
            let a: AlignArgs = args(p)?;
            align(&e, &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlignArgs {
    pub scans: Vec<PathBuf>,
    pub mappers: Option<Vec<NodePath>>,
    pub apply: Option<bool>,
}

/// The unit square's corners in the order of the mapper's corners: top left, top right, bottom
/// right, bottom left.
const UNIT: [[f64; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// The eight turns and mirrors of the camera's axes, as matrices (row by row).
const TURNS: [([[f64; 2]; 2], &str); 8] = [
    ([[1.0, 0.0], [0.0, 1.0]], "none"),
    ([[0.0, 1.0], [-1.0, 0.0]], "a quarter clockwise"),
    ([[-1.0, 0.0], [0.0, -1.0]], "upside down"),
    ([[0.0, -1.0], [1.0, 0.0]], "a quarter counter-clockwise"),
    ([[-1.0, 0.0], [0.0, 1.0]], "mirrored left to right"),
    ([[1.0, 0.0], [0.0, -1.0]], "mirrored top to bottom"),
    ([[0.0, 1.0], [1.0, 0.0]], "mirrored along the diagonal"),
    ([[0.0, -1.0], [-1.0, 0.0]], "mirrored along the other diagonal"),
];

fn turn(m: &[[f64; 2]; 2], p: [f64; 2]) -> [f64; 2] {
    [m[0][0] * p[0] + m[0][1] * p[1], m[1][0] * p[0] + m[1][1] * p[1]]
}

/// The inverse of a turn is its transpose.
fn unturn(m: &[[f64; 2]; 2], p: [f64; 2]) -> [f64; 2] {
    [m[0][0] * p[0] + m[1][0] * p[1], m[0][1] * p[0] + m[1][1] * p[1]]
}

fn round(v: f64) -> f64 {
    let r = (v * DECIMALS).round() / DECIMALS;
    // No negative zero in the params.
    if r == 0.0 { 0.0 } else { r }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    let at = ((sorted.len().max(1) as f64 - 1.0) * q).round() as usize;
    sorted.get(at).copied().unwrap_or(f64::NAN)
}

/// One projector's fit.
struct Fitted {
    scan: PathBuf,
    /// Camera px to projector uv.
    h: Homography,
    /// The projector's frame in camera px (top left, top right, bottom right, bottom left).
    frame: [[f64; 2]; 4],
    points: usize,
    kept: usize,
    error_px: [f64; 3],
    projector: [usize; 2],
}

fn fit(dir: &Path, scan: &Path) -> CmdResult<Fitted> {
    let folder = dir.join(scan);
    let cam = read_map(&folder.join(CAMERA_MAP))?;
    let proj = read_map(&folder.join(PROJECTOR_MAP))?;
    let (pw, ph) = (proj.width, proj.height);
    let mut from = Vec::new();
    let mut to = Vec::new();
    for (i, p) in cam.rgba.iter().enumerate() {
        let (u, v) = (f64::from(p[0]), f64::from(p[1]));
        if u.is_finite() && v.is_finite() && u >= 0.0 && v >= 0.0 {
            from.push([(i % cam.width) as f64 + 0.5, (i / cam.width) as f64 + 0.5]);
            to.push([u, v]);
        }
    }
    if from.len() < MIN_POINTS {
        return Err(format!(
            "{}: only {} decoded camera pixels; scan again (scan.capture reports why)",
            folder.display(),
            from.len()
        )
        .into());
    }
    let r = Homography::fit_robust(&from, &to, FLOOR_PX / pw.max(1) as f64)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut errors: Vec<f64> = r.errors.iter().map(|e| e * pw as f64).collect();
    errors.sort_by(f64::total_cmp);
    let inv = r.homography.inverse().ok_or_else(|| format!("{}: the fit is singular", folder.display()))?;
    let mut frame = [[0.0; 2]; 4];
    for (f, c) in frame.iter_mut().zip(UNIT) {
        *f = inv
            .apply(c)
            .ok_or_else(|| format!("{}: the projector's frame reaches infinity", folder.display()))?;
    }
    Ok(Fitted {
        scan: scan.to_path_buf(),
        h: r.homography,
        frame,
        points: from.len(),
        kept: r.kept,
        error_px: [0.5, 0.9, 0.99].map(|q| (percentile(&errors, q) * 100.0).round() / 100.0),
        projector: [pw, ph],
    })
}

pub fn align(e: &Engine, a: &AlignArgs) -> CmdResult<Json> {
    let started = Instant::now();
    if a.scans.is_empty() || a.scans.len() > 16 {
        return Err("`scans` lists 1 to 16 folders".into());
    }
    if let Some(m) = &a.mappers
        && m.len() != a.scans.len()
    {
        return Err("`mappers` lists one tex.mapper per scan".into());
    }
    let net = e.session().doc().snapshot();
    if let Some(mappers) = &a.mappers {
        for (k, path) in mappers.iter().enumerate() {
            let id = require(&net, path)?;
            if net.node(id).is_none_or(|n| n.kind != MAPPER) {
                return Err(format!("{path} is not a {MAPPER}").into());
            }
            if mappers.iter().take(k).any(|p| p == path) {
                return Err(format!("{path} is listed twice").into());
            }
        }
    }
    let session = e.session();
    let dir = session.dir();
    let fits: Vec<Fitted> = a.scans.iter().map(|s| fit(dir, s)).collect::<CmdResult<_>>()?;
    let fitted = started.elapsed();

    // Which way is up: the turn that makes the first frame's top edge point right and its left
    // edge down.
    let first = fits.first().ok_or("no scans")?;
    let axes = |f: &[[f64; 2]; 4], m: &[[f64; 2]; 2]| {
        let [tl, tr, _, bl] = f.map(|p| turn(m, p));
        let (ex, ey) = ([tr[0] - tl[0], tr[1] - tl[1]], [bl[0] - tl[0], bl[1] - tl[1]]);
        let n = |v: [f64; 2]| v[0].hypot(v[1]).max(1e-12);
        (ex[0] / n(ex), ey[1] / n(ey))
    };
    let (m, turned) = TURNS
        .iter()
        .max_by(|x, y| {
            let s = |t: &([[f64; 2]; 2], &str)| {
                let (a, b) = axes(&first.frame, &t.0);
                a + b
            };
            s(x).total_cmp(&s(y))
        })
        .copied()
        .ok_or("no turn")?;
    for f in &fits {
        let (x, y) = axes(&f.frame, &m);
        if x < 0.5 || y < 0.5 {
            return Err(format!(
                "{}: this projector's frame is turned against the first one's in the camera's view; \
                 scan.align lines up projectors that stand the same way",
                f.scan.display()
            )
            .into());
        }
    }

    // Edges in the turned camera: left, right, top, bottom of each frame.
    let edges: Vec<[f64; 4]> = fits
        .iter()
        .map(|f| {
            let [tl, tr, br, bl] = f.frame.map(|p| turn(&m, p));
            [tl[0].max(bl[0]), tr[0].min(br[0]), tl[1].max(tr[1]), bl[1].min(br[1])]
        })
        .collect();
    // In a row (side by side) or a column (one above the other), as the frames' centres spread.
    let spread = |vertical: bool| {
        let c: Vec<f64> =
            edges.iter().map(|[l, r, t, b]| if vertical { (t + b) / 2.0 } else { (l + r) / 2.0 }).collect();
        c.iter().copied().fold(f64::MIN, f64::max) - c.iter().copied().fold(f64::MAX, f64::min)
    };
    let column = spread(true) > spread(false);
    // Per frame its interval along the row (or column) and across it.
    let along = |k: usize| edges.get(k).map(|e| if column { [e[2], e[3]] } else { [e[0], e[1]] });
    let across = |k: usize| edges.get(k).map(|e| if column { [e[0], e[1]] } else { [e[2], e[3]] });
    let mut order: Vec<usize> = (0..fits.len()).collect();
    order.sort_by(|x, y| {
        let c = |k: usize| along(k).map_or(0.0, |a| a[0] + a[1]);
        c(*x).total_cmp(&c(*y))
    });
    let (Some(&lo), Some(&hi)) = (order.first(), order.last()) else { return Err("no scans".into()) };
    let get = |v: Option<[f64; 2]>| v.ok_or_else(|| CommandError("lost a scan".into()));
    let start = get(along(lo))?[0];
    let end = get(along(hi))?[1];
    let mut band = [f64::MIN, f64::MAX];
    for k in 0..fits.len() {
        let c = get(across(k))?;
        band = [band[0].max(c[0]), band[1].min(c[1])];
    }
    let (length, breadth) = (end - start, band[1] - band[0]);
    if !(length > 1.0 && breadth > 1.0) {
        let frames: Vec<String> = fits
            .iter()
            .map(|f| {
                let c = f.frame.map(|p| format!("({:.0}, {:.0})", p[0], p[1]));
                format!(
                    "{}: frame in the camera {} (top left, top right, bottom right, bottom left), fit error median {} p90 {} projector px",
                    f.scan.display(),
                    c.join(" "),
                    f.error_px[0],
                    f.error_px[1]
                )
            })
            .collect();
        return Err(format!(
            "the projectors' frames share no rectangle in the camera's picture (taken as a {}, camera turned {turned}); {}",
            if column { "column" } else { "row" },
            frames.join("; ")
        )
        .into());
    }
    let mut warnings: Vec<String> = Vec::new();
    for pair in order.windows(2) {
        if let [x, y] = *pair
            && get(along(x))?[1] <= get(along(y))?[0]
        {
            warnings.push(format!(
                "{} and {} do not overlap: the content has a gap between them",
                fits.get(x).map(|f| f.scan.display().to_string()).unwrap_or_default(),
                fits.get(y).map(|f| f.scan.display().to_string()).unwrap_or_default()
            ));
        }
    }
    for f in &fits {
        if f.error_px[0] > 3.0 {
            warnings.push(format!(
                "{}: the homography fits with a median error of {} projector px: the surface is not flat (or the scan is noisy), so the corners line the projectors up only roughly",
                f.scan.display(),
                f.error_px[0]
            ));
        }
    }
    // A point of the content, `t` along and `c` across, in the turned camera.
    let point = |t: f64, c: f64| if column { [c, t] } else { [t, c] };

    let mapper_desc = e.types().desc(MAPPER);
    let mut ops = Vec::new();
    let mut report = Vec::new();
    for (k, f) in fits.iter().enumerate() {
        let own = get(along(k))?;
        let u0 = if k == lo { 0.0 } else { ((own[0] - start) / length).clamp(0.0, 1.0) };
        let u1 = if k == hi { 1.0 } else { ((own[1] - start) / length).clamp(0.0, 1.0) };
        if u1 <= u0 {
            return Err(format!("{}: this projector has no share of the content", f.scan.display()).into());
        }
        let (t0, t1) = (start + u0 * length, start + u1 * length);
        let (c0, c1) = (band[0], band[1]);
        let share = if column {
            [point(t0, c0), point(t0, c1), point(t1, c1), point(t1, c0)]
        } else {
            [point(t0, c0), point(t1, c0), point(t1, c1), point(t0, c1)]
        };
        let mut corners = [[0.0; 2]; 4];
        for (c, (p, base)) in corners.iter_mut().zip(share.into_iter().zip(UNIT)) {
            let q = f.h.apply(unturn(&m, p)).ok_or("a corner maps to infinity")?;
            *c = [round(q[0] - base[0]), round(q[1] - base[1])];
        }
        if corners.iter().flatten().any(|v| v.abs() > 0.5) {
            warnings.push(format!(
                "{}: a corner moves by more than half the picture; is the camera square-on and the scan right?",
                f.scan.display()
            ));
        }
        let region = if column {
            [0.0, round(u0), 1.0, round(u1 - u0)]
        } else {
            [round(u0), 0.0, round(u1 - u0), 1.0]
        };
        let mapper = a.mappers.as_ref().and_then(|m| m.get(k));
        if let Some(path) = mapper {
            let set = |name: &str, value: Value| -> CmdResult<Op> {
                Ok(Op::Set { param: ParamPath::new(path.clone(), Name::new(name)?)?, value })
            };
            ops.push(set("region", Value::Vec4(region))?);
            ops.push(set("fit", Value::Text("stretch".into()))?);
            for (name, c) in CORNERS.iter().zip(corners) {
                ops.push(set(name, Value::Vec2(c))?);
            }
            let id = require(&net, path)?;
            let warp = net
                .node(id)
                .zip(mapper_desc)
                .and_then(|(n, d)| n.param(d, "warp").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            if !warp.trim().is_empty() {
                warnings.push(format!("{path} has a mesh warp, which now moves the aligned picture"));
            }
        }
        let name = |i: usize| CORNERS.get(i).copied().unwrap_or("corner");
        let corner_json: serde_json::Map<String, Json> =
            corners.iter().enumerate().map(|(i, c)| (name(i).to_string(), json!(c))).collect();
        report.push(json!({
            "scan": f.scan.display().to_string(),
            "mapper": mapper.map(ToString::to_string),
            "place": order.iter().position(|o| *o == k),
            "projector": f.projector,
            "points": f.points,
            "kept": f.kept,
            "error_px": {"median": f.error_px[0], "p90": f.error_px[1], "p99": f.error_px[2]},
            "frame_in_camera": f.frame.map(|p| p.map(|v| v.round())),
            "region": region,
            "corners": corner_json,
        }));
    }
    let apply = a.apply.unwrap_or(true) && a.mappers.is_some();
    let history = if apply { Some(outcome(&edit(e, &ops, "scan.align")?)) } else { None };
    let (width, height) = if column { (breadth, length) } else { (length, breadth) };
    let aspect = width / height;
    // The content's corners back in the camera's own axes: x, y, x, y of its bounding box.
    let back: Vec<[f64; 2]> =
        [point(start, band[0]), point(end, band[0]), point(end, band[1]), point(start, band[1])]
            .iter()
            .map(|p| unturn(&m, *p))
            .collect();
    let camera_rect = [
        back.iter().map(|p| p[0]).fold(f64::MAX, f64::min).round(),
        back.iter().map(|p| p[1]).fold(f64::MAX, f64::min).round(),
        back.iter().map(|p| p[0]).fold(f64::MIN, f64::max).round(),
        back.iter().map(|p| p[1]).fold(f64::MIN, f64::max).round(),
    ];
    Ok(json!({
        "message": format!(
            "{} projector(s); content {:.0} x {:.0} camera px (aspect {:.3}){}",
            fits.len(),
            width,
            height,
            aspect,
            if apply { ", mappers written" } else { " (not applied)" }
        ),
        "projectors": report,
        "content": {
            "camera_rect": camera_rect,
            "aspect": (aspect * 1e4).round() / 1e4,
            "camera_turned": turned,
            "layout": if column { "column" } else { "row" },
        },
        "applied": apply,
        "history": history,
        "warnings": warnings,
        "ms": {"fit": ms(fitted), "total": ms(started.elapsed())},
        "next": "content of that aspect (e.g. tex.* with width = height x aspect) into the mappers; the mapper pattern param shows the grid",
    }))
}
