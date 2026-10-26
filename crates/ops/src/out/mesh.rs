//! `out.mesh`: writes its input geometry to an OBJ, STL or glb file.
//!
//! Every item of the [`Geo`] is written with its matrix baked into the positions and normals,
//! all items in one mesh. Each part of each item stays a part of its own (an OBJ `o`, a glTF
//! node; STL has no parts); a name used twice gets `.2`, `.3`, ... as `geo.file` names glTF
//! nodes that share one. Material colours go into a glb, an item's material from
//! `geo.assign_material` replacing its file's colours; OBJ and STL get none. Textures are not
//! written.
//!
//! Copies from `geo.tex_instance` live in textures on the GPU. With `instances` on `expand` the
//! node reads those textures back and writes one copy per texel, placed as the renderer places
//! it, its parts named `<part>_<i>` for copy `i`. The copies' colours are not written. With
//! `skip` every item is written once at its own matrix.
//!
//! Modes, numbered files and the job that writes work as in `out.table`
//! ([`super::table`]): mode `changes` writes when the geometry differs, mode `manual` only on
//! `mesh_out.write` through [`MeshOuts`]. Whether the geometry differs is told by the models,
//! matrices and materials, not by its vertices. Instanced copies change without the geometry
//! changing, so an expanded input is written again whenever it is cooked again (unless the
//! file comes out the same).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use half::f16;
use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::geo::{Mat4, Model, mul};
use penumbra_gpu::{Geo, PendingRead, ReadPool, ReadState, RowsError, Tex};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator,
};
use penumbra_mesh::{Limits, Mesh, write};
use serde::Serialize;

use super::file::{Dest, FOLDER, Report, Writers, write_bytes};

/// Kind of the mesh writing node.
pub const OUT_MESH: &str = "out.mesh";
/// Largest file the node writes: as large as `geo.file` reads.
pub const MAX_FILE_BYTES: u64 = 256 << 20;
/// A readback of instance textures older than this is given up (a lost device).
const STALE: Duration = Duration::from_secs(10);

const EXTENSIONS: &[&str] = &["obj", "stl", "glb"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let limits = Limits::default();
    types.register(FnNodeType::new(
        NodeDesc::new(OUT_MESH, "Writes its input geometry to an OBJ, STL or glb file")
            .param(ParamSpec::file("file", EXTENSIONS).doc(
                "The file to write, relative to the project; empty writes export/<node name>.obj (.stl, .glb \
                 with that format). The node creates the export folder but no other",
            ))
            .param(ParamSpec::bool("overwrite", true).doc(
                "On: every write replaces the file, for a program that keeps reading it. Off: every write \
                 makes a new file with the next free name (_2, _3, ...); in mode changes that is a new file \
                 per change",
            ))
            .param(
                ParamSpec::choice("format", &["auto", "obj", "stl", "glb"])
                    .doc("The file format. Each is written so geo.file with up auto reads it back upright")
                    .choice_doc("auto", "by extension, obj for any other")
                    .choice_doc("obj", "text, y up, one object per part, no materials")
                    .choice_doc("stl", "binary, z up, one solid without parts or materials")
                    .choice_doc("glb", "binary glTF, y up, one node per part, material colours"),
            )
            .param(
                ParamSpec::choice("mode", &["changes", "manual"])
                    .doc("When the node writes")
                    .choice_doc("changes", "whenever the geometry differs from what was written")
                    .choice_doc("manual", "only on the command mesh_out.write"),
            )
            .param(
                ParamSpec::choice("instances", &["expand", "skip"])
                    .doc(&format!(
                        "What happens to copies from geo.tex_instance. At most {} triangles and {} parts \
                         are written",
                        limits.max_triangles, limits.max_parts
                    ))
                    .choice_doc(
                        "expand",
                        "one copy per texel, read back from the GPU, each its own part. For copies that \
                         move every frame use mode manual",
                    )
                    .choice_doc("skip", "each model once, where it stands without its copies"),
            )
            .input("in", DataKind::Geo)
            .describe_input("in", "The geometry to write"),
        || Box::new(MeshOut::default()) as Box<dyn Operator>,
    ))
}

/// What `mesh_out.status` tells about one node.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    /// The node's path; the command fills it in.
    pub node: String,
    /// The file the node writes to.
    pub file: String,
    pub format: String,
    pub mode: String,
    /// Seconds since the node last wrote the file; `None` before its first write.
    pub written_seconds_ago: Option<f64>,
    /// Size of the mesh last written or found already in the file.
    pub triangles: usize,
    pub parts: usize,
    /// Models written, each copy of an instanced one counted.
    pub copies: usize,
    pub bytes: u64,
    /// A write (or the readback of instances before it) is under way.
    pub writing: bool,
    /// Why the last write failed.
    pub error: Option<String>,
}

impl Report for Status {
    fn set_node(&mut self, node: String) {
        self.node = node;
    }

    fn set_written_seconds_ago(&mut self, seconds: Option<f64>) {
        self.written_seconds_ago = seconds;
    }

    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Write requests from outside the cook (`mesh_out.write`) and the state of every `out.mesh`,
/// by node. Provided by the engine as a cooker resource; cheap to clone.
pub type MeshOuts = Writers<Status>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Obj,
    Stl,
    Glb,
}

impl Format {
    fn of_extension(ext: &str) -> Format {
        match ext.to_ascii_lowercase().as_str() {
            "stl" => Format::Stl,
            "glb" => Format::Glb,
            _ => Format::Obj,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Format::Obj => "obj",
            Format::Stl => "stl",
            Format::Glb => "glb",
        }
    }

    fn encode(self, mesh: &Mesh) -> Vec<u8> {
        match self {
            Format::Obj => write::to_obj(mesh, None).into_bytes(),
            Format::Stl => write::to_stl(mesh),
            Format::Glb => write::to_glb(mesh),
        }
    }
}

/// Where and how the node writes.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    dest: Dest,
    format: Format,
}

fn target(cx: &CookContext<'_>) -> Result<Target, CookError> {
    let file = cx.text("file")?.trim();
    let format = match cx.text("format")? {
        "auto" => None,
        f @ ("obj" | "stl" | "glb") => Some(Format::of_extension(f)),
        other => return Err(CookError::failed(format!("unknown format `{other}`"))),
    };
    let (path, folder) = if file.is_empty() {
        let ext = format.unwrap_or(Format::Obj).name();
        let folder = cx.resolve_path(FOLDER)?;
        (folder.join(format!("{}.{ext}", cx.def().name.as_str())), Some(folder))
    } else {
        (cx.resolve_path(file)?, None)
    };
    let format = format
        .unwrap_or_else(|| Format::of_extension(path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    Ok(Target { dest: Dest { path, folder, overwrite: cx.bool("overwrite")? }, format })
}

/// One item to write.
struct Piece {
    model: Arc<Model>,
    world: Mat4,
    /// Replaces the colour of every material of the model.
    color: Option<[f32; 4]>,
    /// The copies' textures, as indices into the readback.
    instances: Option<InstanceTextures>,
}

#[derive(Clone, Copy, Debug)]
struct InstanceTextures {
    position: usize,
    aim: Option<usize>,
    scale: Option<usize>,
}

/// Everything a write needs besides the instance textures.
struct Plan {
    pieces: Vec<Piece>,
    target: Target,
    force: bool,
    /// The request the write answers, if any.
    ticket: Option<u64>,
}

/// A texture read back: rgba per texel, rows top first.
struct Texels {
    width: u32,
    height: u32,
    data: Vec<[f32; 4]>,
}

impl Texels {
    /// Instance `i`'s texel, as the renderer reads it: `(i % width, i / width)`, `None` past the end.
    fn get(&self, i: u32) -> Option<[f32; 4]> {
        let w = self.width.max(1);
        if i / w >= self.height {
            return None;
        }
        self.data.get(i as usize).copied()
    }
}

/// What a finished job did.
#[derive(Debug)]
struct Wrote {
    /// The target's path, which `last` remembers.
    wanted: PathBuf,
    /// The file written: the target's path, or a numbered name beside it.
    path: PathBuf,
    counts: Counts,
    bytes: u64,
    hash: u64,
    /// False when the file already held the bytes.
    wrote: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counts {
    triangles: usize,
    parts: usize,
    copies: usize,
    /// Copies left out because their place is not a finite number.
    left_out: usize,
}

/// One item for [`Key`]: the model, its matrix, its material colour and how many copies it has,
/// floats as bits.
type ItemKey = (u64, [[u64; 4]; 4], Option<[u32; 4]>, usize);

/// Whether the geometry changed since the last write, told without looking at the vertices.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    items: Vec<ItemKey>,
    expand: bool,
}

fn key(geo: &Geo, expand: bool) -> Key {
    let items = geo
        .items
        .iter()
        .map(|i| {
            (
                i.model.id,
                i.world.map(|r| r.map(f64::to_bits)),
                i.material.as_ref().map(|m| m.color.map(f32::to_bits)),
                i.copies(),
            )
        })
        .collect();
    Key { items, expand }
}

/// What was last handed to a job.
struct Sent {
    key: Key,
    /// The input itself when its instances are expanded: their textures may change while the
    /// key stays. Held so its address is not reused.
    data: Option<Data>,
    target: Target,
}

#[derive(Default)]
struct MeshOut {
    slot: AsyncSlot<Wrote>,
    /// The readback of instance textures a write waits for, and when it started.
    reading: Option<(PendingRead, Instant, Plan)>,
    /// The request the job in flight answers, if any.
    flight: Option<u64>,
    sent: Option<Sent>,
    /// The target path and hash of the file last written or found.
    last: Option<(PathBuf, u64)>,
    /// The target path and the file last written for it.
    file: Option<(PathBuf, PathBuf)>,
    /// The newest request answered.
    answered: u64,
    left_out: usize,
    status: Status,
    written: Option<Instant>,
}

/// The size of what `geo` writes, told before writing it.
#[derive(Clone, Copy, Debug, Default)]
struct Sizes {
    triangles: usize,
    vertices: usize,
    parts: usize,
    /// Runs of one part and material.
    ranges: usize,
    copies: usize,
    /// Copies not written (`instances` on `skip`).
    skipped: usize,
}

fn sizes(geo: &Geo, expand: bool) -> Sizes {
    let mut s = Sizes::default();
    for item in &geo.items {
        let mesh = &item.model.mesh;
        let n = item.copies();
        let written = if expand { n } else { 1 };
        let parts: HashSet<u32> = mesh.ranges.iter().map(|r| r.part).collect();
        let add =
            |total: &mut usize, each: usize| *total = total.saturating_add(each.saturating_mul(written));
        add(&mut s.triangles, mesh.triangle_count());
        add(&mut s.vertices, mesh.vertices.len());
        add(&mut s.parts, parts.len());
        add(&mut s.ranges, mesh.ranges.len());
        add(&mut s.copies, 1);
        if !expand && item.instances.is_some() {
            s.skipped = s.skipped.saturating_add(n);
        }
    }
    s
}

/// An error when `geo` is more than a file may hold: what `geo.file` reads back.
fn check_budget(geo: &Geo, expand: bool) -> Result<(), String> {
    let l = Limits::default();
    let s = sizes(geo, expand);
    let copied = s.copies > geo.items.len();
    let what = if copied { format!(" in {} copies", s.copies) } else { String::new() };
    let over = |n: usize, max: usize, name: &str| {
        (n > max).then(|| format!("{n} {name}{what} are more than a file may hold ({max})"))
    };
    let error = over(s.triangles, l.max_triangles, "triangles")
        .or_else(|| over(s.vertices, l.max_vertices, "vertices"))
        .or_else(|| over(s.parts, l.max_parts, "parts"))
        .or_else(|| over(s.ranges, l.max_ranges, "runs of one part and material"));
    match error {
        Some(e) if copied => Err(format!("{e}; nothing is written. Fewer copies or instances skip fit")),
        Some(e) => Err(format!("{e}; nothing is written")),
        None => Ok(()),
    }
}

/// The mesh of one item, ready to be copied: only its parts with triangles, its material
/// colours replaced by `color`, without images.
fn template(mesh: &Mesh, color: Option<[f32; 4]>) -> Mesh {
    let mut ids: HashMap<u32, u32> = HashMap::new();
    let mut parts = Vec::new();
    let ranges = mesh
        .ranges
        .iter()
        .map(|r| {
            let part = *ids.entry(r.part).or_insert_with(|| {
                parts.push(mesh.parts.get(r.part as usize).cloned().unwrap_or_default());
                parts.len() as u32 - 1
            });
            penumbra_mesh::Range { part, ..*r }
        })
        .collect();
    let materials = mesh
        .materials
        .iter()
        .map(|m| penumbra_mesh::Material {
            name: m.name.clone(),
            color: color.unwrap_or(m.color),
            texture: None,
        })
        .collect();
    Mesh {
        vertices: mesh.vertices.clone(),
        indices: mesh.indices.clone(),
        ranges,
        parts,
        materials,
        images: Vec::new(),
        inner_edges: Vec::new(),
    }
}

/// A turn taking +y to `aim` with +z kept up as far as it can be, as columns right, forward,
/// up; none for a zero or non-finite direction. As the renderer's shader turns copies.
fn aim_turn(aim: [f64; 3]) -> [[f64; 3]; 3] {
    let none = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let len = aim.iter().map(|x| x * x).sum::<f64>().sqrt();
    if !(len > 1e-12 && len < 1e30) {
        return none;
    }
    let f = aim.map(|x| x / len);
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    };
    let mut r = cross(f, [0.0, 0.0, 1.0]);
    if r.iter().map(|x| x * x).sum::<f64>() < 1e-12 {
        r = [1.0, 0.0, 0.0];
    }
    let rl = r.iter().map(|x| x * x).sum::<f64>().sqrt();
    let r = r.map(|x| x / rl);
    [r, f, cross(r, f)]
}

/// Copy `i`'s matrix inside its item: scaled, then turned, then moved, as the renderer does.
fn instance_matrix(i: u32, position: &Texels, aim: Option<&Texels>, scale: Option<&Texels>) -> Mat4 {
    let rgb = |t: [f32; 4]| [f64::from(t[0]), f64::from(t[1]), f64::from(t[2])];
    let p = rgb(position.get(i).unwrap_or([0.0; 4]));
    let turn = aim.map_or_else(|| aim_turn([0.0; 3]), |a| aim_turn(rgb(a.get(i).unwrap_or([0.0; 4]))));
    let s = scale.map_or([1.0; 3], |t| rgb(t.get(i).unwrap_or([1.0; 4])));
    let [r, f, u] = turn;
    let row = |k: usize| {
        let at = |c: [f64; 3]| c.get(k).copied().unwrap_or(0.0);
        [at(r) * s[0], at(f) * s[1], at(u) * s[2], at(p)]
    };
    [row(0), row(1), row(2), [0.0, 0.0, 0.0, 1.0]]
}

/// Every piece as one mesh: each copy's matrix baked in, parts named uniquely.
fn assemble(pieces: &[Piece], textures: &[Texels]) -> Result<(Mesh, Counts), String> {
    let mut out = Mesh::default();
    let mut counts = Counts::default();
    for piece in pieces {
        let t = template(&piece.model.mesh, piece.color);
        let Some(inst) = piece.instances else {
            out.append(&t.transformed(&piece.world));
            counts.copies += 1;
            continue;
        };
        let tex = |k: usize| textures.get(k).ok_or("an instance texture was not read back");
        let position = tex(inst.position)?;
        let aim = inst.aim.map(tex).transpose()?;
        let scale = inst.scale.map(tex).transpose()?;
        let count = position.width.saturating_mul(position.height);
        for i in 0..count {
            let m = mul(&piece.world, &instance_matrix(i, position, aim, scale));
            if !m.iter().flatten().all(|x| x.is_finite()) {
                counts.left_out += 1;
                continue;
            }
            let mut copy = t.transformed(&m);
            for p in &mut copy.parts {
                *p = format!("{p}_{i}");
            }
            out.append(&copy);
            counts.copies += 1;
        }
    }
    dedupe_materials(&mut out);
    unique_parts(&mut out.parts);
    let limits = Limits::default();
    if out.materials.len() > limits.max_materials {
        return Err(format!(
            "{} different materials are more than a file may hold ({})",
            out.materials.len(),
            limits.max_materials
        ));
    }
    if !out.vertices.iter().all(|v| v.position.iter().all(|c| c.is_finite())) {
        return Err("a position is not a finite number".into());
    }
    out.check().map_err(|e| e.to_string())?;
    counts.triangles = out.triangle_count();
    counts.parts = out.parts.len();
    Ok((out, counts))
}

/// Copies share their materials: equal ones become one.
fn dedupe_materials(mesh: &mut Mesh) {
    let mut seen: HashMap<(String, [u32; 4]), u32> = HashMap::new();
    let mut kept = Vec::new();
    let map: Vec<u32> = mesh
        .materials
        .iter()
        .map(|m| {
            *seen.entry((m.name.clone(), m.color.map(f32::to_bits))).or_insert_with(|| {
                kept.push(m.clone());
                kept.len() as u32 - 1
            })
        })
        .collect();
    for r in &mut mesh.ranges {
        r.material = map.get(r.material as usize).copied().unwrap_or(0);
    }
    mesh.materials = kept;
}

/// Names used twice get `.2`, `.3`, ... (as `geo.file` names glTF nodes sharing a name); an
/// empty name becomes `part`.
fn unique_parts(parts: &mut [String]) {
    let mut taken: HashSet<String> = HashSet::with_capacity(parts.len());
    for p in parts.iter_mut() {
        if p.trim().is_empty() {
            *p = "part".into();
        }
        if !taken.contains(p.as_str()) {
            taken.insert(p.clone());
            continue;
        }
        let mut k = 2u32;
        while taken.contains(&format!("{p}.{k}")) {
            k += 1;
        }
        *p = format!("{p}.{k}");
        taken.insert(p.clone());
    }
}

/// The textures of a finished readback, in the order they were read.
fn texels(read: &PendingRead) -> Result<Vec<Texels>, String> {
    (0..read.len())
        .map(|k| {
            let (width, height) = read.size(k).ok_or("nothing was read back")?;
            let mut data = Vec::with_capacity(width as usize * height as usize);
            read.rows(k, |row: &[u8]| -> Result<(), String> {
                for px in row.as_chunks::<8>().0 {
                    let (halves, _) = px.as_chunks::<2>();
                    let mut c = [0f32; 4];
                    for (out, h) in c.iter_mut().zip(halves) {
                        *out = f16::from_le_bytes(*h).to_f32();
                    }
                    data.push(c);
                }
                Ok(())
            })
            .map_err(|e| match e {
                RowsError::Gpu(e) => e.to_string(),
                RowsError::Sink(e) => e,
            })?;
            Ok(Texels { width, height, data })
        })
        .collect()
}

/// Builds the mesh, encodes it and writes it, unless `force` is off and the bytes are the ones
/// last written (`last`) or, with `overwrite` on, the ones the file holds.
fn write(plan: &Plan, textures: &[Texels], last: Option<(PathBuf, u64)>) -> Result<Wrote, String> {
    let target = &plan.target;
    let name = target.dest.path.display();
    let (mesh, counts) = assemble(&plan.pieces, textures).map_err(|e| format!("{name}: {e}"))?;
    if mesh.indices.is_empty() {
        return Err(format!("{name}: no triangles to write"));
    }
    let bytes = target.format.encode(&mesh);
    drop(mesh);
    let len = bytes.len() as u64;
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "{name}: the mesh is {} MiB as {}, a mesh file may have {} MiB",
            len >> 20,
            target.format.name(),
            MAX_FILE_BYTES >> 20
        ));
    }
    let w = write_bytes(&bytes, &target.dest, last.as_ref(), plan.force)?;
    Ok(Wrote {
        wanted: target.dest.path.clone(),
        path: w.path,
        counts,
        bytes: len,
        hash: w.hash,
        wrote: w.wrote,
    })
}

impl MeshOut {
    /// Starts writing `geo`: at once, or after reading its instance textures back.
    fn start(
        &mut self,
        cx: &CookContext<'_>,
        data: &Data,
        target: Target,
        force: bool,
        ticket: Option<u64>,
    ) -> Result<(), CookError> {
        let geo = data.get::<Geo>().ok_or_else(|| CookError::failed("the input is not geometry"))?;
        let expand = cx.text("instances")? == "expand";
        check_budget(geo, expand).map_err(CookError::failed)?;
        let mut textures: Vec<Tex> = Vec::new();
        let mut index = |t: &Tex| -> usize {
            match textures.iter().position(|x| x.same(t)) {
                Some(k) => k,
                None => {
                    textures.push(t.clone());
                    textures.len() - 1
                }
            }
        };
        let pieces = geo
            .items
            .iter()
            .map(|i| Piece {
                model: i.model.clone(),
                world: i.world,
                color: i.material.as_ref().map(|m| m.color),
                instances: i.instances.as_ref().filter(|_| expand).map(|inst| InstanceTextures {
                    position: index(&inst.position),
                    aim: inst.aim.as_ref().map(&mut index),
                    scale: inst.scale.as_ref().map(&mut index),
                }),
            })
            .collect();
        let held = (!textures.is_empty()).then(|| data.clone());
        let sent = Sent { key: key(geo, expand), data: held, target: target.clone() };
        let plan = Plan { pieces, target, force, ticket };
        if textures.is_empty() {
            self.spawn(cx, plan, None);
        } else {
            let gpu = crate::gpu(cx)?;
            let waker = cx.waker();
            let refs: Vec<&Tex> = textures.iter().collect();
            let ready = Some(Arc::new(move || waker.wake()) as Arc<dyn Fn() + Send + Sync>);
            let read = gpu.read_async_notify(&refs, &mut ReadPool::default(), ready)?;
            self.reading = Some((read, Instant::now(), plan));
        }
        self.sent = Some(sent);
        Ok(())
    }

    fn spawn(&mut self, cx: &CookContext<'_>, plan: Plan, read: Option<PendingRead>) {
        let last = self.last.clone();
        self.flight = plan.ticket;
        self.slot.start(cx.spawn(move |_job| {
            let textures = match &read {
                Some(r) => texels(r)?,
                None => Vec::new(),
            };
            drop(read);
            write(&plan, &textures, last)
        }));
    }

    /// Hands a finished readback to a job.
    fn read_done(&mut self, cx: &CookContext<'_>) -> Result<(), CookError> {
        let Some((read, started, _)) = &self.reading else { return Ok(()) };
        let state = match crate::gpu(cx) {
            Ok(gpu) => read.wait(gpu, Duration::ZERO),
            Err(_) => read.state(),
        };
        let failed = match state {
            ReadState::Waiting if started.elapsed() > STALE => "the GPU did not deliver the copies' places",
            ReadState::Waiting => return Ok(()),
            ReadState::Failed => "the GPU could not read the copies' places back",
            ReadState::Ready => "",
        };
        let Some((read, _, plan)) = self.reading.take() else { return Ok(()) };
        if !failed.is_empty() {
            if let Some(t) = plan.ticket {
                self.answered = self.answered.max(t);
            }
            // Tried again at the next change.
            self.sent = None;
            return Err(CookError::failed(failed));
        }
        self.spawn(cx, plan, Some(read));
        Ok(())
    }

    /// Takes a finished job's result.
    fn finish(&mut self) {
        let busy = self.slot.busy();
        let good = self.slot.poll();
        if !busy || self.slot.busy() {
            return;
        }
        if let Some(ticket) = self.flight.take() {
            self.answered = self.answered.max(ticket);
        }
        if good && let Some(w) = self.slot.latest() {
            self.last = Some((w.wanted.clone(), w.hash));
            if w.wrote {
                self.written = Some(Instant::now());
                self.file = Some((w.wanted.clone(), w.path.clone()));
            }
            self.status.triangles = w.counts.triangles;
            self.status.parts = w.counts.parts;
            self.status.copies = w.counts.copies;
            self.status.bytes = w.bytes;
            self.left_out = w.counts.left_out;
        }
    }

    /// Starts a write when one is due.
    fn decide(&mut self, cx: &CookContext<'_>, requested: u64) -> Result<(), CookError> {
        let open = requested > self.answered;
        let target = target(cx);
        let mode = cx.text("mode")?;
        self.status.mode = mode.to_string();
        if let Ok(t) = &target {
            let file = match &self.file {
                Some((wanted, written)) if *wanted == t.dest.path => written,
                _ => &t.dest.path,
            };
            self.status.file = file.display().to_string();
            self.status.format = t.format.name().to_string();
        }
        self.read_done(cx)?;
        if self.slot.busy() || self.reading.is_some() {
            // The job's end recooks the node; a request waits until then.
            return Ok(());
        }
        let data = cx.input(0).filter(|d| d.get::<Geo>().is_some()).cloned();
        let answer = |me: &mut Self, e: CookError| {
            // A request that cannot be served is answered with the error.
            me.answered = me.answered.max(requested);
            e
        };
        let target = match target {
            Ok(t) => t,
            Err(e) => return Err(answer(self, e)),
        };
        if open {
            let Some(data) = data else {
                return Err(answer(self, CookError::failed("nothing connected: nothing to write")));
            };
            return self.start(cx, &data, target, true, Some(requested)).map_err(|e| answer(self, e));
        }
        if mode != "changes" {
            return Ok(());
        }
        let Some(data) = data else { return Ok(()) };
        let Some(geo) = data.get::<Geo>() else { return Ok(()) };
        let expand = cx.text("instances")? == "expand";
        let same = self.sent.as_ref().is_some_and(|s| {
            s.target == target
                && s.key == key(geo, expand)
                && s.data.as_ref().is_none_or(|held| held.same(&data))
        });
        if !same {
            self.start(cx, &data, target, false, None)?;
        }
        Ok(())
    }
}

impl Operator for MeshOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.finish();
        let outs = cx.resource::<MeshOuts>();
        let node = cx.node();
        let requested = outs.map_or(0, |o| o.check_in(node, cx.waker()));
        let result = self.decide(cx, requested);
        if let Some(outs) = outs {
            let mut status = self.status.clone();
            status.writing = self.slot.busy() || self.reading.is_some();
            status.error = match &result {
                Err(CookError::Failed(e)) => Some(e.clone()),
                _ => self.slot.error().map(str::to_string),
            };
            outs.report(node, status, self.written, self.answered);
        }
        result?;
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        match cx.input_as::<Geo>(0) {
            None if cx.input(0).is_none() => cx.warn("nothing connected: nothing is written"),
            Some(geo) if cx.text("instances")? == "skip" => {
                let skipped = sizes(geo, false).skipped;
                if skipped > 0 {
                    cx.warn(format!(
                        "instances is skip: each model is written once, without its {skipped} copies"
                    ));
                }
            }
            _ => {}
        }
        if self.left_out > 0 {
            cx.warn(format!("{} copies left out: their place is not a finite number", self.left_out));
        }
        Ok(Cooked::Outputs(Vec::new()))
    }

    fn job_status(&self) -> Option<JobStatus> {
        if self.reading.is_some() {
            return Some(JobStatus::Running { progress: None });
        }
        Some(self.slot.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_placed_as_the_renderer_places_them() {
        let texels = |px: Vec<[f32; 4]>| Texels { width: px.len() as u32, height: 1, data: px };
        let position = texels(vec![[1.0, 2.0, 3.0, 1.0], [0.0; 4]]);
        // Copy 0 turns its +y to +x and is twice as tall; copy 1 reads past the end of both.
        let aim = texels(vec![[1.0, 0.0, 0.0, 1.0]]);
        let scale = texels(vec![[1.0, 1.0, 2.0, 1.0]]);
        let m = instance_matrix(0, &position, Some(&aim), Some(&scale));
        let at = |m: &Mat4, p: [f64; 3]| penumbra_gpu::geo::transform(m, p);
        // +y goes to +x, +x to -y (right of the aim, z up), +z stays up, scaled by 2.
        assert_eq!(at(&m, [0.0, 1.0, 0.0]), [2.0, 2.0, 3.0]);
        assert_eq!(at(&m, [1.0, 0.0, 0.0]), [1.0, 1.0, 3.0]);
        assert_eq!(at(&m, [0.0, 0.0, 1.0]), [1.0, 2.0, 5.0]);
        let m = instance_matrix(1, &position, Some(&aim), Some(&scale));
        assert_eq!(m, penumbra_gpu::geo::IDENTITY, "past the end: not turned, scale 1");
        // Straight up: the right axis falls back to +x.
        assert_eq!(aim_turn([0.0, 0.0, 2.0]), [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]]);
    }

    #[test]
    fn parts_get_unique_names() {
        let mut parts = vec!["box".to_string(), "box".into(), String::new(), "box.2".into(), "box".into()];
        unique_parts(&mut parts);
        assert_eq!(parts, ["box", "box.2", "part", "box.2.2", "box.3"]);
    }
}
