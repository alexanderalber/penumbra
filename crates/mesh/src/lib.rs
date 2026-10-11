//! Meshes on the CPU: what flows along a `geo` connection before it reaches the GPU, the readers
//! for OBJ, STL and glTF, and a few primitives.
//!
//! Every file is hostile input. The readers are our own (not `effectcraft-model`, see the plan):
//! they check every count against [`Limits`] *before* allocating for it, use checked
//! arithmetic on offsets, refuse non-finite coordinates, and never panic. Callers still run
//! them inside `catch_unwind` (a job does), as for every foreign-looking parser.
//!
//! Conventions: world units are metres where known, **z up** (the user's choice, as Blender and
//! most CAD tools). The readers return what the file holds; [`Mesh::to_z_up`] turns a y-up mesh
//! (glTF by its spec, OBJ by habit) into z up, and [`Up::of_file`] says what a format usually is. Texture coordinates have their origin at the top left,
//! as every texture in penumbra (OBJ's `vt` is flipped on the way in). Triangles are lists of
//! three indices; the winding is kept as in the file but not relied on (the renderer does not
//! cull by default).

mod gltf;
mod obj;
pub mod prim;
mod stl;
pub mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use penumbra_core::Error;

/// One vertex: position, normal (unit length, or zero where unknown) and texture coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// A run of triangles that share a part and a material: `count` indices from `first`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub first: u32,
    pub count: u32,
    pub part: u32,
    pub material: u32,
}

/// A surface's look as the file describes it: a colour (linear, straight alpha) times an
/// optional texture.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    pub color: [f32; 4],
    /// Index into [`Mesh::images`].
    pub texture: Option<u32>,
}

impl Material {
    pub fn plain(name: &str, color: [f32; 4]) -> Self {
        Self { name: name.to_string(), color, texture: None }
    }
}

/// An image a material refers to, still encoded (PNG, JPEG, ...). Decoding is the GPU side's
/// business, under its own image limits.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A mesh with named parts. Part ids are the indices into [`Mesh::parts`]; they come from the
/// file (OBJ `o`/`g`, glTF nodes, one per STL file) and are what the render node's data pass
/// writes, so a shader or a mask can tell the parts of an object apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub ranges: Vec<Range>,
    pub parts: Vec<String>,
    /// Never empty after [`Builder::finish`]: material 0 is the default (white).
    pub materials: Vec<Material>,
    pub images: Vec<Image>,
}

/// Per-part numbers, e.g. for the `parts` table of `geo.file`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartInfo {
    pub id: u32,
    pub name: String,
    pub triangles: usize,
    pub bounds: Option<Bounds>,
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    pub fn of(points: impl IntoIterator<Item = [f32; 3]>) -> Option<Bounds> {
        let mut b: Option<Bounds> = None;
        for p in points {
            b = Some(match b {
                None => Bounds { min: p, max: p },
                Some(b) => b.with(p),
            });
        }
        b
    }

    pub fn with(self, p: [f32; 3]) -> Bounds {
        Bounds {
            min: [self.min[0].min(p[0]), self.min[1].min(p[1]), self.min[2].min(p[2])],
            max: [self.max[0].max(p[0]), self.max[1].max(p[1]), self.max[2].max(p[2])],
        }
    }

    pub fn union(self, other: Bounds) -> Bounds {
        self.with(other.min).with(other.max)
    }

    pub fn center(&self) -> [f32; 3] {
        let (a, b) = (self.min, self.max);
        [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2])]
    }

    pub fn size(&self) -> [f32; 3] {
        let (a, b) = (self.min, self.max);
        [b[0] - a[0], b[1] - a[1], b[2] - a[2]]
    }

    /// The eight corners.
    pub fn corners(&self) -> [[f32; 3]; 8] {
        let (a, b) = (self.min, self.max);
        [0, 1, 2, 3, 4, 5, 6, 7].map(|k| {
            [
                if k & 1 == 0 { a[0] } else { b[0] },
                if k & 2 == 0 { a[1] } else { b[1] },
                if k & 4 == 0 { a[2] } else { b[2] },
            ]
        })
    }
}

/// Which axis a file treats as up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Up {
    Y,
    Z,
}

impl Up {
    /// What a format usually is: glTF y up (its spec), OBJ y up (most exporters), STL z up (most
    /// CAD and 3D printing tools).
    pub fn of_file(name: &str) -> Up {
        let ext =
            Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext == "stl" { Up::Z } else { Up::Y }
    }
}

impl Mesh {
    /// Turns a mesh whose up is `from` into z up: y up is turned +90 degrees about x, so
    /// `(x, y, z)` becomes `(x, -z, y)` and the front (+z in glTF) faces -y. A rotation, so
    /// windings and normals stay right.
    pub fn to_z_up(&mut self, from: Up) {
        if from == Up::Z {
            return;
        }
        let turn = |[x, y, z]: [f32; 3]| [x, -z, y];
        for v in &mut self.vertices {
            v.position = turn(v.position);
            v.normal = turn(v.normal);
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Replaces the normals by smooth ones: at each corner the average of the faces around
    /// it (weighted by their angles there, so how a face is split does not matter) within one
    /// part, leaving out faces turned away by more than `crease_degrees` from the corner's own
    /// (180: all of them; 30: hard edges stay hard). `rings` above 1 repeats the averaging over
    /// the faces' smoothed normals, reaching that many rings of faces around each corner: for
    /// meshes from voxel or marching-cubes tools, whose staircases of small steps still show
    /// after one ring. For meshes that carry only face normals, such as STL, whose faceted
    /// surfaces otherwise shade grainy. A vertex whose corners come out different is split;
    /// more than `max_vertices` is an error, the mesh left as it was.
    ///
    /// Bounded for hostile input: corners sharing a point beyond [`SMOOTH_GROUP`] faces take
    /// the plain average (no crease test), so the work stays linear per ring.
    pub fn smooth_normals(
        &mut self,
        crease_degrees: f32,
        rings: u32,
        max_vertices: usize,
    ) -> Result<(), Error> {
        let tris = self.triangle_count();
        // Part of each triangle, from the ranges.
        let mut part = vec![0u32; tris];
        for r in &self.ranges {
            let (a, b) = (r.first as usize / 3, (r.first as usize + r.count as usize) / 3);
            for p in part.get_mut(a..b.min(tris)).unwrap_or(&mut []) {
                *p = r.part;
            }
        }
        let pos = |i: u32| self.vertices.get(i as usize).map_or([0.0; 3], |v| v.position);
        // Unit face normals, and per corner the face's angle there.
        let mut unit = Vec::with_capacity(tris);
        let mut angle = Vec::with_capacity(tris * 3);
        for t in self.indices.as_chunks::<3>().0 {
            let [p0, p1, p2] = t.map(pos);
            unit.push(normalize(cross(sub(p1, p0), sub(p2, p0))));
            for (a, b, c) in [(p0, p1, p2), (p1, p2, p0), (p2, p0, p1)] {
                let (u, v) = (normalize(sub(b, a)), normalize(sub(c, a)));
                let w = (u[0] * v[0] + u[1] * v[1] + u[2] * v[2]).clamp(-1.0, 1.0).acos();
                angle.push(if w.is_finite() { w } else { 0.0 });
            }
        }
        // Corners grouped by (part, point).
        let mut ids: std::collections::HashMap<(u32, [u32; 3]), u32> = std::collections::HashMap::new();
        let mut group = Vec::with_capacity(self.indices.len());
        for (c, i) in self.indices.iter().enumerate() {
            let key = (part.get(c / 3).copied().unwrap_or(0), pos(*i).map(|x| (x + 0.0).to_bits()));
            let next = ids.len() as u32;
            group.push(*ids.entry(key).or_insert(next));
        }
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); ids.len()];
        drop(ids);
        // The corners of each group.
        for (c, g) in group.iter().enumerate() {
            if let Some(m) = members.get_mut(*g as usize) {
                m.push(c as u32);
            }
        }
        let cos =
            if crease_degrees >= 180.0 { -2.0 } else { crease_degrees.max(0.0).to_radians().cos() - 1e-4 };
        let add = |n: [f32; 3], a: [f32; 3], w: f32| [n[0] + a[0] * w, n[1] + a[1] * w, n[2] + a[2] * w];
        // One ring: each corner's normal from `face` (a normal per triangle) around it.
        let ring = |face: &[[f32; 3]]| -> Vec<[f32; 3]> {
            let sum = |list: &[u32], own: Option<[f32; 3]>| {
                let mut n = [0.0f32; 3];
                for s in list {
                    let t = *s as usize / 3;
                    let (Some(f), Some(u), Some(w)) = (face.get(t), unit.get(t), angle.get(*s as usize))
                    else {
                        continue;
                    };
                    if own.is_none_or(|o| o[0] * u[0] + o[1] * u[1] + o[2] * u[2] >= cos) {
                        n = add(n, *f, *w);
                    }
                }
                normalize(n)
            };
            // Groups without a crease test, once each.
            let plain: Vec<Option<[f32; 3]>> = members
                .iter()
                .map(|list| (cos < -1.0 || list.len() > SMOOTH_GROUP).then(|| sum(list, None)))
                .collect();
            group
                .iter()
                .enumerate()
                .map(|(c, g)| {
                    let own = unit.get(c / 3).copied().unwrap_or([0.0; 3]);
                    let n = match plain.get(*g as usize).copied().flatten() {
                        Some(n) => n,
                        None => sum(members.get(*g as usize).map_or(&[][..], Vec::as_slice), Some(own)),
                    };
                    if n == [0.0; 3] { own } else { n }
                })
                .collect()
        };
        let mut corners = ring(&unit);
        for _ in 1..rings.clamp(1, MAX_RINGS) {
            // Each face's normal: the average of its corners'.
            let faces: Vec<[f32; 3]> = corners
                .as_chunks::<3>()
                .0
                .iter()
                .zip(&unit)
                .map(|([a, b, c], u)| {
                    let n = normalize([a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]]);
                    if n == [0.0; 3] { *u } else { n }
                })
                .collect();
            corners = ring(&faces);
        }
        let mut vertices = self.vertices.clone();
        let mut indices = self.indices.clone();
        // Whether a vertex has its smooth normal yet.
        let mut set = vec![false; vertices.len()];
        // Copies made by splitting, by (vertex, normal), so corners alike share one.
        let mut copies: std::collections::HashMap<(u32, [u32; 3]), u32> = std::collections::HashMap::new();
        for (n, index) in corners.iter().copied().zip(indices.iter_mut()) {
            let v = *index as usize;
            let count = vertices.len();
            let Some(vertex) = vertices.get_mut(v) else { continue };
            match set.get_mut(v) {
                Some(done) if !*done => {
                    vertex.normal = n;
                    *done = true;
                }
                _ if vertex.normal != n => {
                    if let Some(copy) = copies.get(&(*index, n.map(f32::to_bits))) {
                        *index = *copy;
                        continue;
                    }
                    if count >= max_vertices {
                        return Err(Error::new(format!("smoothing needs more than {max_vertices} vertices")));
                    }
                    let copy = Vertex { normal: n, ..*vertex };
                    copies.insert((*index, n.map(f32::to_bits)), count as u32);
                    *index = count as u32;
                    vertices.push(copy);
                    set.push(true);
                }
                _ => {}
            }
        }
        self.vertices = vertices;
        self.indices = indices;
        Ok(())
    }

    pub fn bounds(&self) -> Option<Bounds> {
        Bounds::of(self.vertices.iter().map(|v| v.position))
    }

    /// Triangles and bounds per part.
    pub fn part_info(&self) -> Vec<PartInfo> {
        let mut info: Vec<PartInfo> = self
            .parts
            .iter()
            .enumerate()
            .map(|(i, name)| PartInfo { id: i as u32, name: name.clone(), triangles: 0, bounds: None })
            .collect();
        for r in &self.ranges {
            let Some(p) = info.get_mut(r.part as usize) else { continue };
            p.triangles += r.count as usize / 3;
            let start = r.first as usize;
            let end = start.saturating_add(r.count as usize);
            for i in self.indices.get(start..end).unwrap_or(&[]) {
                if let Some(v) = self.vertices.get(*i as usize) {
                    p.bounds = Some(
                        p.bounds.map_or(Bounds { min: v.position, max: v.position }, |b| b.with(v.position)),
                    );
                }
            }
        }
        info
    }

    /// Whether the mesh is consistent: indices inside the vertices, ranges inside the indices,
    /// in order and covering them, parts, materials and textures that exist, finite numbers.
    /// The readers produce consistent meshes; this is the last line before the GPU.
    pub fn check(&self) -> Result<(), Error> {
        let n = self.vertices.len();
        if !self.indices.len().is_multiple_of(3) {
            return Err(Error::new("the index count is not a multiple of 3"));
        }
        if self.indices.iter().any(|i| *i as usize >= n) {
            return Err(Error::new("an index points past the vertices"));
        }
        let mut next = 0u64;
        for r in &self.ranges {
            if u64::from(r.first) != next || r.count % 3 != 0 {
                return Err(Error::new("the ranges do not cover the triangles in order"));
            }
            next += u64::from(r.count);
            if r.part as usize >= self.parts.len() || r.material as usize >= self.materials.len() {
                return Err(Error::new("a range names a part or material that does not exist"));
            }
        }
        if next != self.indices.len() as u64 {
            return Err(Error::new("the ranges do not cover the triangles"));
        }
        for m in &self.materials {
            if m.texture.is_some_and(|t| t as usize >= self.images.len()) {
                return Err(Error::new(format!("material `{}` names a missing image", m.name)));
            }
            if !m.color.iter().all(|c| c.is_finite()) {
                return Err(Error::new(format!("material `{}` has a colour that is not finite", m.name)));
            }
        }
        let finite = |v: &Vertex| v.position.iter().chain(&v.normal).chain(&v.uv).all(|c| c.is_finite());
        if !self.vertices.iter().all(finite) {
            return Err(Error::new("a vertex is not finite"));
        }
        Ok(())
    }
}

/// Budgets for reading a file. A file that would exceed one is refused with an error saying
/// which, before the memory for it is allocated.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Largest mesh file, and largest file it refers to (a glTF's `.bin`, an OBJ's `.mtl`).
    pub max_file_bytes: u64,
    /// All referenced files and embedded images together.
    pub max_total_bytes: u64,
    /// 8 Mi vertices of 32 bytes fill one 256 MiB GPU buffer, wgpu's default limit.
    pub max_vertices: usize,
    pub max_triangles: usize,
    pub max_parts: usize,
    /// Runs of one part and material (one draw call each).
    pub max_ranges: usize,
    pub max_materials: usize,
    pub max_images: usize,
    /// glTF: nodes visited while walking the scene (a hostile file can share children to make
    /// the walk exponential) and the deepest nesting.
    pub max_node_visits: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 << 20,
            max_total_bytes: 512 << 20,
            max_vertices: 8 << 20,
            max_triangles: 4 << 20,
            max_parts: 1 << 16,
            max_ranges: 1 << 16,
            max_materials: 4096,
            max_images: 64,
            max_node_visits: 100_000,
            max_depth: 64,
        }
    }
}

/// A mesh read from a file, with what the reader skipped or could not use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub mesh: Mesh,
    pub warnings: Vec<String>,
}

/// Extensions the file picker offers.
pub const EXTENSIONS: &[&str] = &["obj", "stl", "gltf", "glb"];

/// Reads a mesh file. Files it refers to (materials, buffers, textures) are read relative to its
/// directory; absolute paths, drive letters, network paths and URLs are refused.
pub fn load_file(path: &Path, limits: &Limits) -> Result<Loaded, Error> {
    let bytes = read_capped(path, limits.max_file_bytes)?;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let total = std::cell::Cell::new(bytes.len() as u64);
    let sidecar = |reference: &str| -> Result<Vec<u8>, Error> {
        let file = sidecar_path(&dir, reference)?;
        let data = read_capped(&file, limits.max_file_bytes)?;
        let sum = total.get().saturating_add(data.len() as u64);
        if sum > limits.max_total_bytes {
            return Err(Error::new(format!(
                "the files this mesh refers to exceed {} MiB together",
                limits.max_total_bytes >> 20
            )));
        }
        total.set(sum);
        Ok(data)
    };
    parse(&name, &bytes, &sidecar, limits).map_err(|e| e.context(path.display()))
}

/// Reads a mesh from `bytes`; `name` (the file name) picks the format together with the content.
/// `sidecar` reads a file the mesh refers to by its reference as written in the file.
pub fn parse(
    name: &str,
    bytes: &[u8],
    sidecar: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
    limits: &Limits,
) -> Result<Loaded, Error> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(Error::new(format!("larger than {} MiB", limits.max_file_bytes >> 20)));
    }
    let ext =
        Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let stem = if stem.is_empty() { "mesh".to_string() } else { stem };
    let loaded = if bytes.starts_with(b"glTF") {
        gltf::parse(bytes, sidecar, limits)?
    } else {
        match ext.as_str() {
            "gltf" | "glb" => gltf::parse(bytes, sidecar, limits)?,
            "stl" => stl::parse(bytes, &stem, limits)?,
            "obj" => obj::parse(bytes, &stem, sidecar, limits)?,
            _ => {
                let head = bytes.get(..bytes.len().min(512)).unwrap_or(&[]);
                let text = String::from_utf8_lossy(head);
                let trimmed = text.trim_start();
                if trimmed.starts_with('{') {
                    gltf::parse(bytes, sidecar, limits)?
                } else if stl::is_binary(bytes) || trimmed.starts_with("solid") {
                    stl::parse(bytes, &stem, limits)?
                } else {
                    obj::parse(bytes, &stem, sidecar, limits)?
                }
            }
        }
    };
    loaded.mesh.check()?;
    if loaded.mesh.indices.is_empty() {
        return Err(Error::new("no triangles in the file"));
    }
    Ok(loaded)
}

/// Reads at most `max` bytes of `path`, refusing larger files before reading.
fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, Error> {
    let len = std::fs::metadata(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?.len();
    if len > max {
        return Err(Error::new(format!("{}: larger than {} MiB", path.display(), max >> 20)));
    }
    std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Where a file a mesh refers to lives: relative to the mesh's directory. Refused: absolute
/// paths, drive letters, network paths (reading one would make Windows authenticate against a
/// stranger's server) and URLs. `..` is allowed, exporters write `../textures/x.png`.
fn sidecar_path(dir: &Path, reference: &str) -> Result<PathBuf, Error> {
    let r = percent_decode(reference.trim());
    let refuse = || Error::new(format!("`{reference}`: only paths relative to the mesh file are read"));
    let bytes = r.as_bytes();
    let drive = bytes.len() >= 2 && bytes.get(1) == Some(&b':');
    if r.is_empty() || r.starts_with('/') || r.starts_with('\\') || drive || r.contains("://") {
        return Err(refuse());
    }
    let rel = Path::new(&r);
    if rel.is_absolute() || rel.has_root() {
        return Err(refuse());
    }
    Ok(dir.join(rel))
}

/// `%20` and friends, as glTF URIs carry them. Invalid escapes stay as they are.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let (Some(h), Some(l)) =
                (b.get(i + 1).and_then(|x| hex(*x)), b.get(i + 2).and_then(|x| hex(*x)))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// sRGB-encoded 0..1 to linear (OBJ material colours are written as picked on screen).
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Builds a [`Mesh`] while checking every addition against the [`Limits`]. All readers go
/// through it, so no reader can grow a mesh past a budget.
pub struct Builder<'l> {
    limits: &'l Limits,
    mesh: Mesh,
    part_ids: HashMap<String, u32>,
}

impl<'l> Builder<'l> {
    pub fn new(limits: &'l Limits) -> Self {
        let mesh = Mesh { materials: vec![Material::plain("default", [1.0; 4])], ..Mesh::default() };
        Self { limits, mesh, part_ids: HashMap::new() }
    }

    /// The id of the part called `name`, created when new.
    pub fn part(&mut self, name: &str) -> Result<u32, Error> {
        if let Some(id) = self.part_ids.get(name) {
            return Ok(*id);
        }
        if self.mesh.parts.len() >= self.limits.max_parts {
            return Err(Error::new(format!("more than {} parts", self.limits.max_parts)));
        }
        let id = self.mesh.parts.len() as u32;
        self.mesh.parts.push(name.to_string());
        self.part_ids.insert(name.to_string(), id);
        Ok(id)
    }

    /// A part of its own even if the name is taken (glTF nodes may share names).
    pub fn new_part(&mut self, name: &str) -> Result<u32, Error> {
        if self.part_ids.contains_key(name) {
            let mut k = 2;
            loop {
                let candidate = format!("{name}.{k}");
                if !self.part_ids.contains_key(&candidate) {
                    return self.part(&candidate);
                }
                k += 1;
            }
        }
        self.part(name)
    }

    pub fn material(&mut self, m: Material) -> Result<u32, Error> {
        if self.mesh.materials.len() >= self.limits.max_materials {
            return Err(Error::new(format!("more than {} materials", self.limits.max_materials)));
        }
        self.mesh.materials.push(m);
        Ok(self.mesh.materials.len() as u32 - 1)
    }

    pub fn image(&mut self, image: Image) -> Result<u32, Error> {
        if self.mesh.images.len() >= self.limits.max_images {
            return Err(Error::new(format!("more than {} images", self.limits.max_images)));
        }
        self.mesh.images.push(image);
        Ok(self.mesh.images.len() as u32 - 1)
    }

    pub fn vertex_count(&self) -> usize {
        self.mesh.vertices.len()
    }

    /// Whether `more` vertices still fit.
    pub fn reserve_vertices(&mut self, more: usize) -> Result<(), Error> {
        if self.mesh.vertices.len().saturating_add(more) > self.limits.max_vertices {
            return Err(Error::new(format!("more than {} vertices", self.limits.max_vertices)));
        }
        self.mesh.vertices.reserve(more);
        Ok(())
    }

    /// Whether `more` triangles still fit.
    pub fn reserve_triangles(&mut self, more: usize) -> Result<(), Error> {
        if self.mesh.indices.len() / 3 + more > self.limits.max_triangles {
            return Err(Error::new(format!("more than {} triangles", self.limits.max_triangles)));
        }
        self.mesh.indices.reserve(more.saturating_mul(3));
        Ok(())
    }

    pub fn vertex(&mut self, v: Vertex) -> Result<u32, Error> {
        if !v.position.iter().all(|c| c.is_finite()) {
            return Err(Error::new(format!("vertex {} is not finite", self.mesh.vertices.len())));
        }
        if self.mesh.vertices.len() >= self.limits.max_vertices {
            return Err(Error::new(format!("more than {} vertices", self.limits.max_vertices)));
        }
        let clean = |c: f32| if c.is_finite() { c } else { 0.0 };
        self.mesh.vertices.push(Vertex { normal: v.normal.map(clean), uv: v.uv.map(clean), ..v });
        Ok(self.mesh.vertices.len() as u32 - 1)
    }

    /// A triangle of existing vertices, in part `part` with material `material`.
    pub fn triangle(&mut self, t: [u32; 3], part: u32, material: u32) -> Result<(), Error> {
        let n = self.mesh.vertices.len() as u32;
        if t.iter().any(|i| *i >= n) {
            return Err(Error::new("a triangle refers to a vertex that does not exist"));
        }
        if self.mesh.indices.len() / 3 >= self.limits.max_triangles {
            return Err(Error::new(format!("more than {} triangles", self.limits.max_triangles)));
        }
        let at = self.mesh.indices.len() as u32;
        match self.mesh.ranges.last_mut() {
            Some(r) if r.part == part && r.material == material && r.first + r.count == at => r.count += 3,
            _ => {
                if self.mesh.ranges.len() >= self.limits.max_ranges {
                    return Err(Error::new(format!(
                        "more than {} runs of one part and material",
                        self.limits.max_ranges
                    )));
                }
                self.mesh.ranges.push(Range { first: at, count: 3, part, material });
            }
        }
        self.mesh.indices.extend(t);
        Ok(())
    }

    /// Three new vertices with the triangle's own normal (flat shading), for files without
    /// normals.
    pub fn flat_triangle(
        &mut self,
        p: [[f32; 3]; 3],
        uv: [[f32; 2]; 3],
        part: u32,
        material: u32,
    ) -> Result<(), Error> {
        let n = face_normal(p);
        let a = self.vertex(Vertex { position: p[0], normal: n, uv: uv[0] })?;
        let b = self.vertex(Vertex { position: p[1], normal: n, uv: uv[1] })?;
        let c = self.vertex(Vertex { position: p[2], normal: n, uv: uv[2] })?;
        self.triangle([a, b, c], part, material)
    }

    pub fn mesh(&self) -> &Mesh {
        &self.mesh
    }

    /// The mesh; parts without triangles are kept (their ids stay as in the file).
    pub fn finish(self) -> Mesh {
        self.mesh
    }
}

/// Faces around one point beyond which [`Mesh::smooth_normals`] skips the crease test.
pub const SMOOTH_GROUP: usize = 256;
/// Most rings [`Mesh::smooth_normals`] averages over.
pub const MAX_RINGS: u32 = 16;

/// The unit normal of a triangle (counter-clockwise front), zero when it is degenerate.
pub fn face_normal(p: [[f32; 3]; 3]) -> [f32; 3] {
    let u = sub(p[1], p[0]);
    let v = sub(p[2], p[0]);
    normalize(cross(u, v))
}

pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Unit length, or zero for a zero or non-finite vector.
pub fn normalize(a: [f32; 3]) -> [f32; 3] {
    let len = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if len.is_finite() && len > 1e-20 { a.map(|c| c / len) } else { [0.0; 3] }
}

#[cfg(test)]
mod tests;
