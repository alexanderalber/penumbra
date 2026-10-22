//! `tex` nodes that route textures rather than compute them: null, switch, select, feedback.
//! Feedback is two plain inputs, `initial` and `previous`, where `previous` is wired back from
//! the end of the loop and reads that node's result from the previous frame (TouchDesigner's target
//! param got wired the wrong way round every time). They pass the texture they choose on as it is
//! (no copy), except a switch blending two inputs.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::{Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, InputChoice, NodeDesc, NodeTypes, Operator,
    StateItem,
};

use super::{gpu, waiting};

/// Inputs of a switch.
pub const SWITCH_INPUTS: usize = 8;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new("tex.null", "The input, unchanged: a fixed point to refer to or to view")
            .input("in", DataKind::Tex)
            .output("out", DataKind::Tex),
        || Box::new(Null) as Box<dyn Operator>,
    ))?;

    let mut switch = NodeDesc::new("tex.switch", "One of several inputs, or a blend of two neighbours")
        .param(
            ParamSpec::float("index", 0.0)
                .range(0.0, (SWITCH_INPUTS - 1) as f64)
                .doc("Which connected input, counted from 0; past the last, the last"),
        )
        .param(ParamSpec::bool("blend", false).doc("A fractional index mixes the two inputs either side"))
        .param(
            ParamSpec::choice("inactive", &["cook", "skip"])
                .doc("What the inputs not shown do")
                .choice_doc("cook", "their chains keep cooking as if shown")
                .choice_doc(
                    "skip",
                    "only the shown input's chain cooks, the others stand still and cost nothing. \
                     Switching to a skipped input cooks its chain in that frame (a slow chain makes the \
                     frame late, a video may need a moment to catch up), and a feedback loop, cache or \
                     time machine in it carries on from where it stopped. An input counts as connected \
                     as soon as it is wired",
                ),
        );
    for i in 0..SWITCH_INPUTS {
        switch = switch.input(&format!("in{i}"), DataKind::Tex);
    }
    types.register(
        FnNodeType::new(switch.output("out", DataKind::Tex), || {
            Box::new(Switch { ring: TexRing::new(), cross: None }) as Box<dyn Operator>
        })
        .choose_inputs(switch_inputs),
    )?;

    types.register(FnNodeType::new(
        NodeDesc::new("tex.select", "The output of another node, named by path: a wire without the wire")
            .param(
                ParamSpec::node("node")
                    .doc("Path of the node (/name), or the name of a node beside this one"),
            )
            .param(
                ParamSpec::int("output", 0)
                    .clamp(0.0, 63.0)
                    .more()
                    .doc("Which output of that node, counted from 0 (tex.render's data is 1)"),
            )
            .output("out", DataKind::Tex),
        || Box::new(Select) as Box<dyn Operator>,
    ))?;

    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.feedback",
            "A loop: wire the end of the chain back into `previous`; `initial` until that has a frame",
        )
        .stateful()
        .param(ParamSpec::bool("reset", false).doc("Pass `initial` instead, while on"))
        .param(
            ParamSpec::bool("persist", true)
                .more()
                .doc("Keep the loop's last frame in the state snapshots, so a restart carries on from it"),
        )
        .input("initial", DataKind::Tex)
        .input_previous("previous", DataKind::Tex)
        .output("out", DataKind::Tex)
        .time_dependent(),
        || Box::new(Feedback::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn pass_on(data: &Data) -> Result<Cooked, CookError> {
    if data.get::<Tex>().is_none() {
        return Err(CookError::failed("not a texture"));
    }
    Ok(Cooked::Outputs(vec![data.clone()]))
}

struct Null;

impl Operator for Null {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let data = cx.input(0).ok_or_else(|| waiting(cx))?;
        pass_on(data)
    }
}

struct Switch {
    ring: TexRing,
    /// tex.cross's pass, for blending.
    cross: Option<Arc<Pass>>,
}

/// Where a switch's `index` falls among `count` connected inputs: the one below, and the share
/// of the one above (0 for none). Past either end, the end.
fn switch_position(index: f64, count: usize) -> Option<(usize, f64)> {
    let last = count.checked_sub(1)?;
    let index = if index.is_finite() { index.clamp(0.0, last as f64) } else { 0.0 };
    Some((index.floor() as usize, index - index.floor()))
}

/// With `inactive: skip`, the switch needs only the wired inputs it shows.
fn switch_inputs(cx: &InputChoice<'_>) -> Option<Vec<usize>> {
    if cx.text("inactive")? != "skip" {
        return None;
    }
    let wired: Vec<usize> = (0..SWITCH_INPUTS).filter(|i| cx.connected(*i)).collect();
    let (lower, t) = switch_position(cx.float("index")?, wired.len()).unwrap_or((0, 0.0));
    let blend = cx.bool("blend").unwrap_or(false) && t >= 1e-6;
    let upper = if blend { lower + 1 } else { lower };
    Some(wired.iter().skip(lower).take(upper - lower + 1).copied().collect())
}

impl Operator for Switch {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        // Skipping, the inputs not shown read as nothing, so they count by their wires.
        let connected: Vec<Option<&Data>> = if cx.text("inactive")? == "skip" {
            (0..SWITCH_INPUTS)
                .filter(|i| cx.def().inputs.get(*i).is_some_and(Option::is_some))
                .map(|i| cx.input(i))
                .collect()
        } else {
            (0..SWITCH_INPUTS).filter_map(|i| cx.input(i)).map(Some).collect()
        };
        let Some((lower, t)) = switch_position(cx.float("index")?, connected.len()) else {
            return Err(waiting(cx));
        };
        let last = connected.len().saturating_sub(1);
        let pick = |i: usize| connected.get(i.min(last)).copied().flatten();
        let Some(a) = pick(lower) else { return Err(waiting(cx)) };
        let blend = cx.bool("blend")? && t >= 1e-6;
        let b = if blend { pick(lower + 1) } else { Some(a) };
        let Some(b) = b else { return Err(waiting(cx)) };
        if !blend || a.same(b) {
            return pass_on(a);
        }
        let (Some(ta), Some(tb)) = (a.get::<Tex>(), b.get::<Tex>()) else {
            return Err(CookError::failed("not a texture"));
        };
        let gpu = gpu(cx)?;
        let pass = match &self.cross {
            Some(p) => p.clone(),
            None => {
                let cross = super::builtins()
                    .into_iter()
                    .find(|b| b.desc.kind == "tex.cross")
                    .ok_or_else(|| CookError::failed("tex.cross is missing"))?;
                let p = gpu.cached_pass(&cross.desc.kind, cross.source, &cross.desc.params)?;
                self.cross = Some(p.clone());
                p
            }
        };
        let (w, h) = ta.size();
        let target = self.ring.next(gpu, w, h, "tex.switch")?;
        let param = move |name: &str| (name == "cross").then_some(penumbra_core::Value::Float(t));
        pass.render(
            gpu,
            &target,
            &PassInput { inputs: &[Some(ta), Some(tb)], seconds: 0.0, frame: 0, param: &param },
        )?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}

struct Select;

impl Operator for Select {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        if cx.text("node")?.trim().is_empty() {
            return Err(CookError::failed("set `node` to the path of a node"));
        }
        let Some(outs) = cx.referenced("node")? else { return Err(CookError::Pending) };
        let i = usize::try_from(cx.int("output")?).unwrap_or(0);
        let data = outs.get(i).ok_or_else(|| CookError::failed(format!("the node has no output {i}")))?;
        pass_on(data)
    }
}

/// Its state is the frame it passed last (the end of the loop, one frame back). A restart, or
/// `state.restore`, passes the saved frame instead of `initial` or `previous` until the end of
/// the loop has made something new from it, and the loop carries on from there. (Passing it only
/// once lost it when the end of the loop was not cooked in the same pass: the next frame fell
/// back to `initial`.)
struct Feedback {
    last: Option<Data>,
    /// A frame from a state snapshot, passed until `previous` moves on.
    restored: Option<Data>,
    /// What `previous` held when the restored frame was first passed (`Some(None)`: nothing).
    seen: Option<Option<Data>>,
    persist: bool,
}

impl Default for Feedback {
    fn default() -> Self {
        Self { last: None, restored: None, seen: None, persist: true }
    }
}

impl Operator for Feedback {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.persist = cx.bool("persist")?;
        let reset = cx.bool("reset")?;
        if reset {
            self.restored = None;
        }
        // `previous` holds what its source made last frame (it cooks after this node).
        let previous = if reset { None } else { cx.input(1) };
        if self.restored.is_some() {
            let moved_on = match (&self.seen, previous) {
                (Some(None), Some(_)) => true,
                (Some(Some(seen)), Some(p)) => !seen.same(p),
                _ => false,
            };
            if moved_on {
                self.restored = None;
                self.seen = None;
            } else if self.seen.is_none() {
                self.seen = Some(previous.cloned());
            }
        }
        let chosen = match (self.restored.clone(), previous, cx.input(0)) {
            (Some(r), _, _) => r,
            (None, Some(p), _) => p.clone(),
            (None, None, Some(i)) => i.clone(),
            (None, None, None) => return Err(waiting(cx)),
        };
        let cooked = pass_on(&chosen)?;
        self.last = Some(chosen);
        Ok(cooked)
    }

    fn save_state(&self) -> Option<Vec<StateItem>> {
        if !self.persist {
            return None;
        }
        let frame = self.restored.as_ref().or(self.last.as_ref())?;
        Some(vec![StateItem::Data(frame.clone())])
    }

    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        match items.as_slice() {
            [StateItem::Data(d)] if d.get::<Tex>().is_some() => {
                self.restored = Some(d.clone());
                self.seen = None;
                Ok(())
            }
            _ => Err("a feedback keeps one texture".into()),
        }
    }
}
