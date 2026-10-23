//! `scan.regions`: the faces `scan.scene` found, grouped into regions (a crate's front with its
//! ribs and rims, a lid, the floor), cleaned up, the floor switched off. The grouping is given
//! by camera pixels ("the face under (x, y)"), so it survives a rescan while camera and scene
//! stay; it lives in a small file next to the scene that edits (merge, split, disable, ...)
//! change, so an editor needs nothing but this command. The math is
//! [`penumbra_calib::regions`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use penumbra_calib::objects::{self, Map};
use penumbra_calib::regions::{self, FaceFit, Kind, MAX_REGIONS, ShapeOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::scan::{
    CAMERA_MAP, OBJECTS_CAMERA, PROJECTOR_MAP, colour, labels_file, ms, read_labels, read_map, write_file,
};
use crate::scan_relief::round;
use crate::scan_scene::{DEFAULT_SCENE_OUT, PLANES_CAMERA, SCENE_JSON};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// The grouping, in the scene folder unless told otherwise.
pub const REGIONS_FILE: &str = "regions.json";
pub const REGIONS_CAMERA: &str = "regions_camera.png";
pub const REGIONS_VIEW: &str = "regions_view.png";
pub const REGIONS_REPLY: &str = "regions_reply.json";
/// A seed finds the nearest face this far away (camera pixels) when it lies on none: after a
/// rescan the faces' edges move a little.
const DEFAULT_REACH: u32 = 12;
/// Most seeds a region holds, and most regions a grouping lists.
const MAX_SEEDS: usize = 1024;
const MAX_DEFS: usize = 4096;

pub fn regions_file(n: usize) -> String {
    format!("regions_{n}.png")
}

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.regions",
        "The faces scan.scene found grouped into regions, one per part of the scene content treats as one \
         (a crate's front with its rib strips and rims, a lid, a side), cleaned up (seams between a region's \
         faces closed, holes filled, the ragged edge smoothed, four straight edges where the region is a \
         quadrilateral) and the floor switched off. The grouping is a list of regions, each a set of seed \
         pixels in the camera picture (the face under each seed, or the nearest one within reach, belongs \
         to it), an optional name and an optional enabled flag; faces under no seed are regions of their \
         own; a region is enabled unless it says otherwise or lies mostly on the floor (found from the \
         faces' planes against the support). It is kept in a file (regions.json in the scene folder) that \
         `regions` replaces and `edit` changes: [{op: merge, at: [[x, y], ...], name?} (the faces under \
         the points, and the regions they are in, become one region), {op: split, at: [x, y]} (the face \
         under it leaves its region), {op: disable | enable | auto, at} (its region off, on, or back to \
         the default), {op: name, at, name}, {op: clear}]. Writes regions_camera.png and regions_N.png per \
         projector (8-bit, 0 none or disabled, enabled regions numbered top of the camera picture first; \
         drop-in replacements for planes_camera.png and planes_N.png), regions_view.png (regions in \
         colour, disabled faces striped grey, seeds as dots) and regions_reply.json (the reply: per region \
         its faces, seeds, kind floor/level/upright, size, straight corners and per projector its plane; \
         the faces with a pixel inside each to address them by; suggestions: thin faces lying on the \
         plane of a large one they touch, rib strips next to their panel). {scene? (folder scan.scene \
         wrote, scene), file? (the grouping, scene/regions.json), regions? ([{name?, seeds: [[x, y], \
         ...], enabled?}, ...], replaces the file's), edit? ([op, ...], applied to the file's), out? \
         (folder, the scene folder), seam? (camera px, 10), smooth? (camera px, 2.5), straight? (true), \
         reach? (camera px, 12)}",
        move |p| {
            let a: RegionsArgs = args(p)?;
            run(e.session().dir(), &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionsArgs {
    pub scene: Option<PathBuf>,
    pub file: Option<PathBuf>,
    pub regions: Option<Vec<RegionDef>>,
    pub edit: Option<Vec<Edit>>,
    pub out: Option<PathBuf>,
    pub seam: Option<u32>,
    pub smooth: Option<f32>,
    pub straight: Option<bool>,
    pub reach: Option<u32>,
}

/// A region as the grouping lists it: the faces under its seeds (camera pixels).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegionDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub seeds: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// The file the grouping is kept in.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Grouping {
    #[serde(default)]
    pub regions: Vec<RegionDef>,
}

/// A change to the grouping, by camera pixel.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum Edit {
    Merge { at: Vec<[f64; 2]>, name: Option<String> },
    Split { at: [f64; 2] },
    Disable { at: [f64; 2] },
    Enable { at: [f64; 2] },
    Auto { at: [f64; 2] },
    Name { at: [f64; 2], name: String },
    Clear,
}

/// The camera's face labels and how points find faces on them.
struct Faces<'a> {
    w: usize,
    h: usize,
    planes: &'a [u16],
    reach: u32,
}

impl Faces<'_> {
    fn at(&self, p: [f64; 2]) -> Option<u16> {
        regions::face_at(self.w, self.h, self.planes, p, self.reach)
    }

    /// The first region in `defs` holding face `f` (a face goes to the first region that names it).
    fn region_of(&self, defs: &[RegionDef], f: u16) -> Option<usize> {
        defs.iter().position(|d| d.seeds.iter().any(|s| self.at(*s) == Some(f)))
    }
}

/// `edits` applied to `defs`; what did nothing goes into `warnings`.
fn apply(defs: &mut Vec<RegionDef>, edits: &[Edit], faces: &Faces<'_>, warnings: &mut Vec<String>) {
    let on_face = |p: [f64; 2], warnings: &mut Vec<String>| -> Option<u16> {
        let f = faces.at(p);
        if f.is_none() {
            warnings.push(format!("no face at ({}, {}) or within reach; left out", p[0], p[1]));
        }
        f
    };
    for e in edits {
        match e {
            Edit::Merge { at, name } => {
                let hits: Vec<([f64; 2], u16)> =
                    at.iter().filter_map(|p| Some((*p, on_face(*p, warnings)?))).collect();
                let mut idx: Vec<usize> =
                    hits.iter().filter_map(|(_, f)| faces.region_of(defs, *f)).collect();
                idx.sort_unstable();
                idx.dedup();
                let target = match idx.first() {
                    Some(t) => *t,
                    None => {
                        if hits.is_empty() {
                            continue;
                        }
                        defs.push(RegionDef::default());
                        defs.len() - 1
                    }
                };
                for k in idx.iter().skip(1).rev() {
                    if *k < defs.len() {
                        let moved = defs.remove(*k);
                        if let Some(t) = defs.get_mut(target) {
                            t.seeds.extend(moved.seeds);
                            if t.name.is_none() {
                                t.name = moved.name;
                            }
                        }
                    }
                }
                for (p, f) in hits {
                    let has =
                        defs.get(target).is_some_and(|t| t.seeds.iter().any(|s| faces.at(*s) == Some(f)));
                    if !has && let Some(t) = defs.get_mut(target) {
                        t.seeds.push(p);
                    }
                }
                if let (Some(n), Some(t)) = (name, defs.get_mut(target)) {
                    t.name = Some(n.clone());
                }
            }
            Edit::Split { at } => {
                let Some(f) = on_face(*at, warnings) else { continue };
                let before = defs.iter().map(|d| d.seeds.len()).sum::<usize>();
                for d in defs.iter_mut() {
                    d.seeds.retain(|s| faces.at(*s) != Some(f));
                }
                defs.retain(|d| !d.seeds.is_empty());
                if defs.iter().map(|d| d.seeds.len()).sum::<usize>() == before {
                    warnings.push(format!("split: face {f} at ({}, {}) is in no region", at[0], at[1]));
                }
            }
            Edit::Disable { at } | Edit::Enable { at } | Edit::Auto { at } | Edit::Name { at, .. } => {
                let Some(f) = on_face(*at, warnings) else { continue };
                let k = match faces.region_of(defs, f) {
                    Some(k) => k,
                    None => {
                        defs.push(RegionDef { seeds: vec![*at], ..RegionDef::default() });
                        defs.len() - 1
                    }
                };
                let Some(d) = defs.get_mut(k) else { continue };
                match e {
                    Edit::Disable { .. } => d.enabled = Some(false),
                    Edit::Enable { .. } => d.enabled = Some(true),
                    Edit::Auto { .. } => d.enabled = None,
                    Edit::Name { name, .. } => d.name = Some(name.clone()),
                    _ => {}
                }
            }
            Edit::Clear => defs.clear(),
        }
    }
    // A region that is only a face of its own, by default, says nothing.
    defs.retain(|d| !d.seeds.is_empty() && (d.seeds.len() > 1 || d.name.is_some() || d.enabled.is_some()));
}

fn check_defs(defs: &[RegionDef]) -> CmdResult<()> {
    if defs.len() > MAX_DEFS {
        return Err(CommandError(format!("at most {MAX_DEFS} regions")));
    }
    for d in defs {
        if d.seeds.len() > MAX_SEEDS {
            return Err(CommandError(format!("a region holds at most {MAX_SEEDS} seeds")));
        }
        if d.seeds.iter().flatten().any(|v| !v.is_finite()) {
            return Err("a region's `seeds` are [x, y] pairs of numbers".into());
        }
    }
    Ok(())
}

/// One region as found: its faces, where it came from, and whether it is on.
struct Found {
    name: Option<String>,
    faces: Vec<u16>,
    seeds: Vec<[f64; 2]>,
    given: bool,
    enabled: bool,
    enabled_by: &'static str,
    kind: Kind,
    camera_y: f64,
    number: u16,
}

pub fn run(dir: &Path, a: &RegionsArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let scene_dir = dir.join(a.scene.as_deref().unwrap_or(Path::new(DEFAULT_SCENE_OUT)));
    let defaults = ShapeOptions::default();
    let shape_options = ShapeOptions {
        seam: a.seam.unwrap_or(defaults.seam),
        smooth: a.smooth.unwrap_or(defaults.smooth),
        straight: a.straight.unwrap_or(defaults.straight),
    };
    shape_options.check().map_err(CommandError)?;
    let reach = a.reach.unwrap_or(DEFAULT_REACH);
    if reach > 256 {
        return Err("`reach` is 0 to 256 camera pixels".into());
    }

    // What scan.scene wrote.
    let scene_file = scene_dir.join(SCENE_JSON);
    let text = std::fs::read_to_string(&scene_file)
        .map_err(|e| format!("{}: {e} (run scan.scene first)", scene_file.display()))?;
    let scene: Json = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", scene_file.display()))?;
    let size = |v: &Json| -> Option<(usize, usize)> {
        Some((usize::try_from(v.get(0)?.as_u64()?).ok()?, usize::try_from(v.get(1)?.as_u64()?).ok()?))
    };
    let camera = scene.get("camera").and_then(size).ok_or("scene.json names no camera size")?;
    let (cw, ch) = camera;
    if cw == 0 || ch == 0 || cw.saturating_mul(ch) > objects::MAX_PIXELS {
        return Err("scene.json's camera size is out of range".into());
    }
    let face_count =
        scene.get("planes").and_then(Json::as_array).map_or(0, |p| p.len()).min(regions::MAX_REGIONS);
    let support = scene
        .get("support")
        .and_then(|s| s.get("plane"))
        .and_then(Json::as_u64)
        .and_then(|s| u16::try_from(s).ok());
    let planes = read_labels(&scene_dir.join(PLANES_CAMERA), camera)?;
    let objects_camera = read_labels(&scene_dir.join(OBJECTS_CAMERA), camera)?;
    let scans: Vec<PathBuf> = scene
        .get("projectors")
        .and_then(Json::as_array)
        .map(|l| l.iter().filter_map(|p| p.get("scan").and_then(Json::as_str).map(|s| dir.join(s))).collect())
        .unwrap_or_default();
    if scans.is_empty() || scans.len() > objects::MAX_PROJECTORS {
        return Err("scene.json lists no projectors' scans".into());
    }
    let mut to_camera: Vec<Map> = Vec::new();
    let mut from_camera: Vec<Map> = Vec::new();
    let mut proj_objects: Vec<Vec<u16>> = Vec::new();
    for (n, folder) in scans.iter().enumerate() {
        let t = read_map(&folder.join(PROJECTOR_MAP))?;
        let f = read_map(&folder.join(CAMERA_MAP))?;
        if (f.width, f.height) != camera {
            return Err(CommandError(format!(
                "{}: not the camera's size",
                folder.join(CAMERA_MAP).display()
            )));
        }
        proj_objects.push(read_labels(&scene_dir.join(labels_file(n)), (t.width, t.height))?);
        to_camera.push(t);
        from_camera.push(f);
    }
    let read = started.elapsed();

    // The grouping: from the file, replaced, edited, saved when changed.
    let t = Instant::now();
    let mut warnings: Vec<String> = Vec::new();
    let file = match &a.file {
        Some(f) => dir.join(f),
        None => scene_dir.join(REGIONS_FILE),
    };
    let mut grouping: Grouping = match std::fs::read_to_string(&file) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Grouping::default(),
        Err(e) => return Err(CommandError(format!("{}: {e}", file.display()))),
    };
    let faces = Faces { w: cw, h: ch, planes: &planes, reach };
    let changed = a.regions.is_some() || a.edit.is_some();
    if let Some(r) = &a.regions {
        grouping.regions = r.clone();
    }
    check_defs(&grouping.regions)?;
    if let Some(edits) = &a.edit {
        for e in edits {
            if let Edit::Merge { at, .. } = e
                && at.len() > MAX_SEEDS
            {
                return Err(CommandError(format!("a merge names at most {MAX_SEEDS} points")));
            }
        }
        apply(&mut grouping.regions, edits, &faces, &mut warnings);
    }
    check_defs(&grouping.regions)?;
    if changed {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let text = serde_json::to_vec_pretty(&grouping).map_err(|e| CommandError(e.to_string()))?;
        write_file(&file, text)?;
    }

    // The faces' planes and classes, and a pixel inside each to address it by.
    let maps: Vec<&Map> = from_camera.iter().collect();
    let sizes: Vec<(usize, usize)> = to_camera.iter().map(|m| (m.width, m.height)).collect();
    let fits = regions::fit_faces(camera, &planes, face_count, &maps, &sizes);
    let mut face_pixels = vec![0usize; face_count];
    let mut face_y = vec![0f64; face_count];
    for (i, l) in planes.iter().enumerate() {
        if let Some(k) = usize::from(*l).checked_sub(1)
            && let (Some(n), Some(y)) = (face_pixels.get_mut(k), face_y.get_mut(k))
        {
            *n += 1;
            *y += (i / cw) as f64;
        }
    }
    for (y, n) in face_y.iter_mut().zip(&face_pixels) {
        *y /= (*n).max(1) as f64;
    }
    let (kinds, heights, floor) = regions::classify(camera, &fits, support, &face_y);
    if support.is_none() {
        warnings.push("scene.json names no support: the faces are not classed, the floor stays on".into());
    } else if floor.is_none() {
        warnings.push("no floor found below the support: every region stays on unless told otherwise".into());
    }
    let centres = regions::face_centres(cw, ch, &planes, face_count);
    let object_mask: Vec<bool> = objects_camera.iter().map(|o| *o > 0).collect();
    let suggested = regions::suggestions(cw, ch, &planes, &object_mask, &fits, &kinds);

    // The regions: the grouping's, then every face left over on its own.
    let mut owner: Vec<Option<usize>> = vec![None; face_count];
    let mut found: Vec<Found> = Vec::new();
    let kind_of =
        |f: u16| usize::from(f).checked_sub(1).and_then(|k| kinds.get(k).copied()).unwrap_or(Kind::Unknown);
    for (j, d) in grouping.regions.iter().enumerate() {
        let mut members: Vec<u16> = Vec::new();
        for s in &d.seeds {
            match faces.at(*s) {
                None => warnings.push(format!("region {j}: no face at seed ({}, {})", s[0], s[1])),
                Some(f) => {
                    let Some(slot) = owner.get_mut(usize::from(f) - 1) else { continue };
                    match slot {
                        Some(o) if *o != found.len() => warnings.push(format!(
                            "region {j}: face {f} at seed ({}, {}) is already in an earlier region",
                            s[0], s[1]
                        )),
                        Some(_) => {}
                        None => {
                            *slot = Some(found.len());
                            members.push(f);
                        }
                    }
                }
            }
        }
        members.sort_unstable();
        if members.is_empty() {
            warnings.push(format!(
                "region {j}{}: none of its seeds lies on a face; left out",
                name_note(&d.name)
            ));
            continue;
        }
        found.push(Found {
            name: d.name.clone(),
            faces: members,
            seeds: d.seeds.clone(),
            given: true,
            enabled: d.enabled.unwrap_or(true),
            enabled_by: if d.enabled.is_some() { "given" } else { "default" },
            kind: Kind::Unknown,
            camera_y: 0.0,
            number: 0,
        });
    }
    for k in 0..face_count {
        if owner.get(k).is_some_and(|o| o.is_none()) && face_pixels.get(k).is_some_and(|n| *n > 0) {
            let Ok(f) = u16::try_from(k + 1) else { continue };
            let seed = centres.get(k).copied().flatten().map(|[x, y]| [x as f64 + 0.5, y as f64 + 0.5]);
            found.push(Found {
                name: None,
                faces: vec![f],
                seeds: seed.into_iter().collect(),
                given: false,
                enabled: true,
                enabled_by: "default",
                kind: Kind::Unknown,
                camera_y: 0.0,
                number: 0,
            });
        }
    }
    // Kind and place by size; the floor off unless the grouping says otherwise.
    for r in &mut found {
        let mut tally: Vec<(Kind, usize)> = Vec::new();
        let (mut sy, mut n) = (0.0, 0usize);
        for f in &r.faces {
            let k = usize::from(*f) - 1;
            let px = face_pixels.get(k).copied().unwrap_or(0);
            sy += face_y.get(k).copied().unwrap_or(0.0) * px as f64;
            n += px;
            let kind = kind_of(*f);
            match tally.iter_mut().find(|(x, _)| *x == kind) {
                Some(t) => t.1 += px,
                None => tally.push((kind, px)),
            }
        }
        r.camera_y = sy / n.max(1) as f64;
        r.kind = tally.iter().max_by_key(|(_, c)| *c).map_or(Kind::Unknown, |(k, _)| *k);
        if r.enabled_by == "default" && r.kind == Kind::Floor {
            r.enabled = false;
            r.enabled_by = "floor";
        }
    }
    let mut order: Vec<usize> =
        (0..found.len()).filter(|k| found.get(*k).is_some_and(|r| r.enabled)).collect();
    order.sort_by(|x, y| {
        let cy = |k: &usize| found.get(*k).map_or(0.0, |r| r.camera_y);
        cy(x).total_cmp(&cy(y))
    });
    if order.len() > MAX_REGIONS {
        return Err(CommandError(format!(
            "{} regions are on, at most {MAX_REGIONS} fit the label pictures: merge or disable some",
            order.len()
        )));
    }
    for (n, k) in order.iter().enumerate() {
        if let Some(r) = found.get_mut(*k) {
            r.number = u16::try_from(n + 1).unwrap_or(0);
        }
    }
    let mut number_of_face = vec![0u16; face_count + 1];
    for r in &found {
        for f in &r.faces {
            if let Some(s) = number_of_face.get_mut(usize::from(*f)) {
                *s = r.number;
            }
        }
    }

    // Cleaned in the camera picture, carried to every projector.
    let base: Vec<u16> =
        planes.iter().map(|l| number_of_face.get(usize::from(*l)).copied().unwrap_or(0)).collect();
    let free: Vec<bool> = planes.iter().zip(&object_mask).map(|(l, o)| *l == 0 && !o).collect();
    let shaped = regions::shape(cw, ch, &base, &free, &object_mask, order.len(), &shape_options);
    let per_projector: Vec<Vec<u16>> = to_camera
        .iter()
        .zip(&proj_objects)
        .map(|(t, o)| regions::projector_regions(camera, &shaped.labels, t, o))
        .collect();
    let work = t.elapsed();

    // Files.
    let t = Instant::now();
    let out = match &a.out {
        Some(o) => dir.join(o),
        None => scene_dir.clone(),
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let write = |name: &str, bytes: Vec<u8>| write_file(&out.join(name), bytes);
    let bytes =
        |labels: &[u16]| -> Vec<u8> { labels.iter().map(|l| u8::try_from(*l).unwrap_or(0)).collect() };
    let camera_file = write(
        REGIONS_CAMERA,
        penumbra_gpu::image::encode_png_gray8(cw as u32, ch as u32, bytes(&shaped.labels))?,
    )?;
    let mut projector_files = Vec::new();
    for (n, (labels, m)) in per_projector.iter().zip(&to_camera).enumerate() {
        projector_files.push(write(
            &regions_file(n),
            penumbra_gpu::image::encode_png_gray8(m.width as u32, m.height as u32, bytes(labels))?,
        )?);
    }
    let disabled_faces: Vec<bool> =
        planes.iter().map(|l| *l > 0 && number_of_face.get(usize::from(*l)) == Some(&0)).collect();
    let seeds: Vec<[f64; 2]> =
        found.iter().filter(|r| r.given).flat_map(|r| r.seeds.iter().copied()).collect();
    let picture = view(cw, ch, &shaped.labels, &disabled_faces, &object_mask, &seeds);
    let view_file = write(REGIONS_VIEW, penumbra_gpu::encode_png_rgba8(cw as u32, ch as u32, picture)?)?;

    let point = |p: &[f64; 2]| json!([round(p[0]), round(p[1])]);
    let mut region_list: Vec<Json> = Vec::new();
    for k in &order {
        let Some(r) = found.get(*k) else { continue };
        let camera_pixels = shaped.labels.iter().filter(|l| **l == r.number).count();
        let pixels: Vec<usize> =
            per_projector.iter().map(|l| l.iter().filter(|x| **x == r.number).count()).collect();
        let plane: Vec<Json> = (0..to_camera.len())
            .map(|p| {
                let member: Vec<&FaceFit> =
                    r.faces.iter().filter_map(|f| fits.get(usize::from(*f) - 1)?.get(p)?.as_ref()).collect();
                match regions::fit_union(&member) {
                    Some((h, errors)) => json!({
                        "projector_to_camera": h.rows().map(|row| row.map(|v| (v * 1e9).round() / 1e9)),
                        "error_px": round(errors.iter().copied().fold(0.0, f64::max)),
                    }),
                    None => Json::Null,
                }
            })
            .collect();
        region_list.push(json!({
            "region": r.number,
            "name": r.name,
            "faces": r.faces,
            "seeds": r.seeds.iter().map(point).collect::<Vec<_>>(),
            "given": r.given,
            "kind": r.kind.name(),
            "enabled_by": r.enabled_by,
            "camera_y": r.camera_y.round(),
            "camera_pixels": camera_pixels,
            "pixels": pixels,
            "straight": usize::from(r.number).checked_sub(1).and_then(|i| shaped.straight.get(i).copied().flatten()).map(|c| c.iter().map(point).collect::<Vec<_>>()),
            "plane": plane,
        }));
    }
    let disabled: Vec<Json> = found
        .iter()
        .filter(|r| !r.enabled)
        .map(|r| {
            json!({
                "name": r.name,
                "faces": r.faces,
                "seeds": r.seeds.iter().map(point).collect::<Vec<_>>(),
                "given": r.given,
                "kind": r.kind.name(),
                "by": r.enabled_by,
            })
        })
        .collect();
    let face_list: Vec<Json> = (0..face_count)
        .filter(|k| face_pixels.get(*k).is_some_and(|n| *n > 0))
        .map(|k| {
            json!({
                "face": k + 1,
                "region": number_of_face.get(k + 1).copied().unwrap_or(0),
                "at": centres.get(k).copied().flatten(),
                "kind": kinds.get(k).map_or("unknown", |x| x.name()),
                "height": heights.get(k).copied().flatten().map(round),
                "camera_pixels": face_pixels.get(k),
            })
        })
        .collect();
    let at_face = |f: u16| usize::from(f).checked_sub(1).and_then(|k| centres.get(k).copied().flatten());
    let suggestion_list: Vec<Json> = suggested
        .iter()
        .filter(|s| {
            number_of_face.get(usize::from(s.strip)) != number_of_face.get(usize::from(s.panel))
                || number_of_face.get(usize::from(s.strip)) == Some(&0)
        })
        .map(|s| {
            json!({
                "faces": [s.strip, s.panel],
                "at": [at_face(s.strip), at_face(s.panel)],
                "why": "a thin face on the plane of a large one it touches (a rib strip next to its panel)",
                "contact_px": s.contact,
                "off_plane_px": round(s.error_px),
            })
        })
        .collect();
    let written = t.elapsed();
    let message = format!(
        "{} face(s) in {} region(s), {} of them off{}",
        face_list.len(),
        found.len(),
        disabled.len(),
        match floor {
            Some(f) => format!("; the floor is face {f}'s plane"),
            None => String::new(),
        }
    );
    let reply = json!({
        "message": message,
        "camera": [cw, ch],
        "regions": region_list,
        "disabled": disabled,
        "faces": face_list,
        "floor": floor,
        "support": support,
        "suggestions": suggestion_list,
        "grouping": {"file": file.display().to_string(), "saved": changed, "regions": grouping.regions},
        "files": {"camera": camera_file, "projectors": projector_files, "view": view_file},
        "options": {"seam": shape_options.seam, "smooth": shape_options.smooth, "straight": shape_options.straight, "reach": reach},
        "ms": {"read": ms(read), "work": ms(work), "write": ms(written), "total": ms(started.elapsed())},
        "warnings": warnings,
        "next": "regions_camera.png in place of planes_camera.png, regions_N.png in place of planes_N.png (same \
                 8-bit labels, 0 none); group faces with edit [{op: merge, at: [[x, y], ...]}], switch one off \
                 with {op: disable, at: [x, y]}; suggestions list faces that probably belong together",
    });
    let text = serde_json::to_vec_pretty(&reply).map_err(|e| CommandError(e.to_string()))?;
    write(REGIONS_REPLY, text)?;
    Ok(reply)
}

fn name_note(name: &Option<String>) -> String {
    name.as_ref().map_or(String::new(), |n| format!(" ({n})"))
}

/// Regions in colour with dark borders, disabled faces striped grey, objects white, seeds as
/// dots (RGBA bytes).
fn view(
    w: usize,
    h: usize,
    labels: &[u16],
    disabled: &[bool],
    objects: &[bool],
    seeds: &[[f64; 2]],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let (x, y) = (i % w.max(1), i / w.max(1));
        let l = labels.get(i).copied().unwrap_or(0);
        let border = l > 0
            && [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ]
            .into_iter()
            .flatten()
            .any(|j| labels.get(j).copied().unwrap_or(0) != l);
        let c = if border {
            [20, 20, 20]
        } else if l > 0 {
            colour(l).map(|v| (f32::from(v) * 0.85) as u8)
        } else if objects.get(i) == Some(&true) {
            [255, 255, 255]
        } else if disabled.get(i) == Some(&true) {
            if ((x + y) / 6).is_multiple_of(2) { [70, 70, 70] } else { [40, 40, 40] }
        } else {
            [0, 0, 0]
        };
        out.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    for s in seeds {
        let (sx, sy) = (s[0] as isize, s[1] as isize);
        for dy in -3isize..=3 {
            for dx in -3isize..=3 {
                let (x, y) = (sx + dx, sy + dy);
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    continue;
                }
                let c = if dx.abs() <= 1 && dy.abs() <= 1 { [0, 0, 0] } else { [255, 255, 255] };
                let i = (y as usize * w + x as usize) * 4;
                if let Some(px) = out.get_mut(i..i + 3) {
                    px.copy_from_slice(&c);
                }
            }
        }
    }
    out
}
