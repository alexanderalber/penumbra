//! Content delivered per face. `scan.faces` lists the faces of a scene (`scan.scene`'s faces and
//! `scan.regions`' regions, by the names `tex.face_content` takes); `scan.face_template` writes,
//! per face, a folder with a frame for the designer to paint in (the face rectified, its mask,
//! a guide with the camera's photo of it and a grid in true squares, its aspect) and the uv maps
//! `tex.face_content` puts the delivered picture on the face with, in the camera's picture and
//! in each projector's.
//!
//! A face that is a quadrilateral (its corners given to `scan.scene`, the straight corners
//! `scan.regions` found, or four straight edges fitted to its border) is rectified: u and v run
//! along its edges, so a picture painted in the template lands undistorted. Its aspect comes from
//! the user, or is estimated from the perspective of its corners (a rectangle seen by a camera
//! with square pixels and the principal point in the middle). Any other face is framed by its
//! bounding box in the camera's picture: the template is then what the camera sees.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::Homography;
use penumbra_calib::objects::{self, Map};
use penumbra_calib::{regions, scene};
use penumbra_ops::tex::face_content::{FACES_FOLDER, FaceRef, UV_CAMERA, uv_projector};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan::{
    CAMERA_MAP, OBJECTS_CAMERA, PROJECTOR_MAP, labels_file, ms, read_labels, read_map, write_file,
};
use crate::scan_regions::{REGIONS_CAMERA, REGIONS_REPLY, regions_file};
use crate::scan_relief::round;
use crate::scan_scene::{DEFAULT_SCENE_OUT, PLANES_CAMERA, PLANES_CAMERA_MEASURED, SCENE_JSON, planes_file};

type CmdResult<T> = std::result::Result<T, CommandError>;
/// Four corners in camera pixels: far left first, clockwise.
type Quad = [[f64; 2]; 4];

/// Files in a face's folder besides the uv maps.
pub const FACE_JSON: &str = "face.json";
pub const MASK: &str = "mask.png";
pub const GUIDE: &str = "template.png";
pub const UV_PNG: &str = "uv.png";
/// The list of every face written, in the faces folder.
pub const FACES_JSON: &str = "faces.json";
/// The template's longer side unless asked otherwise, pixels.
pub const DEFAULT_SIZE: u32 = 1920;
const MAX_SIZE: u32 = 8192;
/// Light on a face this far outside its outline (in units of its size) still counts.
const UV_MARGIN: f64 = 0.01;
/// Camera pixels a projector's fit may miss by, for the smooth correction of a sagging face.
const TOLERANCE: f64 = 2.0;
/// A found outline frames the face when this share of the face lies inside it...
const OUTLINE_INSIDE: f64 = 0.9;
/// ... and this share of it is the face, an object in front of it or unmeasured.
const OUTLINE_FILL: f64 = 0.8;
/// The focal length assumed when the corners do not tell it, times the camera's width.
const ASSUMED_FOCAL: f64 = 1.5;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.faces",
        "The faces of a scene, by the names tex.face_content and scan.face_template take: every region of \
         scan.regions (by its name, else `region N`) and every face of scan.scene (`face N`), with its size \
         in camera pixels and whether its template exists. {scene? (folder scan.scene wrote, scene)}",
        move |p| {
            let a: FacesArgs = args(p)?;
            list(e.session().dir(), a.scene.as_deref())
        },
    ));
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.face_template",
        "Writes per face of a scene a folder (scene/faces/<face>) with a template for content delivered for \
         that face and the maps tex.face_content puts it on the face with. A face that is a quadrilateral is \
         rectified (u and v along its edges): its corners given to scan.scene, the straight corners \
         scan.regions found, or four edges fitted to its border; its aspect given, else estimated from the \
         perspective of the corners. Any other face is framed by its bounding box in the camera's picture. \
         Files: template.png (the face rectified at the template's size: the camera's photo of it, outside \
         dimmed, the face's edge and a grid in true squares), mask.png (white the face, grey an object in \
         front of it, black not the face), uv.png (camera size, 16-bit: red u, green v, blue 1 on the face, \
         for previewing a delivery on the camera's photo), uv_camera.exr and uv_N.exr per projector (as \
         scan.scene's uv files), face.json (name, size, aspect, corners); faces.json lists all written. \
         {scene? (folder, scene), faces? ([name or number, or {face, aspect? [width, height], corners? \
         [[x, y] x 4] camera px, far left first, clockwise}, ...]; all regions when the scene has any, else \
         all faces, leaving out those under min_pixels), size? (template's longer side, px, 1920), photo? \
         (the camera's picture of the scene; the first scan's photo.png or frames/00.png), min_pixels? \
         (camera px, a thousandth of the picture), focal? (the camera's focal length in pixels of its picture,          for the aspects; else told by the faces' corners where the camera sees them in perspective, or          assumed 1.5 times the picture's width)}",
        move |p| {
            let a: TemplateArgs = args(p)?;
            template(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FacesArgs {
    pub scene: Option<PathBuf>,
}

/// A face by its name or number.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum FaceName {
    Number(u16),
    Text(String),
}

impl FaceName {
    fn reference(&self) -> Option<FaceRef> {
        match self {
            FaceName::Number(n) => FaceRef::parse(&n.to_string()),
            FaceName::Text(t) => FaceRef::parse(t),
        }
    }
    fn text(&self) -> String {
        match self {
            FaceName::Number(n) => n.to_string(),
            FaceName::Text(t) => t.clone(),
        }
    }
}

/// A face to write, with its aspect and corners if given.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Pick {
    Name(FaceName),
    Full(PickFull),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickFull {
    pub face: FaceName,
    pub aspect: Option<[f64; 2]>,
    pub corners: Option<[[f64; 2]; 4]>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TemplateArgs {
    pub scene: Option<PathBuf>,
    pub faces: Option<Vec<Pick>>,
    pub size: Option<u32>,
    pub photo: Option<PathBuf>,
    pub min_pixels: Option<u64>,
    pub focal: Option<f64>,
}

impl TemplateArgs {
    /// The template of one face, everything else as by default.
    pub fn one(scene: &str, face: &str) -> Self {
        Self {
            scene: Some(PathBuf::from(scene)),
            faces: Some(vec![Pick::Name(FaceName::Text(face.to_string()))]),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Face,
    Region,
}

/// A face of a scene as listed: a face of `scan.scene` or a region of `scan.regions`.
#[derive(Clone, Debug)]
pub struct Surface {
    /// The name `tex.face_content` takes: `face 7`, a region's name, or `region 3`.
    pub name: String,
    pub kind: Kind,
    /// Its label in planes_camera.png or regions_camera.png.
    pub number: u16,
    pub camera_pixels: u64,
    /// A region's faces; a face's region.
    pub faces: Vec<u16>,
    pub region: Option<u16>,
    /// Corners known from the scan, and from where.
    pub corners: Option<([[f64; 2]; 4], &'static str)>,
    pub aspect: Option<[f64; 2]>,
    /// Its folder in the faces folder.
    pub folder: String,
}

impl Surface {
    fn json(&self, scene_dir: &Path) -> Json {
        let made = scene_dir.join(FACES_FOLDER).join(&self.folder).join(UV_CAMERA).exists();
        let mut j = json!({
            "name": self.name,
            "kind": match self.kind { Kind::Face => "face", Kind::Region => "region" },
            "number": self.number,
            "camera_pixels": self.camera_pixels,
            "folder": self.folder,
            "template": made,
        });
        if let Some(o) = j.as_object_mut() {
            if self.kind == Kind::Region {
                o.insert("faces".into(), json!(self.faces));
            }
            if let Some(r) = self.region {
                o.insert("region".into(), json!(r));
            }
        }
        j
    }
}

fn read_json(file: &Path, hint: &str) -> CmdResult<Json> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}{hint}", file.display()))?;
    serde_json::from_str(&text).map_err(|e| CommandError(format!("{}: {e}", file.display())))
}

fn point(v: &Json) -> Option<[f64; 2]> {
    Some([v.get(0)?.as_f64()?, v.get(1)?.as_f64()?])
}

fn quad(v: &Json) -> Option<[[f64; 2]; 4]> {
    let l = v.as_array()?;
    if l.len() != 4 {
        return None;
    }
    Some([point(l.first()?)?, point(l.get(1)?)?, point(l.get(2)?)?, point(l.get(3)?)?])
}

/// The scene folder `scene` names (relative to the project), `scan.scene`'s default without.
pub fn scene_dir(dir: &Path, scene: Option<&Path>) -> PathBuf {
    dir.join(scene.unwrap_or(Path::new(DEFAULT_SCENE_OUT)))
}

/// The faces and regions of the scene in `scene_dir`, regions first; with notes on names that
/// could not be used.
pub fn surfaces(scene_dir: &Path) -> CmdResult<(Vec<Surface>, Vec<String>)> {
    let scene = read_json(&scene_dir.join(SCENE_JSON), " (run scan.scene first)")?;
    let mut notes = Vec::new();
    let regions_file = scene_dir.join(REGIONS_REPLY);
    let regions = if regions_file.exists() { Some(read_json(&regions_file, "")?) } else { None };
    // Which region each face is in.
    let face_region = |face: u64| -> Option<u16> {
        regions
            .as_ref()?
            .get("faces")?
            .as_array()?
            .iter()
            .find(|f| f.get("face").and_then(Json::as_u64) == Some(face))?
            .get("region")?
            .as_u64()
            .and_then(|r| u16::try_from(r).ok())
            .filter(|r| *r > 0)
    };
    let rectified: Vec<&Json> =
        scene.get("rectified").and_then(Json::as_array).map(|l| l.iter().collect()).unwrap_or_default();
    // A face scan.scene rectified: its corners and aspect.
    let rectified_face = |face: u16| -> Option<(Quad, Option<[f64; 2]>)> {
        let r = rectified.iter().find(|r| r.get("plane").and_then(Json::as_u64) == Some(u64::from(face)))?;
        Some((r.get("corners").and_then(quad)?, r.get("aspect").and_then(point)))
    };
    let mut out = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    for r in regions.as_ref().and_then(|r| r.get("regions")).and_then(Json::as_array).into_iter().flatten() {
        let Some(number) = r.get("region").and_then(Json::as_u64).and_then(|n| u16::try_from(n).ok()) else {
            continue;
        };
        let fallback = FaceRef::Region(number);
        let given = r.get("name").and_then(Json::as_str).map(str::trim).filter(|n| !n.is_empty());
        let (name, reference) = match given {
            Some(n) => match FaceRef::parse(n) {
                Some(FaceRef::Named(_)) if !folders.contains(&FaceRef::Named(n.into()).folder()) => {
                    (n.to_string(), FaceRef::Named(n.into()))
                }
                _ => {
                    notes.push(format!(
                        "region {number}'s name {n:?} is taken or reads as a number: it goes by `region {number}`"
                    ));
                    (format!("region {number}"), fallback)
                }
            },
            None => (format!("region {number}"), fallback),
        };
        let folder = reference.folder();
        folders.push(folder.clone());
        let faces: Vec<u16> = r
            .get("faces")
            .and_then(Json::as_array)
            .map(|l| l.iter().filter_map(|f| f.as_u64().and_then(|f| u16::try_from(f).ok())).collect())
            .unwrap_or_default();
        // A face of it that scan.scene rectified gives its corners and aspect (a lid claimed by
        // its corners), else the straight corners scan.regions found.
        let from_scene = faces.iter().find_map(|f| rectified_face(*f));
        let (corners, aspect) = match from_scene {
            Some((c, a)) => (Some((c, "scan.scene")), a),
            None => (r.get("straight").and_then(quad).map(|q| (q, "scan.regions")), None),
        };
        out.push(Surface {
            name,
            kind: Kind::Region,
            number,
            camera_pixels: r.get("camera_pixels").and_then(Json::as_u64).unwrap_or(0),
            faces,
            region: None,
            corners,
            aspect,
            folder,
        });
    }
    for p in scene.get("planes").and_then(Json::as_array).into_iter().flatten() {
        let Some(number) = p.get("plane").and_then(Json::as_u64).and_then(|n| u16::try_from(n).ok()) else {
            continue;
        };
        let r = rectified.iter().find(|r| r.get("plane").and_then(Json::as_u64) == Some(u64::from(number)));
        let reference = FaceRef::Face(number);
        out.push(Surface {
            name: format!("face {number}"),
            kind: Kind::Face,
            number,
            camera_pixels: p.get("camera_pixels").and_then(Json::as_u64).unwrap_or(0),
            faces: Vec::new(),
            region: face_region(u64::from(number)),
            corners: r.and_then(|r| r.get("corners")).and_then(quad).map(|q| (q, "scan.scene")),
            aspect: r.and_then(|r| r.get("aspect")).and_then(point),
            folder: reference.folder(),
        });
    }
    Ok((out, notes))
}

/// The surface `name` names.
pub fn find<'a>(list: &'a [Surface], name: &str) -> Option<&'a Surface> {
    let r = FaceRef::parse(name)?;
    match r {
        FaceRef::Face(n) => list.iter().find(|s| s.kind == Kind::Face && s.number == n),
        FaceRef::Region(n) => list.iter().find(|s| s.kind == Kind::Region && s.number == n),
        FaceRef::Named(_) => {
            let folder = r.folder();
            list.iter().find(|s| s.kind == Kind::Region && s.folder == folder)
        }
    }
}

/// `scan.faces`.
pub fn list(dir: &Path, scene: Option<&Path>) -> CmdResult<Json> {
    let scene_dir = scene_dir(dir, scene);
    let (list, notes) = surfaces(&scene_dir)?;
    let regions = list.iter().filter(|s| s.kind == Kind::Region).count();
    let faces = list.len() - regions;
    Ok(json!({
        "message": format!("{regions} region(s), {faces} face(s)"),
        "scene": scene_dir.display().to_string(),
        "faces": list.iter().map(|s| s.json(&scene_dir)).collect::<Vec<_>>(),
        "notes": notes,
    }))
}

/// What `scan.face_template` reads once for every face.
struct Scene {
    dir: PathBuf,
    camera: (usize, usize),
    to_camera: Vec<Map>,
    from_camera: Vec<Map>,
    planes: Vec<u16>,
    /// The faces as measured, before their borders were straightened: their planes are fitted
    /// on these.
    planes_measured: Vec<u16>,
    regions: Option<Vec<u16>>,
    objects: Vec<u16>,
    proj_planes: Vec<Vec<u16>>,
    proj_regions: Vec<Option<Vec<u16>>>,
    proj_objects: Vec<Vec<u16>>,
    /// The camera's picture of the scene (values as stored, 0 to 1) and its size.
    photo: Option<(Vec<[f32; 4]>, usize, usize)>,
    photo_file: Option<PathBuf>,
}

fn read_scene(dir: &Path, scene_dir: &Path, photo: Option<&Path>) -> CmdResult<Scene> {
    let scene = read_json(&scene_dir.join(SCENE_JSON), " (run scan.scene first)")?;
    let size = |v: &Json| -> Option<(usize, usize)> {
        Some((usize::try_from(v.get(0)?.as_u64()?).ok()?, usize::try_from(v.get(1)?.as_u64()?).ok()?))
    };
    let camera = scene.get("camera").and_then(size).ok_or("scene.json names no camera size")?;
    if camera.0 == 0 || camera.1 == 0 || camera.0.saturating_mul(camera.1) > objects::MAX_PIXELS {
        return Err("scene.json's camera size is out of range".into());
    }
    let scans: Vec<PathBuf> = scene
        .get("projectors")
        .and_then(Json::as_array)
        .map(|l| l.iter().filter_map(|p| p.get("scan").and_then(Json::as_str).map(|s| dir.join(s))).collect())
        .unwrap_or_default();
    if scans.is_empty() || scans.len() > objects::MAX_PROJECTORS {
        return Err("scene.json lists no projectors' scans".into());
    }
    let mut s = Scene {
        dir: scene_dir.to_path_buf(),
        camera,
        to_camera: Vec::new(),
        from_camera: Vec::new(),
        planes: read_labels(&scene_dir.join(PLANES_CAMERA), camera)?,
        planes_measured: Vec::new(),
        regions: None,
        objects: read_labels(&scene_dir.join(OBJECTS_CAMERA), camera)?,
        proj_planes: Vec::new(),
        proj_regions: Vec::new(),
        proj_objects: Vec::new(),
        photo: None,
        photo_file: None,
    };
    let measured = scene_dir.join(PLANES_CAMERA_MEASURED);
    s.planes_measured = if measured.exists() { read_labels(&measured, camera)? } else { s.planes.clone() };
    let regions_camera = scene_dir.join(REGIONS_CAMERA);
    if regions_camera.exists() {
        s.regions = Some(read_labels(&regions_camera, camera)?);
    }
    for (n, folder) in scans.iter().enumerate() {
        let t = read_map(&folder.join(PROJECTOR_MAP))?;
        let f = read_map(&folder.join(CAMERA_MAP))?;
        if (f.width, f.height) != camera {
            return Err(CommandError(format!(
                "{}: not the camera's size",
                folder.join(CAMERA_MAP).display()
            )));
        }
        let size = (t.width, t.height);
        s.proj_planes.push(read_labels(&scene_dir.join(planes_file(n)), size)?);
        s.proj_objects.push(read_labels(&scene_dir.join(labels_file(n)), size)?);
        let r = scene_dir.join(regions_file(n));
        s.proj_regions.push(if s.regions.is_some() && r.exists() {
            Some(read_labels(&r, size)?)
        } else {
            None
        });
        s.to_camera.push(t);
        s.from_camera.push(f);
    }
    // The photo: asked for, else the first scan's.
    let candidates: Vec<PathBuf> = match photo {
        Some(p) => vec![dir.join(p)],
        None => scans
            .first()
            .map(|f| vec![f.join(crate::scan_synthetic::PHOTO), f.join("frames").join("00.png")])
            .unwrap_or_default(),
    };
    for file in candidates {
        if !file.exists() {
            if photo.is_some() {
                return Err(CommandError(format!("{}: no such file", file.display())));
            }
            continue;
        }
        let raw = penumbra_gpu::image::decode_file_raw(&file, 16384, objects::MAX_PIXELS as u64)?;
        s.photo = Some((raw.rgba, raw.width as usize, raw.height as usize));
        s.photo_file = Some(file);
        break;
    }
    Ok(s)
}

/// The aspect (width along the first edge over depth along the last) of a rectangle whose
/// corners the camera sees at `c` (far left first, clockwise), for a camera with square pixels
/// and its principal point at `principal`. With `focal` (pixels) that focal length is used;
/// without, the corners tell it, returned too, where they can (`width`, the camera picture's,
/// bounds what is plausible). They cannot when two of the rectangle's edges are parallel in the
/// picture (the camera looks square at it, from straight ahead or tilted only up or down); then
/// the focal length is assumed to be [`ASSUMED_FOCAL`] times `width`, and returned as `None`.
pub fn rectangle_aspect(
    c: [[f64; 2]; 4],
    principal: [f64; 2],
    width: f64,
    focal: Option<f64>,
) -> (f64, Option<f64>) {
    let m = |p: [f64; 2]| [p[0] - principal[0], p[1] - principal[1], 1.0];
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    };
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    // Zhang and He's whiteboard rectification: m1 the first corner, m2 the end of the width,
    // m3 the end of the depth, m4 the corner across.
    let (m1, m2, m4, m3) = (m(c[0]), m(c[1]), m(c[2]), m(c[3]));
    let k2 = dot(cross(m1, m4), m3) / dot(cross(m2, m4), m3);
    let k3 = dot(cross(m1, m4), m2) / dot(cross(m3, m4), m2);
    let n2 = [k2 * m2[0] - m1[0], k2 * m2[1] - m1[1], k2 * m2[2] - m1[2]];
    let n3 = [k3 * m3[0] - m1[0], k3 * m3[1] - m1[1], k3 * m3[2] - m1[2]];
    let ratio = |f2: f64| {
        let r = ((n2[0] * n2[0] + n2[1] * n2[1] + f2 * n2[2] * n2[2])
            / (n3[0] * n3[0] + n3[1] * n3[1] + f2 * n3[2] * n3[2]))
            .sqrt();
        if r.is_finite() && r > 0.0 { r } else { 1.0 }
    };
    if let Some(f) = focal.filter(|f| f.is_finite() && *f > 0.0) {
        return (ratio(f * f), None);
    }
    let denom = n2[2] * n3[2];
    let f2 = -(n2[0] * n3[0] + n2[1] * n3[1]) / denom;
    let plausible = (0.2 * width)..=(20.0 * width);
    // Edges nearly parallel in the picture leave the focal length to the noise of the corners.
    let parallel = n2[2].abs().max(n3[2].abs()) < 0.02 || n2[2].abs().min(n3[2].abs()) < 0.01;
    if !parallel && denom.abs() > 1e-12 && f2.is_finite() && f2 > 0.0 && plausible.contains(&f2.sqrt()) {
        return (ratio(f2), Some(f2.sqrt()));
    }
    let f = ASSUMED_FOCAL * width;
    (ratio(f * f), None)
}

/// Bilinear sample of an RGBA picture at pixel position `p` (centres at .5), edges held.
fn sample(rgba: &[[f32; 4]], w: usize, h: usize, p: [f64; 2]) -> [f32; 4] {
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let x = (p[0] - 0.5).clamp(0.0, (w - 1) as f64);
    let y = (p[1] - 0.5).clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let at = |x: usize, y: usize| rgba.get(y * w + x).copied().unwrap_or([0.0; 4]);
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0.0; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let v = |p: [f32; 4]| p.get(k).copied().unwrap_or(0.0);
        let top = v(a) + (v(b) - v(a)) * fx;
        let bottom = v(c) + (v(d) - v(c)) * fx;
        *o = top + (bottom - top) * fy;
    }
    out
}

/// What a pixel shows for the uv maps' blue: 1 the face, 0.5 an object in front of it, 0.25
/// another face. Inside a rectified face's outline, unmeasured pixels are the face (a dark or
/// glossy part of it) and an object stands in front of it; inside a bounding box neither is
/// known, so both count as not the face.
fn cover(face: u16, label: u16, object: u16, rectified: bool) -> f32 {
    if label == face {
        1.0
    } else if object > 0 && rectified {
        0.5
    } else if label == 0 && rectified {
        1.0
    } else {
        0.25
    }
}

fn inside(uv: [f64; 2]) -> bool {
    let r = -UV_MARGIN..=1.0 + UV_MARGIN;
    r.contains(&uv[0]) && r.contains(&uv[1])
}

/// Writes `bytes` under a temporary name first, so a node watching `file` never reads half of it.
fn write_whole(file: &Path, bytes: Vec<u8>) -> CmdResult<String> {
    let tmp = file.with_extension("tmp");
    write_file(&tmp, bytes)?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file.display().to_string())
}

/// `scan.face_template`.
pub fn template(dir: &Path, a: &TemplateArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let size = a.size.unwrap_or(DEFAULT_SIZE);
    if !(16..=MAX_SIZE).contains(&size) {
        return Err(CommandError(format!("`size` is 16 to {MAX_SIZE} pixels")));
    }
    if a.focal.is_some_and(|f| !(f.is_finite() && f > 0.0 && f < 1e6)) {
        return Err("`focal` is the camera's focal length in pixels, a positive number".into());
    }
    let scene_dir = scene_dir(dir, a.scene.as_deref());
    let (list, mut warnings) = surfaces(&scene_dir)?;
    // The faces asked for, else every region (or every face) of some size.
    let mut picks: Vec<(Surface, Option<[f64; 2]>, Option<Quad>)> = Vec::new();
    match &a.faces {
        Some(asked) => {
            for p in asked {
                let (name, aspect, corners) = match p {
                    Pick::Name(n) => (n, None, None),
                    Pick::Full(f) => (&f.face, f.aspect, f.corners),
                };
                if name.reference().is_none() {
                    return Err("a face is named by its name or number, not an empty text".into());
                }
                if aspect.is_some_and(|a| !a.iter().all(|v| v.is_finite() && *v > 0.0 && *v < 1e6)) {
                    return Err("a face's `aspect` is two positive numbers, its width and height".into());
                }
                if corners.is_some_and(|c| c.iter().flatten().any(|v| !v.is_finite())) {
                    return Err("a face's `corners` are four [x, y] pairs of numbers".into());
                }
                let Some(s) = find(&list, &name.text()) else {
                    return Err(CommandError(format!(
                        "no face {:?} in {} (scan.faces lists them)",
                        name.text(),
                        scene_dir.display()
                    )));
                };
                picks.push((s.clone(), aspect, corners));
            }
        }
        None => {
            let has_regions = list.iter().any(|s| s.kind == Kind::Region);
            let kind = if has_regions { Kind::Region } else { Kind::Face };
            let scene = read_json(&scene_dir.join(SCENE_JSON), "")?;
            let pixels = scene
                .get("camera")
                .and_then(|c| Some(c.get(0)?.as_u64()? * c.get(1)?.as_u64()?))
                .unwrap_or(0);
            let min = a.min_pixels.unwrap_or(pixels / 1000);
            let mut left_out = 0;
            for s in list.iter().filter(|s| s.kind == kind) {
                if s.camera_pixels < min {
                    left_out += 1;
                    continue;
                }
                picks.push((s.clone(), None, None));
            }
            if left_out > 0 {
                warnings.push(format!("{left_out} face(s) under {min} camera px left out (min_pixels)"));
            }
        }
    }
    if picks.is_empty() {
        return Err("no faces to write".into());
    }
    let scene = read_scene(dir, &scene_dir, a.photo.as_deref())?;
    let read = started.elapsed();
    let faces_dir = scene_dir.join(FACES_FOLDER);
    std::fs::create_dir_all(&faces_dir).map_err(|e| format!("{}: {e}", faces_dir.display()))?;
    // The frames first: the focal lengths the faces' corners tell stand in for faces whose
    // corners do not tell one.
    let (cw, ch) = scene.camera;
    let principal = [cw as f64 / 2.0, ch as f64 / 2.0];
    let mut frames = Vec::new();
    let mut told = Vec::new();
    for (s, aspect, corners) in &picks {
        match frame(&scene, s, *corners) {
            Ok(f) => {
                if f.rectified
                    && aspect.is_none()
                    && let (_, Some(px)) = rectangle_aspect(f.corners, principal, cw as f64, None)
                {
                    told.push(px);
                }
                frames.push(Some(f));
            }
            Err(e) => {
                warnings.push(format!("{}: {e}; no template", s.name));
                frames.push(None);
            }
        }
    }
    told.sort_by(f64::total_cmp);
    let focal = match (a.focal, told.get(told.len() / 2)) {
        (Some(px), _) => Focal { px, from: "given", given: true },
        (None, Some(px)) => {
            Focal { px: *px, from: "estimated with the focal length other faces tell", given: false }
        }
        (None, None) => {
            Focal { px: ASSUMED_FOCAL * cw as f64, from: "estimated, focal length assumed", given: false }
        }
    };
    let mut written = Vec::new();
    for ((s, aspect, _), f) in picks.iter().zip(&frames) {
        let Some(f) = f else { continue };
        match face(&scene, s, f, *aspect, focal, size) {
            Ok((j, w)) => {
                written.push(j);
                warnings.extend(w.into_iter().map(|w| format!("{}: {w}", s.name)));
            }
            Err(e) => warnings.push(format!("{}: {e}; no template", s.name)),
        }
    }
    if written.is_empty() {
        return Err(CommandError(format!("no template written: {}", warnings.join("; "))));
    }
    // The list of every face with a template, those written before included.
    let index_file = faces_dir.join(FACES_JSON);
    let mut index: Vec<Json> = std::fs::read_to_string(&index_file)
        .ok()
        .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        .and_then(|j| j.get("faces").and_then(Json::as_array).cloned())
        .unwrap_or_default();
    for j in &written {
        index.retain(|x| x.get("folder") != j.get("folder"));
        index.push(json!({
            "name": j["name"], "folder": j["folder"], "size": j["size"], "aspect": j["aspect"], "frame": j["frame"],
        }));
    }
    let index_json = json!({"scene": scene_dir.display().to_string(), "faces": index});
    write_file(
        &index_file,
        serde_json::to_vec_pretty(&index_json).map_err(|e| CommandError(e.to_string()))?,
    )?;
    Ok(json!({
        "message": format!("{} template(s) in {}", written.len(), faces_dir.display()),
        "faces": written,
        "photo": scene.photo_file.as_ref().map(|p| p.display().to_string()),
        "warnings": warnings,
        "ms": {"read": ms(read), "total": ms(started.elapsed())},
        "next": "deliver a picture or video per face in its template's size (or any size of its aspect); \
                 tex.face_content with `face` set to the face's name and the delivery as its input puts it \
                 on the face, in the camera's picture or straight in a projector's (`projector`)",
    }))
}

/// The labels a face is found by: the camera's and each projector's.
fn labels_of(scene: &Scene, kind: Kind) -> CmdResult<(&[u16], Vec<&[u16]>)> {
    Ok(match kind {
        Kind::Face => (&scene.planes, scene.proj_planes.iter().map(Vec::as_slice).collect()),
        Kind::Region => {
            let labels = scene.regions.as_deref().ok_or("the scene has no regions_camera.png")?;
            let proj = scene
                .proj_regions
                .iter()
                .map(|r| r.as_deref().ok_or("the scene has a projector without regions_N.png"))
                .collect::<Result<Vec<_>, _>>()?;
            (labels, proj)
        }
    })
}

/// Where a face's template lies in the camera's picture.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Its corners: far left first, clockwise.
    pub corners: [[f64; 2]; 4],
    /// Given, from `scan.scene` or `scan.regions`, found, or the bounding box.
    pub from: &'static str,
    /// A quadrilateral the face fills (u and v along its edges), else the bounding box.
    pub rectified: bool,
    /// The face's camera pixels.
    pub pixels: Vec<usize>,
    /// The bounding box's width and height.
    pub bounds: [f64; 2],
    /// Why no outline frames it, when it is the bounding box.
    pub note: Option<String>,
}

/// The frame of face `s`: its corners given, known from the scan, or found (four straight edges
/// fitted to its border, kept when the face fills them), else its bounding box.
fn frame(scene: &Scene, s: &Surface, corners: Option<[[f64; 2]; 4]>) -> CmdResult<Frame> {
    let (cw, ch) = scene.camera;
    let (labels, _) = labels_of(scene, s.kind)?;
    let k = s.number;
    let pixels: Vec<usize> = labels.iter().enumerate().filter(|(_, l)| **l == k).map(|(i, _)| i).collect();
    if pixels.is_empty() {
        return Err("not in the camera's picture".into());
    }
    let (mut x0, mut y0, mut x1, mut y1) = (cw, ch, 0usize, 0usize);
    for i in &pixels {
        let (x, y) = (i % cw, i / cw);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let (bx0, by0, bx1, by1) = (x0 as f64, y0 as f64, x1 as f64 + 1.0, y1 as f64 + 1.0);
    let bounds = [[bx0, by0], [bx1, by0], [bx1, by1], [bx0, by1]];
    let centre = |i: usize| [(i % cw) as f64 + 0.5, (i / cw) as f64 + 0.5];
    // The share of the face inside the outline, and of the outline the face (or an object in
    // front of it, or nothing measured) fills.
    let shares = |c: [[f64; 2]; 4]| -> Option<(f64, f64)> {
        let t = Homography::square_to_quad(c).ok().and_then(|h| h.inverse())?;
        let on = pixels.iter().filter(|i| t.apply(centre(**i)).is_some_and(inside)).count();
        let (mut quad, mut filled) = (0usize, 0usize);
        for (i, l) in labels.iter().enumerate() {
            if t.apply(centre(i)).is_some_and(inside) {
                quad += 1;
                if *l == k || *l == 0 || scene.objects.get(i).is_some_and(|o| *o > 0) {
                    filled += 1;
                }
            }
        }
        Some((on as f64 / pixels.len().max(1) as f64, filled as f64 / quad.max(1) as f64))
    };
    let mut note = None;
    let (corners, from, rectified) = match (corners, s.corners) {
        (Some(c), _) => (c, "given", true),
        (None, Some((c, from))) => (c, from, true),
        (None, None) => match scene::face_outline(cw, ch, labels, k, &scene.objects) {
            Ok(o) => match shares(o.corners) {
                Some((on, filled)) if on >= OUTLINE_INSIDE && filled >= OUTLINE_FILL => {
                    (o.corners, "found", true)
                }
                Some((on, filled)) => {
                    note = Some(format!(
                        "no quadrilateral: {:.0} % of it inside four fitted edges, which it fills to {:.0} %",
                        on * 100.0,
                        filled * 100.0
                    ));
                    (bounds, "bounds", false)
                }
                None => {
                    note = Some("no quadrilateral: the fitted edges cross".into());
                    (bounds, "bounds", false)
                }
            },
            Err(e) => {
                note = Some(format!("no quadrilateral: {e}"));
                (bounds, "bounds", false)
            }
        },
    };
    Ok(Frame { corners, from, rectified, pixels, bounds: [bx1 - bx0, by1 - by0], note })
}

/// The focal length a face's aspect is estimated with when its corners do not tell one, and
/// where it comes from.
#[derive(Clone, Copy, Debug)]
pub struct Focal {
    pub px: f64,
    pub from: &'static str,
    /// Given: used for every face, also where the corners would tell one.
    pub given: bool,
}

/// One face's folder: its template, mask and maps.
fn face(
    scene: &Scene,
    s: &Surface,
    frame: &Frame,
    aspect: Option<[f64; 2]>,
    focal: Focal,
    size: u32,
) -> CmdResult<(Json, Vec<String>)> {
    let mut warnings = Vec::new();
    let (cw, ch) = scene.camera;
    let (labels, proj_labels) = labels_of(scene, s.kind)?;
    let k = s.number;
    let pixels = &frame.pixels;
    let (frame_corners, corners_from, rectified) = (frame.corners, frame.from, frame.rectified);
    let to_face = Homography::square_to_quad(frame_corners)
        .ok()
        .and_then(|h| h.inverse())
        .ok_or("its corners do not form a convex quadrilateral")?;
    let from_face = Homography::square_to_quad(frame_corners).map_err(|e| CommandError(e.0))?;
    let principal = [cw as f64 / 2.0, ch as f64 / 2.0];
    let (aspect, aspect_from, focal_used) = match (aspect, s.aspect) {
        (Some(a), _) => (a, "given", None),
        (None, Some(a)) => (a, "scan.scene", None),
        (None, None) if rectified && focal.given => {
            let (r, _) = rectangle_aspect(frame_corners, principal, cw as f64, Some(focal.px));
            ([r, 1.0], "estimated with the focal length given", Some(focal.px))
        }
        (None, None) if rectified => match rectangle_aspect(frame_corners, principal, cw as f64, None) {
            (r, Some(f)) => ([r, 1.0], "estimated", Some(f)),
            _ => {
                let (r, _) = rectangle_aspect(frame_corners, principal, cw as f64, Some(focal.px));
                ([r, 1.0], focal.from, Some(focal.px))
            }
        },
        (None, None) => (frame.bounds, "the camera's picture", None),
    };
    let focal = focal_used;
    let ratio = aspect[0] / aspect[1];
    let (tw, th) = if ratio >= 1.0 {
        (size, ((f64::from(size) / ratio).round() as u32).clamp(1, MAX_SIZE))
    } else {
        (((f64::from(size) * ratio).round() as u32).clamp(1, MAX_SIZE), size)
    };
    let folder = scene.dir.join(FACES_FOLDER).join(&s.folder);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;

    // The camera's uv, and the same as a 16-bit PNG.
    let mut cam = Vec::with_capacity(cw * ch);
    let mut cam_png = Vec::with_capacity(cw * ch * 3);
    for (i, l) in labels.iter().enumerate() {
        let c = [(i % cw) as f64 + 0.5, (i / cw) as f64 + 0.5];
        match to_face.apply(c).filter(|uv| inside(*uv)) {
            Some([u, v]) => {
                let b = cover(k, *l, scene.objects.get(i).copied().unwrap_or(0), rectified);
                cam.push([u as f32, v as f32, b, 1.0]);
                let q = |t: f64| (t.clamp(0.0, 1.0) * 65535.0).round() as u16;
                cam_png.extend([q(u), q(v), q(f64::from(b))]);
            }
            None => {
                cam.push([-1.0, -1.0, 0.0, 1.0]);
                cam_png.extend([0, 0, 0]);
            }
        }
    }
    let (cwu, chu) = (cw as u32, ch as u32);
    let uv_camera =
        write_whole(&folder.join(UV_CAMERA), penumbra_gpu::image::encode_exr_rgba32f(cwu, chu, &cam)?)?;
    let uv_png =
        write_file(&folder.join(UV_PNG), penumbra_gpu::image::encode_png_rgb16(cwu, chu, &cam_png)?)?;

    // Per projector: its plane through the face's pixels, corrected where the face sags.
    let single: Vec<u16> = labels.iter().map(|l| u16::from(*l == k)).collect();
    let maps: Vec<&Map> = scene.from_camera.iter().collect();
    let sizes: Vec<(usize, usize)> = scene.to_camera.iter().map(|m| (m.width, m.height)).collect();
    let fitted = regions::fit_faces(scene.camera, &single, 1, &maps, &sizes);
    // A region's faces are planes of their own (a crate's ribs stand proud of its panels): each
    // projector pixel on one of them is mapped through that face's plane, the rest through the
    // region's.
    let parts: &[u16] = if s.kind == Kind::Region { &s.faces } else { &[] };
    let part_of = |l: u16| parts.iter().position(|f| *f == l);
    let part_fits = if parts.is_empty() {
        Vec::new()
    } else {
        let part_labels: Vec<u16> =
            scene.planes_measured.iter().map(|l| part_of(*l).map_or(0, |j| j as u16 + 1)).collect();
        regions::fit_faces(scene.camera, &part_labels, parts.len(), &maps, &sizes)
    };
    let mut projectors = Vec::new();
    for (p, map) in scene.to_camera.iter().enumerate() {
        let (pw, ph) = (map.width, map.height);
        let fit = fitted.first().and_then(|f| f.get(p)).and_then(Option::as_ref);
        let face_map =
            fit.map(|f| scene::face_map(map, scene.camera, &f.homography, &to_face, TOLERANCE, UV_MARGIN));
        let part_maps: Vec<Option<scene::FaceMap>> = part_fits
            .iter()
            .map(|f| {
                let f = f.get(p)?.as_ref()?;
                Some(scene::face_map(map, scene.camera, &f.homography, &to_face, TOLERANCE, UV_MARGIN))
            })
            .collect();
        let pl = proj_labels.get(p).copied().unwrap_or(&[]);
        let po = scene.proj_objects.get(p).map(Vec::as_slice).unwrap_or(&[]);
        let pf = scene.proj_planes.get(p).map(Vec::as_slice).unwrap_or(&[]);
        let rgba: Vec<[f32; 4]> = (0..pw * ph)
            .map(|i| {
                let q = [(i % pw) as f64 + 0.5, (i / pw) as f64 + 0.5];
                let own = pf.get(i).and_then(|l| part_of(*l)).and_then(|j| part_maps.get(j)?.as_ref());
                let m = own.or(face_map.as_ref());
                let uv = m.and_then(|m| m.apply(q)).and_then(|c| to_face.apply(c)).filter(|uv| inside(*uv));
                match uv {
                    Some([u, v]) => {
                        let b = cover(
                            k,
                            pl.get(i).copied().unwrap_or(0),
                            po.get(i).copied().unwrap_or(0),
                            rectified,
                        );
                        [u as f32, v as f32, b, 1.0]
                    }
                    None => [-1.0, -1.0, 0.0, 1.0],
                }
            })
            .collect();
        let file = write_whole(
            &folder.join(uv_projector(p)),
            penumbra_gpu::image::encode_exr_rgba32f(pw as u32, ph as u32, &rgba)?,
        )?;
        if face_map.is_none() {
            warnings.push(format!("projector {p} does not light it (its uv map is empty)"));
        }
        projectors.push(json!({
            "uv": file,
            "lit": face_map.is_some(),
            "fit": match (fit, &face_map) {
                (Some(f), Some(m)) => json!({"pixels": f.pixels, "error_px": round(f.error_px), "corrected": m.corrected()}),
                _ => Json::Null,
            },
        }));
    }

    // The template: the face rectified, its mask (2 x 2 samples per pixel), a guide.
    let (tws, ths) = (tw as usize, th as usize);
    let mut mask = Vec::with_capacity(tws * ths);
    let mut guide = Vec::with_capacity(tws * ths * 4);
    let cell = (th.min(tw) as f64 / 8.0).max(4.0);
    let photo_at = |c: [f64; 2]| -> Option<[f32; 4]> {
        let (rgba, pw, ph) = scene.photo.as_ref()?;
        let sx = *pw as f64 / cw as f64;
        let sy = *ph as f64 / ch as f64;
        Some(sample(rgba, *pw, *ph, [c[0] * sx, c[1] * sy]))
    };
    let what = |c: [f64; 2]| -> f32 {
        let (x, y) = (c[0].floor(), c[1].floor());
        if x < 0.0 || y < 0.0 || x >= cw as f64 || y >= ch as f64 {
            return 0.0;
        }
        let i = y as usize * cw + x as usize;
        match cover(
            k,
            labels.get(i).copied().unwrap_or(0),
            scene.objects.get(i).copied().unwrap_or(0),
            rectified,
        ) {
            b if b > 0.9 => 1.0,
            b if b > 0.4 => 0.5,
            _ => 0.0,
        }
    };
    let mut values = vec![0f32; tws * ths];
    for y in 0..ths {
        for x in 0..tws {
            let mut sum = 0.0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let uv = [(x as f64 + dx) / f64::from(tw), (y as f64 + dy) / f64::from(th)];
                sum += from_face.apply(uv).map_or(0.0, what);
            }
            let m = sum / 4.0;
            if let Some(v) = values.get_mut(y * tws + x) {
                *v = m;
            }
            mask.push((m * 255.0).round() as u8);
        }
    }
    // The photo at the template's pixels, brightened so the face's brighter parts show (a dark
    // face photographs nearly black).
    let photo: Vec<Option<[f32; 4]>> = (0..tws * ths)
        .map(|i| {
            let uv = [((i % tws) as f64 + 0.5) / f64::from(tw), ((i / tws) as f64 + 0.5) / f64::from(th)];
            from_face.apply(uv).and_then(photo_at)
        })
        .collect();
    let mut lit: Vec<f32> = photo
        .iter()
        .zip(&values)
        .filter(|(_, m)| **m > 0.9)
        .filter_map(|(p, _)| p.map(|p| p[0].max(p[1]).max(p[2])))
        .collect();
    lit.sort_by(f32::total_cmp);
    let bright = lit.get(lit.len() * 19 / 20).copied().unwrap_or(1.0);
    let gain = (0.85 / bright.max(1e-3)).clamp(1.0, 3.0);
    for y in 0..ths {
        for x in 0..tws {
            let i = y * tws + x;
            let base = photo
                .get(i)
                .copied()
                .flatten()
                .map_or([0.5, 0.5, 0.5], |p| [p[0], p[1], p[2]].map(|v| v * gain));
            let m = values.get(i).copied().unwrap_or(0.0);
            let dim = 0.25 + 0.75 * m;
            let mut rgb = base.map(|v| v * dim);
            // An object in front of the face: hatched.
            if (m - 0.5).abs() < 0.2 && (x + y) / 16 % 3 == 0 {
                rgb = [rgb[0] * 0.7 + 0.3 * 0.9, rgb[1] * 0.7, rgb[2] * 0.7 + 0.3 * 0.9];
            }
            // The face's edge: where the mask changes within a pixel.
            let edge = [(1, 0), (0, 1)].iter().any(|(dx, dy)| {
                values
                    .get((y + dy).min(ths - 1) * tws + (x + dx).min(tws - 1))
                    .is_some_and(|n| (n - m).abs() > 0.3)
            });
            let gx = (x as f64 + 0.5) % cell < 1.0;
            let gy = (y as f64 + 0.5) % cell < 1.0;
            if edge {
                rgb = [1.0, 0.85, 0.1];
            } else if gx || gy || x == 0 || y == 0 || x + 1 == tws || y + 1 == ths {
                rgb = rgb.map(|v| v * 0.5 + 0.5);
            }
            guide.extend(rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
            guide.push(255);
        }
    }
    let mask_file = write_file(&folder.join(MASK), penumbra_gpu::image::encode_png_gray8(tw, th, mask)?)?;
    let guide_file = write_file(&folder.join(GUIDE), penumbra_gpu::encode_png_rgba8(tw, th, guide)?)?;

    let rows = |h: &Homography| h.rows().map(|r| r.map(|v| (v * 1e9).round() / 1e9));
    let j = json!({
        "name": s.name,
        "kind": match s.kind { Kind::Face => "face", Kind::Region => "region" },
        "number": k,
        "folder": s.folder,
        "size": [tw, th],
        "aspect": [round(aspect[0]), round(aspect[1])],
        "aspect_from": aspect_from,
        "focal_px": focal.map(|f| f.round()),
        "frame": if rectified { "rectified" } else { "camera" },
        "corners": frame_corners.map(|c| [round(c[0]), round(c[1])]),
        "corners_from": corners_from,
        "frame_note": frame.note,
        "camera_pixels": pixels.len(),
        "camera_to_uv": rows(&to_face),
        "files": {"template": guide_file, "mask": mask_file, "uv": uv_png, "uv_camera": uv_camera},
        "projectors": projectors,
        "deliver": format!("{tw} x {th} pixels, or any size of aspect {}:{}", round(aspect[0]), round(aspect[1])),
    });
    write_file(
        &folder.join(FACE_JSON),
        serde_json::to_vec_pretty(&j).map_err(|e| CommandError(e.to_string()))?,
    )?;
    Ok((j, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rectangle `w` x `h` on a plane seen by a pinhole: its aspect comes back from its corners.
    #[test]
    fn a_rectangle_seen_at_an_angle_gives_back_its_aspect() {
        let f = 900.0;
        let (cx, cy) = (640.0, 360.0);
        // The plane tilted 50 degrees back and turned 20 degrees, 4 m away.
        let (a, b) = (50f64.to_radians(), 20f64.to_radians());
        let project = |x: f64, y: f64| {
            // Rotate about x by a, then about y by b, then push away.
            let (y1, z1) = (y * a.cos(), y * a.sin());
            let (x2, z2) = (x * b.cos() + z1 * b.sin(), -x * b.sin() + z1 * b.cos());
            let z = z2 + 4.0;
            [cx + f * x2 / z, cy + f * y1 / z]
        };
        let (w, h) = (1.2, 0.8);
        let c = [
            project(-w / 2.0, -h / 2.0),
            project(w / 2.0, -h / 2.0),
            project(w / 2.0, h / 2.0),
            project(-w / 2.0, h / 2.0),
        ];
        let (r, focal) = rectangle_aspect(c, [cx, cy], 1280.0, None);
        assert!((r - 1.5).abs() < 1e-6, "{r}");
        assert!((focal.unwrap() - f).abs() < 1e-3, "{focal:?}");
        // Seen head on: no focal length, the aspect from the sides.
        let flat = [[100.0, 100.0], [400.0, 100.0], [400.0, 300.0], [100.0, 300.0]];
        let (r, focal) = rectangle_aspect(flat, [cx, cy], 1280.0, None);
        assert!((r - 1.5).abs() < 1e-9 && focal.is_none(), "{r} {focal:?}");
    }

    #[test]
    fn cover_counts_unmeasured_pixels_only_inside_a_rectified_outline() {
        assert_eq!(cover(3, 3, 0, false), 1.0);
        assert_eq!(cover(3, 0, 1, true), 0.5);
        assert_eq!(cover(3, 0, 0, true), 1.0);
        assert_eq!(cover(3, 0, 0, false), 0.25);
        assert_eq!(cover(3, 0, 1, false), 0.25);
        assert_eq!(cover(3, 4, 0, true), 0.25);
    }
}
