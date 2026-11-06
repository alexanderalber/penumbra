//! `calib.points.*`: the point pairs of one `view.camera`, what a fit of them says, and the commands
//! a calibration mode is built from. The list lives in the camera's `points` param, so it is saved,
//! undone and copied with the node, and a locked project refuses changes to it.
//!
//! A projector and a frozen camera picture use the same commands: `kind` in `calib.points.setup`
//! says which, and decides what stays free in the fit.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use penumbra_calib::points::{GroupDeviation, PictureKind, Point, PointSet, Quality, Report};
use penumbra_calib::{Camera, Options, expected_deviation};
use penumbra_core::{Name, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_mesh::{Limits, Mesh, Up};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{edit, kind_at, outcome, require, to_json};
use crate::engine::Engine;

type Result<T> = std::result::Result<T, CommandError>;

const CAMERA: &str = "view.camera";
/// Vertices sampled per part for an expected deviation; a part with more is thinned evenly.
const MAX_GROUP_POINTS: usize = 2000;
/// Default of `calib.points.corners`: a corner has faces this far apart in direction.
const CORNER_DEGREES: f64 = 30.0;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> Result<Json>| {
        let e = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&e, p)));
    };
    add(
        "calib.points.setup",
        "Start or change the point list of a view.camera (one undo step); points already there stay. {path, size: [w,h], \
         kind? (projector: principal point free | picture: a frozen camera picture, principal point at its centre), \
         options? (as calib.solve), focal? (a known focal length in pixels, held fixed; null forgets it)}",
        points_setup,
    );
    add(
        "calib.points.set",
        "Add a point pair to a view.camera's list, or change the one with `id` (one undo step). New: {path, world: [x,y,z], \
         picture: [u,v], group?, off?}; changing: {path, id, world?, picture?, group?, off?}. Picture in pixels from the top \
         left corner. `off` keeps a point in the list but out of the fit. Replies its id",
        points_set,
    );
    add(
        "calib.points.remove",
        "Remove point pairs from a view.camera's list (one undo step); {path, id} or {path, all: true}",
        points_remove,
    );
    add("calib.points.list", "A view.camera's point list as stored; {path}", points_list);
    add(
        "calib.points.solve",
        "Fit the camera to its point list and judge the result; the camera goes into its `calibration` param (one undo \
         step) unless `apply` is false. {path, apply?, geo? (a geo.file: expected deviation per part), noise? (click noise \
         per axis in pixels)}. Replies camera, rms, spread, pose (position, forward, up, distance and how well they are \
         known), points (id, error, left_out, expected, ratio: a point's error when left out of the fit, over what is \
         expected there), suspects (ids), deviation and quality (with geo), warnings",
        points_solve,
    );
    add(
        "calib.points.predict",
        "Where the camera's current calibration puts model points, in pixels, and how far that may be off; {path, points: \
         [[x,y,z],...], noise?}. Replies pixels (null behind the camera) and deviation (null without a fit of the list)",
        points_predict,
    );
    add(
        "calib.points.deviation",
        "Expected pixel deviation of the camera's calibration on each part of the object, with the fit's quality (good, \
         fair, weak, unusable); {path, geo (a geo.file) or groups: {name: [[x,y,z],...]}, noise?}",
        points_deviation,
    );
    add(
        "calib.points.corners",
        "The sharp corners of a model, per part: the places a point can be set exactly. {geo (a geo.file), degrees? (how far \
         apart in direction its faces must be, 30)}. Replies parts: [{part, corners: [[x,y,z],...]}], in world coordinates",
        points_corners,
    );
}

fn err(e: impl std::fmt::Display) -> CommandError {
    CommandError(e.to_string())
}

/// The camera's point list text and calibration text; an error unless `path` is a `view.camera`.
fn camera_texts(e: &Engine, path: &NodePath) -> Result<(String, String)> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let node = net.node(id).ok_or_else(|| CommandError(format!("no node at {path}")))?;
    if node.kind != CAMERA {
        return Err(CommandError(format!("{path} is a {}, not a {CAMERA}", node.kind)));
    }
    let desc = kind_at(e, &net, path)?;
    let text = |name: &str| node.param(desc, name).and_then(Value::as_str).unwrap_or_default().to_string();
    Ok((text("points"), text("calibration")))
}

/// The stored text; an empty text is an error that says what to do.
fn parse_set(text: &str) -> Result<PointSet> {
    if text.trim().is_empty() {
        return Err("this camera has no point list yet: calib.points.setup starts one".into());
    }
    serde_json::from_str(text).map_err(|x| CommandError(format!("the point list does not read: {x}")))
}

fn point_set(e: &Engine, path: &NodePath) -> Result<PointSet> {
    let (text, _) = camera_texts(e, path)?;
    parse_set(&text)
}

fn set_param(path: &NodePath, name: &str, text: String) -> Result<Op> {
    let param = ParamPath::new(path.clone(), Name::new(name)?)?;
    Ok(Op::Set { param, value: Value::Text(text) })
}

fn write_points(e: &Engine, path: &NodePath, set: &PointSet, label: &str) -> Result<Json> {
    let text = serde_json::to_string(set).map_err(err)?;
    Ok(outcome(&edit(e, &[set_param(path, "points", text)?], label)?))
}

/// `{"focal": null}` differs from no `focal` at all.
fn some_option<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<f64>>, D::Error> {
    Option::<f64>::deserialize(d).map(Some)
}

fn points_setup(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        size: [u32; 2],
        kind: Option<PictureKind>,
        options: Option<Options>,
        #[serde(default, deserialize_with = "some_option")]
        focal: Option<Option<f64>>,
    }
    let a: A = args(p)?;
    if a.size.iter().any(|s| *s == 0 || *s > 65_536) {
        return Err(CommandError(format!(
            "the picture size must be 1 to 65536 pixels per side, got {:?}",
            a.size
        )));
    }
    if let Some(Some(f)) = a.focal
        && !(f.is_finite() && f > 0.0)
    {
        return Err("a known focal length must be a positive number".into());
    }
    let (text, _) = camera_texts(e, &a.path)?;
    let mut set = if text.trim().is_empty() {
        PointSet::new(a.size, a.kind.unwrap_or_default())
    } else {
        parse_set(&text)?
    };
    set.size = a.size;
    if let Some(kind) = a.kind {
        set.kind = kind;
        set.options = kind.options();
    }
    if let Some(o) = a.options {
        set.options = o;
    }
    if let Some(f) = a.focal {
        set.focal = f;
    }
    let out = write_points(e, &a.path, &set, "calibration setup")?;
    Ok(
        json!({"size": set.size, "kind": set.kind, "options": set.options, "focal": set.focal, "points": set.points.len(), "edit": out}),
    )
}

fn finite(v: &[f64], what: &str) -> Result<()> {
    if v.iter().all(|x| x.is_finite() && x.abs() <= penumbra_calib::MAX_COORDINATE) {
        Ok(())
    } else {
        Err(CommandError(format!("`{what}` must be finite numbers")))
    }
}

fn points_set(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        id: Option<u32>,
        world: Option<[f64; 3]>,
        picture: Option<[f64; 2]>,
        #[serde(default, deserialize_with = "some_group")]
        group: Option<Option<String>>,
        off: Option<bool>,
    }
    fn some_group<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(d).map(Some)
    }
    let a: A = args(p)?;
    if let Some(w) = &a.world {
        finite(w, "world")?;
    }
    if let Some(q) = &a.picture {
        finite(q, "picture")?;
    }
    let mut set = point_set(e, &a.path)?;
    let id = match a.id {
        Some(id) => {
            let point = set
                .points
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or_else(|| CommandError(format!("{} has no point {id}", a.path)))?;
            if let Some(w) = a.world {
                point.world = w;
            }
            if let Some(q) = a.picture {
                point.picture = q;
            }
            if let Some(g) = a.group {
                point.group = g;
            }
            if let Some(off) = a.off {
                point.off = off;
            }
            id
        }
        None => {
            let (Some(world), Some(picture)) = (a.world, a.picture) else {
                return Err("a new point needs `world` and `picture` (`id` changes one that exists)".into());
            };
            let id = set.next_id();
            set.points.push(Point {
                id,
                world,
                picture,
                group: a.group.flatten(),
                off: a.off.unwrap_or(false),
            });
            id
        }
    };
    let out = write_points(e, &a.path, &set, "calibration point")?;
    Ok(json!({"id": id, "points": set.points.len(), "edit": out}))
}

fn points_remove(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        id: Option<u32>,
        #[serde(default)]
        all: bool,
    }
    let a: A = args(p)?;
    let mut set = point_set(e, &a.path)?;
    match (a.id, a.all) {
        (Some(id), false) => {
            let before = set.points.len();
            set.points.retain(|c| c.id != id);
            if set.points.len() == before {
                return Err(CommandError(format!("{} has no point {id}", a.path)));
            }
        }
        (None, true) => set.points.clear(),
        _ => return Err("give either `id` or `all: true`".into()),
    }
    let out = write_points(e, &a.path, &set, "calibration point removed")?;
    Ok(json!({"points": set.points.len(), "edit": out}))
}

fn points_list(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
    }
    let a: A = args(p)?;
    to_json(&point_set(e, &a.path)?)
}

/// The vertices of a `geo.file`'s model in world coordinates, as the node shows them.
fn model_of(e: &Engine, path: &NodePath) -> Result<Mesh> {
    let net = e.session().doc().snapshot();
    let id = require(&net, path)?;
    let node = net.node(id).ok_or_else(|| CommandError(format!("no node at {path}")))?;
    if node.kind != "geo.file" {
        return Err(CommandError(format!("{path} is a {}, not a geo.file", node.kind)));
    }
    let desc = kind_at(e, &net, path)?;
    let text = |name: &str| node.param(desc, name).and_then(Value::as_str).unwrap_or_default().to_string();
    let file = text("file");
    if file.trim().is_empty() {
        return Err(CommandError(format!("{path} has no file")));
    }
    let name = std::path::Path::new(file.trim());
    let full = if name.is_absolute() { name.to_path_buf() } else { e.session().dir().join(name) };
    let mut loaded = penumbra_mesh::load_file(&full, &Limits::default()).map_err(err)?;
    let up = match text("up").as_str() {
        "y" => Up::Y,
        "z" => Up::Z,
        _ => Up::of_file(&full.to_string_lossy()),
    };
    loaded.mesh.to_z_up(up);
    Ok(loaded.mesh)
}

/// Every part's triangles as index triples.
fn triangles_by_part(mesh: &Mesh) -> Vec<(String, Vec<[u32; 3]>)> {
    let mut parts: Vec<(String, Vec<[u32; 3]>)> =
        mesh.parts.iter().map(|name| (name.clone(), Vec::new())).collect();
    for range in &mesh.ranges {
        let first = range.first as usize;
        let last = first.saturating_add(range.count as usize).min(mesh.indices.len());
        let Some(indices) = mesh.indices.get(first..last) else { continue };
        let Some((_, tris)) = parts.get_mut(range.part as usize) else { continue };
        tris.extend(indices.as_chunks::<3>().0.iter().copied());
    }
    parts
}

/// Each part's vertices, thinned to at most [`MAX_GROUP_POINTS`].
fn groups_of(mesh: &Mesh) -> BTreeMap<String, Vec<[f64; 3]>> {
    let mut out = BTreeMap::new();
    for (name, tris) in triangles_by_part(mesh) {
        let mut used: Vec<u32> = tris.iter().flatten().copied().collect();
        used.sort_unstable();
        used.dedup();
        if used.is_empty() {
            continue;
        }
        let step = used.len().div_ceil(MAX_GROUP_POINTS).max(1);
        let points: Vec<[f64; 3]> = used
            .iter()
            .step_by(step)
            .filter_map(|i| mesh.vertices.get(*i as usize))
            .map(|v| v.position.map(f64::from))
            .collect();
        out.insert(name, points);
    }
    out
}

fn deviation_json(devs: &[GroupDeviation], quality: Quality) -> Json {
    json!({"deviation": devs, "quality": quality})
}

fn points_solve(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        apply: Option<bool>,
        geo: Option<NodePath>,
        noise: Option<f64>,
    }
    let a: A = args(p)?;
    let set = point_set(e, &a.path)?;
    let report: Report = set.fit().map_err(err)?;
    let mut reply = to_json(&report)?;
    let camera = report.solution.camera;
    if let Some(map) = reply.as_object_mut() {
        // Flat for the caller: the solution's fields beside the judgement.
        if let Some(Json::Object(solution)) = map.remove("solution") {
            for (k, v) in solution {
                map.insert(k, v);
            }
        }
    }
    let mut warnings = report.warnings.clone();
    if let Some(geo) = &a.geo {
        let groups = groups_of(&model_of(e, geo)?);
        let (devs, quality) = set.deviation(&camera, &groups, a.noise.or(Some(report.noise))).map_err(err)?;
        if quality >= Quality::Weak {
            let worst = devs.iter().max_by(|x, y| x.rms.total_cmp(&y.rms));
            if let Some(w) = worst {
                warnings.push(format!(
                    "The picture may be off by about {:.0} px on {}. More points, spread over the object and its plates, make it steadier.",
                    w.rms, w.group
                ));
            }
        }
        if let Some(map) = reply.as_object_mut() {
            map.insert("deviation".into(), json!(devs));
            map.insert("quality".into(), json!(quality));
        }
    }
    if let Some(map) = reply.as_object_mut() {
        map.insert("warnings".into(), json!(warnings));
    }
    if a.apply.unwrap_or(true) {
        let text = serde_json::to_string(&camera).map_err(err)?;
        let out = edit(e, &[set_param(&a.path, "calibration", text)?], "calibration solved")?;
        if let Some(map) = reply.as_object_mut() {
            map.insert("edit".into(), outcome(&out));
        }
    }
    Ok(reply)
}

/// The camera's stored calibration, or an error that says what to do.
fn stored_camera(e: &Engine, path: &NodePath) -> Result<Camera> {
    let (_, text) = camera_texts(e, path)?;
    if text.trim().is_empty() {
        return Err(CommandError(format!("{path} has no calibration yet: calib.points.solve makes one")));
    }
    serde_json::from_str(&text)
        .map_err(|x| CommandError(format!("{path}: the calibration does not read: {x}")))
}

fn points_predict(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        points: Vec<[f64; 3]>,
        noise: Option<f64>,
    }
    let a: A = args(p)?;
    for w in &a.points {
        finite(w, "points")?;
    }
    let camera = stored_camera(e, &a.path)?;
    let pixels: Vec<Option<[f64; 2]>> = a.points.iter().map(|w| camera.project(*w)).collect();
    let (text, _) = camera_texts(e, &a.path)?;
    let deviation = parse_set(&text).ok().and_then(|set| {
        let pairs: Vec<([f64; 3], [f64; 2])> =
            set.points.iter().filter(|c| !c.off).map(|c| (c.world, c.picture)).collect();
        let noise = a.noise.unwrap_or(penumbra_calib::points::MIN_CLICK_NOISE);
        let options = Options { fix_focal: set.focal.is_some(), ..set.options };
        expected_deviation(&pairs, &camera, &options, Some(noise), &a.points).ok()
    });
    let deviation = deviation.unwrap_or_else(|| vec![None; a.points.len()]);
    Ok(json!({"pixels": pixels, "deviation": deviation}))
}

fn points_deviation(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: NodePath,
        geo: Option<NodePath>,
        groups: Option<BTreeMap<String, Vec<[f64; 3]>>>,
        noise: Option<f64>,
    }
    let a: A = args(p)?;
    let groups = match (a.geo, a.groups) {
        (Some(geo), None) => groups_of(&model_of(e, &geo)?),
        (None, Some(groups)) => groups,
        _ => return Err("give either `geo` or `groups`".into()),
    };
    for w in groups.values().flatten() {
        finite(w, "groups")?;
    }
    let set = point_set(e, &a.path)?;
    let camera = stored_camera(e, &a.path)?;
    let (devs, quality) = set.deviation(&camera, &groups, a.noise).map_err(err)?;
    Ok(deviation_json(&devs, quality))
}

/// A place on a mesh and the directions of the faces that meet there.
type Place = ([f32; 3], Vec<[f64; 3]>);

/// Direction of a triangle's face, or `None` for one without area.
fn face_direction(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<[f64; 3]> {
    let d =
        |p: [f32; 3], q: [f32; 3]| [f64::from(q[0] - p[0]), f64::from(q[1] - p[1]), f64::from(q[2] - p[2])];
    let (u, v) = (d(a, b), d(a, c));
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    (len > 1e-14).then(|| [n[0] / len, n[1] / len, n[2] / len])
}

/// The places on a part where faces of three or more directions meet: true 3D corners. A curve cut
/// into short flat steps has only two directions at each place, so it yields none.
fn corners_of(mesh: &Mesh, tris: &[[u32; 3]], degrees: f64) -> Vec<[f64; 3]> {
    let same = degrees.to_radians().cos();
    // Vertices at one place share a key, as files often repeat them per face.
    let key = |p: [f32; 3]| p.map(|v| (f64::from(v) * 1e5).round() as i64);
    let mut faces: HashMap<[i64; 3], Place> = HashMap::new();
    for t in tris {
        let Some([a, b, c]) = t
            .iter()
            .map(|i| mesh.vertices.get(*i as usize).map(|v| v.position))
            .collect::<Option<Vec<_>>>()
            .and_then(|v| <[[f32; 3]; 3]>::try_from(v).ok())
        else {
            continue;
        };
        let Some(n) = face_direction(a, b, c) else { continue };
        for p in [a, b, c] {
            faces.entry(key(p)).or_insert((p, Vec::new())).1.push(n);
        }
    }
    let mut out: Vec<[f64; 3]> = faces
        .values()
        .filter(|(_, normals)| {
            let mut kinds: Vec<[f64; 3]> = Vec::new();
            for n in normals {
                let new = kinds.iter().all(|k| k[0] * n[0] + k[1] * n[1] + k[2] * n[2] < same);
                if new {
                    kinds.push(*n);
                }
            }
            kinds.len() >= 3
        })
        .map(|(p, _)| p.map(f64::from))
        .collect();
    out.sort_by(|a, b| a[2].total_cmp(&b[2]).then(a[0].total_cmp(&b[0])).then(a[1].total_cmp(&b[1])));
    out
}

fn points_corners(e: &Engine, p: &Json) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        geo: NodePath,
        degrees: Option<f64>,
    }
    let a: A = args(p)?;
    let degrees = a.degrees.unwrap_or(CORNER_DEGREES);
    if !(1.0..=120.0).contains(&degrees) {
        return Err("`degrees` is 1 to 120".into());
    }
    let mesh = model_of(e, &a.geo)?;
    let parts: Vec<Json> = triangles_by_part(&mesh)
        .iter()
        .map(|(name, tris)| json!({"part": name, "corners": corners_of(&mesh, tris, degrees)}))
        .collect();
    Ok(json!({"parts": parts}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;
    use penumbra_project::{Content, Session};
    use std::fmt::Write as _;

    struct Fixture {
        registry: Registry,
        engine: Arc<Engine>,
        dir: ScratchDir,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = ScratchDir::unborn(&format!("calib-points-{name}"));
            let types = Arc::new(penumbra_ops::builtin_types().unwrap());
            let session = Arc::new(Session::open(dir.path(), types, &Content::default()).unwrap());
            let engine = Engine::start(session).unwrap();
            let registry = crate::engine_registry(&engine);
            Self { registry, engine, dir }
        }
        fn call(&self, method: &str, params: Json) -> Json {
            let r = self.registry.dispatch(method, &params);
            assert_eq!(r["ok"], true, "{method} {params}: {r}");
            r["result"].clone()
        }
        fn fail(&self, method: &str, params: Json) -> String {
            let r = self.registry.dispatch(method, &params);
            assert_eq!(r["ok"], false, "{method} {params} should fail: {r}");
            r["error"].as_str().unwrap().to_string()
        }
    }

    /// A stepped block in metres, y up in the file, two parts, sized as the test camera sees it.
    fn write_model(dir: &std::path::Path) -> String {
        let mut obj = String::new();
        let mut v = 1;
        for (name, z0, z1, half) in [("front", 0.03, 0.06, 0.2), ("back", 0.0, 0.03, 0.3)] {
            let _ = writeln!(obj, "o {name}");
            for (x, y, z) in [
                (-half, -half, z0),
                (half, -half, z0),
                (half, half, z0),
                (-half, half, z0),
                (-half, -half, z1),
                (half, -half, z1),
                (half, half, z1),
                (-half, half, z1),
            ] {
                let _ = writeln!(obj, "v {x} {y} {z}");
            }
            for f in [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]] {
                let _ = writeln!(obj, "f {} {} {} {}", v + f[0], v + f[1], v + f[2], v + f[3]);
            }
            v += 8;
        }
        let path = dir.join("block.obj");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, obj).unwrap();
        path.to_string_lossy().to_string()
    }

    fn truth() -> Camera {
        let pose = penumbra_calib::Pose::look_at([0.6, -1.8, 0.2], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]).unwrap();
        let mut intrinsics = penumbra_calib::Intrinsics::from_throw_ratio([1920, 1080], 1.4);
        intrinsics.principal = [960.0, 1000.0];
        Camera { intrinsics, pose }
    }

    #[test]
    fn from_clicks_to_a_camera_with_undo() {
        let f = Fixture::new("flow");
        f.call("node.create", json!({"path": "/cam", "kind": "view.camera"}));
        let model = write_model(f.dir.path());
        f.call("node.create", json!({"path": "/model", "kind": "geo.file", "params": {"file": model}}));

        // The model's corners are what can be clicked: every part has its eight.
        let parts = f.call("calib.points.corners", json!({"geo": "/model"}));
        let parts = parts["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "{parts:?}");
        for part in parts {
            assert_eq!(part["corners"].as_array().unwrap().len(), 8, "{part}");
        }

        assert!(f.fail("calib.points.list", json!({"path": "/cam"})).contains("calib.points.setup"));
        assert!(
            f.fail("calib.points.setup", json!({"path": "/model", "size": [1920, 1080]}))
                .contains("not a view.camera")
        );
        f.call("calib.points.setup", json!({"path": "/cam", "size": [1920, 1080]}));
        assert!(
            f.fail("calib.points.predict", json!({"path": "/cam", "points": [[0, 0, 0]]}))
                .contains("calib.points.solve")
        );

        let camera = truth();
        let mut ids = Vec::new();
        for part in parts {
            for c in part["corners"].as_array().unwrap() {
                let w = [c[0].as_f64().unwrap(), c[1].as_f64().unwrap(), c[2].as_f64().unwrap()];
                let q = camera.project(w).unwrap();
                let wiggle = (ids.len() as f64 * 1.7).sin() * 0.5;
                let r = f.call(
                    "calib.points.set",
                    json!({"path": "/cam", "world": w, "picture": [q[0] + wiggle, q[1] - wiggle], "group": part["part"]}),
                );
                ids.push(r["id"].as_u64().unwrap());
            }
        }
        assert_eq!(ids.len(), 16);
        assert_eq!(
            f.call("calib.points.list", json!({"path": "/cam"}))["points"].as_array().unwrap().len(),
            16
        );

        // One click off: it is named, and the reply has everything the flow shows.
        let bad = ids[5];
        let c = f.call("calib.points.list", json!({"path": "/cam"}))["points"][5].clone();
        let shifted = json!([c["picture"][0].as_f64().unwrap() + 30.0, c["picture"][1]]);
        f.call("calib.points.set", json!({"path": "/cam", "id": bad, "picture": shifted}));
        let r = f.call("calib.points.solve", json!({"path": "/cam", "geo": "/model", "apply": false}));
        assert_eq!(r["suspects"][0], bad, "{r}");
        assert!(r["warnings"].to_string().contains(&format!("Point {bad}")), "{r}");
        assert_eq!(r["deviation"].as_array().unwrap().len(), 2, "{r}");
        assert!(r["points"][0]["left_out"].is_number(), "{r}");

        // Off, the fit is clean and goes into the node.
        f.call("calib.points.set", json!({"path": "/cam", "id": bad, "off": true}));
        let r = f.call("calib.points.solve", json!({"path": "/cam", "geo": "/model"}));
        assert!(r["rms"].as_f64().unwrap() < 1.0, "{r}");
        assert_eq!(r["points"].as_array().unwrap().len(), 15);
        assert_eq!(r["quality"], "good", "{r}");
        assert!(r["pose"]["distance_spread"].is_number(), "{r}");

        // The prediction lands near the truth, with how far it may be off.
        let w = [0.2, -0.06, -0.2];
        let r = f.call("calib.points.predict", json!({"path": "/cam", "points": [w]}));
        let t = camera.project(w).unwrap();
        let (x, y) = (r["pixels"][0][0].as_f64().unwrap(), r["pixels"][0][1].as_f64().unwrap());
        assert!((x - t[0]).hypot(y - t[1]) < 3.0, "{r}");
        assert!(r["deviation"][0].as_f64().unwrap() > 0.0, "{r}");
        let r = f.call("calib.points.deviation", json!({"path": "/cam", "groups": {"a": [w]}}));
        assert_eq!(r["deviation"][0]["group"], "a", "{r}");

        // Undone, the calibration is gone and the list is as before the solve.
        f.call("history.undo", json!({}));
        assert!(
            f.fail("calib.points.predict", json!({"path": "/cam", "points": [w]}))
                .contains("calib.points.solve")
        );

        f.call("calib.points.remove", json!({"path": "/cam", "id": bad}));
        assert!(f.fail("calib.points.remove", json!({"path": "/cam", "id": bad})).contains("no point"));
        f.call("calib.points.remove", json!({"path": "/cam", "all": true}));
        assert!(f.fail("calib.points.solve", json!({"path": "/cam"})).contains("at least 4"));
        assert!(f.fail("calib.points.set", json!({"path": "/cam", "world": [0, 0, 0]})).contains("needs"));
    }

    #[test]
    fn a_frozen_picture_gives_a_camera_for_the_view_node() {
        let f = Fixture::new("picture");
        f.call("node.create", json!({"path": "/phone", "kind": "view.camera"}));
        let size = [1128, 1575];
        let r = f.call("calib.points.setup", json!({"path": "/phone", "size": size, "kind": "picture"}));
        assert_eq!(r["options"]["fix_principal_point"], true, "{r}");
        let pose =
            penumbra_calib::Pose::look_at([0.3, -0.7, 0.05], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]).unwrap();
        let mut lens = penumbra_calib::Intrinsics::from_throw_ratio(size, 1.0);
        lens.focal = [1165.0, 1165.0];
        let camera = Camera { intrinsics: lens, pose };
        for (i, w) in [
            [-0.15, -0.03, -0.27],
            [0.15, -0.03, -0.27],
            [0.0, -0.03, 0.27],
            [-0.1, -0.03, 0.05],
            [0.1, -0.03, 0.1],
            [-0.15, 0.0, -0.27],
            [0.15, 0.0, -0.2],
            [0.05, -0.015, -0.1],
            [-0.05, -0.01, 0.2],
            [0.12, -0.02, 0.0],
        ]
        .into_iter()
        .enumerate()
        {
            let q = camera.project(w).unwrap();
            let d = (i as f64).sin() * 0.3;
            f.call(
                "calib.points.set",
                json!({"path": "/phone", "world": w, "picture": [q[0] + d, q[1] - d]}),
            );
        }
        let r = f.call("calib.points.solve", json!({"path": "/phone"}));
        let got = r["pose"]["position"].as_array().unwrap();
        let want = camera.position();
        let off = (0..3).map(|k| (got[k].as_f64().unwrap() - want[k]).powi(2)).sum::<f64>().sqrt();
        assert!(off < 0.1, "{off}: {r}");
        // The node now holds it, as a view for renders and lights.
        let q = f.call("calib.points.predict", json!({"path": "/phone", "points": [[0.0, -0.03, 0.27]]}));
        assert!(q["pixels"][0][0].is_number(), "{q}");
        let (text, calibration) = camera_texts(&f.engine, &"/phone".parse().unwrap()).unwrap();
        assert!(!text.is_empty());
        let stored: Camera = serde_json::from_str(&calibration).unwrap();
        assert_eq!(stored.intrinsics.size, size);
    }
}
