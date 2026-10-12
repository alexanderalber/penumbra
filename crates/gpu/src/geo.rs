//! Meshes on the GPU: what flows along a `geo` connection.
//!
//! A [`Model`] is a mesh uploaded once (vertex and index buffers, its material textures) with
//! the CPU copy kept beside it (bounds, parts, picking in the calibration mode later). A [`Geo`]
//! is a list of models, each placed by a matrix and with its part ids moved by an offset: a
//! transform changes matrices and a merge concatenates lists, so neither copies or re-uploads
//! geometry.
//!
//! A [`Material`] can be put on every model of a [`Geo`] by `geo.material`; without one, a model
//! shows its file's materials as they are (unlit, as [`Shading::Constant`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use penumbra_core::Error;
use penumbra_mesh::{Bounds, Mesh};

use crate::{Gpu, Tex};

/// Bytes of one vertex on the GPU: position, normal, uv as `f32`.
pub const VERTEX_BYTES: u64 = 32;
/// Bytes of one corner of a wireframe's triangle: a vertex, then its barycentric coordinates and
/// which of the triangle's edges lie inside a flat polygon.
pub const WIRE_BYTES: u64 = 48;
/// Faces whose normals differ by less than this cosine (about one degree) count as one flat
/// polygon for a wireframe's `polygons` edges.
const FLAT: f32 = 0.99985;
/// Most texture memory one model's material images may take once decoded (half floats).
pub const MAX_TEXTURE_BYTES: u64 = 1 << 30;

/// A row-major 4x4 matrix for column vectors.
pub type Mat4 = [[f64; 4]; 4];

pub const IDENTITY: Mat4 =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// An uploaded mesh.
pub struct Model {
    pub mesh: Mesh,
    /// Unique per upload, for caches keyed by geometry.
    pub id: u64,
    pub(crate) vertices: wgpu::Buffer,
    pub(crate) indices: wgpu::Buffer,
    /// One per [`Mesh::images`]; `None` where the image could not be decoded.
    pub(crate) textures: Vec<Option<Tex>>,
    /// One per [`Mesh::ranges`]: the middle of its triangles' box, for drawing transparent
    /// surfaces back to front.
    pub(crate) centers: Vec<[f32; 3]>,
    /// The mesh's box, measured once: a render asks for it on every cook, and walking millions
    /// of vertices each time cost a frame's budget several times over.
    pub(crate) bounds: Option<Bounds>,
    /// Every triangle's corners on their own, for wireframes: made on the first wireframe draw,
    /// since it takes three times the vertices.
    wire: Mutex<Option<wgpu::Buffer>>,
}

impl Model {
    /// Uploads `mesh` (checked first) and decodes its material images. Images that fail to
    /// decode or exceed the budget are left out with a message in the returned warnings. Safe
    /// to call off the cook thread.
    pub fn upload(gpu: &Gpu, mesh: Mesh, label: &str) -> Result<(Model, Vec<String>), Error> {
        mesh.check()?;
        if mesh.indices.is_empty() {
            return Err(Error::new("the mesh has no triangles"));
        }
        let vbytes = mesh.vertices.len() as u64 * VERTEX_BYTES;
        let max = gpu.device().limits().max_buffer_size;
        if vbytes > max {
            return Err(Error::new(format!(
                "{} vertices exceed the GPU's largest buffer",
                mesh.vertices.len()
            )));
        }
        let mut v = Vec::with_capacity(vbytes as usize);
        for x in &mesh.vertices {
            for f in x.position.iter().chain(&x.normal).chain(&x.uv) {
                v.extend_from_slice(&f.to_le_bytes());
            }
        }
        let i: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let buffer = |bytes: &[u8], usage: wgpu::BufferUsages, what: &str| -> Result<wgpu::Buffer, Error> {
            let (b, error) = gpu.scoped(|| {
                let b = gpu.device().create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: bytes.len() as u64,
                    usage: usage | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.queue().write_buffer(&b, 0, bytes);
                b
            });
            match error {
                Some(e) => Err(Error::new(format!("cannot upload the {what}: {e}"))),
                None => Ok(b),
            }
        };
        let vertices = buffer(&v, wgpu::BufferUsages::VERTEX, "vertices")?;
        let indices = buffer(&i, wgpu::BufferUsages::INDEX, "indices")?;
        let mut warnings = Vec::new();
        let mut textures = Vec::with_capacity(mesh.images.len());
        let mut spent = 0u64;
        let used: Vec<u32> = mesh.materials.iter().filter_map(|m| m.texture).collect();
        for (k, image) in mesh.images.iter().enumerate() {
            if !used.contains(&(k as u32)) {
                textures.push(None);
                continue;
            }
            let decoded = crate::image::decode(&image.bytes, gpu.max_texture_size().min(8192));
            let tex = match decoded {
                Ok(d) => {
                    let bytes = u64::from(d.width) * u64::from(d.height) * 8;
                    if spent + bytes > MAX_TEXTURE_BYTES {
                        warnings.push(format!(
                            "texture `{}` left out: the model's textures exceed {} MiB",
                            image.name,
                            MAX_TEXTURE_BYTES >> 20
                        ));
                        None
                    } else {
                        spent += bytes;
                        match gpu.upload(&d, &image.name) {
                            Ok(t) => Some(t),
                            Err(e) => {
                                warnings.push(format!("texture `{}`: {e}", image.name));
                                None
                            }
                        }
                    }
                }
                Err(e) => {
                    warnings.push(format!("texture `{}`: {e}", image.name));
                    None
                }
            };
            textures.push(tex);
        }
        let centers = mesh
            .ranges
            .iter()
            .map(|r| {
                let corners = mesh
                    .indices
                    .get(r.first as usize..(r.first as usize).saturating_add(r.count as usize))
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|i| mesh.vertices.get(*i as usize).map(|v| v.position));
                Bounds::of(corners).map_or([0.0; 3], |b| b.center())
            })
            .collect();
        let bounds = mesh.bounds();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let wire = Mutex::new(None);
        Ok((Model { mesh, id, vertices, indices, textures, centers, bounds, wire }, warnings))
    }

    /// The vertices a wireframe draws: three per triangle in index order (so a run of indices
    /// draws the same corners unindexed), each with its barycentric coordinates and, in the
    /// fourth float, a bit per edge that lies inside a flat polygon (bit k: the edge opposite
    /// corner k). Made once, on first use.
    pub(crate) fn wire(&self, gpu: &Gpu) -> Result<wgpu::Buffer, Error> {
        let mut slot = self.wire.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(b) = slot.as_ref() {
            return Ok(b.clone());
        }
        let bytes = self.mesh.indices.len() as u64 * WIRE_BYTES;
        if bytes > gpu.device().limits().max_buffer_size {
            return Err(Error::new(format!(
                "{} triangles are too many for a wireframe",
                self.mesh.triangle_count()
            )));
        }
        let inside = inner_edges(&self.mesh);
        let mut data = Vec::with_capacity(bytes as usize);
        for (t, corners) in self.mesh.indices.as_chunks::<3>().0.iter().enumerate() {
            let mask = f32::from(inside.get(t).copied().unwrap_or(0));
            for (k, i) in corners.iter().enumerate() {
                let v = self.mesh.vertices.get(*i as usize).copied().unwrap_or_default();
                let mut bary = [0.0f32; 3];
                if let Some(b) = bary.get_mut(k) {
                    *b = 1.0;
                }
                for f in v.position.iter().chain(&v.normal).chain(&v.uv).chain(&bary).chain([&mask]) {
                    data.extend_from_slice(&f.to_le_bytes());
                }
            }
        }
        let (buffer, error) = gpu.scoped(|| {
            let b = gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("wireframe"),
                size: data.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue().write_buffer(&b, 0, &data);
            b
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot upload the wireframe: {e}")));
        }
        *slot = Some(buffer.clone());
        Ok(buffer)
    }

    /// GPU memory of the buffers and textures.
    pub fn bytes(&self) -> u64 {
        self.vertices.size()
            + self.indices.size()
            + self.textures.iter().flatten().map(Tex::bytes).sum::<u64>()
            + self
                .wire
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map_or(0, wgpu::Buffer::size)
    }
}

/// Per triangle, a bit per edge (bit k: the edge opposite corner k) shared with exactly one
/// other triangle that lies in the same plane: the diagonal of a quad, the fan inside an
/// n-gon. Edges are matched by their corners' positions, so vertices split at a seam still
/// meet.
pub(crate) fn inner_edges(mesh: &Mesh) -> Vec<u8> {
    let position = |i: u32| mesh.vertices.get(i as usize).map(|v| v.position);
    let triangles = mesh.indices.as_chunks::<3>().0;
    let normals: Vec<Option<[f32; 3]>> = triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(position);
            let (a, b, c) = (a?, b?, c?);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            (len > 0.0 && len.is_finite()).then(|| n.map(|x| x / len))
        })
        .collect();
    // Each edge's triangles and which of their edges it is.
    type Key = [u32; 3];
    let mut edges: HashMap<(Key, Key), Vec<(usize, u8)>> = HashMap::new();
    for (t, corners) in triangles.iter().enumerate() {
        for k in 0..3u8 {
            let a = corners.get(usize::from((k + 1) % 3)).and_then(|i| position(*i));
            let b = corners.get(usize::from((k + 2) % 3)).and_then(|i| position(*i));
            let (Some(a), Some(b)) = (a, b) else { continue };
            let (a, b) = (a.map(f32::to_bits), b.map(f32::to_bits));
            edges.entry(if a < b { (a, b) } else { (b, a) }).or_default().push((t, k));
        }
    }
    let mut mask = vec![0u8; triangles.len()];
    for shared in edges.values() {
        let &[(s, j), (t, k)] = shared.as_slice() else { continue };
        let (Some(Some(a)), Some(Some(b))) = (normals.get(s), normals.get(t)) else { continue };
        // Either winding: a flat polygon's triangles may face opposite ways in a sloppy file.
        if (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).abs() >= FLAT {
            if let Some(m) = mask.get_mut(s) {
                *m |= 1 << j;
            }
            if let Some(m) = mask.get_mut(t) {
                *m |= 1 << k;
            }
        }
    }
    mask
}

/// One model placed in the world.
#[derive(Clone)]
pub struct Item {
    pub model: Arc<Model>,
    /// Model to world.
    pub world: Mat4,
    /// Added to the model's part ids, so merged inputs keep distinct ids.
    pub part_offset: u32,
    /// Replaces the shading of the file's materials; `None`: they show as they are.
    pub material: Option<Arc<Material>>,
}

/// How a material answers light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shading {
    /// Its colour as it is, with or without lights.
    Constant,
    /// Lambert diffuse plus a Blinn-Phong highlight per light; black without lights.
    Phong,
    /// Metallic and rough as in glTF: Lambert diffuse and a GGX highlight (Cook-Torrance); black
    /// without lights.
    Pbr,
    /// Nothing, but what lies behind it is hidden: a real object standing in front of a
    /// projection, or a cut-out. Casts shadows. The pictures geo nodes show of themselves draw it
    /// as any surface.
    Occluder,
}

/// A material drawn as the edges of its triangles, over its faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wire {
    /// The lines' width in pixels of the render.
    pub width: f32,
    /// Leave out edges inside flat polygons (a quad's diagonal), as a modelling tool shows them.
    pub polygons: bool,
    /// What the faces between the lines show.
    pub faces: Faces,
    /// Linear, straight alpha: the faces' colour ([`Faces::Filled`]).
    pub fill: [f32; 4],
}

/// What a wireframe's faces show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Faces {
    /// Nothing: the lines behind show through.
    Open,
    /// The fill colour; opaque, it hides the lines behind.
    Filled,
    /// Nothing, but they hide what is behind them: only the lines in front show.
    Hidden,
}

/// A material from a `mat` node, put on geometry by `geo.material`.
#[derive(Clone)]
pub struct Material {
    pub shading: Shading,
    /// Linear, straight alpha; replaces the file's colour.
    pub color: [f32; 4],
    /// Linear; the highlight's colour (phong).
    pub specular: [f32; 3],
    /// The highlight's exponent: higher is smaller and sharper (phong).
    pub shininess: f32,
    /// Linear; light the surface gives off by itself (phong, pbr).
    pub emit: [f32; 3],
    /// 0 a dielectric (plastic, paint, glass), 1 a metal, whose colour tints its reflections
    /// (pbr).
    pub metallic: f32,
    /// 0 a mirror, 1 matte (pbr).
    pub roughness: f32,
    /// Replaces the file's textures where given.
    pub texture: Option<Tex>,
    /// Only the triangles' edges show; `None`: the whole surface.
    pub wire: Option<Wire>,
}

impl Material {
    /// White, unlit: what a model shows without a material.
    pub fn constant() -> Self {
        Self {
            shading: Shading::Constant,
            color: [1.0; 4],
            specular: [0.0; 3],
            shininess: 1.0,
            emit: [0.0; 3],
            metallic: 0.0,
            roughness: 0.5,
            texture: None,
            wire: None,
        }
    }
}

/// The payload of `mat` data: a material, and a picture of it on a sphere for the editor's
/// thumbnail and the viewer.
#[derive(Clone)]
pub struct Mat {
    pub material: Arc<Material>,
    pub preview: Option<Tex>,
}

/// The payload of `geo` data: models placed in the world, and a picture of them for the
/// editor's thumbnail, the viewer and `node.snapshot`.
#[derive(Clone, Default)]
pub struct Geo {
    pub items: Vec<Item>,
    pub preview: Option<Tex>,
}

impl Geo {
    pub fn single(model: Arc<Model>) -> Self {
        Self { items: vec![Item { model, world: IDENTITY, part_offset: 0, material: None }], preview: None }
    }

    /// Number of part ids: one past the highest.
    pub fn part_count(&self) -> u32 {
        self.items.iter().map(|i| i.part_offset + i.model.mesh.parts.len() as u32).max().unwrap_or(0)
    }

    /// Name of every part id (later items win where ids overlap, which merges never make).
    pub fn part_names(&self) -> Vec<String> {
        let mut names = vec![String::new(); self.part_count() as usize];
        for item in &self.items {
            for (k, n) in item.model.mesh.parts.iter().enumerate() {
                if let Some(slot) = names.get_mut(item.part_offset as usize + k) {
                    slot.clone_from(n);
                }
            }
        }
        names
    }

    pub fn triangle_count(&self) -> usize {
        self.items.iter().map(|i| i.model.mesh.triangle_count()).sum()
    }

    pub fn vertex_count(&self) -> usize {
        self.items.iter().map(|i| i.model.mesh.vertices.len()).sum()
    }

    /// The world bounds: each model's box moved by its matrix.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut out: Option<Bounds> = None;
        for item in &self.items {
            let Some(b) = item.model.bounds else { continue };
            for c in b.corners() {
                let p = transform(&item.world, c.map(f64::from)).map(|x| x as f32);
                out = Some(out.map_or(Bounds { min: p, max: p }, |o| o.with(p)));
            }
        }
        out
    }
}

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let bt = transpose(b);
    a.map(|row| bt.map(|col| row.iter().zip(&col).map(|(x, y)| x * y).sum()))
}

pub fn transpose(m: &Mat4) -> Mat4 {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m0, n, o, p]] = *m;
    [[a, e, i, m0], [b, f, j, n], [c, g, k, o], [d, h, l, p]]
}

pub fn transform(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
    let v = [p[0], p[1], p[2], 1.0];
    let [r0, r1, r2, _] = m;
    let dot = |r: &[f64; 4]| r.iter().zip(&v).map(|(a, b)| a * b).sum();
    [dot(r0), dot(r1), dot(r2)]
}

/// The matrix for normals: the inverse transpose of the upper 3x3, up to scale (its
/// cofactors, with the determinant's sign so mirrored models keep outward normals), scaled so
/// its largest entry is 1. Unscaled, the cofactors of a 0.001 scale (millimetres to metres)
/// are 1e-6, and the normals' squared length fell below the shader's "no normal" test in
/// scattered pixels: grain all over the model.
pub fn normal_matrix(m: &Mat4) -> [[f64; 3]; 3] {
    let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let s = if det < 0.0 { -1.0 } else { 1.0 };
    let n = [
        [e * i - f * h, f * g - d * i, d * h - e * g],
        [c * h - b * i, a * i - c * g, b * g - a * h],
        [b * f - c * e, c * d - a * f, a * e - b * d],
    ];
    let largest = n.iter().flatten().fold(0.0f64, |m, x| m.max(x.abs()));
    let k = if largest > 0.0 && largest.is_finite() { s / largest } else { s };
    n.map(|row| row.map(|x| x * k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(x: f64, y: f64, z: f64) -> Mat4 {
        [[x, 0.0, 0.0, 0.0], [0.0, y, 0.0, 0.0], [0.0, 0.0, z, 0.0], [0.0, 0.0, 0.0, 1.0]]
    }

    #[test]
    fn normal_matrix_is_not_shrunk_by_a_small_scale() {
        // Millimetres to metres: the cofactors alone are 1e-6.
        let n = normal_matrix(&scale(0.001, 0.001, 0.001));
        assert_eq!(n, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        // Uneven: normals lean towards the squashed axis.
        let n = normal_matrix(&scale(0.001, 0.002, 0.004));
        assert!((n[0][0] - 1.0).abs() < 1e-12 && (n[2][2] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn inner_edges_are_the_diagonals_of_flat_quads() {
        // A unit quad as two triangles, and a third one folded up along the quad's right edge.
        let at = |p: [f32; 3]| penumbra_mesh::Vertex { position: p, ..Default::default() };
        let mesh = Mesh {
            vertices: vec![
                at([0.0, 0.0, 0.0]),
                at([1.0, 0.0, 0.0]),
                at([1.0, 1.0, 0.0]),
                at([0.0, 1.0, 0.0]),
                at([1.0, 0.0, 1.0]),
            ],
            indices: vec![0, 1, 2, 0, 2, 3, 1, 4, 2],
            ..Default::default()
        };
        // The diagonal 0-2 is opposite the first triangle's second corner and the second
        // triangle's third; the fold 1-2 stays.
        assert_eq!(inner_edges(&mesh), vec![0b010, 0b100, 0]);
    }

    #[test]
    fn normal_matrix_keeps_normals_outward_when_mirrored() {
        let n = normal_matrix(&scale(-0.001, 0.001, 0.001));
        // The cofactor flips y and z; the determinant's sign flips all back: x mirrors.
        assert_eq!(n, [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }
}
