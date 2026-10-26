//! `tex.blend_weight`: a projector's picture weighted for a blend of several projectors. In
//! `smooth` mode the whole picture is multiplied by the projector's blend weight, as a
//! `tex.composite` multiply would. In `detail` mode the picture is split at `size` pixels: the
//! large features (a Gaussian blur of the picture) are weighted by the blend weight, the small
//! ones (the picture minus that blur) by the detail weight, which is 1 where this projector is
//! the one that draws fine detail and 0 where another does. Lines, outlines and text then come
//! from one projector only and do not double where two projectors disagree by a pixel or two,
//! while brightness still crosses over smoothly.
//!
//! Light cannot be negative, so where a small dark feature sits beside a bright one the sum of
//! the projectors keeps a faint halo: about the other projectors' share of the blurred picture.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{PassCell, gpu, inputs, waiting};
use crate::on_gpu;

pub const KIND: &str = "tex.blend_weight";
const SOURCE: &str = include_str!("blend_weight.wgsl");

/// The input slots.
const IN: usize = 0;
const WEIGHT: usize = 1;
const DETAIL: usize = 2;

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "A projector's picture weighted for a blend: all of it by the blend weight, or the small features from one projector only",
    ).block(penumbra_graph::Block::ScanAndShapes)
    .param(
        ParamSpec::choice("mode", &["smooth", "detail"])
            .doc("How the picture is weighted")
            .choice_doc("smooth", "all of it by the blend weight (as a tex.composite multiply)")
            .choice_doc(
                "detail",
                "large features by the blend weight, features smaller than size by the detail weight, so they come from one projector only",
            ),
    )
    .param(
        ParamSpec::float("size", 8.0)
            .clamp(0.5, 128.0)
            .range(1.0, 32.0)
            .curve(penumbra_core::Curve::Power(2.0))
            .unit(Unit::Pixels)
            .doc("In detail mode, where small features end: the blur radius that separates them, in projector pixels; a little more than the projectors disagree by"),
    )
    .input("in", DataKind::Tex)
    .input("weight", DataKind::Tex)
    .input("detail", DataKind::Tex)
    .output("out", DataKind::Tex)
    .describe_input("in", "The picture, in the projector's pixels")
    .describe_input("weight", "The projector's blend weight (scan.blend's weight_N.png)")
    .describe_input(
        "detail",
        "The projector's detail weight (scan.blend's detail_N.png); detail mode only",
    )
}

fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    gpu.cached_pass(KIND, SOURCE, &desc().params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || {
            Box::new(BlendWeight {
                pass: PassCell::default(),
                blur: PassCell::default(),
                middle: TexRing::new(),
                low: TexRing::new(),
                out: TexRing::new(),
            }) as Box<dyn Operator>
        })
        .warm_up(on_gpu(warm_up)),
    )
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu)?;
    super::blur::pass(gpu).map(|_| ())
}

struct BlendWeight {
    pass: PassCell,
    blur: PassCell,
    middle: TexRing,
    low: TexRing,
    out: TexRing,
}

impl Operator for BlendWeight {
    fn trim(&mut self) {
        self.middle.trim();
        self.low.trim();
        self.out.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = self.pass.get(gpu, pass)?;
        let inputs = inputs(cx);
        let at = |i: usize| inputs.get(i).copied().flatten();
        let src = at(IN).ok_or_else(|| waiting(cx))?;
        let weight = at(WEIGHT)
            .ok_or_else(|| CookError::failed("connect the projector's blend weight to `weight`"))?;
        let (w, h) = src.size();
        let detail = match (cx.text("mode")?, at(DETAIL)) {
            ("detail", Some(d)) => Some(d),
            ("detail", None) => {
                cx.warn("detail mode without a detail weight: weighted as in smooth mode");
                None
            }
            _ => None,
        };
        // The large features: the picture blurred, or the picture itself in smooth mode, where
        // the small features (picture minus blur) are then nothing and the detail weight unused.
        let low = match detail {
            Some(_) => {
                let blur = self.blur.get(gpu, super::blur::pass)?;
                let size = cx.float("size")?;
                let mut current = src.clone();
                for (i, axis) in [[1.0, 0.0], [0.0, 1.0]].into_iter().enumerate() {
                    let ring = if i == 0 { &mut self.middle } else { &mut self.low };
                    let target = ring.next(gpu, w, h, KIND)?;
                    let param = move |name: &str| match name {
                        "axis" => Some(Value::Vec2(axis)),
                        "size" => Some(Value::Float(size)),
                        "direction" => Some(Value::Text("both".into())),
                        "extend" => Some(Value::Text("hold".into())),
                        _ => None,
                    };
                    blur.render(
                        gpu,
                        &target,
                        &PassInput { inputs: &[Some(&current)], seconds: 0.0, frame: 0, param: &param },
                    )?;
                    current = target;
                }
                current
            }
            None => src.clone(),
        };
        let target = self.out.next(gpu, w, h, KIND)?;
        let all = [Some(src), Some(&low), Some(weight), Some(detail.unwrap_or(weight))];
        let param = |name: &str| cx.param(name).cloned();
        pass.render(gpu, &target, &PassInput { inputs: &all, seconds: 0.0, frame: 0, param: &param })?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}
