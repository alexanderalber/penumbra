//! What the `pts` nodes that run a program over points share: the input, the time a program
//! sees, a node's own program compiled once per process, a program that changes compiled off
//! the cook thread, and the WGSL that combines a value with an attribute.
//!
//! A built-in node is a WGSL `fn main()`, the attributes it uses and its params
//! ([`penumbra_gpu::points::program::Spec`]); [`Own::run`] does the rest.

use std::sync::Arc;

use penumbra_core::{ParamSpec, Value};
use penumbra_gpu::points::program::{self, Generated, Outputs, Program, Run, Shape, Spec};
use penumbra_gpu::points::{AttrType, Points};
use penumbra_gpu::{Gpu, Tex};
use penumbra_graph::{AsyncSlot, CookContext, CookError, JobStatus};

/// The attributes the points of `pts.generate` usually have: the input a node's program is
/// compiled for before its first cook.
pub(crate) fn generated_shape() -> Shape {
    Shape::new(&[("position", AttrType::Vec3), ("normal", AttrType::Vec3), ("uv", AttrType::Vec2)])
}

/// The points on input 0, or why there are none.
pub(crate) fn input<'a>(cx: &'a CookContext<'_>) -> Result<&'a Points, CookError> {
    match cx.input_as::<Points>(0) {
        Some(p) => Ok(p),
        None if cx.def().inputs.first().is_some_and(Option::is_some) => Err(CookError::Pending),
        None => Err(CookError::failed("connect points to `in`")),
    }
}

/// The show time, frame and the length of a frame in seconds, as a program sees them; zeros
/// without reading the clock when the program does not use them.
pub(crate) fn time(cx: &CookContext<'_>, timed: bool) -> (f64, i64, f64) {
    if !timed {
        return (0.0, 0, 0.0);
    }
    let t = cx.time();
    let rate = t.rate.as_f64();
    let dt = if rate > 0.0 { 1.0 / rate } else { 0.0 };
    (t.seconds(), t.frame, dt)
}

/// A built-in node's program: compiled once per process for each set of input attributes it
/// meets, and the buffers it writes.
#[derive(Default)]
pub(crate) struct Own {
    program: Option<Program>,
    outputs: Outputs,
}

impl Own {
    /// Runs `spec` over `input`. `timed`: the program reads the time. `param`: the values of
    /// the spec's params.
    pub(crate) fn run(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &Gpu,
        spec: &Spec,
        input: &Points,
        timed: bool,
        param: &dyn Fn(&str) -> Option<Value>,
    ) -> Result<Points, CookError> {
        let generated = program::generate(spec, &Shape::of(input), program::storage_budget(gpu))?;
        let program = match self.program.take() {
            Some(p) if *p.generated() == generated => p,
            _ => Program::cached(gpu, generated)?,
        };
        let (seconds, frame, dt) = time(cx, timed);
        let run = Run { seconds, frame, dt, param, pictures: &[] };
        let out = program.run(gpu, input, &mut self.outputs, &run);
        self.program = Some(program);
        Ok(out?)
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.outputs.bytes()
    }

    pub(crate) fn trim(&mut self) {
        self.outputs.trim();
    }
}

/// A program that changes while the show runs (a file being edited, a typed expression):
/// compiled off the cook thread, the last one that compiled kept until the next does.
#[derive(Default)]
pub(crate) struct Compiling {
    slot: AsyncSlot<Arc<Program>>,
    current: Option<Arc<Program>>,
    /// What the job in flight, or the last one, compiles.
    asked: Option<Generated>,
    outputs: Outputs,
}

impl Compiling {
    /// Takes a compile that finished.
    pub(crate) fn poll(&mut self) {
        if self.slot.poll() {
            self.current = self.slot.latest().cloned();
        }
    }

    /// Starts compiling `generated`, unless that is what the last compile was given.
    pub(crate) fn want(&mut self, cx: &CookContext<'_>, gpu: &Gpu, generated: Generated) {
        if self.asked.as_ref() == Some(&generated) {
            return;
        }
        self.asked = Some(generated.clone());
        let gpu = gpu.clone();
        self.slot
            .start(cx.spawn(move |_job| Program::compile(&gpu, generated).map(Arc::new).map_err(|e| e.0)));
    }

    /// The last program that compiled, where it fits `input`.
    pub(crate) fn current(&self, input: &Points) -> Option<Arc<Program>> {
        self.current.clone().filter(|p| p.generated().fits(input))
    }

    /// The last program that compiled, whatever it fits.
    pub(crate) fn latest(&self) -> Option<&Program> {
        self.current.as_deref()
    }

    /// The compiler's message for the last compile, while it failed.
    pub(crate) fn error(&self) -> Option<&str> {
        self.slot.error()
    }

    pub(crate) fn status(&self) -> JobStatus {
        self.slot.status()
    }

    /// Runs `program` over `input` into this node's buffers.
    pub(crate) fn run(
        &mut self,
        cx: &CookContext<'_>,
        gpu: &Gpu,
        program: &Program,
        input: &Points,
        param: &dyn Fn(&str) -> Option<Value>,
        pictures: &[Option<&Tex>],
    ) -> Result<Points, CookError> {
        let (seconds, frame, dt) = time(cx, program.uses_time());
        let run = Run { seconds, frame, dt, param, pictures };
        Ok(program.run(gpu, input, &mut self.outputs, &run)?)
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.outputs.bytes()
    }

    pub(crate) fn trim(&mut self) {
        self.outputs.trim();
    }
}

/// The choices of `operation`: how a node's value meets the attribute's.
pub(crate) const OPERATIONS: [&str; 3] = ["add", "set", "multiply"];

/// The `operation` param, with `default` chosen.
pub(crate) fn operation_param(default: &str, what: &str) -> ParamSpec {
    let mut spec = ParamSpec::choice("operation", &OPERATIONS)
        .doc(&format!("How {what} meets the attribute's value"))
        .choice_doc("add", &format!("{what} is added to it"))
        .choice_doc("set", &format!("{what} takes its place"))
        .choice_doc("multiply", &format!("it is multiplied by {what}"));
    spec.default = Value::Text(default.to_string());
    spec
}

/// WGSL that combines a value with an attribute as `params.operation` says, per width.
pub(crate) const COMBINE: &str = "
fn pts_combine1(a: f32, b: f32) -> f32 {
    var r = a + b;
    if params.operation == operation_set {
        r = b;
    } else if params.operation == operation_multiply {
        r = a * b;
    }
    return r;
}

fn pts_combine2(a: vec2f, b: vec2f) -> vec2f {
    return vec2f(pts_combine1(a.x, b.x), pts_combine1(a.y, b.y));
}

fn pts_combine3(a: vec3f, b: vec3f) -> vec3f {
    return vec3f(pts_combine1(a.x, b.x), pts_combine1(a.y, b.y), pts_combine1(a.z, b.z));
}

fn pts_combine4(a: vec4f, b: vec4f) -> vec4f {
    return vec4f(pts_combine3(a.xyz, b.xyz), pts_combine1(a.w, b.w));
}
";

/// The WGSL statement that combines the `vec4f` `value` with the attribute `name` of type `ty`
/// ([`COMBINE`]). With `rgb_only`, a `vec4f` attribute keeps its fourth number. Whole numbers
/// take the value rounded down.
pub(crate) fn combine(name: &str, ty: AttrType, value: &str, rgb_only: bool) -> String {
    match ty {
        AttrType::F32 => format!("{name} = pts_combine1({name}, {value}.x);"),
        AttrType::Vec2 => format!("{name} = pts_combine2({name}, {value}.xy);"),
        AttrType::Vec3 => format!("{name} = pts_combine3({name}, {value}.xyz);"),
        AttrType::Vec4 if rgb_only => {
            format!("{name} = vec4f(pts_combine3({name}.xyz, {value}.xyz), {name}.w);")
        }
        AttrType::Vec4 => format!("{name} = pts_combine4({name}, {value});"),
        AttrType::U32 => format!("{name} = u32(max(floor(pts_combine1(f32({name}), {value}.x)), 0.0));"),
        AttrType::I32 => format!("{name} = i32(floor(pts_combine1(f32({name}), {value}.x)));"),
    }
}

/// The choices of `type`: the type of an attribute the input lacks and penumbra does not know.
pub(crate) const TYPES: [&str; 7] = ["auto", "float", "vec2", "vec3", "vec4", "int", "uint"];

/// The `type` param.
pub(crate) fn type_param() -> ParamSpec {
    ParamSpec::choice("type", &TYPES)
        .more()
        .doc("The type of the attribute when it is new: one the input has keeps its type, and so does one penumbra knows (position, color, scale, ...)")
        .choice_doc("auto", "a number (float) for a name penumbra does not know")
        .choice_doc("float", "one number")
        .choice_doc("vec2", "two numbers")
        .choice_doc("vec3", "three numbers, such as a position or a direction")
        .choice_doc("vec4", "four numbers, such as a colour")
        .choice_doc("int", "a whole number")
        .choice_doc("uint", "a whole number from 0")
}

/// The type the attribute `name` has for a node with `type` `chosen`: the input's, the standard
/// one, the chosen one, a float.
pub(crate) fn attr_type(input: &Points, name: &str, chosen: &str) -> AttrType {
    if let Some(a) = input.attr(name) {
        return a.ty();
    }
    if let Some(s) = penumbra_gpu::points::default_of(name) {
        return s.ty;
    }
    match chosen {
        "auto" => AttrType::F32,
        word => program::type_word(word).unwrap_or(AttrType::F32),
    }
}

/// The attribute name a user typed into param `param`, as penumbra names it.
pub(crate) fn attr_name<'a>(cx: &'a CookContext<'_>, param: &str) -> Result<&'a str, CookError> {
    let name = penumbra_gpu::points::canonical(cx.text(param)?);
    if name.is_empty() {
        return Err(CookError::failed(format!("`{param}` names no attribute")));
    }
    program::check_name(name).map_err(|e| CookError::failed(format!("{param}: {}", e.0)))?;
    Ok(name)
}
