//! `pts.feedback`: feeds a chain's points back into it the next frame, as `tex.feedback` does
//! with pictures: a simulation made by hand, points that pile up or drift a little more each
//! frame. TouchDesigner's Feedback POP.
//!
//! `initial` is what the loop starts from, and what it starts from again after a reset; the end
//! of the chain wired into `previous` comes back a frame later. The chain may change anything:
//! the number of points (a `pts.delete` in the loop), their attributes. What comes back is
//! passed on as it is, without its box.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Points;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use crate::waiting;

pub const KIND: &str = "pts.feedback";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Feeds a chain's points back into it the next frame: a simulation of your own, points that drift or pile up. Wire the end of the chain into `previous`",
        )
        .stateful()
        .param(ParamSpec::bool("reset", false).doc("Pass `initial` instead, while on"))
        .param(
            ParamSpec::int(penumbra_graph::REPEAT, 1)
                .clamp(1.0, penumbra_graph::MAX_REPEAT as f64)
                .range(1.0, 8.0)
                .doc(
                    "How often the loop runs per frame. Every node between this one and the end wired into `previous` cooks this many times, each run reading the run before; nodes outside the loop cook once. Costs the loop's time this many times. The clock does not move between the runs",
                ),
        )
        .input("initial", DataKind::Pts)
        .input_previous("previous", DataKind::Pts)
        .describe_input("initial", "Passed on until `previous` has points, and while `reset` is on: what the loop starts from")
        .describe_input(
            "previous",
            "The end of the loop: what it held a frame ago is passed on. TouchDesigner calls it the target. Its points may differ from `initial` in number and attributes",
        )
        .output("out", DataKind::Pts)
        .time_dependent(),
        || Box::new(Feedback) as Box<dyn Operator>,
    ))
}

/// Passes on `previous`, `initial` before it has points and while `reset` is on. The cooker
/// shows it nothing on `previous` after a reset (`node.reset`, a component's reset), so the
/// loop starts from `initial` again.
struct Feedback;

impl Operator for Feedback {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let previous = if cx.bool("reset")? { None } else { cx.input(1) };
        let chosen = match (previous, cx.input(0)) {
            (Some(p), _) => p,
            (None, Some(i)) => return passed(i),
            (None, None) => return Err(waiting(cx)),
        };
        let Some(points) = chosen.get::<Points>() else {
            return Err(CookError::failed("not points"));
        };
        // A box that comes round grows with every step of a loop that widens it (noise added
        // to the positions, a turn), and never shrinks again: what comes back has none, and
        // what draws it reads the points' real box from the GPU.
        if points.bounds().is_none() {
            return Ok(Cooked::Outputs(vec![chosen.clone()]));
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points.clone().with_bounds(None))]))
    }
}

/// `initial`, passed on as it is.
fn passed(initial: &Data) -> Result<Cooked, CookError> {
    if initial.get::<Points>().is_none() {
        return Err(CookError::failed("not points"));
    }
    Ok(Cooked::Outputs(vec![initial.clone()]))
}
