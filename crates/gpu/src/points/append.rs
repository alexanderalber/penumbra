//! New points at the end of a set whose count is on the GPU ([`append`]): births of particles.
//!
//! The set's count stays on the GPU throughout: a pass reads it from the header, writes the new
//! points at `[count, count + born)` and a last pass raises the header by `born`. How many are
//! born is worked out on the GPU from what is wanted and the room left, so the count never
//! passes the room the caller gives (never above the capacity), and a set that is full takes no
//! more without a word. The new points keep the order they are born in: birth `t` of this step
//! lands at `count + t`, whatever else runs on the GPU.
//!
//! Each new point has a source point: picked at random by its id ([`Pick::Random`]), or birth
//! `t` comes from source point `t % sources` ([`Pick::Cycle`]), so every source point gives as
//! many. Each attribute of a new point is the source point's value of the same name, a fixed
//! value, or a WGSL expression of the source point's attributes, the new point's id and the
//! caller's params ([`Value`]). Ids come from a counter of the caller's ([`append`], `ids`):
//! birth `t` gets `ids[0] + t`, and the counter moves on by the points born.
//!
//! The work goes over as many dispatches as the GPU's storage buffers per program take: each
//! writes some attributes of every new point, and every one of them works out the same births,
//! so they agree.

use std::fmt::Write as _;

use penumbra_core::{Error, ParamSpec, Value as ParamValue};

use super::program::{element, literal, storage_budget};
use super::{AttrType, Points, canonical, default_of};
use crate::{Buffer, Dispatch, Gpu, Out, Step};

/// Points per row of the dispatch grid.
const ROW: u32 = 1 << 16;

/// How a new point's attribute is made.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The source point's value of the attribute of the same name; its default where the source
    /// lacks it or has it of another type.
    Copy,
    /// This value, in as many components as the type has.
    Fixed([f32; 4]),
    /// A WGSL expression of the attribute's type. It sees `id` (the new point's id), `src` (the
    /// source point's index), `born` (the birth's number in this step, from 0), `params.<name>`,
    /// `pts_rand(key, k)` and `pts_rand3(key, k)` (random numbers in 0..1), `pts_ball(key, k)` (a
    /// point in the unit ball) and `src_<name>` for each name in `reads`: the source point's
    /// value of that attribute, its default where the source lacks it.
    Expr { wgsl: String, reads: Vec<String> },
}

/// One attribute of the new points that is not simply copied.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub value: Value,
}

impl Item {
    pub fn new(name: &str, value: Value) -> Self {
        Self { name: canonical(name).to_string(), value }
    }
}

/// Which source point a birth comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// One at random, by the birth's id and the salt.
    Random,
    /// Birth `t` from point `t % sources`: every source point the same number of births.
    Cycle,
}

/// What one step gives birth to.
pub struct Births<'a> {
    /// The source points; their count may be on the GPU.
    pub source: &'a Points,
    /// Births wanted in all, or with `per_point` for each source point alive.
    pub wanted: u32,
    pub per_point: bool,
    pub pick: Pick,
    /// Mixed into every random number, so another salt gives other numbers.
    pub salt: u32,
    /// The count the set may reach at most: the capacity, or less.
    pub room: u32,
    /// The caller's params the expressions read, and their values.
    pub params: &'a [ParamSpec],
    pub param: &'a dyn Fn(&str) -> Option<ParamValue>,
}

impl Births<'_> {
    /// Births at most, as the CPU knows it: what the dispatches cover.
    pub fn most(&self) -> u32 {
        let sources = self.source.count().at_most();
        let wanted =
            if self.per_point { u64::from(self.wanted) * u64::from(sources) } else { u64::from(self.wanted) };
        let wanted = if sources == 0 { 0 } else { wanted };
        u32::try_from(wanted).unwrap_or(u32::MAX).min(self.room)
    }
}

/// The params every birth program reads besides the caller's.
fn own_params() -> [ParamSpec; 5] {
    [
        ParamSpec::int("pts_room", 0),
        ParamSpec::int("pts_wanted", 0),
        ParamSpec::int("pts_per_point", 0),
        ParamSpec::int("pts_sources", 0),
        ParamSpec::int("pts_salt", 0),
    ]
}

/// The births of this step, the same in every pass: the room left, what is wanted, a source to
/// come from.
const BORN: &str = "
fn pts_u(v: i32) -> u32 {
    return u32(max(v, 0));
}

fn pts_sources() -> u32 {
    return min(pts_source.x, pts_u(params.pts_sources));
}

fn pts_born(count: u32) -> u32 {
    let room = pts_u(params.pts_room);
    let n = pts_sources();
    var wanted = pts_u(params.pts_wanted);
    if params.pts_per_point != 0 {
        wanted = wanted * n;
    }
    if n == 0u || count >= room {
        wanted = 0u;
    }
    return min(wanted, room - min(count, room));
}
";

/// The random numbers of the expressions.
const RANDOM: &str = "
fn pts_rand3(key: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(key, k, u32(max(params.pts_salt, 0)))) >> vec3u(8u)) * (1.0 / 16777216.0);
}

fn pts_rand(key: u32, k: u32) -> f32 {
    return pts_rand3(key, k).x;
}

// A point in the ball of radius 1, evenly spread.
fn pts_ball(key: u32, k: u32) -> vec3f {
    let u = pts_rand3(key, k);
    let z = 2.0 * u.x - 1.0;
    let a = 6.2831853 * u.y;
    let r = sqrt(max(1.0 - z * z, 0.0));
    return vec3f(r * cos(a), r * sin(a), z) * pow(pts_rand(key, k + 1u), 1.0 / 3.0);
}
";

/// One attribute written in one pass, resolved against the set and the source.
#[derive(Clone, Debug)]
struct Slot {
    name: String,
    ty: AttrType,
    value: Value,
    /// Source attributes it reads: (name, type), or (name, None) for one the source lacks.
    reads: Vec<(String, Option<AttrType>)>,
}

impl Slot {
    /// Storage buffers it binds: its own, and each source attribute it reads that exists.
    fn bindings(&self) -> usize {
        1 + self.reads.iter().filter(|(_, t)| t.is_some()).count()
    }
}

/// Writes the births of one step into `state`, whose count is on the GPU and whose buffers the
/// caller owns: nobody else may read them yet. Every attribute of `state` gets a value: `items`
/// says how for the ones it names; the others copy the source point's value of their name, or
/// take their default. `ids` is the caller's counter, at least 16 bytes: its first word is the
/// id of the next birth, and it moves on by the points born. `Err` for a set whose count is
/// known on the CPU, an expression that does not compile, an attribute that needs more storage
/// buffers than a program gets, a room beyond the capacity.
pub fn append(
    gpu: &Gpu,
    state: &Points,
    ids: &Buffer,
    births: &Births<'_>,
    items: &[Item],
) -> Result<(), Error> {
    let Some(header) = state.count().header().cloned() else {
        return Err(Error::new("points are born into a set whose count is on the GPU"));
    };
    if births.room > state.capacity() {
        return Err(Error::new(format!(
            "room for {} points is more than the capacity of {}",
            births.room,
            state.capacity()
        )));
    }
    if ids.bytes() < 16 {
        return Err(Error::new("the counter of ids holds 16 bytes"));
    }
    let most = births.most();
    if most == 0 {
        return Ok(());
    }
    let source = births.source;
    let slots: Vec<Slot> = state
        .attrs()
        .iter()
        .map(|a| {
            let value = items.iter().find(|i| i.name == a.name()).map_or(Value::Copy, |i| i.value.clone());
            let names: Vec<String> = match &value {
                // A source attribute of another type is not read: the default goes in.
                Value::Copy if source.attr(a.name()).map(super::Attr::ty) == Some(a.ty()) => {
                    vec![a.name().to_string()]
                }
                Value::Copy => Vec::new(),
                Value::Fixed(_) => Vec::new(),
                Value::Expr { reads, .. } => reads.iter().map(|r| canonical(r).to_string()).collect(),
            };
            let reads = names
                .into_iter()
                .map(|n| {
                    // A standard attribute of another type than its own reads as its default.
                    let ty = source
                        .attr(&n)
                        .map(super::Attr::ty)
                        .filter(|t| default_of(&n).is_none_or(|d| d.ty == *t));
                    (n, ty)
                })
                .collect();
            Slot { name: a.name().to_string(), ty: a.ty(), value, reads }
        })
        .collect();
    let budget = storage_budget(gpu) as usize;
    let groups = pack(&slots, budget)?;
    let source_header = source.count().header_for(gpu)?;
    let mut params: Vec<ParamSpec> = births.params.to_vec();
    params.extend(own_params());
    let salt = births.salt;
    let room = births.room;
    let per_point = births.per_point;
    let wanted = births.wanted;
    let sources = source.count().at_most();
    let param = |name: &str| match name {
        "pts_room" => Some(ParamValue::Int(i64::from(room))),
        "pts_wanted" => Some(ParamValue::Int(i64::from(wanted.min(i32::MAX as u32)))),
        "pts_per_point" => Some(ParamValue::Int(i64::from(per_point))),
        "pts_sources" => Some(ParamValue::Int(i64::from(sources))),
        "pts_salt" => Some(ParamValue::Int(i64::from(salt & 0x7fff_ffff))),
        other => (births.param)(other),
    };
    let dispatch =
        Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
    let (w, h) = (most.clamp(1, ROW), most.div_ceil(most.clamp(1, ROW)));
    for group in &groups {
        let (source_text, buffers) =
            group_source(group, state, source, births.pick, &header, &source_header, ids)?;
        let program = gpu.cached_compute("pts births", &source_text, &params)?;
        let refs: Vec<&Buffer> = buffers.iter().collect();
        program.run_with(
            gpu,
            &refs,
            &[Step { entry: "pts_birth", out: Out::grid(w, h), input: &dispatch }],
        )?;
    }
    // Last, the count and the counter move on; every pass above read them as they were.
    let bump = gpu.cached_compute("pts births count", &format!("{BUMP}{BORN}"), &params)?;
    bump.run_with(
        gpu,
        &[&header, &source_header, ids],
        &[Step { entry: "pts_bump", out: Out::range(1), input: &dispatch }],
    )
}

/// Slots into passes of at most `budget` storage buffers each, in order.
fn pack(slots: &[Slot], budget: usize) -> Result<Vec<Vec<Slot>>, Error> {
    let mut groups: Vec<Vec<Slot>> = Vec::new();
    let mut used = 0usize;
    for s in slots {
        let need = s.bindings();
        if need > budget {
            return Err(Error::new(format!(
                "the births of `{}` read {} attributes; this GPU gives a program {budget} storage buffers",
                s.name,
                need - 1
            )));
        }
        match groups.last_mut() {
            Some(g) if used + need <= budget => {
                g.push(s.clone());
                used += need;
            }
            _ => {
                groups.push(vec![s.clone()]);
                used = need;
            }
        }
    }
    Ok(groups)
}

/// The WGSL of one pass and its buffers in binding order.
fn group_source(
    group: &[Slot],
    state: &Points,
    source: &Points,
    pick: Pick,
    header: &Buffer,
    source_header: &Buffer,
    ids: &Buffer,
) -> Result<(String, Vec<Buffer>), Error> {
    let mut s = String::from(
        "@group(2) @binding(0) var<uniform> pts_count: vec4u;\n\
         @group(2) @binding(1) var<uniform> pts_source: vec4u;\n\
         @group(2) @binding(2) var<uniform> pts_ids: vec4u;\n",
    );
    let mut buffers = vec![header.clone(), source_header.clone(), ids.clone()];
    let mut binding = 3u32;
    // Source attributes read in this pass, each bound once.
    let mut bound: Vec<(String, AttrType)> = Vec::new();
    for slot in group {
        for (name, ty) in &slot.reads {
            let Some(ty) = ty else { continue };
            if bound.iter().any(|(n, _)| n == name) {
                continue;
            }
            let attr = source.attr(name).ok_or_else(|| Error::new(format!("the source lost `{name}`")))?;
            let _ = writeln!(
                s,
                "@group(2) @binding({binding}) var<storage, read> pts_src_{name}: array<{}>;",
                element(*ty)
            );
            buffers.push(attr.buffer().clone());
            binding += 1;
            bound.push((name.clone(), *ty));
        }
    }
    for (k, slot) in group.iter().enumerate() {
        let attr =
            state.attr(&slot.name).ok_or_else(|| Error::new(format!("the points lost `{}`", slot.name)))?;
        let _ = writeln!(
            s,
            "@group(2) @binding({binding}) var<storage, read_write> pts_out{k}: array<{}>;",
            element(slot.ty)
        );
        buffers.push(attr.buffer().clone());
        binding += 1;
    }
    s.push_str(BORN);
    s.push_str(RANDOM);
    let pick_src = match pick {
        Pick::Random => "pts_rand3(id, 0x51ed270bu).x * f32(n)",
        Pick::Cycle => "",
    };
    s.push_str(
        "\n@compute @workgroup_size(64)\n\
         fn pts_birth(@builtin(global_invocation_id) gid: vec3u) {\n\
         \x20   let born = gid.y * pn.size.x + gid.x;\n\
         \x20   let count = pts_count.x;\n\
         \x20   if gid.x >= pn.size.x || born >= pts_born(count) {\n\
         \x20       return;\n\
         \x20   }\n\
         \x20   let n = pts_sources();\n\
         \x20   let id = pts_ids.x + born;\n",
    );
    if pick_src.is_empty() {
        s.push_str("    let src = born % n;\n");
    } else {
        let _ = writeln!(s, "    let src = min(u32({pick_src}), n - 1u);");
    }
    s.push_str("    let j = count + born;\n");
    // The source point's values, typed; defaults for what the source lacks.
    let mut declared: Vec<&str> = Vec::new();
    for (name, ty) in group.iter().flat_map(|slot| slot.reads.iter()) {
        if declared.contains(&name.as_str()) {
            continue;
        }
        declared.push(name);
        let value = match ty {
            Some(AttrType::Vec3) => format!("pts_src_{name}[src].xyz"),
            Some(_) => format!("pts_src_{name}[src]"),
            None => {
                let d = default_of(name);
                literal(d.map_or(AttrType::F32, |d| d.ty), d.map_or([0.0; 4], |d| d.default))
            }
        };
        let _ = writeln!(s, "    let src_{name} = {value};");
    }
    for (k, slot) in group.iter().enumerate() {
        let value = match &slot.value {
            Value::Copy => match slot.reads.first() {
                Some((name, Some(ty))) if *ty == slot.ty => format!("src_{name}"),
                _ => {
                    let d =
                        default_of(&slot.name).filter(|d| d.ty == slot.ty).map_or([0.0; 4], |d| d.default);
                    literal(slot.ty, d)
                }
            },
            Value::Fixed(v) => literal(slot.ty, *v),
            Value::Expr { wgsl, .. } => format!("{}({wgsl})", slot.ty.wgsl()),
        };
        let stored = match slot.ty {
            AttrType::Vec3 if slot.name == "position" => format!("vec4f({value}, 1.0)"),
            AttrType::Vec3 => format!("vec4f({value}, 0.0)"),
            _ => value,
        };
        let _ = writeln!(s, "    pts_out{k}[j] = {stored};");
    }
    s.push_str("    _ = src;\n    _ = id;\n}\n");
    Ok((s, buffers))
}

/// Raises the count by the births and moves the counter of ids on.
const BUMP: &str = "
@group(2) @binding(0) var<storage, read_write> pts_header: array<u32>;
@group(2) @binding(1) var<uniform> pts_source: vec4u;
@group(2) @binding(2) var<storage, read_write> pts_next: array<u32>;

@compute @workgroup_size(64)
fn pts_bump(@builtin(global_invocation_id) gid: vec3u) {
    if gid.x != 0u {
        return;
    }
    let count = pts_header[0];
    let born = pts_born(count);
    pts_header[0] = count + born;
    pts_next[0] = pts_next[0] + born;
}
";

#[cfg(test)]
mod tests;
