//! Node types for this crate's tests. Channel data is one channel `value` at 60 Hz.

use std::time::{Duration, Instant};

use penumbra_core::{Chans, Name, NodePath, ParamSpec};

use crate::cook::CookContext;
use crate::job::{AsyncSlot, JobStatus};
use crate::network::Op;
use crate::node::{CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator, StateItem};

pub fn create(path: &str, kind: &str) -> Op {
    Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.to_string(), params: Default::default() }
}

pub fn chans(v: f64) -> Chans {
    Name::new("value").and_then(|n| Chans::single(n, v, 60.0)).unwrap()
}

fn chan(v: f64) -> Result<Cooked, CookError> {
    Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans(v))]))
}

fn input(cx: &CookContext<'_>, i: usize) -> f64 {
    cx.input_as::<Chans>(i).and_then(|c| c.value(None)).unwrap_or(0.0)
}

/// A closure as an operator.
struct F<C>(C);

impl<C: FnMut(&CookContext<'_>) -> Result<Cooked, CookError> + Send> Operator for F<C> {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        (self.0)(cx)
    }
}

fn simple<C>(types: &mut NodeTypes, desc: NodeDesc, cook: C)
where
    C: Fn(&CookContext<'_>) -> Result<Cooked, CookError> + Clone + Send + Sync + 'static,
{
    types.register(FnNodeType::new(desc, move || Box::new(F(cook.clone())) as Box<dyn Operator>)).unwrap();
}

pub fn test_types() -> NodeTypes {
    let mut t = NodeTypes::new();
    let value = || ParamSpec::float("value", 0.0);
    simple(
        &mut t,
        NodeDesc::new("test.const", "A number").param(value()).output("out", DataKind::Chan),
        |cx| chan(cx.float("value")?),
    );
    simple(
        &mut t,
        NodeDesc::new("test.add", "a + b")
            .input("a", DataKind::Chan)
            .input("b", DataKind::Chan)
            .output("out", DataKind::Chan),
        |cx| chan(input(cx, 0) + input(cx, 1)),
    );
    simple(
        &mut t,
        NodeDesc::new("test.time", "Seconds").time_dependent().output("out", DataKind::Chan),
        |cx| chan(cx.time().seconds()),
    );
    simple(
        &mut t,
        NodeDesc::new("test.sneaky", "Reads time undeclared").output("out", DataKind::Chan),
        |cx| chan(cx.time().seconds()),
    );
    simple(
        &mut t,
        NodeDesc::new("test.fail", "Fails on request")
            .param(value())
            .param(ParamSpec::bool("fail", false))
            .output("out", DataKind::Chan),
        |cx| {
            if cx.bool("fail")? { Err(CookError::failed("asked to fail")) } else { chan(cx.float("value")?) }
        },
    );
    simple(
        &mut t,
        NodeDesc::new("test.panic", "Panics on request")
            .param(ParamSpec::bool("boom", false))
            .output("out", DataKind::Chan),
        |cx| if cx.bool("boom")? { panic!("boom") } else { chan(1.0) },
    );
    simple(&mut t, NodeDesc::new("test.liar", "Wrong output count").output("out", DataKind::Chan), |_| {
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Chan, chans(1.0)),
            Data::new(DataKind::Chan, chans(2.0)),
        ]))
    });
    simple(
        &mut t,
        NodeDesc::new("test.slowcook", "Takes 2 ms").param(value()).output("out", DataKind::Chan),
        |cx| {
            std::thread::sleep(Duration::from_millis(2));
            chan(cx.float("value")?)
        },
    );
    simple(&mut t, NodeDesc::new("test.tex", "A texture").output("out", DataKind::Tex), |_| {
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, ())]))
    });
    simple(&mut t, NodeDesc::new("test.sink", "Consumes").input("in", DataKind::Chan), |_| {
        Ok(Cooked::Outputs(vec![]))
    });
    simple(&mut t, NodeDesc::new("test.group", "Holds nodes").container(), |_| Ok(Cooked::Outputs(vec![])));
    simple(
        &mut t,
        NodeDesc::new("test.comp", "A component")
            .param(ParamSpec::text(crate::EXPOSE, "").multiline())
            .container(),
        |_| Ok(Cooked::Outputs(vec![])),
    );
    let pass = |cx: &CookContext<'_>| match cx.input(0) {
        Some(d) => Ok(Cooked::Outputs(vec![d.clone()])),
        None => Err(CookError::failed("nothing connected")),
    };
    simple(
        &mut t,
        NodeDesc::new("test.in", "Component input")
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .port(crate::PortRole::In),
        pass,
    );
    simple(
        &mut t,
        NodeDesc::new("test.out", "Component output")
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .port(crate::PortRole::Out),
        pass,
    );
    t.register(FnNodeType::new(
        NodeDesc::new("test.open", "Params per instance; reads the file `file` and a provided f64")
            .param(ParamSpec::file("file", &[]))
            .extra_params()
            .output("out", DataKind::Chan),
        || Box::new(Open::default()) as Box<dyn Operator>,
    ))
    .unwrap();
    t.register(FnNodeType::new(
        NodeDesc::new("test.slow", "value * 2 after `ms` milliseconds, off the cook thread")
            .param(value())
            .param(ParamSpec::int("ms", 10))
            .output("out", DataKind::Chan),
        || Box::new(Slow::default()) as Box<dyn Operator>,
    ))
    .unwrap();
    // Passes the wired input at `index` (counted over wired inputs); with `skip` it needs only
    // that one, else all.
    let pick_desc = NodeDesc::new("test.pick", "One of its inputs")
        .param(ParamSpec::float("index", 0.0))
        .param(ParamSpec::bool("skip", false))
        .input("a", DataKind::Chan)
        .input("b", DataKind::Chan)
        .input("c", DataKind::Chan)
        .output("out", DataKind::Chan);
    let pick = |cx: &CookContext<'_>| {
        let wired: Vec<usize> = (0..3).filter(|i| cx.def().inputs[*i].is_some()).collect();
        let i = cx.float("index")?.max(0.0) as usize;
        let slot = *wired.get(i.min(wired.len().saturating_sub(1))).ok_or(CookError::Pending)?;
        match cx.input(slot) {
            Some(d) => Ok(Cooked::Outputs(vec![d.clone()])),
            None => Err(CookError::Pending),
        }
    };
    t.register(FnNodeType::new(pick_desc, move || Box::new(F(pick)) as Box<dyn Operator>).choose_inputs(
        |cx| {
            if !cx.bool("skip")? {
                return None;
            }
            let wired: Vec<usize> = (0..3).filter(|i| cx.connected(*i)).collect();
            let i = cx.float("index")?.max(0.0) as usize;
            Some(wired.get(i.min(wired.len().saturating_sub(1))).copied().into_iter().collect())
        },
    ))
    .unwrap();
    // Outputs `a` and `b` as their params; an output whose param did not change is handed on
    // as the same data.
    t.register(FnNodeType::new(
        NodeDesc::new("test.pair", "Two numbers, each kept while its param stays")
            .param(ParamSpec::float("a", 0.0))
            .param(ParamSpec::float("b", 0.0))
            .output("a", DataKind::Chan)
            .output("b", DataKind::Chan),
        || Box::new(Pair::default()) as Box<dyn Operator>,
    ))
    .unwrap();
    // `out` is `value`; the lazy `extra` is `value + 100` while read, skipped otherwise.
    simple(
        &mut t,
        NodeDesc::new("test.lazy", "A number, and a lazy second one")
            .param(value())
            .output("out", DataKind::Chan)
            .output("extra", DataKind::Chan)
            .lazy_output("extra"),
        |cx| {
            let v = cx.float("value")?;
            let extra = if cx.output_wanted(1) {
                Data::new(DataKind::Chan, chans(v + 100.0))
            } else {
                Data::skipped(DataKind::Chan)
            };
            Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans(v)), extra]))
        },
    );
    t.register(FnNodeType::new(
        NodeDesc::new("test.counter", "Counts its cooks (time-dependent); keeps the count as state")
            .time_dependent()
            .output("out", DataKind::Chan),
        || Box::new(Counter(0)) as Box<dyn Operator>,
    ))
    .unwrap();
    t
}

/// Counts cooks; the count is its state, saved as text.
struct Counter(u64);

impl Operator for Counter {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let _ = cx.time();
        self.0 += 1;
        chan(self.0 as f64)
    }

    fn save_state(&self) -> Option<Vec<StateItem>> {
        Some(vec![StateItem::Bytes(self.0.to_string().into_bytes())])
    }

    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        let [StateItem::Bytes(b)] = items.as_slice() else { return Err("expected one count".into()) };
        self.0 = std::str::from_utf8(b).ok().and_then(|t| t.parse().ok()).ok_or("not a count")?;
        Ok(())
    }
}

/// Declares `gain` per instance; time-dependent while `animate` is set; output is
/// `gain * provided f64 * length of file` (+ seconds when animated).
#[derive(Default)]
struct Open {
    animate: bool,
    watch: Option<crate::FileWatch>,
}

impl Operator for Open {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        for name in cx.def().params.keys() {
            if !["file", "gain", "animate"].contains(&name.as_str()) {
                cx.warn(format!("unknown param `{name}`"));
            }
        }
        self.animate = cx.param("animate").and_then(penumbra_core::Value::as_bool).unwrap_or(false);
        let gain = cx.param("gain").and_then(penumbra_core::Value::as_f64).unwrap_or(1.0);
        let factor = cx.resource::<f64>().copied().unwrap_or(1.0);
        let path = cx.resolve_path(cx.text("file")?)?;
        if self.watch.as_ref().is_none_or(|w| w.path() != path) {
            self.watch = Some(cx.watch_file(&path));
        }
        let len = std::fs::metadata(&path).map(|m| m.len() as f64).unwrap_or(0.0);
        let t = if self.animate { cx.time().seconds() } else { 0.0 };
        chan(gain * factor * len + t)
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.animate)
    }

    fn params(&self) -> Option<Vec<ParamSpec>> {
        Some(vec![ParamSpec::float("gain", 1.0), ParamSpec::bool("animate", false)])
    }

    fn density(&self, _def: &crate::NodeDef, _desc: &NodeDesc, _input: usize) -> crate::Density {
        crate::Density::needs([2.0; 2])
    }
}

#[derive(Default)]
struct Slow {
    slot: AsyncSlot<f64>,
}

impl Operator for Slow {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        if cx.first_cook() || cx.params_changed() {
            let value = cx.float("value")?;
            let ms = u64::try_from(cx.int("ms")?).unwrap_or(0);
            self.slot.start(cx.spawn(move |job| {
                let until = Instant::now() + Duration::from_millis(ms);
                while Instant::now() < until {
                    if job.cancelled() {
                        return Err("cancelled".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(value * 2.0)
            }));
        }
        match (fresh, self.slot.latest()) {
            (true, Some(v)) => chan(*v),
            (_, None) => Err(CookError::Pending),
            (false, Some(_)) => Ok(Cooked::Unchanged),
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

#[derive(Default)]
struct Pair {
    last: [Option<(f64, Data)>; 2],
}

impl Operator for Pair {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let mut outs = Vec::new();
        for (i, name) in ["a", "b"].into_iter().enumerate() {
            let v = cx.float(name)?;
            let data = match &self.last[i] {
                Some((old, d)) if *old == v => d.clone(),
                _ => Data::new(DataKind::Chan, chans(v)),
            };
            self.last[i] = Some((v, data.clone()));
            outs.push(data);
        }
        Ok(Cooked::Outputs(outs))
    }
}
