//! `chan.audio_envelope`: how loud a sound is now, one channel per sound channel, to bind.
//!
//! Each channel runs its own [`Envelope`] (see `penumbra-audio`'s `envelope` module for the
//! detector) over every sample of every block; the output is the level after the block's last
//! sample, one sample per channel at the show rate, as other chan nodes give the value now.
//!
//! Blocks: as in [`super::filter`], the state per channel name carries across blocks. A new
//! input block continues from the state at the end of the last one. The same input again (a
//! param changed, or a device that delivered nothing) is followed again from the state at the
//! start of that block, so no block counts twice and a recook gives the same value. A change
//! of sample rate starts every channel over.
//!
//! [`Scale`] and its params are shared with `chan.audio_spectrum`.

use penumbra_audio::{Detect, Envelope};
use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

/// Kind of the envelope follower.
pub const ENVELOPE: &str = "chan.audio_envelope";

/// How a level becomes a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// The amplitude, full scale is 1; 0 below the floor.
    Linear,
    /// dBFS, not below the floor.
    Db,
    /// 0 at the floor to 1 at full scale, even in dB.
    Level,
}

impl Scale {
    pub fn parse(name: &str) -> Self {
        match name {
            "db" => Self::Db,
            "level" => Self::Level,
            _ => Self::Linear,
        }
    }

    /// `amplitude` (full scale 1) in this scale, with silence at `floor` dB.
    pub fn apply(self, amplitude: f64, floor: f64) -> f64 {
        let floor = if floor.is_finite() { floor.clamp(-200.0, -1.0) } else { -60.0 };
        let a = if amplitude.is_finite() { amplitude.max(0.0) } else { 0.0 };
        let db = if a > 0.0 { (20.0 * a.log10()).max(floor) } else { floor };
        match self {
            Self::Linear if db <= floor => 0.0,
            Self::Linear => a,
            Self::Db => db,
            Self::Level => ((db - floor) / -floor).clamp(0.0, 1.0),
        }
    }
}

/// The `unit` param, `default` first among the choices.
pub(crate) fn unit_param(default: &str, what: &str) -> ParamSpec {
    let mut choices = vec![default];
    choices.extend(["linear", "db", "level"].into_iter().filter(|c| *c != default));
    ParamSpec::choice("unit", &choices)
        .doc(&format!("What number {what} gives"))
        .choice_doc("linear", "the amplitude: 1 is full scale, a sine at -20 dBFS reads 0.1. 0 below floor")
        .choice_doc("db", "dBFS: 0 is full scale, -20 a tenth of it. Never below floor")
        .choice_doc("level", "0 at floor to 1 at full scale, even in dB: ready to drive a param")
}

/// The `floor` param.
pub(crate) fn floor_param(default: f64) -> ParamSpec {
    ParamSpec::float("floor", default)
        .clamp(-200.0, -1.0)
        .range(-120.0, -20.0)
        .unit(Unit::Decibels)
        .doc("Quieter than this counts as silence: 0 in linear and level, the floor itself in db")
}

/// A time param in seconds, from 0 (at once).
pub(crate) fn time_param(name: &str, default: f64, doc: &str) -> ParamSpec {
    ParamSpec::float(name, default)
        .clamp(0.0, 60.0)
        .range(0.001, 5.0)
        .curve(Curve::Log)
        .unit(Unit::Seconds)
        .doc(doc)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(
        ENVELOPE,
        "How loud a sound is now, per channel, as a number to bind: rises with the sound in attack and falls in release",
    )
    .stateful()
    .param(
        ParamSpec::choice("detect", &["rms", "peak"])
            .doc("What loudness means")
            .choice_doc("rms", "the power, averaged over about attack: loudness as heard. A sine at -20 dBFS reads -23 dB")
            .choice_doc("peak", "the waveform's crests, as a peak meter: a sine at -20 dBFS reads -20 dB"),
    )
    .param(time_param(
        "attack",
        0.01,
        "Seconds the value takes to get about two thirds of the way up when the sound gets louder. Shorter catches every hit",
    ))
    .param(time_param(
        "release",
        0.2,
        "Seconds the value takes to fall to about a third when the sound gets quieter. Longer holds hits for the eye",
    ))
    .param(unit_param("linear", "a channel"))
    .param(floor_param(-60.0))
    .input("audio", DataKind::Audio)
    .describe_input("audio", "The sound to measure, e.g. from an audio.device_in, at any sample rate and with any channels")
    .output("out", DataKind::Chan);
    types.register(FnNodeType::new(desc, || Box::new(Follower::default()) as Box<dyn Operator>))
}

/// Every channel's envelope at one sample rate.
#[derive(Clone, Debug, Default)]
struct State {
    rate: f64,
    chans: Vec<(Name, Envelope)>,
}

#[derive(Default)]
struct Follower {
    /// The state at the start and at the end of the last block.
    start: State,
    end: State,
    /// The last input block.
    last: Option<Data>,
}

impl Operator for Follower {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(data) = cx.input(0) else {
            return Err(crate::waiting_for(cx, "connect sound to audio"));
        };
        let Some(input) = data.get::<Chans>() else {
            return Err(CookError::failed("audio carries no sound"));
        };
        let rate = input.rate();
        if !(rate.is_finite() && rate > 0.0) {
            return Err(CookError::failed(format!("the input is at {rate} samples per second")));
        }
        let detect = if cx.text("detect")? == "peak" { Detect::Peak } else { Detect::Rms };
        let (attack, release) = (cx.float("attack")?, cx.float("release")?);
        let scale = Scale::parse(cx.text("unit")?);
        let floor = cx.float("floor")?;
        let mut state = match &self.last {
            Some(last) if last.same(data) => self.start.clone(),
            _ => self.end.clone(),
        };
        if state.rate != rate {
            state = State { rate, chans: Vec::new() };
        }
        let start = state.clone();
        let mut kept = Vec::with_capacity(input.channels().len());
        let mut out = Vec::with_capacity(input.channels().len());
        for ch in input.channels() {
            let mut env =
                state.chans.iter().find(|(n, _)| *n == ch.name).map_or_else(Envelope::default, |(_, e)| *e);
            let level = env.run(&ch.samples, rate, attack, release, detect);
            kept.push((ch.name.clone(), env));
            out.push(Channel { name: ch.name.clone(), samples: vec![scale.apply(level, floor)] });
        }
        state.chans = kept;
        self.start = start;
        self.end = state;
        self.last = Some(data.clone());
        let chans = Chans::new(cx.rate().as_f64(), out)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}
