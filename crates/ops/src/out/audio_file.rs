//! `out.audio_file`: records sound to a WAV file, the counterpart of `out.video` for sound alone.
//!
//! The node never waits for the disk: each block goes to the recording's thread
//! ([`penumbra_record::wav`]), which writes it. When the thread falls behind by more than
//! [`penumbra_record::wav::QUEUE_SECONDS`], blocks are dropped and counted.
//!
//! - **What it hears:** with `source` at `input`, the sound wired to it, one new block per show
//!   tick (the same contract `out.audio` plays by, [`crate::audio::block`]). With `all`, what
//!   every `out.audio` of the project plays, through a tap of the [`Outputs`] hub, as
//!   `out.video`'s `audio: all` hears it: after reverb, level and limiter, before `audio.mute`.
//!   Blocks of a pass reach the node at its next cook.
//! - **Placement by show time:** sample k of the file is the sound at k / rate seconds after
//!   the start. While the clock stands still nothing is recorded; a jump back or of more than a
//!   second forward continues the file without a gap; a shorter jump forward leaves silence for
//!   the time skipped, as the picture of an `out.video` holds its frame for it.
//! - **Format:** rate and channels as the first block has them, fixed for the file; a block at
//!   another rate or with another channel count is left out (counted, the node warns).
//!
//! `record` is the one switch, as for `out.video`: the panel's record button and
//! `audio_file_out.start` / `.stop` set it (as edits), a binding drives it. A recording starts
//! when it is on and stops when it turns off, the node is deleted, its project closed or the app
//! quit; stopping finishes the file. `record` opens off whatever the project file says. A
//! recording that could not start or ended by itself (an error) is not tried again until
//! `record` turns off and on.

use penumbra_audio::playback::{MAX_RATE, MIN_RATE};
use penumbra_audio::{Outputs, Tap};
use penumbra_core::{Chans, Error, NodeId, ParamSpec, Tick};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_record::wav::MAX_CHANNELS;
use penumbra_record::{AudioBlock, Hub, SoundRecording, WavFormat, WavSettings};

use super::video::FOLDER;
use crate::audio::block::{BlockClock, Step, sample_at};

/// Kind of the sound recording node.
pub const AUDIO_FILE_OUT: &str = "out.audio_file";
/// Most of one input block that is recorded, seconds (the newest part of a longer one).
pub const MAX_BLOCK_SECONDS: f64 = 1.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(AUDIO_FILE_OUT, "Records sound to a WAV file: its input, or everything the out.audio nodes play")
            .param(ParamSpec::bool("record", false).opens_at_default().doc(
                "Recording while on: turning record on starts a new file, turning record off finishes the \
                 file. The record button above and audio_file_out.start / .stop switch this param; bind it to \
                 a channel to start and stop from OSC or a button. A project always opens with record off",
            ))
            .param(
                ParamSpec::choice("source", &["input", "all"])
                    .doc("What is recorded, fixed when the recording starts")
                    .choice_doc("input", "the sound wired to `in`, as it comes, with its rate and channels")
                    .choice_doc(
                        "all",
                        "what every out.audio of the project plays, added up as the speakers get it, in stereo \
                         (audio.mute silences the room, not the file). No out.audio: nothing is recorded",
                    ),
            )
            .param(
                ParamSpec::choice("format", &WavFormat::NAMES)
                    .doc("How the samples are stored, fixed when the recording starts")
                    .choice_doc("pcm24", "24-bit integers, what editing programs expect. Sound above full scale is clipped")
                    .choice_doc(
                        "float32",
                        "32-bit floats, in files a third larger. Sound above full scale is kept, for an input without a limiter",
                    ),
            )
            .param(ParamSpec::text("file", "").doc(
                "The WAV file, relative to the project; empty records to recordings/<node name>.wav. A file that \
                 exists is never overwritten: the recording gets the next free name with _2, _3, ...",
            ))
            .input("in", DataKind::Audio)
            .describe_input("in", "The sound to record with `source` at input: any number of channels up to 64")
            // Cooked at every new show tick: it reads the tick to tell a new block from the same one.
            .time_dependent(),
        || Box::new(AudioFileOut::default()) as Box<dyn Operator>,
    ))
}

/// A recording under way.
struct Take {
    recording: SoundRecording,
    /// Show time of the file's first sample.
    start: Tick,
    /// Hears the project's sound, with `source` all; `None`: the input.
    tap: Option<Tap>,
}

impl Take {
    /// Hands `block`, whose first sample is the show's sample `first`, to the recording at the
    /// file's sample of its show time.
    fn put(&self, first: i64, rate: u32, channels: Vec<Vec<f32>>) {
        let at = first.saturating_sub(sample_at(self.start, rate));
        self.recording.push(AudioBlock { at, rate, channels });
    }

    /// Hands the sound the tap heard since the last cook to the recording.
    fn listen(&self, outputs: Option<&Outputs>) {
        let Some(tap) = &self.tap else { return };
        for block in tap.take() {
            self.put(block.first, block.rate, block.channels);
        }
        let sources = outputs.map_or(0, Outputs::lanes);
        self.recording.sound(Some(sources), tap.dropped(), tap.refused());
    }
}

/// One new block of the input.
struct Block {
    first: i64,
    rate: u32,
    channels: Vec<Vec<f32>>,
    /// Channels the input has beyond [`MAX_CHANNELS`], left out.
    beyond: usize,
}

#[derive(Default)]
struct AudioFileOut {
    hub: Option<(Hub, NodeId)>,
    take: Option<Take>,
    /// A recording from the tap that stopped at the last cook, kept for one more: the sound of
    /// the pass that stopped it reaches the tap only after that pass.
    closing: Option<Take>,
    /// A recording was started (or tried) since `record` last turned on.
    tried: bool,
    /// Why the last start failed, shown while `record` stays on.
    failed: Option<String>,
    clock: BlockClock,
    /// The input recorded last, to tell a new block from the same one handed on again.
    last: Option<Data>,
}

impl Drop for AudioFileOut {
    fn drop(&mut self) {
        // The recording finishes its file on its own thread.
        self.take = None;
        self.closing = None;
        if let Some((hub, node)) = &self.hub {
            hub.detach_sound(*node);
        }
    }
}

/// The input's rate as a whole number of samples per second.
fn whole_rate(rate: f64) -> Result<u32, CookError> {
    if rate.is_finite() && rate.fract() == 0.0 && (f64::from(MIN_RATE)..=f64::from(MAX_RATE)).contains(&rate)
    {
        // `as`: checked to be a whole number in range.
        Ok(rate as u32)
    } else {
        Err(CookError::failed(format!(
            "the input is at {rate} samples per second; out.audio_file records whole rates from {MIN_RATE} to {MAX_RATE}"
        )))
    }
}

impl AudioFileOut {
    /// The input's new block at this tick, if there is one. `Ok(None)`: nothing connected, or
    /// the same block as before.
    fn input_block(&mut self, cx: &CookContext<'_>) -> Result<Option<Block>, CookError> {
        let Some(data) = cx.input(0) else {
            self.last = None;
            return Ok(None);
        };
        let Some(chans) = data.get::<Chans>() else {
            return Err(crate::waiting(cx));
        };
        let rate = whole_rate(chans.rate())?;
        if chans.channels().is_empty() {
            return Ok(None);
        }
        let time = cx.time();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), rate);
        let same = self.last.as_ref().is_some_and(|d| d.same(data));
        if span.step == Step::Again || same {
            return Ok(None);
        }
        self.last = Some(data.clone());
        // `as`: seconds to a sample count.
        let max = (f64::from(rate) * MAX_BLOCK_SECONDS) as usize;
        let all = chans.channels();
        let mut channels: Vec<Vec<f32>> = all
            .iter()
            .take(MAX_CHANNELS)
            .map(|c| {
                let skip = c.samples.len().saturating_sub(max);
                // `as`: samples to f32 as recordings keep them.
                c.samples.iter().skip(skip).map(|v| if v.is_finite() { *v as f32 } else { 0.0 }).collect()
            })
            .collect();
        let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
        for c in &mut channels {
            c.resize(frames, 0.0);
        }
        // The block ends now.
        let first = span.end.saturating_sub(i64::try_from(frames).unwrap_or(0));
        Ok(Some(Block { first, rate, channels, beyond: all.len().saturating_sub(MAX_CHANNELS) }))
    }

    fn start(&mut self, cx: &CookContext<'_>, hub: &Hub, node: NodeId, now: Tick) -> Result<(), CookError> {
        self.take = None;
        let all = match cx.text("source")? {
            "all" => true,
            "input" => false,
            other => return Err(CookError::failed(format!("unknown source `{other}`"))),
        };
        if !all && cx.input(0).is_none() {
            return Err(CookError::failed("nothing to record: connect a sound, or set source to all"));
        }
        let name = cx.text("format")?;
        let format =
            WavFormat::parse(name).ok_or_else(|| CookError::failed(format!("unknown format `{name}`")))?;
        let file = cx.text("file")?.trim();
        let mut path = if file.is_empty() {
            cx.resolve_path(FOLDER)?.join(cx.def().name.as_str())
        } else {
            cx.resolve_path(file)?
        };
        if path.extension().is_none() {
            path.set_extension("wav");
        }
        let recording = hub.start_sound(node, WavSettings { path, format }).map_err(CookError::failed)?;
        let tap = match (all, cx.resource::<Outputs>()) {
            (true, Some(outputs)) => {
                recording.sound(Some(outputs.lanes()), 0, 0);
                Some(outputs.tap())
            }
            (true, None) => return Err(CookError::failed("audio output is not available in this process")),
            (false, _) => None,
        };
        self.take = Some(Take { recording, start: now, tap });
        Ok(())
    }
}

impl Operator for AudioFileOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Hub>()
            .ok_or_else(|| CookError::failed("recording is not available in this process"))?;
        let node = cx.node();
        if self.hub.is_none() {
            self.hub = Some((hub.clone(), node));
        }
        hub.attach_sound(node, cx.def().name.as_str());
        let record = cx.bool("record")?;
        let now = cx.time().tick;
        if self.take.as_ref().is_some_and(|t| !t.recording.is_running()) {
            // Ended by itself (an error): its status says why.
            self.take = None;
        }
        let outputs = cx.resource::<Outputs>();
        if let Some(take) = self.closing.take() {
            // Dropping it finishes the file.
            take.listen(outputs);
        }
        if let Some(take) = &self.take {
            take.listen(outputs);
        }
        // The input's block of this tick belongs to the recording also when this cook stops it:
        // it holds the sound up to now.
        let input_take = self.take.as_ref().is_some_and(|t| t.tap.is_none());
        let starting = record && self.take.is_none() && !self.tried;
        let block = if input_take || (starting && cx.text("source")? == "input") {
            self.input_block(cx)?
        } else {
            self.last = None;
            self.clock.reset();
            None
        };
        if !record {
            if let Some(take) = self.take.take() {
                if let Some(b) = block.as_ref().filter(|_| take.tap.is_none()) {
                    take.put(b.first, b.rate, b.channels.clone());
                }
                if take.tap.is_some() && outputs.is_some_and(|o| o.lanes() > 0) {
                    self.closing = Some(take);
                }
            }
            self.tried = false;
            self.failed = None;
        } else {
            if !self.tried {
                self.tried = true;
                match self.start(cx, hub, node, now) {
                    Ok(()) => {}
                    Err(CookError::Failed(e)) => self.failed = Some(e),
                    Err(CookError::Pending) => self.tried = false,
                }
            }
            if let Some(take) = self.take.as_ref().filter(|t| t.tap.is_none())
                && let Some(b) = &block
            {
                take.put(b.first, b.rate, b.channels.clone());
            }
        }
        if let Some(e) = &self.failed {
            return Err(CookError::failed(format!(
                "not recording: {e}; turn record off and on to try again"
            )));
        }
        // The last recording's error and drops count while `record` is on; once off, the status
        // line of the panel and audio_file_out.status still tell.
        let Some(s) = hub.sound_status(node).filter(|_| record) else {
            return Ok(Cooked::Outputs(Vec::new()));
        };
        if let Some(e) = s.error {
            return Err(CookError::failed(format!("{e}; turn record off and on to try again")));
        }
        if s.dropped > 0 {
            cx.warn(format!("{} blocks dropped: the disk did not keep up", s.dropped));
        }
        if s.refused > 0 {
            let word = if s.channels == 1 { "channel" } else { "channels" };
            cx.warn(format!(
                "{} blocks left out: the sound is no longer the file's {} Hz, {} {word}; turn record off and on \
                 for a file in the new format",
                s.refused, s.rate, s.channels
            ));
        }
        if let Some(b) = block.as_ref().filter(|b| b.beyond > 0) {
            cx.warn(format!(
                "the file takes the first {MAX_CHANNELS} channels; {} more are left out",
                b.beyond
            ));
        }
        let from_tap = self.take.as_ref().is_some_and(|t| t.tap.is_some());
        if s.recording && from_tap && s.sources == Some(0) {
            cx.warn("no out.audio plays: nothing is recorded");
        } else if s.recording && !from_tap && cx.input(0).is_none() {
            cx.warn("nothing connected: nothing is recorded until a sound is");
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}
