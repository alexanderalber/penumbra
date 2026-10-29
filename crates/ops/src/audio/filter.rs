//! `audio.filter`: a filter for any sound, every channel on its own.
//!
//! The core is the topology-preserving state-variable filter of [`super::synth`] (Zavalishin,
//! in Simper's form), stable for every cutoff below Nyquist and every damping above 0, also
//! while both move. Every type is one sum of the filter's input, band and low outputs
//! (Simper's mixing for the bell and the shelves), so a change of type glides like any other
//! param. `slope` 24 cascades a second stage for lowpass and highpass, tuned as a fourth-order
//! Butterworth at the default resonance; for the other types the second stage passes the sound
//! on unchanged.
//!
//! `resonance` 0..1 maps to Q = 0.5 * 40^(resonance^1.5): 0.5 at 0, about 0.7 (Butterworth) at
//! the default 0.2, 2.8 at 0.6, 20 at 1.
//!
//! Smoothing: the params are constant within a cook, so their coefficients are worked out once
//! per block, and every coefficient glides towards its new value per sample, 90% of the way in
//! [`GLIDE_SECONDS`]. A bound or swept cutoff therefore never steps.
//!
//! Blocks: the state per channel name carries across blocks. The input decides what a block
//! is. A new input block continues from the state at the end of the last one. The same input
//! at the same tick (a recook after a param change) is filtered again from the state at the
//! start of that block. The same input at a later tick (a device that delivered nothing) hands
//! on the last output unchanged, so nothing downstream hears it twice. A change of sample rate
//! starts every channel over; a channel name seen for the first time starts from silence.

use std::f64::consts::PI;

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Tick, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::synth::{Svf, SvfCoef, TOP, glide};

/// Kind of the filter.
pub const FILTER: &str = "audio.filter";
/// The filter types, in the order of the `type` choice.
pub const TYPES: [&str; 7] = ["lowpass", "highpass", "bandpass", "notch", "peak", "low_shelf", "high_shelf"];
/// Seconds every coefficient takes to get 90% of the way to a new value.
pub const GLIDE_SECONDS: f64 = 0.015;
/// The lowest cutoff, Hz.
pub const MIN_CUTOFF: f64 = 20.0;
/// The highest cutoff the param offers, Hz; the filter also stays below [`TOP`] of the rate.
pub const MAX_CUTOFF: f64 = 20_000.0;
/// Input samples beyond this (60 dB above full scale) are clipped to it, so a blast cannot fill
/// the state with numbers that take seconds to ring down.
pub const CLIP: f64 = 1000.0;
/// The Qs of a fourth-order Butterworth's two stages.
const BUTTERWORTH_4: [f64; 2] = [0.541_196, 1.306_563];
/// The Q of a second-order Butterworth.
const BUTTERWORTH_2: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
    Peak,
    LowShelf,
    HighShelf,
}

impl Kind {
    pub fn parse(name: &str) -> Self {
        match name {
            "highpass" => Self::Highpass,
            "bandpass" => Self::Bandpass,
            "notch" => Self::Notch,
            "peak" => Self::Peak,
            "low_shelf" => Self::LowShelf,
            "high_shelf" => Self::HighShelf,
            _ => Self::Lowpass,
        }
    }
}

/// The Q of `resonance` (0..1; not finite counts as the default 0.2).
pub fn q_of(resonance: f64) -> f64 {
    let r = if resonance.is_finite() { resonance.clamp(0.0, 1.0) } else { 0.2 };
    0.5 * 40f64.powf(r.powf(1.5))
}

/// What the params ask for.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub kind: Kind,
    /// Two stages (24 dB per octave) for lowpass and highpass.
    pub steep: bool,
    pub cutoff: f64,
    pub resonance: f64,
    /// dB, for peak and the shelves.
    pub gain: f64,
    /// 0 is the input as it is, 1 the filtered sound.
    pub mix: f64,
}

/// Coefficients of both stages and the mix: per stage the prewarped cutoff `g`, the damping
/// `k` and the weights of input, band and low output.
type Coefs = [f64; 11];
const STAGE: usize = 5;
const MIX: usize = 10;

fn stage(g: f64, k: f64, m: [f64; 3]) -> [f64; STAGE] {
    [g, k, m[0], m[1], m[2]]
}

/// The coefficients `s` asks for at `rate` samples per second.
pub fn design(s: &Settings, rate: f64) -> Coefs {
    let hi = TOP * rate;
    let lo = MIN_CUTOFF.min(hi);
    let cutoff = if s.cutoff.is_finite() { s.cutoff.clamp(lo, hi) } else { 1000f64.clamp(lo, hi) };
    let g = (PI * cutoff / rate).tan();
    let q = q_of(s.resonance);
    let k = 1.0 / q;
    let db = if s.gain.is_finite() { s.gain.clamp(-48.0, 48.0) } else { 0.0 };
    let a = 10f64.powf(db / 40.0);
    // The second stage's damping at 24 dB: the default resonance gives a fourth-order
    // Butterworth, more resonance raises its peak as in the single stage.
    let (k1, k2) = (1.0 / BUTTERWORTH_4[0], 1.0 / (q * BUTTERWORTH_4[1] / BUTTERWORTH_2));
    let pass = stage(g, 2.0, [1.0, 0.0, 0.0]);
    let lowpass = |k: f64| stage(g, k, [0.0, 0.0, 1.0]);
    let highpass = |k: f64| stage(g, k, [1.0, -k, -1.0]);
    let (one, two) = match s.kind {
        Kind::Lowpass if s.steep => (lowpass(k1), lowpass(k2)),
        Kind::Lowpass => (lowpass(k), pass),
        Kind::Highpass if s.steep => (highpass(k1), highpass(k2)),
        Kind::Highpass => (highpass(k), pass),
        Kind::Bandpass => (stage(g, k, [0.0, k, 0.0]), pass),
        Kind::Notch => (stage(g, k, [1.0, -k, 0.0]), pass),
        Kind::Peak => {
            let kp = 1.0 / (q * a);
            (stage(g, kp, [1.0, kp * (a * a - 1.0), 0.0]), pass)
        }
        Kind::LowShelf => (stage(g / a.sqrt(), k, [1.0, k * (a - 1.0), a * a - 1.0]), pass),
        Kind::HighShelf => (stage(g * a.sqrt(), k, [a * a, k * (1.0 - a) * a, 1.0 - a * a]), pass),
    };
    let mut c = [0.0; 11];
    c[..STAGE].copy_from_slice(&one);
    c[STAGE..MIX].copy_from_slice(&two);
    c[MIX] = if s.mix.is_finite() { s.mix.clamp(0.0, 1.0) } else { 1.0 };
    c
}

/// The filter's state: the coefficients as they are now and every channel's two stages.
#[derive(Clone, Debug, Default)]
pub struct State {
    rate: f64,
    coefs: Option<Coefs>,
    chans: Vec<(Name, [Svf; 2])>,
}

/// One sample's coefficients, worked out once for all channels.
struct Step {
    /// Per stage the filter's coefficients and the weights of input, band and low output.
    stages: [(SvfCoef, [f64; 3]); 2],
    mix: f64,
}

impl State {
    /// Filters `input` towards the coefficients `target`, from this state on.
    pub fn run(&mut self, input: &Chans, target: &Coefs) -> Result<Chans, Error> {
        let rate = input.rate();
        if rate != self.rate {
            *self = Self { rate, ..Self::default() };
        }
        let len = input.len();
        let speed = glide(GLIDE_SECONDS, rate);
        let mut cur = self.coefs.unwrap_or(*target);
        let mut steps = Vec::with_capacity(len);
        for _ in 0..len {
            for (c, t) in cur.iter_mut().zip(target) {
                *c += (t - *c) * speed;
            }
            let coef = |g: f64, k: f64| SvfCoef::of_g(g.max(1e-9), k.max(1e-6));
            steps.push(Step {
                stages: [
                    (coef(cur[0], cur[1]), [cur[2], cur[3], cur[4]]),
                    (coef(cur[STAGE], cur[STAGE + 1]), [cur[STAGE + 2], cur[STAGE + 3], cur[STAGE + 4]]),
                ],
                mix: cur[MIX],
            });
        }
        self.coefs = Some(cur);
        let mut kept = Vec::with_capacity(input.channels().len());
        let mut out = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut svf =
                self.chans.iter().find(|(n, _)| *n == ch.name).map_or([Svf::default(); 2], |(_, s)| *s);
            let mut samples = Vec::with_capacity(ch.samples.len());
            for (&x, st) in ch.samples.iter().zip(&steps) {
                let x = if x.is_finite() { x.clamp(-CLIP, CLIP) } else { 0.0 };
                let mut y = x;
                for (f, (c, m)) in svf.iter_mut().zip(&st.stages) {
                    let (low, band, _) = f.tick(y, c);
                    y = m[0] * y + m[1] * band + m[2] * low;
                }
                let y = x + (y - x) * st.mix;
                if y.is_finite() && svf.iter().all(Svf::is_finite) {
                    samples.push(y);
                } else {
                    svf = [Svf::default(); 2];
                    samples.push(0.0);
                }
            }
            kept.push((ch.name.clone(), svf));
            out.push(Channel { name: ch.name.clone(), samples });
        }
        self.chans = kept;
        Chans::new(rate, out)
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let shaped = ["lowpass", "highpass"];
    let boosts = ["peak", "low_shelf", "high_shelf"];
    let desc = NodeDesc::new(
        FILTER,
        "Shapes a sound by frequency: removes the highs or the lows, picks out or cuts a band, or turns a range up or down. Every channel is filtered on its own",
    )
    .param(
        ParamSpec::choice("type", &TYPES)
            .doc("What the filter does around cutoff. A change glides over 15 ms")
            .choice_doc("lowpass", "lets through what is below cutoff and removes the highs: makes a sound duller or further away")
            .choice_doc("highpass", "lets through what is above cutoff and removes the lows: makes a sound thinner, removes rumble")
            .choice_doc("bandpass", "keeps only a band around cutoff: a telephone or radio sound; more resonance narrows the band")
            .choice_doc("notch", "cuts out a narrow band around cutoff, e.g. a hum or a ringing tone; more resonance narrows the cut")
            .choice_doc("peak", "turns a band around cutoff up or down by gain, e.g. brings out a voice; more resonance narrows the band")
            .choice_doc("low_shelf", "turns everything below cutoff up or down by gain, e.g. tames a room's boom")
            .choice_doc("high_shelf", "turns everything above cutoff up or down by gain, e.g. adds air or softens hiss"),
    )
    .param(
        ParamSpec::float("cutoff", 1000.0)
            .clamp(MIN_CUTOFF, MAX_CUTOFF)
            .curve(Curve::Log)
            .unit(Unit::Hertz)
            .doc("The frequency where the filter acts, in Hz. Stays below 45% of the sample rate. Follows the param smoothly, so bind it to tracking or sweep it"),
    )
    .param(
        ParamSpec::float("resonance", 0.2)
            .clamp(0.0, 1.0)
            .doc("How sharply the filter acts at cutoff. 0.2 is a plain filter without a bump; higher adds a ringing peak at cutoff (lowpass, highpass) or narrows the band (bandpass, notch, peak); 1 nearly whistles"),
    )
    .param(
        ParamSpec::choice("slope", &["12", "24"])
            .doc("How steeply the sound falls off beyond cutoff, in dB per octave. 24 removes more of what lies beyond cutoff")
            .choice_doc("12", "a gentle slope: one filter stage")
            .choice_doc("24", "a steep slope: two filter stages in a row")
            .applies_when("type", &shaped),
    )
    .param(
        ParamSpec::float("gain", 0.0)
            .clamp(-24.0, 24.0)
            .unit(Unit::Decibels)
            .doc("How much peak or shelf turns its range up (above 0) or down (below 0), in dB")
            .applies_when("type", &boosts),
    )
    .param(
        ParamSpec::float("mix", 1.0)
            .clamp(0.0, 1.0)
            .more()
            .doc("1 is only the filtered sound, 0 only the input; between blends the two"),
    )
    .input("in", DataKind::Audio)
    .describe_input("in", "The sound to filter, at any sample rate and with any channels")
    .output("out", DataKind::Audio)
    .sampled()
    .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Filter::default()) as Box<dyn Operator>))
}

#[derive(Default)]
struct Filter {
    /// The state at the start and at the end of the last block.
    start: State,
    end: State,
    /// The input and output of the last block, and the tick it was filtered at.
    last: Option<(Data, Data, Tick)>,
}

impl Operator for Filter {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(data) = cx.input(0) else {
            return Err(crate::waiting_for(cx, "connect sound to in"));
        };
        let Some(input) = data.get::<Chans>() else {
            return Err(CookError::failed("in carries no sound"));
        };
        let rate = input.rate();
        if !(rate.is_finite() && rate > 0.0) {
            return Err(CookError::failed(format!("the input is at {rate} samples per second")));
        }
        let tick = cx.time().tick;
        let settings = Settings {
            kind: Kind::parse(cx.text("type")?),
            steep: cx.text("slope")? == "24",
            cutoff: cx.float("cutoff")?,
            resonance: cx.float("resonance")?,
            gain: cx.float("gain")?,
            mix: cx.float("mix")?,
        };
        let mut state = match &self.last {
            Some((i, o, t)) if i.same(data) => {
                if *t != tick || !cx.params_changed() {
                    // Already filtered: a device that delivered nothing, or nothing changed.
                    // The tick stays the block's own, so a param change now does not filter
                    // it again either.
                    return Ok(Cooked::Outputs(vec![o.clone()]));
                }
                self.start.clone()
            }
            _ => self.end.clone(),
        };
        let start = state.clone();
        let out = state.run(input, &design(&settings, rate))?;
        let out = Data::new(DataKind::Audio, out);
        self.start = start;
        self.end = state;
        self.last = Some((data.clone(), out.clone(), tick));
        Ok(Cooked::Outputs(vec![out]))
    }
}
