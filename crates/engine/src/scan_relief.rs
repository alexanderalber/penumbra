//! `scan.relief`: height maps of a mostly flat object (a relief panel) from its scans, without
//! a scan of the empty scene. The math is [`penumbra_calib::relief`]; this reads the scans,
//! brings every projector's heights into the first one's units and writes the files.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::objects::{self, Map};
use penumbra_calib::relief::{self, Heuristic, Relief, ReliefOptions, View};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan::{CAMERA_MAP, PROJECTOR_MAP, ms, read_map};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Where `scan.relief` writes unless told otherwise (relative to the project).
pub const DEFAULT_RELIEF_OUT: &str = "relief";
pub const HEIGHT_CAMERA: &str = "height_camera.exr";
pub const RELIEF_VIEW: &str = "relief_view.png";
/// The heights are normalised over these quantiles of all lit pixels.
const RANGE: [f64; 2] = [0.01, 0.995];
/// An overlap correlation below this is reported.
const WEAK_CORRELATION: f64 = 0.8;
/// A panel holding less than this share of the measured pixels is reported.
const SMALL_PIECE: f64 = 0.3;

pub fn height_file(n: usize) -> String {
    format!("height_{n}.exr")
}
pub fn mask_file(n: usize) -> String {
    format!("mask_{n}.png")
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.relief",
        "Height maps of a mostly flat object (a relief panel) from its scans (folders scan.capture or \
         scan.decode wrote), no reference scan needed: per projector a robust plane projector -> camera, \
         the residual along its main direction as the height, the panel cut out at jumps, holes filled; \
         every projector brought to the first one's units over the camera pixels both light (symmetric \
         fit); writes height_N.exr (0 to 1 over the heights' 1st to 99.5th percentile, for tex.lookup), \
         mask_N.png (the light mask, for tex.image or scan.blend's masks), height_camera.exr and \
         relief_view.png (the same in camera space, to look at); {scans: [folder, ...], out? (folder, \
         relief), sign? (\"auto\" | 1 | -1, the first projector's, the others follow it by the overlap: auto is a \
         heuristic, the wall around the panel taken as behind it; 1 and -1 are fixed to the camera's \
         axes, the reply's sign.as_param pins what a run found), scale? (\"symmetric\" | \"least_squares\"), jump? (camera px per \
         projector px, 4), erode? (projector px, 4), reach? (projector px the light may go past the \
         measured panel, 60)}",
        move |p| {
            let a: ReliefArgs = args(p)?;
            run(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReliefArgs {
    pub scans: Vec<PathBuf>,
    pub out: Option<PathBuf>,
    pub sign: Option<Json>,
    pub scale: Option<ScaleKind>,
    pub jump: Option<f32>,
    pub erode: Option<u32>,
    pub reach: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleKind {
    #[default]
    Symmetric,
    LeastSquares,
}

impl ScaleKind {
    fn name(self) -> &'static str {
        match self {
            ScaleKind::Symmetric => "symmetric",
            ScaleKind::LeastSquares => "least_squares",
        }
    }
}

/// `None` for auto, else the sign asked for.
fn sign(v: Option<&Json>) -> CmdResult<Option<f32>> {
    match v {
        None => Ok(None),
        Some(Json::String(s)) if s == "auto" => Ok(None),
        Some(Json::Number(n)) if n.as_f64() == Some(1.0) => Ok(Some(1.0)),
        Some(Json::Number(n)) if n.as_f64() == Some(-1.0) => Ok(Some(-1.0)),
        Some(other) => Err(CommandError(format!("`sign` is \"auto\", 1 or -1, not {other}"))),
    }
}

fn heuristic_json(h: &Heuristic) -> Json {
    match h {
        Heuristic::Wall { pixels, median } => {
            json!({"by": h.name(), "wall_pixels": pixels, "wall_median": median.abs()})
        }
        Heuristic::Skew { skew } => json!({"by": h.name(), "skew": skew.abs()}),
    }
}

/// Runs `scan.relief` over the scans of `a` (paths relative to `dir`).
pub fn run(dir: &Path, a: &ReliefArgs) -> CmdResult<Json> {
    let started = Instant::now();
    if a.scans.is_empty() || a.scans.len() > objects::MAX_PROJECTORS {
        return Err(CommandError(format!("`scans` lists 1 to {} folders", objects::MAX_PROJECTORS)));
    }
    let defaults = ReliefOptions::default();
    let o = ReliefOptions {
        jump: a.jump.unwrap_or(defaults.jump),
        erode: a.erode.unwrap_or(defaults.erode),
        reach: a.reach.unwrap_or(defaults.reach),
    };
    o.check().map_err(CommandError)?;
    let asked = sign(a.sign.as_ref())?;
    let kind = a.scale.unwrap_or_default();

    let mut to_camera: Vec<Map> = Vec::new();
    let mut from_camera: Vec<Map> = Vec::new();
    let mut camera: Option<(usize, usize)> = None;
    for (n, folder) in a.scans.iter().enumerate() {
        let folder = dir.join(folder);
        let p2c = read_map(&folder.join(PROJECTOR_MAP))?;
        let c2p = read_map(&folder.join(CAMERA_MAP))?;
        let size = (c2p.width, c2p.height);
        match camera {
            None => camera = Some(size),
            Some(c) if c != size => {
                return Err(CommandError(format!(
                    "scan {n}'s camera picture is {} x {}, scan 0's {} x {}: one camera for all",
                    size.0, size.1, c.0, c.1
                )));
            }
            Some(_) => {}
        }
        to_camera.push(p2c);
        from_camera.push(c2p);
    }
    let camera = camera.ok_or("no scans")?;
    let read = started.elapsed();

    let t = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut reliefs: Vec<Relief> = Vec::new();
    for ((map, cam), folder) in to_camera.iter().zip(&from_camera).zip(&a.scans) {
        let r = relief::analyse(map, cam, &o)
            .map_err(|e| CommandError(format!("{}: {e}", dir.join(folder).display())))?;
        reliefs.push(r);
    }
    // The first projector's sign as asked (fixed to the camera's axes, see `fixed_sign`); the
    // others follow it through the overlap.
    if let (Some(s), Some(first)) = (asked, reliefs.first_mut())
        && fixed_sign(first.direction) != s
    {
        first.flip();
    }
    let mut converted: Vec<Vec<f32>> = Vec::new();
    let mut scales: Vec<Json> = Vec::new();
    for n in 0..reliefs.len() {
        if n == 0 {
            converted.push(reliefs.first().map(|r| r.heights.clone()).unwrap_or_default());
            scales.push(Json::Null);
            continue;
        }
        let (x, y) = {
            let (Some(r), Some(c)) = (reliefs.get(n), from_camera.get(n)) else { continue };
            let this =
                View { from_camera: c, heights: &r.heights, mask: &r.mask, width: r.width, height: r.height };
            let earlier: Vec<View<'_>> = (0..n)
                .filter_map(|k| {
                    let e = reliefs.get(k)?;
                    Some(View {
                        from_camera: from_camera.get(k)?,
                        heights: converted.get(k)?,
                        mask: &e.mask,
                        width: e.width,
                        height: e.height,
                    })
                })
                .collect();
            relief::overlap(&this, &earlier)
        };
        let overlap = x.len();
        let Some(r) = reliefs.get_mut(n) else { continue };
        match relief::scale_fit(&x, &y, kind == ScaleKind::Symmetric) {
            Some(f) => {
                if f.correlation < WEAK_CORRELATION {
                    warnings.push(format!(
                        "projector {n}: its heights and the others' correlate only {:.2} over the {overlap} camera pixels both light; its scale is doubtful",
                        f.correlation
                    ));
                }
                // A negative slope: this projector's sign heuristic disagrees with the ones before.
                let flips = f.scale < 0.0;
                if flips {
                    r.flip();
                    warnings.push(format!(
                        "projector {n}: its own sign heuristic disagrees with the overlap with the projectors before; the overlap wins"
                    ));
                }
                let k = f.scale.abs();
                converted.push(r.heights.iter().map(|h| (f64::from(*h) * k + f.offset) as f32).collect());
                scales.push(json!({
                    "scale": round(k), "offset": round(f.offset), "fit": kind.name(),
                    "symmetric": round(f.symmetric.abs()), "least_squares": round(f.least_squares.abs()),
                    "overlap": overlap, "kept": f.kept, "correlation": round(f.correlation.abs()),
                    "sign_from": "the overlap",
                }));
            }
            None => {
                warnings.push(format!(
                    "projector {n} shares only {overlap} camera pixels with the projectors before (at least {}): its heights stay in its own units and sign",
                    relief::MIN_OVERLAP
                ));
                converted.push(r.heights.clone());
                scales.push(json!({"scale": null, "overlap": overlap, "sign_from": "heuristic"}));
            }
        }
    }

    let lit: Vec<f32> = converted
        .iter()
        .zip(&reliefs)
        .flat_map(|(h, r)| h.iter().zip(&r.mask).filter(|(_, m)| **m).map(|(h, _)| *h))
        .collect();
    let [lo, hi] = relief::range(&lit, RANGE[0], RANGE[1])
        .ok_or("no projector lights its panel: every mask is empty")?;
    let span = if hi > lo { hi - lo } else { 1.0 };
    let norm = |h: f32| ((h - lo) / span).clamp(0.0, 1.0);
    let views: Vec<View<'_>> = converted
        .iter()
        .zip(&reliefs)
        .zip(&from_camera)
        .map(|((h, r), c)| View {
            from_camera: c,
            heights: h,
            mask: &r.mask,
            width: r.width,
            height: r.height,
        })
        .collect();
    let cam_heights = relief::camera_heights(&views, camera);
    let work = t.elapsed();

    let t = Instant::now();
    let out = dir.join(a.out.as_deref().unwrap_or(Path::new(DEFAULT_RELIEF_OUT)));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| -> CmdResult<String> {
        let file = out.join(name);
        std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(file.display().to_string())
    };
    let mut per_projector = Vec::new();
    for (n, ((r, h), scale)) in reliefs.iter().zip(&converted).zip(&scales).enumerate() {
        let (w, ht) = (r.width as u32, r.height as u32);
        let rgba: Vec<[f32; 4]> = h.iter().map(|v| norm(*v)).map(|v| [v, v, v, 1.0]).collect();
        let height = write(&height_file(n), penumbra_gpu::image::encode_exr_rgba32f(w, ht, &rgba)?)?;
        let bytes: Vec<u8> = r.mask.iter().map(|m| if *m { 255 } else { 0 }).collect();
        let mask = write(&mask_file(n), penumbra_gpu::image::encode_png_gray8(w, ht, bytes)?)?;
        let area = r.mask.iter().filter(|m| **m).count();
        let piece = r.piece.iter().filter(|m| **m).count();
        let share = r.piece_measured as f64 / r.measured.max(1) as f64;
        if share < SMALL_PIECE {
            warnings.push(format!(
                "projector {n}: the panel holds only {:.0} % of the measured pixels; does the object fill most of the frame, is `jump` too low?",
                share * 100.0
            ));
        }
        if area == 0 {
            warnings.push(format!("projector {n}: its light mask is empty"));
        }
        let own: Vec<f32> = r.heights.iter().zip(&r.mask).filter(|(_, m)| **m).map(|(h, _)| *h).collect();
        let own_range = relief::range(&own, RANGE[0], RANGE[1]);
        let sign_from = if n == 0 {
            match asked {
                Some(_) => json!("given"),
                None => json!("heuristic"),
            }
        } else {
            scale.get("sign_from").cloned().unwrap_or_else(|| json!("heuristic"))
        };
        per_projector.push(json!({
            "scan": a.scans.get(n).map(|s| s.display().to_string()),
            "size": [w, ht],
            "measured": r.measured,
            "panel": {"pixels": piece, "measured": r.piece_measured, "share_of_measured": round(share)},
            "relief_area": {"pixels": area, "share_of_frame": round(area as f64 / r.mask.len().max(1) as f64)},
            "fit_error_px": {"median": round(r.error_px[0]), "p90": round(r.error_px[1])},
            "direction": [round(r.direction[0]), round(r.direction[1])],
            "sign": {"from": sign_from, "as_param": fixed_sign(r.direction), "heuristic": heuristic_json(&r.heuristic), "flipped_from_heuristic": r.flipped},
            "height_range_own": own_range.map(|[a, b]| json!([round(f64::from(a)), round(f64::from(b))])),
            "to_first": scale,
            "height": height,
            "mask": mask,
        }));
    }
    let (cw, ch) = (camera.0 as u32, camera.1 as u32);
    let cam_rgba: Vec<[f32; 4]> = cam_heights
        .iter()
        .map(|h| match h {
            Some(v) => {
                let v = norm(*v);
                [v, v, v, 1.0]
            }
            None => [0.0, 0.0, 0.0, 0.0],
        })
        .collect();
    let camera_file = write(HEIGHT_CAMERA, penumbra_gpu::image::encode_exr_rgba32f(cw, ch, &cam_rgba)?)?;
    // Grey by height (low dark), black where no projector lights the panel.
    let mut view = Vec::with_capacity(cam_heights.len() * 4);
    for h in &cam_heights {
        let g = h.map_or(0, |v| (20.0 + 235.0 * norm(v)).round() as u8);
        view.extend_from_slice(&[g, g, g, 255]);
    }
    let view_file = write(RELIEF_VIEW, penumbra_gpu::encode_png_rgba8(cw, ch, view)?)?;
    let written = t.elapsed();
    let seen = cam_heights.iter().filter(|h| h.is_some()).count();

    Ok(json!({
        "message": format!(
            "{} projector(s); heights {:.1} to {:.1} camera px of parallax (projector 0's units) as 0 to 1; {seen} camera pixels on the relief",
            reliefs.len(), lo, hi
        ),
        "camera": [cw, ch],
        "range": {"low": round(f64::from(lo)), "high": round(f64::from(hi)), "units": "camera px of parallax, projector 0's", "quantiles": RANGE},
        "projectors": per_projector,
        "files": {"camera": camera_file, "view": view_file},
        "options": {"jump": o.jump, "erode": o.erode, "reach": o.reach, "sign": asked.map_or(json!("auto"), |s| json!(s)), "scale": kind.name()},
        "ms": {"read": ms(read), "work": ms(work), "write": ms(written), "total": ms(started.elapsed())},
        "warnings": warnings,
        "next": "per projector: tex.image of height_N.exr into tex.lookup's in, a tex.ramp as its lut (the colours by \
                 height); tex.composite multiply with tex.image of mask_N.png (or scan.blend with the masks, and its \
                 weight_N.png); into out.projector",
    }))
}

/// The `sign` param's value that gives heights growing along `direction`: 1 when it points to
/// the camera picture's right (or, nearly vertical, down), -1 otherwise. A fixed convention, so
/// a sign once found right stays right while camera and projector stay where they are.
fn fixed_sign(direction: [f64; 2]) -> f32 {
    if direction[0] + 0.01 * direction[1] >= 0.0 { 1.0 } else { -1.0 }
}

fn round(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}
