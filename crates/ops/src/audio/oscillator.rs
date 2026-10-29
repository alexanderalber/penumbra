//! `audio.oscillator`: a test tone (sine, saw, square, triangle) or noise, block by block on
//! the show clock ([`super::block`]).
//!
//! The phase is kept in waves as `f64` and advanced sample by sample, so it runs on across
//! blocks without a jump. A new frequency or amplitude (a bound param moves once per frame) is
//! reached by a straight ramp across the block, so it does not click. Noise is an integer hash
//! of the sample number and the seed, the same on every machine.

use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::block::{BlockClock, Step};

/// Kind of the oscillator.
pub const OSCILLATOR: &str = "audio.oscillator";
/// The sample rates an audio generator accepts.
pub const MIN_RATE: u32 = 8_000;
pub const MAX_RATE: u32 = 192_000;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(OSCILLATOR, "A test tone or noise as sound: sine, saw, square, triangle, noise")
            .param(
                ParamSpec::choice("shape", &["sine", "saw", "square", "triangle", "noise"])
                    .doc("The form of the wave, from -1 to 1 before amplitude")
                    .choice_doc("sine", "a pure tone")
                    .choice_doc("saw", "rising evenly from -1 to 1, then jumping back: bright and buzzing")
                    .choice_doc("square", "1 for the first half of each wave, -1 for the second: hollow")
                    .choice_doc("triangle", "straight up and straight down: softer than the square")
                    .choice_doc("noise", "white noise, the same for one seed on every machine"),
            )
            .param(
                ParamSpec::float("frequency", 440.0)
                    .clamp(0.0, f64::from(MAX_RATE) / 2.0)
                    .range(20.0, 20_000.0)
                    .curve(Curve::Log)
                    .unit(Unit::Hertz)
                    .doc("Waves per second: 440 Hz is the A above middle C. A change glides across one frame")
                    .applies_when("shape", &["sine", "saw", "square", "triangle"]),
            )
            .param(
                ParamSpec::float("amplitude", 0.25)
                    .range(0.0, 1.0)
                    .doc("The peak of the wave; 1 is full scale. A change glides across one frame"),
            )
            .param(ParamSpec::int("rate", 48_000).clamp(f64::from(MIN_RATE), f64::from(MAX_RATE)).more().doc(
                "Samples per second of the sound made; nodes that mix sound need the same rate on all inputs",
            ))
            .param(
                ParamSpec::int("seed", 0)
                    .range(0.0, 100.0)
                    .more()
                    .doc("Picks another noise of the same character")
                    .applies_when("shape", &["noise"]),
            )
            .param(ParamSpec::text("name", "ch1").doc("Name of the channel it makes, as other nodes see it"))
            .output("out", DataKind::Audio)
            .sampled()
            .time_dependent(),
        || Box::new(Oscillator::default()) as Box<dyn Operator>,
    ))
}

/// Where a block starts or ends: the phase in waves (0 to 1) and the params reached.
#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    phase: f64,
    frequency: f64,
    amplitude: f64,
}

#[derive(Default)]
struct Oscillator {
    clock: BlockClock,
    /// The voice at the start and the end of the last block.
    start: Voice,
    end: Voice,
    name: Option<Name>,
}

/// The waves on a phase in waves (0 to 1), as `chan.lfo` draws them.
pub(crate) fn wave(shape: &str, x: f64) -> f64 {
    match shape {
        "triangle" => 1.0 - 4.0 * (x - 0.5).abs(),
        "square" => {
            if x < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        "saw" => 2.0 * x - 1.0,
        _ => (x * std::f64::consts::TAU).sin(),
    }
}

/// White noise in -1..1 at sample `n`.
pub(crate) fn noise(n: i64, seed: i64) -> f64 {
    // The top 24 bits of the hash, as -1..1.
    f64::from(crate::chan::hash(n, seed) >> 8) / f64::from(1u32 << 23) - 1.0
}

impl Operator for Oscillator {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let rate =
            u32::try_from(cx.int("rate")?.clamp(i64::from(MIN_RATE), i64::from(MAX_RATE))).unwrap_or(48_000);
        let nyquist = f64::from(rate) / 2.0;
        let frequency = cx.float("frequency")?.clamp(0.0, nyquist);
        let amplitude = cx.float("amplitude")?;
        let shape = cx.text("shape")?;
        let seed = cx.int("seed")?;
        let name_text = cx.text("name")?.trim();
        if self.name.as_ref().is_none_or(|n| n.as_str() != name_text) {
            self.name = Some(Name::new(name_text).map_err(|e| e.context("`name`"))?);
        }
        let time = cx.time();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), rate);
        let from = match span.step {
            Step::Again => self.start,
            Step::Continue => self.end,
            // Starting over at sample n: the phase a steady tone would have there, so a render
            // from any point gives the same wave.
            Step::Fresh => Voice {
                phase: (frequency * span.start as f64 / f64::from(rate)).rem_euclid(1.0),
                frequency,
                amplitude,
            },
        };
        let n = span.len();
        let mut samples = Vec::with_capacity(n);
        let mut phase = from.phase;
        let r = f64::from(rate);
        for i in 0..n {
            // From the previous value at the first sample towards the new one; the next block
            // starts on it.
            let k = i as f64 / n as f64;
            let f = from.frequency + (frequency - from.frequency) * k;
            let a = from.amplitude + (amplitude - from.amplitude) * k;
            let x = if shape == "noise" {
                noise(span.start.saturating_add(i as i64), seed)
            } else {
                wave(shape, phase)
            };
            samples.push(a * x);
            phase = (phase + f / r).rem_euclid(1.0);
        }
        self.start = from;
        self.end = Voice { phase, frequency, amplitude };
        let Some(name) = self.name.clone() else { return Err(CookError::Pending) };
        let chans = Chans::new(r, vec![Channel { name, samples }])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}
