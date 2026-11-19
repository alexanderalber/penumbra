//! `pts.feedback`: feeds a chain's points back into it the next frame, as `tex.feedback` does
//! with pictures: a simulation made by hand, points that pile up or drift a little more each
//! frame. TouchDesigner's Feedback POP.
//!
//! `initial` is what the loop starts from, and what it starts from again after a reset; the end
//! of the chain wired into `previous` comes back a frame later. The chain may change anything:
//! the number of points (a `pts.delete` in the loop), their attributes. What comes back is
//! passed on as it is. Its box stays where it lies inside the box of `initial` or of what the
//! node gave out the frame before; a box that grows is dropped.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Points;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

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
        || Box::new(Feedback::default()) as Box<dyn Operator>,
    ))
}

/// Passes on `previous`, `initial` before it has points and while `reset` is on. The cooker
/// shows it nothing on `previous` after a reset (`node.reset`, a component's reset), so the
/// loop starts from `initial` again.
#[derive(Default)]
struct Feedback {
    /// The box of what the node gave out last.
    gave: Option<Bounds>,
}

impl Operator for Feedback {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let previous = if cx.bool("reset")? { None } else { cx.input(1) };
        let initial = cx.input(0);
        let chosen = match (previous, initial) {
            (Some(p), _) => p,
            (None, Some(i)) => {
                let Some(points) = i.get::<Points>() else {
                    return Err(CookError::failed("not points"));
                };
                self.gave = points.bounds();
                return Ok(Cooked::Outputs(vec![i.clone()]));
            }
            (None, None) => return Err(waiting(cx)),
        };
        let Some(points) = chosen.get::<Points>() else {
            return Err(CookError::failed("not points"));
        };
        // A box that comes round grows with every step of a loop that widens it (noise added
        // to the positions, a turn), and never shrinks again. Such a box is dropped, and what
        // draws the points reads their real box from the GPU. A box that stays inside the one
        // the loop started from, or the one of the frame before, cannot grow and is kept.
        let start = initial.and_then(|i| i.get::<Points>()).and_then(Points::bounds);
        let came = points.bounds();
        let kept = came.filter(|b| [start, self.gave].iter().flatten().any(|outer| inside(b, outer)));
        self.gave = kept;
        if kept == came {
            return Ok(Cooked::Outputs(vec![chosen.clone()]));
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points.clone().with_bounds(kept))]))
    }
}

/// Whether `b` lies inside `outer` or on its edge.
fn inside(b: &Bounds, outer: &Bounds) -> bool {
    let low = b.min.iter().zip(&outer.min).all(|(v, o)| v >= o);
    let high = b.max.iter().zip(&outer.max).all(|(v, o)| v <= o);
    low && high
}
