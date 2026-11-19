//! `pts.from_geo`: points from geometry, as TouchDesigner's SOP to POP (the mesh's vertices) and
//! Sprinkle POP (points scattered over its surface) make them. The way to emit particles from a
//! model or a text: `geo.text` or `geo.file` into this node into `pts.particles`.
//!
//! Meshes live on the CPU, so the points are made there and uploaded once per change of the
//! geometry or a param, in a job off the cook thread: a scatter of a million points over a large
//! mesh takes a moment. Meanwhile the last points stay; under exact frames the cook waits.
//!
//! Points are in world space: each item's placement is applied, normals turned with it. Items
//! drawn as instances (sprites, copies on points) are left out with a warning.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::Geo;
use penumbra_gpu::geo::{Mat4, Model, normal_matrix, transform};
use penumbra_gpu::points::AttrType;
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator,
};

use super::upload::{Column, Uploads, bounds_of};
use crate::gpu;

pub const KIND: &str = "pts.from_geo";

/// Choices of `mode`.
pub const MODES: [&str; 2] = ["vertices", "surface"];

/// Most points the node makes.
pub const MAX_POINTS: u32 = 1 << 22;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Points from geometry: a point at every vertex of a mesh, or points scattered evenly over its surface. To emit particles from a model or a text, or to draw a model as dots",
        )
        .param(
            ParamSpec::choice("mode", &MODES)
                .doc("Where the points come from")
                .choice_doc("vertices", "one point at each vertex as the mesh stores it: a corner shared by faces of different normals is there once per face (a box has 24). As TouchDesigner's SOP to POP")
                .choice_doc("surface", "`count` points spread at random over the triangles, evenly by area, with the surface's normal and texture coordinates there. As TouchDesigner's Sprinkle POP"),
        )
        .param(
            ParamSpec::int("count", 10_000)
                .clamp(1.0, f64::from(MAX_POINTS))
                .range(100.0, 1_000_000.0)
                .curve(Curve::Log)
                .doc("surface: how many points")
                .applies_when("mode", &["surface"]),
        )
        .param(
            ParamSpec::int("seed", 0)
                .clamp(0.0, 1e9)
                .doc("surface: another seed gives other places; the same seed the same ones")
                .applies_when("mode", &["surface"]),
        )
        .input("in", DataKind::Geo)
        .describe_input("in", "The geometry, as tex.render would place it: each item's placement is applied. Items drawn as instances are left out")
        .output("out", DataKind::Pts)
        .describe_output(
            "out",
            "`position`, `normal` and `uv` in world space; `color` from the material where the geometry has one that is not plain white. Exact bounds",
        ),
        || Box::new(FromGeo::default()) as Box<dyn Operator>,
    ))
}

/// What the points are made of: the items' models, placements and colours, and the params.
#[derive(Clone)]
struct Request {
    items: Vec<Source>,
    surface: bool,
    count: u32,
    seed: u64,
}

#[derive(Clone)]
struct Source {
    model: Arc<Model>,
    world: Mat4,
    /// The item's material colour, which replaces the file's.
    color: Option<[f32; 4]>,
}

impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface
            && self.count == other.count
            && self.seed == other.seed
            && self.items.len() == other.items.len()
            && self
                .items
                .iter()
                .zip(&other.items)
                .all(|(a, b)| Arc::ptr_eq(&a.model, &b.model) && a.world == b.world && a.color == b.color)
    }
}

/// The points made, ready to upload.
struct Made {
    count: usize,
    columns: Vec<Column>,
    /// Points left out past [`MAX_POINTS`].
    cut: usize,
}

#[derive(Default)]
struct FromGeo {
    slot: AsyncSlot<Arc<Made>>,
    asked: Option<Request>,
    /// The request whose points went out last, and them.
    sent: Option<(Request, penumbra_gpu::Points)>,
    uploads: Uploads,
}

impl Operator for FromGeo {
    fn gpu_bytes(&self) -> u64 {
        self.uploads.bytes()
    }

    fn trim(&mut self) {
        self.uploads.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let Some(geo) = cx.input_as::<Geo>(0) else {
            return Err(if cx.def().inputs.first().is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect geometry to `in`")
            });
        };
        let instanced = geo.items.iter().filter(|i| i.instances.is_some()).count();
        if instanced > 0 {
            if instanced == geo.items.len() {
                return Err(CookError::failed(
                    "the geometry is drawn as instances only (sprites or copies on points), which live on the GPU: there is no mesh to take points from",
                ));
            }
            cx.warn(format!(
                "{instanced} items drawn as instances are left out: their copies live on the GPU"
            ));
        }
        let request = Request {
            items: geo
                .items
                .iter()
                .filter(|i| i.instances.is_none())
                .map(|i| Source {
                    model: i.model.clone(),
                    world: i.world,
                    color: i.material.as_ref().map(|m| m.color),
                })
                .collect(),
            surface: cx.text("mode")? == "surface",
            count: u32::try_from(cx.int("count")?.clamp(1, i64::from(MAX_POINTS))).unwrap_or(1),
            seed: u64::try_from(cx.int("seed")?.max(0)).unwrap_or(0),
        };
        if self.sent.as_ref().is_some_and(|(r, _)| *r == request) {
            return Ok(Cooked::Unchanged);
        }
        if self.asked.as_ref() != Some(&request) {
            self.asked = Some(request.clone());
            let job = request.clone();
            self.slot.start(cx.spawn(move |_job| Ok(Arc::new(make(&job)))));
        }
        if cx.exact_frames() {
            super::program::finish(&mut self.slot);
        } else {
            self.slot.poll();
        }
        let done = !self.slot.busy();
        match self.slot.latest().cloned() {
            Some(made) if done => {
                if made.cut > 0 {
                    cx.warn(format!("{} vertices past {MAX_POINTS} are left out", made.cut));
                }
                let bounds = made.columns.first().and_then(bounds_of);
                let points = self.uploads.upload(gpu, made.count, &made.columns, bounds)?;
                self.sent = Some((request, points.clone()));
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
            }
            _ => match (&self.sent, self.slot.error()) {
                (_, Some(e)) if done => Err(CookError::failed(e.to_string())),
                // The points before keep going out while the new ones are made.
                (Some((_, points)), _) => Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points.clone())])),
                (None, _) => Err(CookError::Pending),
            },
        }
    }
}

/// A source's mesh in world space: positions, unit normals (none where the mesh's is zero) and
/// texture coordinates per vertex, and each vertex's colour.
struct Placed {
    positions: Vec<[f64; 3]>,
    normals: Vec<[f64; 3]>,
    uvs: Vec<[f32; 2]>,
}

fn place(s: &Source) -> Placed {
    let mesh = &s.model.mesh;
    let n = normal_matrix(&s.world);
    let turn = |v: [f32; 3]| {
        let v = v.map(f64::from);
        unit(n.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2]))
    };
    Placed {
        positions: mesh.vertices.iter().map(|v| transform(&s.world, v.position.map(f64::from))).collect(),
        normals: mesh.vertices.iter().map(|v| turn(v.normal)).collect(),
        uvs: mesh.vertices.iter().map(|v| v.uv).collect(),
    }
}

/// `w[0] a + w[1] b + w[2] c` for the corners `[a, b, c]`, component by component.
fn mix<T, const N: usize>(w: [T; 3], [a, b, c]: [[T; N]; 3]) -> [T; N]
where
    T: Copy + Default + std::ops::Mul<Output = T> + std::ops::Add<Output = T>,
{
    let mut out = [T::default(); N];
    for (o, ((x, y), z)) in out.iter_mut().zip(a.iter().zip(&b).zip(&c)) {
        *o = w[0] * *x + w[1] * *y + w[2] * *z;
    }
    out
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-12 && len.is_finite() { v.map(|x| x / len) } else { [0.0; 3] }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// The colour a source's material `m` shows: the item's, else the file's.
fn color_of(s: &Source, material: u32) -> [f32; 4] {
    s.color.or_else(|| s.model.mesh.materials.get(material as usize).map(|m| m.color)).unwrap_or([1.0; 4])
}

/// Whether any of the sources shows a colour other than plain white.
fn coloured(sources: &[Source]) -> bool {
    sources.iter().any(|s| {
        s.color.is_some_and(|c| c != [1.0; 4])
            || s.model.mesh.ranges.iter().any(|r| color_of(s, r.material) != [1.0; 4])
    })
}

/// A random number generator for the scatter: the same seed, the same points.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// In [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

fn make(r: &Request) -> Made {
    let colour = coloured(&r.items);
    let mut position = Column::new("position", AttrType::Vec3);
    let mut normal = Column::new("normal", AttrType::Vec3);
    let mut uv = Column::new("uv", AttrType::Vec2);
    let mut color = Column::new("color", AttrType::Vec4);
    let f = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32, 0.0];
    let mut count = 0usize;
    let mut cut = 0usize;
    if r.surface {
        // Every triangle of every source in world space, with its area added up as it goes.
        let placed: Vec<Placed> = r.items.iter().map(place).collect();
        // (source, first index of the triangle, material, the area up to and with it)
        let mut tris: Vec<(usize, usize, u32, f64)> = Vec::new();
        let mut total = 0.0f64;
        for (k, (s, p)) in r.items.iter().zip(&placed).enumerate() {
            let mesh = &s.model.mesh;
            for range in &mesh.ranges {
                let first = range.first as usize;
                let end = first.saturating_add(range.count as usize).min(mesh.indices.len());
                let mut i = first;
                while i + 3 <= end {
                    let corner = |j: usize| {
                        mesh.indices.get(i + j).and_then(|v| p.positions.get(*v as usize)).copied()
                    };
                    if let (Some(a), Some(b), Some(c)) = (corner(0), corner(1), corner(2)) {
                        let n = cross(sub(b, a), sub(c, a));
                        let area = 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        if area.is_finite() && area > 0.0 {
                            total += area;
                            tris.push((k, i, range.material, total));
                        }
                    }
                    i += 3;
                }
            }
        }
        if tris.is_empty() || total <= 0.0 {
            return Made { count: 0, columns: vec![position, normal, uv], cut: 0 };
        }
        let mut rng = SplitMix(r.seed ^ 0x5eed);
        for _ in 0..r.count {
            let at = rng.unit() * total;
            let t = tris.partition_point(|x| x.3 <= at).min(tris.len() - 1);
            let Some(&(k, i, material, _)) = tris.get(t) else { continue };
            let (Some(s), Some(p)) = (r.items.get(k), placed.get(k)) else { continue };
            let mesh = &s.model.mesh;
            let idx = |j: usize| mesh.indices.get(i + j).map_or(0, |v| *v as usize);
            let (ia, ib, ic) = (idx(0), idx(1), idx(2));
            // Uniform over the triangle.
            let su = rng.unit().sqrt();
            let v = rng.unit();
            let w = [1.0 - su, su * (1.0 - v), su * v];
            let at3 = |list: &Vec<[f64; 3]>| {
                let g = |q: usize| list.get(q).copied().unwrap_or([0.0; 3]);
                let (a, b, c) = (g(ia), g(ib), g(ic));
                mix(w, [a, b, c])
            };
            let pos = at3(&p.positions);
            let mut nrm = unit(at3(&p.normals));
            if nrm == [0.0; 3] {
                let g = |q: usize| p.positions.get(q).copied().unwrap_or([0.0; 3]);
                nrm = unit(cross(sub(g(ib), g(ia)), sub(g(ic), g(ia))));
            }
            let g = |q: usize| p.uvs.get(q).copied().unwrap_or([0.0; 2]);
            let (ua, ub, uc) = (g(ia), g(ib), g(ic));
            let t2 = mix([w[0] as f32, w[1] as f32, w[2] as f32], [ua, ub, uc]);
            position.push(f(pos));
            normal.push(f(nrm));
            uv.push([t2[0], t2[1], 0.0, 0.0]);
            if colour {
                color.push(color_of(s, material));
            }
            count += 1;
        }
    } else {
        for s in &r.items {
            let p = place(s);
            let mesh = &s.model.mesh;
            // Each vertex's material: the first run of triangles that uses it.
            let mut material = vec![0u32; mesh.vertices.len()];
            let mut seen = vec![false; mesh.vertices.len()];
            for range in &mesh.ranges {
                let first = range.first as usize;
                let end = first.saturating_add(range.count as usize).min(mesh.indices.len());
                for v in mesh.indices.get(first..end).unwrap_or(&[]) {
                    let v = *v as usize;
                    if let (Some(m), Some(s)) = (material.get_mut(v), seen.get_mut(v))
                        && !*s
                    {
                        *m = range.material;
                        *s = true;
                    }
                }
            }
            for (v, pos) in p.positions.iter().enumerate() {
                if count >= MAX_POINTS as usize {
                    cut += 1;
                    continue;
                }
                position.push(f(*pos));
                normal.push(f(p.normals.get(v).copied().unwrap_or([0.0; 3])));
                let t = p.uvs.get(v).copied().unwrap_or([0.0; 2]);
                uv.push([t[0], t[1], 0.0, 0.0]);
                if colour {
                    color.push(color_of(s, material.get(v).copied().unwrap_or(0)));
                }
                count += 1;
            }
        }
    }
    let mut columns = vec![position, normal, uv];
    if colour {
        columns.push(color);
    }
    Made { count, columns, cut }
}
