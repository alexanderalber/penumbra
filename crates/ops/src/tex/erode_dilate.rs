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
            "Grows the bright parts of a picture: each pixel takes the brightest value around it. Thickens lines, fills small holes in a mask",
            "How far bright parts grow, in pixels across and down: each pixel takes the largest value within this distance. 0 on one side leaves that direction as it is; fractions grow the outermost pixel in part",
            "What the dilate reads beyond the input's edges; hold keeps the edges as they are",
        )
    } else {
        (
            "Shrinks the bright parts of a picture: each pixel takes the darkest value around it. Thins lines, removes specks from a mask",
            "How far bright parts shrink, in pixels across and down: each pixel takes the smallest value within this distance. 0 on one side leaves that direction as it is; fractions shrink by the outermost pixel in part",
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
        let reaches = |r: f64| r.is_finite() && r.clamp(0.0, MAX_RADIUS) > 0.0;
        let mut axes: Vec<[f64; 2]> = Vec::with_capacity(2);
        if reaches(size[0]) {
            axes.push([1.0, 0.0]);
        }
        if reaches(size[1]) {
            axes.push([0.0, 1.0]);
        }
        // A size of 0 still renders once, a copy, so the output is a texture of this node.
        if axes.is_empty() {
            axes.push([0.0, 0.0]);
        }
        let grow = if self.kind == DILATE { 1.0 } else { 0.0 };
        let mut current = src.clone();
        for (i, axis) in axes.iter().enumerate() {
            let last = i + 1 == axes.len();
            let ring = if last { &mut self.out } else { &mut self.middle };
            let target = ring.next(gpu, w, h, self.kind)?;
            let axis = *axis;
            let param = move |name: &str| match name {
                "axis" => Some(Value::Vec2(axis)),
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
