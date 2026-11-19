//! `pts.merge` and `pts.switch`: several sets of points as one, and one of several.
//!
//! `pts.merge` puts the points of its inputs one after the other ([`Merger`]): every attribute of
//! any input is there, with its default where an input lacks it. A count kept on the GPU stays
//! there. `pts.switch` passes one input on as it is, chosen by `index`.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Points;
use penumbra_gpu::points::merge::Merger;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, InputChoice, MAX_RUN, NodeDesc, NodeTypes,
    Operator,
};

use crate::gpu;

pub const MERGE: &str = "pts.merge";
pub const SWITCH: &str = "pts.switch";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            MERGE,
            "Puts several sets of points into one: the points of the first input, then those of the second, and so on. Each attribute any input has is there; an input without it gives its points the default",
        )
        .input_run("in", DataKind::Pts, 2, "The sets of points, in order")
        .output("out", DataKind::Pts)
        .describe_output(
            "out",
            "Every input's points one after the other. An attribute two inputs give different types is an error. A count kept on the GPU (after pts.delete, from pts.particles) stays there. Lines stay where every input has the same lines; rows and columns of a grid go",
        ),
        || Box::new(Merge::default()) as Box<dyn Operator>,
    ))?;
    types.register(
        FnNodeType::new(
            NodeDesc::new(SWITCH, "Chooses which of its sets of points goes out, as they are: cut between shapes or looks")
                .param(
                    ParamSpec::int("index", 0)
                        .clamp(0.0, (MAX_RUN - 1) as f64)
                        .range(0.0, 7.0)
                        .doc("Which input goes out: the wired inputs counted from 0, gaps skipped. Past the last: the last. Bind a channel to it to switch from the network"),
                )
                .param(
                    ParamSpec::choice("inactive", &["cook", "skip"])
                        .doc("Whether the chains not chosen keep running (switching is instant) or pause (they cost nothing)")
                        .choice_doc("cook", "their chains keep cooking as if chosen")
                        .choice_doc(
                            "skip",
                            "only the chosen input's chain cooks; the others stand still. A particle system in a skipped chain carries on from where it stopped when it is chosen again",
                        ),
                )
                .input_run("in", DataKind::Pts, 2, "The sets of points to choose from")
                .output("out", DataKind::Pts),
            || Box::new(Switch) as Box<dyn Operator>,
        )
        .choose_inputs(switch_inputs),
    )?;
    Ok(())
}

#[derive(Default)]
struct Merge {
    merger: Merger,
}

impl Operator for Merge {
    fn gpu_bytes(&self) -> u64 {
        self.merger.bytes()
    }

    fn trim(&mut self) {
        self.merger.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let wired: Vec<usize> = (0..cx.def().inputs.len())
            .filter(|k| cx.def().inputs.get(*k).is_some_and(Option::is_some))
            .collect();
        if wired.is_empty() {
            return Err(CookError::failed("connect points to `in0`, `in1`, ..."));
        }
        let mut sets: Vec<(usize, &Points)> = Vec::with_capacity(wired.len());
        for k in &wired {
            match cx.input_as::<Points>(*k) {
                Some(p) => sets.push((*k, p)),
                None if cx.input_stuck(*k) => {
                    return Err(CookError::failed(format!("the node wired to `in{k}` has no points")));
                }
                None => return Err(CookError::Pending),
            }
        }
        // One input: nothing to merge, passed on as it is.
        if let [(_, only)] = sets.as_slice() {
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, (*only).clone())]));
        }
        let out = self.merger.merge(gpu, &sets)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}

/// Which of `count` wired inputs `index` chooses: past the last, the last.
fn chosen(index: i64, count: usize) -> Option<usize> {
    let last = count.checked_sub(1)?;
    Some(usize::try_from(index.max(0)).unwrap_or(usize::MAX).min(last))
}

/// With `inactive: skip`, the switch needs only the input it chooses.
fn switch_inputs(cx: &InputChoice<'_>) -> Option<Vec<usize>> {
    if cx.text("inactive")? != "skip" {
        return None;
    }
    let wired: Vec<usize> = (0..MAX_RUN).filter(|i| cx.connected(*i)).collect();
    let k = chosen(cx.float("index")? as i64, wired.len()).unwrap_or(0);
    Some(wired.get(k).copied().into_iter().collect())
}

struct Switch;

impl Operator for Switch {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let wired: Vec<usize> =
            (0..MAX_RUN).filter(|k| cx.def().inputs.get(*k).is_some_and(Option::is_some)).collect();
        let Some(k) = chosen(cx.int("index")?, wired.len()).and_then(|i| wired.get(i).copied()) else {
            return Err(CookError::failed("connect points to `in0`, `in1`, ..."));
        };
        match cx.input(k) {
            Some(d) if d.get::<Points>().is_some() => Ok(Cooked::Outputs(vec![d.clone()])),
            Some(_) => Err(CookError::failed("not points")),
            None if cx.input_stuck(k) => {
                Err(CookError::failed(format!("the node wired to `in{k}` has no points")))
            }
            None => Err(CookError::Pending),
        }
    }
}
