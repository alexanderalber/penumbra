//! `tex.erode` and `tex.dilate`: a rectangular minimum or maximum, separable, so two passes of
//! `erode_dilate.wgsl` through an intermediate texture (one pass when one side of the size is 0).

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Gpu, Pass, PassInput, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{PassCell, gpu, inputs, waiting};
use crate::on_gpu;

const ERODE: &str = "tex.erode";
const DILATE: &str = "tex.dilate";
const SOURCE: &str = include_str!("erode_dilate.wgsl");

/// Largest radius in pixels, on either axis.
const MAX_RADIUS: f64 = 256.0;

fn desc(kind: &str) -> NodeDesc {
    let (summary, size_doc, extend_doc) = if kind == DILATE {
        (
            "Grows the bright parts of a picture: channel by channel, each pixel takes the largest value around it. Thickens lines, fills small holes in a mask",
            "How far bright parts grow, in pixels across and down, at most 256: each pixel takes the largest value within this distance. 0 on one side leaves that direction as it is. A fraction such as 1.5 takes the outermost pixel in by half",
            "What the dilate reads beyond the input's edges; hold keeps the edges as they are",
        )
    } else {
        (
            "Shrinks the bright parts of a picture: channel by channel, each pixel takes the smallest value around it. Thins lines, removes specks from a mask",
            "How far bright parts shrink, in pixels across and down, at most 256: each pixel takes the smallest value within this distance. 0 on one side leaves that direction as it is. A fraction such as 1.5 takes the outermost pixel in by half",
            "What the erode reads beyond the input's edges; hold keeps shapes that touch the edge from shrinking away from it",
        )
    };
    NodeDesc::new(kind, summary)
        .param(ParamSpec::vec2("size", [2.0, 2.0]).range(0.0, 32.0).unit(Unit::Pixels).doc(size_doc))
        .param(super::extend("hold").more().doc(extend_doc))
        .input("in", DataKind::Tex)
        .output("out", DataKind::Tex)
}

/// The pass's params: the node's plus the axis and the direction the node sets per pass. Both
/// kinds share one pass.
fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params = desc(ERODE).params;
    params.push(ParamSpec::vec2("axis", [1.0, 0.0]));
    params.push(ParamSpec::float("radius", 0.0));
    params.push(ParamSpec::float("grow", 0.0));
    gpu.cached_pass(ERODE, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    for kind in [ERODE, DILATE] {
        types.register(
            FnNodeType::new(desc(kind), move || {
                Box::new(Morph {
                    kind,
                    pass: PassCell::default(),
                    middle: TexRing::new(),
                    out: TexRing::new(),
                }) as Box<dyn Operator>
            })
            .warm_up(on_gpu(warm_up)),
        )?;
    }
    Ok(())
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

struct Morph {
    /// [`ERODE`] or [`DILATE`].
    kind: &'static str,
    pass: PassCell,
    middle: TexRing,
    out: TexRing,
}

impl Operator for Morph {
    fn gpu_bytes(&self) -> u64 {
        self.middle.bytes() + self.out.bytes()
    }

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
        let size = match cx.param("size") {
            Some(Value::Vec2(s)) => *s,
            _ => [0.0; 2],
        };
        let radius = |r: f64| if r.is_nan() { 0.0 } else { r.clamp(0.0, MAX_RADIUS) };
        let mut passes: Vec<([f64; 2], f64)> = Vec::with_capacity(2);
        for (axis, r) in [([1.0, 0.0], radius(size[0])), ([0.0, 1.0], radius(size[1]))] {
            if r > 0.0 {
                passes.push((axis, r));
            }
        }
        // A size of 0 still renders once, a copy, so the output is a texture of this node.
        if passes.is_empty() {
            passes.push(([0.0, 0.0], 0.0));
        }
        let grow = if self.kind == DILATE { 1.0 } else { 0.0 };
        let mut current = src.clone();
        for (i, &(axis, radius)) in passes.iter().enumerate() {
            let last = i + 1 == passes.len();
            let ring = if last { &mut self.out } else { &mut self.middle };
            let target = ring.next(gpu, w, h, self.kind)?;
            let param = move |name: &str| match name {
                "axis" => Some(Value::Vec2(axis)),
                "radius" => Some(Value::Float(radius)),
                "grow" => Some(Value::Float(grow)),
                _ => cx.param(name).cloned(),
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
