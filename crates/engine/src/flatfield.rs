//! `projector.flatfield`: measures how brightness and colour vary across each projector's
//! picture, and the black where projectors overlap, with a camera; writes per projector a gain
//! map and a black lift as pictures for `out.projector`'s `gain` and `black` inputs. The math is
//! in [`penumbra_calib::flatfield`].
//!
//! It shows flat signals through the output board like `projector.match` (the network is not
//! changed while it measures, and the projectors' own corrections are bypassed): all black,
//! each projector's white alone, then each primary at a few levels. Camera pixels are taken to
//! projector pixels by the projector's scan (`cam_to_proj.exr` for the gain's samples,
//! `proj_to_cam.exr` for the black lift), so the scans must fit the current setup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_calib::colour::{self, Picture};
use penumbra_calib::flatfield::{self, Surface};
use penumbra_calib::objects::Map;
use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::{Network, Op};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{edit_as, outcome};
use crate::engine::Engine;
use crate::jobs::Job;
use crate::projector::{CameraStream, Claim, Rig, projector_list, settle_of, timeout_of};
use crate::scan::{CAMERA_MAP, PROJECTOR_MAP, read_map};
use penumbra_project::Actor;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub const DEFAULT_LEVELS: u32 = 5;
pub const DEFAULT_LOW: f64 = 0.6;
pub const DEFAULT_FRAMES: usize = 4;
pub const DEFAULT_DEGREE: usize = 3;
pub const DEFAULT_TRIM: f64 = 0.25;
pub const DEFAULT_FLOOR: f64 = 0.02;
pub const DEFAULT_COVER: f32 = 0.1;
pub const DEFAULT_BLUR: f32 = 6.0;
pub const DEFAULT_OUT: &str = "flatfield";
/// Most camera samples per projector the surface is fitted to.
const MAX_SAMPLES: usize = 80_000;
/// Camera pixels dropped at the edge of a projector's lit area (half lit, or the light of a
/// neighbour pixel spilling over).
const EDGE: usize = 2;
/// Above this distance of the kept samples from the fit (in logs) the surface is not even.
const UNEVEN: f64 = 0.05;
/// The image node kind the maps are read by.
const IMAGE: &str = "tex.image_file";

/// Files per projector, by the projector node's name.
pub fn gain_file(name: &str) -> String {
    format!("gain_{name}.png")
}
pub fn black_file(name: &str) -> String {
    format!("black_{name}.png")
}
/// What was measured before the fit, in the camera's picture: to compare with the gain map.
pub fn measured_file(name: &str) -> String {
    format!("measured_{name}.png")
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "projector.flatfield",
        "Measure with a camera how brightness and colour vary across each projector's picture (hot spot, \
         a colour cast towards an edge), and the brighter black where projectors overlap. Writes per \
         projector a gain map and a black lift (gain_NAME.png, black_NAME.png) and connects them to \
         out.projector's gain and black inputs. Blocks for about 20 s per projector; {camera \
         (tex.video_in path), projectors (out.projector paths), scans (one scan folder per projector, \
         with cam_to_proj.exr and proj_to_cam.exr, relative to the project; they must fit the current \
         setup), even? (all: brightness and colour, measured on a flat evenly coloured surface | colour: \
         the colour cast only, which a relief's shading does not change), levels? (drive levels per \
         primary, 5), low? (lowest level, sRGB-encoded drive, 0.6), frames? (camera frames averaged per \
         picture, 4), settle? (s, 0.5), timeout? (s, 5), degree? (of the smooth surface fitted over the \
         projector, 3), trim? (share of samples the fit may ignore, the surface's own pattern, 0.25), \
         floor? (share of the picture allowed to stay dimmer than the target, 0.02), cover? (share of a \
         projector's brightest that counts as lit for the black, 0.1), blur? (of the black lift, \
         projector px, 6), contrast? (projector contrast, black = white / contrast, where the camera \
         cannot read the black), out? (folder in the project, flatfield), apply? (true: connect the \
         maps, one undo step), pictures? (false: also keep the camera's pictures, 16-bit sRGB PNGs, in \
         out/pictures), wait? (false: start and answer at once; job.status follows it; true)}",
        move |p| {
            let a: FlatfieldArgs = args(p)?;
            flatfield_job(&e, a, Actor::Claude)
        },
    ));
}

/// What the gain evens out.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Even {
    /// Brightness and colour: for a flat, evenly coloured surface.
    #[default]
    All,
    /// The colour only (the balance of red, green and blue), which a surface's shading does not
    /// change: for a relief or objects, where brightness also varies with the surface's slope.
    Colour,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FlatfieldArgs {
    pub camera: NodePath,
    pub projectors: Vec<NodePath>,
    pub scans: Vec<PathBuf>,
    pub levels: Option<u32>,
    pub low: Option<f64>,
    pub frames: Option<usize>,
    pub settle: Option<f64>,
    pub timeout: Option<f64>,
    pub degree: Option<usize>,
    pub trim: Option<f64>,
    pub floor: Option<f64>,
    pub cover: Option<f32>,
    pub blur: Option<f32>,
    pub contrast: Option<f64>,
    pub out: Option<PathBuf>,
    pub apply: Option<bool>,
    pub pictures: Option<bool>,
    #[serde(default)]
    pub even: Even,
    /// Answer when done (the default), or start and answer at once (`job.status` follows it).
    pub wait: Option<bool>,
}

fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    if v.is_finite() { (v * f).round() / f } else { 0.0 }
}

/// The projector uv the camera pixel `i` of a `w` x `h` picture sees, by a `cam_to_proj` map.
fn projector_uv(map: &Map, i: usize, w: usize, h: usize) -> Option<[f32; 2]> {
    let (x, y) = (i % w.max(1), i / w.max(1));
    let mx = (((x as f64 + 0.5) / w as f64) * map.width as f64) as usize;
    let my = (((y as f64 + 0.5) / h as f64) * map.height as f64) as usize;
    map.uv(my.min(map.height.saturating_sub(1)) * map.width + mx.min(map.width.saturating_sub(1)))
}

/// Values 0..1 as a 16-bit RGB PNG, sRGB-encoded as `tex.image_file` decodes such files.
fn png16(w: usize, h: usize, values: &[[f32; 3]]) -> CmdResult<Vec<u8>> {
    let encode = |v: f32| {
        (penumbra_gpu::readback::linear_to_srgb(v.clamp(0.0, 1.0)) * 65535.0).round().clamp(0.0, 65535.0)
            as u16
    };
    let flat: Vec<u16> = values.iter().flat_map(|p| p.map(encode)).collect();
    Ok(penumbra_gpu::image::encode_png_rgb16(w as u32, h as u32, &flat)?)
}

fn stats(values: &[[f32; 3]]) -> Json {
    let per = |c: usize| {
        let v: Vec<f64> = values.iter().filter_map(|p| p.get(c)).map(|x| f64::from(*x)).collect();
        let q = |x: f64| round(flatfield::quantile(&v, x).unwrap_or(0.0), 4);
        json!({"min": q(0.0), "p05": q(0.05), "median": q(0.5), "p95": q(0.95), "max": q(1.0)})
    };
    json!({"r": per(0), "g": per(1), "b": per(2)})
}

/// `projector.flatfield`'s numeric arguments, checked, the defaults filled in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    levels: usize,
    low: f64,
    frames: usize,
    settle: f64,
    timeout: f64,
    fit: GainFit,
    floor: f64,
    cover: f32,
    blur: f32,
}

/// How the gain is fitted over a projector.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GainFit {
    degree: usize,
    trim: f64,
    even: Even,
}

fn settings(a: &FlatfieldArgs) -> CmdResult<Settings> {
    let levels = a.levels.unwrap_or(DEFAULT_LEVELS) as usize;
    if !(2..=flatfield::MAX_LEVELS).contains(&levels) {
        return Err(format!("`levels` is 2 to {}", flatfield::MAX_LEVELS).into());
    }
    let low = a.low.unwrap_or(DEFAULT_LOW);
    if !(low.is_finite() && (0.2..=0.95).contains(&low)) {
        return Err("`low` is 0.2 to 0.95 (sRGB-encoded drive)".into());
    }
    let frames = a.frames.unwrap_or(DEFAULT_FRAMES);
    if !(1..=32).contains(&frames) {
        return Err("`frames` is 1 to 32".into());
    }
    let settle = settle_of(a.settle, crate::projector::DEFAULT_SETTLE)?;
    let timeout = timeout_of(a.timeout)?;
    let degree = a.degree.unwrap_or(DEFAULT_DEGREE);
    if degree > flatfield::MAX_DEGREE {
        return Err(format!("`degree` is 0 to {}", flatfield::MAX_DEGREE).into());
    }
    let trim = a.trim.unwrap_or(DEFAULT_TRIM);
    if !(trim.is_finite() && (0.0..=0.6).contains(&trim)) {
        return Err("`trim` is 0 to 0.6".into());
    }
    let floor = a.floor.unwrap_or(DEFAULT_FLOOR);
    if !(floor.is_finite() && (0.0..=0.3).contains(&floor)) {
        return Err("`floor` is 0 to 0.3".into());
    }
    let cover = a.cover.unwrap_or(DEFAULT_COVER);
    if !(cover.is_finite() && (0.01..=0.9).contains(&cover)) {
        return Err("`cover` is 0.01 to 0.9".into());
    }
    let blur = a.blur.unwrap_or(DEFAULT_BLUR);
    if !(blur.is_finite() && (0.0..=64.0).contains(&blur)) {
        return Err("`blur` is 0 to 64 projector pixels".into());
    }
    if let Some(c) = a.contrast
        && !(c.is_finite() && (10.0..=1e6).contains(&c))
    {
        return Err("`contrast` is 10 to 1000000".into());
    }
    if a.projectors.is_empty() || a.projectors.len() != a.scans.len() {
        return Err("`projectors` and `scans` list one scan folder per projector".into());
    }
    let fit = GainFit { degree, trim, even: a.even };
    Ok(Settings { levels, low, frames, settle, timeout, fit, floor, cover, blur })
}

/// The drives each primary is shown at: `levels` from `low` to full, evenly.
fn drives(levels: usize, low: f64) -> Vec<f64> {
    (0..levels).map(|l| low + (1.0 - low) * l as f64 / (levels - 1).max(1) as f64).collect()
}

/// The camera's pictures: all black, each projector's white alone, and per projector, channel
/// and drive that primary alone.
struct Pictures {
    black: Picture,
    whites: Vec<Picture>,
    ramps: Vec<[Vec<Picture>; 3]>,
}

/// Takes the [`Pictures`] of `count` projectors: `frames` camera frames averaged per picture,
/// three times as many for black (its values are the faintest).
fn measure(rig: &mut Rig<'_>, count: usize, drives: &[f64], frames: usize) -> CmdResult<Pictures> {
    rig.frames = frames * 3;
    rig.note("all black, then each projector's white");
    let black = rig.shot(|_| [0.0; 3])?;
    rig.frames = frames;
    let mut whites = Vec::with_capacity(count);
    for k in 0..count {
        whites.push(rig.shot(|j| if j == k { [1.0; 3] } else { [0.0; 3] })?);
    }
    let mut ramps: Vec<[Vec<Picture>; 3]> = Vec::with_capacity(count);
    for k in 0..count {
        let path = rig.projectors.get(k).map(|(_, p)| p.clone()).unwrap_or_default();
        rig.note(&format!("ramps of {path} ({} of {count})", k + 1));
        let mut per: [Vec<Picture>; 3] = Default::default();
        for (c, ramp) in per.iter_mut().enumerate() {
            for d in drives {
                let mut s = [0.0f32; 3];
                if let Some(v) = s.get_mut(c) {
                    *v = *d as f32;
                }
                ramp.push(rig.shot(|j| if j == k { s } else { [0.0; 3] })?);
            }
        }
        ramps.push(per);
    }
    Ok(Pictures { black, whites, ramps })
}

/// Where each projector lights the camera's picture: at all (`covers`, the gain's samples and
/// the black) and strongly (the measuring spots, picked as `projector.match` picks them).
struct Lighting {
    covers: Vec<Vec<bool>>,
    spots: colour::Spots,
}

fn lighting(
    projectors: &[(NodeId, String)],
    pictures: &Pictures,
    cover_share: f32,
    warnings: &mut Vec<String>,
) -> CmdResult<Lighting> {
    let black = &pictures.black;
    let mut lit = Vec::with_capacity(projectors.len());
    for ((_, path), white) in projectors.iter().zip(&pictures.whites) {
        let mask = colour::lit(white, black).ok_or_else(|| {
            format!("{path} lights no camera pixel (is it in the camera's view, on, and the exposure right?)")
        })?;
        lit.push(mask);
    }
    let covers: Vec<Vec<bool>> =
        pictures.whites.iter().map(|w| flatfield::cover(&w.rgb, &black.rgb, cover_share)).collect();
    let spots = colour::spots(&lit, black.width, black.height);
    if projectors.len() > 1 && !spots.shared {
        warnings.push(
            "the projectors do not overlap in the camera's view; a match made by projector.match may not \
             hold across them (each one's gain is relative to its own centre)"
                .into(),
        );
    }
    Ok(Lighting { covers, spots })
}

/// One projector's gain: its surfaces (log gain over the projector, per channel), taken
/// relative to the spot by `offset`.
struct Gain {
    surfaces: [Surface; 3],
    offset: [f64; 3],
    /// The fitted log gains (relative to the spot) at the samples, all channels.
    logs: Vec<f64>,
    /// The measured gains (relative to the spot) in the camera's picture, before the fit.
    picture: Vec<[f32; 3]>,
    report: Json,
    warnings: Vec<String>,
}

/// The gain of the projector at `path`: per camera sample it lights (`cover`, eroded) how much
/// dimmer it is than the projector's typical pixel (`ramps` read against `drives`), fitted over
/// the projector by the scan's `map` (`cam_to_proj`), then taken relative to the fit at `spot`.
#[allow(clippy::too_many_arguments)]
fn gain(
    path: &str,
    map: &Map,
    ramps: &[Vec<Picture>; 3],
    black: &Picture,
    cover: &[bool],
    spot: &[bool],
    drives: &[f64],
    fit: GainFit,
) -> CmdResult<Gain> {
    let GainFit { degree, trim, even } = fit;
    let (cw, ch) = (black.width, black.height);
    let levels = drives.len();
    let mut warnings = Vec::new();
    // All the projector lights, not only its brightest part: on a surface of mixed colours
    // (a dark relief before a white wall) that would be the white wall alone.
    let inner = colour::erode(cover, cw, ch, EDGE);
    let candidates: Vec<usize> = (0..cw * ch).filter(|i| inner.get(*i).copied().unwrap_or(false)).collect();
    let stride = candidates.len().div_ceil(MAX_SAMPLES).max(1);
    let mut points = Vec::with_capacity(candidates.len() / stride + 1);
    let mut unmapped = 0usize;
    for &i in candidates.iter().step_by(stride) {
        match projector_uv(map, i, cw, ch) {
            Some(uv) => points.push((i, uv)),
            None => unmapped += 1,
        }
    }
    let reading = |pic: &Picture, i: usize, c: usize| {
        let at = |p: &Picture| p.rgb.get(i).and_then(|x| x.get(c)).copied().unwrap_or(0.0);
        f64::from(at(pic) - at(black))
    };
    // The reference: per channel and level the median over the projector's pixels.
    let mut reference = [vec![0.0; levels], vec![0.0; levels], vec![0.0; levels]];
    for (c, (refs, ramp)) in reference.iter_mut().zip(ramps).enumerate() {
        for (r, pic) in refs.iter_mut().zip(ramp) {
            let values: Vec<f64> = points.iter().map(|(i, _)| reading(pic, *i, c)).collect();
            *r = flatfield::quantile(&values, 0.5).ok_or_else(|| {
                format!("{path}: no camera pixel it lights has a position in its scan (does the scan fit the setup?)")
            })?;
        }
    }
    let mut samples: [Vec<[f64; 3]>; 3] = Default::default();
    let mut picture = vec![[0f32; 3]; cw * ch];
    for (i, [u, v]) in &points {
        let mut gains = [None; 3];
        for (c, (g, ramp)) in gains.iter_mut().zip(ramps).enumerate() {
            let readings: Vec<f64> = ramp.iter().map(|p| reading(p, *i, c)).collect();
            let refs = reference.get(c).ok_or("lost a reference")?;
            *g = flatfield::relative_gain(drives, &readings, refs).map(f64::ln);
        }
        let logs = match even {
            Even::All => gains,
            Even::Colour => match gains {
                [Some(r), Some(g), Some(b)] => flatfield::colour_only([r, g, b]).map(Some),
                _ => [None; 3],
            },
        };
        for (c, (out, l)) in samples.iter_mut().zip(logs).enumerate() {
            if let Some(l) = l {
                out.push([f64::from(*u), f64::from(*v), l]);
                if let Some(px) = picture.get_mut(*i).and_then(|p| p.get_mut(c)) {
                    *px = l.exp() as f32;
                }
            }
        }
    }
    let mut fitted: Vec<Surface> = Vec::with_capacity(3);
    let mut fit_report = Vec::new();
    for (c, s) in samples.iter().enumerate() {
        let surface = Surface::fit(s, degree, trim, 3).map_err(|err| {
            format!(
                "{path}, {}: {err} (does the scan fit the setup? is the camera's view lit?)",
                ["red", "green", "blue"].get(c).copied().unwrap_or("?")
            )
        })?;
        let (all, best) = surface.residual(s, trim);
        fit_report.push(json!({
            "samples": s.len(),
            "rms_log": round(all, 4),
            "rms_log_kept": round(best, 4),
            "relative_range": [round(surface.range[0].exp(), 4), round(surface.range[1].exp(), 4)],
        }));
        fitted.push(surface);
    }
    let [r, g, b]: [Surface; 3] = fitted.try_into().map_err(|_| "lost a fit")?;
    let surfaces = [r, g, b];
    if even == Even::All {
        let kept: Vec<f64> = samples.iter().zip(&surfaces).map(|(s, f)| f.residual(s, trim).1).collect();
        if kept.iter().any(|v| *v > UNEVEN) {
            warnings.push(format!(
                "{path}: the surface's shading is in the gain too. The surface is far from even ({:.0} % from \
                 the fit): a relief, objects or a pattern. Measure on a flat, evenly coloured surface, or \
                 use even: colour",
                kept.iter().fold(0.0f64, |a, v| a.max(*v)) * 100.0
            ));
        }
    }
    // The fit at the spot: the gains are taken relative to it.
    let mut at_spot = [0.0f64; 3];
    let mut n = 0usize;
    for (i, _) in spot.iter().enumerate().filter(|(_, m)| **m) {
        if let Some([u, v]) = projector_uv(map, i, cw, ch) {
            for (o, sf) in at_spot.iter_mut().zip(&surfaces) {
                *o += sf.value(f64::from(u), f64::from(v));
            }
            n += 1;
        }
    }
    let offset = if n > 0 {
        at_spot.map(|v| v / n as f64)
    } else {
        warnings.push(format!(
            "{path}: its scan has no position at the measuring spot; its gain is relative to its typical pixel"
        ));
        [0.0; 3]
    };
    let mut logs = Vec::new();
    for ((s, sf), o) in samples.iter().zip(&surfaces).zip(offset) {
        logs.extend(s.iter().map(|x| sf.value(x[0], x[1]) - o));
    }
    for p in &mut picture {
        for (v, o) in p.iter_mut().zip(offset) {
            *v *= (-o).exp() as f32;
        }
    }
    let report = json!({
        "path": path,
        "lit_pixels": colour::count(cover),
        "camera_samples": points.len(),
        "unmapped": unmapped,
        "reference": reference.iter().map(|r| r.iter().map(|v| round(*v, 5)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "spot_relative": offset.map(|o| round(o.exp(), 4)),
        "fit": fit_report,
    });
    Ok(Gain { surfaces, offset, logs, picture, report, warnings })
}

/// The black lift: per projector its black (measured, or estimated from `contrast`) lifted
/// where the others are brighter, in the camera's picture.
struct Blacks {
    lifts: Vec<Vec<Option<[f32; 3]>>>,
    report: Vec<Json>,
    /// The black where no projector lights.
    room: [f64; 3],
}

fn blacks(
    projectors: &[(NodeId, String)],
    pictures: &Pictures,
    lighting: &Lighting,
    contrast: Option<f64>,
    warnings: &mut Vec<String>,
) -> Blacks {
    let black = &pictures.black;
    let (cw, ch) = (black.width, black.height);
    let (covers, spots) = (&lighting.covers, &lighting.spots);
    // Each white smoothed within where it lights: the lift is relative to the projector's light
    // there, not to the surface's shading (dark slopes would get a lift they show darker anyway).
    let white_sigma = (cw.max(ch) as f32 / 40.0).max(1.0);
    let subtracted: Vec<Vec<[f32; 3]>> = pictures
        .whites
        .iter()
        .zip(covers)
        .map(|(w, cover)| {
            let raw: Vec<[f32; 3]> =
                w.rgb.iter().zip(&black.rgb).map(|(w, b)| [w[0] - b[0], w[1] - b[1], w[2] - b[2]]).collect();
            flatfield::smooth_within(cw, ch, &raw, cover, white_sigma)
        })
        .collect();
    let (measured_blacks, room) = flatfield::blacks(&black.rgb, covers);
    let mut used_blacks = Vec::with_capacity(projectors.len());
    let mut report = Vec::new();
    for (k, (_, path)) in projectors.iter().enumerate() {
        let measured = measured_blacks.get(k).copied().flatten();
        let white_spot = spots
            .masks
            .get(k)
            .and_then(|s| colour::trimmed_mean(pictures.whites.get(k)?, s))
            .zip(colour::trimmed_mean(black, spots.masks.get(k).map(Vec::as_slice).unwrap_or(&[])))
            .map(|(w, b)| [w[0] - b[0], w[1] - b[1], w[2] - b[2]]);
        let readable = measured.is_some_and(|m| m.iter().any(|v| *v > flatfield::MIN_BLACK));
        let (used, source) = match (contrast, readable, white_spot) {
            (Some(c), false, Some(w)) => (w.map(|v| v / c), "contrast"),
            (_, true, _) => (measured.unwrap_or([0.0; 3]), "measured"),
            _ => ([0.0; 3], "none"),
        };
        if source == "none" && projectors.len() > 1 {
            warnings.push(format!(
                "{path}: its black is too faint for the camera (or it lights nothing alone); no lift from it. \
                 Give `contrast` to estimate it"
            ));
        }
        report.push(json!({
            "path": path,
            "measured": measured.map(|m| m.map(|v| round(v, 6))),
            "used": used.map(|v| round(v, 6)),
            "source": source,
            "white_at_spot": white_spot.map(|w| w.map(|v| round(v, 4))),
        }));
        used_blacks.push(used);
    }
    let lifts = flatfield::lift(covers, &subtracted, &used_blacks);
    Blacks { lifts, report, room }
}

/// Keeps the camera's pictures as 16-bit PNGs in `folder`.
fn save_pictures(folder: &Path, pictures: &Pictures, drives: &[f64]) -> CmdResult<()> {
    std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    let save = |name: String, pic: &Picture| -> CmdResult<()> {
        let file = folder.join(name);
        std::fs::write(&file, png16(pic.width, pic.height, &pic.rgb)?)
            .map_err(|err| format!("{}: {err}", file.display()).into())
    };
    save("black.png".into(), &pictures.black)?;
    for (k, (white, per)) in pictures.whites.iter().zip(&pictures.ramps).enumerate() {
        save(format!("p{k}_white.png"), white)?;
        for (c, ramp) in per.iter().enumerate() {
            for (pic, d) in ramp.iter().zip(drives) {
                save(format!("p{k}_c{c}_{:.3}.png", d), pic)?;
            }
        }
    }
    Ok(())
}

/// A projector's maps in its own pixels: the gain (`gain`'s surfaces, relative to the spot,
/// times the common `scale`) and the black lift (`lift`, in the camera's `cw` x `ch` picture,
/// taken to the projector by its `proj_to_cam` map and blurred by `blur`).
fn projector_maps(
    gain: &Gain,
    scale: f64,
    lift: &[Option<[f32; 3]>],
    (cw, ch): (usize, usize),
    p2c: &Map,
    blur: f32,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let (pw, ph) = (p2c.width, p2c.height);
    let scales = gain.offset.map(|o| scale * (-o).exp());
    let gain_map = flatfield::gain_map(&gain.surfaces, scales, pw, ph);
    let lift = flatfield::to_projector(lift, cw, ch, p2c, blur, [0.0; 3]);
    (gain_map, lift)
}

/// `projector.flatfield` in stages: the settings; the scans read; the pictures taken
/// ([`measure`]); where each projector lights ([`lighting`]); per projector its gain ([`gain`])
/// and their common scale; the black lift ([`blacks`]); the maps written per projector
/// ([`projector_maps`]) and connected as one undo step.
/// `projector.flatfield` as a job, its edit made as `actor`: answers when done unless `a.wait`
/// is false.
pub fn flatfield_job(e: &Arc<Engine>, a: FlatfieldArgs, actor: Actor) -> CmdResult<Json> {
    let wait = a.wait.unwrap_or(true);
    crate::jobs::run(e, "projector.flatfield", actor, wait, move |e, job| run(e, &a, Some(job), actor))
}

pub fn run(e: &Engine, a: &FlatfieldArgs, job: Option<&Job>, actor: Actor) -> CmdResult<Json> {
    let started = Instant::now();
    let Settings { levels, low, frames, settle, timeout, fit, floor, cover, blur } = settings(a)?;
    let session = e.session();
    let dir = session.dir().to_path_buf();
    let net = session.doc().snapshot();
    let projectors = projector_list(e, &net, Some(&a.projectors))?;

    // The scans first: a missing file fails before anything is shown.
    let mut from_camera = Vec::with_capacity(projectors.len());
    let mut to_camera = Vec::with_capacity(projectors.len());
    for scan in &a.scans {
        let folder = dir.join(scan);
        from_camera.push(read_map(&folder.join(CAMERA_MAP))?);
        to_camera.push(read_map(&folder.join(PROJECTOR_MAP))?);
    }

    // The pictures.
    let camera = CameraStream::open(e, &net, &a.camera)?;
    let claim = Claim::new(e, projectors.iter().map(|(id, _)| *id).collect())?;
    let mut rig = Rig {
        engine: e,
        projectors: projectors.clone(),
        camera,
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        pass: e.output().pass,
        frames: frames * 3,
        job,
    };
    if let Some(job) = job {
        job.expect(1 + projectors.len() * (1 + 3 * levels));
    }
    let drives = drives(levels, low);
    let pictures = measure(&mut rig, projectors.len(), &drives, frames)?;
    drop(claim);
    let measured_s = started.elapsed().as_secs_f64();
    if let Some(job) = job {
        job.step("fitting the gain and the black", Some(0.95));
    }
    let (cw, ch) = (pictures.black.width, pictures.black.height);

    let mut warnings: Vec<String> = Vec::new();
    let lighting = lighting(&projectors, &pictures, cover, &mut warnings)?;

    // The gain: per camera sample how much dimmer it is than the projector's typical pixel,
    // fitted over the projector, then taken relative to the spot.
    let mut gains: Vec<Gain> = Vec::with_capacity(projectors.len());
    for (k, (_, path)) in projectors.iter().enumerate() {
        let map = from_camera.get(k).ok_or("lost a map")?;
        let per = pictures.ramps.get(k).ok_or("lost a ramp")?;
        let cover = lighting.covers.get(k).ok_or("lost a mask")?;
        let spot = lighting.spots.masks.get(k).map(Vec::as_slice).unwrap_or(&[]);
        let g = gain(path, map, per, &pictures.black, cover, spot, &drives, fit)?;
        warnings.extend(g.warnings.iter().cloned());
        gains.push(g);
    }
    let fitted: Vec<Vec<f64>> = gains.iter().map(|g| g.logs.clone()).collect();
    let scale = flatfield::common_scale(&fitted, floor).ok_or("no samples to scale the gain by")?;

    // The black.
    let blacks = blacks(&projectors, &pictures, &lighting, a.contrast, &mut warnings);

    // The files.
    let out_rel = a.out.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_OUT));
    let out = dir.join(&out_rel);
    std::fs::create_dir_all(&out).map_err(|err| format!("{}: {err}", out.display()))?;
    if a.pictures.unwrap_or(false) {
        save_pictures(&out.join("pictures"), &pictures, &drives)?;
    }
    let write = |name: &str, bytes: Vec<u8>| -> CmdResult<String> {
        let file = out.join(name);
        std::fs::write(&file, bytes).map_err(|err| format!("{}: {err}", file.display()))?;
        Ok(file.display().to_string())
    };
    let mut files = Vec::with_capacity(projectors.len());
    let mut reports = Vec::with_capacity(projectors.len());
    for (k, ((_, path), g)) in projectors.iter().zip(&gains).enumerate() {
        let name =
            NodePath::parse(path)?.name().map(|n| n.as_str().to_string()).unwrap_or_else(|| format!("{k}"));
        let p2c = to_camera.get(k).ok_or("lost a map")?;
        let (pw, ph) = (p2c.width, p2c.height);
        let lift_camera = blacks.lifts.get(k).map(Vec::as_slice).unwrap_or(&[]);
        let (gain_map, lift) = projector_maps(g, scale, lift_camera, (cw, ch), p2c, blur);
        let gain_path = write(&gain_file(&name), png16(pw, ph, &gain_map)?)?;
        let black_path = write(&black_file(&name), png16(pw, ph, &lift)?)?;
        let scaled: Vec<[f32; 3]> =
            g.picture.iter().map(|p| p.map(|v| (v * scale as f32).clamp(0.0, 1.0))).collect();
        let measured_path = write(&measured_file(&name), png16(cw, ch, &scaled)?)?;
        let mut report = g.report.clone();
        if let Some(o) = report.as_object_mut() {
            o.insert("gain".into(), stats(&gain_map));
            o.insert("lift".into(), stats(&lift));
            o.insert(
                "files".into(),
                json!({"gain": gain_path, "black": black_path, "measured": measured_path}),
            );
        }
        reports.push(report);
        files.push((path.clone(), name));
    }

    let apply = a.apply.unwrap_or(true);
    let history = if apply {
        let ops = connect_ops(e, &net, &files, &out_rel)?;
        Some(outcome(&edit_as(&session, actor, &ops, "projector.flatfield")?))
    } else {
        None
    };
    Ok(json!({
        "message": format!(
            "{} projector(s); gains scaled by {:.3} so the dimmest {:.0} % of the picture needs full drive{}",
            projectors.len(),
            scale,
            (1.0 - floor) * 100.0,
            if apply { "; maps connected to the gain and black inputs" } else { " (not connected)" }
        ),
        "scale": round(scale, 5),
        "levels": drives.iter().map(|d| round(*d, 4)).collect::<Vec<_>>(),
        "camera_picture": [cw, ch],
        "shared_spot": lighting.spots.shared,
        "room_black": blacks.room.map(|v| round(v, 6)),
        "blacks": blacks.report,
        "projectors": reports,
        "applied": apply,
        "history": history,
        "warnings": warnings,
        "seconds": {"measure": round(measured_s, 2), "total": round(started.elapsed().as_secs_f64(), 2)},
    }))
}

/// For each projector (path, node name): a `tex.image_file` of its gain map into its `gain` input and
/// one of its black lift into `black`; image nodes already connected there get the file instead.
fn connect_ops(e: &Engine, net: &Network, files: &[(String, String)], out: &Path) -> CmdResult<Vec<Op>> {
    let mut ops = Vec::new();
    let mut taken: Vec<NodePath> = Vec::new();
    let rel = |file: String| out.join(file).to_string_lossy().replace('\\', "/");
    for (path, name) in files {
        let node = NodePath::parse(path)?;
        let id = crate::commands::require(net, &node)?;
        let parent = node.parent().unwrap_or_else(NodePath::root);
        for (input, kind, file) in [(1u32, "gain", gain_file(name)), (2u32, "black", black_file(name))] {
            let value = Value::Text(rel(file));
            let connected = net
                .node(id)
                .and_then(|n| n.inputs.get(input as usize).copied().flatten())
                .filter(|port| net.node(port.node).is_some_and(|n| n.kind == IMAGE))
                .and_then(|port| net.path_of(port.node));
            if let Some(image) = connected {
                ops.push(Op::Set { param: ParamPath::new(image, Name::new("file")?)?, value });
                continue;
            }
            // A fresh name next to the projector.
            let mut image = None;
            for n in 0..100 {
                let candidate = if n == 0 { format!("{name}_{kind}") } else { format!("{name}_{kind}{n}") };
                let p = parent.child(Name::new(&candidate)?)?;
                if net.resolve(&p).is_none() && !taken.contains(&p) {
                    image = Some(p);
                    break;
                }
            }
            let image = image.ok_or_else(|| format!("no free name for {name}'s {kind} image"))?;
            taken.push(image.clone());
            let types = e.types();
            if types.desc(IMAGE).is_none() {
                return Err(format!("no {IMAGE} node type").into());
            }
            ops.push(Op::Create { path: image.clone(), kind: IMAGE.into(), params: Default::default() });
            ops.push(Op::Set { param: ParamPath::new(image.clone(), Name::new("file")?)?, value });
            ops.push(Op::Connect { from: image, output: 0, to: node.clone(), input });
        }
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 32;

    /// A `cam_to_proj` (or `proj_to_cam`) map of an `N` x `N` picture onto itself.
    fn identity() -> Map {
        let mut rgba = Vec::with_capacity(N * N);
        for y in 0..N {
            for x in 0..N {
                rgba.push([(x as f32 + 0.5) / N as f32, (y as f32 + 0.5) / N as f32, 1.0, 1.0]);
            }
        }
        Map::new(N, N, rgba).unwrap()
    }

    fn picture(value: impl Fn(usize) -> [f32; 3]) -> Picture {
        Picture { width: N, height: N, rgb: (0..N * N).map(value).collect() }
    }

    /// A projector whose light at camera column x is `field(u)` times its drive's light, over a
    /// room black of 0.01: its black, white and ramps at `drives`.
    fn pictures(drives: &[f64], field: impl Fn(f64) -> f64) -> Pictures {
        let u = |i: usize| ((i % N) as f64 + 0.5) / N as f64;
        let light = |i: usize, d: f64| (0.01 + 0.5 * field(u(i)) * colour::decode(d)) as f32;
        let black = picture(|_| [0.01; 3]);
        let white = picture(|i| [light(i, 1.0); 3]);
        let ramps = [0, 1, 2].map(|c| {
            drives
                .iter()
                .map(|d| {
                    picture(|i| {
                        let mut v = [0.01f32; 3];
                        v[c] = light(i, *d);
                        v
                    })
                })
                .collect()
        });
        Pictures { black, whites: vec![white], ramps: vec![ramps] }
    }

    const FIT: GainFit = GainFit { degree: 3, trim: 0.25, even: Even::All };

    fn gain_of(field: impl Fn(f64) -> f64) -> (Gain, f64, Vec<[f32; 3]>) {
        let drives = drives(5, 0.6);
        let p = pictures(&drives, field);
        let all = vec![true; N * N];
        let g = gain("/p", &identity(), &p.ramps[0], &p.black, &all, &all, &drives, FIT).unwrap();
        let scale = flatfield::common_scale(std::slice::from_ref(&g.logs), DEFAULT_FLOOR).unwrap();
        let (map, lift) = projector_maps(&g, scale, &vec![None; N * N], (N, N), &identity(), 0.0);
        assert!(lift.iter().all(|p| *p == [0.0; 3]), "no lift");
        (g, scale, map)
    }

    #[test]
    fn a_flat_field_gets_gain_one() {
        let (g, scale, map) = gain_of(|_| 1.0);
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
        assert!(g.offset.iter().all(|o| o.abs() < 1e-6), "{:?}", g.offset);
        assert!((scale - 1.0).abs() < 1e-6, "{scale}");
        assert!(map.iter().flatten().all(|v| (v - 1.0).abs() < 1e-4));
        assert_eq!(g.report["unmapped"], 0);
        assert_eq!(g.report["spot_relative"], json!([1.0, 1.0, 1.0]));
    }

    #[test]
    fn a_brighter_side_gets_less_gain() {
        // Brighter to the right, smoothly: e^(0.4 u).
        let (_, _, map) = gain_of(|u| (0.4 * u).exp());
        let at = |x: usize| map[(N / 2) * N + x][1];
        assert!(at(1) > at(N - 2), "{} {}", at(1), at(N - 2));
        let ratio = f64::from(at(1) / at(N - 2));
        let want = (0.4 * (N - 3) as f64 / N as f64).exp();
        // Within the interpolation between the drive levels.
        assert!((ratio - want).abs() < 0.05 * want, "{ratio} {want}");
        assert!(map.iter().flatten().all(|v| *v <= 1.0));
    }

    #[test]
    fn settings_fill_defaults_and_check_ranges() {
        let args = |v: Json| -> FlatfieldArgs { serde_json::from_value(v).unwrap() };
        let base = json!({"camera": "/cam", "projectors": ["/p"], "scans": ["s"]});
        let s = settings(&args(base.clone())).unwrap();
        assert_eq!((s.levels, s.low, s.frames, s.fit), (5, DEFAULT_LOW, DEFAULT_FRAMES, FIT));
        for (key, bad, says) in [
            ("levels", json!(1), "`levels`"),
            ("low", json!(0.1), "`low`"),
            ("frames", json!(0), "`frames`"),
            ("degree", json!(99), "`degree`"),
            ("trim", json!(0.7), "`trim`"),
            ("floor", json!(0.5), "`floor`"),
            ("cover", json!(0.0), "`cover`"),
            ("blur", json!(65.0), "`blur`"),
            ("contrast", json!(5.0), "`contrast`"),
            ("scans", json!([]), "one scan folder per projector"),
        ] {
            let mut v = base.clone();
            v[key] = bad;
            let err = settings(&args(v)).unwrap_err();
            assert!(err.0.contains(says), "{key}: {}", err.0);
        }
    }

    #[test]
    fn small_helpers() {
        assert_eq!(
            drives(5, 0.6).iter().map(|d| round(*d, 6)).collect::<Vec<_>>(),
            [0.6, 0.7, 0.8, 0.9, 1.0]
        );
        assert_eq!(round(1.23456, 2), 1.23);
        assert_eq!(round(f64::NAN, 2), 0.0);
        // A 2 x 1 map seen by a 4 x 2 camera.
        let map = Map::new(2, 1, vec![[0.1, 0.2, 1.0, 1.0], [0.9, 0.8, 1.0, 1.0]]).unwrap();
        assert_eq!(projector_uv(&map, 1, 4, 2), Some([0.1, 0.2]));
        assert_eq!(projector_uv(&map, 6, 4, 2), Some([0.9, 0.8]));
        let s = stats(&[[0.0, 1.0, 2.0], [1.0, 1.0, 2.0], [2.0, 1.0, 2.0]]);
        assert_eq!(s["r"]["median"], 1.0);
        assert_eq!(s["r"]["max"], 2.0);
        assert_eq!(s["b"]["min"], 2.0);
        assert!(png16(2, 1, &[[0.0; 3], [1.0; 3]]).unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn lighting_finds_the_projector_and_its_black() {
        let p = pictures(&drives(2, 0.6), |_| 1.0);
        let one = vec![(NodeId(1), "/p".to_string())];
        let mut warnings = Vec::new();
        let l = lighting(&one, &p, DEFAULT_COVER, &mut warnings).unwrap();
        assert!(warnings.is_empty() && l.spots.shared);
        assert_eq!(colour::count(&l.covers[0]), N * N);
        let b = blacks(&one, &p, &l, Some(100.0), &mut warnings);
        assert!(warnings.is_empty(), "one projector: no warning about its black");
        assert_eq!(b.report[0]["source"], "measured");
        assert_eq!(b.report[0]["measured"], json!([0.01, 0.01, 0.01]));
        assert_eq!(b.lifts.len(), 1);
        let dark = Pictures { whites: vec![p.black.clone()], ..p };
        let err = lighting(&one, &dark, DEFAULT_COVER, &mut warnings).err().unwrap();
        assert!(err.0.starts_with("/p lights no camera pixel"));
    }
}
