//! `out.audio`: plays its input on a speaker or headphones.
//!
//! - **One block per frame:** each advance of the show tick hands the new block to the
//!   engine's [`Outputs`] hub, which sums the blocks of every `out.audio` on the same device
//!   and pushes them to that device's player after the cook pass. A recook at the same tick (a
//!   param changed while the show stands still) pushes nothing, and neither does an input that
//!   hands on the same result as before (a device source that got nothing new, `Data::same`).
//!   So the show plays sound only while it runs; paused, the output fades out.
//! - **A jump** (the first block, a seek, a gap of more than half a second) fades the new block
//!   in over a few milliseconds, so it does not click, and empties the reverb: no tail of the
//!   sound from before the jump.
//! - **The chain:** non-numbers become silence, then the room reverb
//!   ([`penumbra_audio::reverb`], params `reverb`, `room`, `damping`), then `level` (dB) and
//!   `mute`, both ramped across the block, then a look-ahead limiter at -1 dBFS
//!   ([`penumbra_audio::limiter`]), always on, and a hard clip at the same ceiling. The reverb
//!   comes before `level` and `mute`, so muting silences its tail at once and a level change
//!   keeps the balance of dry and room. It comes before the limiter, so the ceiling holds for
//!   the tail too.
//! - **Reverb and channels:** with `reverb` above 0 a mono input leaves the reverb as stereo
//!   (the room gives it width; a mono device plays the mean of both). Once wide, the node stays
//!   wide until a jump, so turning `reverb` down to 0 does not change the channel count while
//!   the sound plays. A node that starts with `reverb` 0 plays its input exactly as it comes. A sudden blast in a room full of people is the audio
//!   equivalent of a crash; the limiter is not a param for that reason. What it takes off is
//!   reported in the node's warning and in `audio.status`.
//! - **Recordings:** each block goes to the hub with the show's sample number of its first
//!   sample, the limiter's delay taken off, so `out.video` can put it at its show time
//!   ([`Outputs::tap`]).
//! - **Device:** part of an output's name, empty for the system's default output. The hub
//!   opens the device when a node wants it (after every change of the network, [`wants`]) and
//!   closes it, after a fade, when none does.

use penumbra_audio::limiter::{Limiter, db_to_gain};
use penumbra_audio::playback::{MAX_CHANNELS, MAX_LATENCY_MS, MAX_RATE, MIN_LATENCY_MS, MIN_RATE, Settings};
use penumbra_audio::reverb::{self, Reverb};
use penumbra_audio::{Outputs, Want};
use penumbra_core::{Chans, Error, ParamSpec, Unit, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};

use crate::audio::block::{BlockClock, Step};

/// Kind of the audio output node.
pub const AUDIO_OUT: &str = "out.audio";
/// The fade-in of a block that does not continue the one before, seconds.
pub const JUMP_FADE_SECONDS: f64 = 0.003;
/// Most of one block that is played, seconds (the newest part of a longer one).
pub const MAX_BLOCK_SECONDS: f64 = 1.0;
/// The quietest `level` short of silence, dB; below it the output is silent.
pub const MIN_LEVEL_DB: f64 = -100.0;
/// The loudest `level`, dB.
pub const MAX_LEVEL_DB: f64 = 24.0;
/// The default target of the queue, milliseconds.
pub const DEFAULT_LATENCY_MS: f64 = 50.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_OUT,
            "Plays sound on a speaker or headphones, always through a limiter that keeps it below -1 dBFS",
        )
        .param(ParamSpec::text("device", "").doc(
            "Part of the output's name (the menu beside it lists them), e.g. `Speakers`; empty: the system's \
             default output. Several out.audio nodes on one device are added together",
        ))
        .param(ParamSpec::bool("active", true).doc("Off: plays nothing, and the device closes when no other node uses it"))
        .param(
            ParamSpec::float("level", 0.0)
                .clamp(MIN_LEVEL_DB, MAX_LEVEL_DB)
                .range(-60.0, 12.0)
                .unit(Unit::Decibels)
                .doc("The volume, in dB: 0 plays the sound as it comes, -6 at half the amplitude. A change glides across one frame. The limiter comes after it"),
        )
        .param(ParamSpec::bool("mute", false).doc("Silence, faded across one frame; the device stays open. audio.mute mutes every output at once"))
        .param(
            ParamSpec::float("reverb", reverb::DEFAULT_MIX)
                .clamp(0.0, 1.0)
                .range(0.0, 1.0)
                .doc("How much room the sound gets: 0 plays it dry, 0.2 a small room you just notice, 1 only the room. A mono sound gets stereo width from it"),
        )
        .param(
            ParamSpec::float("room", reverb::DEFAULT_ROOM)
                .clamp(0.0, 1.0)
                .range(0.0, 1.0)
                .doc("The size of the room: how long the sound rings on. 0 dies away in 0.3 s, 0.4 in 0.85 s, 0.7 in 1.8 s, 1 in 4 s"),
        )
        .param(
            ParamSpec::float("damping", reverb::DEFAULT_DAMPING)
                .clamp(0.0, 1.0)
                .range(0.0, 1.0)
                .more()
                .doc("How much faster high tones die away in the room: 0 as fast as low ones, 0.5 in about half the time, 1 in a tenth. More sounds warmer and duller"),
        )
        .param(
            ParamSpec::float("latency", DEFAULT_LATENCY_MS)
                .clamp(MIN_LATENCY_MS, MAX_LATENCY_MS)
                .range(20.0, 200.0)
                .more()
                .doc("Milliseconds of sound queued ahead of the device, which adds its own buffer on top. Less answers faster; too little and late frames are heard as gaps (audio.status counts them as underruns)"),
        )
        .input("in", DataKind::Audio)
        .describe_input("in", "The sound to play: mono plays on every speaker, stereo on left and right")
        // Cooked at every new show tick: it reads the tick to tell a new block from the same one.
        .time_dependent(),
        || Box::new(AudioOut::default()) as Box<dyn Operator>,
    ))
}

/// What the network's active `out.audio` nodes want (stored params).
pub fn wants(net: &Network, types: &NodeTypes) -> Vec<Want> {
    let Some(desc) = types.desc(AUDIO_OUT) else { return Vec::new() };
    net.nodes()
        .filter(|n| n.kind == AUDIO_OUT)
        .filter(|n| n.param(desc, "active").and_then(Value::as_bool).unwrap_or(true))
        .map(|n| {
            let key = n.param(desc, "device").and_then(Value::as_str).unwrap_or("").trim().to_string();
            let latency = n.param(desc, "latency").and_then(Value::as_f64).unwrap_or(DEFAULT_LATENCY_MS);
            Want { node: n.id, key, settings: settings(latency) }
        })
        .collect()
}

fn settings(latency_ms: f64) -> Settings {
    let latency_ms = if latency_ms.is_finite() { latency_ms } else { DEFAULT_LATENCY_MS };
    Settings { latency_ms: latency_ms.clamp(MIN_LATENCY_MS, MAX_LATENCY_MS), ..Settings::default() }
}

/// The level and mute as one factor (0 for silence).
fn gain_of(level_db: f64, mute: bool) -> f64 {
    if mute || !level_db.is_finite() || level_db <= MIN_LEVEL_DB {
        0.0
    } else {
        db_to_gain(level_db.min(MAX_LEVEL_DB))
    }
}

#[derive(Default)]
struct AudioOut {
    clock: BlockClock,
    /// The input played last, to tell a new block from the same one handed on again.
    last: Option<Data>,
    /// The gain at the end of the last block.
    gain: Option<f64>,
    limiter: Option<Limiter>,
    reverb: Option<Reverb>,
    /// A mono input plays as stereo: the reverb was on since the last jump.
    wide: bool,
}

impl AudioOut {
    /// The block through the room reverb: unchanged while the reverb is off and has no tail
    /// (a mono block doubled while the node is wide).
    fn room(&mut self, block: Vec<Vec<f64>>, rate: u32, p: &reverb::Params) -> Vec<Vec<f64>> {
        let on = p.mix.is_finite() && p.mix > 0.0;
        let ringing = self.reverb.as_ref().is_some_and(|r| r.wet() > 0.0);
        if !on && !ringing {
            if let Some(r) = self.reverb.as_mut() {
                // The tail must not come back when the reverb is turned up again.
                r.reset();
            }
            if self.wide && block.len() == 1 {
                let mono = block.into_iter().next().unwrap_or_default();
                return vec![mono.clone(), mono];
            }
            return block;
        }
        let inputs = block.len();
        let outputs = if inputs == 1 { 2 } else { inputs };
        if self.reverb.as_ref().is_none_or(|r| r.rate() != rate || r.inputs() != inputs) {
            self.reverb = Some(Reverb::new(rate, inputs, outputs));
        }
        let Some(r) = self.reverb.as_mut() else { return block };
        self.wide |= inputs == 1;
        let frames = block.iter().map(Vec::len).max().unwrap_or(0);
        let mut out = vec![vec![0.0; frames]; outputs];
        r.process(&block, &mut out, p);
        out
    }

    fn report(&self, cx: &CookContext<'_>, outputs: &Outputs, key: &str) {
        let Some(bus) = outputs.bus_status(key) else { return };
        if let Some(p) = &bus.player
            && !p.open
        {
            let what = if key.is_empty() { "the default output".to_string() } else { format!("`{key}`") };
            cx.warn(match &p.error {
                Some(e) => format!("not playing on {what}: {e}"),
                None => format!("opening {what} ..."),
            });
        }
        if bus.muted {
            cx.warn("every output is muted (audio.mute)");
        }
        let held = outputs.lane_status(cx.node()).map_or(0.0, |(_, l)| l.reduction_held_db);
        if held >= 1.0 {
            cx.warn(format!(
                "the limiter takes up to {held:.1} dB off: the sound is louder than -1 dBFS; turn level down"
            ));
        }
        if bus.reduction_db >= 1.0 {
            cx.warn(format!(
                "the nodes on this device add up to more than -1 dBFS: the limiter of their sum takes up to {:.1} dB off",
                bus.reduction_db
            ));
        }
    }
}

impl Operator for AudioOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let outputs = cx
            .resource::<Outputs>()
            .ok_or_else(|| CookError::failed("audio output is not available in this process"))?;
        if !cx.bool("active")? {
            self.last = None;
            self.clock.reset();
            return Ok(Cooked::Outputs(Vec::new()));
        }
        let key = cx.text("device")?.trim().to_string();
        let Some(data) = cx.input(0) else {
            self.last = None;
            self.report(cx, outputs, &key);
            cx.warn("nothing connected: silent");
            return Ok(Cooked::Outputs(Vec::new()));
        };
        let Some(chans) = data.get::<Chans>() else {
            return Err(crate::waiting(cx));
        };
        let rate_f = chans.rate();
        // `as`: checked to be a whole number in range first.
        let rate = if rate_f.is_finite()
            && rate_f.fract() == 0.0
            && (f64::from(MIN_RATE)..=f64::from(MAX_RATE)).contains(&rate_f)
        {
            rate_f as u32
        } else {
            return Err(CookError::failed(format!(
                "the input is at {rate_f} samples per second; out.audio plays whole rates from {MIN_RATE} to {MAX_RATE}"
            )));
        };
        if chans.channels().is_empty() {
            self.report(cx, outputs, &key);
            return Ok(Cooked::Outputs(Vec::new()));
        }
        let time = cx.time();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), rate);
        let again = span.step == Step::Again;
        let same = self.last.as_ref().is_some_and(|d| d.same(data));
        if again || same {
            // The same block again: it was played.
            self.report(cx, outputs, &key);
            return Ok(Cooked::Outputs(Vec::new()));
        }
        self.last = Some(data.clone());
        let max = (rate_f * MAX_BLOCK_SECONDS) as usize;
        let mut block: Vec<Vec<f64>> = chans
            .channels()
            .iter()
            .take(MAX_CHANNELS)
            .map(|c| {
                let skip = c.samples.len().saturating_sub(max);
                c.samples.iter().skip(skip).map(|v| if v.is_finite() { *v } else { 0.0 }).collect()
            })
            .collect();
        let frames = block.iter().map(Vec::len).max().unwrap_or(0);
        for c in &mut block {
            c.resize(frames, 0.0);
        }
        // The show's sample number of the block's first sample: the block ends now.
        let first = span.end.saturating_sub(i64::try_from(frames).unwrap_or(0));
        let jump = span.step == Step::Fresh;
        let room = reverb::Params {
            mix: cx.float("reverb")?,
            room: cx.float("room")?,
            damping: cx.float("damping")?,
        };
        if jump {
            if let Some(r) = self.reverb.as_mut() {
                r.reset();
            }
            self.wide = false;
        }
        let mut block = self.room(block, rate, &room);
        let frames = block.iter().map(Vec::len).max().unwrap_or(0);
        let target = gain_of(cx.float("level")?, cx.bool("mute")?);
        let from = self.gain.unwrap_or(target);
        // `as`: a few hundred samples.
        let fade = if jump { ((JUMP_FADE_SECONDS * rate_f) as usize).max(1) } else { 0 };
        for c in &mut block {
            for (k, v) in c.iter_mut().enumerate() {
                // `as`: positions in a block.
                let t = (k + 1) as f64 / frames.max(1) as f64;
                let mut g = from + (target - from) * t;
                if k < fade {
                    let x = (k as f64 + 0.5) / fade as f64;
                    g *= x * x * (3.0 - 2.0 * x);
                }
                *v *= g;
            }
        }
        self.gain = Some(target);
        let channels = block.len();
        if self.limiter.as_ref().is_none_or(|l| l.rate() != rate || l.channels() != channels) {
            self.limiter = Some(Limiter::new(rate, channels));
        } else if jump && let Some(l) = self.limiter.as_mut() {
            l.reset();
        }
        let (reduction, delay) = match self.limiter.as_mut() {
            Some(l) => {
                l.process(&mut block);
                (l.take_reduction(), l.lookahead())
            }
            None => (0.0, 0),
        };
        let latency = cx.float("latency")?;
        // The limiter delays the sound: what leaves it now was meant to be heard `delay` earlier.
        let first = first.saturating_sub(i64::try_from(delay).unwrap_or(0));
        outputs
            .submit_at(cx.node(), &key, settings(latency), rate, &block, reduction, Some(first))
            .map_err(CookError::failed)?;
        self.report(cx, outputs, &key);
        Ok(Cooked::Outputs(Vec::new()))
    }
}
