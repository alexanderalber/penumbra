//! `scan.synthetic`: the scans of a ray-cast room ([`penumbra_calib::synthetic`]), written as
//! `scan.decode` writes a real one, so `scan.scene`, `scan.relief` and the content built on
//! them can be tried and rehearsed without projectors or a camera.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::synthetic::Room;
use penumbra_gpu::Pixels;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan::{CAMERA_MAP, PROJECTOR_MAP, QUALITY, SCENE, ms, write_file};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Where `scan.synthetic` writes unless told otherwise (relative to the project).
pub const DEFAULT_SYNTHETIC_OUT: &str = "synthetic";
/// The camera's picture of the room lit white by one projector; a real decode does not write
/// it (its first captured frame is the nearest thing).
pub const PHOTO: &str = "photo.png";
/// The folders of the scans, one per projector.
pub const FOLDERS: [&str; 2] = ["a", "b"];

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.synthetic",
        "Writes the scans of a made-up room, ray cast, as scan.decode writes real ones: for trying \
         scan.scene and the content after it without projectors or a camera. The room \"pumpkin\" is a \
         landscape of five Euro crates with a ribbed pumpkin on one of them, two projectors left and right \
         of the camera, decode noise like a real scan's; \"landscape\" three crates and a ball, \"crate\" \
         one crate and a ball (both small and nearly noise free, as the tests use them). Writes per \
         projector a folder (a, b) with proj_to_cam.exr, cam_to_proj.exr, scene.png and quality.png, \
         and photo.png (the camera's picture of the room lit white by that projector, which a real decode \
         does not write). {room? (pumpkin, landscape or crate; pumpkin), out? (folder, synthetic), \
         scale? (pumpkin: camera and projectors are scale times 480 x 270 pixels, 0.25 to 8; 4, full HD), \
         noise? (camera px, the standard deviation of the decode noise per axis; pumpkin 0.15 times \
         scale), outliers? (share of pixels decoded far off, 0 to 0.1; pumpkin 0.002)}",
        move |p| {
            let a: SyntheticArgs = args(p)?;
            run(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SyntheticArgs {
    pub room: Option<String>,
    pub out: Option<PathBuf>,
    pub scale: Option<f64>,
    pub noise: Option<f64>,
    pub outliers: Option<f64>,
}

/// The room `a` asks for.
fn room(a: &SyntheticArgs) -> CmdResult<Room> {
    if a.scale.is_some_and(|s| !(0.25..=8.0).contains(&s)) {
        return Err("`scale` is 0.25 to 8".into());
    }
    let mut room = match a.room.as_deref().unwrap_or("pumpkin") {
        "pumpkin" => Room::pumpkin(a.scale.unwrap_or(4.0)),
        "landscape" | "crate" if a.scale.is_some() => {
            return Err("`scale` is for the pumpkin room only".into());
        }
        "landscape" => Room::landscape(),
        "crate" => Room::default(),
        other => return Err(CommandError(format!("no room `{other}`: pumpkin, landscape or crate"))),
    };
    if let Some(n) = a.noise {
        if !(0.0..=8.0).contains(&n) {
            return Err("`noise` is 0 to 8 camera pixels".into());
        }
        room.sigma = n;
        room.noise = 0.0;
    }
    if let Some(o) = a.outliers {
        if !(0.0..=0.1).contains(&o) {
            return Err("`outliers` is 0 to 0.1".into());
        }
        room.outliers = o;
    }
    Ok(room)
}

/// Writes the scans of the room `a` asks for into its folders (relative to `dir`).
pub fn run(dir: &Path, a: &SyntheticArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let room = room(a)?;
    let out = dir.join(a.out.as_deref().unwrap_or(Path::new(DEFAULT_SYNTHETIC_OUT)));
    let (cw, ch) = room.camera;
    let (pw, ph) = room.projector;
    let png =
        |w: usize, h: usize, data: Vec<[f32; 4]>| Pixels { width: w as u32, height: h as u32, data }.to_png();
    let mut scans = Vec::new();
    for (p, name) in FOLDERS.iter().enumerate() {
        let folder = out.join(name);
        std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let write = |file: &str, bytes: Vec<u8>| write_file(&folder.join(file), bytes);
        let (to_camera, _) = room.to_camera(p);
        let from_camera = room.from_camera(p);
        let photo = room.photo(p);
        // The scene as the projector sees it: the photo at each projector pixel's camera spot.
        let scene: Vec<[f32; 4]> = (0..to_camera.rgba.len())
            .map(|j| match to_camera.pixel_in(j, cw, ch).and_then(|c| photo.get(c)) {
                Some(c) => *c,
                None => [0.0; 4],
            })
            .collect();
        let quality: Vec<[f32; 4]> = from_camera
            .rgba
            .iter()
            .map(|m| if m[2] > 0.0 { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] })
            .collect();
        let lit = from_camera.rgba.iter().filter(|m| m[2] > 0.0).count();
        let seen = (0..to_camera.rgba.len()).filter(|j| to_camera.uv(*j).is_some()).count();
        let files = json!({
            "camera_map": write(CAMERA_MAP, penumbra_gpu::image::encode_exr_rgba32f(cw as u32, ch as u32, &from_camera.rgba)?)?,
            "projector_map": write(PROJECTOR_MAP, penumbra_gpu::image::encode_exr_rgba32f(pw as u32, ph as u32, &to_camera.rgba)?)?,
            "scene": write(SCENE, png(pw, ph, scene)?)?,
            "quality": write(QUALITY, png(cw, ch, quality)?)?,
            "photo": write(PHOTO, png(cw, ch, photo)?)?,
        });
        scans.push(json!({
            "folder": folder.display().to_string(),
            "camera_pixels": lit,
            "projector_pixels": seen,
            "files": files,
        }));
    }
    let folders: Vec<String> = FOLDERS
        .iter()
        .map(|f| a.out.as_deref().unwrap_or(Path::new(DEFAULT_SYNTHETIC_OUT)).join(f).display().to_string())
        .collect();
    Ok(json!({
        "message": format!("{} scans of a {} x {} projector seen by a {} x {} camera", FOLDERS.len(), pw, ph, cw, ch),
        "room": a.room.as_deref().unwrap_or("pumpkin"),
        "camera": [cw, ch],
        "projector": [pw, ph],
        "noise": {"sigma_px": room.sigma, "uniform_px": room.noise, "outliers": room.outliers},
        "scans": scans,
        "ms": ms(started.elapsed()),
        "next": format!("scan.scene {{scans: {}}}", json!(folders)),
    }))
}
