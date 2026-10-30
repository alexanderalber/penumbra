//! `audio.instrument`: ready-made one-shot sounds (pluck, mallet, bell, kick, hat, sweep),
//! polyphonic, played by channels, block by block on the show clock ([`super::block`]).
//!
//! Each channel of the `play` input is a lane. A lane starts a note when its value crosses
//! `threshold` in the direction `trigger` picks, at the sample the crossing falls on
//! ([`super::lanes`]). A channel that only knows "this frame" (a pulse from 0 to 1) starts its
//! note at the block's first sample. Lane `i` plays the degree `note + i * spread` of the scale
//! ([`super::pitch`]). Every note fades by itself; the voices are in [`voice`]. When more notes
//! sound than `voices` allows, the oldest fades out in 5 ms. Nothing wired: silence.
//!
//! A press through [`Presses`] (`audio.play`, the panel's play button) plays a note as if its
//! lane had crossed at the first sample of the next block, wired or not. Presses of the same
//! lane before that block play one note.
//!
//! The block contract is the oscillator's: `Again` renders the same block again from the state
//! at its start (voices and lane values), `Continue` goes on from the end, `Fresh` drops every
//! voice and the lanes' history, so no note comes from a value seen before a seek or a gap.

pub mod voice;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use self::voice::{PRESETS, Play, Preset, STEAL_SECONDS, Voice};
use super::block::{BlockClock, Step};
use super::lanes::{self, Crossing, Direction, Event, MAX_LEVEL_DB, MIN_LEVEL_DB, Seen};
use super::oscillator::{MAX_RATE, MIN_RATE};
use super::synth::{gain_of, soft_clip};

/// Kind of the instrument.
pub const INSTRUMENT: &str = "audio.instrument";
/// Most voices sounding at once.
pub const MAX_VOICES: usize = 128;
/// Most different lanes pressed before one block; more presses are dropped.
const MAX_PRESSES: usize = 64;

/// Presses from outside the cook (`audio.play`, the panel's play button), by node: the lanes
/// that start a note at the node's next block. Provided by the engine as a cooker resource;
/// cheap to clone.
#[derive(Clone, Default)]
pub struct Presses(Arc<Mutex<BTreeMap<NodeId, Vec<usize>>>>);

impl Presses {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lane `lane` of the node starts a note at the node's next block. False when the node has
    /// not cooked yet.
    pub fn request(&self, node: NodeId, lane: usize) -> bool {
        let mut entries = lock(&self.0);
        let Some(lanes) = entries.get_mut(&node) else { return false };
        if !lanes.contains(&lane) && lanes.len() < MAX_PRESSES {
            lanes.push(lane);
        }
        true
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Registers the node; with `take`, moves its pending presses to `out`.
    pub(crate) fn check_in(&self, node: NodeId, take: bool, out: &mut Vec<usize>) {
        let mut entries = lock(&self.0);
        let lanes = entries.entry(node).or_default();
        if take {
            out.append(lanes);
        }
    }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let pitched: Vec<&str> = PRESETS.into_iter().filter(|p| Preset::parse(p).pitched()).collect();
    let mut desc = NodeDesc::new(
        INSTRUMENT,
        "Ready-made sounds that play on a trigger and fade by themselves: pluck, mallet, bell, kick, hat. Each channel wired into play plays its own note of a scale",
    )
    .param(
        ParamSpec::choice("preset", &PRESETS)
            .doc("The sound. Changing it leaves notes already sounding as they are")
            .choice_doc("pluck", "a plucked string: a bright attack, faded to a tenth after decay seconds")
            .choice_doc("mallet", "a marimba-like wooden bar: short and round")
            .choice_doc("bell", "a struck bell: metallic overtones, rings about 8 times as long as decay")
            .choice_doc("kick", "a bass drum: a sine falling from about 300 Hz to 45 Hz in 150 ms. Ignores key, scale and note")
            .choice_doc("hat", "a hi-hat: a short burst of high noise, about a fifth of decay. Ignores key, scale and note"),
    );
    for p in lanes::pitch_params(
        "Which note plays: 0 is the root, 1 is range octaves higher. Rounded to the nearest note of the scale. Read when a note starts",
        "Notes of the scale between neighbouring play channels: with 1, channel 0 plays the root, channel 1 the next note up, and so on",
    ) {
        desc = desc.param(p.applies_when("preset", &pitched));
    }
    let desc = desc
        .param(
            ParamSpec::float("brightness", 0.5)
                .clamp(0.0, 1.0)
                .doc("0 is dull and soft, 1 bright and sharp. For kick: how high the drop starts. Read when a note starts"),
        )
        .param(lanes::seconds_param(
            "decay",
            0.3,
            "How long a note rings: seconds until a pluck has faded to a tenth. Each preset scales it, see preset",
        ))
        .param(lanes::level_param(
            "The volume, in dB: at 0 one note peaks near full scale, -6 is half the amplitude. A change glides across one frame",
        ))
        .param(
            ParamSpec::choice("trigger", &["up", "down", "both"])
                .doc("Which crossings of threshold by a play channel start a note")
                .choice_doc("up", "the value rises above threshold: a pulse from 0 to 1 plays once")
                .choice_doc("down", "the value falls to threshold or below")
                .choice_doc("both", "either way: a swinging value plays at every pass, as a pendulum at the bottom")
                .applies_when_wired("play"),
        )
        .param(
            ParamSpec::float("threshold", 0.0)
                .range(-1.0, 1.0)
                .more()
                .doc("The value a play channel must cross to start a note. 0 suits pulses from 0 to 1 and waves around 0")
                .applies_when_wired("play"),
        )
        .param(
            ParamSpec::int("voices", 32)
                .clamp(1.0, MAX_VOICES as f64)
                .range(1.0, 64.0)
                .more()
                .doc("Most notes sounding at once. One more fades out the oldest in 5 ms"),
        )
        .param(ParamSpec::int("rate", 48_000).clamp(f64::from(MIN_RATE), f64::from(MAX_RATE)).more().doc(
            "Samples per second of the sound made. out.audio nodes on one device need the same rate; audio.mix converts between rates",
        ))
        .param(ParamSpec::text("name", "ch1").doc("Name of the channel it makes, as other nodes see it"))
        .input("play", DataKind::Chan)
        .describe_input(
            "play",
            "Channels that play notes, each its own line of notes: a crossing of threshold starts a note, at its exact time inside the frame",
        )
        .output("out", DataKind::Audio)
        .sampled()
        .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Instrument::default()) as Box<dyn Operator>))
}

/// Everything that lasts from one block to the next.
#[derive(Clone, Debug, Default)]
struct State {
    voices: Vec<Voice>,
    /// Per lane, by channel name: its value at the end of the block.
    lanes: BTreeMap<String, Seen>,
    /// The gain as the block ended.
    gain: Option<f64>,
}

#[derive(Default)]
struct Instrument {
    clock: BlockClock,
    start: State,
    end: State,
    name: Option<Name>,
    /// Scratch: the crossings of one lane, the note-ons of the block.
    crossings: Vec<Crossing>,
    events: Vec<Event>,
    /// The lanes pressed at the start of the current block, kept for a render `Again`.
    pressed: Vec<usize>,
}

impl Operator for Instrument {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let rate =
            u32::try_from(cx.int("rate")?.clamp(i64::from(MIN_RATE), i64::from(MAX_RATE))).unwrap_or(48_000);
        let r = f64::from(rate);
        let preset = Preset::parse(cx.text("preset")?);
        let tuning = lanes::tuning(cx)?;
        let brightness = cx.float("brightness")?.clamp(0.0, 1.0);
        let decay = cx.float("decay")?.clamp(0.005, 60.0);
        let gain = gain_of(cx.float("level")?, MIN_LEVEL_DB, MAX_LEVEL_DB);
        let dir = Direction::parse(cx.text("trigger")?);
        let threshold = cx.float("threshold")?;
        let max_voices = usize::try_from(cx.int("voices")?.clamp(1, MAX_VOICES as i64)).unwrap_or(32);
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
        // A new block takes the presses made since the last; a block rendered again keeps its own.
        let fresh_block = span.step != Step::Again;
        if fresh_block {
            self.pressed.clear();
        }
        if let Some(presses) = cx.resource::<Presses>() {
            presses.check_in(cx.node(), fresh_block, &mut self.pressed);
        }
        lanes::events(
            cx.input_as::<Chans>(0),
            &mut st.lanes,
            time.seconds(),
            r,
            &span,
            threshold,
            dir,
            &mut self.crossings,
            &mut self.events,
        );
        if !self.pressed.is_empty() {
            for &lane in &self.pressed {
                self.events.push(Event { sample: span.start, lane, rising: true });
            }
            self.events.sort();
        }

        let mut out = vec![0.0; n];
        let mut at = 0usize;
        for e in &self.events {
            let rel = usize::try_from(e.sample - span.start).unwrap_or(0).min(n);
            render(&mut st.voices, &mut out, at, rel, span.start, r);
            at = rel;
            let frequency = if preset.pitched() { tuning.hertz(e.lane) } else { 0.0 };
            let play = Play { preset, frequency, brightness, decay, rate: r };
            let Some(v) = Voice::new(&play, e.sample, e.lane) else { continue };
            // Make room: the oldest voices not yet fading go first.
            let mut sounding = st.voices.iter().filter(|v| !v.fading()).count();
            while sounding >= max_voices {
                let Some(old) = st.voices.iter_mut().filter(|v| !v.fading()).min_by_key(|v| v.start) else {
                    break;
                };
                old.fade_out(STEAL_SECONDS, r);
                sounding -= 1;
            }
            st.voices.push(v);
        }
        render(&mut st.voices, &mut out, at, n, span.start, r);

        let g0 = st.gain.unwrap_or(gain);
        for (i, s) in out.iter_mut().enumerate() {
            let k = i as f64 / n.max(1) as f64;
            *s = soft_clip(*s * (g0 + (gain - g0) * k));
        }
        st.gain = Some(gain);
        self.end = st;
        let Some(name) = self.name.clone() else { return Err(CookError::Pending) };
        let chans = Chans::new(r, vec![Channel { name, samples: out }])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}

/// Renders every voice into `out[from..to]` (`start` is the sample number of `out[0]`) and
/// drops the ones that ended.
fn render(voices: &mut Vec<Voice>, out: &mut [f64], from: usize, to: usize, start: i64, rate: f64) {
    let Some(seg) = out.get_mut(from..to) else { return };
    if seg.is_empty() {
        return;
    }
    for v in voices.iter_mut() {
        v.render(seg, start + from as i64, rate);
    }
    voices.retain(|v| !v.done());
}
