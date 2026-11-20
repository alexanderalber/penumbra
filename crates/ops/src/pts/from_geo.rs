//! `pts.from_geo`: points from geometry, as TouchDesigner's SOP to POP (the mesh's vertices) and
//! Sprinkle POP (points scattered over its surface) make them. The way to emit particles from a
//! model or a text: `geo.text` or `geo.file` into this node into `pts.particles`.
//!
//! Meshes live on the CPU, so the points are made there, in each item's own space, once per
//! change of a model or a param, in a job off the cook thread: a scatter of a million points
//! over a large mesh takes a moment. Meanwhile the last points stay; under exact frames the cook
//! waits. A job that runs is never thrown away: a newer request starts when it has ended.
//!
//! Points are in world space: each item's placement is applied at every cook on the GPU, normals
//! turned with it, so a model that moves or turns needs no new job and no new upload. The points
//! as made go to the GPU once per job, in their item's space; a compute pass places them. The
//! share of points each triangle gets follows its area under the placement at the time the
//! points were made; a scale that changes later stretches the points with the mesh. The box is
//! exact under the placements the points were made with, else the box around each item's box as
//! it is placed now. Items drawn as instances (sprites, copies on points) are left out with a
//! warning.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Value};
use penumbra_gpu::geo::{Mat4, Model, normal_matrix, transform};
use penumbra_gpu::points::{Attr, AttrType, Points};
use penumbra_gpu::{Buffer, BufferRing, Dispatch, Geo, Gpu, Out, Step};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator,
};
use penumbra_mesh::Bounds;

use super::upload::{Column, Uploads};
use crate::{gpu, on_gpu};

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
            "`position`, `normal` and `uv` in world space; `color` from the material where the geometry has one that is not plain white. A model that moves or turns takes its points with it at every frame. The box is exact while the model stands where it stood when the points were made; once it moves, a box around its own box as placed",
        ),
        || Box::new(FromGeo::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
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

/// Places the points made in their item's space: per item seven rows in `pts_worlds`, the
/// placement's first three (model to world) and the turn of normals. An item past `items` stays
/// where its model has it.
const PLACE: &str = "
@group(2) @binding(0) var<storage, read> pts_item: array<u32>;
@group(2) @binding(1) var<storage, read> pts_position: array<vec4f>;
@group(2) @binding(2) var<storage, read> pts_normal: array<vec4f>;
@group(2) @binding(3) var<storage, read> pts_worlds: array<vec4f>;
@group(2) @binding(4) var<storage, read_write> pts_placed: array<vec4f>;
@group(2) @binding(5) var<storage, read_write> pts_turned: array<vec4f>;

@compute @workgroup_size(64)
fn place(@builtin(global_invocation_id) gid: vec3u) {
    let i = gid.y * pn.size.x + gid.x;
    if gid.x >= pn.size.x || i >= u32(max(params.n, 0)) {
        return;
    }
    let k = pts_item[i];
    let p = vec4f(pts_position[i].xyz, 1.0);
    let n = pts_normal[i].xyz;
    if k >= u32(max(params.items, 0)) {
        pts_placed[i] = p;
        pts_turned[i] = vec4f(n, 0.0);
        return;
    }
    let b = 7u * k;
    pts_placed[i] = vec4f(dot(pts_worlds[b], p), dot(pts_worlds[b + 1u], p), dot(pts_worlds[b + 2u], p), 1.0);
    let t = vec3f(dot(pts_worlds[b + 4u].xyz, n), dot(pts_worlds[b + 5u].xyz, n), dot(pts_worlds[b + 6u].xyz, n));
    let l = length(t);
    pts_turned[i] = vec4f(select(vec3f(0.0), t / l, l > 1e-12 && l < 1e30), 0.0);
}
";

fn place_params() -> [ParamSpec; 2] {
    [ParamSpec::int("n", 0), ParamSpec::int("items", 0)]
}

/// Points per row of the placing pass's grid.
const ROW: u32 = 1 << 16;

/// Compiles the placing pass.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    gpu.cached_compute("pts from_geo place", PLACE, &place_params()).map(|_| ())
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
    /// The placements the points were made under, per item.
    worlds: Vec<Mat4>,
    /// Per item, the box around its points in its own space.
    boxes: Vec<Option<Bounds>>,
    /// The box around every point under `worlds`.
    world_box: Option<Bounds>,
}

impl Made {
    /// The points' attributes in their item's space, and `item`: what goes to the GPU once.
    fn columns(&self) -> Vec<Column> {
        let n = self.item.len();
        let mut item = Column::new(ITEM, AttrType::U32);
        let mut position = Column::new("position", AttrType::Vec3);
        let mut normal = Column::new("normal", AttrType::Vec3);
        let mut uv = Column::new("uv", AttrType::Vec2);
        item.words.extend_from_slice(&self.item);
        position.words.reserve(n * 4);
        normal.words.reserve(n * 4);
        uv.words.reserve(n * 2);
        for ((p, nrm), t) in self.position.iter().zip(&self.normal).zip(&self.uv) {
            position.push([p[0], p[1], p[2], 1.0]);
            normal.push([nrm[0], nrm[1], nrm[2], 0.0]);
            uv.push([t[0], t[1], 0.0, 0.0]);
        }
        let mut columns = vec![item, position, normal, uv];
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

    /// The box around the points placed by `worlds`: exact under the placements they were made
    /// with, else the box around each item's box placed (an item past the end of `worlds` as its
    /// model has it).
    fn bounds(&self, worlds: &[Mat4]) -> Option<Bounds> {
        if worlds == self.worlds.as_slice() {
            return self.world_box;
        }
        let mut out: Option<Bounds> = None;
        for (k, b) in self.boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            for c in b.corners() {
                let c = c.map(f64::from);
                let p = worlds.get(k).map_or(c, |w| transform(w, c)).map(|x| x as f32);
                if p.iter().all(|x| x.is_finite()) {
                    out = Some(out.map_or(Bounds { min: p, max: p }, |o| o.with(p)));
                }
            }
        }
        out
    }

    /// The boxes of the points, per item in its own space and all together under `worlds`.
    fn boxed(mut self) -> Self {
        let mut boxes: Vec<Option<Bounds>> = vec![None; self.worlds.len()];
        let mut world_box: Option<Bounds> = None;
        let grow = |b: &mut Option<Bounds>, p: [f32; 3]| {
            if p.iter().all(|x| x.is_finite()) {
                *b = Some(b.map_or(Bounds { min: p, max: p }, |o| o.with(p)));
            }
        };
        for (k, p) in self.item.iter().zip(&self.position) {
            if let Some(b) = boxes.get_mut(*k as usize) {
                grow(b, *p);
            }
            let placed =
                self.worlds.get(*k as usize).map_or(*p, |w| transform(w, p.map(f64::from)).map(|x| x as f32));
            grow(&mut world_box, placed);
        }
        self.boxes = boxes;
        self.world_box = world_box;
        self
    }
}

/// The attribute of the points as uploaded that names their item; it stays in the node.
const ITEM: &str = "item";

#[derive(Default)]
struct FromGeo {
    slot: AsyncSlot<Arc<Made>>,
    asked: Option<Request>,
    /// The points that went out last: what they were made from, the placements, and them.
    sent: Option<(Arc<Made>, Vec<Mat4>, Points)>,
    uploads: Uploads,
    /// The points as made, in their items' space, on the GPU: uploaded once per job.
    model: Option<(Arc<Made>, Points)>,
    /// The placements for the placing pass, seven rows per item.
    worlds: Option<Buffer>,
    /// The placed positions and normals that go out.
    placed: [BufferRing; 2],
}

impl FromGeo {
    /// The points of `made` placed by `worlds` on the GPU; `uv` and `color` go out as uploaded.
    fn place(&mut self, gpu: &Gpu, made: &Arc<Made>, worlds: &[Mat4]) -> Result<Points, Error> {
        let model = match &self.model {
            Some((m, p)) if Arc::ptr_eq(m, made) => p.clone(),
            _ => {
                let p = self.uploads.upload(gpu, made.item.len(), &made.columns(), None)?;
                self.model = Some((made.clone(), p.clone()));
                p
            }
        };
        let n = made.item.len() as u32;
        let capacity = model.capacity();
        let attr = |name: &str| {
            model.attr(name).ok_or_else(|| Error::new(format!("the uploaded points lack `{name}`")))
        };
        let (item, position, normal) = (attr(ITEM)?, attr("position")?, attr("normal")?);
        let mut rows: Vec<[f32; 4]> = Vec::with_capacity(worlds.len().max(1) * 7);
        for w in worlds {
            let turn = normal_matrix(w);
            rows.extend(w.iter().map(|r| r.map(|x| x as f32)));
            rows.extend(turn.iter().map(|r| [r[0] as f32, r[1] as f32, r[2] as f32, 0.0]));
        }
        if rows.is_empty() {
            rows.push([0.0; 4]);
        }
        let bytes = rows.len() as u64 * 16;
        let table = match &self.worlds {
            Some(b) if b.bytes() == bytes => b.clone(),
            _ => {
                let b = Buffer::new(gpu, bytes, "pts from_geo worlds")?;
                self.worlds = Some(b.clone());
                b
            }
        };
        table.write(gpu, 0, &rows)?;
        let [ring_p, ring_n] = &mut self.placed;
        let placed = ring_p.next(gpu, u64::from(capacity) * 16, "pts position")?;
        let turned = ring_n.next(gpu, u64::from(capacity) * 16, "pts normal")?;
        if n > 0 {
            let items = worlds.len() as i64;
            let param = |name: &str| match name {
                "n" => Some(Value::Int(i64::from(n))),
                "items" => Some(Value::Int(items)),
                _ => None,
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            let (w, h) = (n.clamp(1, ROW), n.div_ceil(n.clamp(1, ROW)));
            gpu.cached_compute("pts from_geo place", PLACE, &place_params())?.run_with(
                gpu,
                &[item.buffer(), position.buffer(), normal.buffer(), &table, &placed, &turned],
                &[Step { entry: "place", out: Out::grid(w, h), input: &input }],
            )?;
        }
        let mut attrs = vec![
            Attr::new("position", AttrType::Vec3, placed)?,
            Attr::new("normal", AttrType::Vec3, turned)?,
        ];
        attrs.extend(
            model.attrs().iter().filter(|a| !["position", "normal", ITEM].contains(&a.name())).cloned(),
        );
        Ok(Points::new(capacity, n, attrs)?.with_bounds(made.bounds(worlds).filter(|_| n > 0)))
    }
}

impl Operator for FromGeo {
    fn gpu_bytes(&self) -> u64 {
        self.uploads.bytes()
            + self.worlds.as_ref().map_or(0, Buffer::bytes)
            + self.placed.iter().map(BufferRing::bytes).sum::<u64>()
    }

    fn trim(&mut self) {
        self.uploads.trim();
        self.placed.iter_mut().for_each(BufferRing::trim);
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
        let points = self.place(gpu, &made, &worlds)?;
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
        worlds: r.items.iter().map(|s| s.world).collect(),
        boxes: Vec::new(),
        world_box: None,
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
            return m.boxed();
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
    m.boxed()
}
