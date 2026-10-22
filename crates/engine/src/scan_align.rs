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
use penumbra_calib::objects::Map;
use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::{Network, Op};
use penumbra_ops::tex::mapper::{CORNERS, MAPPER};
use penumbra_project::Actor;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{outcome, require};
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
        "Line projectors in a row or a column up from their scans (folders scan.capture or scan.decode \
         wrote). Fits a homography camera -> projector per scan; the content is the camera-axis rectangle \
         spanning all their frames. Writes per projector its mapper's region (its share of the content) \
         and corners (where that share lands in the projector), fit stretch, as one undo step; {scans: \
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
    fit_maps(scan, &folder, &cam, &proj)
}

/// The fit of a scan's maps: `cam` its `cam_to_proj`, `proj` its `proj_to_cam` (for the
/// projector's size only); `folder` names it in messages.
fn fit_maps(scan: &Path, folder: &Path, cam: &Map, proj: &Map) -> CmdResult<Fitted> {
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

/// `scans` and `mappers` of `a`, checked against each other.
fn check_lists(a: &AlignArgs) -> CmdResult<()> {
    if a.scans.is_empty() || a.scans.len() > 16 {
        return Err("`scans` lists 1 to 16 folders".into());
    }
    if let Some(m) = &a.mappers
        && m.len() != a.scans.len()
    {
        return Err("`mappers` lists one tex.mapper per scan".into());
    }
    Ok(())
}

/// Every mapper of `mappers` is a `tex.mapper` of `net`, each listed once.
fn check_mappers(net: &Network, mappers: &[NodePath]) -> CmdResult<()> {
    for (k, path) in mappers.iter().enumerate() {
        let id = require(net, path)?;
        if net.node(id).is_none_or(|n| n.kind != MAPPER) {
            return Err(format!("{path} is not a {MAPPER}").into());
        }
        if mappers.iter().take(k).any(|p| p == path) {
            return Err(format!("{path} is listed twice").into());
        }
    }
    Ok(())
}

/// How well turn `m` puts frame `f` upright: its top edge's share pointing right and its left
/// edge's share pointing down (1 and 1 when exactly upright).
fn axes(f: &[[f64; 2]; 4], m: &[[f64; 2]; 2]) -> (f64, f64) {
    let [tl, tr, _, bl] = f.map(|p| turn(m, p));
    let (ex, ey) = ([tr[0] - tl[0], tr[1] - tl[1]], [bl[0] - tl[0], bl[1] - tl[1]]);
    let n = |v: [f64; 2]| v[0].hypot(v[1]).max(1e-12);
    (ex[0] / n(ex), ey[1] / n(ey))
}

/// Which way is up: the turn that makes the first frame's top edge point right and its left edge
/// down, with its name; every frame must stand that way.
fn orient(fits: &[Fitted]) -> CmdResult<([[f64; 2]; 2], &'static str)> {
    let first = fits.first().ok_or("no scans")?;
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
    for f in fits {
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
    Ok((m, turned))
}

/// The content in the turned camera: the projectors in a row or a column, in order along it,
/// and the rectangle from the near edge of the first to the far edge of the last, across it
/// the band all frames cover.
struct Layout {
    /// Per frame left, right, top, bottom in the turned camera.
    edges: Vec<[f64; 4]>,
    column: bool,
    /// The frames' indices along the row (column).
    order: Vec<usize>,
    lo: usize,
    hi: usize,
    start: f64,
    end: f64,
    band: [f64; 2],
}

impl Layout {
    /// Frame `k`'s interval along the row (or column).
    fn along(&self, k: usize) -> CmdResult<[f64; 2]> {
        let e = self.edges.get(k).ok_or_else(|| CommandError("lost a scan".into()))?;
        Ok(if self.column { [e[2], e[3]] } else { [e[0], e[1]] })
    }

    fn length(&self) -> f64 {
        self.end - self.start
    }

    fn breadth(&self) -> f64 {
        self.band[1] - self.band[0]
    }

    /// A point of the content, `t` along and `c` across, in the turned camera.
    fn point(&self, t: f64, c: f64) -> [f64; 2] {
        if self.column { [c, t] } else { [t, c] }
    }
}

/// The [`Layout`] of `fits` in the camera turned by `m` (`turned` names the turn, for messages).
fn layout(fits: &[Fitted], m: &[[f64; 2]; 2], turned: &str) -> CmdResult<Layout> {
    // Edges in the turned camera: left, right, top, bottom of each frame.
    let edges: Vec<[f64; 4]> = fits
        .iter()
        .map(|f| {
            let [tl, tr, br, bl] = f.frame.map(|p| turn(m, p));
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
    Ok(Layout { edges, column, order, lo, hi, start, end, band })
}

/// What the layout warns of: gaps between neighbours, fits too rough for a flat surface.
fn layout_warnings(fits: &[Fitted], layout: &Layout) -> CmdResult<Vec<String>> {
    let mut warnings: Vec<String> = Vec::new();
    for pair in layout.order.windows(2) {
        if let [x, y] = *pair
            && layout.along(x)?[1] <= layout.along(y)?[0]
        {
            warnings.push(format!(
                "{} and {} do not overlap: the content has a gap between them",
                fits.get(x).map(|f| f.scan.display().to_string()).unwrap_or_default(),
                fits.get(y).map(|f| f.scan.display().to_string()).unwrap_or_default()
            ));
        }
    }
    for f in fits {
        if f.error_px[0] > 3.0 {
            warnings.push(format!(
                "{}: the corners line the projectors up only roughly. The homography fits with a median \
                 error of {} projector px: the surface is not flat, or the scan is noisy",
                f.scan.display(),
                f.error_px[0]
            ));
        }
    }
    Ok(warnings)
}

/// A projector's share of the content: its mapper's `region` and its `corner_*` offsets.
struct Placed {
    region: [f64; 4],
    corners: [[f64; 2]; 4],
}

/// Projector `k`'s (fit `f`) share of the content of `layout`, in the camera turned by `m`.
fn place(k: usize, f: &Fitted, layout: &Layout, m: &[[f64; 2]; 2]) -> CmdResult<Placed> {
    let (start, length) = (layout.start, layout.length());
    let own = layout.along(k)?;
    let u0 = if k == layout.lo { 0.0 } else { ((own[0] - start) / length).clamp(0.0, 1.0) };
    let u1 = if k == layout.hi { 1.0 } else { ((own[1] - start) / length).clamp(0.0, 1.0) };
    if u1 <= u0 {
        return Err(format!("{}: this projector has no share of the content", f.scan.display()).into());
    }
    let (t0, t1) = (start + u0 * length, start + u1 * length);
    let (c0, c1) = (layout.band[0], layout.band[1]);
    let point = |t: f64, c: f64| layout.point(t, c);
    let share = if layout.column {
        [point(t0, c0), point(t0, c1), point(t1, c1), point(t1, c0)]
    } else {
        [point(t0, c0), point(t1, c0), point(t1, c1), point(t0, c1)]
    };
    let mut corners = [[0.0; 2]; 4];
    for (c, (p, base)) in corners.iter_mut().zip(share.into_iter().zip(UNIT)) {
        let q = f.h.apply(unturn(m, p)).ok_or("a corner maps to infinity")?;
        *c = [round(q[0] - base[0]), round(q[1] - base[1])];
    }
    let region = if layout.column {
        [0.0, round(u0), 1.0, round(u1 - u0)]
    } else {
        [round(u0), 0.0, round(u1 - u0), 1.0]
    };
    Ok(Placed { region, corners })
}

/// The edits that place mapper `path`'s picture: `region`, fit stretch, the corners.
fn mapper_ops(path: &NodePath, placed: &Placed) -> CmdResult<Vec<Op>> {
    let set = |name: &str, value: Value| -> CmdResult<Op> {
        Ok(Op::Set { param: ParamPath::new(path.clone(), Name::new(name)?)?, value })
    };
    let mut ops =
        vec![set("region", Value::Vec4(placed.region))?, set("fit", Value::Text("stretch".into()))?];
    for (name, c) in CORNERS.iter().zip(placed.corners) {
        ops.push(set(name, Value::Vec2(c))?);
    }
    Ok(ops)
}

/// The content's bounding box in the camera's own axes: x, y, x, y.
fn camera_rect(layout: &Layout, m: &[[f64; 2]; 2]) -> [f64; 4] {
    let (start, end, band) = (layout.start, layout.end, layout.band);
    let back: Vec<[f64; 2]> = [
        layout.point(start, band[0]),
        layout.point(end, band[0]),
        layout.point(end, band[1]),
        layout.point(start, band[1]),
    ]
    .iter()
    .map(|p| unturn(m, *p))
    .collect();
    [
        back.iter().map(|p| p[0]).fold(f64::MAX, f64::min).round(),
        back.iter().map(|p| p[1]).fold(f64::MAX, f64::min).round(),
        back.iter().map(|p| p[0]).fold(f64::MIN, f64::max).round(),
        back.iter().map(|p| p[1]).fold(f64::MIN, f64::max).round(),
    ]
}

pub fn align(e: &Engine, a: &AlignArgs) -> CmdResult<Json> {
    align_as(e, a, Actor::Claude)
}

/// [`align`], the mappers' edit made as `actor` (the user's scan button edits as the user). In
/// stages: the lists checked; a fit per scan ([`fit`]); which way is up ([`orient`]); the content
/// ([`layout`]); per projector its share ([`place`]) and its mapper's edit; the edit made in the
/// project the mappers were read from.
pub fn align_as(e: &Engine, a: &AlignArgs, actor: Actor) -> CmdResult<Json> {
    let started = Instant::now();
    check_lists(a)?;
    let session = e.session();
    let net = session.doc().snapshot();
    if let Some(mappers) = &a.mappers {
        check_mappers(&net, mappers)?;
    }
    let dir = session.dir();
    let fits: Vec<Fitted> = a.scans.iter().map(|s| fit(dir, s)).collect::<CmdResult<_>>()?;
    let fitted = started.elapsed();

    let (m, turned) = orient(&fits)?;
    let layout = layout(&fits, &m, turned)?;
    let mut warnings = layout_warnings(&fits, &layout)?;

    let mapper_desc = e.types().desc(MAPPER);
    let mut ops = Vec::new();
    let mut report = Vec::new();
    for (k, f) in fits.iter().enumerate() {
        let placed = place(k, f, &layout, &m)?;
        let corners = placed.corners;
        if corners.iter().flatten().any(|v| v.abs() > 0.5) {
            warnings.push(format!(
                "{}: a corner moves by more than half the picture; is the camera square-on and the scan right?",
                f.scan.display()
            ));
        }
        let mapper = a.mappers.as_ref().and_then(|m| m.get(k));
        if let Some(path) = mapper {
            ops.extend(mapper_ops(path, &placed)?);
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
            "place": layout.order.iter().position(|o| *o == k),
            "projector": f.projector,
            "points": f.points,
            "kept": f.kept,
            "error_px": {"median": f.error_px[0], "p90": f.error_px[1], "p99": f.error_px[2]},
            "frame_in_camera": f.frame.map(|p| p.map(|v| v.round())),
            "region": placed.region,
            "corners": corner_json,
        }));
    }
    let apply = a.apply.unwrap_or(true) && a.mappers.is_some();
    let history = if apply {
        let out = session.edit(actor, &ops, None, "scan.align", Instant::now()).map_err(|e| e.0)?;
        Some(outcome(&out))
    } else {
        None
    };
    let (length, breadth) = (layout.length(), layout.breadth());
    let (width, height) = if layout.column { (breadth, length) } else { (length, breadth) };
    let aspect = width / height;
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
            "camera_rect": camera_rect(&layout, &m),
            "aspect": (aspect * 1e4).round() / 1e4,
            "camera_turned": turned,
            "layout": if layout.column { "column" } else { "row" },
        },
        "applied": apply,
        "history": history,
        "warnings": warnings,
        "ms": {"fit": ms(fitted), "total": ms(started.elapsed())},
        "next": "content of that aspect (e.g. tex.* with width = height x aspect) into the mappers; the mapper pattern param shows the grid",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CW: usize = 120;
    const CH: usize = 60;

    /// The fit of a synthetic scan: the projector's frame is the camera rectangle `x0..x1`,
    /// `y0..y1` (camera px), upright; camera pixels outside it are not decoded.
    fn fitted(name: &str, [x0, y0, x1, y1]: [f64; 4]) -> Fitted {
        let mut rgba = Vec::with_capacity(CW * CH);
        for y in 0..CH {
            for x in 0..CW {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let (u, v) = ((px - x0) / (x1 - x0), (py - y0) / (y1 - y0));
                let inside = (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v);
                rgba.push(if inside { [u as f32, v as f32, 1.0, 1.0] } else { [-1.0, -1.0, 0.0, 0.0] });
            }
        }
        let cam = Map::new(CW, CH, rgba).unwrap();
        let proj = Map::new(64, 32, vec![[0.0; 4]; 64 * 32]).unwrap();
        fit_maps(Path::new(name), Path::new(name), &cam, &proj).unwrap()
    }

    fn near(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn turns_and_their_inverses() {
        for (m, _) in TURNS {
            let p = [3.0, -2.0];
            assert_eq!(unturn(&m, turn(&m, p)), p);
        }
        assert_eq!(turn(&TURNS[1].0, [1.0, 0.0]), [0.0, -1.0]);
        assert_eq!(round(-0.000_001), 0.0);
        assert!(round(-0.000_001).is_sign_positive());
        assert_eq!(round(0.123_456_7), 0.123_46);
    }

    #[test]
    fn percentiles_of_sorted_values() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.9), 5.0);
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert!(percentile(&[], 0.5).is_nan());
    }

    #[test]
    fn a_fit_finds_the_projectors_frame_in_the_camera() {
        let f = fitted("a", [10.0, 5.0, 70.0, 55.0]);
        let want = [[10.0, 5.0], [70.0, 5.0], [70.0, 55.0], [10.0, 55.0]];
        for (got, want) in f.frame.iter().zip(want) {
            assert!(near(*got, want), "{:?}", f.frame);
        }
        assert_eq!(f.points, 60 * 50);
        assert_eq!(f.projector, [64, 32]);
        assert!(f.error_px[0] < 0.01, "{:?}", f.error_px);
        // Too few decoded pixels.
        let cam = Map::new(4, 4, vec![[0.5, 0.5, 1.0, 1.0]; 16]).unwrap();
        let err = fit_maps(Path::new("s"), Path::new("s"), &cam, &cam).err().unwrap();
        assert!(err.0.contains("only 16 decoded camera pixels"), "{}", err.0);
    }

    #[test]
    fn two_projectors_side_by_side_share_the_content() {
        let fits = [fitted("a", [10.0, 5.0, 70.0, 55.0]), fitted("b", [50.0, 15.0, 110.0, 55.0])];
        let (m, turned) = orient(&fits).unwrap();
        assert_eq!(turned, "none");
        let layout = layout(&fits, &m, turned).unwrap();
        assert!(!layout.column);
        assert_eq!(layout.order, [0, 1]);
        assert!((layout.start - 10.0).abs() < 1e-3 && (layout.end - 110.0).abs() < 1e-3);
        assert!((layout.band[0] - 15.0).abs() < 1e-3 && (layout.band[1] - 55.0).abs() < 1e-3);
        assert!(layout_warnings(&fits, &layout).unwrap().is_empty());
        assert_eq!(camera_rect(&layout, &m), [10.0, 15.0, 110.0, 55.0]);

        // The first shows 0..0.6 of the content, its top corners moved down to the band.
        let a = place(0, &fits[0], &layout, &m).unwrap();
        assert_eq!(a.region, [0.0, 0.0, 0.6, 1.0]);
        for (got, want) in a.corners.iter().zip([[0.0, 0.2], [0.0, 0.2], [0.0, 0.0], [0.0, 0.0]]) {
            assert!(near(*got, want), "{:?}", a.corners);
        }
        // The second 0.4..1, all of its own frame.
        let b = place(1, &fits[1], &layout, &m).unwrap();
        assert_eq!(b.region, [0.4, 0.0, 0.6, 1.0]);
        assert!(b.corners.iter().all(|c| near(*c, [0.0, 0.0])), "{:?}", b.corners);

        let path = NodePath::parse("/m").unwrap();
        let ops = mapper_ops(&path, &b).unwrap();
        assert_eq!(ops.len(), 2 + CORNERS.len());
    }

    #[test]
    fn a_column_a_gap_and_frames_that_do_not_meet() {
        let fits = [fitted("a", [10.0, 0.0, 60.0, 25.0]), fitted("b", [12.0, 35.0, 58.0, 60.0])];
        let (m, turned) = orient(&fits).unwrap();
        let laid = layout(&fits, &m, turned).unwrap();
        assert!(laid.column);
        let w = layout_warnings(&fits, &laid).unwrap();
        assert_eq!(w, ["a and b do not overlap: the content has a gap between them"]);
        let b = place(1, &fits[1], &laid, &m).unwrap();
        assert_eq!(b.region[0], 0.0);
        assert_eq!(b.region[2], 1.0);

        // Side by side but one above the other's band: no rectangle in common.
        let apart = [fitted("a", [0.0, 0.0, 50.0, 20.0]), fitted("b", [60.0, 30.0, 110.0, 50.0])];
        let (m, turned) = orient(&apart).unwrap();
        let err = layout(&apart, &m, turned).err().unwrap();
        assert!(err.0.starts_with("the projectors' frames share no rectangle"), "{}", err.0);
    }

    #[test]
    fn a_turned_camera_and_a_frame_turned_against_the_first() {
        let mut f = fitted("a", [10.0, 5.0, 70.0, 55.0]);
        // The camera turned a quarter: the frame's top edge points up the picture.
        f.frame = [[10.0, 55.0], [10.0, 5.0], [60.0, 5.0], [60.0, 55.0]];
        let (_, turned) = orient(std::slice::from_ref(&f)).unwrap();
        assert_eq!(turned, "a quarter counter-clockwise");
        let upright = fitted("b", [10.0, 5.0, 70.0, 55.0]);
        let err = orient(&[f, upright]).err().unwrap();
        assert!(err.0.starts_with("b: this projector's frame is turned"), "{}", err.0);
    }

    #[test]
    fn the_lists_are_checked() {
        let args = |v: Json| -> AlignArgs { serde_json::from_value(v).unwrap() };
        assert!(check_lists(&args(json!({"scans": ["a"]}))).is_ok());
        assert!(check_lists(&args(json!({"scans": []}))).is_err());
        let err = check_lists(&args(json!({"scans": ["a", "b"], "mappers": ["/m"]}))).unwrap_err();
        assert!(err.0.contains("one tex.mapper per scan"));
    }
}
