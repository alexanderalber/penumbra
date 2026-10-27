//! `scan.scene`: the flat faces of a scene (floor, wall, the faces of boxes) and the objects
//! among them from one scan per projector, without a scan of the empty scene; any face of known
//! aspect rectified; the objects' height over the face they stand on. The math is
//! [`penumbra_calib::scene`]; this reads the scans, numbers faces and objects alike in every
//! projector, brings the heights into the first projector's units and writes the files.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::Homography;
use penumbra_calib::borders::{self, BorderOptions, How, Straightened};
use penumbra_calib::objects::{self, Map, Parts};
use penumbra_calib::relief::{self, Relief, View};
use penumbra_calib::scene::{self, FaceMap, Outline, SceneOptions, SceneParts};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan::{CAMERA_MAP, OBJECTS_CAMERA, PROJECTOR_MAP, colour, labels_file, ms, read_map, write_file};
use crate::scan_relief::{
    HEIGHT_CAMERA, ScaleKind, fixed_sign, height_file, heuristic_json, mask_file, round, sign, unify,
};

type CmdResult<T> = std::result::Result<T, CommandError>;
/// A face to rectify: its number, aspect and corners if given.
type FaceToRectify = (u16, [f64; 2], Option<[[f64; 2]; 4]>);

/// Where `scan.scene` writes unless told otherwise (relative to the project).
pub const DEFAULT_SCENE_OUT: &str = "scene";
pub const PLANES_CAMERA: &str = "planes_camera.png";
/// The faces in the camera picture as measured, before their borders were straightened.
pub const PLANES_CAMERA_MEASURED: &str = "planes_camera_measured.png";
pub const SCENE_VIEW: &str = "scene_view.png";
pub const SCENE_JSON: &str = "scene.json";
/// A Euro crate's top, the default for the support: width (along the outline's first edge)
/// first.
pub const DEFAULT_ASPECT: [f64; 2] = [60.0, 40.0];
/// The heights are normalised over these quantiles of all lit pixels.
const RANGE: [f64; 2] = [0.01, 0.995];
/// Erosion of the labels before they are compared across projectors, projector pixels.
const NUMBER_ERODE: u32 = 2;
/// Gaps in the camera picture of the faces up to about twice this are filled, camera pixels.
const CAMERA_GAP: u32 = 3;
/// A camera pixel shows a projector pixel's face when that pixel lands this close to it, camera
/// pixels.
const BACK_PX: f32 = 4.0;
/// Light on a rectified face this far outside its outline (in units of the top's size) still counts.
const UV_MARGIN: f64 = 0.01;
/// Objects' labels in the picture the borders are straightened in: from here on (faces are 1 to
/// 255), so their pixels and borders stay as measured.
pub const OBJECT_LABELS: u16 = 256;

pub fn planes_file(n: usize) -> String {
    format!("planes_{n}.png")
}
pub fn scene_file(n: usize) -> String {
    format!("scene_{n}.png")
}
/// Face `plane`'s u and v in projector `n`'s pixels, and in the camera's.
pub fn uv_file(plane: u16, n: usize) -> String {
    format!("uv_{plane}_{n}.exr")
}
pub fn uv_camera_file(plane: u16) -> String {
    format!("uv_{plane}_camera.exr")
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.scene",
        "Flat faces and the objects among them from one scan per projector (folders scan.capture or \
         scan.decode wrote), no reference scan needed: every flat face (floor, wall, each face of each \
         box) fits a homography of its own; they are found one fit after the other by RANSAC until no \
         fit holds a face of min_plane, coplanar faces that do not touch (the tops of two boxes side by \
         side) split into faces of their own, patches of curved surfaces dropped. What fits no face is an \
         object; the face right below the objects is the support. Any face that is a rectangle of known \
         aspect can be rectified: its four edges in the camera picture, completed where hidden, give the \
         homography to its u and v. Borders between faces are straight where the measured one is a line (two \
         faces meeting at a crease on the line their planes cross in, where every projector agrees and the \
         measured border lies on it; else a fitted line with corners); where no line fits (curved or ragged \
         borders) and along objects they stay as measured. Writes per projector planes_N.png and labels_N.png (8-bit, 0 none, k \
         the face or object, numbered alike in every projector; labels_N.png as scan.objects writes it), \
         scene_N.png (faces in colour, objects white), uv_K_N.exr per rectified face K (red and green its \
         u along the outline's first edge and v along its last, 0 to 1; blue 1 on the face, also where \
         nothing was measured, 0.5 on an object, 0.25 on another face; -1 outside the outline), \
         height_N.exr and mask_N.png (the objects' height over the support, as scan.relief writes them); in camera \
         space planes_camera.png, objects_camera.png, uv_K_camera.exr, height_camera.exr and \
         scene_view.png (with the rectified faces' outlines and a grid in true squares); scene.json (the \
         reply). {scans: [folder, ...], out? (folder, scene), rectify? ([{plane?, aspect: [width, depth], \
         corners? ([[x, y] x 4] camera px, instead of finding them; without plane they give the face: the \
         plane most pixels inside lie on, taken from the faces and objects it was split into, also where \
         it sags a little)}, ...], else the support at 60:40), \
         support? (plane number, or \"rectified\" for the first face in rectify; else the one below the \
         objects), sign?, scale? (as scan.relief), \
         tolerance? (camera px a pixel may lie off its face, 2), min_plane? (share of the measured \
         pixels, 0.002), max_planes? (most faces kept, 255), min_object? (projector px, 1/256 of the \
         projector), erode? (projector px the light mask shrinks, 2), straight? (straight borders in \
         planes_camera.png and planes_N.png, true; planes_camera_measured.png keeps the measured ones), \
         straight_tolerance? (camera px nine tenths of a measured border may lie off its line, 2), \
         straight_length? (shortest straight border, px, 16)}",
        move |p| {
            let a: SceneArgs = args(p)?;
            run(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneArgs {
    pub scans: Vec<PathBuf>,
    pub out: Option<PathBuf>,
    pub rectify: Option<Vec<RectifyArgs>>,
    pub support: Option<SupportArg>,
    pub sign: Option<Json>,
    pub scale: Option<ScaleKind>,
    pub tolerance: Option<f32>,
    pub min_plane: Option<f32>,
    pub max_planes: Option<usize>,
    pub min_object: Option<u32>,
    pub erode: Option<u32>,
    pub straight: Option<bool>,
    pub straight_tolerance: Option<f64>,
    pub straight_length: Option<f64>,
}

/// The support: a plane number, or `"rectified"` for the first face in `rectify`.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SupportArg {
    Plane(u16),
    Named(String),
}

/// A face to rectify: its number, its aspect (width along the outline's first edge, then depth
/// along its last) and optionally its corners in the camera picture (far left first, then
/// clockwise), instead of finding them. Corners without a number (or number 0) give the face
/// itself: whatever plane most of the pixels inside them lie on, claimed from the faces and
/// objects it was split into ([`scene::claim_face`]).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RectifyArgs {
    #[serde(default)]
    pub plane: Option<u16>,
    pub aspect: [f64; 2],
    pub corners: Option<[[f64; 2]; 4]>,
}

/// A rectified face: its number, aspect, outline (when found rather than given), corners, the
/// homography camera -> (u, v) and per projector its map projector -> camera.
struct Rectified {
    plane: u16,
    aspect: [f64; 2],
    outline: Option<Outline>,
    corners: [[f64; 2]; 4],
    to_face: Homography,
    projectors: Vec<Option<FaceMap>>,
}

impl Rectified {
    /// The face's (u, v) at projector `p`'s point `at`.
    fn uv(&self, p: usize, at: [f64; 2]) -> Option<[f64; 2]> {
        let m = self.projectors.get(p)?.as_ref()?;
        self.to_face.apply(m.apply(at)?)
    }
}

/// Per camera pixel the face it shows: the label of the projector pixel that lights it, the
/// first projector whose pixel there also lands back near it (a label grown past its face's
/// edge would paint the wall behind); else the label of a measured projector pixel landing in
/// it; else the first label there at all (the two maps of a real decode disagree by more than
/// that in bands along the stripes).
fn camera_view_of(
    to_camera: &[Map],
    from_camera: &[Map],
    parts: &[Parts],
    camera: (usize, usize),
) -> Vec<u16> {
    let (cw, ch) = (camera.0 as f32, camera.1 as f32);
    let mut splat = vec![0u16; camera.0 * camera.1];
    for (t, p) in to_camera.iter().zip(parts).rev() {
        for (j, l) in p.labels.iter().enumerate() {
            if *l > 0
                && t.measured(j)
                && let Some(c) = t.pixel_in(j, camera.0, camera.1)
                && let Some(s) = splat.get_mut(c)
            {
                *s = *l;
            }
        }
    }
    (0..camera.0 * camera.1)
        .map(|c| {
            let (x, y) = ((c % camera.0.max(1)) as f32 + 0.5, (c / camera.0.max(1)) as f32 + 0.5);
            to_camera
                .iter()
                .zip(from_camera)
                .zip(parts)
                .filter_map(|((t, m), p)| {
                    let j = m.pixel_in(c, p.width, p.height)?;
                    let [u, v] = t.uv(j)?;
                    ((u * cw - x).hypot(v * ch - y) <= BACK_PX).then(|| p.labels.get(j).copied())?
                })
                .find(|l| *l > 0)
                .or_else(|| splat.get(c).copied().filter(|l| *l > 0))
                .or_else(|| {
                    from_camera
                        .iter()
                        .zip(parts)
                        .filter_map(|(m, p)| p.labels.get(m.pixel_in(c, p.width, p.height)?).copied())
                        .find(|l| *l > 0)
                })
                .unwrap_or(0)
        })
        .collect()
}

/// Labels of one kind (planes or objects) for [`objects::number`].
fn parts_of(s: &SceneParts, labels: &[u16], count: usize) -> Parts {
    let n = labels.len();
    Parts {
        width: s.width,
        height: s.height,
        labels: labels.to_vec(),
        cuts: vec![false; n],
        dropped: vec![false; n],
        count,
        silhouette: 0,
        compared: 0,
        over: 0,
    }
}

/// Per projector, its labels before numbering to the numbers after (index: the label before).
fn renumbering(before: &[Vec<u16>], after: &[Parts], counts: &[usize]) -> Vec<Vec<u16>> {
    before
        .iter()
        .zip(after)
        .zip(counts)
        .map(|((b, a), c)| {
            let mut map = vec![0u16; c + 1];
            for (x, y) in b.iter().zip(&a.labels) {
                if let Some(m) = map.get_mut(usize::from(*x))
                    && *m == 0
                {
                    *m = *y;
                }
            }
            map
        })
        .collect()
}

fn rows_json(h: &Homography) -> Json {
    json!(h.rows().map(|r| r.map(|v| (v * 1e9).round() / 1e9)))
}

/// Runs `scan.scene` over the scans of `a` (paths relative to `dir`).
pub fn run(dir: &Path, a: &SceneArgs) -> CmdResult<Json> {
    let started = Instant::now();
    if a.scans.is_empty() || a.scans.len() > objects::MAX_PROJECTORS {
        return Err(CommandError(format!("`scans` lists 1 to {} folders", objects::MAX_PROJECTORS)));
    }
    let defaults = SceneOptions::default();
    let base = SceneOptions {
        tolerance: a.tolerance.unwrap_or(defaults.tolerance),
        min_plane: a.min_plane.unwrap_or(defaults.min_plane),
        max_planes: a.max_planes.unwrap_or(defaults.max_planes),
        min_object: 0,
    };
    base.check().map_err(CommandError)?;
    let erode = a.erode.unwrap_or(2);
    let straight = a.straight.unwrap_or(true);
    let border_defaults = BorderOptions::default();
    let border_options = BorderOptions {
        tolerance: a.straight_tolerance.unwrap_or(border_defaults.tolerance),
        min_length: a.straight_length.unwrap_or(border_defaults.min_length),
    };
    border_options.check().map_err(CommandError)?;
    if erode > 64 {
        return Err("`erode` is 0 to 64 projector pixels".into());
    }
    for r in a.rectify.iter().flatten() {
        if !r.aspect.iter().all(|v| v.is_finite() && *v > 0.0 && *v < 1e6) {
            return Err("a face's `aspect` is two positive numbers, its width and depth".into());
        }
        if r.corners.is_some_and(|c| c.iter().flatten().any(|v| !v.is_finite())) {
            return Err("a face's `corners` are four [x, y] pairs of numbers".into());
        }
    }
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
    let (cw, ch) = camera;
    let read = started.elapsed();

    let t = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut found: Vec<SceneParts> = Vec::new();
    let mut min_objects: Vec<usize> = Vec::new();
    for (map, folder) in to_camera.iter().zip(&a.scans) {
        let min_object = match a.min_object {
            Some(m) => m as usize,
            None => (map.len() / 256).max(1),
        };
        min_objects.push(min_object);
        let o = SceneOptions { min_object, ..base };
        let s = scene::analyse(map, camera, &o)
            .map_err(|e| CommandError(format!("{}: {e}", dir.join(folder).display())))?;
        found.push(s);
    }
    let scans: Vec<&Map> = to_camera.iter().collect();
    // A face in one projector beats an object in another (a top one projector decodes badly).
    // First pixel by pixel: a face one projector sees at a grazing angle joins the object next to
    // it, where the other projector, facing it, finds the face; then whole objects.
    let carved = scene::carve_disputed_objects(&mut found, &scans, camera, &min_objects);
    let objects_on_faces = scene::drop_objects_on_faces(&mut found, &scans, camera);
    // Faces given by their corners alone, claimed in every projector before the numbering.
    let mut claimed: Vec<(usize, Vec<Option<usize>>)> = Vec::new();
    for (j, r) in a.rectify.iter().flatten().enumerate() {
        if r.plane.is_some_and(|p| p != 0) {
            continue;
        }
        let Some(corners) = r.corners else {
            return Err("a face to rectify names its `plane`, its `corners` or both".into());
        };
        let Some(to_face) = Homography::square_to_quad(corners).ok().and_then(|h| h.inverse()) else {
            return Err("a face's `corners` do not form a convex quadrilateral".into());
        };
        let tol = f64::from(base.tolerance);
        // Each projector alone first; then the others again, searching where the one that
        // claimed most found the face.
        let alone: Vec<Option<(SceneParts, scene::Claim)>> = found
            .iter()
            .zip(&to_camera)
            .map(|(s, m)| {
                let mut s = s.clone();
                scene::claim_face(&mut s, m, camera, &to_face, tol, UV_MARGIN, None).map(|c| (s, c))
            })
            .collect();
        let count = |c: &scene::Claim| c.camera.iter().filter(|x| **x).count();
        let lead = alone
            .iter()
            .enumerate()
            .filter_map(|(p, c)| Some((p, count(&c.as_ref()?.1))))
            .max_by_key(|(_, n)| *n)
            .map(|(p, _)| p);
        let prior = lead.and_then(|p| alone.get(p)?.as_ref()).map(|(_, c)| c.camera.clone());
        let mut per: Vec<Option<usize>> = Vec::new();
        for (p, ((s, m), mine)) in found.iter_mut().zip(&to_camera).zip(alone).enumerate() {
            if Some(p) == lead
                && let Some((claimed_parts, c)) = mine
            {
                *s = claimed_parts;
                per.push(Some(c.plane));
                continue;
            }
            per.push(
                scene::claim_face(s, m, camera, &to_face, tol, UV_MARGIN, prior.as_deref()).map(|c| c.plane),
            );
        }
        claimed.push((j, per));
    }
    // A claimed face is no object any more where one projector claimed it: the other one's object
    // pixels there (it claims only the pixels it decoded well enough) lose to it.
    let carved: Vec<usize> = if claimed.is_empty() {
        carved
    } else {
        let again = scene::carve_disputed_objects(&mut found, &scans, camera, &min_objects);
        carved.iter().zip(again).map(|(a, b)| a + b).collect()
    };

    // Planes and objects numbered alike in every projector, top of the camera picture first.
    let plane_counts: Vec<usize> = found.iter().map(|s| s.planes.len()).collect();
    let object_counts: Vec<usize> = found.iter().map(|s| s.objects).collect();
    let mut plane_parts: Vec<Parts> =
        found.iter().zip(&plane_counts).map(|(s, c)| parts_of(s, &s.plane_labels, *c)).collect();
    let mut object_parts: Vec<Parts> =
        found.iter().zip(&object_counts).map(|(s, c)| parts_of(s, &s.object_labels, *c)).collect();
    let planes_found = objects::number_faces(&mut plane_parts, &scans, camera, NUMBER_ERODE)?;
    let objects_found = objects::number(&mut object_parts, &scans, camera, NUMBER_ERODE)?;
    let plane_before: Vec<Vec<u16>> = found.iter().map(|s| s.plane_labels.clone()).collect();
    let mut plane_numbers = renumbering(&plane_before, &plane_parts, &plane_counts);
    // A claimed face is one face in every projector, whatever the numbering matched it with: the
    // projectors may light different parts of it.
    for (_, per) in &claimed {
        let first = per
            .iter()
            .enumerate()
            .find_map(|(p, k)| plane_numbers.get(p)?.get((*k)? + 1).copied().filter(|n| *n > 0));
        let Some(target) = first else { continue };
        for (p, k) in per.iter().enumerate() {
            let (Some(k), Some(numbers), Some(parts)) = (k, plane_numbers.get_mut(p), plane_parts.get_mut(p))
            else {
                continue;
            };
            let Some(slot) = numbers.get_mut(k + 1) else { continue };
            let old = *slot;
            if old == target || old == 0 {
                continue;
            }
            *slot = target;
            for l in parts.labels.iter_mut().filter(|l| **l == old) {
                *l = target;
            }
        }
    }
    // Per projector and plane number the projector's own plane (its largest one if two merged).
    let own_plane = |p: usize, number: u16| -> Option<usize> {
        let s = found.get(p)?;
        let map = plane_numbers.get(p)?;
        (0..s.planes.len())
            .filter(|k| map.get(k + 1) == Some(&number))
            .max_by_key(|k| s.planes.get(*k).map_or(0, |pl| pl.pixels))
    };
    let homography = |p: usize, number: u16| -> Option<Homography> {
        let k = own_plane(p, number)?;
        Some(found.get(p)?.planes.get(k)?.homography)
    };

    // Where two projectors see different faces on one surface, the one sampling it finer wins.
    let handed = scene::hand_over_faces(&mut plane_parts, &to_camera, &from_camera, camera, BACK_PX);
    let planes_measured =
        scene::fill_gaps(cw, ch, &camera_view_of(&to_camera, &from_camera, &plane_parts, camera), CAMERA_GAP);
    let objects_camera = objects::camera_labels(&object_parts, &scans, camera, NUMBER_ERODE)?;
    if objects_found.count == 0 {
        warnings.push("no objects found: every measured pixel lies on a plane".into());
    }
    // Straight borders: in the camera picture, where two faces meet on the line their planes
    // cross in (as every projector seeing both sends them), else on a line fitted to the
    // measured border; per projector the same in its own pixels. Objects keep theirs.
    let faces = 1..=u16::try_from(planes_found.count).unwrap_or(u16::MAX);
    let (planes_camera, straight_camera) = if straight {
        let views: Vec<Vec<Option<Homography>>> = faces
            .clone()
            .map(|k| (0..found.len()).map(|p| homography(p, k).and_then(|h| h.inverse())).collect())
            .collect();
        let s = straighten_faces(cw, ch, &planes_measured, &objects_camera, &views, &border_options);
        (s.labels.clone(), Some(s))
    } else {
        (planes_measured.clone(), None)
    };
    let mut straight_projectors: Vec<Option<Straightened>> = Vec::new();
    for (p, (pp, op)) in plane_parts.iter_mut().zip(&object_parts).enumerate() {
        if !straight {
            straight_projectors.push(None);
            continue;
        }
        let views: Vec<Vec<Option<Homography>>> = faces.clone().map(|k| vec![homography(p, k)]).collect();
        let s = straighten_faces(pp.width, pp.height, &pp.labels, &op.labels, &views, &border_options);
        pp.labels.clone_from(&s.labels);
        straight_projectors.push(Some(s));
    }

    // The support: asked for, or the plane below the objects.
    let scores = scene::support_scores(cw, ch, &planes_camera, &objects_camera, planes_found.count);
    let claimed_number = |j: usize| -> Option<u16> {
        let (_, per) = claimed.iter().find(|(i, _)| *i == j)?;
        per.iter()
            .enumerate()
            .find_map(|(p, k)| plane_numbers.get(p)?.get((*k)? + 1).copied().filter(|n| *n > 0))
    };
    let first_rectified = a.rectify.as_ref().and_then(|l| l.first()).and_then(|r| match r.plane {
        Some(p) if p != 0 => Some(p),
        _ => claimed_number(0),
    });
    let support: Option<(u16, &str)> = match &a.support {
        Some(SupportArg::Plane(s)) if *s == 0 || usize::from(*s) > planes_found.count => {
            return Err(CommandError(format!("`support` is a plane number, 1 to {}", planes_found.count)));
        }
        Some(SupportArg::Plane(s)) => Some((*s, "given")),
        Some(SupportArg::Named(n)) if n == "rectified" => match first_rectified {
            Some(s) => Some((s, "the rectified face")),
            None => {
                return Err(CommandError(
                    "`support` is \"rectified\" but the first face in `rectify` was not found".into(),
                ));
            }
        },
        Some(SupportArg::Named(n)) => {
            return Err(CommandError(format!("`support` is a plane number or \"rectified\", not {n:?}")));
        }
        None => scores
            .iter()
            .enumerate()
            .filter(|(_, s)| **s > 0)
            .max_by_key(|(k, s)| (**s, std::cmp::Reverse(*k)))
            .and_then(|(k, _)| u16::try_from(k + 1).ok())
            .map(|s| (s, "below the objects")),
    };

    // The faces to rectify: asked for, else the support as a Euro crate's top.
    let asked_faces: Vec<FaceToRectify> = match &a.rectify {
        Some(list) => list
            .iter()
            .enumerate()
            .filter_map(|(j, r)| match r.plane.filter(|p| *p != 0).or_else(|| claimed_number(j)) {
                Some(plane) => Some((plane, r.aspect, r.corners)),
                None => {
                    warnings.push(format!(
                        "face {j} to rectify: no plane holds most of the pixels inside its corners; not rectified"
                    ));
                    None
                }
            })
            .collect(),
        None => support.map(|(s, _)| (s, DEFAULT_ASPECT, None)).into_iter().collect(),
    };
    if support.is_none() && objects_found.count > 0 {
        warnings.push("no face lies right below the objects: no support, no heights".into());
    }
    let mut rectified: Vec<Rectified> = Vec::new();
    for (plane, aspect, corners) in asked_faces {
        if plane == 0 || usize::from(plane) > planes_found.count {
            return Err(CommandError(format!(
                "`rectify` names plane {plane}; the planes are 1 to {}",
                planes_found.count
            )));
        }
        let (outline, corners) = match corners {
            Some(c) => (None, c),
            None => match scene::face_outline(cw, ch, &planes_camera, plane, &objects_camera) {
                Ok(o) => {
                    let c = o.corners;
                    (Some(o), c)
                }
                Err(e) => {
                    warnings.push(format!("plane {plane}: no outline ({e}); not rectified"));
                    continue;
                }
            },
        };
        let Some(to_face) = Homography::square_to_quad(corners).ok().and_then(|h| h.inverse()) else {
            warnings.push(format!(
                "plane {plane}: its corners do not form a convex quadrilateral; not rectified"
            ));
            continue;
        };
        let projectors = (0..found.len())
            .map(|p| {
                let k = own_plane(p, plane)?;
                let h = &found.get(p)?.planes.get(k)?.homography;
                Some(scene::face_map(
                    to_camera.get(p)?,
                    camera,
                    h,
                    &to_face,
                    f64::from(base.tolerance),
                    UV_MARGIN,
                ))
            })
            .collect();
        rectified.push(Rectified { plane, aspect, outline, corners, to_face, projectors });
    }

    // Heights over the support, per projector that sees an object on it.
    let mut reliefs: Vec<Relief> = Vec::new();
    let mut relief_of: Vec<usize> = Vec::new();
    if let Some((s, _)) = support {
        for (p, ((map, parts), op)) in to_camera.iter().zip(&plane_parts).zip(&object_parts).enumerate() {
            if op.labels.iter().all(|l| *l == 0) {
                continue;
            }
            let Some(k) = own_plane(p, s) else {
                warnings.push(format!("projector {p} does not light the support: no heights"));
                continue;
            };
            let Some(plane) = found.get(p).and_then(|f| f.planes.get(k)) else { continue };
            match scene::relief(map, camera, &plane.homography, &op.labels, &parts.labels, s, erode) {
                Ok(r) => {
                    reliefs.push(r);
                    relief_of.push(p);
                }
                Err(e) => warnings.push(format!("projector {p}: no heights ({e})")),
            }
        }
    }
    let cams: Vec<&Map> = relief_of.iter().filter_map(|p| from_camera.get(*p)).collect();
    let mut relief_warnings = Vec::new();
    let (converted, scales) = unify(&mut reliefs, &cams, asked, kind, &mut relief_warnings);
    // `unify` counts the projectors with heights; name them by their place in `scans`.
    for w in relief_warnings {
        let mut w = w;
        for (k, p) in relief_of.iter().enumerate().rev() {
            w = w.replace(&format!("projector {k}"), &format!("projector {p}"));
        }
        warnings.push(w);
    }
    let lit: Vec<f32> = converted
        .iter()
        .zip(&reliefs)
        .flat_map(|(h, r)| h.iter().zip(&r.mask).filter(|(_, m)| **m).map(|(h, _)| *h))
        .collect();
    let range = relief::range(&lit, RANGE[0], RANGE[1]);
    let work = t.elapsed();

    // Files.
    let t = Instant::now();
    let out = dir.join(a.out.as_deref().unwrap_or(Path::new(DEFAULT_SCENE_OUT)));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| write_file(&out.join(name), bytes);
    let bytes =
        |labels: &[u16]| -> Vec<u8> { labels.iter().map(|l| u8::try_from(*l).unwrap_or(0)).collect() };
    let norm = |h: f32| match range {
        Some([lo, hi]) => ((h - lo) / if hi > lo { hi - lo } else { 1.0 }).clamp(0.0, 1.0),
        None => 0.0,
    };
    let mut per_projector = Vec::new();
    for (p, ((s, pp), op)) in found.iter().zip(&plane_parts).zip(&object_parts).enumerate() {
        let (w, h) = (s.width as u32, s.height as u32);
        let planes = write(&planes_file(p), penumbra_gpu::image::encode_png_gray8(w, h, bytes(&pp.labels))?)?;
        let labels = write(&labels_file(p), penumbra_gpu::image::encode_png_gray8(w, h, bytes(&op.labels))?)?;
        let picture =
            write(&scene_file(p), penumbra_gpu::encode_png_rgba8(w, h, view(&pp.labels, &op.labels))?)?;
        let mut uv = Vec::new();
        for r in &rectified {
            if r.projectors.get(p).is_none_or(|m| m.is_none()) {
                continue;
            }
            let rgba: Vec<[f32; 4]> = pp
                .labels
                .iter()
                .zip(&op.labels)
                .enumerate()
                .map(|(i, (l, o))| {
                    let c = [(i % s.width.max(1)) as f64 + 0.5, (i / s.width.max(1)) as f64 + 0.5];
                    match r.uv(p, c) {
                        Some([u, v]) if inside(u, v) => [u as f32, v as f32, cover(r.plane, *l, *o), 1.0],
                        _ => [-1.0, -1.0, 0.0, 1.0],
                    }
                })
                .collect();
            let file = write(&uv_file(r.plane, p), penumbra_gpu::image::encode_exr_rgba32f(w, h, &rgba)?)?;
            uv.push(json!({"plane": r.plane, "uv": file}));
        }
        let heights = match relief_of.iter().position(|q| *q == p) {
            Some(k) => {
                let (Some(r), Some(hs)) = (reliefs.get(k), converted.get(k)) else { continue };
                let rgba: Vec<[f32; 4]> = hs.iter().map(|v| norm(*v)).map(|v| [v, v, v, 1.0]).collect();
                let height = write(&height_file(p), penumbra_gpu::image::encode_exr_rgba32f(w, h, &rgba)?)?;
                let m: Vec<u8> = r.mask.iter().map(|m| if *m { 255 } else { 0 }).collect();
                let mask = write(&mask_file(p), penumbra_gpu::image::encode_png_gray8(w, h, m)?)?;
                let sign_from = match (k, asked) {
                    (0, Some(_)) => json!("given"),
                    (0, None) => json!("heuristic"),
                    _ => {
                        scales.get(k).and_then(|s| s.get("sign_from")).cloned().unwrap_or(json!("heuristic"))
                    }
                };
                json!({
                    "fit_error_px": {"median": round(r.error_px[0]), "p90": round(r.error_px[1])},
                    "direction": [round(r.direction[0]), round(r.direction[1])],
                    "sign": {"from": sign_from, "as_param": fixed_sign(r.direction), "heuristic": heuristic_json(&r.heuristic), "flipped_from_heuristic": r.flipped},
                    "to_first": scales.get(k).cloned().unwrap_or(Json::Null),
                    "height": height,
                    "mask": mask,
                })
            }
            None => Json::Null,
        };
        let own: Vec<Json> = s
            .planes
            .iter()
            .enumerate()
            .map(|(k, pl)| {
                json!({
                    "plane": plane_numbers.get(p).and_then(|m| m.get(k + 1)).copied().unwrap_or(0),
                    "pixels": pl.pixels,
                    "error_px": {"median": round(pl.error_px[0]), "p90": round(pl.error_px[1])},
                    "group": pl.group,
                    "bend": {"share": round(pl.bend[0]), "rms_px": round(pl.bend[1]), "bow_px": round(pl.bend[2])},
                })
            })
            .collect();
        per_projector.push(json!({
            "scan": a.scans.get(p).map(|x| x.display().to_string()),
            "size": [w, h],
            "measured": s.measured,
            "planes": own,
            "dropped_planes": s.dropped_planes,
            "joined_planes": s.joined_planes,
            "objects_on_faces": objects_on_faces.get(p).copied().unwrap_or(0),
            "carved_px": carved.get(p).copied().unwrap_or(0),
            "handed_over_px": handed.get(p).copied().unwrap_or(0),
            "objects": op.labels.iter().copied().max().unwrap_or(0),
            "unexplained": s.unexplained,
            "files": {"planes": planes, "labels": labels, "picture": picture, "uv": uv},
            "heights": heights,
            "borders": straight_projectors.get(p).and_then(|s| s.as_ref()).map(borders_json),
        }));
    }
    let (cwu, chu) = (cw as u32, ch as u32);
    let planes_file_cam =
        write(PLANES_CAMERA, penumbra_gpu::image::encode_png_gray8(cwu, chu, bytes(&planes_camera))?)?;
    let objects_file_cam =
        write(OBJECTS_CAMERA, penumbra_gpu::image::encode_png_gray8(cwu, chu, bytes(&objects_camera))?)?;
    let measured_file = write(
        PLANES_CAMERA_MEASURED,
        penumbra_gpu::image::encode_png_gray8(cwu, chu, bytes(&planes_measured))?,
    )?;
    let mut picture = view(&planes_camera, &objects_camera);
    if let Some(s) = &straight_camera {
        draw_runs(&mut picture, cw, ch, &s.runs);
    }
    for r in &rectified {
        draw_outline(&mut picture, cw, ch, &r.corners, r.aspect);
    }
    let view_file = write(SCENE_VIEW, penumbra_gpu::encode_png_rgba8(cwu, chu, picture)?)?;
    let mut faces_json = Vec::new();
    for r in &rectified {
        let rgba: Vec<[f32; 4]> = (0..cw * ch)
            .map(|c| {
                let q = [(c % cw.max(1)) as f64 + 0.5, (c / cw.max(1)) as f64 + 0.5];
                match r.to_face.apply(q) {
                    Some([u, v]) if inside(u, v) => {
                        let on = cover(
                            r.plane,
                            planes_camera.get(c).copied().unwrap_or(0),
                            objects_camera.get(c).copied().unwrap_or(0),
                        );
                        [u as f32, v as f32, on, 1.0]
                    }
                    _ => [-1.0, -1.0, 0.0, 1.0],
                }
            })
            .collect();
        let file =
            write(&uv_camera_file(r.plane), penumbra_gpu::image::encode_exr_rgba32f(cwu, chu, &rgba)?)?;
        faces_json.push(json!({
            "plane": r.plane,
            "aspect": r.aspect,
            "corners": r.corners.map(|c| [round(c[0]), round(c[1])]),
            "corners_from": if r.outline.is_some() { "found" } else { "given" },
            "edge_points": r.outline.as_ref().map(|o| o.edges.map(|e| e.points)),
            "border_points": r.outline.as_ref().map(|o| o.border),
            "camera_to_uv": rows_json(&r.to_face),
            "projector_to_uv": r.projectors.iter().map(|m| m.as_ref().map_or(Json::Null, |m| rows_json(&r.to_face.after(&m.homography)))).collect::<Vec<_>>(),
            "projector_fit": r.projectors.iter().map(|m| m.as_ref().map_or(Json::Null, |m| json!({
                "pixels": m.pixels,
                "error_px": {"plane": round(m.error_px[0]), "corrected": round(m.error_px[1])},
                "bend_px": {"rms": round(m.bend_px[0]), "largest": round(m.bend_px[1])},
                "corrected": m.corrected(),
            }))).collect::<Vec<_>>(),
            "uv_camera": file,
        }));
    }
    let height_camera = if reliefs.is_empty() {
        None
    } else {
        let views: Vec<View<'_>> = converted
            .iter()
            .zip(&reliefs)
            .zip(&cams)
            .map(|((h, r), c)| View {
                from_camera: c,
                heights: h,
                mask: &r.mask,
                width: r.width,
                height: r.height,
            })
            .collect();
        let rgba: Vec<[f32; 4]> = relief::camera_heights(&views, camera)
            .iter()
            .map(|h| match h {
                Some(v) => {
                    let v = norm(*v);
                    [v, v, v, 1.0]
                }
                None => [0.0, 0.0, 0.0, 0.0],
            })
            .collect();
        Some(write(HEIGHT_CAMERA, penumbra_gpu::image::encode_exr_rgba32f(cwu, chu, &rgba)?)?)
    };

    let plane_list: Vec<Json> = (1..=planes_found.count)
        .map(|k| {
            let label = u16::try_from(k).unwrap_or(0);
            let pixels: Vec<usize> = plane_parts.iter().map(|p| p.labels.iter().filter(|l| **l == label).count()).collect();
            let errors: Vec<Json> = (0..found.len())
                .map(|p| {
                    own_plane(p, label)
                        .and_then(|j| found.get(p)?.planes.get(j))
                        .map_or(Json::Null, |pl| json!([round(pl.error_px[0]), round(pl.error_px[1])]))
                })
                .collect();
            json!({
                "plane": k,
                "camera_y": planes_found.camera_y.get(k - 1).map(|y| y.round()),
                "camera_pixels": planes_camera.iter().filter(|l| **l == label).count(),
                "pixels": pixels,
                "seen_by": pixels.iter().enumerate().filter(|(_, c)| **c > 0).map(|(n, _)| n).collect::<Vec<_>>(),
                "error_px": errors,
                "below_objects": scores.get(k - 1),
                "support": support.is_some_and(|(s, _)| usize::from(s) == k),
            })
        })
        .collect();
    let object_list: Vec<Json> = (1..=objects_found.count)
        .map(|k| {
            let label = u16::try_from(k).unwrap_or(0);
            let pixels: Vec<usize> = object_parts.iter().map(|p| p.labels.iter().filter(|l| **l == label).count()).collect();
            json!({
                "object": k,
                "camera_y": objects_found.camera_y.get(k - 1).map(|y| y.round()),
                "camera_pixels": objects_camera.iter().filter(|l| **l == label).count(),
                "pixels": pixels,
                "seen_by": pixels.iter().enumerate().filter(|(_, c)| **c > 0).map(|(n, _)| n).collect::<Vec<_>>(),
            })
        })
        .collect();
    let written = t.elapsed();
    let message = format!(
        "{} plane(s), {} object(s); {}{}",
        planes_found.count,
        objects_found.count,
        match support {
            Some((s, by)) => format!("plane {s} the support ({by})"),
            None => "no support".into(),
        },
        match rectified.len() {
            0 => String::new(),
            n => format!("; {n} face(s) rectified"),
        }
    );
    let reply = json!({
        "message": message,
        "camera": [cw, ch],
        "planes": plane_list,
        "objects": object_list,
        "support": support.map(|(s, by)| json!({"plane": s, "by": by})),
        "rectified": faces_json,
        "range": range.map(|[lo, hi]| json!({"low": round(f64::from(lo)), "high": round(f64::from(hi)), "units": "camera px of parallax over the support, the first projector with heights", "quantiles": RANGE})),
        "projectors": per_projector,
        "borders": straight_camera.as_ref().map(borders_json),
        "files": {"planes_camera": planes_file_cam, "planes_camera_measured": measured_file, "objects_camera": objects_file_cam, "view": view_file, "height_camera": height_camera},
        "options": {"straight": straight, "straight_tolerance": border_options.tolerance, "straight_length": border_options.min_length, "tolerance": base.tolerance, "min_plane": base.min_plane, "max_planes": base.max_planes, "erode": erode, "sign": asked.map_or(json!("auto"), |s| json!(s)), "scale": kind.name()},
        "ms": {"read": ms(read), "work": ms(work), "write": ms(written), "total": ms(started.elapsed())},
        "warnings": warnings,
        "next": "a rectified face: tex.image_file of uv_K_N.exr into a tex.shader (red, green its u and v, blue 1 on \
                 it; u times aspect[0] and v times aspect[1] for square tiles); the faces: planes_N.png or \
                 planes_camera.png into shapes.trace (labels) for masks per face; the objects: height_N.exr and \
                 mask_N.png as from scan.relief, labels_N.png and objects_camera.png as from scan.objects \
                 (scan.blend's masks, shapes.trace)",
    });
    let text = serde_json::to_vec_pretty(&reply).map_err(|e| CommandError(e.to_string()))?;
    write(SCENE_JSON, text)?;
    Ok(reply)
}

/// `(u, v)` lies on the top (with a hair of margin).
/// What a pixel inside a rectified face's outline shows, for the uv files' blue: 1 the face, 0.5
/// an object, 0.25 another face. Inside the outline, a pixel no face and no object claims (not
/// measured, such as a dark or glossy part of the face, or the rims and ribs kept out of the
/// objects) is the face too, so content fills the whole outline and not just the pixels that
/// happened to decode.
fn cover(face: u16, plane: u16, object: u16) -> f32 {
    if plane == face {
        1.0
    } else if object > 0 {
        0.5
    } else if plane == 0 {
        1.0
    } else {
        0.25
    }
}

fn inside(u: f64, v: f64) -> bool {
    let r = -UV_MARGIN..=1.0 + UV_MARGIN;
    r.contains(&u) && r.contains(&v)
}

/// Faces in flat colours, objects white, nothing black (RGBA bytes).
fn view(planes: &[u16], objects: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(planes.len() * 4);
    for (p, o) in planes.iter().zip(objects) {
        let c = if *o > 0 {
            [255, 255, 255]
        } else if *p > 0 {
            colour(*p).map(|v| (f32::from(v) * 0.8) as u8)
        } else {
            [0, 0, 0]
        };
        out.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    out
}

/// The outline in white into an RGBA picture, its first corner marked, and a grey grid on the
/// top in true squares (a quarter of its shorter side), to check the rectification by eye.
fn draw_outline(rgba: &mut [u8], w: usize, h: usize, corners: &[[f64; 2]; 4], aspect: [f64; 2]) {
    let mut dot = |p: Option<[f64; 2]>, c: [u8; 3]| {
        let Some([x, y]) = p else { return };
        if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
            let i = ((y as usize) * w + x as usize) * 4;
            if let Some(px) = rgba.get_mut(i..i + 3) {
                px.copy_from_slice(&c);
            }
        }
    };
    let Ok(to_camera) = Homography::square_to_quad(*corners) else { return };
    let side = |a: [f64; 2], b: [f64; 2]| a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt();
    let reach = side(corners[0], corners[2]).max(side(corners[1], corners[3]));
    let steps = (reach * 2.0).ceil().clamp(1.0, 1e5) as usize;
    let mut line = |a: [f64; 2], b: [f64; 2], c: [u8; 3]| {
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            dot(to_camera.apply([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]), c);
        }
    };
    let cell = aspect[0].min(aspect[1]) / 4.0;
    let (nu, nv) = ((aspect[0] / cell).round() as usize, (aspect[1] / cell).round() as usize);
    for k in 1..nu.min(64) {
        let u = k as f64 * cell / aspect[0];
        line([u, 0.0], [u, 1.0], [150, 150, 150]);
    }
    for k in 1..nv.min(64) {
        let v = k as f64 * cell / aspect[1];
        line([0.0, v], [1.0, v], [150, 150, 150]);
    }
    for (a, b) in [
        ([0.0, 0.0], [1.0, 0.0]),
        ([1.0, 0.0], [1.0, 1.0]),
        ([1.0, 1.0], [0.0, 1.0]),
        ([0.0, 1.0], [0.0, 0.0]),
    ] {
        line(a, b, [255, 255, 255]);
    }
    let c = corners[0];
    for dy in -3..=3 {
        for dx in -3..=3 {
            dot(Some([c[0] + f64::from(dx), c[1] + f64::from(dy)]), [255, 40, 40]);
        }
    }
}

/// Face labels (`planes`, 1 to 255) with straight borders, the objects (`objects`, any label
/// above 0 there) kept as they are. `views` per face: its homography from this picture into each
/// view.
pub fn straighten_faces(
    w: usize,
    h: usize,
    planes: &[u16],
    objects: &[u16],
    views: &[Vec<Option<Homography>>],
    o: &BorderOptions,
) -> Straightened {
    let combined: Vec<u16> = planes
        .iter()
        .zip(objects)
        .map(|(p, ob)| if *ob > 0 { OBJECT_LABELS.saturating_add(*ob) } else { *p })
        .collect();
    let mut s = borders::straighten(w, h, &combined, OBJECT_LABELS, views, o);
    for ((l, p), ob) in s.labels.iter_mut().zip(planes).zip(objects) {
        if *ob > 0 || *l >= OBJECT_LABELS {
            *l = *p;
        }
    }
    s
}

/// What the straightening did, for the reply: the borders it could change, how much of their
/// length became straight (fitted, or on two planes' meeting line) and the goodness of the
/// straight runs (the 90th percentile distance of the measured border from its line, the bow).
pub fn borders_json(s: &Straightened) -> Json {
    let st = &s.stats;
    let quantiles = |how: How| -> Json {
        let mut p90: Vec<f64> = s.runs.iter().filter(|r| r.how == how).map(|r| r.p90).collect();
        let mut bow: Vec<f64> = s.runs.iter().filter(|r| r.how == how).map(|r| r.bow).collect();
        p90.sort_by(f64::total_cmp);
        bow.sort_by(f64::total_cmp);
        let q = |v: &[f64], f: f64| {
            v.get(((v.len().saturating_sub(1)) as f64 * f).round() as usize).copied().map(round)
        };
        json!({
            "runs": p90.len(),
            "p90_px": {"median": q(&p90, 0.5), "max": q(&p90, 1.0)},
            "bow_px": {"median": q(&bow, 0.5), "max": q(&bow, 1.0)},
        })
    };
    let share = |x: f64| if st.length > 0.0 { round(x / st.length) } else { 0.0 };
    json!({
        "borders": st.chains,
        "straight_borders": st.straight_chains,
        "length_px": st.length.round(),
        "straight_share": share(st.straight_length),
        "planes_share": share(st.meeting_length),
        "planes_tried": st.meeting_tried,
        "planes_held": st.meeting_held,
        "planes_no_line": st.meeting_none,
        "planes_disagree": st.meeting_disagree,
        "seams": {"tried": st.seams_tried, "closed": st.seams_held, "px": st.seam_px},
        "fitted": quantiles(How::Fitted),
        "planes": quantiles(How::Meeting),
        "changed_px": st.changed,
    })
}

/// The straight runs into an RGBA picture: on two planes' meeting line yellow, fitted cyan.
fn draw_runs(rgba: &mut [u8], w: usize, h: usize, runs: &[borders::Run]) {
    for r in runs {
        let c = match r.how {
            How::Meeting => [255, 220, 0],
            How::Fitted => [0, 220, 255],
        };
        let len = (r.to[0] - r.from[0]).hypot(r.to[1] - r.from[1]);
        let steps = (len * 2.0).ceil().clamp(1.0, 1e5) as usize;
        for k in 0..=steps {
            let t = k as f64 / steps as f64;
            let (x, y) = (r.from[0] + t * (r.to[0] - r.from[0]), r.from[1] + t * (r.to[1] - r.from[1]));
            if x < 0.0 || y < 0.0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let i = ((y as usize) * w + x as usize) * 4;
            if let Some(px) = rgba.get_mut(i..i + 3) {
                px.copy_from_slice(&c);
            }
        }
    }
}
