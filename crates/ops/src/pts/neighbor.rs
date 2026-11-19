//! `pts.neighbor`: what each point's neighbours add up to, TouchDesigner's Neighbor POP.
//!
//! Per point: how many points lie within `distance` (`neighbors`), the vector to their mean
//! position (`cohesion`), a push away from those nearer than `separation` (`separation`), and
//! the mean of one attribute over them (`near_<name>`, such as `near_velocity`). These are the
//! three rules of a flock (boids): `pts.math` turns them into `force` inside a particle loop.
//! The indices of the neighbours themselves are not given out.
//!
//! The search is [`penumbra_gpu::points::near`]: the same points give the same numbers bit for
//! bit, since the grid visits the neighbours in a fixed order.

use penumbra_core::{Curve, Error, ParamSpec, Value};
use penumbra_gpu::points::near::{self, MAX_PER_CELL, NearGrid};
use penumbra_gpu::points::program::check_name;
use penumbra_gpu::points::{Attr, AttrType, Reports, canonical, default_of};
use penumbra_gpu::{Buffer, BufferRing, Dispatch, Gpu, Out, Step};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::{input, wake};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.neighbor";

/// The attributes the node writes.
pub const COUNT: &str = "neighbors";
pub const COHESION: &str = "cohesion";
pub const SEPARATION: &str = "separation";

/// The prefix of the averaged attribute's name.
pub const PREFIX: &str = "near_";

/// The default cap on the points one cube of the search gives a point.
pub const DEFAULT_PER_CELL: u32 = 64;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Neighbor::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "Counts each point's neighbours within `distance` and averages over them: the way to their middle, a push away from the nearest, the mean of an attribute. The three rules of a flock (boids)",
    )
    .param(
        ParamSpec::float("distance", super::proximity::DEFAULT_DISTANCE)
            .clamp(1e-6, 1e4)
            .range(0.001, 1.0)
            .curve(Curve::Log)
            .doc("How far a point looks for neighbours, in metres"),
    )
    .param(
        ParamSpec::float("separation", 0.05)
            .clamp(0.0, 1e4)
            .range(0.0, 1.0)
            .doc("Neighbours nearer than this, in metres, push the point away: the `separation` attribute. 0: no push. Beyond `distance` it acts as `distance`"),
    )
    .param(
        ParamSpec::text("average", "velocity")
            .doc("An attribute averaged over the neighbours into `near_<name>`: velocity for a flock that flies alike, color to blur colours. Empty: none. A point without neighbours keeps its own value"),
    )
    .param(
        ParamSpec::int("max_per_cell", i64::from(DEFAULT_PER_CELL))
            .clamp(1.0, f64::from(MAX_PER_CELL))
            .range(8.0, 1024.0)
            .curve(Curve::Log)
            .doc("The search cuts space into cubes as wide as `distance`. A point looks at no more than this many points of each cube around it: a dense clump costs no more than that. The node warns when points are left out")
            .more(),
    )
    .input("in", DataKind::Pts)
    .describe_input("in", "The points, such as particles inside their loop")
    .output("out", DataKind::Pts)
    .describe_output(
        "out",
        "The points with `neighbors` (how many lie within `distance`), `cohesion` (from the point to the mean position of its neighbours, 0 without any), `separation` (away from the neighbours nearer than `separation`: each pushes along the line between the two, 1 at no distance down to 0 at `separation`; two points on top of each other push each other opposite ways) and `near_<name>` (the mean of `average`)",
    )
}

/// How the averaged attribute is laid out: none, or its type and whether the points have it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Average {
    None,
    /// The points' own attribute.
    Read(AttrType),
    /// A standard attribute the points lack: every point has its default.
    Fixed(AttrType, [f32; 4]),
}

/// The WGSL of the pass for `average`.
fn source(average: Average) -> String {
    let element = |ty: AttrType| match ty {
        AttrType::Vec3 | AttrType::Vec4 => "vec4f",
        ty => ty.wgsl(),
    };
    let mut s = String::from(
        "@group(2) @binding(0) var<storage, read_write> neighbors: array<f32>;
@group(2) @binding(1) var<storage, read_write> cohesion: array<vec4f>;
@group(2) @binding(2) var<storage, read_write> separation: array<vec4f>;
@group(2) @binding(3) var<uniform> header: vec4u;
",
    );
    let first = match average {
        Average::None => 4,
        Average::Fixed(ty, _) => {
            s.push_str(&format!(
                "@group(2) @binding(4) var<storage, read_write> mean_out: array<{}>;\n",
                element(ty)
            ));
            5
        }
        Average::Read(ty) => {
            let el = element(ty);
            s.push_str(&format!("@group(2) @binding(4) var<storage, read> mean_in: array<{el}>;\n"));
            s.push_str(&format!("@group(2) @binding(5) var<storage, read_write> mean_out: array<{el}>;\n"));
            6
        }
    };
    s.push_str(&near::query_wgsl(first));
    let (zero, add, write) = match average {
        Average::None => (String::new(), String::new(), String::new()),
        Average::Fixed(ty, v) => {
            let value = match ty {
                AttrType::Vec3 | AttrType::Vec4 => {
                    format!("vec4f({:?}, {:?}, {:?}, {:?})", v[0], v[1], v[2], v[3])
                }
                AttrType::Vec2 => format!("vec2f({:?}, {:?})", v[0], v[1]),
                _ => format!("f32({:?})", v[0]),
            };
            (String::new(), String::new(), format!("    mean_out[i] = {value};\n"))
        }
        Average::Read(ty) => {
            let el = element(ty);
            let mean = match ty {
                // The fourth number of a vec3 stays the point's own.
                AttrType::Vec3 => "vec4f(sum.xyz / f32(c), own.w)",
                _ => "sum / f32(c)",
            };
            (
                format!("    var sum = {el}(0.0);\n"),
                "        sum += mean_in[w.index];\n".to_string(),
                format!(
                    "    let own = mean_in[i];\n    var mean = own;\n    if c > 0u {{\n        mean = {mean};\n    }}\n    mean_out[i] = mean;\n"
                ),
            )
        }
    };
    let walk = near::near_loop(
        "q.xyz",
        "i",
        &format!(
            "            c += 1u;
            toward += w.position - q.xyz;
            if w.distance < apart {{
                var away = (q.xyz - w.position) / max(w.distance, 1e-30);
                if w.distance <= 0.0 {{
                    // On top of each other: a way from the pair's indices, the other way for the
                    // other point, so duplicates part.
                    let h = (min(i, w.index) * 0x9e3779b1u) ^ (max(i, w.index) * 0x85ebca77u);
                    var d = vec3f(f32(h & 1023u), f32((h >> 10u) & 1023u), f32((h >> 20u) & 1023u)) / 511.5 - 1.0;
                    if dot(d, d) < 1e-6 {{
                        d = vec3f(1.0, 0.0, 0.0);
                    }}
                    away = select(-1.0, 1.0, i < w.index) * normalize(d);
                }}
                push += away * (1.0 - w.distance / apart);
            }}
    {add}"
        ),
    );
    s.push_str(&format!(
        "
@compute @workgroup_size(64)
fn neighbor(@builtin(global_invocation_id) id: vec3u) {{
    let k = id.y * pn.size.x + id.x;
    let n = u32(max(params.n, 0));
    if id.x >= pn.size.x || k >= n {{
        return;
    }}
    let q = pn_near_points[k];
    let i = bitcast<u32>(q.w);
    if i >= min(header.x, n) {{
        return;
    }}
    let apart = params.separation;
    var c = 0u;
    var toward = vec3f(0.0);
    var push = vec3f(0.0);
{zero}{walk}    neighbors[i] = f32(c);
    var middle = vec3f(0.0);
    if c > 0u {{
        middle = toward / f32(c);
    }}
    cohesion[i] = vec4f(middle, 0.0);
    separation[i] = vec4f(push, 0.0);
{write}}}
"
    ));
    s
}

fn params() -> [ParamSpec; 2] {
    [ParamSpec::int("n", 0), ParamSpec::float("separation", 0.0)]
}

fn label(average: Average) -> &'static str {
    match average {
        Average::None => "pts neighbor",
        _ => "pts neighbor average",
    }
}

/// Compiles the pass for every kind of average, so no cook waits for a compile.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    near::warm_up(gpu)?;
    let mut all = vec![Average::None];
    for ty in [AttrType::F32, AttrType::Vec2, AttrType::Vec3, AttrType::Vec4] {
        all.push(Average::Read(ty));
    }
    if let Some(v) = default_of("velocity") {
        all.push(Average::Fixed(v.ty, v.default));
    }
    for a in all {
        gpu.cached_compute(label(a), &source(a), &params())?;
    }
    Ok(())
}

#[derive(Default)]
struct Neighbor {
    grid: NearGrid,
    rings: Vec<(String, BufferRing)>,
    /// The count of points past the cap of their cube, read back for the warning, by the search
    /// it came from: the bits of `distance` and the cap.
    crowded: Reports<(u32, u32)>,
}

impl Neighbor {
    fn next(&mut self, gpu: &Gpu, name: &str, bytes: u64) -> Result<Buffer, Error> {
        let label = format!("pts {name}");
        if let Some((_, ring)) = self.rings.iter_mut().find(|(n, _)| n == name) {
            return ring.next(gpu, bytes, &label);
        }
        let mut ring = BufferRing::new();
        let b = ring.next(gpu, bytes, &label)?;
        self.rings.push((name.to_string(), ring));
        Ok(b)
    }
}

impl Operator for Neighbor {
    fn gpu_bytes(&self) -> u64 {
        self.grid.bytes() + self.rings.iter().map(|(_, r)| r.bytes()).sum::<u64>()
    }

    fn trim(&mut self) {
        self.grid.trim();
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let distance = cx.float("distance")? as f32;
        let apart = (cx.float("separation")? as f32).min(distance);
        let cap = u32::try_from(cx.int("max_per_cell")?.clamp(1, i64::from(MAX_PER_CELL)))
            .unwrap_or(DEFAULT_PER_CELL);
        let wanted = canonical(cx.text("average")?);
        let average = if wanted.is_empty() {
            Average::None
        } else {
            check_name(wanted).map_err(|e| CookError::failed(format!("average: {}", e.0)))?;
            match (points.attr(wanted), default_of(wanted)) {
                (Some(a), _) => match a.ty() {
                    AttrType::U32 | AttrType::I32 => {
                        return Err(CookError::failed(format!(
                            "average: `{wanted}` holds whole numbers; average an attribute of floats"
                        )));
                    }
                    ty => Average::Read(ty),
                },
                (None, Some(s)) => match s.ty {
                    AttrType::U32 | AttrType::I32 => {
                        return Err(CookError::failed(format!("average: `{wanted}` holds whole numbers")));
                    }
                    ty => Average::Fixed(ty, s.default),
                },
                (None, None) => {
                    return Err(CookError::failed(format!("average: the points have no `{wanted}`")));
                }
            }
        };
        let out_name = format!("{PREFIX}{wanted}");
        if !wanted.is_empty() {
            check_name(&out_name).map_err(|e| CookError::failed(format!("average: {}", e.0)))?;
        }
        let capacity = u64::from(points.capacity().max(1));
        let count = self.next(gpu, COUNT, capacity * 4)?;
        let toward = self.next(gpu, COHESION, capacity * 16)?;
        let push = self.next(gpu, SEPARATION, capacity * 16)?;
        let mean = match average {
            Average::None => None,
            Average::Read(ty) | Average::Fixed(ty, _) => {
                Some((ty, self.next(gpu, &out_name, capacity * ty.stride())?))
            }
        };
        self.rings.retain(|(n, _)| [COUNT, COHESION, SEPARATION, out_name.as_str()].contains(&n.as_str()));
        let near = self.grid.build(gpu, points, distance, cap)?;
        let search = (distance.to_bits(), cap);
        if let Some(near) = &near {
            let n = near.at_most();
            let header = points.count().header_for(gpu)?;
            let mut buffers: Vec<&Buffer> = vec![&count, &toward, &push, &header];
            let source_attr = match average {
                Average::Read(_) => points.attr(wanted).map(Attr::buffer),
                _ => None,
            };
            if let Some(b) = source_attr {
                buffers.push(b);
            }
            if let Some((_, b)) = &mean {
                buffers.push(b);
            }
            buffers.push(near.cells());
            buffers.push(near.points());
            let program = gpu.cached_compute(label(average), &source(average), &params())?;
            let param = |name: &str| match name {
                "n" => Some(Value::Int(i64::from(n))),
                "separation" => Some(Value::Float(f64::from(apart))),
                _ => None,
            };
            let input =
                Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
            let w = n.clamp(1, 1 << 16);
            program.run_with(
                gpu,
                &buffers,
                &[Step { entry: "neighbor", out: Out::grid(w, n.div_ceil(w)), input: &input }],
            )?;
            // For the warning only: one that cannot start says nothing.
            let from = points.attr("position").map(|a| a.buffer().clone());
            let mut keys: Vec<&Buffer> = vec![&header];
            keys.extend(from.as_ref());
            self.crowded.read(gpu, near.crowded(), &search, &keys, Some(wake(cx)));
        }
        if let Some(crowded) = self.crowded.words(&search).map(|w| w[0]).filter(|n| *n > 0) {
            cx.warn(format!(
                "{crowded} points crowd their cube beyond `max_per_cell`: no point looks at them, so the counts and means leave them out. Raise `max_per_cell` or lower `distance`"
            ));
        }
        let mut out = points
            .with_attr(Attr::new(COUNT, AttrType::F32, count)?)?
            .with_attr(Attr::new(COHESION, AttrType::Vec3, toward)?)?
            .with_attr(Attr::new(SEPARATION, AttrType::Vec3, push)?)?;
        if let Some((ty, b)) = mean {
            out = out.with_attr(Attr::new(&out_name, ty, b)?)?;
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
