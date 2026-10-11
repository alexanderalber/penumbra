//! `geo` nodes: a mesh file, a primitive, a transform, a merge, and a material. The data on a `geo`
//! connection is a [`Geo`]: models uploaded once, placed by matrices, with part ids; plus a small
//! picture of them for the editor, the viewer and `node.snapshot`.
//!
//! Files are hostile input: they are read and uploaded in a job (inside `catch_unwind`), under
//! the budgets of [`penumbra_mesh::Limits`] and the image limits for their textures.

use std::sync::Arc;

use penumbra_calib::{Camera, Intrinsics, Pose};
use penumbra_core::{Curve, Error, ParamSpec, Table, Unit};
use penumbra_gpu::geo::{Item, Mat4, Model, mul};
use penumbra_gpu::render::{Light, Look, Renderer, Surface, View};
use penumbra_gpu::{Geo, Gpu, Mat, Tex};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};
use penumbra_mesh::{Bounds, Limits, Mesh, Up, prim};

/// Choices of `geo.file`'s `up`.
pub const UP: [&str; 3] = ["auto", "y", "z"];

/// Choices of `geo.file`'s `normals`.
pub const NORMALS: [&str; 2] = ["file", "smooth"];

/// Size of the picture a `geo` node shows of itself.
pub const PREVIEW: (u32, u32) = (512, 288);

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "geo.file",
            "A mesh file (OBJ, STL, glTF/GLB) with its parts; reloads when the file changes",
        )
        .param(ParamSpec::file("file", penumbra_mesh::EXTENSIONS).doc("Relative to the project directory"))
        .param(ParamSpec::choice("up", &UP).rarely_used().doc(
            "Which axis is up in the file, turned into penumbra's z up; auto: y for glTF and OBJ, z for STL",
        ))
        .param(ParamSpec::choice("normals", &NORMALS).doc(
            "file: as the file has them (flat where it has none, as STL); smooth: averaged over each corner's faces, for faceted or voxel meshes",
        ))
        .param(
            ParamSpec::int("rings", 1)
                .clamp(1.0, f64::from(penumbra_mesh::MAX_RINGS))
                .rarely_used()
                .doc("smooth: rings of faces averaged around each corner; 3 to 6 for voxel or marching-cubes meshes"),
        )
        .param(
            ParamSpec::float("crease", 180.0)
                .clamp(0.0, 180.0)
                .unit(Unit::Degrees)
                .rarely_used()
                .doc("smooth: edges sharper than this stay hard; 180 smooths everything"),
        )
        .output("out", DataKind::Geo)
        .output("parts", DataKind::Table),
        || Box::new(GeoFile::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.primitive", "A box, a sphere or a plane, centred on the origin")
            .param(ParamSpec::choice("shape", &["box", "sphere", "plane"]))
            .param(
                ParamSpec::vec3("size", [1.0; 3]).range(0.0, 10.0).doc(
                    "Box: x, y, z; sphere: the diameter is x; plane (upright, facing -y): x wide, z high",
                ),
            )
            .param(
                ParamSpec::int("segments", 48)
                    .clamp(3.0, f64::from(prim::MAX_SEGMENTS))
                    .range(3.0, 128.0)
                    .curve(Curve::Log)
                    .rarely_used()
                    .doc("Around the sphere"),
            )
            .output("out", DataKind::Geo),
        || Box::new(Primitive::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.transform", "Scale, rotate, then translate")
            .param(ParamSpec::vec3("translate", [0.0; 3]).range(-10.0, 10.0))
            .param(
                ParamSpec::vec3("rotate", [0.0; 3])
                    .range(-180.0, 180.0)
                    .unit(Unit::Degrees)
                    .doc("About x, then y, then z"),
            )
            .param(ParamSpec::vec3("scale", [1.0; 3]).range(0.0, 4.0))
            .param(
                ParamSpec::float("uniform", 1.0)
                    .range(0.001, 1000.0)
                    .curve(Curve::Log)
                    .doc("Times the scale: 0.001 turns millimetres into metres"),
            )
            .input("in", DataKind::Geo)
            .output("out", DataKind::Geo),
        || Box::new(Transform::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.material", "Puts a material on every model of the input")
            .input("in", DataKind::Geo)
            .input("mat", DataKind::Mat)
            .output("out", DataKind::Geo),
        || Box::new(PutMaterial::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.merge", "Several inputs as one; part ids continue from input to input")
            .input("in0", DataKind::Geo)
            .input("in1", DataKind::Geo)
            .input("in2", DataKind::Geo)
            .input("in3", DataKind::Geo)
            .output("out", DataKind::Geo),
        || Box::new(Merge::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn gpu<'a>(cx: &'a CookContext<'_>) -> Result<&'a Gpu, CookError> {
    cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU: geo nodes cannot cook"))
}

/// A camera looking at `bounds` from the front (-y), right and above, the whole box in view.
pub fn framing(bounds: Option<Bounds>, size: (u32, u32), fov_degrees: f64) -> Option<Camera> {
    let b = bounds.unwrap_or(Bounds { min: [-0.5; 3], max: [0.5; 3] });
    let c = b.center().map(f64::from);
    let s = b.size().map(f64::from);
    let radius = (0.5 * (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt()).max(1e-3);
    let fov = fov_degrees.to_radians().clamp(0.05, 3.0);
    let aspect = f64::from(size.0) / f64::from(size.1.max(1));
    let half = (0.5 * fov).tan().min((0.5 * fov).tan() * aspect);
    let distance = radius / half.atan().sin() * 1.02;
    let dir: [f64; 3] = FRONT_RIGHT_ABOVE;
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let eye = [0, 1, 2]
        .map(|k| c.get(k).copied().unwrap_or(0.0) + dir.get(k).copied().unwrap_or(0.0) / len * distance);
    let intrinsics = perspective(size, fov_degrees, [0.0, 0.0]);
    Some(Camera { intrinsics, pose: Pose::look_at(eye, c, UP_AXIS)? })
}

/// World up: z (metres, as Blender and most CAD tools).
pub const UP_AXIS: [f64; 3] = [0.0, 0.0, 1.0];

/// Where a framing camera looks from, relative to what it frames: in front (-y), to the right
/// and above.
pub const FRONT_RIGHT_ABOVE: [f64; 3] = [0.55, -0.72, 0.42];

/// A lens with vertical field of view `fov_degrees`, square pixels and the principal point
/// moved by `shift` picture sizes (positive: the picture moves right and up, as a projector's
/// lens shift).
pub fn perspective(size: (u32, u32), fov_degrees: f64, shift: [f64; 2]) -> Intrinsics {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let f = 0.5 * h / (0.5 * fov_degrees.to_radians().clamp(1e-3, 3.1)).tan();
    Intrinsics {
        size: [size.0, size.1],
        focal: [f, f],
        principal: [0.5 * w - shift[0] * w, 0.5 * h + shift[1] * h],
        distortion: [0.0; 2],
    }
}

/// The render view of a camera: its clip matrix, depth row and position. `None` for a camera
/// that cannot make one (see `Camera::clip_from_world`).
pub fn view_of(camera: &Camera, near: f64, far: f64) -> Option<View> {
    let clip = camera.clip_from_world(near, far)?;
    let r = camera.pose.rotation_matrix();
    let [r20, r21, r22] = r[2];
    Some(View {
        clip_from_world: clip,
        view_z: [r20, r21, r22, camera.pose.translation[2]],
        eye: camera.position(),
    })
}

/// What a node's picture of itself shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    /// Geometry: every material shaded (constant ones too, shape matters more than colour),
    /// lit a little from everywhere and the rest from the camera, so every side it sees is lit.
    Geometry,
    /// A material on a sphere filling the picture's height: lit from the upper left, constant
    /// ones flat, as they render.
    Material,
}

/// The picture of `geo` for thumbnails, framed.
pub fn preview(gpu: &Gpu, renderer: &mut Renderer, geo: &Geo, kind: Preview) -> Result<Tex, Error> {
    preview_lit(gpu, renderer, geo, kind, &[])
}

/// [`preview`] with `extra` lights besides its own (a pbr material's studio surroundings).
pub fn preview_lit(
    gpu: &Gpu,
    renderer: &mut Renderer,
    geo: &Geo,
    kind: Preview,
    extra: &[Light],
) -> Result<Tex, Error> {
    let bounds = geo.bounds().map(|b| match kind {
        Preview::Geometry => b,
        // A sphere's box is wider than the sphere by the square root of three; a little room
        // for the perspective.
        Preview::Material => {
            let (c, s) = (b.center(), b.size().map(|v| v * 0.58 / 3f32.sqrt()));
            Bounds {
                min: [c[0] - s[0], c[1] - s[1], c[2] - s[2]],
                max: [c[0] + s[0], c[1] + s[1], c[2] + s[2]],
            }
        }
    });
    let camera = framing(bounds, PREVIEW, 35.0).ok_or_else(|| Error::new("cannot frame the geometry"))?;
    let b = geo.bounds().map_or(1.0, |b| {
        let s = b.size();
        f64::from((s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt()).max(1e-3)
    });
    let view = view_of(&camera, b * 0.01, b * 20.0).ok_or_else(|| Error::new("cannot frame the geometry"))?;
    let look = Look {
        surface: Surface::Material,
        shade_all: kind == Preview::Geometry,
        background: [0.0; 4],
        cull: false,
        antialias: true,
        overlay: None,
        shadow_size: 0,
    };
    let lights = match kind {
        Preview::Geometry => [Light::ambient([0.25; 3]), Light::point(view.eye, [0.75; 3])],
        Preview::Material => {
            let mut key = Light::ambient([0.9; 3]);
            key.kind = penumbra_gpu::render::LightKind::Directional;
            // From the left of the camera and above it (the camera stands front right).
            key.direction = [0.45, 0.85, -0.8];
            [Light::ambient([0.15; 3]), key]
        }
    };
    let lights: Vec<Light> = lights.into_iter().chain(extra.iter().cloned()).collect();
    let tex = Tex::new(gpu, PREVIEW.0, PREVIEW.1, "geo preview")?;
    renderer.render(gpu, geo, &lights, &view, &look, None, &tex, None)?;
    Ok(tex)
}

fn with_preview(gpu: &Gpu, renderer: &mut Renderer, mut geo: Geo) -> Geo {
    match preview(gpu, renderer, &geo, Preview::Geometry) {
        Ok(t) => geo.preview = Some(t),
        Err(e) => log::warn!("geo preview: {e}"),
    }
    geo
}

/// The `parts` table: a header, then one row per part.
fn parts_table(mesh: &Mesh) -> Result<Table, Error> {
    let mut rows = vec![
        ["id", "name", "triangles", "min_x", "min_y", "min_z", "max_x", "max_y", "max_z"]
            .map(str::to_string)
            .to_vec(),
    ];
    for p in mesh.part_info() {
        let mut row = vec![p.id.to_string(), p.name.clone(), p.triangles.to_string()];
        if let Some(b) = p.bounds {
            row.extend(b.min.iter().chain(&b.max).map(|v| format!("{v:.4}")));
        }
        rows.push(row);
    }
    Table::new(rows)
}

struct Loaded {
    geo: Geo,
    parts: Table,
    warnings: Vec<String>,
}

#[derive(Default)]
struct GeoFile {
    slot: AsyncSlot<Arc<Loaded>>,
    watch: Option<FileWatch>,
    /// The `up`, `normals`, `crease` and `rings` the last load was started with.
    how: Option<(String, String, (u64, u32))>,
}

impl Operator for GeoFile {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let up_param = cx.text("up")?.to_string();
        let smooth = cx.text("normals")? == "smooth";
        let crease = cx.float("crease")?;
        let rings = u32::try_from(cx.int("rings")?).unwrap_or(1);
        let how = (
            up_param.clone(),
            cx.text("normals")?.to_string(),
            if smooth { (crease.to_bits(), rings) } else { (0, 0) },
        );
        let turned = self.how.as_ref() != Some(&how);
        if moved || turned || self.watch.as_ref().is_some_and(FileWatch::take_changed) {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            self.how = Some(how);
            let gpu = gpu(cx)?.clone();
            let p = path.clone();
            let up = match up_param.as_str() {
                "y" => Up::Y,
                "z" => Up::Z,
                _ => Up::of_file(&p.to_string_lossy()),
            };
            self.slot.start(cx.spawn(move |_job| {
                let mut loaded = penumbra_mesh::load_file(&p, &Limits::default()).map_err(|e| e.0)?;
                loaded.mesh.to_z_up(up);
                if smooth {
                    loaded
                        .mesh
                        .smooth_normals(crease as f32, rings, Limits::default().max_vertices)
                        .map_err(|e| e.0)?;
                }
                let parts = parts_table(&loaded.mesh).map_err(|e| e.0)?;
                let label =
                    p.file_name().map_or_else(|| "mesh".to_string(), |n| n.to_string_lossy().to_string());
                let (model, mut warnings) = Model::upload(&gpu, loaded.mesh, &label).map_err(|e| e.0)?;
                warnings.splice(0..0, loaded.warnings);
                let geo = with_preview(&gpu, &mut Renderer::new(), Geo::single(Arc::new(model)));
                Ok(Arc::new(Loaded { geo, parts, warnings }))
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        let Some(l) = self.slot.latest() else { return Err(CookError::Pending) };
        for w in l.warnings.iter().take(4) {
            cx.warn(w.clone());
        }
        if fresh {
            Ok(Cooked::Outputs(vec![
                Data::new(DataKind::Geo, l.geo.clone()),
                Data::new(DataKind::Table, l.parts.clone()),
            ]))
        } else {
            Ok(Cooked::Unchanged)
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

#[derive(Default)]
struct Primitive {
    renderer: Renderer,
}

impl Operator for Primitive {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let size = vec3(cx, "size")?.map(|v| v.abs() as f32);
        let mesh = match cx.text("shape")? {
            "sphere" => prim::sphere(0.5 * size[0], u32::try_from(cx.int("segments")?).unwrap_or(48))?,
            "plane" => prim::plane([size[0], size[2]])?,
            _ => prim::cuboid(size)?,
        };
        let (model, _) = Model::upload(gpu, mesh, "primitive")?;
        let geo = with_preview(gpu, &mut self.renderer, Geo::single(Arc::new(model)));
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

fn vec3(cx: &CookContext<'_>, name: &str) -> Result<[f64; 3], CookError> {
    match cx.param(name).and_then(|v| v.components()) {
        Some(&[x, y, z]) => Ok([x, y, z]),
        _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
    }
}

/// Scale, then rotate about x, y, z, then translate.
pub fn trs(translate: [f64; 3], rotate_degrees: [f64; 3], scale: [f64; 3]) -> Mat4 {
    let [ax, ay, az] = rotate_degrees.map(f64::to_radians);
    let (sx, cx) = ax.sin_cos();
    let (sy, cy) = ay.sin_cos();
    let (sz, cz) = az.sin_cos();
    let rx = [[1.0, 0.0, 0.0, 0.0], [0.0, cx, -sx, 0.0], [0.0, sx, cx, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let ry = [[cy, 0.0, sy, 0.0], [0.0, 1.0, 0.0, 0.0], [-sy, 0.0, cy, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let rz = [[cz, -sz, 0.0, 0.0], [sz, cz, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let [x, y, z] = scale;
    let s = [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let mut m = mul(&rz, &mul(&ry, &mul(&rx, &s)));
    let [t0, t1, t2] = translate;
    let [r0, r1, r2, _] = &mut m;
    r0[3] = t0;
    r1[3] = t1;
    r2[3] = t2;
    m
}

#[derive(Default)]
struct Transform {
    renderer: Renderer,
}

impl Operator for Transform {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let Some(input) = cx.input_as::<Geo>(0) else { return Err(waiting(cx)) };
        let u = cx.float("uniform")?;
        let scale = vec3(cx, "scale")?.map(|s| s * u);
        let m = trs(vec3(cx, "translate")?, vec3(cx, "rotate")?, scale);
        if !m.iter().flatten().all(|v| v.is_finite()) {
            return Err(CookError::failed("the transform is not finite"));
        }
        let items = input.items.iter().map(|i| Item { world: mul(&m, &i.world), ..i.clone() }).collect();
        let geo = with_preview(gpu, &mut self.renderer, Geo { items, preview: None });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

#[derive(Default)]
struct Merge {
    renderer: Renderer,
}

impl Operator for Merge {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let mut items = Vec::new();
        let mut offset = 0u32;
        let mut any = false;
        for k in 0..cx.def().inputs.len() {
            let Some(g) = cx.input_as::<Geo>(k) else { continue };
            any = true;
            items.extend(g.items.iter().map(|i| Item { part_offset: i.part_offset + offset, ..i.clone() }));
            offset += g.part_count();
        }
        if !any {
            return Err(waiting(cx));
        }
        let geo = with_preview(gpu, &mut self.renderer, Geo { items, preview: None });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

#[derive(Default)]
struct PutMaterial {
    renderer: Renderer,
}

impl Operator for PutMaterial {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let Some(input) = cx.input_as::<Geo>(0) else { return Err(waiting(cx)) };
        let Some(mat) = cx.input_as::<Mat>(1) else {
            return Err(if cx.def().inputs.get(1).is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect a material (a mat node)")
            });
        };
        let items =
            input.items.iter().map(|i| Item { material: Some(mat.material.clone()), ..i.clone() }).collect();
        let geo = with_preview(gpu, &mut self.renderer, Geo { items, preview: None });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

fn waiting(cx: &CookContext<'_>) -> CookError {
    if cx.def().inputs.iter().any(Option::is_some) {
        CookError::Pending
    } else {
        CookError::failed("connect an input")
    }
}
