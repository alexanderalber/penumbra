//! Structured light in the engine. Commands: `scan.patterns` (the pictures of a scan, in order)
//! and `scan.decode` (camera pictures of them in a folder to maps and the scene as the projector
//! sees it, written as files).
//!
//! The decode is a one-off job on the caller's thread (the control channel's connection), never
//! on the cook thread; nothing of it runs per frame. Its results are plain files in the project,
//! read by `tex.image`: the projector-space map goes into `tex.remap`'s `map` with the camera as
//! `in`, so the camera's view lands in projector pixels.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

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
        "Decode a structured light scan: the camera's pictures of tex.scan_pattern 0, 1, 2, ... as \
         files in a folder (sorted by name, numbers by value), written as cam_to_proj.exr and \
         proj_to_cam.exr (maps for tex.remap), scene.png (the scene as the projector sees it) and \
         quality.png; {frames (folder, relative to the project), projector?: [w, h] (1920, 1080), \
         out? (folder, default frames/decoded), fill? (projector px, 32), options?: {min_contrast?, \
         saturation?, min_confidence?, tolerance?}}",
        move |p| {
            let a: DecodeArgs = args(p)?;
            decode(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecodeArgs {
    pub frames: PathBuf,
    pub projector: Option<[u32; 2]>,
    pub out: Option<PathBuf>,
    pub fill: Option<u32>,
    #[serde(default)]
    pub options: Options,
}

/// Decodes the pictures in `a.frames` (relative to `dir`) and writes the results; the reply
/// counts what was found.
pub fn decode(dir: &Path, a: &DecodeArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let [pw, ph] = a.projector.unwrap_or(DEFAULT_PROJECTOR);
    let patterns = Patterns::new(pw, ph).map_err(CommandError)?;
    a.options.check().map_err(CommandError)?;
    let fill = a.fill.unwrap_or(DEFAULT_FILL);
    if fill > scan::MAX_FILL {
        return Err(CommandError(format!("`fill` is 0 to {} projector pixels", scan::MAX_FILL)));
    }
    let frames = dir.join(&a.frames);
    let files = pictures(&frames)?;
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
    let read = started.elapsed();
    let decoder = decoder.ok_or("no pictures")?;
    let t = Instant::now();
    let result = decoder.finish(&a.options).map_err(CommandError)?;
    let inverse = result.inverse(fill);
    let decoded = t.elapsed();

    let out = match &a.out {
        Some(o) => dir.join(o),
        None => frames.join("decoded"),
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| -> CmdResult<String> {
        let file = out.join(name);
        std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(file.display().to_string())
    };
    let (cw, ch) = result.camera;
    let png = |w: u32, h: u32, data: Vec<[f32; 4]>| Pixels { width: w, height: h, data }.to_png();
    let files = json!({
        "camera_map": write(CAMERA_MAP, penumbra_gpu::image::encode_exr_rgba32f(cw, ch, &result.camera_map())?)?,
        "projector_map": write(PROJECTOR_MAP, penumbra_gpu::image::encode_exr_rgba32f(pw, ph, &inverse.projector_map())?)?,
        "scene": write(SCENE, png(pw, ph, inverse.scene())?)?,
        "quality": write(QUALITY, png(cw, ch, result.quality())?)?,
    });

    let stats = &result.stats;
    let total = result.status.len().max(1);
    let valid = stats.count(Status::Valid);
    let mut warnings: Vec<String> = Vec::new();
    if valid * 10 < total {
        warnings.push(format!(
            "only {valid} of {total} camera pixels decoded: is the projector in view, the room dark and the order right?"
        ));
    }
    if stats.resolved != stats.bits {
        warnings.push(format!(
            "the camera resolves {} of {} column and {} of {} row bits: the map is a few projector pixels coarse",
            stats.resolved[0], stats.bits[0], stats.resolved[1], stats.bits[1]
        ));
    }
    if stats.count(Status::Clipped) * 100 > total {
        warnings
            .push("more than 1 % of the camera picture clipped in white: lower the camera's exposure".into());
    }
    let counts: serde_json::Map<String, Json> =
        Status::ALL.iter().map(|s| (s.name().to_string(), json!(stats.count(*s)))).collect();
    let [measured, filled, empty] = inverse.counts();
    let ms = |d: std::time::Duration| (d.as_secs_f64() * 1000.0).round();
    Ok(json!({
        "message": format!(
            "{valid} of {total} camera pixels decoded; {measured} projector pixels seen, {filled} filled"
        ),
        "camera": [cw, ch],
        "projector": [pw, ph],
        "pictures": patterns.count(),
        "camera_pixels": counts,
        "projector_pixels": {"measured": measured, "filled": filled, "empty": empty},
        "bits": stats.bits,
        "resolved_bits": stats.resolved,
        "step": stats.step,
        "files": files,
        "ms": {"read": ms(read), "decode": ms(decoded), "total": ms(started.elapsed())},
        "warnings": warnings,
        "next": format!(
            "tex.image of {PROJECTOR_MAP} into tex.remap's map, the camera into its in: the camera's view in projector pixels"
        ),
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
    use super::natural;
    use std::cmp::Ordering;

    #[test]
    fn names_sort_with_numbers_by_value() {
        let mut v = vec!["10.png", "2.png", "img_011.jpg", "img_9.jpg", "1.png", "B.png", "a.png"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["1.png", "2.png", "10.png", "a.png", "B.png", "img_9.jpg", "img_011.jpg"]);
        assert_eq!(natural("007", "7"), Ordering::Greater);
    }
}
