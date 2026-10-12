//! `tex` nodes: 2D textures on the GPU.
//!
//! Every node renders one fullscreen pass ([`penumbra_gpu::Pass`]); the built-ins are WGSL
//! files in this directory with their parameters declared here, the shader node reads both from
//! a file in the project. The data on a `tex` connection is a [`Tex`]: linear light,
//! premultiplied alpha, origin top left. Without a GPU (headless tools, tests of other crates)
//! these nodes fail with a clear error instead of producing anything.

pub mod blobtrack;
mod blur;
mod cache;
mod denoise;
mod filters;
mod flow;
mod image;
mod layer;
pub mod mapper;
mod opticalflow;
mod ramp;
pub mod render;
mod shader;
pub mod shapes;
mod text;
mod timemachine;
mod video;
pub mod videoin;

pub use text::{DEFAULT_FONT, TEXT, families as font_families};
pub use video::VIDEO;
pub use videoin::VIDEO_IN;

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::{Gpu, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, Density, FnNodeType, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

/// Largest width or height a resolution parameter accepts; the device may allow less.
pub const MAX_SIDE: f64 = 16384.0;

/// How a built-in node picks its resolution (fixed).
#[derive(Clone, Copy)]
enum Size {
    /// The `width` and `height` parameters.
    Params,
    /// The first connected input's.
    Input,
    /// The `width` and `height` parameters, once an input is there (a fit).
    Own,
    /// From the params and the first connected input's size (a flop, a resize).
    Custom(SizeFn),
}

/// A resolution from the params and the input's resolution.
type SizeFn = fn(&CookContext<'_>, (u32, u32)) -> Result<(u32, u32), CookError>;

/// Edge modes of `extend` params, in the order `pn_extend` numbers them.
const EXTEND: [&str; 4] = ["hold", "zero", "repeat", "mirror"];

/// A built-in pass node.
struct Builtin {
    desc: NodeDesc,
    source: &'static str,
    size: Size,
    /// Whether an instance reads the clock, from its parameters (`None`: never).
    animated: Option<fn(&CookContext<'_>) -> bool>,
    /// How the inputs relate to the output ([`Operator::density`]).
    density: DensityFn,
}

/// [`Operator::density`] of a built-in: from the params, the input's index and the sizes of
/// the last cook.
type DensityFn = fn(&NodeDef, &NodeDesc, usize, Option<Sizes>) -> Density;

/// The first input's size and the output's, as last cooked.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sizes {
    input: (u32, u32),
    output: (u32, u32),
}

/// The same frame for every input.
fn same(_: &NodeDef, _: &NodeDesc, _: usize, _: Option<Sizes>) -> Density {
    Density::SAME
}

fn builtins() -> Vec<Builtin> {
    let mut all = core_builtins();
    all.extend(filters::builtins());
    all
}

fn core_builtins() -> Vec<Builtin> {
    vec![
        Builtin {
            desc: sized(NodeDesc::new("tex.constant", "One colour everywhere"))
                .param(ParamSpec::color("color", [0.0, 0.0, 0.0, 1.0]).doc("Linear RGBA, straight alpha")),
            source: include_str!("constant.wgsl"),
            size: Size::Params,
            animated: None,
            density: same,
        },
        Builtin {
            desc: sized(NodeDesc::new(
                "tex.noise",
                "Fractal noise: offset + amplitude * noise, the same on every GPU",
            ))
            .param(
                ParamSpec::choice("basis", &["gradient", "value"])
                    .doc("Smooth gradient noise or blockier value noise"),
            )
            .param(ParamSpec::int("seed", 0).range(0.0, 100.0))
            .param(
                ParamSpec::float("frequency", 4.0)
                    .range(0.1, 100.0)
                    .curve(Curve::Log)
                    .doc("Cells per image height"),
            )
            .param(ParamSpec::int("octaves", 3).clamp(1.0, 8.0))
            .param(
                ParamSpec::float("roughness", 0.5)
                    .range(0.0, 1.0)
                    .doc("Amplitude from one octave to the next"),
            )
            .param(
                ParamSpec::float("lacunarity", 2.0)
                    .range(1.0, 4.0)
                    .more()
                    .doc("Frequency from one octave to the next"),
            )
            .param(ParamSpec::float("amplitude", 0.5).range(0.0, 2.0))
            .param(ParamSpec::float("offset", 0.5).range(-1.0, 1.0))
            .param(
                ParamSpec::float("speed", 0.0)
                    .range(0.0, 4.0)
                    .doc("Movement through the noise per second; 0 is still"),
            )
            .param(ParamSpec::vec3("translate", [0.0; 3]).more().doc("Position in the noise"))
            .param(ParamSpec::bool("color", false).doc("Three independent channels instead of grey"))
            .param(ParamSpec::float("alpha", 1.0).clamp(0.0, 1.0).more()),
            source: include_str!("noise.wgsl"),
            size: Size::Params,
            animated: Some(|cx| cx.float("speed").is_ok_and(|s| s != 0.0)),
            density: same,
        },
        Builtin {
            desc: NodeDesc::new(
                "tex.transform",
                "Translate, rotate and scale around a pivot; resolution of the input",
            )
            .param(
                ParamSpec::vec2("translate", [0.0; 2]).range(-1.0, 1.0).doc("In image sizes; y points down"),
            )
            .param(
                ParamSpec::float("rotate", 0.0)
                    .range(-180.0, 180.0)
                    .unit(Unit::Degrees)
                    .doc("Counter-clockwise"),
            )
            .param(ParamSpec::vec2("scale", [1.0; 2]).range(0.0, 4.0))
            .param(ParamSpec::vec2("pivot", [0.5; 2]).range(0.0, 1.0).more().doc("0, 0 is the top left"))
            .param(ParamSpec::choice("extend", &EXTEND).doc("Outside the input"))
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("transform.wgsl"),
            size: Size::Input,
            animated: None,
            density: transform_density,
        },
        Builtin {
            desc: NodeDesc::new("tex.level", "Levels, gamma, contrast, brightness, invert, opacity")
                .param(ParamSpec::float("black", 0.0).range(0.0, 1.0).doc("Input value that becomes 0"))
                .param(ParamSpec::float("white", 1.0).range(0.0, 1.0).doc("Input value that becomes 1"))
                .param(ParamSpec::float("gamma", 1.0).clamp(0.01, 10.0).curve(Curve::Log))
                .param(ParamSpec::float("contrast", 1.0).range(0.0, 4.0))
                .param(ParamSpec::float("brightness", 1.0).range(0.0, 4.0))
                .param(ParamSpec::bool("invert", false))
                .param(ParamSpec::float("opacity", 1.0).range(0.0, 1.0))
                .input("in", DataKind::Tex)
                .output("out", DataKind::Tex),
            source: include_str!("level.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
        },
        Builtin {
            desc: NodeDesc::new(
                "tex.quantize",
                "Fewer colours (levels per channel) and fewer pixels (blocks)",
            )
            .param(
                ParamSpec::int("levels", 4)
                    .clamp(2.0, 256.0)
                    .range(2.0, 32.0)
                    .doc("Values per channel, black and white included"),
            )
            .param(
                ParamSpec::int("block", 1)
                    .clamp(1.0, 1024.0)
                    .range(1.0, 64.0)
                    .unit(Unit::Pixels)
                    .doc("Side of the square blocks that take one colour; 1 keeps every pixel"),
            )
            .param(
                ParamSpec::choice("dither", &["none", "ordered"])
                    .doc("Ordered: a 4 x 4 Bayer pattern per block, so gradients survive few levels"),
            )
            .param(
                ParamSpec::choice("space", &["perceptual", "linear"])
                    .more()
                    .doc("Where the levels are evenly spaced: to the eye (sRGB curve) or in linear light"),
            )
            .param(ParamSpec::bool("alpha", false).more().doc("Quantize alpha too"))
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("quantize.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
        },
        Builtin {
            desc: NodeDesc::new(
                "tex.composite",
                "Top onto bottom; resolution of the bottom (or the top when only it is connected)",
            )
            .param(ParamSpec::choice(
                "op",
                &[
                    "over",
                    "add",
                    "multiply",
                    "screen",
                    "subtract",
                    "difference",
                    "maximum",
                    "minimum",
                    "average",
                    "inside",
                    "outside",
                    "atop",
                    "overlay",
                ],
            ).doc("inside, outside: the top only where the bottom is, or is not; atop: the top over the bottom, only where the bottom is; overlay: darkens the dark and brightens the bright parts of the bottom by the top"))
            .param(
                ParamSpec::bool("swap", false)
                    .doc("Swap the inputs: bottom onto top (TouchDesigner's Swap Operation Order)"),
            )
            .input("bottom", DataKind::Tex)
            .input("top", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("composite.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
        },
    ]
}

/// The transform's inverse mapping (transform.wgsl) differentiated: from output to input
/// texture coordinates through the aspect, the turn and the scale. Scaled up by 2, the input
/// moves half as far per output pixel, so it needs twice the resolution.
fn transform_density(def: &NodeDef, desc: &NodeDesc, _: usize, sizes: Option<Sizes>) -> Density {
    let [sx, sy] = match def.param(desc, "scale").and_then(|v| v.components()) {
        Some(&[x, y]) => [x, y],
        _ => [1.0, 1.0],
    };
    if sx.abs() < 1e-6 || sy.abs() < 1e-6 {
        return Density::Unrelated;
    }
    let a = sizes.map_or(1.0, |s| f64::from(s.output.0) / f64::from(s.output.1.max(1)));
    let angle = def.param(desc, "rotate").and_then(|v| v.as_f64()).unwrap_or(0.0).to_radians();
    let (c, s) = (angle.cos(), angle.sin());
    // diag(1 / (sx a), 1 / sy) * [[c, -s], [s, c]] * diag(a, 1)
    Density::Linear([[c / sx, -s / (sx * a)], [s * a / sy, c / sy]])
}

/// Adds `width`, `height` and [`keep_size`].
fn sized(desc: NodeDesc) -> NodeDesc {
    desc.param(ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels))
        .param(ParamSpec::int("height", 1080).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels))
        .param(keep_size())
        .output("out", DataKind::Tex)
}

/// The param that pins a node's `width` and `height` against `res.optimize`, on every node whose
/// size those params set.
pub const KEEP_SIZE: &str = "keep_size";

fn keep_size() -> ParamSpec {
    ParamSpec::bool(KEEP_SIZE, false).more().doc(
        "res.optimize leaves width and height alone (a deliberate resolution: a simulation, a pixel look)",
    )
}

/// Registers every `tex` node type.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    for b in builtins() {
        let b = Arc::new(b);
        let desc = b.desc.clone();
        types.register(FnNodeType::new(desc, move || {
            Box::new(BuiltinOp { node: b.clone(), ring: TexRing::new(), animated: false, sizes: None })
                as Box<dyn Operator>
        }))?;
    }
    blobtrack::register(types)?;
    blur::register(types)?;
    cache::register(types)?;
    denoise::register(types)?;
    flow::register(types)?;
    image::register(types)?;
    layer::register(types)?;
    mapper::register(types)?;
    opticalflow::register(types)?;
    ramp::register(types)?;
    render::register(types)?;
    shader::register(types)?;
    shapes::register(types)?;
    text::register(types)?;
    timemachine::register(types)?;
    video::register(types)?;
    videoin::register(types)?;
    Ok(())
}

/// Compiles every built-in pass, so the first node of each kind does not stall the cook thread
/// (a pipeline takes up to about a hundred milliseconds on some backends). Call it on a
/// background thread when the GPU appears.
pub fn warm_up(gpu: &Gpu) {
    for b in builtins() {
        if let Err(e) = gpu.cached_pass(&b.desc.kind, b.source, &b.desc.params) {
            log::error!("built-in shader {} does not compile: {e}", b.desc.kind);
        }
    }
    if let Err(e) = blobtrack::warm_up(gpu) {
        log::error!("built-in shader tex.blobtrack does not compile: {e}");
    }
    if let Err(e) = blur::warm_up(gpu) {
        log::error!("built-in shader tex.blur does not compile: {e}");
    }
    if let Err(e) = denoise::warm_up(gpu) {
        log::error!("built-in shader tex.denoise does not compile: {e}");
    }
    if let Err(e) = opticalflow::warm_up(gpu) {
        log::error!("built-in shader tex.opticalflow does not compile: {e}");
    }
    if let Err(e) = ramp::warm_up(gpu) {
        log::error!("built-in shader tex.ramp does not compile: {e}");
    }
    if let Err(e) = shapes::warm_up(gpu) {
        log::error!("built-in shader tex.shapes does not compile: {e}");
    }
    if let Err(e) = text::warm_up(gpu) {
        log::error!("built-in shader tex.text does not compile: {e}");
    }
    if let Err(e) = timemachine::warm_up(gpu) {
        log::error!("built-in shader tex.timemachine does not compile: {e}");
    }
    if let Err(e) = gpu.video_pipeline() {
        log::error!("built-in shader video.wgsl does not compile: {e}");
    }
}

struct BuiltinOp {
    node: Arc<Builtin>,
    ring: TexRing,
    animated: bool,
    sizes: Option<Sizes>,
}

impl Operator for BuiltinOp {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let b = &*self.node;
        let gpu = gpu(cx)?;
        let pass = gpu.cached_pass(&b.desc.kind, b.source, &b.desc.params)?;
        let inputs = inputs(cx);
        let first = || inputs.iter().flatten().next().map(|t| t.size()).ok_or_else(|| waiting(cx));
        let (w, h) = match b.size {
            Size::Params => (side(cx, "width")?, side(cx, "height")?),
            Size::Input => first()?,
            Size::Own => {
                first()?;
                (side(cx, "width")?, side(cx, "height")?)
            }
            Size::Custom(f) => f(cx, first()?)?,
        };
        self.sizes = first().ok().map(|input| Sizes { input, output: (w, h) });
        self.animated = b.animated.is_some_and(|f| f(cx));
        let (seconds, frame) = if self.animated {
            let t = cx.time();
            (t.seconds(), t.frame)
        } else {
            (0.0, 0)
        };
        let target = self.ring.next(gpu, w, h, &b.desc.kind)?;
        let param = |name: &str| cx.param(name).cloned();
        pass.render(gpu, &target, &PassInput { inputs: &inputs, seconds, frame, param: &param });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }

    fn time_dependent(&self) -> Option<bool> {
        self.node.animated.map(|_| self.animated)
    }

    fn density(&self, def: &NodeDef, desc: &NodeDesc, input: usize) -> Density {
        (self.node.density)(def, desc, input, self.sizes)
    }

    fn sets_size(&self, def: &NodeDef, desc: &NodeDesc) -> bool {
        match self.node.size {
            Size::Params | Size::Own => true,
            Size::Input => false,
            // A resize: only while its `mode` says so.
            Size::Custom(_) => def.param(desc, "mode").and_then(|v| v.as_str()) == Some("custom"),
        }
    }
}

/// Why a filter without an input texture cannot cook: connected but upstream has nothing yet
/// (an image loading), so wait; or nothing connected.
fn waiting(cx: &CookContext<'_>) -> CookError {
    if cx.def().inputs.iter().any(Option::is_some) {
        CookError::Pending
    } else {
        CookError::failed("connect an input")
    }
}

fn gpu<'a>(cx: &'a CookContext<'_>) -> Result<&'a Gpu, CookError> {
    cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU: texture nodes cannot cook"))
}

/// The texture inputs, `None` where unconnected.
fn inputs<'a>(cx: &'a CookContext<'_>) -> Vec<Option<&'a Tex>> {
    (0..cx.def().inputs.len().min(penumbra_gpu::pass::INPUTS)).map(|i| cx.input_as::<Tex>(i)).collect()
}

fn side(cx: &CookContext<'_>, name: &str) -> Result<u32, CookError> {
    u32::try_from(cx.int(name)?).map_err(|_| CookError::failed(format!("`{name}` must be positive")))
}

#[cfg(test)]
mod tests;
