//! `chan.envelope`: attack, decay, sustain and release on a gate or a trigger, per channel.
//!
//! The envelope is made of straight segments, so each cook steps it by the exact time since the
//! last one: slow frames, skipped frames and an idle editor give the same curve as a steady 60
//! fps. A change of the gate counts at the time of the sample that shows it; the envelope runs
//! from there, so the curve does not depend on how long the frame before it took. Channels with
//! many samples per frame step by the sample period, sample by sample.

use std::collections::BTreeMap;

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Tick, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::out;

/// The kind of the envelope node.
pub const ENVELOPE: &str = "chan.envelope";

/// Registers `chan.envelope`.
pub(super) fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let time = |name: &str, default: f64, doc: &str| {
        ParamSpec::float(name, default)
            .clamp(0.0, 3600.0)
            .range(0.0, 5.0)
            .curve(Curve::Power(2.0))
            .unit(Unit::Seconds)
            .doc(doc)
    };
    types.register(
        FnNodeType::new(
            NodeDesc::new(
                ENVELOPE,
                "Turns a gate or a trigger into a rise and fall (attack, decay, sustain, release): a \
                 flash on a beat, a light that swells while a button is held",
            )
            .stateful()
            .param(
                ParamSpec::choice("mode", &["gate", "trigger"])
                    .doc("What the input means")
                    .choice_doc(
                        "gate",
                        "on while above `threshold`: attack and decay, sustain while on, release when it drops",
                    )
                    .choice_doc(
                        "trigger",
                        "each rise above `threshold` starts the whole envelope: attack, decay, `hold` at \
                         sustain, release. For one-frame presses and beats",
                    ),
            )
            .param(time("attack", 0.05, "Time from 0 to 1 after the gate opens; 0 jumps to 1"))
            .param(time("decay", 0.2, "Time from 1 down to `sustain`"))
            .param(
                ParamSpec::float("sustain", 0.7)
                    .clamp(0.0, 1.0)
                    .range(0.0, 1.0)
                    .doc("The level held while the gate stays on (in trigger mode: for `hold`)"),
            )
            .param(time("release", 0.5, "Time from the level at release down to 0"))
            .param(time("hold", 0.0, "Trigger mode: how long the level stays at `sustain` before the release"))
            .param(
                ParamSpec::float("threshold", 0.5)
                    .range(0.0, 1.0)
                    .doc("The input counts as on above this value"),
            )
            .input("in", DataKind::Chan)
            .output("out", DataKind::Chan)
            .describe_input("in", "Gates or triggers, one envelope per channel")
            .describe_output("out", "One envelope per input channel, 0 to 1, with the input's names")
            .time_dependent(),
            || Box::new(EnvelopeNode::default()) as Box<dyn Operator>,
        )
        .inactive(inactive),
    )
}

/// Why param `name` does not count with the mode of `node`.
fn inactive(node: &NodeDef, desc: &NodeDesc, name: &str) -> Option<&'static str> {
    let mode = node.param(desc, "mode").and_then(Value::as_str).unwrap_or("gate");
    match name {
        "hold" if mode != "trigger" => Some("Only trigger mode holds; a gate sustains while it is on"),
        _ => None,
    }
}

/// The envelope's shape for this cook.
#[derive(Clone, Copy, Debug)]
struct Shape {
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    hold: f64,
    trigger: bool,
    threshold: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    /// Trigger mode: at sustain for this many seconds more.
    Hold(f64),
    /// Falling by this much per second.
    Release(f64),
}

/// One channel's envelope.
#[derive(Clone, Copy, Debug)]
struct Voice {
    level: f64,
    stage: Stage,
    gate: bool,
}

impl Voice {
    const IDLE: Voice = Voice { level: 0.0, stage: Stage::Idle, gate: false };

    /// Runs the envelope on for `dt` seconds.
    fn advance(&mut self, mut dt: f64, s: &Shape) {
        // Each pass ends a stage or uses up the time; six stages at most per call.
        for _ in 0..8 {
            if dt.is_nan() || dt <= 0.0 {
                return;
            }
            match self.stage {
                Stage::Idle => {
                    self.level = 0.0;
                    return;
                }
                Stage::Attack => {
                    if s.attack <= 0.0 || self.level >= 1.0 {
                        self.level = 1.0;
                        self.stage = Stage::Decay;
                        continue;
                    }
                    let need = (1.0 - self.level) * s.attack;
                    if dt < need {
                        self.level += dt / s.attack;
                        return;
                    }
                    dt -= need;
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
                Stage::Decay => {
                    let rate = if s.decay > 0.0 { (1.0 - s.sustain) / s.decay } else { f64::INFINITY };
                    let need = if self.level > s.sustain { (self.level - s.sustain) / rate } else { 0.0 };
                    if dt < need {
                        self.level -= dt * rate;
                        return;
                    }
                    dt -= need;
                    self.level = s.sustain;
                    self.stage = if s.trigger { Stage::Hold(s.hold) } else { Stage::Sustain };
                }
                Stage::Sustain => {
                    self.level = s.sustain;
                    return;
                }
                Stage::Hold(left) => {
                    self.level = s.sustain;
                    if dt < left {
                        self.stage = Stage::Hold(left - dt);
                        return;
                    }
                    dt -= left;
                    self.release(s);
                }
                Stage::Release(rate) => {
                    let need = self.level / rate;
                    if dt < need {
                        self.level -= dt * rate;
                        return;
                    }
                    dt -= need;
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
    }

    /// Starts the release from the level now, so it takes `release` seconds whatever the level.
    fn release(&mut self, s: &Shape) {
        self.stage = if s.release > 0.0 && self.level > 0.0 {
            Stage::Release(self.level / s.release)
        } else {
            self.level = 0.0;
            Stage::Idle
        };
    }

    /// Takes the gate's new value `x`: a rise starts the attack from the level now (no jump), a
    /// fall in gate mode the release.
    fn gate(&mut self, x: f64, s: &Shape) {
        let on = x > s.threshold;
        if on && !self.gate {
            self.stage = Stage::Attack;
            if s.attack <= 0.0 {
                self.level = 1.0;
                self.stage = Stage::Decay;
            }
        } else if !on && self.gate && !s.trigger && !matches!(self.stage, Stage::Release(_) | Stage::Idle) {
            self.release(s);
        }
        self.gate = on;
    }

    /// A trigger that switches to gate mode holds at sustain; one that switches to trigger mode
    /// mid-sustain holds for `hold` and releases.
    fn adapt(&mut self, s: &Shape) {
        match self.stage {
            Stage::Sustain if s.trigger => self.stage = Stage::Hold(s.hold),
            Stage::Hold(_) if !s.trigger => {
                if self.gate {
                    self.stage = Stage::Sustain;
                } else {
                    self.release(s);
                }
            }
            Stage::Sustain if !self.gate => self.release(s),
            _ => {}
        }
    }
}

fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

#[derive(Default)]
struct EnvelopeNode {
    voices: BTreeMap<Name, Voice>,
    last: Option<Tick>,
}

impl Operator for EnvelopeNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let now = cx.time().tick;
        let Some(input) = cx.input_as::<Chans>(0) else {
            self.voices.clear();
            self.last = None;
            return out(Chans::empty(cx.rate().as_f64())?);
        };
        // Going back in time (a seek) starts over, like a new node.
        let dt = match self.last {
            Some(last) if now.0 >= last.0 => Tick(now.0 - last.0).seconds(),
            _ => {
                self.voices.clear();
                0.0
            }
        };
        self.last = Some(now);
        let shape = Shape {
            attack: finite(cx.float("attack")?, 0.0).max(0.0),
            decay: finite(cx.float("decay")?, 0.0).max(0.0),
            sustain: finite(cx.float("sustain")?, 0.0).clamp(0.0, 1.0),
            release: finite(cx.float("release")?, 0.0).max(0.0),
            hold: finite(cx.float("hold")?, 0.0).max(0.0),
            trigger: cx.text("mode")? == "trigger",
            threshold: finite(cx.float("threshold")?, 0.5),
        };
        let step = if input.len() > 1 { 1.0 / input.rate() } else { dt };
        let mut voices = BTreeMap::new();
        let mut channels = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut voice = self.voices.remove(&ch.name);
            let mut samples = Vec::with_capacity(ch.samples.len());
            for &x in &ch.samples {
                let v = match &mut voice {
                    Some(v) => {
                        v.adapt(&shape);
                        v.advance(step, &shape);
                        v
                    }
                    None => voice.insert(Voice::IDLE),
                };
                v.gate(x, &shape);
                samples.push(v.level);
            }
            if let Some(v) = voice {
                voices.insert(ch.name.clone(), v);
            }
            channels.push(Channel { name: ch.name.clone(), samples });
        }
        self.voices = voices;
        out(Chans::new(input.rate(), channels)?.shaped_like(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(trigger: bool) -> Shape {
        Shape { attack: 0.1, decay: 0.2, sustain: 0.5, release: 0.4, hold: 0.3, trigger, threshold: 0.5 }
    }

    /// The level after the gate `x` at each of `times` (seconds), stepped from one to the next.
    fn run(s: &Shape, events: &[(f64, f64)], until: f64, step: f64) -> Vec<(f64, f64)> {
        let mut v = Voice::IDLE;
        let mut t = 0.0;
        let mut gate = 0.0;
        let mut out = Vec::new();
        let mut next = events.iter().peekable();
        while t <= until + 1e-9 {
            while let Some((at, x)) = next.peek() {
                if *at <= t + 1e-9 {
                    gate = *x;
                    next.next();
                } else {
                    break;
                }
            }
            v.gate(gate, s);
            out.push((t, v.level));
            v.advance(step, s);
            t += step;
        }
        out
    }

    #[test]
    fn a_gate_rises_decays_sustains_and_releases_on_time() {
        let s = shape(false);
        let mut v = Voice::IDLE;
        v.gate(1.0, &s);
        assert_eq!(v.level, 0.0);
        v.advance(0.05, &s);
        assert!((v.level - 0.5).abs() < 1e-12, "half way up: {}", v.level);
        v.advance(0.05 + 0.1, &s);
        assert!((v.level - 0.75).abs() < 1e-12, "half way down to sustain: {}", v.level);
        v.advance(10.0, &s);
        assert_eq!((v.level, v.stage), (0.5, Stage::Sustain));
        v.gate(0.0, &s);
        v.advance(0.2, &s);
        assert!((v.level - 0.25).abs() < 1e-12, "{}", v.level);
        v.advance(0.2, &s);
        assert_eq!((v.level, v.stage), (0.0, Stage::Idle));
    }

    #[test]
    fn a_release_during_the_attack_takes_the_release_time_from_there() {
        let s = shape(false);
        let mut v = Voice::IDLE;
        v.gate(1.0, &s);
        v.advance(0.08, &s);
        v.gate(0.0, &s);
        assert!((v.level - 0.8).abs() < 1e-12);
        v.advance(0.2, &s);
        assert!((v.level - 0.4).abs() < 1e-12, "{}", v.level);
        // A new gate during the release climbs from where it is, without a jump.
        v.gate(1.0, &s);
        v.advance(0.01, &s);
        assert!((v.level - 0.5).abs() < 1e-12, "{}", v.level);
    }

    #[test]
    fn a_one_frame_trigger_plays_the_whole_envelope() {
        let s = shape(true);
        let mut v = Voice::IDLE;
        v.gate(1.0, &s);
        v.advance(1.0 / 60.0, &s);
        v.gate(0.0, &s);
        // Attack 0.1, decay 0.2, hold 0.3: at 0.5 s still at sustain, then down over 0.4 s.
        v.advance(0.5 - 1.0 / 60.0, &s);
        assert!((v.level - 0.5).abs() < 1e-12 && matches!(v.stage, Stage::Hold(_)), "{v:?}");
        v.advance(0.1 + 0.2, &s);
        assert!((v.level - 0.25).abs() < 1e-9, "{v:?}");
        v.advance(1.0, &s);
        assert_eq!(v.stage, Stage::Idle);
        // In gate mode the same one-frame press only starts the attack.
        let g = shape(false);
        let mut v = Voice::IDLE;
        v.gate(1.0, &g);
        v.advance(1.0 / 60.0, &g);
        v.gate(0.0, &g);
        assert!(matches!(v.stage, Stage::Release(_)) && v.level < 0.2, "{v:?}");
    }

    #[test]
    fn the_curve_is_the_same_at_any_frame_rate() {
        // Events on multiples of half a second, sampled at 60, 30, 10 and 2 fps: wherever the
        // slower runs have a sample, the level matches 60 fps.
        for trigger in [false, true] {
            let s = shape(trigger);
            let events = [(0.0, 1.0), (0.5, 0.0), (1.5, 1.0), (2.0, 0.0)];
            let fast = run(&s, &events, 3.0, 1.0 / 60.0);
            for step in [1.0 / 30.0, 0.1, 0.5] {
                for (t, level) in run(&s, &events, 3.0, step) {
                    let Some((_, want)) = fast.iter().find(|(f, _)| (f - t).abs() < 1e-6) else {
                        continue;
                    };
                    assert!(
                        (level - want).abs() < 1e-9,
                        "trigger {trigger}, step {step}, t {t}: {level} vs {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_times_jump_and_odd_numbers_do_not_break_it() {
        let s = Shape {
            attack: 0.0,
            decay: 0.0,
            sustain: 0.3,
            release: 0.0,
            hold: 0.0,
            trigger: false,
            threshold: 0.5,
        };
        let mut v = Voice::IDLE;
        v.gate(1.0, &s);
        assert_eq!(v.level, 1.0);
        v.advance(1e-6, &s);
        assert_eq!(v.level, 0.3);
        v.gate(0.0, &s);
        assert_eq!((v.level, v.stage), (0.0, Stage::Idle));
        v.gate(f64::NAN, &s);
        v.advance(f64::NAN, &s);
        v.advance(f64::INFINITY, &s);
        assert!(v.level.is_finite());
    }
}
