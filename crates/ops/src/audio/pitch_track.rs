//! `chan.audio_pitch`: which tone is sounding, as channels to bind. See `penumbra-audio`'s
//! `pitch` module for the method.
//!
//! Blocks: as in [`super::envelope`], the tracker's state carries across blocks. A new input
//! block continues from the state at the end of the last one. The same input again (a param
//! changed, or a device that delivered nothing) is analysed again from the state at the start
//! of that block, so no block counts twice and a recook gives the same values. A change of
//! sample rate starts over.

use penumbra_audio::pitch::{self, PitchSettings, PitchState, PitchTracker};
use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::envelope::time_param;

/// Kind of the pitch tracker.
pub const PITCH: &str = "chan.audio_pitch";

/// The channels it outputs, in order. New channels go at the end, so the order of the others
/// stays.
pub const CHANNELS: [&str; 7] =
    ["note", "frequency", "pitch_class", "clarity", "sure", "heard_note", "heard_frequency"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(
        PITCH,
        "Which tone is sounding: the note of a voice, a whistle or an instrument, held through pauses, as numbers to bind",
    )
    .stateful()
    .param(ParamSpec::text("channel", "").doc("The channel to listen to, e.g. `ch1`; empty: all mixed"))
    .param(
        ParamSpec::float("min", 50.0)
            .clamp(pitch::MIN_HZ, pitch::MAX_HZ)
            .range(20.0, 1000.0)
            .curve(Curve::Log)
            .unit(Unit::Hertz)
            .doc("The lowest tone looked for. Lower costs latency: the analysis waits for two periods of it, 40 ms at 50 Hz. 80 suits a voice, 40 a bass, 400 a whistle"),
    )
    .param(
        ParamSpec::float("max", 4000.0)
            .clamp(pitch::MIN_HZ, pitch::MAX_HZ)
            .range(100.0, 8000.0)
            .curve(Curve::Log)
            .unit(Unit::Hertz)
            .doc("The highest tone looked for. Sound above it is filtered down, so a range of 40 to 200 picks a bass line under a louder melody. A tone above max reads clarity 0"),
    )
    .param(
        ParamSpec::float("threshold", 0.8)
            .clamp(0.0, 1.0)
            .doc("The clarity a tone needs to move note. Clean singing and whistling read above 0.9, white noise below 0.3, rumble and dense music mostly below 0.8. Lower follows more of a mix and jumps more"),
    )
    .param(time_param(
        "smooth",
        0.05,
        "Seconds note takes to get about two thirds of the way to a new tone. 0 jumps at once; longer glides from note to note",
    ))
    .param(
        ParamSpec::float("floor", -60.0)
            .clamp(-120.0, 0.0)
            .range(-90.0, -20.0)
            .unit(Unit::Decibels)
            .doc("Quieter than this counts as silence: clarity 0, note stays. Set it above the room's noise"),
    )
    .input("audio", DataKind::Audio)
    .describe_input("audio", "The sound to listen to, e.g. from an audio.device_in or an audio.file")
    .output("out", DataKind::Chan)
    .describe_output(
        "out",
        "note: the last sure tone as a MIDI note number with fraction (60 is middle C, 69 is A at 440 Hz, 12 is an octave), held through silence and noise, 0 until the first tone. frequency: the same in Hz. pitch_class: note within its octave, 0 (C) to 12. clarity: 0 to 1, how clearly the sound repeats at its pitch now. sure: 1 while note follows the sound. heard_note, heard_frequency: the estimate now, sure or not, 0 in silence",
    );
    types.register(FnNodeType::new(desc, || Box::new(Tracker::default()) as Box<dyn Operator>))
}

#[derive(Default)]
struct Tracker {
    tracker: Option<PitchTracker>,
    /// The state at the start of the last block.
    start: Option<PitchState>,
    /// The last input block.
    last: Option<Data>,
    names: Vec<Name>,
    mono: Vec<f32>,
}

impl Operator for Tracker {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(data) = cx.input(0) else {
            return Err(crate::waiting_for(cx, "connect sound to audio"));
        };
        let Some(input) = data.get::<Chans>() else {
            return Err(CookError::failed("audio carries no sound"));
        };
        let rate = input.rate();
        let settings = PitchSettings {
            min_hz: cx.float("min")?,
            max_hz: cx.float("max")?,
            threshold: cx.float("threshold")?,
            smooth: cx.float("smooth")?,
            floor_db: cx.float("floor")?,
        };
        if self.tracker.as_ref().is_none_or(|t| t.rate() != rate) {
            self.tracker = Some(PitchTracker::new(rate).map_err(CookError::failed)?);
            self.start = None;
            self.last = None;
        }
        super::mono(input, cx.text("channel")?, &mut self.mono)?;
        let Some(tracker) = self.tracker.as_mut() else { return Err(CookError::Pending) };
        match (&self.last, &self.start) {
            (Some(last), Some(start)) if last.same(data) => tracker.restore(start),
            _ => self.start = Some(tracker.snapshot()),
        }
        self.last = Some(data.clone());
        let p = tracker.process(&self.mono, &settings);
        if self.names.len() != CHANNELS.len() {
            self.names = CHANNELS.iter().filter_map(|n| Name::new(n).ok()).collect();
        }
        let class = if p.frequency > 0.0 { p.note.rem_euclid(12.0) } else { 0.0 };
        let values = [
            p.note,
            p.frequency,
            class,
            p.clarity,
            f64::from(u8::from(p.sure)),
            p.heard_note,
            p.heard_frequency,
        ];
        let channels = self
            .names
            .iter()
            .zip(values)
            .map(|(name, v)| Channel { name: name.clone(), samples: vec![v] })
            .collect();
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}
