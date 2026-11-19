//! `pts.from_geo`: points from geometry, as TouchDesigner's SOP to POP (the mesh's vertices) and
//! Sprinkle POP (points scattered over its surface) make them. The way to emit particles from a
//! model or a text: `geo.text` or `geo.file` into this node into `pts.particles`.
//!
//! Meshes live on the CPU, so the points are made there, in each item's own space, once per
//! change of a model or a param, in a job off the cook thread: a scatter of a million points
//! over a large mesh takes a moment. Meanwhile the last points stay; under exact frames the cook
//! waits. A job that runs is never thrown away: a newer request starts when it has ended.
//!
//! Points are in world space: each item's placement is applied at every cook, normals turned
//! with it, so a model that moves or turns needs no new job. The share of points each triangle
//! gets follows its area under the placement at the time the points were made; a scale that
//! changes later stretches the points with the mesh. Items drawn as instances (sprites, copies on
//! points) are left out with a warning.

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
            "`position`, `normal` and `uv` in world space; `color` from the material where the geometry has one that is not plain white. A model that moves or turns takes its points with it at every frame. Exact bounds",
        ),
        || Box::new(FromGeo::default()) as Box<dyn Operator>,
    ))
}

/// What the points are made of: the items' models and colours, and the params. The items'
/// placements are not part of it: they are applied to the points at every cook.
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
    /// The placement when the points are made: it weighs the triangles by their area as they
    /// stand.
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
                .all(|(a, b)| Arc::ptr_eq(&a.model, &b.model) && a.color == b.color)
    }
}

/// The points made, in each item's own space, ready to be placed.
struct Made {
    /// Per point: the item it lies on.
    item: Vec<u32>,
    position: Vec<[f32; 3]>,
    normal: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    /// Per point, when any item shows a colour other than plain white.
    color: Option<Vec<[f32; 4]>>,
    /// Points left out past [`MAX_POINTS`].
    cut: usize,
}

impl Made {
    /// The points' attributes with each item's placement in `worlds` applied (an item past the
    /// end stays where its model has it).
    fn columns(&self, worlds: &[Mat4]) -> Vec<Column> {
        let n = self.item.len();
        let turns: Vec<_> = worlds.iter().map(normal_matrix).collect();
        let mut position = Column::new("position", AttrType::Vec3);
        let mut normal = Column::new("normal", AttrType::Vec3);
        let mut uv = Column::new("uv", AttrType::Vec2);
        position.words.reserve(n * 4);
        normal.words.reserve(n * 4);
        uv.words.reserve(n * 2);
        let f = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32, 0.0];
        for ((item, p), (nrm, t)) in
            self.item.iter().zip(&self.position).zip(self.normal.iter().zip(&self.uv))
        {
            let p = p.map(f64::from);
            let nrm = nrm.map(f64::from);
            match (worlds.get(*item as usize), turns.get(*item as usize)) {
                (Some(world), Some(turn)) => {
                    position.push(f(transform(world, p)));
                    normal.push(f(unit(turn.map(|row| row[0] * nrm[0] + row[1] * nrm[1] + row[2] * nrm[2]))));
                }
                _ => {
                    position.push(f(p));
                    normal.push(f(nrm));
                }
            }
            uv.push([t[0], t[1], 0.0, 0.0]);
        }
        let mut columns = vec![position, normal, uv];
        if let Some(colors) = &self.color {
            let mut color = Column::new("color", AttrType::Vec4);
            color.words.reserve(n * 4);
            for c in colors {
                color.push(*c);
            }
            columns.push(color);
        }
        columns
    }
}

#[derive(Default)]
struct FromGeo {
    slot: AsyncSlot<Arc<Made>>,
    asked: Option<Request>,
    /// The points that went out last: what they were made from, the placements, and them.
    sent: Option<(Arc<Made>, Vec<Mat4>, penumbra_gpu::Points)>,
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
        let meshes = || geo.items.iter().filter(|i| i.instances.is_none());
        let request = Request {
            items: meshes()
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
        let worlds: Vec<Mat4> = meshes().map(|i| i.world).collect();
        let exact = cx.exact_frames();
        let mut fresh = self.slot.poll();
        // A job that runs is never thrown away: a newer request starts when it has ended.
        if self.asked.as_ref() != Some(&request) {
            if exact {
                fresh |= super::program::finish(&mut self.slot);
            }
            if !self.slot.busy() {
                self.asked = Some(request.clone());
                let job = request;
                self.slot.start(cx.spawn(move |_job| Ok(Arc::new(make(&job)))));
            }
        }
        if exact {
            fresh |= super::program::finish(&mut self.slot);
        }
        let Some(made) = self.slot.latest().cloned() else {
            return match self.slot.error() {
                Some(e) if !self.slot.busy() => Err(CookError::failed(e.to_string())),
                _ => Err(CookError::Pending),
            };
        };
        if let Some(e) = self.slot.error().filter(|_| !self.slot.busy()) {
            return Err(CookError::failed(e.to_string()));
        }
        if made.cut > 0 {
            cx.warn(format!("{} vertices past {MAX_POINTS} are left out", made.cut));
        }
        if !fresh
            && let Some((before, placed, _)) = &self.sent
            && Arc::ptr_eq(before, &made)
            && *placed == worlds
        {
            return Ok(Cooked::Unchanged);
        }
        let columns = made.columns(&worlds);
        let bounds = columns.first().and_then(bounds_of);
        let points = self.uploads.upload(gpu, made.item.len(), &columns, bounds)?;
        self.sent = Some((made, worlds, points.clone()));
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
    }
}

/// A source's mesh in world space, for the area of its triangles.
fn placed(s: &Source) -> Vec<[f64; 3]> {
    s.model.mesh.vertices.iter().map(|v| transform(&s.world, v.position.map(f64::from))).collect()
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
    let mut m = Made {
        item: Vec::new(),
        position: Vec::new(),
        normal: Vec::new(),
        uv: Vec::new(),
        color: colour.then(Vec::new),
        cut: 0,
    };
    let f = |v: [f64; 3]| [v[0] as f32, v[1] as f32, v[2] as f32];
    if r.surface {
        // Every triangle of every source, weighed by its area in world space, added up as it goes.
        // (source, first index of the triangle, material, the area up to and with it)
        let mut tris: Vec<(usize, usize, u32, f64)> = Vec::new();
        let mut total = 0.0f64;
        for (k, s) in r.items.iter().enumerate() {
            let world = placed(s);
            let mesh = &s.model.mesh;
            for range in &mesh.ranges {
                let first = range.first as usize;
                let end = first.saturating_add(range.count as usize).min(mesh.indices.len());
                let mut i = first;
                while i + 3 <= end {
                    let corner =
                        |j: usize| mesh.indices.get(i + j).and_then(|v| world.get(*v as usize)).copied();
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
            m.color = None;
            return m;
        }
        let n = r.count as usize;
        m.item.reserve(n);
        m.position.reserve(n);
        m.normal.reserve(n);
        m.uv.reserve(n);
        let mut rng = SplitMix(r.seed ^ 0x5eed);
        for _ in 0..r.count {
            let at = rng.unit() * total;
            let t = tris.partition_point(|x| x.3 <= at).min(tris.len() - 1);
            let Some(&(k, i, material, _)) = tris.get(t) else { continue };
            let Some(s) = r.items.get(k) else { continue };
            let mesh = &s.model.mesh;
            let vertex = |j: usize| mesh.indices.get(i + j).and_then(|v| mesh.vertices.get(*v as usize));
            let (Some(va), Some(vb), Some(vc)) = (vertex(0), vertex(1), vertex(2)) else { continue };
            // Uniform over the triangle.
            let su = rng.unit().sqrt();
            let v = rng.unit();
            let w = [1.0 - su, su * (1.0 - v), su * v];
            let g = |x: [f32; 3]| x.map(f64::from);
            let (a, b, c) = (g(va.position), g(vb.position), g(vc.position));
            let pos = mix(w, [a, b, c]);
            let mut nrm = unit(mix(w, [g(va.normal), g(vb.normal), g(vc.normal)]));
            if nrm == [0.0; 3] {
                nrm = unit(cross(sub(b, a), sub(c, a)));
            }
            let t2 = mix([w[0] as f32, w[1] as f32, w[2] as f32], [va.uv, vb.uv, vc.uv]);
            m.item.push(k as u32);
            m.position.push(f(pos));
            m.normal.push(f(nrm));
            m.uv.push(t2);
            if let Some(colors) = &mut m.color {
                colors.push(color_of(s, material));
            }
        }
    } else {
        for (k, s) in r.items.iter().enumerate() {
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
            for (v, vertex) in mesh.vertices.iter().enumerate() {
                if m.item.len() >= MAX_POINTS as usize {
                    m.cut += 1;
                    continue;
                }
                m.item.push(k as u32);
                m.position.push(vertex.position);
                m.normal.push(f(unit(vertex.normal.map(f64::from))));
                m.uv.push(vertex.uv);
                if let Some(colors) = &mut m.color {
                    colors.push(color_of(s, material.get(v).copied().unwrap_or(0)));
                }
            }
        }
    }
    m
}
