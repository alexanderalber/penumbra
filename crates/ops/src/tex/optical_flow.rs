//! `tex.optical_flow`: how far each part of the picture moved since the previous input frame.
//! Pyramidal Lucas-Kanade on the GPU (`optical_flow.wgsl`): the input is shrunk to `size` wide,
//! a pyramid of halved levels is built, and the motion is refined from the coarsest level to the
//! finest, a few steps per level. The previous frame's pyramid is kept, so each new frame costs
//! one pyramid plus the steps. The same input frame again is not measured twice.
//!
//! Output, at the analysed size: red and green the motion in image sizes per input frame (x in
//! widths, y in heights, down positive), signed; blue 0, or with `confidence` how well the
//! motion there is pinned down (0 to 1); alpha 1. What is at a pixel now was that far up and left in the previous frame, so
//! `tex.displace` with `midpoint` 0 and `weight` -1 pulls the previous frame onto this one.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::{Batch, Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, waiting};

const KIND: &str = "tex.optical_flow";
const SOURCE: &str = include_str!("optical_flow.wgsl");

/// Most pyramid levels.
const MAX_LEVELS: usize = 8;
/// Smallest side of the coarsest level; fewer levels are built than asked for below it.
const MIN_SIDE: u32 = 8;

const PREP: i64 = 0;
const DOWN: i64 = 1;
const SOLVE: i64 = 2;
const OUT: i64 = 3;
const SMOOTH: i64 = 4;
/// Most smoothing rounds; the last one's taps are 2^(n-1) pixels apart.
const MAX_SMOOTH: i64 = 6;

fn desc() -> NodeDesc {
    NodeDesc::new(KIND, "How far each part of the picture moved since the previous frame (optical flow)")
        .stateful()
        .param(ParamSpec::bool("colors", false).doc(
            "Show the motion as colours instead of passing the numbers on: direction as hue, speed as \
             brightness. For looking at it; nodes that use the motion want this off",
        ))
        .param(ParamSpec::bool("confidence", false).more().doc(
            "Blue says how well the motion is pinned down, 0 to 1: high where there is texture, moving \
             or not, low on a plain wall; off, blue is 0 and a still picture black",
        ))
        .param(ParamSpec::int("fill", 4).clamp(0.0, MAX_SMOOTH as f64).range(0.0, MAX_SMOOTH as f64).doc(
            "Rounds of filling in: the motion found at edges and texture spreads into plain parts of the \
             same thing (skin, a shirt), not across edges; each round reaches twice as far. 0: off",
        ))
        .param(
            ParamSpec::float("threshold", 0.25)
                .clamp(0.0, 16.0)
                .range(0.0, 4.0)
                .curve(Curve::Power(2.0))
                .unit(Unit::Pixels)
                .doc(
                    "Slower motion than this fades out (none below half of it), so camera noise does not \
                     shimmer; in pixels at the analysed size per frame",
                ),
        )
        .param(
            ParamSpec::int("size", 320)
                .clamp(16.0, 1920.0)
                .range(64.0, 960.0)
                .curve(Curve::Log)
                .unit(Unit::Pixels)
                .more()
                .doc(
                    "Width the input is analysed at, and the output's; more sees finer motion and costs more",
                ),
        )
        .param(ParamSpec::int("levels", 4).clamp(1.0, MAX_LEVELS as f64).range(1.0, 6.0).more().doc(
            "Pyramid levels, each half the size of the one before; each one doubles the fastest motion \
             followed",
        ))
        .param(
            ParamSpec::int("iterations", 3)
                .clamp(1.0, 10.0)
                .range(1.0, 6.0)
                .more()
                .doc("Refining steps per level; more is more exact and costs more"),
        )
        .param(ParamSpec::int("window", 3).clamp(1.0, 8.0).range(1.0, 6.0).unit(Unit::Pixels).more().doc(
            "Radius of the neighbourhood each pixel's motion is judged from; more is smoother and \
             steadier, less keeps small moving things apart",
        ))
        .input("in", DataKind::Tex)
        .output("out", DataKind::Tex)
}

/// The pass's params: the node's plus `stage`, set by the node.
fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    let mut params = desc().params;
    params.push(ParamSpec::int("stage", 0));
    params.push(ParamSpec::int("spread", 1));
    gpu.cached_pass(KIND, SOURCE, &params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(OpticalFlow::default()) as Box<dyn Operator>))
}

pub fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

/// Sizes of the pyramid levels, finest first.
fn level_sizes(width: u32, height: u32, levels: usize) -> Vec<(u32, u32)> {
    let mut sizes = vec![(width, height)];
    let (mut w, mut h) = (width, height);
    while sizes.len() < levels {
        (w, h) = (w.div_ceil(2), h.div_ceil(2));
        if w < MIN_SIDE || h < MIN_SIDE {
            break;
        }
        sizes.push((w, h));
    }
    sizes
}

#[derive(Default)]
struct OpticalFlow {
    /// Per level: the luma pyramid and the motion.
    values: Vec<TexRing>,
    flows: Vec<TexRing>,
    smoothed: TexRing,
    out: TexRing,
    /// The previous input frame's pyramid.
    previous: Option<Vec<Tex>>,
    /// The last result and the input frame it was measured on.
    last: Option<Tex>,
    input: Option<Data>,
}

impl Operator for OpticalFlow {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let pass = pass(gpu)?;
        let data = cx.input(0).ok_or_else(|| waiting(cx))?;
        let src = data.get::<Tex>().ok_or_else(|| CookError::failed("not a texture"))?;
        if let (Some(last), Some(input)) = (&self.last, &self.input)
            && input.same(data)
        {
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, last.clone())]));
        }

        let (w, h) = src.size();
        let width = u32::try_from(cx.int("size")?).unwrap_or(320).clamp(16, w.max(16));
        let height = ((u64::from(width) * u64::from(h)) / u64::from(w.max(1))).clamp(1, 4096) as u32;
        let levels = usize::try_from(cx.int("levels")?).unwrap_or(1).clamp(1, MAX_LEVELS);
        let iterations = cx.int("iterations")?.clamp(1, 10);
        let sizes = level_sizes(width, height, levels);
        for rings in [&mut self.values, &mut self.flows] {
            rings.resize_with(sizes.len(), TexRing::new);
        }

        // All passes go out in one submission.
        let mut batch = Batch::new(gpu, KIND);
        let mut render = |target: &Tex, stage: i64, spread: i64, inputs: &[Option<&Tex>]| {
            let param = move |name: &str| match name {
                "stage" => Some(Value::Int(stage)),
                "spread" => Some(Value::Int(spread)),
                _ => cx.param(name).cloned(),
            };
            pass.encode(&mut batch, target, &PassInput { inputs, seconds: 0.0, frame: 0, param: &param });
        };

        // This frame's pyramid.
        let mut pyramid: Vec<Tex> = Vec::with_capacity(sizes.len());
        for (&(lw, lh), values) in sizes.iter().zip(&mut self.values) {
            let target = values.next(gpu, lw, lh, KIND)?;
            match pyramid.last() {
                None => render(&target, PREP, 1, &[Some(src)]),
                Some(finer) => render(&target, DOWN, 1, &[Some(finer)]),
            }
            pyramid.push(target);
        }

        // Coarse to fine against the previous frame's pyramid; on the first frame (or after a
        // change of size) there is nothing to compare with, and the motion is none.
        let previous = self.previous.take().filter(|p| {
            p.len() == pyramid.len() && p.iter().zip(&pyramid).all(|(a, b)| a.size() == b.size())
        });
        let mut flow: Option<Tex> = None;
        if let Some(previous) = &previous {
            let levels = pyramid.iter().zip(previous).zip(&mut self.flows).rev();
            for ((now, before), flows) in levels {
                let (lw, lh) = now.size();
                for _ in 0..iterations {
                    let target = flows.next(gpu, lw, lh, KIND)?;
                    render(&target, SOLVE, 1, &[Some(now), Some(before), flow.as_ref()]);
                    flow = Some(target);
                }
            }
        }
        if let (Some(finest), Some(mut current)) = (pyramid.first(), flow.take()) {
            for round in 0..cx.int("fill")?.clamp(0, MAX_SMOOTH) {
                let target = self.smoothed.next(gpu, width, height, KIND)?;
                render(&target, SMOOTH, 1 << round, &[Some(&current), Some(finest)]);
                current = target;
            }
            flow = Some(current);
        }
        let out = self.out.next(gpu, width, height, KIND)?;
        render(&out, OUT, 1, &[flow.as_ref()]);
        batch.submit();

        self.previous = Some(pyramid);
        self.last = Some(out.clone());
        self.input = Some(data.clone());
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, out)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_stop_before_they_get_too_small() {
        assert_eq!(level_sizes(320, 180, 4), vec![(320, 180), (160, 90), (80, 45), (40, 23)]);
        assert_eq!(level_sizes(40, 20, 8), vec![(40, 20), (20, 10)]);
        assert_eq!(level_sizes(16, 4, 3), vec![(16, 4)]);
    }
}
