//! Structured light in the engine. Commands: `scan.patterns` (the pictures of a scan, in order),
//! `scan.decode` (camera pictures of them in a folder to maps and the scene as the projector
//! sees it, written as files), `scan.objects` (the objects in front of a surface, from a scan of
//! the empty scene and one with them) and `scan.blend` (weights and smoothed maps for projectors
//! lighting the same surface).
//!
//! The decode is a one-off job on the caller's thread (the control channel's connection), never
//! on the cook thread; nothing of it runs per frame. Its results are plain files in the project,
//! read by `tex.image`: the projector-space map goes into `tex.remap`'s `map` with the camera as
//! `in`, so the camera's view lands in projector pixels.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_calib::objects;
use penumbra_calib::scan::{self, Decoder, Options, Patterns, Status};
use penumbra_gpu::Pixels;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

/// The projector size assumed when none is given.
pub const DEFAULT_PROJECTOR: [u32; 2] = [1920, 1080];
/// Hole fill in projector pixels unless another is asked for.
pub const DEFAULT_FILL: u32 = 32;
/// Writes `bytes` to `file`; the file's path for the reply.
pub(crate) fn write_file(file: &Path, bytes: Vec<u8>) -> CmdResult<String> {
    std::fs::write(file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file.display().to_string())
}

/// `fill` (hole fill in projector pixels) of a scan command, checked.
pub(crate) fn fill_of(fill: Option<u32>) -> CmdResult<u32> {
    let fill = fill.unwrap_or(DEFAULT_FILL);
    if fill > scan::MAX_FILL {
        return Err(CommandError(format!("`fill` is 0 to {} projector pixels", scan::MAX_FILL)));
    }
    Ok(fill)
}

/// Largest side of a camera picture read.
const MAX_PICTURE_SIDE: u32 = 8192;
/// Most files a frames folder may hold (a scan of the largest projector has 58 pictures).
const MAX_FILES: usize = 1000;
/// Extensions taken as pictures; decoding goes by content.
const EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "tif", "tiff", "exr", "hdr", "webp", "bmp", "tga", "qoi", "pfm", "ppm", "pgm"];

/// Files `scan.decode` writes.
pub const CAMERA_MAP: &str = "cam_to_proj.exr";
pub const PROJECTOR_MAP: &str = "proj_to_cam.exr";
pub const SCENE: &str = "scene.png";
pub const QUALITY: &str = "quality.png";

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    registry.register(FnCommand::new(
        "scan.patterns",
        "The pictures of a structured light scan in the order tex.scan_pattern shows them; {projector?: [w, h] (1920, 1080)}",
        |p| {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                projector: Option<[u32; 2]>,
            }
            let a: A = args(p)?;
            let [w, h] = a.projector.unwrap_or(DEFAULT_PROJECTOR);
            let patterns = Patterns::new(w, h).map_err(CommandError)?;
            let names: Vec<String> =
                (0..patterns.count()).filter_map(|k| patterns.get(k).map(|p| p.name())).collect();
            Ok(json!({
                "count": patterns.count(),
                "column_bits": patterns.column_bits(),
                "row_bits": patterns.row_bits(),
                "patterns": names,
            }))
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.decode",
        "Decode a structured light scan from the camera's pictures of tex.scan_pattern 0, 1, 2, ... \
         (files in a folder, sorted by name, numbers by value). Writes cam_to_proj.exr and \
         proj_to_cam.exr (maps for tex.remap), scene.png (the scene as the projector sees it) and \
         quality.png; {frames (folder, relative to the project; or a list of folders: scans of one projector \
         at different levels of light, merged per camera pixel, the surest decode winning, the first's colour), projector?: [w, h] (1920, 1080), \
         out? (folder, default frames/decoded), fill? (projector px, 32), options?: {min_contrast?, \
         saturation?, min_confidence?, tolerance?, window? (camera px round a pixel over which the bits it resolves are judged, 7; 0 the whole picture), \
         epipolar? (px a decoded pixel may lie off the geometry the decode as a whole obeys, else it is dropped: light thrown on by the walls decodes wrong, 4; 0 off)}}",
        move |p| {
            let a: DecodeArgs = args(p)?;
            decode(e.session().dir(), &a)
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.objects",
        "Find the objects in front of a surface by parallax, per projector, between a scan of the empty \
         scene (reference) and a scan with the objects (folders scan.decode wrote). The objects are \
         numbered alike in every projector, top of the camera picture first. Writes per projector \
         labels_N.png (8-bit, 0 background, k the object) and parts_N.png, plus objects_camera.png (the \
         same labels in camera space, for shapes.trace with labels) and objects_camera_view.png; \
         {projectors: [{reference, scan}, ...], out? (folder, objects), threshold? (camera px, 25), \
         split? (camera px per projector px, 2), min_area? (projector px, 1/256 of the projector), \
         erode? (projector px, 2), unmeasured? (projector px, 4), rim? (projector px, 25)}",
        move |p| {
            let a: ObjectsArgs = args(p)?;
            objects(e.session().dir(), &a)
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.blend",
        "Blend weights for projectors lighting the same surface, from their scans (folders scan.decode \
         wrote). Writes weight_N.png (16-bit, sRGB-encoded so tex.image gives the weight; multiply the \
         projector's picture by it) and proj_to_cam_smooth_N.exr (the map smoothed, for tex.remap); \
         {scans: [folder, ...], masks? ([file or null, ...], e.g. scan.objects' labels_N.png), out? \
         (folder, objects), ramp? (projector px, 200), blur? (projector px, 3), reach? (camera px, 6), \
         sigma? (projector px, 6), into_scans? (write weight.png and proj_to_cam_smooth.exr into each \
         scan's folder, where tex.scan reads them, false)}",
        move |p| {
            let a: BlendArgs = args(p)?;
            blend(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecodeArgs {
    pub frames: Frames,
    pub projector: Option<[u32; 2]>,
    pub out: Option<PathBuf>,
    pub fill: Option<u32>,
    #[serde(default)]
    pub options: Options,
}

/// One folder of camera pictures, or several: scans of the same projector at different levels of
/// light, merged.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Frames {
    One(PathBuf),
    Several(Vec<PathBuf>),
}

/// Most frame folders `scan.decode` merges.
const MAX_LEVELS: usize = 4;

/// Decodes the pictures in `a.frames` (relative to `dir`) and writes the results; the reply
/// counts what was found.
pub fn decode(dir: &Path, a: &DecodeArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let [pw, ph] = a.projector.unwrap_or(DEFAULT_PROJECTOR);
    let patterns = Patterns::new(pw, ph).map_err(CommandError)?;
    a.options.check().map_err(CommandError)?;
    let fill = fill_of(a.fill)?;
    let folders: Vec<PathBuf> = match &a.frames {
        Frames::One(f) => vec![dir.join(f)],
        Frames::Several(list) => list.iter().map(|f| dir.join(f)).collect(),
    };
    if folders.is_empty() || folders.len() > MAX_LEVELS {
        return Err(format!("`frames` is a folder or a list of 1 to {MAX_LEVELS}").into());
    }
    let mut decoders = Vec::with_capacity(folders.len());
    for frames in &folders {
        decoders.push(decode_folder(frames, patterns)?);
    }
    let read = started.elapsed();
    let first = folders.first().ok_or("no frames")?;
    let out = match &a.out {
        Some(o) => dir.join(o),
        None => first.join("decoded"),
    };
    let decoded = write_decoded(&decoders, &a.options, fill, &out)?;
    let mut reply = decoded.fields.clone();
    reply.insert("message".into(), json!(decoded.message()));
    reply.insert("warnings".into(), json!(decoded.warnings));
    reply.insert(
        "next".into(),
        json!(format!(
            "tex.image of {PROJECTOR_MAP} into tex.remap's map, the camera into its in: the camera's view in projector pixels"
        )),
    );
    reply.insert(
        "ms".into(),
        json!({"read": ms(read), "decode": ms(decoded.decode), "total": ms(started.elapsed())}),
    );
    Ok(Json::Object(reply))
}

/// The pictures of one scan in `frames`, added to a decoder.
fn decode_folder(frames: &Path, patterns: Patterns) -> CmdResult<Decoder> {
    let (pw, ph) = patterns.size();
    let files = pictures(frames)?;
    if files.len() != patterns.count() {
        return Err(CommandError(format!(
            "{} holds {} picture(s); a scan of a {pw} x {ph} projector has {} (scan.patterns lists them)",
            frames.display(),
            files.len(),
            patterns.count()
        )));
    }

    let mut decoder: Option<Decoder> = None;
    for (k, file) in files.iter().enumerate() {
        let picture = penumbra_gpu::image::decode_file(file, MAX_PICTURE_SIDE)?;
        let d = match &mut decoder {
            Some(d) => d,
            None => {
                decoder.insert(Decoder::new(patterns, picture.width, picture.height).map_err(CommandError)?)
            }
        };
        if d.size() != (picture.width, picture.height) {
            let (w, h) = d.size();
            return Err(CommandError(format!(
                "{} is {} x {}, the first picture {w} x {h}",
                file.display(),
                picture.width,
                picture.height
            )));
        }
        let rgba = picture.to_rgba32f();
        d.add(k, &rgba).map_err(|e| CommandError(format!("{}: {e}", file.display())))?;
    }
    Ok(decoder.ok_or("no pictures")?)
}

/// What [`write_decoded`] found and wrote.
pub(crate) struct Decoded {
    /// The counts and files as the replies list them: `camera`, `projector`, `pictures`,
    /// `camera_pixels`, `projector_pixels`, `bits`, `resolved_bits`, `step`, `files`.
    pub fields: serde_json::Map<String, Json>,
    /// Camera pixels decoded, of all.
    pub valid: usize,
    pub total: usize,
    /// Projector pixels seen by the camera, and filled in between.
    pub measured: usize,
    pub filled: usize,
    pub warnings: Vec<String>,
    /// How long decoding and writing took.
    pub decode: Duration,
    pub write: Duration,
}

impl Decoded {
    pub(crate) fn message(&self) -> String {
        format!(
            "{} of {} camera pixels decoded; {} projector pixels seen, {} filled",
            self.valid, self.total, self.measured, self.filled
        )
    }
}

/// What a decode's counts warn of: few camera pixels decoded (`valid` of `total`), bits the
/// camera does not resolve, much of the picture clipped.
fn decode_warnings(
    valid: usize,
    total: usize,
    clipped: usize,
    bits: [u32; 2],
    resolved: [u32; 2],
) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    if valid * 10 < total {
        warnings.push(format!(
            "only {valid} of {total} camera pixels decoded: is the projector in view, the room dark and the order right?"
        ));
    }
    if resolved != bits {
        warnings.push(format!(
            "the camera resolves {} of {} column and {} of {} row bits: the map is a few projector pixels coarse",
            resolved[0], bits[0], resolved[1], bits[1]
        ));
    }
    if clipped * 100 > total {
        warnings
            .push("more than 1 % of the camera picture clipped in white: lower the camera's exposure".into());
    }
    warnings
}

/// Decodes what `decoders` collected (scans of one projector at different levels of light,
/// merged by [`scan::fuse`] if more than one) and writes `scan.decode`'s files into `out`. With
/// more than one the fields also hold `levels` (per scan the camera pixels decoded, taken from it
/// and decoded in it alone) and `apart_px` (how far overlapping decodes lie apart, median and 90th
/// percentile).
pub(crate) fn write_decoded(
    decoders: &[Decoder],
    options: &Options,
    fill: u32,
    out: &Path,
) -> CmdResult<Decoded> {
    let patterns = decoders.first().ok_or("no pictures")?.patterns();
    let (pw, ph) = patterns.size();
    let t = Instant::now();
    let mut scans = Vec::with_capacity(decoders.len());
    for d in decoders {
        scans.push(d.finish(options).map_err(CommandError)?);
    }
    let fusion = scan::fuse(scans).map_err(CommandError)?;
    let levels: Option<Json> = (decoders.len() > 1).then(|| {
        let list: Vec<Json> = (0..decoders.len())
            .map(|k| {
                let at = |v: &[usize]| v.get(k).copied().unwrap_or(0);
                json!({"valid": at(&fusion.valid), "taken": at(&fusion.taken), "only": at(&fusion.only)})
            })
            .collect();
        Json::Array(list)
    });
    let apart = fusion.apart;
    let result = fusion.scan;
    let inverse = result.inverse(fill);
    let decoded = t.elapsed();

    let t = Instant::now();
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| write_file(&out.join(name), bytes);
    let (cw, ch) = result.camera;
    let png = |w: u32, h: u32, data: Vec<[f32; 4]>| Pixels { width: w, height: h, data }.to_png();
    let files = json!({
        "camera_map": write(CAMERA_MAP, penumbra_gpu::image::encode_exr_rgba32f(cw, ch, &result.camera_map())?)?,
        "projector_map": write(PROJECTOR_MAP, penumbra_gpu::image::encode_exr_rgba32f(pw, ph, &inverse.projector_map())?)?,
        "scene": write(SCENE, png(pw, ph, inverse.scene())?)?,
        "quality": write(QUALITY, png(cw, ch, result.quality())?)?,
    });
    let written = t.elapsed();

    let stats = &result.stats;
    let total = result.status.len().max(1);
    let valid = stats.count(Status::Valid);
    let warnings = decode_warnings(valid, total, stats.count(Status::Clipped), stats.bits, stats.resolved);
    let counts: serde_json::Map<String, Json> =
        Status::ALL.iter().map(|s| (s.name().to_string(), json!(stats.count(*s)))).collect();
    let [measured, filled, empty] = inverse.counts();
    let fields = json!({
        "camera": [cw, ch],
        "projector": [pw, ph],
        "pictures": patterns.count(),
        "camera_pixels": counts,
        "projector_pixels": {"measured": measured, "filled": filled, "empty": empty},
        "bits": stats.bits,
        "resolved_bits": stats.resolved,
        "step": stats.step,
        "off_geometry": stats.off_geometry,
        "files": files,
    });
    let Json::Object(mut fields) = fields else { return Err("lost the counts".into()) };
    if let Some(levels) = levels {
        fields.insert("levels".into(), levels);
        fields.insert("apart_px".into(), json!(apart.map(|[m, p]| json!({"median": m, "p90": p}))));
    }
    Ok(Decoded { fields, valid, total, measured, filled, warnings, decode: decoded, write: written })
}

/// Where `scan.objects` and `scan.blend` write unless told otherwise (relative to the project).
pub const DEFAULT_OBJECTS_OUT: &str = "objects";
/// Files `scan.objects` writes besides the per projector ones.
pub const OBJECTS_CAMERA: &str = "objects_camera.png";
pub const OBJECTS_CAMERA_VIEW: &str = "objects_camera_view.png";
/// Per projector files; `n` is the projector's place in the command's list.
pub fn labels_file(n: usize) -> String {
    format!("labels_{n}.png")
}
pub fn parts_file(n: usize) -> String {
    format!("parts_{n}.png")
}
pub fn weight_file(n: usize) -> String {
    format!("weight_{n}.png")
}
pub fn smooth_file(n: usize) -> String {
    format!("proj_to_cam_smooth_{n}.exr")
}
/// Default standard deviation of the map smoothing, projector pixels.
pub const DEFAULT_SIGMA: f32 = 6.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanPair {
    /// The scan of the empty scene.
    pub reference: PathBuf,
    /// The scan with the objects.
    pub scan: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectsArgs {
    pub projectors: Vec<ScanPair>,
    pub out: Option<PathBuf>,
    pub threshold: Option<f32>,
    pub split: Option<f32>,
    pub min_area: Option<u32>,
    pub erode: Option<u32>,
    pub unmeasured: Option<f32>,
    pub rim: Option<f32>,
}

/// A map file `scan.decode` wrote.
pub(crate) fn read_map(file: &Path) -> CmdResult<objects::Map> {
    let raw = penumbra_gpu::image::decode_file_raw(file, scan::MAX_SIDE, objects::MAX_PIXELS as u64)?;
    objects::Map::new(raw.width as usize, raw.height as usize, raw.rgba)
        .map_err(|e| CommandError(format!("{}: {e}", file.display())))
}

/// A label or mask picture: per pixel the integer in its first channel (0 outside).
pub(crate) fn read_labels(file: &Path, size: (usize, usize)) -> CmdResult<Vec<u16>> {
    let raw = penumbra_gpu::image::decode_file_raw(file, scan::MAX_SIDE, objects::MAX_PIXELS as u64)?;
    if (raw.width as usize, raw.height as usize) != size {
        return Err(CommandError(format!(
            "{} is {} x {}, its projector {} x {}",
            file.display(),
            raw.width,
            raw.height,
            size.0,
            size.1
        )));
    }
    let value = |c: f32| if c.is_finite() { (c * raw.scale).round().clamp(0.0, 65535.0) as u16 } else { 0 };
    Ok(raw.rgba.iter().map(|p| value(p[0])).collect())
}

/// A colour per object for the pictures (sRGB bytes): hues a golden angle apart.
pub(crate) fn colour(k: u16) -> [u8; 3] {
    let h = (f32::from(k) * 0.618_034).fract() * 6.0;
    let (s, v) = (0.7f32, 1.0f32);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r, g, b].map(|t| ((t + m) * 255.0).round() as u8)
}

pub(crate) fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round()
}

/// `scan.objects`' options from its arguments, checked; `min_area` is set per projector.
fn object_options(a: &ObjectsArgs) -> CmdResult<objects::ObjectOptions> {
    let defaults = objects::ObjectOptions::default();
    let o = objects::ObjectOptions {
        threshold: a.threshold.unwrap_or(defaults.threshold),
        split: a.split.unwrap_or(defaults.split),
        min_area: 0,
        erode: a.erode.unwrap_or(defaults.erode),
        unmeasured: a.unmeasured.unwrap_or(defaults.unmeasured),
        rim: a.rim.unwrap_or(defaults.rim),
    };
    o.check().map_err(CommandError)?;
    Ok(o)
}

/// One projector's maps for `scan.objects`: the `proj_to_cam` maps of the empty scene and of the
/// one with the objects, and the latter's `cam_to_proj`.
struct ObjectScans {
    reference: objects::Map,
    scan: objects::Map,
    camera: objects::Map,
}

fn read_object_scans(dir: &Path, pair: &ScanPair) -> CmdResult<ObjectScans> {
    let scan_dir = dir.join(&pair.scan);
    let reference = read_map(&dir.join(&pair.reference).join(PROJECTOR_MAP))?;
    let scan = read_map(&scan_dir.join(PROJECTOR_MAP))?;
    let camera = read_map(&scan_dir.join(CAMERA_MAP))?;
    Ok(ObjectScans { reference, scan, camera })
}

/// Checks projector `n`'s maps against each other and its camera against the one of the
/// projectors before (`camera`, set by the first).
fn check_object_scans(
    n: usize,
    s: &ObjectScans,
    camera: &mut Option<(usize, usize)>,
) -> CmdResult<(usize, usize)> {
    let size = (s.camera.width, s.camera.height);
    match *camera {
        None => *camera = Some(size),
        Some(c) if c != size => {
            return Err(CommandError(format!(
                "projector {n}'s camera picture is {} x {}, projector 0's {} x {}: one camera for all",
                size.0, size.1, c.0, c.1
            )));
        }
        Some(_) => {}
    }
    if (s.reference.width, s.reference.height) != (s.scan.width, s.scan.height) {
        return Err(CommandError(format!(
            "projector {n}: the reference is {} x {}, the scan {} x {}; both must be of the same projector",
            s.reference.width, s.reference.height, s.scan.width, s.scan.height
        )));
    }
    Ok(size)
}

/// The smallest part kept: `min_area` if given, else 1/256 of the projector's pixels.
fn min_area_of(min_area: Option<u32>, pixels: usize) -> usize {
    match min_area {
        Some(m) => m as usize,
        None => (pixels / 256).max(1),
    }
}

/// What projector `n`'s parts warn of.
fn parts_warnings(n: usize, p: &objects::Parts) -> Vec<String> {
    let mut warnings = Vec::new();
    if p.compared * 10 < p.labels.len() {
        warnings.push(format!(
            "projector {n}: only {} of {} pixels measured in both scans; are they of the same projector and camera?",
            p.compared,
            p.labels.len()
        ));
    }
    if p.over > 0 {
        warnings.push(format!("projector {n}: {} parts beyond {} dropped", p.over, objects::MAX_PARTS));
    }
    warnings
}

/// What the numbering warns of.
fn found_warnings(found: &objects::Objects, threshold: f32) -> Vec<String> {
    let mut warnings = Vec::new();
    if found.over > 0 {
        warnings.push(format!("{} objects beyond {} dropped", found.over, objects::MAX_PARTS));
    }
    if found.count == 0 {
        warnings.push(format!(
            "no objects found: nothing moved more than {threshold} camera pixels against the reference"
        ));
    }
    warnings
}

/// Labels as 8-bit grey (0 beyond 255).
fn label_bytes(labels: &[u16]) -> Vec<u8> {
    labels.iter().map(|l| u8::try_from(*l).unwrap_or(0)).collect()
}

/// A projector's parts as RGBA bytes: objects in colour, cuts at parallax jumps darker, dropped
/// pixels dark red.
fn parts_view(p: &objects::Parts) -> Vec<u8> {
    let mut view = Vec::with_capacity(p.labels.len() * 4);
    for ((l, cut), dropped) in p.labels.iter().zip(&p.cuts).zip(&p.dropped) {
        let mut c = if *l > 0 { colour(*l) } else { [0, 0, 0] };
        if *dropped {
            c = [110, 0, 0];
        } else if *cut && *l > 0 {
            c = c.map(|v| v / 2);
        }
        view.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    view
}

/// Labels as RGBA bytes, each object in its colour.
fn labels_view(labels: &[u16]) -> Vec<u8> {
    let mut view = Vec::with_capacity(labels.len() * 4);
    for l in labels {
        let c = if *l > 0 { colour(*l) } else { [0, 0, 0] };
        view.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    view
}

/// Per object its height in the camera, its pixels per projector, the projectors that see it
/// and its camera pixels.
fn object_list(found: &objects::Objects, parts: &[objects::Parts], cam_labels: &[u16]) -> Vec<Json> {
    let mut list = Vec::new();
    for k in 1..=found.count {
        let label = u16::try_from(k).unwrap_or(0);
        let pixels: Vec<usize> =
            parts.iter().map(|p| p.labels.iter().filter(|l| **l == label).count()).collect();
        let seen_by: Vec<usize> =
            pixels.iter().enumerate().filter(|(_, c)| **c > 0).map(|(n, _)| n).collect();
        list.push(json!({
            "object": k,
            "camera_y": found.camera_y.get(k - 1).map(|y| y.round()),
            "pixels": pixels,
            "seen_by": seen_by,
            "camera_pixels": cam_labels.iter().filter(|l| **l == label).count(),
        }));
    }
    list
}

/// What `scan.objects` wrote: per projector its report, and the camera's files.
struct ObjectFiles {
    per_projector: Vec<Json>,
    camera: String,
    camera_view: String,
}

/// Writes the labels and their pictures into `out`: per projector and in the camera.
fn write_objects(
    out: &Path,
    a: &ObjectsArgs,
    parts: &[objects::Parts],
    cam_labels: &[u16],
    camera: (usize, usize),
) -> CmdResult<ObjectFiles> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| write_file(&out.join(name), bytes);
    let mut per_projector = Vec::new();
    for (n, p) in parts.iter().enumerate() {
        let (w, h) = (p.width as u32, p.height as u32);
        let labels =
            write(&labels_file(n), penumbra_gpu::image::encode_png_gray8(w, h, label_bytes(&p.labels))?)?;
        let picture = write(&parts_file(n), penumbra_gpu::encode_png_rgba8(w, h, parts_view(p))?)?;
        per_projector.push(json!({
            "reference": a.projectors.get(n).map(|x| x.reference.display().to_string()),
            "scan": a.projectors.get(n).map(|x| x.scan.display().to_string()),
            "size": [w, h],
            "pixels": p.pixels(),
            "silhouette": p.silhouette,
            "dropped": p.dropped.iter().filter(|d| **d).count(),
            "compared": p.compared,
            "labels": labels,
            "picture": picture,
        }));
    }
    let (cw, ch) = (camera.0 as u32, camera.1 as u32);
    let camera_file =
        write(OBJECTS_CAMERA, penumbra_gpu::image::encode_png_gray8(cw, ch, label_bytes(cam_labels))?)?;
    let camera_view =
        write(OBJECTS_CAMERA_VIEW, penumbra_gpu::encode_png_rgba8(cw, ch, labels_view(cam_labels))?)?;
    Ok(ObjectFiles { per_projector, camera: camera_file, camera_view })
}

/// Finds the objects of `a` (paths relative to `dir`) and writes their labels: per projector its
/// maps read and checked ([`read_object_scans`], [`check_object_scans`]) and its parts found;
/// then the parts numbered as objects across the projectors, the files written
/// ([`write_objects`]) and the objects listed ([`object_list`]).
pub fn objects(dir: &Path, a: &ObjectsArgs) -> CmdResult<Json> {
    let started = Instant::now();
    if a.projectors.is_empty() || a.projectors.len() > objects::MAX_PROJECTORS {
        return Err(CommandError(format!("`projectors` lists 1 to {} projectors", objects::MAX_PROJECTORS)));
    }
    let mut o = object_options(a)?;

    let mut scans = Vec::new();
    let mut parts = Vec::new();
    let mut camera: Option<(usize, usize)> = None;
    let mut warnings: Vec<String> = Vec::new();
    let mut read = Duration::ZERO;
    let mut work = Duration::ZERO;
    for (n, pair) in a.projectors.iter().enumerate() {
        let t = Instant::now();
        let s = read_object_scans(dir, pair)?;
        read += t.elapsed();
        let size = check_object_scans(n, &s, &mut camera)?;
        let t = Instant::now();
        o.min_area = min_area_of(a.min_area, s.scan.len());
        let p = objects::segment(&s.reference, &s.scan, size, &o).map_err(CommandError)?;
        work += t.elapsed();
        warnings.extend(parts_warnings(n, &p));
        scans.push(s.scan);
        parts.push(p);
    }
    let camera = camera.ok_or("no projectors")?;
    let t = Instant::now();
    let refs: Vec<&objects::Map> = scans.iter().collect();
    let found = objects::number(&mut parts, &refs, camera, o.erode).map_err(CommandError)?;
    let cam_labels = objects::camera_labels(&parts, &refs, camera, o.erode).map_err(CommandError)?;
    work += t.elapsed();
    warnings.extend(found_warnings(&found, o.threshold));

    let t = Instant::now();
    let out = dir.join(a.out.as_deref().unwrap_or(Path::new(DEFAULT_OBJECTS_OUT)));
    let files = write_objects(&out, a, &parts, &cam_labels, camera)?;
    let written = t.elapsed();

    let list = object_list(&found, &parts, &cam_labels);
    let (cw, ch) = (camera.0 as u32, camera.1 as u32);
    Ok(json!({
        "message": format!("{} object(s) in {} projector(s)", found.count, parts.len()),
        "camera": [cw, ch],
        "objects": list,
        "projectors": files.per_projector,
        "files": {"camera": files.camera, "camera_view": files.camera_view},
        "options": {
            "threshold": o.threshold, "split": o.split, "min_area": o.min_area, "erode": o.erode,
            "unmeasured": o.unmeasured, "rim": o.rim,
        },
        "ms": {"read": ms(read), "work": ms(work), "write": ms(written), "total": ms(started.elapsed())},
        "warnings": warnings,
        "next": format!(
            "tex.image of {OBJECTS_CAMERA} and shapes.trace with labels: one shape per object in camera space; \
             scan.blend with the labels_N.png as masks"
        ),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlendArgs {
    pub scans: Vec<PathBuf>,
    pub masks: Option<Vec<Option<PathBuf>>>,
    pub out: Option<PathBuf>,
    pub ramp: Option<f32>,
    pub blur: Option<f32>,
    pub reach: Option<f32>,
    pub sigma: Option<f32>,
    /// Write `weight.png` and `proj_to_cam_smooth.exr` into each scan's folder (where a
    /// `tex.scan` reads them) instead of numbered files into `out`.
    pub into_scans: Option<bool>,
}

/// Blend weights and smoothed maps for the scans of `a` (paths relative to `dir`).
pub fn blend(dir: &Path, a: &BlendArgs) -> CmdResult<Json> {
    let started = Instant::now();
    if a.scans.is_empty() || a.scans.len() > objects::MAX_PROJECTORS {
        return Err(CommandError(format!("`scans` lists 1 to {} folders", objects::MAX_PROJECTORS)));
    }
    if a.masks.as_ref().is_some_and(|m| m.len() != a.scans.len()) {
        return Err("`masks` lists one file (or null) per scan".into());
    }
    let defaults = objects::BlendOptions::default();
    let o = objects::BlendOptions {
        ramp: a.ramp.unwrap_or(defaults.ramp),
        blur: a.blur.unwrap_or(defaults.blur),
        reach: a.reach.unwrap_or(defaults.reach),
    };
    o.check().map_err(CommandError)?;
    let sigma = a.sigma.unwrap_or(DEFAULT_SIGMA);
    if !sigma.is_finite() || !(0.0..=objects::MAX_SIGMA).contains(&sigma) {
        return Err(CommandError(format!("`sigma` is 0 to {} projector pixels", objects::MAX_SIGMA)));
    }

    let mut to_camera = Vec::new();
    let mut from_camera = Vec::new();
    let mut masks: Vec<Option<Vec<u16>>> = Vec::new();
    for (n, folder) in a.scans.iter().enumerate() {
        let folder = dir.join(folder);
        let p2c = read_map(&folder.join(PROJECTOR_MAP))?;
        let c2p = read_map(&folder.join(CAMERA_MAP))?;
        let mask = match a.masks.as_ref().and_then(|m| m.get(n)).and_then(Option::as_ref) {
            Some(file) => Some(read_labels(&dir.join(file), (p2c.width, p2c.height))?),
            None => None,
        };
        to_camera.push(p2c);
        from_camera.push(c2p);
        masks.push(mask);
    }
    let read = started.elapsed();
    let t = Instant::now();
    let inputs: Vec<objects::BlendInput<'_>> = to_camera
        .iter()
        .zip(&from_camera)
        .zip(&masks)
        .map(|((t, f), m)| objects::BlendInput { to_camera: t, from_camera: f, mask: m.as_deref() })
        .collect();
    let b = objects::blend(&inputs, &o).map_err(CommandError)?;
    let (overlap, error) = objects::coverage(&inputs, &b.weights);
    let blended = t.elapsed();
    let t = Instant::now();
    let smoothed: Vec<Vec<[f32; 4]>> = to_camera
        .iter()
        .zip(&masks)
        .map(|(m, mask)| objects::smooth(m, mask.as_deref(), sigma))
        .collect::<Result<_, _>>()
        .map_err(CommandError)?;
    let smoothing = t.elapsed();

    let t = Instant::now();
    let into_scans = a.into_scans.unwrap_or(false);
    if into_scans && a.out.is_some() {
        return Err("`out` and `into_scans` exclude each other".into());
    }
    let out = dir.join(a.out.as_deref().unwrap_or(Path::new(DEFAULT_OBJECTS_OUT)));
    if !into_scans {
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    let files = |n: usize| -> (PathBuf, PathBuf) {
        match a.scans.get(n).filter(|_| into_scans) {
            Some(scan) => {
                let folder = dir.join(scan);
                (
                    folder.join(penumbra_ops::tex::scan::WEIGHT),
                    folder.join(penumbra_ops::tex::scan::SMOOTH_MAP),
                )
            }
            None => (out.join(weight_file(n)), out.join(smooth_file(n))),
        }
    };
    let mut per_projector = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (n, ((map, w), smooth)) in to_camera.iter().zip(&b.weights).zip(&smoothed).enumerate() {
        let (pw, ph) = (map.width as u32, map.height as u32);
        // sRGB-encoded: tex.image decodes 16-bit files as sRGB, so it gives back the weight.
        let encode =
            |v: f32| (penumbra_gpu::readback::linear_to_srgb(v) * 65535.0).round().clamp(0.0, 65535.0) as u16;
        let values: Vec<u16> = w.iter().map(|v| encode(*v)).collect();
        let (weight_path, map_path) = files(n);
        let weight = write_file(&weight_path, penumbra_gpu::image::encode_png_gray16(pw, ph, &values)?)?;
        let map_file = write_file(&map_path, penumbra_gpu::image::encode_exr_rgba32f(pw, ph, smooth)?)?;
        let shared = b.shared.get(n).copied().unwrap_or(0);
        if shared == 0 && a.scans.len() > 1 {
            warnings.push(format!(
                "projector {n} shares no camera pixel with another: its weight is 1 everywhere"
            ));
        }
        per_projector.push(json!({
            "scan": a.scans.get(n).map(|s| s.display().to_string()),
            "size": [pw, ph],
            "shared": shared,
            "weight": weight,
            "map": map_file,
        }));
    }
    let written = t.elapsed();
    Ok(json!({
        "message": format!(
            "{} projector(s); {overlap} camera pixels lit by two or more, their weights sum to 1 within {:.3} on average",
            a.scans.len(),
            error
        ),
        "projectors": per_projector,
        "overlap": {"camera_pixels": overlap, "mean_error": error},
        "options": {"ramp": o.ramp, "blur": o.blur, "reach": o.reach, "sigma": sigma},
        "ms": {
            "read": ms(read), "blend": ms(blended), "smooth": ms(smoothing), "write": ms(written),
            "total": ms(started.elapsed()),
        },
        "warnings": warnings,
        "next": "per projector: tex.remap of the camera-space content through tex.image of \
                 proj_to_cam_smooth_N.exr (extend zero), tex.composite multiply with tex.image of \
                 weight_N.png, into out.projector",
    }))
}

/// The picture files directly in `dir`, sorted by name with numbers by value (`2.png` before
/// `10.png`).
fn pictures(dir: &Path) -> CmdResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if is_file && ext.is_some_and(|e| EXTENSIONS.contains(&e.as_str())) {
            files.push(path);
            if files.len() > MAX_FILES {
                return Err(CommandError(format!("{} holds more than {MAX_FILES} pictures", dir.display())));
            }
        }
    }
    files.sort_by(|a, b| {
        natural(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    Ok(files)
}

/// Compares names with runs of digits by value.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (ra, rb) = (run(&mut a), run(&mut b));
                let (ta, tb) = (ra.trim_start_matches('0'), rb.trim_start_matches('0'));
                let o =
                    ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| ra.len().cmp(&rb.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_with_numbers_by_value() {
        let mut v = vec!["10.png", "2.png", "img_011.jpg", "img_9.jpg", "1.png", "B.png", "a.png"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["1.png", "2.png", "10.png", "a.png", "B.png", "img_9.jpg", "img_011.jpg"]);
        assert_eq!(natural("007", "7"), Ordering::Greater);
    }

    fn map(w: usize, h: usize) -> objects::Map {
        objects::Map::new(w, h, vec![[0.5, 0.5, 1.0, 1.0]; w * h]).unwrap()
    }

    fn parts(labels: Vec<u16>, cuts: Vec<bool>, dropped: Vec<bool>) -> objects::Parts {
        let count = labels.iter().copied().max().unwrap_or(0) as usize;
        objects::Parts {
            width: labels.len(),
            height: 1,
            cuts,
            dropped,
            count,
            silhouette: 0,
            compared: labels.len(),
            over: 0,
            labels,
        }
    }

    #[test]
    fn decode_warnings_say_what_went_wrong() {
        assert!(decode_warnings(50, 100, 1, [3, 2], [3, 2]).is_empty());
        let w = decode_warnings(5, 100, 2, [3, 2], [2, 2]);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].starts_with("only 5 of 100 camera pixels decoded"));
        assert!(w[1].starts_with("the camera resolves 2 of 3 column and 2 of 2 row bits"));
        assert!(w[2].starts_with("more than 1 % of the camera picture clipped"));
    }

    #[test]
    fn object_options_and_the_smallest_part() {
        let args = |v: Json| -> ObjectsArgs { serde_json::from_value(v).unwrap() };
        let o = object_options(&args(json!({"projectors": [], "threshold": 10.0}))).unwrap();
        assert_eq!((o.threshold, o.min_area, o.erode), (10.0, 0, objects::ObjectOptions::default().erode));
        let err = object_options(&args(json!({"projectors": [], "split": 0.0}))).unwrap_err();
        assert!(err.0.contains("`split`"));
        assert_eq!(min_area_of(Some(7), 1_000_000), 7);
        assert_eq!(min_area_of(None, 2560), 10);
        assert_eq!(min_area_of(None, 100), 1);
    }

    #[test]
    fn object_scans_must_fit_each_other_and_one_camera() {
        let scans = |cam: (usize, usize), reference: (usize, usize)| ObjectScans {
            reference: map(reference.0, reference.1),
            scan: map(8, 4),
            camera: map(cam.0, cam.1),
        };
        let mut camera = None;
        assert_eq!(check_object_scans(0, &scans((16, 8), (8, 4)), &mut camera).unwrap(), (16, 8));
        assert_eq!(camera, Some((16, 8)));
        let err = check_object_scans(1, &scans((12, 8), (8, 4)), &mut camera).unwrap_err();
        assert!(
            err.0.starts_with("projector 1's camera picture is 12 x 8, projector 0's 16 x 8"),
            "{}",
            err.0
        );
        let err = check_object_scans(1, &scans((16, 8), (8, 5)), &mut camera).unwrap_err();
        assert!(err.0.starts_with("projector 1: the reference is 8 x 5, the scan 8 x 4"), "{}", err.0);
    }

    #[test]
    fn parts_and_objects_drawn_and_listed() {
        let p = parts(
            vec![0, 1, 1, 2, 300],
            vec![false, true, false, false, false],
            vec![false, false, false, true, false],
        );
        assert_eq!(label_bytes(&p.labels), [0, 1, 1, 2, 0]);
        let view = parts_view(&p);
        assert_eq!(view.len(), 5 * 4);
        assert_eq!(&view[0..4], &[0, 0, 0, 255]);
        let c1 = colour(1);
        assert_eq!(&view[4..8], &[c1[0] / 2, c1[1] / 2, c1[2] / 2, 255], "a cut is darker");
        assert_eq!(&view[8..12], &[c1[0], c1[1], c1[2], 255]);
        assert_eq!(&view[12..16], &[110, 0, 0, 255], "dropped");
        assert_eq!(labels_view(&[0, 1]), [0, 0, 0, 255, c1[0], c1[1], c1[2], 255]);
        // Colours a golden angle apart: neighbours differ.
        assert_ne!(colour(1), colour(2));

        let other = parts(vec![2, 2, 0], vec![false; 3], vec![false; 3]);
        let found = objects::Objects { count: 2, camera_y: vec![10.4, 30.6], over: 0 };
        let list = object_list(&found, &[p, other], &[1, 2, 2, 0]);
        assert_eq!(
            list,
            [
                json!({"object": 1, "camera_y": 10.0, "pixels": [2, 0], "seen_by": [0], "camera_pixels": 1}),
                json!({"object": 2, "camera_y": 31.0, "pixels": [1, 2], "seen_by": [0, 1], "camera_pixels": 2}),
            ]
        );
    }

    #[test]
    fn warnings_of_the_parts_and_the_objects() {
        let mut p = parts(vec![0; 20], vec![false; 20], vec![false; 20]);
        p.compared = 1;
        p.over = 3;
        let w = parts_warnings(2, &p);
        assert_eq!(w.len(), 2);
        assert!(w[0].starts_with("projector 2: only 1 of 20 pixels measured in both scans"));
        assert!(w[1].starts_with("projector 2: 3 parts beyond"));
        let none = objects::Objects { count: 0, camera_y: Vec::new(), over: 0 };
        assert_eq!(
            found_warnings(&none, 25.0),
            ["no objects found: nothing moved more than 25 camera pixels against the reference"]
        );
    }
}
