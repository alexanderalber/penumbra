//! `tex.contour`: lines where a channel crosses evenly spaced levels (`contour.wgsl`). With
//! `smooth` the channel is first blurred inside the node, two passes of the same shader through
//! intermediate textures, so a noisy height map gives calm lines while the picture the lines are
//! drawn over (mode `over`) and its alpha stay sharp.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{PassCell, gpu, waiting};
use crate::on_gpu;

const KIND: &str = "tex.contour";
const SOURCE: &str = include_str!("contour.wgsl");

/// Largest `smooth` radius in pixels; a tap per pixel of radius and pass.
const MAX_SMOOTH: f64 = 64.0;

/// The values of the pass's `stage`, in the order contour.wgsl numbers them.
const DRAW: f64 = 0.0;
const SMOOTH_X: f64 = 1.0;
const SMOOTH_Y: f64 = 2.0;
const DRAW_SMOOTHED: f64 = 3.0;

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Contour lines of a channel: a line at every multiple of an interval")
        .param(
            ParamSpec::float("interval", 0.05)
                .range(0.005, 0.25)
                .doc("Step in the channel's value from one line to the next"),
        )
        .param(
            ParamSpec::float("offset", 0.0)
                .range(0.0, 0.25)
                .doc("Value of one of the lines; the others lie whole intervals above and below"),
        )
        .param(
            ParamSpec::float("thickness", 1.5)
                .range(0.5, 8.0)
                .unit(Unit::Pixels)
                .doc("Width of the lines in pixels, the same on steep and gentle slopes"),
        )
        .param(
            ParamSpec::float("smooth", 0.0)
                .clamp(0.0, MAX_SMOOTH)
                .range(0.0, 16.0)
                .curve(Curve::Power(2.0))
                .unit(Unit::Pixels)
                .doc(
                    "Smooths a noisy height map so the lines come out calm, without islands. Radius in pixels, like tex.blur's size. Under mode over the picture itself stays sharp; 0: off",
                ),
        )
        .param(ParamSpec::color("color", [1.0; 4]).doc("Colour of the lines"))
        .param(super::filters::channel(
            "Channel the lines follow, on straight colour (a height map: any of the colours)",
        ))
        .param(
            ParamSpec::vec2("range", [0.0, 1.0])
                .more()
                .doc("Only values strictly between these get a line: a map clipped at 0 or 1 gets none around its flat parts"),
        )
        .param(
            ParamSpec::choice("mode", &["lines", "alpha", "over"])
                .doc("What the lines are drawn on")
                .choice_doc("lines", "on opaque black")
                .choice_doc("alpha", "on transparent, to lay over a picture with tex.composite")
                .choice_doc("over", "over the input"),
        )
        .input("in", DataKind::Tex)
        .output("out", DataKind::Tex)
}

/// `smooth` as the pass knows it: the word is reserved in WGSL.
const RADIUS: &str = "radius";

/// The pass's params: the node's (`smooth` as [`RADIUS`]) plus `stage`, set by the node per pass.
fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params: Vec<ParamSpec> = desc().params.into_iter().filter(|p| p.name != "smooth").collect();
    params.push(ParamSpec::float(RADIUS, 0.0));
    params.push(ParamSpec::float("stage", DRAW));
    gpu.cached_pass(KIND, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Contour::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

#[derive(Default)]
struct Contour {
    pass: PassCell,
    /// The channel blurred across, then down (only with `smooth`).
    across: TexRing,
    field: TexRing,
    out: TexRing,
}

impl Contour {
    fn render(
        pass: &Pass,
        gpu: &Gpu,
        cx: &CookContext<'_>,
        target: &Tex,
        inputs: &[Option<&Tex>],
        stage: f64,
    ) -> Result<(), CookError> {
        let param = move |name: &str| match name {
            "stage" => Some(Value::Float(stage)),
            RADIUS => cx.param("smooth").cloned(),
            _ => cx.param(name).cloned(),
        };
        pass.render(gpu, target, &PassInput { inputs, seconds: 0.0, frame: 0, param: &param })?;
        Ok(())
    }
}

impl Operator for Contour {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = self.pass.get(gpu, pass)?;
        let src = cx.input_as::<Tex>(0).ok_or_else(|| waiting(cx))?;
        let (w, h) = src.size();
        let smooth = cx.float("smooth")?;
        let out = self.out.next(gpu, w, h, KIND)?;
        if smooth.is_finite() && smooth >= 0.5 {
            let across = self.across.next(gpu, w, h, KIND)?;
            Self::render(&pass, gpu, cx, &across, &[Some(src)], SMOOTH_X)?;
            let field = self.field.next(gpu, w, h, KIND)?;
            Self::render(&pass, gpu, cx, &field, &[None, Some(&across)], SMOOTH_Y)?;
            Self::render(&pass, gpu, cx, &out, &[Some(src), Some(&field)], DRAW_SMOOTHED)?;
        } else {
            Self::render(&pass, gpu, cx, &out, &[Some(src)], DRAW)?;
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, out)]))
    }
}
