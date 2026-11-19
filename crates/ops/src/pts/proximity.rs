//! `pts.proximity`: lines between near points, TouchDesigner's Proximity POP: the plexus look.
//!
//! Each point chooses up to `max_lines` of the points within `distance` and at least
//! `min_distance` away, the nearest first (or a random choice that stays with the pair). A pair
//! is one line, whichever end chose it. The lines come out as points, two per line
//! (`Strips::Pairs`), which `geo.from_pts` draws in lines mode: each end has the position and
//! the attributes of its point, and `span` (the line's length) and `span01` (the length over
//! `distance`), so `pts.math` can fade the long lines.
//!
//! The search is [`penumbra_gpu::points::near`]; the lines are [`Pairer`].

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::canonical;
use penumbra_gpu::points::near::MAX_PER_CELL;
use penumbra_gpu::points::pairs::{self, Choose, MAX_LINES, Pairer, Settings};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::program::{input, wake};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.proximity";

/// Choices of `choose`.
pub const CHOOSE: [&str; 2] = ["nearest", "random"];

/// The default of `distance`: wide enough to join the neighbours of a default `pts.generate`
/// grid (10 x 10 over a metre, about 0.11 m apart).
pub const DEFAULT_DISTANCE: f64 = 0.15;

/// The default of `max`: the most lines.
pub const DEFAULT_MAX: u32 = 1_000_000;

/// The default cap on the points one cube of the search gives a point.
pub const DEFAULT_PER_CELL: u32 = 64;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(Proximity::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pairs::warm_up(gpu)
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        KIND,
        "Lines between near points: the plexus look. Each point joins up to `max_lines` of its neighbours within `distance`. geo.from_pts draws the lines in lines mode",
    )
    .param(
        ParamSpec::float("distance", DEFAULT_DISTANCE)
            .clamp(1e-6, 1e4)
            .range(0.001, 1.0)
            .curve(Curve::Log)
            .doc("The longest line, in metres: points farther apart are not joined"),
    )
    .param(
        ParamSpec::float("min_distance", 0.0)
            .clamp(0.0, 1e4)
            .range(0.0, 1.0)
            .doc("The shortest line, in metres: points nearer than this are not joined"),
    )
    .param(
        ParamSpec::int("max_lines", 4)
            .clamp(1.0, f64::from(MAX_LINES))
            .range(1.0, 12.0)
            .doc("Lines each point starts, at most. A point that other points choose can end up with more lines"),
    )
    .param(
        ParamSpec::choice("choose", &CHOOSE)
            .doc("Which neighbours a point joins when more than `max_lines` are within `distance`")
            .choice_doc("nearest", "the nearest ones. A line can switch to another neighbour when two distances change order")
            .choice_doc("random", "a random few, by a number each pair keeps while both points exist: lines change only as points come within `distance` or leave it. Points with an `id` (particles) keep their numbers by id"),
    )
    .param(
        ParamSpec::int("seed", 0)
            .clamp(0.0, 1e9)
            .range(0.0, 100.0)
            .doc("random: another number chooses other neighbours")
            .applies_when("choose", &["random"]),
    )
    .param(
        ParamSpec::bool("duplicates", false)
            .doc("Two points that choose each other get two lines, one from each. Off: one line. Two lines on top of each other look brighter with additive blending")
            .more(),
    )
    .param(
        ParamSpec::int("max", i64::from(DEFAULT_MAX))
            .clamp(1.0, 64.0 * 1024.0 * 1024.0)
            .range(1000.0, 10_000_000.0)
            .curve(Curve::Log)
            .doc("The most lines. Lines past this many are left out, the lines of the last points first. Memory: about 100 bytes a line with the default attributes")
            .more(),
    )
    .param(
        ParamSpec::text("attributes", "color width")
            .doc("Attributes each end of a line takes from its point besides position, separated by spaces. The ones the points lack are left out")
            .more(),
    )
    .param(
        ParamSpec::int("max_per_cell", i64::from(DEFAULT_PER_CELL))
            .clamp(1.0, f64::from(MAX_PER_CELL))
            .range(8.0, 1024.0)
            .curve(Curve::Log)
            .doc("The search cuts space into cubes as wide as `distance`. A point looks at no more than this many points of each cube around it: a dense clump costs no more than that. The node warns when points are left out. Many points lower it, so a frame stays short: to 795 for 100 000 points, 79 for a million, with a warning")
            .more(),
    )
    .input("in", DataKind::Pts)
    .describe_input("in", "The points to join, as many as they may be: generated, particles, points from a picture")
    .output("out", DataKind::Pts)
    .describe_output(
        "out",
        "Two points per line, its two ends: position and the attributes in `attributes` of each end's point, `span` (the line's length in metres) and `span01` (the length over `distance`: 0 for touching points, 1 at the longest). The number of lines is on the GPU",
    )
}

#[derive(Default)]
struct Proximity {
    pairer: Pairer,
}

impl Operator for Proximity {
    fn gpu_bytes(&self) -> u64 {
        self.pairer.bytes()
    }

    fn trim(&mut self) {
        self.pairer.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let choose = match cx.text("choose")? {
            "random" => {
                Choose::Random { seed: u32::try_from(cx.int("seed")?.clamp(0, 1_000_000_000)).unwrap_or(0) }
            }
            _ => Choose::Nearest,
        };
        let attrs: Vec<String> = cx
            .text("attributes")?
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| !w.is_empty())
            .map(|w| canonical(w).to_string())
            .collect();
        let distance = cx.float("distance")? as f32;
        let set = Settings {
            distance,
            min_distance: cx.float("min_distance")? as f32,
            most: u32::try_from(cx.int("max_lines")?.clamp(1, i64::from(MAX_LINES))).unwrap_or(4),
            choose,
            both: cx.bool("duplicates")?,
            max: u32::try_from(cx.int("max")?.clamp(1, i64::from(u32::MAX))).unwrap_or(DEFAULT_MAX),
            cap: u32::try_from(cx.int("max_per_cell")?.clamp(1, i64::from(MAX_PER_CELL)))
                .unwrap_or(DEFAULT_PER_CELL),
            attrs,
        };
        let out = self.pairer.pairs(gpu, points, &set, Some(wake(cx)))?;
        if let Some(w) = penumbra_gpu::points::near::lowered(set.cap, points.count().at_most()) {
            cx.warn(w);
        }
        if set.min_distance >= set.distance {
            cx.warn(
                "`min_distance` is at or above `distance`: no two points can be joined. Lower `min_distance`",
            );
        } else if let Some([_, lines, crowded, wanted]) = self.pairer.words(&set) {
            if wanted == 0 && points.count().seen().is_some_and(|n| n > 1) {
                cx.warn(format!(
                    "no two points lie within `distance` ({} m) of each other, so there are no lines. Raise `distance`",
                    set.distance
                ));
            }
            if wanted > lines {
                cx.warn(format!(
                    "{wanted} lines are wanted and `max` keeps {lines}: raise `max`, or lower `max_lines` or `distance`"
                ));
            }
            if crowded > 0 {
                cx.warn(format!(
                    "{crowded} points crowd their cube beyond `max_per_cell`: no point looks at them, so some lines are missing. Raise `max_per_cell` or lower `distance`"
                ));
            }
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
