//! `geo` nodes: a mesh file, a primitive, a transform, a merge, copies placed by a table or by
//! textures, and a material. The data on a `geo` connection is a [`Geo`]: models uploaded once, placed by
//! matrices, with part ids. The picture of them the editor, the viewer and `node.snapshot` show
//! is made by whoever shows it ([`preview`]), not by the nodes on every cook.
//!
//! Files are hostile input: they are read and uploaded in a job (inside `catch_unwind`), under
//! the budgets of [`penumbra_mesh::Limits`] and the image limits for their textures.

use std::sync::Arc;

use penumbra_calib::{Camera, Intrinsics, Pose};
use penumbra_core::{Curve, Error, ParamSpec, Table, Unit};
use penumbra_gpu::geo::{Instances, Item, Mat4, Model, mul};
use penumbra_gpu::render::{Light, Look, MAX_DRAWS, Renderer, Surface, View};
use penumbra_gpu::{Geo, Gpu, Mat, Tex};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};
use penumbra_mesh::{Bounds, Limits, Mesh, Motion, Up, prim};

use crate::{gpu, waiting};

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
            "A 3D model from a file (OBJ, STL, glTF/GLB, FBX) with its parts; updates when the file changes. An animated FBX plays: parts move with their models, skinned characters bend with their bones",
        )
        .param(
            ParamSpec::file("file", penumbra_mesh::EXTENSIONS)
                .doc("The mesh file, relative to the project directory; saving it again reloads the node"),
        )
        .param(
            ParamSpec::choice("up", &UP)
                .more()
                .doc("Which axis is up in the file, turned into penumbra's z up")
                .choice_doc(
                    "auto",
                    "y for glTF and OBJ, z for STL; FBX files say which way is up, and that is used",
                )
                .choice_doc("y", "the file's y is up (most modelling tools, glTF)")
                .choice_doc("z", "the file's z is up already, as STL from CAD usually is"),
        )
        .param(
            ParamSpec::choice("normals", &NORMALS)
                .doc("Which way each corner faces, for the shading")
                .choice_doc("file", "as the file has them (flat where it has none, as STL)")
                .choice_doc("smooth", "averaged over each corner's faces, for faceted or voxel meshes"),
        )
        .param(ParamSpec::int("rings", 1).clamp(1.0, f64::from(penumbra_mesh::MAX_RINGS)).more().doc(
            "smooth: rings of faces averaged around each corner; 3 to 6 for voxel or marching-cubes meshes",
        ))
        .param(
            ParamSpec::float("crease", 180.0)
                .clamp(0.0, 180.0)
                .unit(Unit::Degrees)
                .more()
                .doc("smooth: edges sharper than this stay hard; 180 smooths everything"),
        )
        .param(
            ParamSpec::int("take", 0)
                .clamp(0.0, 1e6)
                .range(0.0, 10.0)
                .doc("Which animation of an FBX file plays: 0 is the first. The `takes` output lists them"),
        )
        .param(ParamSpec::bool("play", true).doc("Off holds the animation at `time`"))
        .param(
            ParamSpec::float("speed", 1.0)
                .clamp(0.0, 16.0)
                .range(0.0, 4.0)
                .doc("Playback rate of the animation: 2 plays twice as fast"),
        )
        .param(ParamSpec::bool("loop", true).doc("Off stops the animation on its last frame"))
        .param(
            ParamSpec::float("time", 0.0)
                .clamp(-1e7, 1e7)
                .range(0.0, 60.0)
                .unit(Unit::Seconds)
                .doc("Seconds into the animation, added to the seconds played. With `play` off, bind a channel here to drive the animation"),
        )
        .output("out", DataKind::Geo)
        .output("parts", DataKind::Table)
        .output("takes", DataKind::Table)
        .describe_output("out", "The mesh, posed at the animation's current time")
        .describe_output("parts", "One row per part: id, name, triangles, bounds (min_x .. max_z)")
        .describe_output("takes", "One row per animation in the file: take, name, start, stop, seconds. Only a header for a still file"),
        || Box::new(GeoFile::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.primitive", "A simple shape: a box, a sphere or a plane, centred on the origin")
            .param(
                ParamSpec::choice("shape", &["box", "sphere", "plane"])
                    .doc("Which shape, centred on the origin; size sets its measures")
                    .choice_doc("box", "a cuboid of size x by y by z")
                    .choice_doc("sphere", "a ball as wide as size x")
                    .choice_doc("plane", "an upright rectangle facing -y, size x wide and size z high"),
            )
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
                    .more()
                    .doc("Steps around the sphere (half as many from pole to pole); more is rounder and costs more"),
            )
            .param(
                ParamSpec::int("cells", 1)
                    .clamp(1.0, f64::from(prim::MAX_CELLS))
                    .range(1.0, 128.0)
                    .curve(Curve::Log)
                    .doc("Plane: quads along each side; with mat.wireframe a grid of lines (a floor to fly over)"),
            )
            .output("out", DataKind::Geo),
        || Box::new(Primitive::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.transform", "Moves, turns and scales geometry")
            .param(
                ParamSpec::vec3("translate", [0.0; 3])
                    .range(-10.0, 10.0)
                    .doc("Move along x, y, z (z up), in scene units (metres)"),
            )
            .param(
                ParamSpec::vec3("rotate", [0.0; 3])
                    .range(-180.0, 180.0)
                    .unit(Unit::Degrees)
                    .doc("Turn about the origin: about x, then y, then z"),
            )
            .param(
                ParamSpec::vec3("scale", [1.0; 3])
                    .range(0.0, 4.0)
                    .doc("Size along x, y, z around the origin: 1 keeps the size, negative mirrors"),
            )
            .param(
                ParamSpec::float("uniform", 1.0)
                    .range(0.001, 1000.0)
                    .curve(Curve::Log)
                    .doc("Times the scale: 0.001 turns millimetres into metres"),
            )
            .param(order_param("object", "the origin", "world"))
            .input("in", DataKind::Geo)
            .output("out", DataKind::Geo),
        || Box::new(Transform) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "geo.uv_project",
            "Sets how a texture lies on geometry: thrown on along an axis, wrapped around it or onto each side. Replaces the texture coordinates of the file",
        )
        .param(
            ParamSpec::choice("shape", &UV_SHAPES)
                .doc("How the texture reaches the surface")
                .choice_doc("planar", "straight along `axis`, like a slide projector: the far side shows it mirrored")
                .choice_doc("cylinder", "wrapped once around `axis`, the texture's top towards +axis. Large flat triangles show jagged edges in the texture: the angle is only known at their corners")
                .choice_doc("sphere", "wrapped around `axis` and pinched together at both poles. Large flat triangles show jagged edges too")
                .choice_doc("box", "onto each of six sides, whichever a triangle faces most; `axis` is unused"),
        )
        .param(
            ParamSpec::choice("axis", &UV_AXES)
                .doc("planar: the direction the texture travels. cylinder and sphere: the axis it wraps around")
                .choice_doc("auto", "planar: y, the view from the front. cylinder and sphere: z, upright")
                .choice_doc("x", "planar: seen from +x")
                .choice_doc("y", "planar: seen from the front (-y)")
                .choice_doc("z", "planar: seen from above"),
        )
        .param(
            ParamSpec::choice("fit", &UV_FITS)
                .doc("How big the texture lies on the surface")
                .choice_doc("geometry", "the texture spans the box around the input, stretched to it")
                .choice_doc("units", "one texture per scene unit, its middle at the origin, its proportions kept"),
        )
        .param(
            ParamSpec::vec2("repeat", [1.0; 2])
                .range(-8.0, 8.0)
                .doc("How many times the texture fits across and down, about its middle; negative mirrors"),
        )
        .param(
            ParamSpec::float("rotate", 0.0)
                .range(-180.0, 180.0)
                .unit(Unit::Degrees)
                .doc("Turns the texture on the surface, counter-clockwise. 90 makes a left-to-right ramp run bottom to top"),
        )
        .param(
            ParamSpec::vec2("offset", [0.0; 2])
                .range(-1.0, 1.0)
                .doc("Moves the texture on the surface, in texture sizes: x to the right, y up"),
        )
        .input("in", DataKind::Geo)
        .output("out", DataKind::Geo)
        .describe_input("in", "Where the geometry stands decides the mapping: a geo.transform before this node changes it, one after it moves the texture along")
        .describe_output("out", "The same geometry with new texture coordinates; part ids and materials stay"),
        || Box::new(UvProject::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.assign_material", "Gives geometry a material: how it looks when rendered")
            .input("in", DataKind::Geo)
            .input("mat", DataKind::Mat)
            .output("out", DataKind::Geo),
        || Box::new(PutMaterial) as Box<dyn Operator>,
    ))?;
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                "geo.merge",
                "Combines several pieces of geometry into one, to render them together",
            )
            .input_run("in", DataKind::Geo, 2, "The pieces of geometry")
            .input_steering("show")
            .describe_input("show", crate::show::SHOW_DOC)
            .output("out", DataKind::Geo)
            .describe_output("out", "All inputs as one; part ids continue from input to input"),
            || Box::new(Merge::default()) as Box<dyn Operator>,
        )
        .choose_inputs(crate::show::choose),
    )?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "geo.instance",
            "Many copies of a model, each placed by a row of a table: a grid, a ring, a crowd of objects",
        )
        .input("in", DataKind::Geo)
        .input("table", DataKind::Table)
        .describe_input("table", "One row per copy. The first row names the columns: tx ty tz to move, rx ry rz to turn (degrees), sx sy sz to scale")
        .output("out", DataKind::Geo)
        .describe_output("out", "The copies; part ids continue from copy to copy"),
        || Box::new(Instance) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "geo.tex_instance",
            "Thousands of copies of a model, placed, scaled, turned and coloured by pictures, all on the GPU: particles, swarms",
        )
        .param(
            ParamSpec::float("extent", 10.0)
                .clamp(0.0, 1e6)
                .range(0.1, 1000.0)
                .curve(Curve::Log)
                .more()
                .doc("Half the side of a box around the origin that holds every copy. Framing and shadow maps use this box, since the positions stay on the GPU"),
        )
        .input("in", DataKind::Geo)
        .input("position", DataKind::Tex)
        .input("aim", DataKind::Tex)
        .input("scale", DataKind::Tex)
        .input("color", DataKind::Tex)
        .describe_input("position", "One texel per copy: copy i reads texel (i % width, i / width). Its rgb moves the copy (x, y, z in metres)")
        .describe_input("aim", "Its rgb turns the copy's +y towards that direction, keeping +z up")
        .describe_input("scale", "Its rgb scales the copy per axis")
        .describe_input("color", "Its rgb multiplies the copy's colour")
        .output("out", DataKind::Geo),
        || Box::new(TexInstance) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("geo.null", "Passes its input on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Geo)
            .output("out", DataKind::Geo),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
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
        parallel: false,
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
        ao: None,
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

/// The `takes` table: a header, then one row per take.
fn takes_table(motion: Option<&Motion>) -> Result<Table, Error> {
    let mut rows = vec![["take", "name", "start", "stop", "seconds"].map(str::to_string).to_vec()];
    for (k, t) in motion.map_or(&[][..], |m| m.takes.as_slice()).iter().enumerate() {
        rows.push(vec![
            k.to_string(),
            t.name.clone(),
            format!("{:.4}", t.start),
            format!("{:.4}", t.stop),
            format!("{:.4}", t.duration()),
        ]);
    }
    Table::new(rows)
}

struct Loaded {
    geo: Geo,
    parts: Table,
    takes: Table,
    /// The animation, for a file whose models move.
    motion: Option<Motion>,
    warnings: Vec<String>,
}

/// A pause in cooking longer than this is not played through: the animation goes on where it
/// was, as `tex.video_file` does.
const GAP_SECONDS: f64 = 0.5;

#[derive(Default)]
struct GeoFile {
    slot: AsyncSlot<Arc<Loaded>>,
    watch: Option<FileWatch>,
    /// The `up`, `normals`, `crease` and `rings` the last load was started with.
    how: Option<(String, String, (u64, u32))>,
    /// The loaded file moves and `play` is on: the node follows the show clock.
    playing: bool,
    /// Seconds played so far, times `speed`.
    played: f64,
    /// Show time of the last cook that played.
    last_time: Option<f64>,
    /// The take and the position (bits) last posed, for the loaded file.
    shown: Option<(usize, u64)>,
    /// The vertices posed last, kept to reuse their memory.
    scratch: Vec<penumbra_mesh::Vertex>,
    /// The vertex buffer of the model posed last, which only the output holds.
    posed_bytes: u64,
}

impl GeoFile {
    /// Seconds into `take` for this cook: `time` plus the seconds played, wrapped or held at
    /// the ends.
    fn position(
        &mut self,
        cx: &CookContext<'_>,
        take: &penumbra_mesh::motion::Take,
    ) -> Result<f64, CookError> {
        if self.playing {
            let now = cx.time().seconds();
            let dt = match self.last_time {
                Some(last) if (0.0..=GAP_SECONDS).contains(&(now - last)) => now - last,
                Some(_) => f64::from(cx.rate().den()) / f64::from(cx.rate().num().max(1)),
                None => 0.0,
            };
            self.last_time = Some(now);
            self.played += dt * cx.float("speed")?.clamp(0.0, 16.0);
        } else {
            self.last_time = None;
        }
        let p = cx.float("time")? + self.played;
        let d = take.duration();
        Ok(if !p.is_finite() || d <= 0.0 {
            0.0
        } else if cx.bool("loop")? {
            p.rem_euclid(d)
        } else {
            p.clamp(0.0, d)
        })
    }
}

impl Operator for GeoFile {
    fn gpu_bytes(&self) -> u64 {
        self.slot.latest().map_or(0, |l| l.geo.items.iter().map(|i| i.model.bytes()).sum::<u64>())
            + self.posed_bytes
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        // Before anything is remembered: a cook before the GPU arrives must not count as the
        // load started, or the file never loads once it is there.
        let gpu = gpu(cx)?.clone();
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
            let p = path.clone();
            let up = match up_param.as_str() {
                "y" => Up::Y,
                "z" => Up::Z,
                _ => Up::of_file(&p.to_string_lossy()),
            };
            let gpu = gpu.clone();
            self.slot.start(cx.spawn(move |_job| {
                let mut loaded = penumbra_mesh::load_file(&p, &Limits::default()).map_err(|e| e.0)?;
                loaded.mesh.to_z_up(up);
                if let Some(m) = loaded.motion.as_mut() {
                    m.to_z_up(up);
                }
                if smooth {
                    let split = loaded
                        .mesh
                        .smooth_normals(crease as f32, rings, Limits::default().max_vertices)
                        .map_err(|e| e.0)?;
                    if let Some(m) = loaded.motion.as_mut() {
                        m.split(&split);
                    }
                }
                let parts = parts_table(&loaded.mesh).map_err(|e| e.0)?;
                let takes = takes_table(loaded.motion.as_ref()).map_err(|e| e.0)?;
                let motion = loaded.motion.take();
                let label =
                    p.file_name().map_or_else(|| "mesh".to_string(), |n| n.to_string_lossy().to_string());
                let (model, mut warnings) = Model::upload(&gpu, loaded.mesh, &label).map_err(|e| e.0)?;
                warnings.splice(0..0, loaded.warnings);
                let geo = Geo::single(Arc::new(model));
                Ok(Arc::new(Loaded { geo, parts, takes, motion, warnings }))
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        let Some(l) = self.slot.latest().cloned() else { return Err(CookError::Pending) };
        for w in l.warnings.iter().take(4) {
            cx.warn(w.clone());
        }
        if fresh {
            self.shown = None;
        }
        let take = usize::try_from(cx.int("take")?).unwrap_or(0);
        let moving = l.motion.as_ref().filter(|m| m.moves(take));
        if let Some(m) = l.motion.as_ref()
            && take >= m.takes.len()
        {
            cx.warn(format!("the file has {} takes: `take` counts from 0", m.takes.len()));
        }
        self.playing = moving.is_some() && cx.bool("play")?;
        let geo = match (moving, l.motion.as_ref().and_then(|m| m.takes.get(take))) {
            (Some(m), Some(t)) => {
                let at = self.position(cx, t)?;
                if !fresh && self.shown == Some((take, at.to_bits())) {
                    return Ok(Cooked::Unchanged);
                }
                self.shown = Some((take, at.to_bits()));
                let Some(base) = l.geo.items.first() else {
                    return Err(CookError::failed("the mesh is empty"));
                };
                let mut vertices = std::mem::take(&mut self.scratch);
                m.deform(&base.model.mesh.vertices, take, at, &mut vertices);
                let model = base.model.with_vertices(&gpu, vertices)?;
                self.posed_bytes = model.vertex_bytes();
                Geo::single(Arc::new(model))
            }
            _ if fresh || self.shown.take().is_some() => {
                self.posed_bytes = 0;
                l.geo.clone()
            }
            _ => return Ok(Cooked::Unchanged),
        };
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Geo, geo),
            Data::new(DataKind::Table, l.parts.clone()),
            Data::new(DataKind::Table, l.takes.clone()),
        ]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.playing)
    }
}

/// Keeps the model it made last, which its output holds anyway, to count it.
#[derive(Default)]
struct Primitive(Option<Arc<Model>>);

impl Operator for Primitive {
    fn gpu_bytes(&self) -> u64 {
        self.0.as_ref().map_or(0, |m| m.bytes())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let size = vec3(cx, "size")?.map(|v| v.abs() as f32);
        let mesh = match cx.text("shape")? {
            "sphere" => prim::sphere(0.5 * size[0], u32::try_from(cx.int("segments")?).unwrap_or(48))?,
            "plane" => prim::grid([size[0], size[2]], u32::try_from(cx.int("cells")?).unwrap_or(1))?,
            _ => prim::cuboid(size)?,
        };
        let (model, _) = Model::upload(gpu, mesh, "primitive")?;
        let model = Arc::new(model);
        self.0 = Some(model.clone());
        let geo = Geo::single(model);
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

fn vec3(cx: &CookContext<'_>, name: &str) -> Result<[f64; 3], CookError> {
    match cx.param(name).and_then(|v| v.components()) {
        Some(&[x, y, z]) => Ok([x, y, z]),
        _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
    }
}

/// The orders the `order` param of `tex.transform` and `geo.transform` offers: the steps
/// scale, rotate and translate by their first letter, in the order they act.
pub const ORDERS: [&str; 6] = ["srt", "str", "rst", "rts", "tsr", "trs"];

/// The `order` param of a transform. `thing` is what it moves, `around` the point it turns
/// and scales about, `axes` the axes a plain move follows.
pub(crate) fn order_param(thing: &str, around: &str, axes: &str) -> ParamSpec {
    ParamSpec::choice("order", &ORDERS)
        .more()
        .doc("The order of scale, rotate and translate. Each step also acts on what the steps before it did")
        .choice_doc("srt", &format!("scale, rotate, translate: the move follows the {axes} axes, whatever the turn and size"))
        .choice_doc("str", &format!("scale, translate, rotate: the move turns with rotate, so the {thing} circles {around}"))
        .choice_doc("rst", &format!("rotate, scale, translate: a non-uniform scale stretches along the {axes} axes and skews a turned {thing}"))
        .choice_doc("rts", &format!("rotate, translate, scale: the move grows with scale, and a non-uniform scale skews a turned {thing}"))
        .choice_doc("tsr", &format!("translate, scale, rotate: the move grows with scale and turns with rotate, circling {around}"))
        .choice_doc("trs", &format!("translate, rotate, scale: the move turns and grows, and a non-uniform scale skews a turned {thing}"))
}

/// Scale, then rotate about x, y, z, then translate.
pub fn trs(translate: [f64; 3], rotate_degrees: [f64; 3], scale: [f64; 3]) -> Mat4 {
    ordered("srt", translate, rotate_degrees, scale)
}

/// The steps in `order` (one of [`ORDERS`]; anything else is `srt`) as one matrix.
pub fn ordered(order: &str, translate: [f64; 3], rotate_degrees: [f64; 3], scale: [f64; 3]) -> Mat4 {
    let order = if ORDERS.contains(&order) { order } else { "srt" };
    let [ax, ay, az] = rotate_degrees.map(f64::to_radians);
    let (sx, cx) = ax.sin_cos();
    let (sy, cy) = ay.sin_cos();
    let (sz, cz) = az.sin_cos();
    let rx = [[1.0, 0.0, 0.0, 0.0], [0.0, cx, -sx, 0.0], [0.0, sx, cx, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let ry = [[cy, 0.0, sy, 0.0], [0.0, 1.0, 0.0, 0.0], [-sy, 0.0, cy, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let rz = [[cz, -sz, 0.0, 0.0], [sz, cz, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let r = mul(&rz, &mul(&ry, &rx));
    let [x, y, z] = scale;
    let s = [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let [t0, t1, t2] = translate;
    let t = [[1.0, 0.0, 0.0, t0], [0.0, 1.0, 0.0, t1], [0.0, 0.0, 1.0, t2], [0.0, 0.0, 0.0, 1.0]];
    let mut m = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    for step in order.chars() {
        let next = match step {
            's' => &s,
            'r' => &r,
            _ => &t,
        };
        m = mul(next, &m);
    }
    m
}

#[derive(Default)]
struct Transform;

impl Operator for Transform {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        gpu(cx)?;
        let Some(input) = cx.input_as::<Geo>(0) else { return Err(waiting(cx)) };
        let u = cx.float("uniform")?;
        let scale = vec3(cx, "scale")?.map(|s| s * u);
        let m = ordered(cx.text("order")?, vec3(cx, "translate")?, vec3(cx, "rotate")?, scale);
        if !m.iter().flatten().all(|v| v.is_finite()) {
            return Err(CookError::failed("the transform is not finite"));
        }
        let items = input.items.iter().map(|i| Item { world: mul(&m, &i.world), ..i.clone() }).collect();
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

#[derive(Default)]
struct Merge {
    /// The part count each input had when it last arrived. An input that is hidden or not
    /// there yet keeps its place in the numbering, so the part ids after it do not shift.
    counts: Vec<u32>,
}

impl Operator for Merge {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        gpu(cx)?;
        let mut items = Vec::new();
        let mut offset = 0u32;
        let mut any = false;
        let inputs = cx.def().inputs.len();
        self.counts.resize(inputs, 0);
        for (k, count) in self.counts.iter_mut().enumerate() {
            let Some(g) = cx.input_as::<Geo>(k) else {
                // An input wired to nothing gives up its place.
                if !cx.def().inputs.get(k).is_some_and(Option::is_some) {
                    *count = 0;
                }
                offset = offset.saturating_add(*count);
                continue;
            };
            any = true;
            items.extend(
                g.items
                    .iter()
                    .map(|i| Item { part_offset: i.part_offset.saturating_add(offset), ..i.clone() }),
            );
            *count = g.part_count();
            offset = offset.saturating_add(*count);
        }
        if !any && !crate::show::all_hidden(cx) {
            return Err(waiting(cx));
        }
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

/// Most copies `geo.instance` makes; the renderer's draw limit is shared with the rest.
pub const MAX_INSTANCES: usize = 10_000;

/// The columns `geo.instance` reads, by header name: translate, rotate (degrees, about x, then
/// y, then z) and scale, as `geo.transform` applies them.
pub const INSTANCE_COLUMNS: [&str; 9] = ["tx", "ty", "tz", "rx", "ry", "rz", "sx", "sy", "sz"];

/// Placements from `table`: its first row names the columns (any of [`INSTANCE_COLUMNS`], in
/// any order, others ignored), every further row is one copy. A missing column or an empty
/// cell is 0 (1 for scale). Returns the matrices and a warning, if any.
pub fn placements(table: &Table) -> (Vec<Mat4>, Option<String>) {
    let Some(header) = table.rows().first() else { return (Vec::new(), None) };
    let index: Vec<Option<usize>> =
        INSTANCE_COLUMNS.iter().map(|name| header.iter().position(|h| h.trim() == *name)).collect();
    if index.iter().all(Option::is_none) {
        return (
            Vec::new(),
            Some(format!("the first row names the columns: {}", INSTANCE_COLUMNS.join(", "))),
        );
    }
    let mut warning = None;
    let mut out = Vec::new();
    for (r, row) in table.rows().iter().enumerate().skip(1) {
        if out.len() >= MAX_INSTANCES {
            warning = Some(format!("more than {MAX_INSTANCES} rows: the rest are left out"));
            break;
        }
        let mut v = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut bad = false;
        for (slot, col) in v.iter_mut().zip(&index) {
            let Some(cell) = col.and_then(|c| row.get(c)).map(|c| c.trim()).filter(|c| !c.is_empty()) else {
                continue;
            };
            match cell.parse::<f64>() {
                Ok(x) if x.is_finite() => *slot = x,
                _ => bad = true,
            }
        }
        if bad {
            warning.get_or_insert_with(|| format!("row {r} holds a cell that is not a number: left out"));
            continue;
        }
        out.push(trs([v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]));
    }
    (out, warning)
}

#[derive(Default)]
struct Instance;

impl Operator for Instance {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        gpu(cx)?;
        let (Some(input), Some(table)) = (cx.input_as::<Geo>(0), cx.input_as::<Table>(1)) else {
            let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
            return Err(if wired(0) && wired(1) {
                CookError::Pending
            } else {
                CookError::failed("connect a geometry and a table of placements")
            });
        };
        let (mut places, warning) = placements(table);
        let mut warnings: Vec<String> = warning.into_iter().collect();
        // Every copy of every item is a draw, and the renderer draws at most MAX_DRAWS. Copies
        // of copies multiply: two chained instances of 10 000 rows would make 10^8 items.
        let fit = MAX_DRAWS / input.items.len().max(1);
        if fit == 0 {
            return Err(CookError::failed(format!(
                "the geometry holds {} items; copies of it would be more than the {MAX_DRAWS} the renderer draws",
                input.items.len()
            )));
        }
        if places.len() > fit {
            warnings.push(format!(
                "{} copies of {} items would be more than the {MAX_DRAWS} the renderer draws: the first {fit} \
                 are kept",
                places.len(),
                input.items.len()
            ));
            places.truncate(fit);
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let parts = input.part_count();
        let mut items = Vec::with_capacity(places.len() * input.items.len());
        for (k, m) in places.iter().enumerate() {
            let offset = u32::try_from(k).unwrap_or(u32::MAX).saturating_mul(parts);
            items.extend(input.items.iter().map(|i| Item {
                world: mul(m, &i.world),
                part_offset: i.part_offset.saturating_add(offset),
                ..i.clone()
            }));
        }
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

#[derive(Default)]
struct TexInstance;

impl Operator for TexInstance {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        gpu(cx)?;
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let (Some(input), Some(position)) = (cx.input_as::<Geo>(0), cx.input_as::<Tex>(1)) else {
            return Err(if wired(0) && wired(1) {
                CookError::Pending
            } else {
                CookError::failed("connect a geometry and a texture of positions")
            });
        };
        // Wired but not cooked yet: wait rather than draw a frame without them.
        let optional = |k: usize| -> Result<Option<Tex>, CookError> {
            match cx.input_as::<Tex>(k) {
                Some(t) => Ok(Some(t.clone())),
                None if wired(k) => Err(CookError::Pending),
                None => Ok(None),
            }
        };
        let instances = Arc::new(Instances {
            position: position.clone(),
            aim: optional(2)?,
            scale: optional(3)?,
            color: optional(4)?,
            extent: cx.float("extent")? as f32,
        });
        if input.items.iter().any(|i| i.instances.is_some()) {
            cx.warn("the input is instanced already: its instances are replaced");
        }
        let items =
            input.items.iter().map(|i| Item { instances: Some(instances.clone()), ..i.clone() }).collect();
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

/// Choices of `geo.uv_project`'s `shape`, `axis` and `fit`.
pub const UV_SHAPES: [&str; 4] = ["planar", "cylinder", "sphere", "box"];
pub const UV_AXES: [&str; 4] = ["auto", "x", "y", "z"];
pub const UV_FITS: [&str; 2] = ["geometry", "units"];

/// Most pieces `geo.uv_project` takes: each piece is a model of its own afterwards, so a large
/// `geo.instance` before the node would upload thousands of them.
pub const MAX_UV_ITEMS: usize = 256;

/// Keeps the models it made last, which its output holds anyway: to count them, and to use
/// them again for a piece whose model, placement and projection are the same as last time.
/// A cook for another reason (a merge upstream, a piece that moved beside it) then uploads
/// nothing for the pieces that stayed.
#[derive(Default)]
struct UvProject(Vec<Projected>);

/// A piece projected: what it was made from and the model with its uvs.
struct Projected {
    source: Arc<Model>,
    world: Mat4,
    projection: penumbra_mesh::uv_project::Projection,
    made: Arc<Model>,
}

impl Operator for UvProject {
    fn gpu_bytes(&self) -> u64 {
        self.0.iter().map(|p| p.made.triangle_bytes()).sum()
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        use penumbra_mesh::uv_project::{Axis, Projection, Shape, placed_bounds, project};
        let gpu = gpu(cx)?;
        let Some(input) = cx.input_as::<Geo>(0) else { return Err(waiting(cx)) };
        if input.items.len() > MAX_UV_ITEMS {
            return Err(CookError::failed(format!(
                "{} pieces of geometry, at most {MAX_UV_ITEMS} work here: put this node before geo.instance",
                input.items.len()
            )));
        }
        let shape = match cx.text("shape")? {
            "cylinder" => Shape::Cylinder,
            "sphere" => Shape::Sphere,
            "box" => Shape::Cube,
            _ => Shape::Planar,
        };
        let axis = match (cx.text("axis")?, shape) {
            ("x", _) => Axis::X,
            ("y", _) => Axis::Y,
            ("z", _) => Axis::Z,
            (_, Shape::Planar) => Axis::Y,
            _ => Axis::Z,
        };
        let fit = if cx.text("fit")? == "units" {
            None
        } else {
            input
                .items
                .iter()
                .filter_map(|i| placed_bounds(&i.model.mesh, &i.world))
                .reduce(|a, b| a.union(b))
        };
        let projection = Projection {
            shape,
            axis,
            fit,
            repeat: vec2(cx, "repeat")?,
            rotate_degrees: cx.float("rotate")?,
            offset: vec2(cx, "offset")?,
        };
        let max = Limits::default().max_vertices;
        let mut models = Vec::with_capacity(input.items.len());
        let mut items = Vec::with_capacity(input.items.len());
        for item in &input.items {
            let same = |p: &&Projected| {
                Arc::ptr_eq(&p.source, &item.model) && p.world == item.world && p.projection == projection
            };
            let model = match self.0.iter().find(same) {
                Some(p) => p.made.clone(),
                None => {
                    let p = project(&item.model.mesh, &item.world, &projection, max)?;
                    Arc::new(item.model.with_triangles(gpu, p.vertices, p.indices)?)
                }
            };
            models.push(Projected {
                source: item.model.clone(),
                world: item.world,
                projection,
                made: model.clone(),
            });
            items.push(Item { model, ..item.clone() });
        }
        self.0 = models;
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

fn vec2(cx: &CookContext<'_>, name: &str) -> Result<[f64; 2], CookError> {
    match cx.param(name).and_then(|v| v.components()) {
        Some(&[x, y]) => Ok([x, y]),
        _ => Err(CookError::failed(format!("`{name}` must be two numbers"))),
    }
}

#[derive(Default)]
struct PutMaterial;

impl Operator for PutMaterial {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        gpu(cx)?;
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
        let geo = Geo { items, preview: None };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, geo)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
        let row = |r: &[f64; 4]| r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3];
        [row(&m[0]), row(&m[1]), row(&m[2])]
    }

    #[test]
    fn the_order_decides_what_the_later_steps_do_to_the_move() {
        // Moved 1 along x, turned 90 degrees about z, scaled by 2: where the origin lands.
        let want = [
            ("srt", [1.0, 0.0, 0.0]),
            ("str", [0.0, 1.0, 0.0]),
            ("rst", [1.0, 0.0, 0.0]),
            ("rts", [2.0, 0.0, 0.0]),
            ("tsr", [0.0, 2.0, 0.0]),
            ("trs", [0.0, 2.0, 0.0]),
            ("nonsense", [1.0, 0.0, 0.0]),
        ];
        for (order, at) in want {
            let m = ordered(order, [1.0, 0.0, 0.0], [0.0, 0.0, 90.0], [2.0; 3]);
            let got = apply(&m, [0.0; 3]);
            assert!(got.iter().zip(at).all(|(g, w)| (g - w).abs() < 1e-9), "{order}: {got:?}");
        }
    }

    #[test]
    fn a_non_uniform_scale_after_a_turn_stretches_along_the_world_axes() {
        // Turned 90 degrees about z, then stretched along x: the point on x ends up on y,
        // unstretched; scaled first, it is stretched before it turns.
        let turned_first = ordered("rst", [0.0; 3], [0.0, 0.0, 90.0], [2.0, 1.0, 1.0]);
        let scaled_first = ordered("srt", [0.0; 3], [0.0, 0.0, 90.0], [2.0, 1.0, 1.0]);
        let a = apply(&turned_first, [1.0, 0.0, 0.0]);
        let b = apply(&scaled_first, [1.0, 0.0, 0.0]);
        assert!((a[1] - 1.0).abs() < 1e-9 && a[0].abs() < 1e-9, "{a:?}");
        assert!((b[1] - 2.0).abs() < 1e-9 && b[0].abs() < 1e-9, "{b:?}");
    }
}
