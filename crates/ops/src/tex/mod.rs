//! `tex` nodes: 2D textures on the GPU.
//!
//! Every node renders one fullscreen pass ([`penumbra_gpu::Pass`]); the built-ins are WGSL
//! files in this directory with their parameters declared here, the shader node reads both from
//! a file in the project. The data on a `tex` connection is a [`Tex`]: linear light,
//! premultiplied alpha, origin top left. Without a GPU (headless tools, tests of other crates)
//! these nodes fail with a clear error instead of producing anything.

mod audio_meters;
pub mod blob_track;
mod blur;
mod cache;
mod convert;
mod denoise;
mod filters;
mod flow;
mod image;
mod layer;
pub mod mapper;
mod optical_flow;
mod ramp;
pub mod render;
mod shader;
pub mod shapes;
mod text;
mod time_machine;
mod video;
pub mod video_in;

pub use audio_meters::AUDIO_METERS;
pub use text::{DEFAULT_FONT, TEXT, families as font_families};
pub use video::VIDEO;
pub use video_in::VIDEO_IN;

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
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

/// An `extend` param ([`EXTEND`]) with `default` chosen: what a node fetches from beyond its
/// input's edges.
fn extend(default: &str) -> ParamSpec {
    let mut spec = ParamSpec::choice("extend", &EXTEND)
        .doc("What shows where the picture is fetched from beyond the input's edges")
        .choice_doc("hold", "the edge pixels continue outwards")
        .choice_doc("zero", "transparent black")
        .choice_doc("repeat", "the input again, tiled")
        .choice_doc("mirror", "the input again, mirrored, so the copies meet at the edges");
    spec.default = Value::Text(default.to_string());
    spec
}

/// A built-in pass node.
struct Builtin {
    desc: NodeDesc,
    source: &'static str,
    size: Size,
    /// Whether an instance reads the clock, from its parameters (`None`: never).
    animated: Option<fn(&CookContext<'_>) -> bool>,
    /// How the inputs relate to the output ([`Operator::density`]).
    density: DensityFn,
    /// A fold: the first two inputs go through the shader, then the result and each further
    /// connected input in turn (`tex.composite`'s layers).
    fold: bool,
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
                .param(
                    ParamSpec::color("color", [0.0, 0.0, 0.0, 1.0])
                        .doc("The colour, linear RGBA with straight alpha; above 1 is brighter than white"),
                ),
            source: include_str!("constant.wgsl"),
            size: Size::Params,
            animated: None,
            density: same,
            fold: false,
        },
        Builtin {
            desc: sized(NodeDesc::new(
                "tex.noise",
                "Fractal noise: offset + amplitude * noise, the same on every GPU",
            ))
            .param(
                ParamSpec::choice("basis", &["gradient", "value"])
                    .doc("The kind of noise each octave is made of")
                    .choice_doc("gradient", "smooth, round blobs (Perlin-like)")
                    .choice_doc("value", "blockier, with a visible grid of cells"),
            )
            .param(
                ParamSpec::int("seed", 0)
                    .range(0.0, 100.0)
                    .doc("Picks another noise of the same look; each number gives the same picture every time"),
            )
            .param(
                ParamSpec::float("frequency", 4.0)
                    .range(0.1, 100.0)
                    .curve(Curve::Log)
                    .doc("Size of the pattern: noise cells per image height; higher is finer"),
            )
            .param(
                ParamSpec::int("octaves", 3)
                    .clamp(1.0, 8.0)
                    .doc("Layers of ever finer noise added up (fractal detail); 1 is smooth blobs only"),
            )
            .param(
                ParamSpec::float("roughness", 0.5)
                    .range(0.0, 1.0)
                    .doc("Strength of each octave relative to the one before; higher gives more fine grain"),
            )
            .param(
                ParamSpec::float("lacunarity", 2.0)
                    .range(1.0, 4.0)
                    .more()
                    .doc("How much finer each octave is than the one before (2: twice the cells)"),
            )
            .param(
                ParamSpec::float("amplitude", 0.5)
                    .range(0.0, 2.0)
                    .doc("Contrast: the noise (about -1 to 1) is multiplied by this, then offset is added"),
            )
            .param(
                ParamSpec::float("offset", 0.5)
                    .range(-1.0, 1.0)
                    .doc("Added after amplitude: the mean grey. With 0.5 and 0.5 the values span about 0 to 1"),
            )
            .param(
                ParamSpec::float("speed", 0.0)
                    .range(0.0, 4.0)
                    .doc("How fast the pattern changes, in cells per second; it moves through a third dimension, so it morphs rather than slides. 0 is still"),
            )
            .param(
                ParamSpec::vec3("translate", [0.0; 3])
                    .more()
                    .doc("Position in the noise, in cells: x and y slide the picture, z morphs it as speed does"),
            )
            .param(ParamSpec::bool("color", false).doc("Three independent channels instead of grey"))
            .param(
                ParamSpec::float("alpha", 1.0)
                    .clamp(0.0, 1.0)
                    .more()
                    .doc("Opacity of the whole picture; below 1, what lies under it in a composite shows through"),
            ),
            source: include_str!("noise.wgsl"),
            size: Size::Params,
            animated: Some(|cx| cx.float("speed").is_ok_and(|s| s != 0.0)),
            density: same,
            fold: false,
        },
        Builtin {
            desc: NodeDesc::new(
                "tex.transform",
                "Translate, rotate and scale around a pivot; resolution of the input",
            )
            .param(
                ParamSpec::vec2("translate", [0.0; 2])
                    .range(-1.0, 1.0)
                    .doc("Shift in image sizes: 1 moves by the whole width or height; positive y moves down"),
            )
            .param(
                ParamSpec::float("rotate", 0.0)
                    .range(-180.0, 180.0)
                    .unit(Unit::Degrees)
                    .doc("Turn around the pivot, counter-clockwise on screen"),
            )
            .param(
                ParamSpec::vec2("scale", [1.0; 2])
                    .range(0.0, 4.0)
                    .doc("Size across and down, around the pivot: 1 keeps it, 2 doubles it, negative mirrors"),
            )
            .param(
                ParamSpec::vec2("pivot", [0.5; 2])
                    .range(0.0, 1.0)
                    .more()
                    .doc("The point rotate and scale turn and grow around, in image sizes: 0, 0 is the top left, 0.5, 0.5 the centre"),
            )
            .param(extend("hold"))
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("transform.wgsl"),
            size: Size::Input,
            animated: None,
            density: transform_density,
            fold: false,
        },
        Builtin {
            desc: NodeDesc::new("tex.level", "Levels, gamma, contrast, brightness, invert, opacity")
                .param(
                    ParamSpec::float("black", 0.0)
                        .range(0.0, 1.0)
                        .doc("Input value that becomes 0, per channel; darker goes below 0. The first step"),
                )
                .param(
                    ParamSpec::float("white", 1.0)
                        .range(0.0, 1.0)
                        .doc("Input value that becomes 1, per channel; brighter goes above 1 (not clipped)"),
                )
                .param(
                    ParamSpec::float("gamma", 1.0)
                        .clamp(0.01, 10.0)
                        .curve(Curve::Log)
                        .doc("Bends the midtones after black and white: above 1 brightens them, below 1 darkens; 0 and 1 stay"),
                )
                .param(
                    ParamSpec::float("contrast", 1.0)
                        .range(0.0, 4.0)
                        .doc("Spreads values away from mid grey (0.5) after gamma: 0 is flat grey, 2 doubles the distance"),
                )
                .param(
                    ParamSpec::float("brightness", 1.0)
                        .range(0.0, 4.0)
                        .doc("Multiplies the colour after contrast: 0 is black, 2 twice as bright"),
                )
                .param(
                    ParamSpec::bool("invert", false)
                        .doc("1 minus the colour, after all the steps above; alpha stays"),
                )
                .param(
                    ParamSpec::float("opacity", 1.0)
                        .range(0.0, 1.0)
                        .doc("Multiplies alpha: 0 transparent, 1 as the input; fades a layer in a composite"),
                )
                .input("in", DataKind::Tex)
                .output("out", DataKind::Tex),
            source: include_str!("level.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
            fold: false,
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
                    .doc("How values between two levels are shown")
                    .choice_doc("none", "each snaps to the nearest level: hard bands")
                    .choice_doc(
                        "ordered",
                        "a 4 x 4 Bayer pattern per block mixes the two levels, so gradients survive few levels",
                    ),
            )
            .param(
                ParamSpec::choice("space", &["perceptual", "linear"])
                    .more()
                    .doc("Where the levels are evenly spaced")
                    .choice_doc("perceptual", "to the eye (on the sRGB curve): even steps from black to white")
                    .choice_doc("linear", "in linear light: most levels look bright, few dark"),
            )
            .param(
                ParamSpec::bool("alpha", false)
                    .more()
                    .doc("Snap alpha to the levels too; off keeps the input's alpha, so soft edges stay soft"),
            )
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("quantize.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
            fold: false,
        },
        Builtin {
            desc: NodeDesc::new(
                "tex.composite",
                "layer2 onto layer1, then layer3 onto that, then layer4; resolution of the first connected input",
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
                    "color",
                    "luminosity",
                ],
            )
            .doc("How each layer goes onto what lies below it: layer2 onto layer1, then layer3 onto that, ...")
            .choice_doc("over", "the layer in front, covering by its alpha")
            .choice_doc("add", "both summed: brighter, black adds nothing")
            .choice_doc("multiply", "both multiplied: darker, white changes nothing")
            .choice_doc("screen", "the opposite of multiply: lighter, black changes nothing")
            .choice_doc("subtract", "the picture below minus the layer's colour")
            .choice_doc("difference", "how far the colours differ, per channel")
            .choice_doc("maximum", "the brighter of both, per channel")
            .choice_doc("minimum", "the darker of both, per channel")
            .choice_doc("average", "half of each")
            .choice_doc("inside", "the layer only where the picture below is (its alpha)")
            .choice_doc("outside", "the layer only where the picture below is not")
            .choice_doc("atop", "the layer over the picture below, only where that is")
            .choice_doc(
                "overlay",
                "darkens the dark and brightens the bright parts of the picture below by the layer",
            )
            .choice_doc("color", "the layer's hue and saturation at the brightness below (tinting a grey picture)")
            .choice_doc("luminosity", "the layer's brightness with the hue and saturation below"))
            .param(
                ParamSpec::bool("swap", false)
                    .doc("Swap the order of each step: the picture below onto the layer (TouchDesigner's Swap Operation Order)"),
            )
            .input("layer1", DataKind::Tex)
            .input("layer2", DataKind::Tex)
            .input("layer3", DataKind::Tex)
            .input("layer4", DataKind::Tex)
            .output("out", DataKind::Tex),
            source: include_str!("composite.wgsl"),
            size: Size::Input,
            animated: None,
            density: same,
            fold: true,
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
    desc.param(width_param()).param(height_param()).param(keep_size()).output("out", DataKind::Tex)
}

/// The `width` of a node that makes a picture of its own size.
fn width_param() -> ParamSpec {
    ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels).doc(
        "Width of the picture made, in pixels; res.optimize may change it to what the projectors need, unless keep_size is on",
    )
}

/// The `height` of a node that makes a picture of its own size.
fn height_param() -> ParamSpec {
    ParamSpec::int("height", 1080).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels).doc(
        "Height of the picture made, in pixels; res.optimize may change it to what the projectors need, unless keep_size is on",
    )
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
    audio_meters::register(types)?;
    blob_track::register(types)?;
    blur::register(types)?;
    cache::register(types)?;
    convert::register(types)?;
    denoise::register(types)?;
    flow::register(types)?;
    image::register(types)?;
    layer::register(types)?;
    mapper::register(types)?;
    optical_flow::register(types)?;
    ramp::register(types)?;
    render::register(types)?;
    shader::register(types)?;
    shapes::register(types)?;
    text::register(types)?;
    time_machine::register(types)?;
    video::register(types)?;
    video_in::register(types)?;
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
    if let Err(e) = audio_meters::warm_up(gpu) {
        log::error!("built-in shader tex.audio_meters does not compile: {e}");
    }
    if let Err(e) = blob_track::warm_up(gpu) {
        log::error!("built-in shader tex.blob_track does not compile: {e}");
    }
    if let Err(e) = blur::warm_up(gpu) {
        log::error!("built-in shader tex.blur does not compile: {e}");
    }
    if let Err(e) = convert::warm_up(gpu) {
        log::error!("built-in shader chan.from_tex does not compile: {e}");
    }
    if let Err(e) = denoise::warm_up(gpu) {
        log::error!("built-in shader tex.denoise does not compile: {e}");
    }
    if let Err(e) = optical_flow::warm_up(gpu) {
        log::error!("built-in shader tex.optical_flow does not compile: {e}");
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
    if let Err(e) = time_machine::warm_up(gpu) {
        log::error!("built-in shader tex.time_machine does not compile: {e}");
    }
    if let Err(e) = gpu.video_pipeline() {
        log::error!("built-in shader video.wgsl does not compile: {e}");
    }
    if let Err(e) = gpu.yuv_pipeline() {
        log::error!("built-in shader yuv.wgsl (out.video) does not compile: {e}");
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
        let mut target = self.ring.next(gpu, w, h, &b.desc.kind)?;
        let param = |name: &str| cx.param(name).cloned();
        let fold = b.fold;
        let first = if fold { inputs.get(..2).unwrap_or(&inputs) } else { &inputs };
        pass.render(gpu, &target, &PassInput { inputs: first, seconds, frame, param: &param });
        if fold {
            for k in 2..cx.def().inputs.len() {
                let Some(layer) = cx.input_as::<Tex>(k) else { continue };
                let next = self.ring.next(gpu, w, h, &b.desc.kind)?;
                let pair = [Some(&target), Some(layer)];
                pass.render(gpu, &next, &PassInput { inputs: &pair, seconds, frame, param: &param });
                target = next;
            }
        }
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
