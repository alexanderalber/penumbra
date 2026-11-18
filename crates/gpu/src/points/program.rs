//! Compute programs over the attributes of points: the one path every `pts` node that changes
//! attributes takes, whether its WGSL comes from a file (`pts.shader`), a typed expression or
//! the node itself.
//!
//! A program is WGSL with a function `fn main()` that runs once per point. Each attribute it
//! uses is a variable of that name, holding the point's value: `position`, `color`, `scale`.
//! What `main` leaves in an attribute it writes becomes the point's new value:
//!
//! ```wgsl
//! fn main() {
//!     position.z += sin(position.x * 6.283 + time) * params.height;
//! }
//! ```
//!
//! The program declares the attributes it uses ([`Decl`]): read only, written starting from the
//! input's value, or set starting from the default. [`generate`] resolves them against the
//! input's attributes ([`Shape`]) and writes the rest of the shader around the source: one
//! storage buffer per attribute read and one per attribute written, the variables, the helpers
//! and the entry point. The source comes first, so the compiler's line numbers are the lines of
//! the file.
//!
//! What `main` sees besides the attributes:
//!
//! ```wgsl
//! index: u32        // the point's place, from 0
//! count: u32        // how many points there are
//! index01: f32      // index / (count - 1): 0 at the first point, 1 at the last
//! time: f32         // the show time in seconds
//! dt: f32           // the length of one frame in seconds, 1 / the frame rate
//! frame: i32        // the show's frame
//! fn rand(k: u32) -> f32     // a random number in 0..1, the same for this point and k every time
//! fn rand3(k: u32) -> vec3f  // three of them
//! params.<name>              // the program's params, as in every shader
//! pic0, pic1, pn_clamp       // pictures, sampled with textureSampleLevel(pic0, pn_clamp, uv, 0.0)
//! pn_noise, pn_value_noise, pn_hash   // noise and hashes, as in every shader
//! ```
//!
//! A missing attribute that is read holds its default ([`super::default_of`], or the declared
//! one). No program writes into its input's buffers: each written attribute gets a buffer of
//! the node's ([`Outputs`]); the others pass on as they are.
//!
//! The entry point runs over a grid of up to 65536 points per row, so counts beyond what one
//! row of workgroups covers work. The count comes from one function, `pn_points_count`, so a
//! count kept on the GPU replaces one line.

use std::fmt::Write as _;

use penumbra_core::declare;
use penumbra_core::{Error, ParamSpec, Value};

use super::{Attr, AttrType, Points, canonical, default_of, valid_name};
use crate::compute::{Compute, Dispatch, Out, PRELUDE_STORAGE_BUFFERS, Step};
use crate::{Buffer, BufferRing, Gpu, Tex};

/// The entry point a program gets.
pub const ENTRY: &str = "pn_points";

/// The function a program writes, run once per point.
pub const MAIN: &str = "main";

/// Names the program gives `main` besides the attributes; no attribute can take them.
pub const HELPERS: &[&str] = &["index", "count", "index01", "time", "dt", "frame", "rand", "rand3", MAIN];

/// Params every program gets: the count and the time step.
const COUNT_PARAM: &str = "pn_count";
const DT_PARAM: &str = "pn_dt";

/// Points per row of the dispatch grid: 1024 workgroups of 64.
const ROW: u32 = 1 << 16;

/// How a program uses an attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read only.
    Read,
    /// Read and written: starts as the input's value (the default where the input lacks it).
    Write,
    /// Written only: starts as the default, the input's value is not read.
    Set,
}

/// An attribute a program uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    /// Its name; a TouchDesigner name means the attribute it stands for.
    pub name: String,
    /// Its type; `None` takes the input's, else the standard one.
    pub ty: Option<AttrType>,
    /// The value a point without it has; `None` takes the standard one, else zero.
    pub default: Option<[f32; 4]>,
    pub access: Access,
}

impl Decl {
    fn new(name: &str, access: Access) -> Self {
        Self { name: canonical(name).to_string(), ty: None, default: None, access }
    }

    pub fn read(name: &str) -> Self {
        Self::new(name, Access::Read)
    }

    pub fn write(name: &str) -> Self {
        Self::new(name, Access::Write)
    }

    pub fn set(name: &str) -> Self {
        Self::new(name, Access::Set)
    }

    pub fn ty(self, ty: AttrType) -> Self {
        Self { ty: Some(ty), ..self }
    }

    pub fn default(self, value: [f32; 4]) -> Self {
        Self { default: Some(value), ..self }
    }
}

/// What a program is made of before it meets its input.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    /// Names the program in errors: the file name, or the node kind.
    pub label: String,
    /// WGSL with `fn main()`.
    pub source: String,
    pub decls: Vec<Decl>,
    /// The program's params, read as `params.<name>`.
    pub params: Vec<ParamSpec>,
}

/// The attributes a set of points has, by name and type: what [`generate`] resolves against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape(Vec<(String, AttrType)>);

impl Shape {
    /// Attributes by name (penumbra's) and type, such as a node's usual input has them, for
    /// compiling its program before the first cook.
    pub fn new(attrs: &[(&str, AttrType)]) -> Self {
        Self(attrs.iter().map(|(n, t)| (canonical(n).to_string(), *t)).collect())
    }

    pub fn of(points: &Points) -> Self {
        Self(points.attrs().iter().map(|a| (a.name().to_string(), a.ty())).collect())
    }

    fn get(&self, name: &str) -> Option<AttrType> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, t)| *t)
    }
}

/// One attribute of a generated program, resolved.
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    name: String,
    ty: AttrType,
    default: [f32; 4],
    /// The type the input had when the program was made; `None` when it lacked the attribute.
    seen: Option<AttrType>,
    /// Whether the program depends on `seen`: it reads the input's value, or took its type
    /// from the input.
    depends: bool,
    /// Bindings in `@group(2)`.
    input: Option<u32>,
    output: Option<u32>,
}

/// A program's whole source, ready to compile, and how its buffers bind.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    label: String,
    source: String,
    params: Vec<ParamSpec>,
    slots: Vec<Slot>,
}

impl Generated {
    /// The WGSL the compiler gets, without the prelude.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// The params the program reads, the ones every program gets included.
    pub fn params(&self) -> &[ParamSpec] {
        &self.params
    }

    /// Storage buffers the program binds.
    pub fn bindings(&self) -> u32 {
        self.slots.iter().map(|s| u32::from(s.input.is_some()) + u32::from(s.output.is_some())).sum()
    }

    /// The attributes the program writes.
    pub fn writes(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().filter(|s| s.output.is_some()).map(|s| s.name.as_str())
    }

    /// Whether `input` has the attributes the program was made for, with the same types.
    pub fn fits(&self, input: &Points) -> bool {
        self.slots.iter().filter(|s| s.depends).all(|s| input.attr(&s.name).map(Attr::ty) == s.seen)
    }
}

/// Storage buffers a program may bind on `gpu`: the device's limit per shader stage, less what
/// the compute prelude binds itself.
pub fn storage_budget(gpu: &Gpu) -> u32 {
    gpu.device().limits().max_storage_buffers_per_shader_stage.saturating_sub(PRELUDE_STORAGE_BUFFERS)
}

/// The globals every program gets from the compute prelude, besides the [`HELPERS`].
const PRELUDE_NAMES: &[&str] =
    &["params", "pn", "pic0", "pic1", "vol0", "vol1", "vol2", "vol3", "vol_out", "pic_out", "Params"];

/// Most declarations one program has: a program reads and writes at most every attribute a set
/// of points holds.
pub const MAX_DECLS: usize = 2 * super::MAX_ATTRS;

/// Whether `name` is one of WGSL's own: a function, a type or one of its short names
/// (`vec3f`, `mat4x4f`), an enumerant.
fn wgsl_builtin(name: &str) -> bool {
    if naga::keywords::wgsl::BUILTIN_IDENTIFIERS.contains(&name) {
        return true;
    }
    let sized = |rest: &str| matches!(rest.as_bytes(), [b'2'..=b'4', b'f' | b'h' | b'i' | b'u']);
    let matrix = |rest: &str| matches!(rest.as_bytes(), [b'2'..=b'4', b'x', b'2'..=b'4', b'f' | b'h']);
    name.strip_prefix("vec").is_some_and(sized) || name.strip_prefix("mat").is_some_and(matrix)
}

/// Whether `name` can name an attribute in a program, where it becomes a variable of that name:
/// an identifier, not a word WGSL reserves or has a function or type of, not a name every
/// program has, not starting with `pn_`. The one check every attribute name in a program goes
/// through, whether typed into a node or declared in a file.
pub fn check_name(name: &str) -> Result<(), Error> {
    let refuse = |why: String| Err(Error::new(format!("{why}; choose another name")));
    if !valid_name(name) || name == "_" || name.starts_with("__") {
        return refuse(format!(
            "`{name}` cannot name an attribute: letters, digits and _ only, up to 64, not starting with a digit or two _"
        ));
    }
    if naga::keywords::wgsl::RESERVED.contains(&name) {
        return refuse(format!("`{name}` is a reserved word in WGSL"));
    }
    if wgsl_builtin(name) {
        return refuse(format!("`{name}` is a WGSL function or type"));
    }
    if HELPERS.contains(&name) || PRELUDE_NAMES.contains(&name) {
        return refuse(format!(
            "`{name}` is a name every program has ({}, {})",
            HELPERS.join(", "),
            PRELUDE_NAMES.join(", ")
        ));
    }
    if name.starts_with("pn_") {
        return refuse(format!("`{name}`: names starting with pn_ are penumbra's own"));
    }
    Ok(())
}

/// The whole program for `spec` over points of `shape`. `Err` with a readable reason for a name
/// that cannot be an attribute, a type the program and the input disagree on, an attribute of
/// unknown type, a source without `fn main`, and for more storage buffers than `budget`.
pub fn generate(spec: &Spec, shape: &Shape, budget: u32) -> Result<Generated, Error> {
    let label = spec.label.as_str();
    if spec.decls.len() > MAX_DECLS {
        return Err(Error::new(format!(
            "{label}: {} declarations are more than the {MAX_DECLS} a program can use",
            spec.decls.len()
        )));
    }
    if !has_main(&spec.source) {
        return Err(Error::new(format!(
            "{label}: no `fn main()`; the program is a function `fn main() {{ ... }}` that runs once per point"
        )));
    }
    let decls = merge(&spec.decls).map_err(|e| e.context(label))?;
    let mut slots = Vec::with_capacity(decls.len());
    let mut binding = 0u32;
    for d in &decls {
        let seen = shape.get(&d.name);
        let standard = default_of(&d.name);
        // An attribute only set takes the standard type before the input's, so the program
        // does not depend on what the input holds.
        let inferred = match d.access {
            Access::Set => standard.map(|s| s.ty).or(seen),
            _ => seen.or(standard.map(|s| s.ty)),
        };
        let depends = d.access != Access::Set || (d.ty.is_none() && standard.is_none());
        let ty = d.ty.or(inferred).ok_or_else(|| {
            Error::new(format!(
                "{label}: `{}` is no attribute of the input and penumbra does not know its type; declare it (float, vec2, vec3, vec4, int or uint)",
                d.name
            ))
        })?;
        let reads = d.access != Access::Set && seen.is_some();
        if reads && seen != Some(ty) {
            return Err(Error::new(format!(
                "{label}: the input's `{}` is {}, the program reads it as {}",
                d.name,
                seen.map_or("", AttrType::wgsl),
                ty.wgsl()
            )));
        }
        let default = d.default.or(standard.filter(|s| s.ty == ty).map(|s| s.default)).unwrap_or([0.0; 4]);
        if default.iter().any(|v| !v.is_finite()) {
            return Err(Error::new(format!("{label}: the default of `{}` is not a finite number", d.name)));
        }
        let input = reads.then(|| {
            binding += 1;
            binding - 1
        });
        let output = (d.access != Access::Read).then(|| {
            binding += 1;
            binding - 1
        });
        slots.push(Slot { name: d.name.clone(), ty, default, seen, depends, input, output });
    }
    if binding > budget {
        let reads = slots.iter().filter(|s| s.input.is_some()).count();
        let writes = slots.iter().filter(|s| s.output.is_some()).count();
        return Err(Error::new(format!(
            "{label}: reading {reads} attributes and writing {writes} takes {binding} storage buffers; this GPU gives a program {budget}. Split the work over two nodes"
        )));
    }
    let mut params = spec.params.clone();
    for p in &params {
        if p.name.starts_with("pn_") {
            return Err(Error::new(format!(
                "{label}: param `{}`: names starting with pn_ are penumbra's own",
                p.name
            )));
        }
    }
    params.push(ParamSpec::int(COUNT_PARAM, 0));
    params.push(ParamSpec::float(DT_PARAM, 0.0));
    let source = format!("{}\n{}", spec.source, around(&slots));
    Ok(Generated { label: label.to_string(), source, params, slots })
}

/// Whether `source` defines `fn main`.
fn has_main(source: &str) -> bool {
    let mut words =
        source.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty());
    let mut last = "";
    for w in words.by_ref() {
        if last == "fn" && w == MAIN {
            return true;
        }
        last = w;
    }
    false
}

/// One declaration per attribute: reading and writing one makes it written from the input's
/// value; types and defaults must agree.
fn merge(decls: &[Decl]) -> Result<Vec<Decl>, Error> {
    let mut out: Vec<Decl> = Vec::new();
    for d in decls {
        let name = canonical(&d.name);
        check_name(name)?;
        match out.iter_mut().find(|o| o.name == name) {
            None => out.push(Decl { name: name.to_string(), ..d.clone() }),
            Some(o) => {
                if let (Some(a), Some(b)) = (o.ty, d.ty)
                    && a != b
                {
                    return Err(Error::new(format!(
                        "`{name}` is declared as {} and as {}",
                        a.wgsl(),
                        b.wgsl()
                    )));
                }
                o.ty = o.ty.or(d.ty);
                o.default = o.default.or(d.default);
                if o.access != d.access {
                    o.access = Access::Write;
                }
            }
        }
    }
    Ok(out)
}

/// A WGSL literal of `ty` from the first components of `v`.
fn literal(ty: AttrType, v: [f32; 4]) -> String {
    let f = |x: f32| format!("{x:?}");
    match ty {
        AttrType::F32 => format!("f32({})", f(v[0])),
        AttrType::Vec2 => format!("vec2f({}, {})", f(v[0]), f(v[1])),
        AttrType::Vec3 => format!("vec3f({}, {}, {})", f(v[0]), f(v[1]), f(v[2])),
        AttrType::Vec4 => format!("vec4f({}, {}, {}, {})", f(v[0]), f(v[1]), f(v[2]), f(v[3])),
        AttrType::U32 => format!("{}u", v[0].max(0.0) as u32),
        // WGSL has no literal for the lowest i32: the minus applies to 2147483648i, out of range.
        AttrType::I32 => match v[0] as i32 {
            i32::MIN => "(-2147483647i - 1i)".to_string(),
            i => format!("{i}i"),
        },
    }
}

/// The element type of an attribute's buffer: a `vec3f` takes 16 bytes as a `vec4f` does, and
/// is bound as one so the fourth float is written too.
fn element(ty: AttrType) -> &'static str {
    match ty {
        AttrType::Vec3 => "vec4f",
        ty => ty.wgsl(),
    }
}

/// Everything around the program's source: buffers, variables, helpers, the entry point.
fn around(slots: &[Slot]) -> String {
    let mut s = String::from("// ---- penumbra points program ----\n");
    for slot in slots {
        let (name, el) = (&slot.name, element(slot.ty));
        if let Some(b) = slot.input {
            let _ = writeln!(s, "@group(2) @binding({b}) var<storage, read> pn_in_{name}: array<{el}>;");
        }
        if let Some(b) = slot.output {
            let _ =
                writeln!(s, "@group(2) @binding({b}) var<storage, read_write> pn_out_{name}: array<{el}>;");
        }
        let _ = writeln!(s, "var<private> {name}: {};", slot.ty.wgsl());
    }
    s.push_str(HELPERS_WGSL);
    s.push_str(
        "@compute @workgroup_size(64)\n\
         fn pn_points(@builtin(global_invocation_id) pn_id: vec3u) {\n\
         \x20   let pn_i = pn_id.y * pn.size.x + pn_id.x;\n\
         \x20   let pn_n = pn_points_count();\n\
         \x20   if pn_id.x >= pn.size.x || pn_i >= pn_n {\n\
         \x20       return;\n\
         \x20   }\n\
         \x20   index = pn_i;\n\
         \x20   count = pn_n;\n\
         \x20   index01 = select(0.0, f32(pn_i) / f32(max(pn_n, 2u) - 1u), pn_n > 1u);\n\
         \x20   time = pn_time.seconds;\n\
         \x20   frame = i32(pn_time.frame);\n\
         \x20   dt = params.pn_dt;\n",
    );
    for slot in slots {
        let name = &slot.name;
        let value = match (slot.input, slot.ty) {
            (Some(_), AttrType::Vec3) => format!("pn_in_{name}[pn_i].xyz"),
            (Some(_), _) => format!("pn_in_{name}[pn_i]"),
            (None, ty) => literal(ty, slot.default),
        };
        let _ = writeln!(s, "    {name} = {value};");
    }
    s.push_str("    main();\n");
    for slot in slots.iter().filter(|s| s.output.is_some()) {
        let name = &slot.name;
        let value = match slot.ty {
            // The fourth float of a position is 1, of any other vector 0.
            AttrType::Vec3 if name == "position" => format!("vec4f({name}, 1.0)"),
            AttrType::Vec3 => format!("vec4f({name}, 0.0)"),
            _ => name.clone(),
        };
        let _ = writeln!(s, "    pn_out_{name}[pn_i] = {value};");
    }
    s.push_str("}\n");
    s
}

const HELPERS_WGSL: &str = "var<private> index: u32;
var<private> count: u32;
var<private> index01: f32;
var<private> time: f32;
var<private> dt: f32;
var<private> frame: i32;

fn pn_points_count() -> u32 {
    return u32(max(params.pn_count, 0));
}

fn rand3(k: u32) -> vec3f {
    // 24 bits each, every value exactly a float below 1.
    return vec3f(pn_hash(vec3u(index, k, 0x2c1b3c6du)) >> vec3u(8u)) * (1.0 / 16777216.0);
}

fn rand(k: u32) -> f32 {
    return rand3(k).x;
}

";

/// The size of the dispatch grid over `n` points: rows of [`ROW`].
fn grid(n: u32) -> (u32, u32) {
    let w = n.clamp(1, ROW);
    (w, n.div_ceil(w))
}

/// The buffers of the attributes a node writes, one ring per name, reused across cooks.
#[derive(Default)]
pub struct Outputs {
    rings: Vec<(String, BufferRing)>,
}

impl Outputs {
    /// A buffer of `bytes` for the attribute `name` that nobody else holds.
    fn next(&mut self, gpu: &Gpu, name: &str, bytes: u64) -> Result<Buffer, Error> {
        let label = format!("pts {name}");
        if let Some((_, ring)) = self.rings.iter_mut().find(|(n, _)| n == name) {
            return ring.next(gpu, bytes, &label);
        }
        let mut ring = BufferRing::new();
        let buffer = ring.next(gpu, bytes, &label)?;
        self.rings.push((name.to_string(), ring));
        Ok(buffer)
    }

    /// Bytes on the GPU of the buffers the rings keep.
    pub fn bytes(&self) -> u64 {
        self.rings.iter().map(|(_, r)| r.bytes()).sum()
    }

    /// Forgets the rings of every attribute `generated` does not write: the buffers of a name
    /// no program writes any more go once nobody downstream holds them.
    fn keep_only(&mut self, generated: &Generated) {
        self.rings.retain(|(n, _)| generated.writes().any(|k| k == n));
    }

    /// Forgets the buffers nobody else holds.
    pub fn trim(&mut self) {
        for (_, r) in &mut self.rings {
            r.trim();
        }
        self.rings.retain(|(_, r)| r.bytes() > 0);
    }
}

/// What one run of a program gets besides its input.
pub struct Run<'a> {
    pub seconds: f64,
    pub frame: i64,
    /// The length of one frame in seconds (1 / the frame rate), so a run repeats exactly.
    pub dt: f64,
    /// The value of a declared param; `None` means its default.
    pub param: &'a dyn Fn(&str) -> Option<Value>,
    /// `pic0` and `pic1`, `None` (or missing) for transparent black.
    pub pictures: &'a [Option<&'a Tex>],
}

/// A compiled program over points.
pub struct Program {
    compute: std::sync::Arc<Compute>,
    generated: Generated,
}

impl Program {
    /// Compiles `generated` anew: for a program that changes, such as a file being edited.
    pub fn compile(gpu: &Gpu, generated: Generated) -> Result<Self, Error> {
        let compute = Compute::compile(gpu, &generated.label, &generated.source, &generated.params)?;
        Ok(Self { compute: std::sync::Arc::new(compute), generated })
    }

    /// Compiles `generated` once per process ([`Gpu::cached_compute`]): for a node's own program.
    pub fn cached(gpu: &Gpu, generated: Generated) -> Result<Self, Error> {
        let compute = gpu.cached_compute(&generated.label, &generated.source, &generated.params)?;
        Ok(Self { compute, generated })
    }

    /// The program [`Program::cached`] compiled for `generated` before, at no cost: `None`
    /// where it would have to compile.
    pub fn compiled(gpu: &Gpu, generated: &Generated) -> Option<Self> {
        let compute = gpu.compiled_compute(&generated.source, &generated.params)?;
        Some(Self { compute, generated: generated.clone() })
    }

    pub fn generated(&self) -> &Generated {
        &self.generated
    }

    /// Whether `main` reads the global `name` (`time`, `pic0`), itself or through a function.
    pub fn uses(&self, name: &str) -> bool {
        self.compute.uses(MAIN, name)
    }

    /// Whether `main` reads the time in any form.
    pub fn uses_time(&self) -> bool {
        ["time", "dt", "frame", "pn_time"].iter().any(|g| self.uses(g))
    }

    /// Whether `main` reads picture `index` (`pic0`, `pic1`).
    pub fn uses_picture(&self, index: usize) -> bool {
        self.uses(&format!("pic{index}"))
    }

    /// Runs the program over `input`: the written attributes in new buffers from `outputs`,
    /// every other attribute passed on. Moving `position` drops the bounds, so a node that knows
    /// better sets them after. `Err` when `input` lacks attributes or types the program was made
    /// for ([`Generated::fits`]) and for any error of the dispatch.
    pub fn run(
        &self,
        gpu: &Gpu,
        input: &Points,
        outputs: &mut Outputs,
        run: &Run<'_>,
    ) -> Result<Points, Error> {
        let g = &self.generated;
        if !g.fits(input) {
            return Err(Error::new(format!("{}: the program was made for other attributes", g.label)));
        }
        outputs.keep_only(g);
        let n = input.count().at_most();
        // Without points the written attributes are there all the same, so what comes out has
        // the same attributes whatever the count: a buffer cannot be empty, so it holds one.
        let capacity = u64::from(input.capacity().max(1));
        let mut bound: Vec<(u32, Buffer)> = Vec::with_capacity(g.slots.len() * 2);
        let mut written: Vec<Attr> = Vec::new();
        for slot in &g.slots {
            if let Some(b) = slot.input {
                let attr = input
                    .attr(&slot.name)
                    .ok_or_else(|| Error::new(format!("{}: the input lost `{}`", g.label, slot.name)))?;
                bound.push((b, attr.buffer().clone()));
            }
            if let Some(b) = slot.output {
                let buffer = outputs.next(gpu, &slot.name, capacity * slot.ty.stride())?;
                written.push(Attr::new(&slot.name, slot.ty, buffer.clone())?);
                bound.push((b, buffer));
            }
        }
        bound.sort_by_key(|(b, _)| *b);
        let buffers: Vec<&Buffer> = bound.iter().map(|(_, b)| b).collect();
        let dt = run.dt;
        let param = |name: &str| match name {
            COUNT_PARAM => Some(Value::Int(i64::from(n))),
            DT_PARAM => Some(Value::Float(dt)),
            _ => (run.param)(name),
        };
        let dispatch = Dispatch {
            volumes: &[],
            pictures: run.pictures,
            data: None,
            seconds: run.seconds,
            frame: run.frame,
            param: &param,
        };
        if n > 0 {
            let (w, h) = grid(n);
            self.compute.run_with(
                gpu,
                &buffers,
                &[Step { entry: ENTRY, out: Out::grid(w, h), input: &dispatch }],
            )?;
        }
        let mut out = input.clone();
        // New positions drop the bounds ([`Points::with_attr`]).
        for attr in written {
            out = out.with_attr(attr)?;
        }
        Ok(out)
    }
}

/// The header words of a `pts.shader` file beyond `@param` and `@group`.
pub const READ: &str = "@read";
pub const WRITE: &str = "@write";

/// What a `pts.shader` file declares: its params and the attributes it uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub params: Vec<ParamSpec>,
    pub decls: Vec<Decl>,
}

/// Parses the header of a points program: `// @param` and `// @group` lines as a shader has
/// them ([`crate::header`]), and
///
/// ```wgsl
/// // @read <name> [<type>] [= <default>]
/// // @write <name> [<type>] [= <default>]
/// ```
///
/// `@read` reads an attribute, `@write` reads and writes it (it starts as the input's value).
/// The type (`float`, `vec2`, `vec3`, `vec4`, `int`, `uint`) is needed for a name penumbra does
/// not know that the input may lack; the default is a number or a list (`[1, 0, 0, 1]`).
/// Errors name the line.
pub fn parse_header(source: &str) -> Result<Header, Error> {
    let shader = crate::header::parse(source)?;
    let mut decls = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let Some((word, args)) = declare::split(line, true) else { continue };
        let ctx = || format!("line {}", i + 1);
        let access = match word {
            READ => Access::Read,
            WRITE => Access::Write,
            _ => {
                if let Some(meant) = declare::misspelt(word, &[READ, WRITE]) {
                    return Err(Error::new(format!(
                        "`{word}` looks like a misspelt `{meant}`; a points program declares @read, @write, @param and @group"
                    ))
                    .context(ctx()));
                }
                continue;
            }
        };
        if decls.len() == MAX_DECLS {
            return Err(Error::new(format!(
                "more than {MAX_DECLS} declarations of attributes; a program can use no more"
            ))
            .context(ctx()));
        }
        decls.push(decl(args, access).map_err(|e| e.context(ctx()))?);
    }
    for p in &shader.params {
        if p.name.starts_with("pn_") {
            return Err(Error::new(format!(
                "param `{}`: names starting with pn_ are penumbra's own",
                p.name
            )));
        }
    }
    Ok(Header { params: shader.params, decls })
}

/// `<name> [<type>] [= <default>] [-- doc]`.
fn decl(args: &str, access: Access) -> Result<Decl, Error> {
    let args = args.split_once("--").map_or(args, |(a, _)| a);
    let (head, default) = match args.split_once('=') {
        Some((h, d)) => (h, Some(d.trim())),
        None => (args, None),
    };
    let mut words = head.split_whitespace();
    let (Some(name), ty, None) = (words.next(), words.next(), words.next()) else {
        return Err(Error::new("expected `<name> [<type>] [= <default>]`, such as `@write heat float = 0`"));
    };
    let name = canonical(name);
    check_name(name)?;
    let mut d = Decl::new(name, access);
    if let Some(t) = ty {
        d.ty = Some(type_word(t)?);
    }
    if let Some(text) = default {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|_| Error::new(format!("the default `{text}` is no number or list of numbers")))?;
        let numbers: Vec<f64> = match &value {
            serde_json::Value::Number(n) => n.as_f64().into_iter().collect(),
            serde_json::Value::Array(a) => a.iter().filter_map(serde_json::Value::as_f64).collect(),
            _ => Vec::new(),
        };
        let wanted = if let serde_json::Value::Array(a) = &value { a.len() } else { 1 };
        if numbers.is_empty() || numbers.len() != wanted || numbers.len() > 4 {
            return Err(Error::new(format!("the default `{text}` is no number or list of up to 4 numbers")));
        }
        let mut v = [0.0f32; 4];
        for (slot, n) in v.iter_mut().zip(&numbers) {
            *slot = *n as f32;
        }
        // One number fills every component.
        if numbers.len() == 1 {
            v = [v[0]; 4];
        }
        d.default = Some(v);
    }
    Ok(d)
}

/// The attribute type a word in a header names.
pub fn type_word(word: &str) -> Result<AttrType, Error> {
    Ok(match word {
        "float" | "f32" => AttrType::F32,
        "vec2" | "vec2f" => AttrType::Vec2,
        "vec3" | "vec3f" => AttrType::Vec3,
        "vec4" | "vec4f" | "color" => AttrType::Vec4,
        "int" | "i32" => AttrType::I32,
        "uint" | "u32" => AttrType::U32,
        other => {
            return Err(Error::new(format!(
                "unknown type `{other}`; types: float, vec2, vec3, vec4, int, uint"
            )));
        }
    })
}

#[cfg(test)]
mod tests;
