//! `tex.denoise`: calms a noisy camera picture. A running average over the frames where the
//! picture is still, the new frame at once where it moves, and there an edge-keeping smoothing
//! within the frame instead (`denoise.wgsl`), each with its own strength for brightness and for
//! colour. One pass per new input frame; the node's last
//! result is the average so far.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{PassCell, gpu, waiting};
use crate::on_gpu;

const KIND: &str = "tex.denoise";
const SOURCE: &str = include_str!("denoise.wgsl");

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "Calms a noisy picture (a camera in the dark): averages over frames where it is still",
    )
    .stateful()
    .param(
        ParamSpec::float("frames", 8.0)
            .clamp(1.0, 256.0)
            .range(1.0, 64.0)
            .curve(Curve::Log)
            .doc(
                "About how many frames are averaged where the picture is still, for brightness noise; more is calmer; 1: off",
            ),
    )
    .param(
        ParamSpec::float("color_frames", 8.0)
            .clamp(1.0, 256.0)
            .range(1.0, 64.0)
            .curve(Curve::Log)
            .doc("The same for colour noise (coloured speckles)"),
    )
    .param(ParamSpec::float("motion", 0.06).clamp(0.0, 1.0).range(0.0, 0.3).curve(Curve::Power(2.0)).doc(
        "A change from which a pixel counts as moving and shows the new frame at once (on the \
                     square root of the values, so it means about the same in dark and bright parts). Raise \
                     it when the noise still flickers, lower it when moving things leave trails",
    ))
    .param(ParamSpec::float("spatial", 0.5).clamp(0.0, 1.0).range(0.0, 1.0).doc(
        "Smooths the brightness of what moves, where the average cannot help, keeping edges stronger than `motion`; 0: off",
    ))
    .param(
        ParamSpec::float("color_spatial", 0.5)
            .clamp(0.0, 1.0)
            .range(0.0, 1.0)
            .doc("The same for the colour; the eye forgives soft colour more than soft brightness"),
    )
    .param(ParamSpec::bool("reset", false).more().doc("Forget the average, while on"))
    .input("in", DataKind::Tex)
    .output("out", DataKind::Tex)
}

/// The pass's params: the node's plus `history`, set by the node.
fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params = desc().params;
    params.push(ParamSpec::float("history", 0.0));
    gpu.cached_pass(KIND, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Denoise::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

#[derive(Default)]
struct Denoise {
    pass: PassCell,
    ring: TexRing,
    /// The last result: the average so far.
    last: Option<Tex>,
    /// The input frame `last` was made from; the same frame again is not averaged twice.
    input: Option<Data>,
    /// Seeds the random rounding, a different pattern per frame.
    frame: u32,
}

impl Operator for Denoise {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = self.pass.get(gpu, pass)?;
        let data = cx.input(0).ok_or_else(|| waiting(cx))?;
        let src = data.get::<Tex>().ok_or_else(|| CookError::failed("not a texture"))?;
        let reset = cx.bool("reset")?;
        if reset {
            self.last = None;
        }
        if let (Some(last), Some(input)) = (&self.last, &self.input)
            && input.same(data)
        {
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, last.clone())]));
        }
        let (w, h) = src.size();
        let history = self.last.take().filter(|t| t.size() == (w, h));
        let target = self.ring.next(gpu, w, h, KIND)?;
        let weight = if history.is_some() { 1.0 } else { 0.0 };
        let param = move |name: &str| {
            if name == "history" { Some(Value::Float(weight)) } else { cx.param(name).cloned() }
        };
        self.frame = (self.frame + 1) % (1 << 20);
        pass.render(
            gpu,
            &target,
            &PassInput {
                inputs: &[Some(src), history.as_ref()],
                seconds: 0.0,
                frame: i64::from(self.frame),
                param: &param,
            },
        )?;
        self.last = (!reset).then(|| target.clone());
        self.input = Some(data.clone());
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}
