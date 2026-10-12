//! `tex.blur`: a separable Gaussian, two passes of `blur.wgsl` through an intermediate texture
//! (one pass for a single direction).

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{EXTEND, gpu, inputs, waiting};

const KIND: &str = "tex.blur";
const SOURCE: &str = include_str!("blur.wgsl");

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Gaussian blur")
        .param(
            ParamSpec::float("size", 8.0)
                .clamp(0.0, 512.0)
                .range(0.0, 100.0)
                .curve(penumbra_core::Curve::Power(2.0))
                .unit(Unit::Pixels)
                .doc("Radius: three standard deviations"),
        )
        .param(ParamSpec::choice("direction", &["both", "horizontal", "vertical"]))
        .param(ParamSpec::choice("extend", &EXTEND).more().doc("What lies beyond the edges"))
        .input("in", DataKind::Tex)
        .output("out", DataKind::Tex)
}

/// The pass's params: the node's plus the axis the node sets per pass.
fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params = desc().params;
    params.push(ParamSpec::vec2("axis", [1.0, 0.0]));
    gpu.cached_pass(KIND, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || {
        Box::new(Blur { middle: TexRing::new(), out: TexRing::new() }) as Box<dyn Operator>
    }))
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

struct Blur {
    middle: TexRing,
    out: TexRing,
}

impl Operator for Blur {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = pass(gpu)?;
        let inputs = inputs(cx);
        let src = inputs.first().copied().flatten().ok_or_else(|| waiting(cx))?;
        let (w, h) = src.size();
        let direction = cx.text("direction")?;
        let axes: &[[f64; 2]] = match direction {
            "horizontal" => &[[1.0, 0.0]],
            "vertical" => &[[0.0, 1.0]],
            _ => &[[1.0, 0.0], [0.0, 1.0]],
        };
        let mut current = src.clone();
        for (i, axis) in axes.iter().enumerate() {
            let last = i + 1 == axes.len();
            let ring = if last { &mut self.out } else { &mut self.middle };
            let target = ring.next(gpu, w, h, KIND)?;
            let axis = *axis;
            let param = move |name: &str| {
                if name == "axis" { Some(Value::Vec2(axis)) } else { cx.param(name).cloned() }
            };
            pass.render(
                gpu,
                &target,
                &PassInput { inputs: &[Some(&current)], seconds: 0.0, frame: 0, param: &param },
            );
            current = target;
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, current)]))
    }
}
