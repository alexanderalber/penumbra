//! `tex` nodes that route textures rather than compute them: null, switch, select, feedback.
//! Feedback is two plain inputs, `initial` and `previous`, where `previous` is wired back from
//! the end of the loop and reads that node's result from the previous frame (TouchDesigner's target
//! param got wired the wrong way round every time). They pass the texture they choose on as it is
//! (no copy), except a switch blending two inputs.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::{Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, StateItem,
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
        .param(ParamSpec::bool("blend", false).doc("A fractional index mixes the two inputs either side"));
    for i in 0..SWITCH_INPUTS {
        switch = switch.input(&format!("in{i}"), DataKind::Tex);
    }
    types.register(FnNodeType::new(switch.output("out", DataKind::Tex), || {
        Box::new(Switch { ring: TexRing::new(), cross: None }) as Box<dyn Operator>
    }))?;

    types.register(FnNodeType::new(
        NodeDesc::new("tex.select", "The output of another node, named by path: a wire without the wire")
            .param(
                ParamSpec::node("node")
                    .doc("Path of the node (/name), or the name of a node beside this one"),
            )
            .param(ParamSpec::int("output", 0).clamp(0.0, 63.0).rarely_used().doc("Which of its outputs"))
            .output("out", DataKind::Tex),
        || Box::new(Select) as Box<dyn Operator>,
    ))?;

    types.register(FnNodeType::new(
        NodeDesc::new(
            "tex.feedback",
            "A loop: wire the end of the chain back into `previous`; `initial` until that has a frame",
        )
        .param(ParamSpec::bool("reset", false).doc("Pass `initial` instead, while on"))
        .param(
            ParamSpec::bool("persist", true)
                .rarely_used()
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

impl Operator for Switch {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let connected: Vec<&Data> = (0..SWITCH_INPUTS).filter_map(|i| cx.input(i)).collect();
        let Some(last) = connected.len().checked_sub(1) else { return Err(waiting(cx)) };
        let index = cx.float("index")?;
        let index = if index.is_finite() { index.clamp(0.0, last as f64) } else { 0.0 };
        let lower = index.floor() as usize;
        let t = index - index.floor();
        let pick = |i: usize| connected.get(i.min(last)).copied();
        let (Some(a), Some(b)) = (pick(lower), pick(lower + 1)) else { return Err(waiting(cx)) };
        if !cx.bool("blend")? || t < 1e-6 || a.same(b) {
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
        );
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
/// back to `initial`, seen in the app on 2026-10-10.)
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
