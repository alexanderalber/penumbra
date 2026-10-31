//! FBX through `penumbra-fbx`: binary files of version 7000 and later, little endian. Read:
//! every model with a mesh, one part each named after the model, with all transforms baked in
//! (pivots, pre- and post-rotation, geometric transforms, inherit types). Polygons are split
//! into triangles. Normals and the first UV set are read, materials as their diffuse colour
//! and diffuse texture (embedded in the file, or a file beside it). Units become metres, and
//! the file's axes become penumbra's: z up, front towards -y. Not read: animation, skins,
//! blend shapes, cameras, lights; ASCII files are refused with an error saying so.

use std::collections::HashMap;
use std::path::Path;

use penumbra_fbx::transform::mul;
use penumbra_fbx::{Axes, Axis, Geometry, Scene};

use crate::{Builder, Error, Image, Limits, Loaded, Material, Vertex, determinant, normal_matrix, normalize};

type Mat = [[f64; 4]; 4];

/// penumbra's axes as FBX names them: x right, z up, and the front of a model facing -y.
const PENUMBRA_AXES: Axes = Axes { right: Axis::PosX, up: Axis::PosZ, front: Axis::NegY };

/// The reader's budgets from the mesh budgets.
fn fbx_limits(limits: &Limits) -> penumbra_fbx::Limits {
    penumbra_fbx::Limits {
        max_file_bytes: limits.max_file_bytes,
        max_array_bytes: limits.max_total_bytes,
        max_depth: limits.max_depth,
        max_vertices: limits.max_vertices,
        max_polygon_vertices: limits.max_triangles.saturating_mul(3),
        ..penumbra_fbx::Limits::default()
    }
}

pub fn parse(
    bytes: &[u8],
    sidecar: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
    limits: &Limits,
) -> Result<Loaded, Error> {
    let fl = fbx_limits(limits);
    let scene = Scene::parse(bytes, &fl).map_err(|e| Error::new(e.0))?;
    let mut warnings = scene.warnings.clone();
    let axes = scene.axes.unwrap_or_else(|| {
        warnings.push("the file does not say which way is up; read as y up".into());
        Axes::Y_UP
    });
    let unit =
        if scene.unit_meters.is_finite() && scene.unit_meters > 0.0 { scene.unit_meters } else { 0.01 };
    let mut root = axes.conversion_to(&PENUMBRA_AXES);
    for row in root.iter_mut().take(3) {
        for x in row.iter_mut().take(3) {
            *x *= unit;
        }
    }
    let mut r = Reader {
        scene: &scene,
        sidecar,
        b: Builder::new(limits),
        materials: HashMap::new(),
        images: HashMap::new(),
        warnings,
    };
    let mut geometries: HashMap<i64, Geometry> = HashMap::new();
    let (mut degenerate, mut out_of_range) = (0usize, 0usize);
    for inst in scene.mesh_instances() {
        let g = match geometries.entry(inst.geometry_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let g = scene.geometry(inst.geometry_id, &fl).map_err(|e| Error::new(e.0))?;
                degenerate += g.degenerate;
                out_of_range += g.out_of_range;
                for m in &g.unknown_mappings {
                    r.warnings.push(format!("a layer with an unknown mapping is skipped ({m})"));
                }
                e.insert(g)
            }
        };
        let g = &*g;
        if g.polygons.is_empty() {
            continue;
        }
        let name = if inst.name.is_empty() { "mesh" } else { inst.name.as_str() };
        let part = r.b.new_part(name)?;
        let world = mul(&root, &inst.geometry_to_world);
        let mut slots = Vec::with_capacity(inst.materials.len());
        for id in &inst.materials {
            slots.push(r.material(*id)?);
        }
        r.geometry(g, &world, part, &slots).map_err(|e| e.context(format!("model `{name}`")))?;
    }
    if degenerate > 0 {
        r.warnings.push(format!("{degenerate} polygons with fewer than three corners skipped"));
    }
    if out_of_range > 0 {
        r.warnings.push(format!("{out_of_range} polygons with a corner outside the vertices skipped"));
    }
    let Reader { b, mut warnings, .. } = r;
    warnings.dedup();
    Ok(Loaded { mesh: b.finish(), warnings })
}

struct Reader<'a> {
    scene: &'a Scene,
    sidecar: &'a dyn Fn(&str) -> Result<Vec<u8>, Error>,
    b: Builder<'a>,
    /// FBX material id to ours.
    materials: HashMap<i64, u32>,
    /// FBX texture id to our image (`None`: could not be read, warned once).
    images: HashMap<i64, Option<u32>>,
    warnings: Vec<String>,
}

impl Reader<'_> {
    fn material(&mut self, id: i64) -> Result<u32, Error> {
        if let Some(m) = self.materials.get(&id) {
            return Ok(*m);
        }
        let Some(info) = self.scene.material(id) else { return Ok(0) };
        let texture = match &info.diffuse_texture {
            Some(t) => self.image(t)?,
            None => None,
        };
        let [r, g, b] = info.diffuse.map(|c| if c.is_finite() { c.max(0.0) as f32 } else { 1.0 });
        let m = self.b.material(Material {
            name: info.name.clone(),
            color: [r, g, b, info.opacity as f32],
            texture,
        })?;
        self.materials.insert(id, m);
        Ok(m)
    }

    /// The image of a texture: embedded, else the relative path beside the file, else the
    /// file name alone beside the file (writers store absolute paths of their own machine).
    fn image(&mut self, t: &penumbra_fbx::TextureInfo) -> Result<Option<u32>, Error> {
        if let Some(i) = self.images.get(&t.id) {
            return Ok(*i);
        }
        let embedded = t.video_id.and_then(|v| self.scene.video_content(v)).map(<[u8]>::to_vec);
        let bytes = match embedded {
            Some(b) => Ok(b),
            None => {
                let base = |p: &str| {
                    Path::new(&p.replace('\\', "/")).file_name().map(|n| n.to_string_lossy().to_string())
                };
                let mut tries: Vec<String> = Vec::new();
                for c in [
                    Some(t.relative_file_name.replace('\\', "/")),
                    base(&t.relative_file_name),
                    base(&t.file_name),
                ]
                .into_iter()
                .flatten()
                {
                    if !c.is_empty() && !tries.contains(&c) {
                        tries.push(c);
                    }
                }
                let mut last = Error::new("no file name");
                let mut found = None;
                for c in &tries {
                    match (self.sidecar)(c) {
                        Ok(b) => {
                            found = Some(b);
                            break;
                        }
                        Err(e) => last = e,
                    }
                }
                found.ok_or(last)
            }
        };
        let id = match bytes {
            Ok(bytes) => {
                let name =
                    if t.name.is_empty() { format!("texture{}", self.images.len()) } else { t.name.clone() };
                Some(self.b.image(Image { name, bytes })?)
            }
            Err(e) => {
                self.warnings.push(format!("texture `{}`: {e}", t.name));
                None
            }
        };
        self.images.insert(t.id, id);
        Ok(id)
    }

    fn geometry(&mut self, g: &Geometry, world: &Mat, part: u32, slots: &[u32]) -> Result<(), Error> {
        let nm = normal_matrix(world);
        let flip = determinant(world) < 0.0;
        let place = |p: [f64; 3]| -> [f32; 3] {
            let row = |r: &[f64; 4]| (r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3]) as f32;
            [row(&world[0]), row(&world[1]), row(&world[2])]
        };
        let turn = |n: [f64; 3]| -> [f32; 3] {
            normalize(nm.map(|r| (r[0] * n[0] + r[1] * n[1] + r[2] * n[2]) as f32))
        };
        let uv = |c: usize| -> [f32; 2] {
            g.uvs.as_ref().and_then(|a| a.at(c)).map_or([0.0; 2], |[u, v]| [u as f32, (1.0 - v) as f32])
        };
        let corners: usize = g.polygons.iter().map(|p| p.count as usize).sum();
        let triangles: usize = g.polygons.iter().map(|p| p.count as usize - 2).sum();
        self.b.reserve_triangles(triangles)?;
        let normals = g.normals.as_ref();
        self.b.reserve_vertices(if normals.is_some() { corners } else { triangles.saturating_mul(3) })?;
        // Corners alike (same point, normal and uv value) share a vertex.
        let mut shared: HashMap<(u32, u32, u32), u32> = HashMap::new();
        for (k, poly) in g.polygons.iter().enumerate() {
            let material = slots.get(g.material(k) as usize).copied().unwrap_or(0);
            let n = poly.count as usize;
            let first = poly.first as usize;
            for tri in g.triangulate(poly) {
                let tri = if flip { [tri[0], tri[2], tri[1]] } else { tri };
                // Bit k: the edge opposite corner k lies inside the polygon (its corners are
                // not neighbours on the polygon's outline).
                let rel = tri.map(|c| c as usize - first);
                let neighbours = |a: usize, b: usize| (a + 1) % n == b || (b + 1) % n == a;
                let inner = if n > 3 {
                    u8::from(!neighbours(rel[1], rel[2]))
                        | u8::from(!neighbours(rel[2], rel[0])) << 1
                        | u8::from(!neighbours(rel[0], rel[1])) << 2
                } else {
                    0
                };
                match normals {
                    Some(na) => {
                        let mut ids = [0u32; 3];
                        for (slot, c) in ids.iter_mut().zip(tri) {
                            let c = c as usize;
                            let v = g.corners.get(c).copied().unwrap_or(0);
                            let key = (
                                v,
                                na.index.get(c).copied().unwrap_or(u32::MAX),
                                g.uvs.as_ref().and_then(|a| a.index.get(c).copied()).unwrap_or(u32::MAX),
                            );
                            *slot = match shared.get(&key) {
                                Some(i) => *i,
                                None => {
                                    let normal = na.at(c).map_or([0.0; 3], turn);
                                    let position = place(g.position(c).unwrap_or([0.0; 3]));
                                    let i = self.b.vertex(Vertex { position, normal, uv: uv(c) })?;
                                    shared.insert(key, i);
                                    i
                                }
                            };
                        }
                        self.b.triangle(ids, part, material)?;
                    }
                    None => {
                        let p = tri.map(|c| place(g.position(c as usize).unwrap_or([0.0; 3])));
                        if crate::face_normal(p) == [0.0; 3] {
                            continue;
                        }
                        self.b.flat_triangle(p, tri.map(|c| uv(c as usize)), part, material)?;
                    }
                }
                self.b.inner_edges(inner);
            }
        }
        Ok(())
    }
}
