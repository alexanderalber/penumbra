//! `pts.resample`: new points evenly spaced along each line, TouchDesigner's Line Resample POP.
//!
//! Each line of the input (a trail, a generated line or circle, a grid's rows, the pairs of
//! `pts.proximity`; points without lines are one line) gets new points, evenly spaced by the
//! distance along it. Each new point mixes the attributes of the two old points around it. By
//! count every line gets the same number, so the lines stay lines of that many points; by length
//! each line gets as many as its length needs, and the count is on the GPU. Closed lines stay
//! closed. The work is [`Resampler`].

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::points::lines::{self, By, MOST_BY_COUNT, Resample, Resampler};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::{input, wake};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.resample";

/// Choices of `by`.
pub const BY: [&str; 2] = ["count", "length"];

/// The default of `max`: the most points that come out.
pub const DEFAULT_MAX: u32 = 1_000_000;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Resampled::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(lines::warm_up)),
    )
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "New points evenly spaced along each line: by a number per line or by the distance between them. Attributes are mixed from the old points around each new one",
    )
    .param(
        ParamSpec::choice("by", &BY)
            .doc("How many points each line gets")
            .choice_doc("count", "`count` on every line, whatever its length")
            .choice_doc("length", "about `length` apart: a long line gets more points than a short one. The number of points is on the GPU then"),
    )
    .param(
        ParamSpec::int("count", 32)
            .clamp(2.0, f64::from(MOST_BY_COUNT))
            .range(2.0, 256.0)
            .curve(Curve::Log)
            .doc("Points on each line, its two ends included. A closed line spaces them all the way round")
            .applies_when("by", &["count"]),
    )
    .param(
        ParamSpec::float("length", 0.05)
            .clamp(1e-6, 1e4)
            .range(0.001, 1.0)
            .curve(Curve::Log)
            .doc("The distance between two points along a line, in metres. Each line rounds it so its points come out even, from end to end")
            .applies_when("by", &["length"]),
    )
    .param(
        ParamSpec::int("max", i64::from(DEFAULT_MAX))
            .clamp(1.0, 64.0 * 1024.0 * 1024.0)
            .range(1000.0, 10_000_000.0)
            .curve(Curve::Log)
            .doc("The most points that come out. Lines past it are left out whole, the last ones first, with a warning")
            .more(),
    )
    .input("in", DataKind::Pts)
    .describe_input("in", "Points on lines: trails, a generated line or circle, a grid's rows, lines of pts.proximity. Points without lines are one line in their order")
    .output("out", DataKind::Pts)
    .describe_output(
        "out",
        "The lines again, with their new points in order along each. Every attribute of the input, mixed from the two old points around each new point; whole numbers such as `id` from the nearer one",
    )
}

#[derive(Default)]
struct Resampled {
    resampler: Resampler,
}

impl Operator for Resampled {
    fn gpu_bytes(&self) -> u64 {
        self.resampler.bytes()
    }

    fn trim(&mut self) {
        self.resampler.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let by = match cx.text("by")? {
            "length" => {
                let l = cx.float("length")?;
                By::Length(if l.is_finite() && l > 0.0 { l.clamp(1e-6, 1e4) as f32 } else { 0.05 })
            }
            _ => By::Count(u32::try_from(cx.int("count")?.clamp(2, i64::from(MOST_BY_COUNT))).unwrap_or(32)),
        };
        let max = u32::try_from(cx.int("max")?.clamp(1, i64::from(u32::MAX))).unwrap_or(DEFAULT_MAX);
        let set = Resample { by, max };
        let out = self.resampler.resample(gpu, points, &set, Some(wake(cx)))?;
        if let Some((kept, wanted)) = self.resampler.cut(&set) {
            cx.warn(format!(
                "the lines want {wanted} points and `max` keeps {kept}: the last lines are left out. Raise `max`, or ask for fewer points"
            ));
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
