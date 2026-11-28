//! `pts.smooth`: points moved towards their neighbours along each line, TouchDesigner's Line
//! Smooth POP.
//!
//! Each step moves every point of a line `strength` of the way to the mean of the points before
//! and after it on its line; `iterations` steps in a row smooth more. Lines never mix: a trail
//! stays apart from the next one. A grid with lines both ways smooths as a surface. The ends of
//! open lines stay put while `pin_ends` is on. Only `position` changes. The work is
//! [`Smoother`].

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::points::lines::{self, MOST_ITERATIONS, Smooth, Smoother};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::input;
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.smooth";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Smoothed::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(lines::warm_up)),
    )
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "Smooths each line: every point moves towards its neighbours on its line, so corners round off and jitter calms down",
    )
    .param(
        ParamSpec::int("iterations", 4)
            .clamp(0.0, f64::from(MOST_ITERATIONS))
            .range(0.0, 50.0)
            .doc("Smoothing steps, one after another. More steps smooth over more points along the line. 0 passes the points on"),
    )
    .param(
        ParamSpec::float("strength", 0.5)
            .clamp(0.0, 1.0)
            .range(0.0, 1.0)
            .doc("How far each step moves a point towards the middle of its neighbours on its line: 0 not at all, 1 all the way"),
    )
    .param(
        ParamSpec::bool("pin_ends", true)
            .doc("The first and last point of each open line stay where they are. Off: the ends move too, and every step pulls the line in from its ends. Closed lines have no ends"),
    )
    .input("in", DataKind::Pts)
    .describe_input("in", "Points on lines: trails, a generated line or circle, a grid, the lines of pts.resample. Points without lines are one line in their order")
    .output("out", DataKind::Pts)
    .describe_output("out", "The same points and lines with smoother positions. Every other attribute stays")
}

#[derive(Default)]
struct Smoothed {
    smoother: Smoother,
}

impl Operator for Smoothed {
    fn gpu_bytes(&self) -> u64 {
        self.smoother.bytes()
    }

    fn trim(&mut self) {
        self.smoother.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let set = Smooth {
            iterations: u32::try_from(cx.int("iterations")?.clamp(0, i64::from(MOST_ITERATIONS)))
                .unwrap_or(0),
            strength: cx.float("strength")? as f32,
            pin_ends: cx.bool("pin_ends")?,
        };
        let out = self.smoother.smooth(gpu, points, &set)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
