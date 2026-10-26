//! `chan.filter`: smooths channels over time, with a choice of filter types.
//!
//! Every type steps by the time since the last cook for channels with one sample per frame (so
//! skipped frames and an idle editor still give the right curve) and by the sample period for
//! channels with many samples. A channel's first sample passes unchanged; switching the type
//! carries on from the last output instead of jumping.
//!
//! The linear types of higher order (`critical`, `bessel`, `butterworth`) run their continuous
//! prototype in controllable canonical form, discretised exactly for each step (zero-order hold,
//! by the matrix exponential), so they stay exact and stable for any step length. Each prototype
//! is scaled in time so that its step response reaches 63 % after `rise`, as a first-order lag
//! does after its time constant: switching the type keeps the felt speed.

use std::collections::{BTreeMap, VecDeque};

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Tick, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::out;

/// The kind of the filter node.
pub const FILTER: &str = "chan.filter";

/// Highest order of the linear types.
const MAX_ORDER: usize = 4;
/// Most history entries `gaussian` weighs for each output sample; a longer window is shortened.
const GAUSSIAN_MAX: usize = 4096;
/// Most history entries `box` keeps per channel; a longer window is shortened.
const BOX_MAX: usize = 1 << 18;
/// Corner frequency of the speed estimate in `one_euro`, as in the paper by Casiez et al. (2012).
const EURO_SPEED_CUTOFF_HZ: f64 = 1.0;
/// A step longer than this many prototype time units lands on the steady state: the response
/// left over is below 1e-20.
const SETTLED: f64 = 50.0;

/// Registers `chan.filter`.
pub(super) fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                FILTER,
                "Smooths jumpy numbers over time: calms a sensor, softens a jump. From a plain lag to \
                 steeper filters, averages, a speed limit and an adaptive filter for tracking",
            )
            .stateful()
            .param(
                ParamSpec::choice(
                    "type",
                    &["first_order", "critical", "bessel", "butterworth", "gaussian", "box", "slew", "one_euro"],
                )
                .doc("How it smooths")
                .choice_doc(
                    "first_order",
                    "a lag with its own times up and down: 63 % of a step after `rise` or `fall`",
                )
                .choice_doc(
                    "critical",
                    "starts and stops softer than first_order and never overshoots; a higher `order` is softer",
                )
                .choice_doc("bessel", "keeps the shape of a movement best and overshoots by less than 1 %")
                .choice_doc(
                    "butterworth",
                    "removes fast wiggle most cleanly and overshoots a step: 4 % at order 2, 11 % at order 4",
                )
                .choice_doc(
                    "gaussian",
                    "average over the last `window` seconds, the newest values count most; lags about a \
                     quarter of `window`",
                )
                .choice_doc(
                    "box",
                    "plain average over the last `window` seconds; lags half of `window`, and a wobble \
                     that repeats every `window` seconds vanishes",
                )
                .choice_doc("slew", "moves at most `up` per second upwards and `down` per second downwards")
                .choice_doc(
                    "one_euro",
                    "smooths strongly while the input rests and follows fast moves with little lag: for \
                     tracked positions. `rise` sets the smoothing at rest, `beta` how fast it follows",
                ),
            )
            .param(
                ParamSpec::int("order", 2)
                    .clamp(2.0, MAX_ORDER as f64)
                    .range(2.0, MAX_ORDER as f64)
                    .doc("Steepness of critical, bessel and butterworth: a higher order lets less wiggle through and starts slower"),
            )
            .param(
                ParamSpec::float("rise", 0.2)
                    .clamp(0.0, 3600.0)
                    .range(0.0, 5.0)
                    .curve(Curve::Power(2.0))
                    .unit(Unit::Seconds)
                    .doc(
                        "Time to cover 63 % of a step up (first_order) or of any step (critical, bessel, \
                         butterworth); 0 follows at once. For one_euro the smoothing at rest",
                    ),
            )
            .param(
                ParamSpec::float("fall", 0.2)
                    .clamp(0.0, 3600.0)
                    .range(0.0, 5.0)
                    .curve(Curve::Power(2.0))
                    .unit(Unit::Seconds)
                    .doc("Time to cover 63 % of a step down"),
            )
            .param(
                ParamSpec::float("window", 0.5)
                    .clamp(0.0, 60.0)
                    .range(0.0, 5.0)
                    .curve(Curve::Power(2.0))
                    .unit(Unit::Seconds)
                    .doc("How far back the average reaches; 0 follows at once"),
            )
            .param(
                ParamSpec::float("up", 1.0)
                    .clamp(0.0, 1e9)
                    .range(0.0, 10.0)
                    .curve(Curve::Power(2.0))
                    .special(0.0, "no limit")
                    .doc("Fastest rise, in units per second"),
            )
            .param(
                ParamSpec::float("down", 1.0)
                    .clamp(0.0, 1e9)
                    .range(0.0, 10.0)
                    .curve(Curve::Power(2.0))
                    .special(0.0, "no limit")
                    .doc("Fastest fall, in units per second"),
            )
            .param(
                ParamSpec::float("beta", 1.0)
                    .clamp(0.0, 1e6)
                    .range(0.0, 10.0)
                    .curve(Curve::Power(2.0))
                    .doc(
                        "How much a fast move loosens the smoothing: 0 smooths like first_order, higher \
                         follows fast moves more closely. Depends on the units of the input",
                    ),
            )
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .time_dependent(),
            || Box::new(Filter::default()) as Box<dyn Operator>,
        )
        .inactive(inactive),
    )
}

/// Why param `name` does not count with the filter type of `node`.
fn inactive(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let kind = node.param(desc, "type").and_then(Value::as_str).unwrap_or("first_order");
    let linear = matches!(kind, "critical" | "bessel" | "butterworth");
    let window = matches!(kind, "gaussian" | "box");
    match name {
        "order" if !linear => Some("Only critical, bessel and butterworth have an order"),
        "rise" if window || kind == "slew" => Some("This type has no time constant; see `window` or `up`"),
        "fall" if kind != "first_order" => {
            Some("Only first_order has its own time down; the others treat both ways alike")
        }
        "window" if !window => Some("Only gaussian and box average over a window"),
        "up" | "down" if kind != "slew" => Some("Only slew limits the speed"),
        "beta" if kind != "one_euro" => Some("Only one_euro adapts to the speed"),
        _ => None,
    }
}

/// The filter's type with what it needs per cook.
enum Mode {
    FirstOrder {
        rise: f64,
        fall: f64,
    },
    /// A linear prototype (`key`: type and order) with its step discretised for this cook;
    /// `step` is `None` when `rise` is 0 (follows at once).
    Linear {
        key: (&'static str, usize),
        a0: f64,
        step: Option<Step>,
    },
    Gaussian {
        window: f64,
    },
    Box {
        window: f64,
    },
    Slew {
        up: f64,
        down: f64,
    },
    OneEuro {
        rise: f64,
        beta: f64,
    },
}

/// One exactly discretised step of a linear prototype: `x' = phi x + gamma u`, `y = a0 x[0]`.
#[derive(Clone, Copy)]
struct Step {
    phi: [[f64; MAX_ORDER]; MAX_ORDER],
    gamma: [f64; MAX_ORDER],
    a0: f64,
}

/// A channel's filter state: its last output and what the type keeps besides.
struct State {
    y: f64,
    inner: Inner,
}

enum Inner {
    /// first_order and slew need only the last output.
    None,
    /// The prototype's state, for the prototype (type and order) it was made with.
    Linear {
        key: (&'static str, usize),
        x: [f64; MAX_ORDER],
    },
    Window(Window),
    /// one_euro's smoothed speed.
    Euro {
        dx: f64,
    },
}

/// The recent samples of a channel: each holds its value for `w` seconds up to its time `t`.
#[derive(Default)]
struct Window {
    t: f64,
    entries: VecDeque<Entry>,
    /// Running sums over `entries` for box: value times weight, and weight.
    sum_xw: f64,
    sum_w: f64,
    /// Pushes since the sums were last recomputed, against creeping rounding error.
    pushes: u32,
}

#[derive(Clone, Copy)]
struct Entry {
    t: f64,
    x: f64,
    w: f64,
}

#[derive(Default)]
struct Filter {
    state: BTreeMap<Name, State>,
    last: Option<Tick>,
}

impl Operator for Filter {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let Some(input) = cx.input_as::<Chans>(0) else {
            self.state.clear();
            self.last = None;
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        // Going back in time (a seek) starts over, like a new node.
        let dt = match self.last {
            Some(last) if now.0 >= last.0 => Tick(now.0 - last.0).seconds(),
            _ => {
                self.state.clear();
                0.0
            }
        };
        self.last = Some(now);
        let step = if input.len() > 1 { 1.0 / input.rate() } else { dt };
        let mode = mode(cx, step)?;
        let mut shortened = None;
        let mut state = BTreeMap::new();
        let mut channels = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut s = self.state.remove(&ch.name);
            let mut samples = Vec::with_capacity(ch.samples.len());
            for &x in &ch.samples {
                let next = match s.take() {
                    None => State::fresh(x, &mode),
                    Some(prev) => prev.advance(x, step, &mode, &mut shortened),
                };
                samples.push(next.y);
                s = Some(next);
            }
            if let Some(s) = s {
                state.insert(ch.name.clone(), s);
            }
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        self.state = state;
        if let Some(seconds) = shortened {
            cx.warn(format!(
                "`window` is too long for this many samples per second; it averages over the last {seconds:.3} s"
            ));
        }
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
    }
}

/// The mode for this cook from the params, with the linear step for `step` seconds.
fn mode(cx: &CookContext<'_>, step: f64) -> Result<Mode, CookError> {
    let rise = cx.float("rise")?;
    Ok(match cx.text("type")? {
        kind @ ("critical" | "bessel" | "butterworth") => {
            let kind = match kind {
                "bessel" => "bessel",
                "butterworth" => "butterworth",
                _ => "critical",
            };
            let order = usize::try_from(cx.int("order")?).unwrap_or(2).clamp(2, MAX_ORDER);
            let proto = prototype(kind, order);
            let step = (rise > 0.0).then(|| discretise(&proto, step * proto.t63 / rise));
            Mode::Linear { key: (kind, order), a0: proto.a[0], step }
        }
        "gaussian" => Mode::Gaussian { window: cx.float("window")? },
        "box" => Mode::Box { window: cx.float("window")? },
        "slew" => Mode::Slew { up: cx.float("up")?, down: cx.float("down")? },
        "one_euro" => Mode::OneEuro { rise, beta: cx.float("beta")? },
        _ => Mode::FirstOrder { rise, fall: cx.float("fall")? },
    })
}

impl State {
    /// The state of a channel whose first sample is `x`.
    fn fresh(x: f64, mode: &Mode) -> State {
        State { y: x, inner: Inner::None }.adopted(mode)
    }

    /// This state with what `mode` keeps, started from the last output where it does not keep
    /// that yet (a new channel, a type or order switched): as if the input had rested there.
    fn adopted(self, mode: &Mode) -> State {
        let y = self.y;
        let inner = match (mode, self.inner) {
            (Mode::Linear { key, .. }, Inner::Linear { key: k, x }) if k == *key => {
                Inner::Linear { key: k, x }
            }
            (Mode::Linear { key, a0, .. }, _) => Inner::Linear { key: *key, x: steady(*a0, y) },
            (Mode::Gaussian { .. } | Mode::Box { .. }, Inner::Window(w)) => Inner::Window(w),
            (Mode::Gaussian { window } | Mode::Box { window }, _) => {
                Inner::Window(Window::resting(y, *window))
            }
            (Mode::OneEuro { .. }, Inner::Euro { dx }) => Inner::Euro { dx },
            (Mode::OneEuro { .. }, _) => Inner::Euro { dx: 0.0 },
            (Mode::FirstOrder { .. } | Mode::Slew { .. }, _) => Inner::None,
        };
        State { y, inner }
    }

    /// The state after sample `x`, `step` seconds after the last one.
    fn advance(self, x: f64, step: f64, mode: &Mode, shortened: &mut Option<f64>) -> State {
        let State { y, inner } = self.adopted(mode);
        if step <= 0.0 {
            // Cooked twice at the same time: nothing has passed.
            return State { y, inner };
        }
        match (mode, inner) {
            (Mode::FirstOrder { rise, fall }, inner) => {
                let tau = if x > y { *rise } else { *fall };
                let y = if tau <= 0.0 { x } else { y + (x - y) * (1.0 - (-step / tau).exp()) };
                State { y, inner }
            }
            (Mode::Linear { key, a0, step: None }, _) => {
                State { y: x, inner: Inner::Linear { key: *key, x: steady(*a0, x) } }
            }
            (Mode::Linear { key, step: Some(d), .. }, Inner::Linear { x: s, .. }) => {
                let mut next = [0.0; MAX_ORDER];
                for ((n, row), g) in next.iter_mut().zip(&d.phi).zip(&d.gamma) {
                    *n = row.iter().zip(&s).map(|(p, v)| p * v).sum::<f64>() + g * x;
                }
                State {
                    y: d.a0 * next.first().copied().unwrap_or(0.0),
                    inner: Inner::Linear { key: *key, x: next },
                }
            }
            (Mode::Gaussian { window }, Inner::Window(mut w)) => {
                let y = w.push(x, step, *window, GAUSSIAN_MAX, shortened);
                let y = y.unwrap_or_else(|| w.gaussian(*window));
                State { y, inner: Inner::Window(w) }
            }
            (Mode::Box { window }, Inner::Window(mut w)) => {
                let y = w.push(x, step, *window, BOX_MAX, shortened);
                let y = y.unwrap_or_else(|| w.average(*window));
                State { y, inner: Inner::Window(w) }
            }
            (Mode::Slew { up, down }, inner) => {
                let rise = if *up > 0.0 { up * step } else { f64::INFINITY };
                let fall = if *down > 0.0 { down * step } else { f64::INFINITY };
                State { y: y + (x - y).clamp(-fall, rise), inner }
            }
            (Mode::OneEuro { rise, beta }, Inner::Euro { dx }) => {
                if *rise <= 0.0 {
                    return State { y: x, inner: Inner::Euro { dx: 0.0 } };
                }
                let speed = (x - y) / step;
                let dx = dx + (speed - dx) * smoothing(EURO_SPEED_CUTOFF_HZ, step);
                let cutoff = 1.0 / (std::f64::consts::TAU * rise) + beta * dx.abs();
                State { y: y + (x - y) * smoothing(cutoff, step), inner: Inner::Euro { dx } }
            }
            // `adopted` made the inner state fit the mode.
            (_, inner) => State { y, inner },
        }
    }
}

/// How far a one-pole low pass with corner frequency `hz` moves towards its input in `step`.
fn smoothing(hz: f64, step: f64) -> f64 {
    1.0 - (-std::f64::consts::TAU * hz * step).exp()
}

/// The prototype state at rest with output `y`: only the first state is not 0.
fn steady(a0: f64, y: f64) -> [f64; MAX_ORDER] {
    [if a0 != 0.0 { y / a0 } else { 0.0 }, 0.0, 0.0, 0.0]
}

impl Window {
    /// A window that has held `y` for all of `window`.
    fn resting(y: f64, window: f64) -> Window {
        let mut w = Window::default();
        if window > 0.0 {
            w.entries.push_back(Entry { t: 0.0, x: y, w: window });
            w.sum_xw = y * window;
            w.sum_w = window;
        }
        w
    }

    /// Adds sample `x`, held for `step`, and drops what fell out of `window`; keeps at most `max`
    /// entries and notes in `shortened` the window that leaves. Returns `x` itself when the window
    /// is 0.
    fn push(
        &mut self,
        x: f64,
        step: f64,
        window: f64,
        max: usize,
        shortened: &mut Option<f64>,
    ) -> Option<f64> {
        self.t += step;
        if window <= 0.0 {
            self.entries.clear();
            self.sum_xw = 0.0;
            self.sum_w = 0.0;
            return Some(x);
        }
        // A window that came in from a type switch or a fresh start sits at time 0.
        self.entries.push_back(Entry { t: self.t, x, w: step });
        self.sum_xw += x * step;
        self.sum_w += step;
        let start = self.t - window;
        while let Some(front) = self.entries.front().copied() {
            if front.t > start && self.entries.len() <= max {
                break;
            }
            if front.t > start {
                *shortened = Some(self.t - front.t);
            }
            self.entries.pop_front();
            self.sum_xw -= front.x * front.w;
            self.sum_w -= front.w;
        }
        self.pushes += 1;
        if self.pushes >= 4096 {
            self.pushes = 0;
            self.sum_xw = self.entries.iter().map(|e| e.x * e.w).sum();
            self.sum_w = self.entries.iter().map(|e| e.w).sum();
        }
        None
    }

    /// The time-weighted average over the last `window` seconds.
    fn average(&self, window: f64) -> f64 {
        let start = self.t - window;
        // The oldest entry may reach back past the window's start: count only its part inside.
        let (mut xw, mut w) = (self.sum_xw, self.sum_w);
        if let Some(front) = self.entries.front() {
            let outside = (front.w - (front.t - start)).clamp(0.0, front.w);
            xw -= front.x * outside;
            w -= outside;
        }
        let last = self.entries.back().map_or(0.0, |e| e.x);
        if w > 1e-12 { xw / w } else { last }
    }

    /// The average over the last `window` seconds, weighted by a half Gaussian (sigma a third of
    /// the window) on each entry's age.
    fn gaussian(&self, window: f64) -> f64 {
        let start = self.t - window;
        let sigma = window / 3.0;
        let (mut xw, mut w) = (0.0, 0.0);
        for e in &self.entries {
            let held = e.w.min(e.t - start).max(0.0);
            if held <= 0.0 {
                continue;
            }
            let age = self.t - (e.t - held / 2.0);
            let g = held * (-(age * age) / (2.0 * sigma * sigma)).exp();
            xw += e.x * g;
            w += g;
        }
        let last = self.entries.back().map_or(0.0, |e| e.x);
        if w > 1e-300 { xw / w } else { last }
    }
}

/// A continuous linear prototype with unit gain at rest, as the monic denominator
/// `s^n + a[n-1] s^(n-1) + ... + a[0]` over the numerator `a[0]`, and the time its step response
/// takes to reach 63 %.
struct Prototype {
    order: usize,
    a: [f64; MAX_ORDER],
    t63: f64,
}

/// The prototype of `kind` at `order` (2 to 4). Bessel is normalised to unit group delay at rest,
/// Butterworth and critical (equal poles) to a corner at 1 rad/s; the `t63` values come from the
/// test that checks them against the step response.
fn prototype(kind: &str, order: usize) -> Prototype {
    let (a, t63) = match (kind, order) {
        ("bessel", 3) => ([15.0, 15.0, 6.0, 0.0], 1.134_415_453_501),
        ("bessel", 4) => ([105.0, 105.0, 45.0, 10.0], 1.125_672_175_339),
        ("bessel", _) => ([3.0, 3.0, 0.0, 0.0], 1.127_077_348_737),
        ("butterworth", 3) => ([1.0, 2.0, 2.0, 0.0], 2.469_343_206_031),
        ("butterworth", 4) => {
            ([1.0, 2.613_125_929_752_753, 3.414_213_562_373_095, 2.613_125_929_752_753], 3.168_511_622_233)
        }
        ("butterworth", _) => ([1.0, std::f64::consts::SQRT_2, 0.0, 0.0], 1.753_124_803_485),
        (_, 3) => ([1.0, 3.0, 3.0, 0.0], 3.258_252_468_867),
        (_, 4) => ([1.0, 4.0, 6.0, 4.0], 4.351_992_687_802),
        _ => ([1.0, 2.0, 1.0, 0.0], 2.146_193_220_621),
    };
    Prototype { order: order.clamp(2, MAX_ORDER), a, t63 }
}

/// Size of the augmented matrix: the states and the held input.
const AUG: usize = MAX_ORDER + 1;
type Matrix = [[f64; AUG]; AUG];

/// The prototype's step over `h` time units with the input held (zero-order hold): the
/// exponential of `[[A, b], [0, 0]] h`.
fn discretise(p: &Prototype, h: f64) -> Step {
    let a0 = p.a[0];
    let mut d = Step { phi: [[0.0; MAX_ORDER]; MAX_ORDER], gamma: [0.0; MAX_ORDER], a0 };
    if !h.is_finite() || h >= SETTLED {
        // Settled within the step: the state at rest for the held input.
        d.gamma = steady(a0, 1.0);
        return d;
    }
    let mut m: Matrix = [[0.0; AUG]; AUG];
    // Controllable canonical form: x[i]' = x[i+1], the last state takes the input.
    for (i, row) in m.iter_mut().enumerate().take(p.order) {
        if i + 1 < p.order {
            if let Some(v) = row.get_mut(i + 1) {
                *v = h;
            }
        } else {
            for (v, a) in row.iter_mut().zip(&p.a).take(p.order) {
                *v = -a * h;
            }
            if let Some(v) = row.last_mut() {
                *v = h;
            }
        }
    }
    let e = expm(&m);
    for ((phi, gamma), row) in d.phi.iter_mut().zip(d.gamma.iter_mut()).zip(&e) {
        for (p, v) in phi.iter_mut().zip(row) {
            *p = *v;
        }
        *gamma = row.last().copied().unwrap_or(0.0);
    }
    d
}

/// The matrix exponential by scaling and squaring around a Taylor series. The matrices here are
/// small and their norm modest, so 18 terms after scaling below 0.5 are accurate to rounding.
fn expm(m: &Matrix) -> Matrix {
    let norm = m.iter().map(|r| r.iter().map(|v| v.abs()).sum::<f64>()).fold(0.0, f64::max);
    let mut squarings = 0;
    let mut scale = 1.0;
    while norm * scale > 0.5 && squarings < 64 {
        scale *= 0.5;
        squarings += 1;
    }
    let x = m.map(|r| r.map(|v| v * scale));
    let mut sum = identity();
    let mut term = identity();
    for k in 1..=18 {
        term = mul(&term, &x).map(|r| r.map(|v| v / f64::from(k)));
        for (s, t) in sum.iter_mut().zip(&term) {
            for (a, b) in s.iter_mut().zip(t) {
                *a += b;
            }
        }
    }
    for _ in 0..squarings {
        sum = mul(&sum, &sum);
    }
    sum
}

fn identity() -> Matrix {
    let mut m = [[0.0; AUG]; AUG];
    for (i, row) in m.iter_mut().enumerate() {
        if let Some(v) = row.get_mut(i) {
            *v = 1.0;
        }
    }
    m
}

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut c = [[0.0; AUG]; AUG];
    for (ci, ai) in c.iter_mut().zip(a) {
        for (aik, bk) in ai.iter().zip(b) {
            for (cij, bkj) in ci.iter_mut().zip(bk) {
                *cij += aik * bkj;
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prototype's step response at time `t` from rest at 0.
    fn step_response(p: &Prototype, t: f64) -> f64 {
        let d = discretise(p, t);
        d.a0 * d.gamma[0]
    }

    /// First time the step response reaches 63.2 %, by scanning and bisection.
    fn first_63(p: &Prototype) -> f64 {
        let target = 1.0 - (-1.0f64).exp();
        let mut t = 0.0;
        while step_response(p, t + 0.01) < target {
            t += 0.01;
        }
        let (mut lo, mut hi) = (t, t + 0.01);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if step_response(p, mid) < target { lo = mid } else { hi = mid }
        }
        lo
    }

    #[test]
    fn prototypes_reach_63_percent_at_their_t63() {
        for kind in ["critical", "bessel", "butterworth"] {
            for order in 2..=4 {
                let p = prototype(kind, order);
                let t = first_63(&p);
                assert!((t - p.t63).abs() < 1e-9, "{kind} {order}: t63 is {t:.12}, table says {}", p.t63);
                // Unit gain at rest.
                assert!((step_response(&p, 200.0) - 1.0).abs() < 1e-12, "{kind} {order}");
            }
        }
    }

    #[test]
    fn overshoot_as_documented() {
        let peak = |kind: &str, order: usize| {
            let p = prototype(kind, order);
            (1..4000).map(|i| step_response(&p, f64::from(i) * 0.01)).fold(0.0, f64::max) - 1.0
        };
        for order in 2..=4 {
            assert!(peak("critical", order) < 1e-9, "critical {order}");
            assert!(peak("bessel", order) < 0.01, "bessel {order}");
        }
        assert!((peak("butterworth", 2) - 0.043).abs() < 0.001, "{}", peak("butterworth", 2));
        assert!((peak("butterworth", 4) - 0.108).abs() < 0.001, "{}", peak("butterworth", 4));
        assert!((peak("butterworth", 3) - 0.081).abs() < 0.002, "{}", peak("butterworth", 3));
    }

    #[test]
    fn steps_compose() {
        // Ten steps of 0.1 land where one step of 1 does.
        let p = prototype("butterworth", 4);
        let (small, big) = (discretise(&p, 0.1), discretise(&p, 1.0));
        let run = |d: &Step, n: usize| {
            let mut x = [0.0; MAX_ORDER];
            for _ in 0..n {
                let mut next = [0.0; MAX_ORDER];
                for ((v, row), g) in next.iter_mut().zip(&d.phi).zip(&d.gamma) {
                    *v = row.iter().zip(&x).map(|(a, b)| a * b).sum::<f64>() + g;
                }
                x = next;
            }
            x
        };
        let (a, b) = (run(&small, 10), run(&big, 1));
        for (a, b) in a.iter().zip(&b) {
            assert!((a - b).abs() < 1e-12, "{a} {b}");
        }
    }
}
