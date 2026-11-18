//! `pts.generate`: points on a simple shape (a grid, a line, a circle, a sphere, a box), on its
//! surface or inside it, evenly ordered or at random. Made on the CPU and uploaded once per
//! change of a param. Ordered lines, rings and grids come with their lines ([`Strips`]), so
//! `geo.from_pts` in lines mode draws them as they are.

use std::f32::consts::{PI, TAU};

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::points::{Attr, AttrType, Points, Strips, capacity_for, most_elements};
use penumbra_gpu::{Buffer, BufferRing, Gpu};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use crate::gpu;

pub const KIND: &str = "pts.generate";

/// Choices of `shape`, `fill` and `distribution`.
pub const SHAPES: [&str; 5] = ["grid", "line", "circle", "sphere", "box"];
pub const FILLS: [&str; 2] = ["surface", "inside"];
pub const DISTRIBUTIONS: [&str; 2] = ["ordered", "random"];
pub const GRID_LINES: [&str; 4] = ["both", "rows", "columns", "none"];

/// Most points the node makes.
pub const MAX_POINTS: u32 = 1 << 22;

/// Most rows or columns of a grid.
const MAX_SIDE: u32 = 4096;

/// The angle between neighbours of a sunflower or Fibonacci spiral: points spread evenly.
const GOLDEN_ANGLE: f32 = 2.399_963_3;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Points on a simple shape: a grid, a line, a circle, a sphere or a box, centred on the origin. On the surface or filling the inside, evenly spread or at random",
        )
        .param(
            ParamSpec::choice("shape", &SHAPES)
                .doc("Which shape the points lie on, centred on the origin; size sets its measures")
                .choice_doc("grid", "rows by columns of points on an upright rectangle facing -y, size x wide and size z high")
                .choice_doc("line", "points along x, size x long")
                .choice_doc("circle", "an upright circle facing -y, as wide as size x")
                .choice_doc("sphere", "a ball as wide as size x")
                .choice_doc("box", "a box of size x by y by z"),
        )
        .param(
            ParamSpec::choice("fill", &FILLS)
                .doc("Circle, sphere and box: where the points lie")
                .choice_doc("surface", "on the outline of the circle, on the sphere, on the six sides of the box")
                .choice_doc("inside", "all over the disc, the ball or the box")
                .applies_when("shape", &["circle", "sphere", "box"]),
        )
        .param(
            ParamSpec::choice("distribution", &DISTRIBUTIONS)
                .doc("How the points spread over the shape")
                .choice_doc("ordered", "evenly, in a fixed order: a lattice, rings or a spiral. The same params give the same points")
                .choice_doc("random", "at random places, the same ones for the same `seed`"),
        )
        .param(
            ParamSpec::int("count", 1000)
                .clamp(1.0, f64::from(MAX_POINTS))
                .range(1.0, 1e6)
                .curve(Curve::Log)
                .doc("How many points. An ordered box comes as near as its lattice allows; the viewer shows the count made")
                .applies_when("shape", &["line", "circle", "sphere", "box"]),
        )
        .param(
            ParamSpec::int("rows", 10)
                .clamp(1.0, f64::from(MAX_SIDE))
                .range(1.0, 1000.0)
                .curve(Curve::Log)
                .doc("Grid: points from top to bottom")
                .applies_when("shape", &["grid"]),
        )
        .param(
            ParamSpec::int("cols", 10)
                .clamp(1.0, f64::from(MAX_SIDE))
                .range(1.0, 1000.0)
                .curve(Curve::Log)
                .doc("Grid: points from left to right")
                .applies_when("shape", &["grid"]),
        )
        .param(
            ParamSpec::choice("lines", &GRID_LINES)
                .doc("Ordered grid: which points join into lines, for geo.from_pts in lines mode. An ordered line is always one line, an ordered circle's outline one closed ring")
                .choice_doc("both", "every row and every column: the grid's wireframe")
                .choice_doc("rows", "each row from left to right")
                .choice_doc("columns", "each column from top to bottom")
                .choice_doc("none", "no lines: drawn as lines, all the points make one line, row after row")
                .applies_when("shape", &["grid"]),
        )
        .param(ParamSpec::vec3("size", [1.0; 3]).range(0.0, 10.0).doc(
            "Grid: x wide, z high. Line: x long. Circle and sphere: the diameter is x. Box: x, y, z. In metres",
        ))
        .param(
            ParamSpec::int("seed", 0)
                .clamp(0.0, 1e9)
                .range(0.0, 100.0)
                .doc("random: another number gives other places for the points")
                .applies_when("distribution", &["random"]),
        )
        .output("out", DataKind::Pts)
        .describe_output("out", "The points with `position`, and `normal` and `uv` where the shape gives them a meaning. Ordered lines, outlines of circles and grids come as lines (strips) that geo.from_pts draws in lines mode"),
        || Box::new(Generate::default()) as Box<dyn Operator>,
    ))
}

/// Points made on the CPU, before upload.
#[derive(Debug, Default, PartialEq)]
pub struct Cloud {
    /// xyz, and 1 in the fourth float.
    pub position: Vec<[f32; 4]>,
    /// Unit length, and 0 in the fourth float.
    pub normal: Option<Vec<[f32; 4]>>,
    pub uv: Option<Vec<[f32; 2]>>,
    /// Rows, columns and slices of an ordered lattice.
    pub dims: Option<[u32; 3]>,
    /// Which points join into lines.
    pub strips: Strips,
}

/// What `pts.generate` makes of its params.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape<'a> {
    pub shape: &'a str,
    pub inside: bool,
    pub random: bool,
    pub count: u32,
    pub rows: u32,
    pub cols: u32,
    pub size: [f32; 3],
    pub seed: u64,
    /// A grid's lines: `both`, `rows`, `columns` or `none` ([`GRID_LINES`]).
    pub lines: &'a str,
}

/// The points of `s`. `Err` for more than [`MAX_POINTS`].
pub fn cloud(s: &Shape<'_>) -> Result<Cloud, Error> {
    let size = s.size.map(|v| if v.is_finite() { v.abs() } else { 0.0 });
    let half = size.map(|v| 0.5 * v);
    let mut rng = Rng::new(s.seed);
    let count = s.count.clamp(1, MAX_POINTS);
    let out = match s.shape {
        "grid" => grid(s.rows, s.cols, half, s.random.then_some(&mut rng), s.lines)?,
        "line" => line(count, half[0], s.random.then_some(&mut rng)),
        "circle" => circle(count, half[0], s.inside, s.random.then_some(&mut rng)),
        "sphere" => sphere(count, half[0], s.inside, s.random.then_some(&mut rng)),
        "box" if s.random => box_random(count, half, s.inside, &mut rng),
        "box" => box_lattice(count, half, s.inside)?,
        other => return Err(Error::new(format!("no shape `{other}`"))),
    };
    if out.position.len() > MAX_POINTS as usize {
        return Err(Error::new(format!(
            "{} points are more than the {MAX_POINTS} allowed",
            out.position.len()
        )));
    }
    Ok(out)
}

fn grid(rows: u32, cols: u32, half: [f32; 3], rng: Option<&mut Rng>, lines: &str) -> Result<Cloud, Error> {
    let (rows, cols) = (rows.clamp(1, MAX_SIDE), cols.clamp(1, MAX_SIDE));
    let n = rows * cols;
    if n > MAX_POINTS {
        return Err(Error::new(format!("{rows} x {cols} points are more than the {MAX_POINTS} allowed")));
    }
    let at = |u: f32, v: f32| [(2.0 * u - 1.0) * half[0], 0.0, (1.0 - 2.0 * v) * half[2], 1.0];
    let (mut position, mut uv) = (Vec::with_capacity(n as usize), Vec::with_capacity(n as usize));
    let ordered = rng.is_none();
    match rng {
        Some(rng) => {
            for _ in 0..n {
                let (u, v) = (rng.next(), rng.next());
                position.push(at(u, v));
                uv.push([u, v]);
            }
        }
        None => {
            let step = |k: u32, of: u32| if of > 1 { k as f32 / (of - 1) as f32 } else { 0.5 };
            for r in 0..rows {
                for c in 0..cols {
                    let (u, v) = (step(c, cols), step(r, rows));
                    position.push(at(u, v));
                    uv.push([u, v]);
                }
            }
        }
    }
    let (along_rows, along_columns) = match lines {
        "both" => (true, true),
        "rows" => (true, false),
        "columns" => (false, true),
        _ => (false, false),
    };
    let strips = match (ordered, along_rows, along_columns) {
        (false, _, _) | (true, false, false) => Strips::None,
        (true, true, false) => Strips::regular(cols),
        (true, rows, columns) => Strips::Grid { cols, rows, columns },
    };
    Ok(Cloud {
        normal: Some(vec![[0.0, -1.0, 0.0, 0.0]; position.len()]),
        position,
        uv: Some(uv),
        dims: ordered.then_some([rows, cols, 1]),
        strips,
    })
}

fn line(count: u32, half: f32, rng: Option<&mut Rng>) -> Cloud {
    let ordered = rng.is_none();
    let ts: Vec<f32> = match rng {
        Some(rng) => (0..count).map(|_| rng.next()).collect(),
        None if count == 1 => vec![0.5],
        None => (0..count).map(|i| i as f32 / (count - 1) as f32).collect(),
    };
    Cloud {
        position: ts.iter().map(|t| [(2.0 * t - 1.0) * half, 0.0, 0.0, 1.0]).collect(),
        normal: None,
        uv: Some(ts.iter().map(|t| [*t, 0.5]).collect()),
        dims: ordered.then_some([1, count, 1]),
        strips: if ordered { Strips::regular(count) } else { Strips::None },
    }
}

/// The texture coordinates of a point on an upright disc of radius `r`: the disc's square, top
/// left at (0, 0).
fn disc_uv(x: f32, z: f32, r: f32) -> [f32; 2] {
    if r > 0.0 { [0.5 + 0.5 * x / r, 0.5 - 0.5 * z / r] } else { [0.5, 0.5] }
}

fn circle(count: u32, r: f32, inside: bool, mut rng: Option<&mut Rng>) -> Cloud {
    let ordered = rng.is_none();
    let mut position = Vec::with_capacity(count as usize);
    let mut uv = Vec::with_capacity(count as usize);
    for i in 0..count {
        let (radius, angle) = match (&mut rng, inside) {
            (Some(rng), false) => (r, TAU * rng.next()),
            (Some(rng), true) => (r * rng.next().sqrt(), TAU * rng.next()),
            (None, false) => (r, TAU * i as f32 / count as f32),
            // A sunflower: every point on its own ring, spread evenly over the disc.
            (None, true) => (r * ((i as f32 + 0.5) / count as f32).sqrt(), GOLDEN_ANGLE * i as f32),
        };
        let (x, z) = (radius * angle.cos(), radius * angle.sin());
        position.push([x, 0.0, z, 1.0]);
        uv.push(disc_uv(x, z, r));
    }
    Cloud {
        normal: Some(vec![[0.0, -1.0, 0.0, 0.0]; position.len()]),
        position,
        uv: Some(uv),
        dims: (ordered && !inside).then_some([1, count, 1]),
        strips: if ordered && !inside { Strips::Regular { len: count, closed: true } } else { Strips::None },
    }
}

/// A direction on the unit sphere from `z` (-1 to 1) and an angle about z.
fn direction(z: f32, angle: f32) -> [f32; 3] {
    let rho = (1.0 - z * z).max(0.0).sqrt();
    [rho * angle.cos(), rho * angle.sin(), z]
}

/// The texture coordinates of a direction, as the sphere of `geo.primitive` has them: u around
/// z starting at +y, v from the north pole (+z) down.
fn sphere_uv(d: [f32; 3]) -> [f32; 2] {
    let phi = (-d[0]).atan2(d[1]);
    let u = if phi < 0.0 { phi / TAU + 1.0 } else { phi / TAU };
    [u, d[2].clamp(-1.0, 1.0).acos() / PI]
}

/// Point `i`'s share of the ball's volume inside it, 0 to 1: an even sequence that does not
/// follow the spiral's (steps of 1 over the plastic number, unrelated to the golden angle).
fn depth(i: u32) -> f32 {
    let x = (f64::from(i) + 0.5) * 0.754_877_666_246_692_7;
    (x - x.floor()) as f32
}

fn sphere(count: u32, r: f32, inside: bool, mut rng: Option<&mut Rng>) -> Cloud {
    let mut position = Vec::with_capacity(count as usize);
    let mut normal = Vec::with_capacity(count as usize);
    let mut uv = Vec::with_capacity(count as usize);
    for i in 0..count {
        let (d, radius) = match &mut rng {
            Some(rng) => {
                let d = direction(2.0 * rng.next() - 1.0, TAU * rng.next());
                (d, if inside { r * rng.next().cbrt() } else { r })
            }
            None => {
                // A Fibonacci spiral from the north pole down. Inside, the distance from the
                // centre follows a sequence of its own, so every direction reaches every depth.
                let t = (i as f32 + 0.5) / count as f32;
                let d = direction(1.0 - 2.0 * t, GOLDEN_ANGLE * i as f32);
                (d, if inside { r * depth(i).cbrt() } else { r })
            }
        };
        position.push([d[0] * radius, d[1] * radius, d[2] * radius, 1.0]);
        normal.push([d[0], d[1], d[2], 0.0]);
        uv.push(sphere_uv(d));
    }
    Cloud { position, normal: Some(normal), uv: Some(uv), dims: None, strips: Strips::None }
}

/// The six sides of a box: the axis each faces along, and its sign.
const SIDES: [(usize, f32); 6] = [(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0), (2, -1.0)];

/// The normal of the side of a box of `half` sizes nearest to `p`.
fn side_normal(p: [f32; 3], half: [f32; 3]) -> [f32; 4] {
    let mut best = (0usize, -1.0f32);
    for (k, h) in half.iter().enumerate() {
        let reach = if *h > 0.0 { p.get(k).map_or(0.0, |v| v.abs() / h) } else { 0.0 };
        if reach > best.1 {
            best = (k, reach);
        }
    }
    let mut n = [0.0; 4];
    let negative = p.get(best.0).is_some_and(|v| *v < 0.0);
    if let Some(c) = n.get_mut(best.0) {
        *c = if negative { -1.0 } else { 1.0 };
    }
    n
}

fn box_random(count: u32, half: [f32; 3], inside: bool, rng: &mut Rng) -> Cloud {
    let mut position = Vec::with_capacity(count as usize);
    let mut normal = Vec::with_capacity(count as usize);
    // Each side by its area, so the points are spread evenly over all six.
    let area = |axis: usize| {
        let others: Vec<f32> = (0..3).filter(|k| *k != axis).filter_map(|k| half.get(k).copied()).collect();
        others.iter().product::<f32>()
    };
    let area_total: f32 = SIDES.iter().map(|(axis, _)| area(*axis)).sum();
    // A box flat in two directions has sides of no area: then its surface is the line or point
    // it is, and the sides across its zero sizes share the points alike.
    let weight = |axis: usize| {
        if area_total > 0.0 {
            area(axis)
        } else if half.get(axis).is_some_and(|h| *h == 0.0) {
            1.0
        } else {
            0.0
        }
    };
    let total: f32 = SIDES.iter().map(|(axis, _)| weight(*axis)).sum();
    for _ in 0..count {
        let mut p = [0.0; 3];
        for (c, h) in p.iter_mut().zip(half) {
            *c = (2.0 * rng.next() - 1.0) * h;
        }
        if !inside {
            let mut pick = rng.next() * total;
            let mut side = SIDES[0];
            for s in SIDES {
                side = s;
                pick -= weight(s.0);
                if pick <= 0.0 {
                    break;
                }
            }
            if let (Some(c), Some(h)) = (p.get_mut(side.0), half.get(side.0)) {
                *c = side.1 * h;
            }
            let mut n = [0.0; 4];
            if let Some(c) = n.get_mut(side.0) {
                *c = side.1;
            }
            normal.push(n);
        }
        position.push([p[0], p[1], p[2], 1.0]);
    }
    Cloud { position, normal: (!inside).then_some(normal), uv: None, dims: None, strips: Strips::None }
}

/// Points along each axis of a lattice of about `count` points in a box of `half` sizes, the
/// spacing as even as the sides allow; only the shell where `inside` is false. With the number
/// of points it holds.
fn lattice_sides(count: u32, half: [f32; 3], inside: bool) -> ([u32; 3], u64) {
    let points = |t: f64| -> ([u32; 3], u64) {
        let n = half
            .map(|h| ((f64::from(h) * 2.0 * t).round().max(0.0) as u32).saturating_add(1).min(MAX_POINTS));
        let all = n.iter().fold(1u64, |a, v| a.saturating_mul(u64::from(*v)));
        let inner = n.iter().fold(1u64, |a, v| a.saturating_mul(u64::from(v.saturating_sub(2))));
        (n, if inside { all } else { all.saturating_sub(inner) })
    };
    // Points per metre: grow until the lattice holds as many as asked, then take the nearer of
    // the two last steps.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    while points(hi).1 < u64::from(count) && hi < 1e9 {
        lo = hi;
        hi *= 2.0;
    }
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if points(mid).1 < u64::from(count) { lo = mid } else { hi = mid }
    }
    let (a, b) = (points(lo), points(hi));
    let off = |n: u64| n.abs_diff(u64::from(count));
    // The nearer of the two, unless it holds more points than allowed.
    if off(a.1) < off(b.1) || b.1 > u64::from(MAX_POINTS) { a } else { b }
}

fn box_lattice(count: u32, half: [f32; 3], inside: bool) -> Result<Cloud, Error> {
    let ([nx, ny, nz], total) = lattice_sides(count, half, inside);
    if total > u64::from(MAX_POINTS) {
        return Err(Error::new(format!("{nx} x {ny} x {nz} points are more than the {MAX_POINTS} allowed")));
    }
    let at = |k: u32, n: u32, h: f32| if n > 1 { (2.0 * k as f32 / (n - 1) as f32 - 1.0) * h } else { 0.0 };
    let mut position = Vec::new();
    let mut normal = Vec::new();
    // Slices from the front (-y) back, rows from the top down, columns from left to right.
    let edge = |v: u32, n: u32| v == 0 || v + 1 == n;
    for j in 0..ny {
        for k in (0..nz).rev() {
            // Inside the shell a row has its two ends only: step over the rest.
            let ends_only = !inside && !(edge(j, ny) || edge(k, nz));
            let step = if ends_only { nx.saturating_sub(1).max(1) } else { 1 };
            for i in (0..nx).step_by(step as usize) {
                let p = [at(i, nx, half[0]), at(j, ny, half[1]), at(k, nz, half[2])];
                position.push([p[0], p[1], p[2], 1.0]);
                if !inside {
                    normal.push(side_normal(p, half));
                }
            }
        }
    }
    Ok(Cloud {
        position,
        normal: (!inside).then_some(normal),
        uv: None,
        dims: inside.then_some([nz, nx, ny]),
        strips: Strips::None,
    })
}

/// A small fast random number generator (SplitMix64): the same numbers for the same seed on
/// every machine.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0x2545_F491_4F6C_DD1D))
    }

    /// A number from 0 up to, not including, 1.
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // The top 24 bits: every value exactly a float below 1.
        (z >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Uploads `cloud` as points, each attribute into a buffer of its ring. The buffers hold
/// [`capacity_for`] the count, so a count that moves a little reuses them.
pub fn upload(gpu: &Gpu, cloud: &Cloud, rings: &mut Rings) -> Result<Points, Error> {
    let n = cloud.position.len();
    let count = u32::try_from(n).map_err(|_| Error::new("too many points"))?;
    if count == 0 {
        return Ok(Points::empty());
    }
    let capacity = capacity_for(count, most_elements(gpu, AttrType::Vec3));
    // `values` holds the floats of `count` elements of `ty`, in its layout.
    let put = |ring: &mut BufferRing, name: &str, ty: AttrType, values: &[f32]| -> Result<Attr, Error> {
        if values.len() as u64 * 4 != u64::from(count) * ty.stride() {
            return Err(Error::new(format!("the attribute `{name}` has another length than the positions")));
        }
        let bytes = u64::from(capacity) * ty.stride();
        let buffer: Buffer = ring.next(gpu, bytes, &format!("pts {name}"))?;
        buffer.write(gpu, 0, values)?;
        Attr::new(name, ty, buffer)
    };
    let mut attrs =
        vec![put(&mut rings.position, "position", AttrType::Vec3, cloud.position.as_flattened())?];
    if let Some(normal) = &cloud.normal {
        attrs.push(put(&mut rings.normal, "normal", AttrType::Vec3, normal.as_flattened())?);
    }
    if let Some(uv) = &cloud.uv {
        attrs.push(put(&mut rings.uv, "uv", AttrType::Vec2, uv.as_flattened())?);
    }
    let bounds = Bounds::of(cloud.position.iter().map(|p| [p[0], p[1], p[2]]));
    Ok(Points::new(capacity, count, attrs)?
        .with_bounds(bounds)
        .with_dims(cloud.dims)
        .with_strips(cloud.strips))
}

/// The buffers of one generator, one ring per attribute.
#[derive(Default)]
pub struct Rings {
    position: BufferRing,
    normal: BufferRing,
    uv: BufferRing,
}

impl Rings {
    fn all(&self) -> [&BufferRing; 3] {
        [&self.position, &self.normal, &self.uv]
    }
}

#[derive(Default)]
struct Generate {
    rings: Rings,
}

impl Operator for Generate {
    fn gpu_bytes(&self) -> u64 {
        self.rings.all().iter().map(|r| r.bytes()).sum()
    }

    fn trim(&mut self) {
        self.rings.position.trim();
        self.rings.normal.trim();
        self.rings.uv.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let size = match cx.param("size").and_then(|v| v.components()) {
            Some(&[x, y, z]) => [x as f32, y as f32, z as f32],
            _ => return Err(CookError::failed("`size` must be three numbers")),
        };
        let int = |name: &str| -> Result<u32, CookError> {
            Ok(u32::try_from(cx.int(name)?.max(0)).unwrap_or(u32::MAX))
        };
        let shape = Shape {
            shape: cx.text("shape")?,
            inside: cx.text("fill")? == "inside",
            random: cx.text("distribution")? == "random",
            count: int("count")?,
            rows: int("rows")?,
            cols: int("cols")?,
            size,
            seed: u64::from(int("seed")?),
            lines: cx.text("lines")?,
        };
        let cloud = cloud(&shape)?;
        let points = upload(gpu, &cloud, &mut self.rings)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
    }
}
