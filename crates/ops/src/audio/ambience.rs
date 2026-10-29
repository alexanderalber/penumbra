//! `audio.ambience`: sustained sounds without notes (drone, pad, noise, water, rain), block by
//! block on the show clock ([`super::block`]). Pitch, brightness, swarm, density and level
//! follow their params all the time, smoothed over `glide`, so a bound param steers the sound
//! without steps.
//! [`swarm`] turns each drone or pad tone into a cloud of wandering voices; [`water`] is the
//! running-water preset, [`rain`] the rain.
//!
//! Nothing wired: one tone sounds all the time. Each channel of the `gate` input is a lane with
//! one tone, pitched as the instrument's lanes ([`super::pitch`]). A lane sounds while its value
//! is above `threshold`: the tone fades in over `attack` and out over `release`, from the sample
//! the crossing falls on ([`super::lanes`]). A closed lane keeps its tone, silent, so its phase
//! runs on. Changing `preset` crossfades every tone to the new sound in 20 ms.
//!
//! The block contract is the oscillator's: `Again` renders the same block again from the state
//! at its start, `Continue` goes on from the end, `Fresh` starts every tone over from silence
//! (fading in over `attack`) and forgets the lanes' history.

mod rain;
mod swarm;
mod water;

use std::collections::BTreeMap;
use std::f64::consts::TAU;

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::block::{BlockClock, Step};
use super::lanes::{self, Crossing, Direction, Event, MAX_LEVEL_DB, MIN_LEVEL_DB, Seen};
use super::oscillator::{MAX_RATE, MIN_RATE, noise};
use super::synth::{Svf, SvfCoef, TOP, gain_of, glide, saw, soft_clip};
use rain::Rain;
use swarm::Swarm;
use water::Water;

/// Kind of the ambience node.
pub const AMBIENCE: &str = "audio.ambience";
/// The presets, in the order of the `preset` choice.
pub const PRESETS: [&str; 5] = ["drone", "pad", "noise", "water", "rain"];
/// Most tones (lanes) at once.
pub const MAX_TONES: usize = 128;
/// The shortest glide, attack and release, seconds: shorter would click.
const SHORTEST: f64 = 0.005;
/// Seconds of the crossfade when the preset changes.
const SWAP_SECONDS: f64 = 0.02;
/// The pad's detuning of its three saws, cents.
const PAD_CENTS: [f64; 3] = [-9.0, 0.0, 7.0];
/// A swarm's extra pad voice is one saw: this brings it to the loudness of the pad's three.
const PAD_SUB: f64 = 0.577;
/// A swarm's extra drone voice: its sine and, with brightness, its octave.
const DRONE_SUB: (f64, f64) = (0.6, 0.3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preset {
    Drone,
    Pad,
    Noise,
    Water,
    Rain,
}

impl Preset {
    fn parse(name: &str) -> Self {
        match name {
            "pad" => Self::Pad,
            "noise" => Self::Noise,
            "water" => Self::Water,
            "rain" => Self::Rain,
            _ => Self::Drone,
        }
    }

    /// A gain per preset, so the presets sound about equally loud.
    fn gain(self) -> f64 {
        match self {
            Self::Drone => 0.6,
            Self::Pad => 0.83,
            Self::Noise => 0.2,
            Self::Water => 0.3,
            Self::Rain => 1.0,
        }
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let mut desc = NodeDesc::new(
        AMBIENCE,
        "A sound that holds without notes: a steady tone, a pad, wind-like noise, running water or rain. Its pitch, brightness and level follow the params smoothly; each channel wired into gate switches its own tone on and off",
    )
    .param(
        ParamSpec::choice("preset", &PRESETS)
            .doc("The sound. A change crossfades in 20 ms")
            .choice_doc("drone", "a steady soft tone: two sines that beat slowly and a quiet fifth; brightness adds the octave")
            .choice_doc("pad", "three slightly detuned saws through a gentle low-pass: warm and wide; brightness opens the filter")
            .choice_doc("noise", "rushing noise like wind; brightness moves it from a low rumble to a high hiss. Ignores key, scale and note")
            .choice_doc("water", "running water: rumble, gurgle, splash and hiss that each swell at random, with small bubbles; brightness moves it from a deep river to a bright trickle. Ignores key, scale and note")
            .choice_doc("rain", "rain: a soft rush with droplets ticking in it; density goes from a few drops to a downpour, brightness from distant and dull to close and crisp. Ignores key, scale and note"),
    );
    for p in lanes::pitch_params(
        "Which note sounds: 0 is the root, 1 is range octaves higher. Rounded to the nearest note of the scale; a change glides there over glide. Bind it to a tracked position to play by moving",
        "Notes of the scale between neighbouring gate channels: with 1, channel 0 holds the root, channel 1 the next note up, and so on",
    ) {
        desc = desc.param(p.applies_when("preset", &["drone", "pad"]));
    }
    let desc = desc
        .param(
            ParamSpec::float("brightness", 0.5)
                .clamp(0.0, 1.0)
                .doc("0 is dark and soft, 1 bright. Follows the param smoothly over glide"),
        )
        .param(
            ParamSpec::float("swarm", 0.0)
                .clamp(0.0, 1.0)
                .doc("0 holds every tone on its note. Above 0 each tone becomes eight voices whose pitches wander slowly at random around the note: a chorus at 0.2, a moving cloud up to half an octave either side at 1. Back at 0 every voice glides onto its note. Follows the param smoothly over glide")
                .applies_when("preset", &["drone", "pad"]),
        )
        .param(
            ParamSpec::float("density", 0.5)
                .clamp(0.0, 1.0)
                .doc("How hard it rains: 0 is a few drops a second, 0.5 about 90, 1 about 600 over a louder rush. Follows the param smoothly over glide")
                .applies_when("preset", &["rain"]),
        )
        .param(lanes::level_param(
            "The volume, in dB: at 0 one tone peaks near full scale, -6 is half the amplitude. Follows the param smoothly over glide",
        ))
        .param(lanes::seconds_param(
            "glide",
            0.1,
            "Seconds that pitch, brightness, swarm, density and level take to get 90% of the way to a new value. At least 5 ms",
        ).more())
        .param(lanes::seconds_param(
            "attack",
            0.1,
            "Seconds a tone takes to fade in when its gate opens. At least 5 ms",
        ))
        .param(lanes::seconds_param(
            "release",
            0.5,
            "Seconds a tone takes to fade out when its gate closes. At least 5 ms",
        ).applies_when_wired("gate"))
        .param(
            ParamSpec::float("threshold", 0.0)
                .range(-1.0, 1.0)
                .more()
                .doc("A gate channel above this value sounds its tone. 0 suits pulses from 0 to 1 and waves around 0")
                .applies_when_wired("gate"),
        )
        .param(ParamSpec::int("rate", 48_000).clamp(f64::from(MIN_RATE), f64::from(MAX_RATE)).more().doc(
            "Samples per second of the sound made; nodes that mix sound need the same rate on all inputs",
        ))
        .param(ParamSpec::text("name", "ch1").doc("Name of the channel it makes, as other nodes see it"))
        .input("gate", DataKind::Chan)
        .describe_input(
            "gate",
            "Channels that switch tones on and off, one tone each: a channel above threshold sounds, from its exact time inside the frame. Nothing wired: one tone always sounds",
        )
        .output("out", DataKind::Audio)
        .sampled()
        .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Ambience::default()) as Box<dyn Operator>))
}

/// One lane's sustained tone.
#[derive(Clone, Copy, Debug)]
struct Tone {
    lane: usize,
    preset: Preset,
    /// Whether the gate is open.
    open: bool,
    /// Replaced by a tone of another preset: fades out in [`SWAP_SECONDS`], then goes.
    retiring: bool,
    /// Replacing a tone of another preset: fades in in [`SWAP_SECONDS`].
    swapping: bool,
    /// The fade, 0 to 1.
    env: f64,
    /// The pitch as log2 Hz, gliding to `target`.
    lf: f64,
    target: f64,
    /// Oscillator phases in waves (drone: four sines, pad: three saws).
    phase: [f64; 4],
    /// The pad's two low-passes.
    lp: [f64; 2],
    /// The noise's filter.
    svf: Svf,
    /// The swarm's extra voices (drone, pad).
    swarm: Swarm,
    /// The water's bands and bubbles.
    water: Water,
    /// The rain's bed and droplets.
    rain: Rain,
}

impl Tone {
    fn new(lane: usize, preset: Preset, lf: f64, start: i64) -> Self {
        let mut phase = [0.0; 4];
        for (k, p) in phase.iter_mut().enumerate() {
            *p = (noise(start, 31 + k as i64 + 7 * lane as i64) + 1.0) / 2.0;
        }
        Self {
            lane,
            preset,
            open: false,
            retiring: false,
            swapping: false,
            env: 0.0,
            lf,
            target: lf,
            phase,
            lp: [0.0; 2],
            svf: Svf::default(),
            swarm: Swarm::new(lane, start),
            water: Water::default(),
            rain: Rain::default(),
        }
    }

    /// Adds `out.len()` samples (sample number `at` first) to `out`; `follow` holds the
    /// smoothed params per sample from the same offset.
    fn render(&mut self, out: &mut [f64], at: i64, follow: &[Follow], s: &Shared) {
        let r = s.rate;
        let top = TOP * r;
        let (rise, fall) = if self.retiring {
            (0.0, s.swap)
        } else if self.swapping {
            (s.swap, s.fall)
        } else {
            (s.rise, s.fall)
        };
        let gain = self.preset.gain();
        for (i, (o, &Follow { bright: b, swarm: sw, density })) in out.iter_mut().zip(follow).enumerate() {
            self.lf += (self.target - self.lf) * s.glide;
            self.env = if self.open && !self.retiring {
                (self.env + rise).min(1.0)
            } else {
                (self.env - fall).max(0.0)
            };
            if self.env <= 0.0 {
                continue;
            }
            // Smoothstep: no kink where a fade starts or ends.
            let amp = self.env * self.env * (3.0 - 2.0 * self.env);
            let t = at + i as i64;
            let x = match self.preset {
                Preset::Drone | Preset::Pad if sw > 0.0 => self.swarmed(t, b, sw, s),
                Preset::Drone => drone(&mut self.phase, self.lf.exp2(), b, r),
                Preset::Pad => {
                    let f = self.lf.exp2();
                    let x = pad_saws(&mut self.phase, f, r, &s.pad);
                    pad_filter(&mut self.lp, x / 3.0, f, b, r)
                }
                Preset::Noise => {
                    let cutoff = 100.0 * (7.5 * b).exp2();
                    let k = 1.4;
                    let coef = SvfCoef::new(cutoff, k, r);
                    let (_, band, _) = self.svf.tick(noise(t, 7919 + self.lane as i64), &coef);
                    // Narrow noise is quieter at low cutoffs: make up for the bandwidth.
                    let makeup = (0.5 * r * k / cutoff.min(top)).sqrt().min(30.0);
                    makeup * k * band
                }
                Preset::Water => self.water.tick(t, b, self.lane, r),
                Preset::Rain => self.rain.tick(t, b, density, self.lane, r),
            };
            *o += x * amp * gain;
        }
    }

    /// One sample of a drone or pad tone with `swarm` `sw` above 0: its own voice and the
    /// extra ones, each at its wandering pitch.
    fn swarmed(&mut self, t: i64, b: f64, sw: f64, s: &Shared) -> f64 {
        let r = s.rate;
        let top = TOP * r;
        let (depth, own, extra) = swarm::mix(sw);
        let m = self.swarm.factors(t, self.lane, r, depth);
        let f = self.lf.exp2();
        let f0 = f * m[0];
        // The note's frequency in waves per sample: each voice multiplies it by its wander.
        let per = f / r;
        let mut sum = 0.0;
        if self.preset == Preset::Pad {
            let x = own * pad_saws(&mut self.phase, f0, r, &s.pad) / 3.0;
            for (p, &w) in self.swarm.phase.iter_mut().zip(&m[1..]) {
                let dt = (per * w).min(0.49);
                sum += saw(*p, dt);
                *p += dt;
                if *p >= 1.0 {
                    *p -= 1.0;
                }
            }
            return pad_filter(&mut self.lp, x + extra * PAD_SUB * sum, f, b, r);
        }
        let x = own * drone_fast(&mut self.phase, f0, b, r);
        let (fundamental, octave) = DRONE_SUB;
        let (top, octave) = (top / r, octave * b);
        for (p, &w) in self.swarm.phase.iter_mut().zip(&m[1..]) {
            let dt = per * w;
            if dt < top {
                sum += fundamental * swarm::sine(*p);
                if 2.0 * dt < top {
                    sum += octave * swarm::sine(2.0 * *p);
                }
            }
            *p += dt.min(0.5);
            if *p >= 1.0 {
                *p -= 1.0;
            }
        }
        x + extra * sum
    }
}

/// One sample of the drone at `f` Hz: two sines 7 cents apart, one weaker, beat gently about
/// once a second at middle C; a quiet fifth and an octave that brightness `b` brings in.
fn drone(phase: &mut [f64; 4], f: f64, b: f64, r: f64) -> f64 {
    let top = TOP * r;
    let parts = [(0.998, 0.5), (1.002, 0.3), (1.5, 0.12), (2.0, 0.3 * b)];
    let mut x = 0.0;
    for (p, (ratio, a)) in phase.iter_mut().zip(parts) {
        let fr = f * ratio;
        if fr < top {
            x += a * (*p * TAU).sin();
        }
        *p = (*p + fr / r).rem_euclid(1.0);
    }
    x
}

/// [`drone`] with the polynomial sine, for the swarm, where it runs beside seven more voices.
fn drone_fast(phase: &mut [f64; 4], f: f64, b: f64, r: f64) -> f64 {
    let top = TOP * r;
    let parts = [(0.998, 0.5), (1.002, 0.3), (1.5, 0.12), (2.0, 0.3 * b)];
    let mut x = 0.0;
    for (p, (ratio, a)) in phase.iter_mut().zip(parts) {
        let fr = f * ratio;
        if fr < top {
            x += a * swarm::sine(*p);
        }
        *p += (fr / r).min(0.5);
        if *p >= 1.0 {
            *p -= 1.0;
        }
    }
    x
}

/// The sum of the pad's three detuned saws around `f` Hz (`ratios` from [`PAD_CENTS`]);
/// advances their phases.
fn pad_saws(phase: &mut [f64; 4], f: f64, r: f64, ratios: &[f64; 3]) -> f64 {
    let mut x = 0.0;
    for (p, ratio) in phase.iter_mut().zip(ratios) {
        let dt = (f * ratio / r).min(0.49);
        x += saw(*p, dt);
        *p += dt;
        if *p >= 1.0 {
            *p -= 1.0;
        }
    }
    x
}

/// The pad's gentle low-pass, two poles, opening with brightness `b` above the note `f`.
fn pad_filter(lp: &mut [f64; 2], x: f64, f: f64, b: f64, r: f64) -> f64 {
    let cutoff = (f * (1.5 + 6.0 * b)).clamp(20.0, TOP * r);
    let a = 1.0 - (-TAU * cutoff / r).exp();
    lp[0] += a * (x - lp[0]);
    lp[1] += a * (lp[0] - lp[1]);
    lp[1]
}

/// The params that follow smoothly, at one sample.
#[derive(Clone, Copy, Debug)]
struct Follow {
    bright: f64,
    swarm: f64,
    density: f64,
}

/// What every tone of one block shares.
struct Shared {
    rate: f64,
    glide: f64,
    rise: f64,
    fall: f64,
    swap: f64,
    /// The pad's frequency ratios, from [`PAD_CENTS`].
    pad: [f64; 3],
}

/// Everything that lasts from one block to the next.
#[derive(Clone, Debug, Default)]
struct State {
    tones: Vec<Tone>,
    /// Per lane, by channel name: its value at the end of the block.
    lanes: BTreeMap<String, Seen>,
    /// Gain, brightness and swarm as the block ended.
    gain: Option<f64>,
    brightness: Option<f64>,
    swarm: Option<f64>,
    density: Option<f64>,
}

#[derive(Default)]
struct Ambience {
    clock: BlockClock,
    start: State,
    end: State,
    name: Option<Name>,
    crossings: Vec<Crossing>,
    events: Vec<Event>,
    /// Per lane, whether its gate is open at the block's start.
    open: Vec<bool>,
}

impl Operator for Ambience {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let rate =
            u32::try_from(cx.int("rate")?.clamp(i64::from(MIN_RATE), i64::from(MAX_RATE))).unwrap_or(48_000);
        let r = f64::from(rate);
        let preset = Preset::parse(cx.text("preset")?);
        let tuning = lanes::tuning(cx)?;
        let brightness = cx.float("brightness")?.clamp(0.0, 1.0);
        let swarm = cx.float("swarm")?;
        let swarm = if swarm.is_finite() { swarm.clamp(0.0, 1.0) } else { 0.0 };
        let density = cx.float("density")?;
        let density = if density.is_finite() { density.clamp(0.0, 1.0) } else { 0.5 };
        let gain = gain_of(cx.float("level")?, MIN_LEVEL_DB, MAX_LEVEL_DB);
        let seconds = |name: &str| -> Result<f64, CookError> {
            let s = cx.float(name)?;
            Ok(if s.is_finite() { s.clamp(SHORTEST, 3600.0) } else { SHORTEST })
        };
        let shared = Shared {
            rate: r,
            glide: glide(seconds("glide")?, r),
            rise: 1.0 / (seconds("attack")? * r),
            fall: 1.0 / (seconds("release")? * r),
            swap: 1.0 / (SWAP_SECONDS * r),
            pad: PAD_CENTS.map(|c| (c / 1200.0).exp2()),
        };
        let threshold = cx.float("threshold")?;
        let name_text = cx.text("name")?.trim();
        if self.name.as_ref().is_none_or(|n| n.as_str() != name_text) {
            self.name = Some(Name::new(name_text).map_err(|e| e.context("`name`"))?);
        }
        let time = cx.time();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), rate);
        let mut st = match span.step {
            Step::Again => self.start.clone(),
            Step::Continue => std::mem::take(&mut self.end),
            Step::Fresh => State::default(),
        };
        self.start = st.clone();
        let n = span.len();
        let input = cx.input_as::<Chans>(0);

        // Each lane's gate at the block's start: from its last value, else (no history) from
        // its value now. Without input, one lane always open.
        self.open.clear();
        match input {
            Some(chans) => self.open.extend(
                chans
                    .channels()
                    .iter()
                    .take(MAX_TONES)
                    .map(|c| st.lanes.get(c.name.as_str()).map_or(c.value(), |s| s.value) > threshold),
            ),
            None => self.open.push(true),
        }
        let lane_count = self.open.len();
        lanes::events(
            input,
            &mut st.lanes,
            time.seconds(),
            r,
            &span,
            threshold,
            Direction::Both,
            &mut self.crossings,
            &mut self.events,
        );

        // One live tone of this preset per lane; tones of another preset fade out.
        let mut swapped = false;
        for t in &mut st.tones {
            if t.preset != preset && !t.retiring {
                t.retiring = true;
                swapped = true;
            }
        }
        for (lane, &open) in self.open.iter().enumerate() {
            let target = tuning.hertz(lane).log2();
            if let Some(t) = st.tones.iter_mut().find(|t| t.lane == lane && !t.retiring) {
                t.open = open;
                t.target = target;
            } else {
                let mut t = Tone::new(lane, preset, target, span.start);
                t.open = open;
                t.swapping = swapped;
                st.tones.push(t);
            }
        }
        for t in &mut st.tones {
            if t.lane >= lane_count {
                t.open = false;
            }
        }

        // Brightness, swarm, density and level, smoothed per sample. The swarm lands exactly on
        // its value at the end, so at 0 the tones are exactly the plain ones again.
        let mut follow = Vec::with_capacity(n);
        let mut gains = Vec::with_capacity(n);
        let (mut b, mut g) = (st.brightness.unwrap_or(brightness), st.gain.unwrap_or(gain));
        let mut w = st.swarm.unwrap_or(swarm);
        let mut d = st.density.unwrap_or(density);
        for _ in 0..n {
            b += (brightness - b) * shared.glide;
            g += (gain - g) * shared.glide;
            w += (swarm - w) * shared.glide;
            if (swarm - w).abs() < 1e-6 {
                w = swarm;
            }
            d += (density - d) * shared.glide;
            follow.push(Follow { bright: b, swarm: w, density: d });
            gains.push(g);
        }

        let mut out = vec![0.0; n];
        let mut at = 0usize;
        for e in &self.events {
            let rel = usize::try_from(e.sample - span.start).unwrap_or(0).min(n);
            render(&mut st.tones, &mut out, at, rel, span.start, &follow, &shared);
            at = rel;
            if let Some(t) = st.tones.iter_mut().find(|t| t.lane == e.lane && !t.retiring) {
                t.open = e.rising;
            }
        }
        render(&mut st.tones, &mut out, at, n, span.start, &follow, &shared);
        st.tones.retain(|t| t.env > 0.0 || (t.lane < lane_count && !t.retiring));
        for t in &mut st.tones {
            if t.env >= 1.0 {
                t.swapping = false;
            }
        }

        for (s, g) in out.iter_mut().zip(&gains) {
            *s = soft_clip(*s * g);
        }
        st.gain = Some(g);
        st.brightness = Some(b);
        st.swarm = Some(w);
        st.density = Some(d);
        self.end = st;
        let Some(name) = self.name.clone() else { return Err(CookError::Pending) };
        let chans = Chans::new(r, vec![Channel { name, samples: out }])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}

/// Renders every tone into `out[from..to]`.
fn render(
    tones: &mut [Tone],
    out: &mut [f64],
    from: usize,
    to: usize,
    start: i64,
    follow: &[Follow],
    s: &Shared,
) {
    let (Some(seg), Some(f)) = (out.get_mut(from..to), follow.get(from..to)) else { return };
    if seg.is_empty() {
        return;
    }
    for t in tones.iter_mut() {
        t.render(seg, start + from as i64, f, s);
    }
}
