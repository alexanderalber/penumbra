//! `tex.blur`: a separable Gaussian, two passes of `blur.wgsl` through an intermediate texture
//! (one pass for a single direction).

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{PassCell, gpu, inputs, waiting};
use crate::on_gpu;

const KIND: &str = "tex.blur";
const SOURCE: &str = include_str!("blur.wgsl");

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "Softens a picture: a Gaussian blur, in every direction or only across or only up and down")
        .param(
            ParamSpec::float("size", 8.0)
                .clamp(0.0, 512.0)
                .range(0.0, 100.0)
                .curve(penumbra_core::Curve::Power(2.0))
                .unit(Unit::Pixels)
                .doc("Blur radius in pixels (three standard deviations of the Gaussian); below 0.5 leaves the input as it is"),
        )
        .param(
            ParamSpec::choice("direction", &["both", "horizontal", "vertical"])
                .doc("Which way the picture is smeared")
                .choice_doc("both", "in every direction, a round blur")
                .choice_doc("horizontal", "only left and right, as motion across")
                .choice_doc("vertical", "only up and down"),
        )
        .param(
            super::extend("hold")
                .more()
                .doc("What the blur reads beyond the input's edges; hold keeps the edges from darkening"),
        )
        .input("in", DataKind::Tex)
        .output("out", DataKind::Tex)
}

/// The pass's params: the node's plus the axis the node sets per pass.
pub(super) fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params = desc().params;
    params.push(ParamSpec::vec2("axis", [1.0, 0.0]));
    gpu.cached_pass(KIND, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || {
            Box::new(Blur { pass: PassCell::default(), middle: TexRing::new(), out: TexRing::new() })
                as Box<dyn Operator>
        })
        .warm_up(on_gpu(warm_up)),
    )
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

struct Blur {
    pass: PassCell,
    middle: TexRing,
    out: TexRing,
}

impl Operator for Blur {
    fn trim(&mut self) {
        self.middle.trim();
        self.out.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = self.pass.get(gpu, pass)?;
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
            )?;
            current = target;
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, current)]))
    }
}
