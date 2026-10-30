//! Audio nodes: the `audio` family (sound as a gapless stream of blocks, see
//! [`DataKind::Audio`]) and the analysis that turns sound into channels.
//!
//! - **`audio.device_in`** reads an input device (by part of its name; empty is the system's
//!   default input) and outputs what arrived since its last cook, one channel per device
//!   channel (`ch1`, `ch2`, ...) at the device's sample rate. It recooks when samples arrive,
//!   not on the show clock, so it keeps listening while the show is paused.
//! - **`audio.oscillator`** ([`oscillator`]) makes a test tone or noise on the show clock,
//!   **`audio.mix`** ([`mix`]) adds sounds by a gain each, **`audio.filter`** ([`filter`])
//!   shapes a sound by frequency. [`block`] says which samples a generator owes per cook.
//! - **`audio.instrument`** ([`instrument`]) plays ready-made one-shot sounds (pluck, mallet,
//!   bell, kick, hat, sweep), **`audio.ambience`** ([`ambience`]) sustained ones (drone, pad,
//!   noise, water, rain), both in a key and scale ([`pitch`]). Their inputs are the bridge from `chan` to
//!   `audio`: a channel crossing a threshold starts a note or opens a gate at the sample the
//!   crossing falls on ([`lanes`]). [`synth`] holds the filter and saw they share.
//! - **`chan.audio_analysis`** turns sound into channels to bind, the bridge from `audio` to
//!   `chan`: levels (`level`, `low`,
//!   `mid`, `high`), events (`onset`, `kick`, `tick` are 1 in the frame they happen), the
//!   tempo (`bpm`, `confidence`, `locked` while a beat is heard) and the phases (`beat`, `bar` rising from 0 to 1, `bar_beat`
//!   counting). Its second output is a spectrum: one channel whose samples are log-spaced bands
//!   from 30 Hz to 16 kHz. See `penumbra-audio`'s `analysis` module for how. Its third, `meters`,
//!   is the viewer's meters as a texture ([`crate::tex::meters_picture`]), drawn only while
//!   something reads it.
//! - **`chan.audio_envelope`** ([`envelope`]) is the loudness of every channel now, with an
//!   attack and a release; **`chan.audio_spectrum`** ([`spectrum`]) the spectrum in bands, as
//!   one channel and as a texture (a row now, or a scrolling spectrogram).
//!
//! The device streams belong to the engine's [`Hub`], which opens what [`wants`] lists after
//! every change of the network.

pub mod ambience;
#[cfg(test)]
mod ambience_tests;
pub mod block;
pub mod envelope;
#[cfg(test)]
mod envelope_tests;
pub mod filter;
#[cfg(test)]
mod filter_tests;
pub mod instrument;
#[cfg(test)]
mod instrument_tests;
pub mod lanes;
pub mod mix;
pub mod oscillator;
pub mod pitch;
pub mod spectrum;
#[cfg(test)]
mod spectrum_tests;
pub mod synth;
#[cfg(test)]
mod tests;

use penumbra_audio::{Analyzer, Cursor, Hub, Settings, Watch};
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, SampleAxis, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

use crate::on_gpu;
use crate::tex::meters_picture::{self, MetersPicture};
use std::sync::Arc;

/// Kind of the audio input node.
pub const AUDIO_IN: &str = "audio.device_in";
/// Kind of the analysis node.
pub const AUDIO_ANALYSIS: &str = "chan.audio_analysis";
/// The index of the analysis' `meters` output.
const METERS: usize = 2;
/// Most seconds of audio one cook of `audio.device_in` hands on (after a long pause, the rest is
/// dropped).
pub const MAX_BLOCK_SECONDS: f64 = 0.5;

/// The channels `chan.audio_analysis` outputs, in order. `beats_per_bar`, `low_hz` and
/// `high_hz` repeat params (`beats_per_bar`, `low`, `high`), so what draws the analysis (its
/// `meters` output, the viewer) knows them from the channels alone, also while a param is
/// bound to a channel. New channels go at the end, so the order of the others stays.
pub const FEATURES: [&str; 18] = [
    "level",
    "low",
    "mid",
    "high",
    "onset",
    "kick",
    "beat",
    "tick",
    "bar",
    "bar_beat",
    "bpm",
    "confidence",
    "rms",
    "peak",
    "beats_per_bar",
    "low_hz",
    "high_hz",
    "locked",
];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_IN,
            "Sound from a microphone or audio interface: one channel per input channel, at the device's rate",
        )
        .block(penumbra_graph::Block::In)
        .sampled()
        .param(ParamSpec::text("device", "").doc(
            "Part of the input's name (Helpers > Audio lists them), e.g. `Focusrite`; empty: the system's \
                 default input",
        ))
        .output("out", DataKind::Audio),
        || Box::new(AudioIn::default()) as Box<dyn Operator>,
    ))?;
    let [width, height, keep_size] = meters_picture::params();
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_ANALYSIS,
            "Listens to sound and turns it into numbers to animate with: loudness, bass, mid and treble, hits, tempo and beat",
        )
        .stateful()
        .param(ParamSpec::text("channel", "").doc("The channel to analyse, e.g. `ch1`; empty: all mixed"))
        .param(ParamSpec::float("offset", 0.0).range(-200.0, 200.0).doc(
            "Milliseconds to move beat and bar ahead by, to make up for the projector's delay",
        ))
        .param(
            ParamSpec::int("beats_per_bar", 4)
                .clamp(1.0, 64.0)
                .range(2.0, 8.0)
                .doc("Beats in one bar (4 for 4/4, 3 for a waltz): how often bar and bar_beat come round"),
        )
        .param(
            ParamSpec::float("sensitivity", 0.5)
                .clamp(0.0, 1.0)
                .doc("How readily an onset or kick counts: higher finds quieter ones"),
        )
        .param(
            ParamSpec::float("min_bpm", 70.0)
                .clamp(20.0, 400.0)
                .range(40.0, 200.0)
                .more()
                .doc("The slowest tempo considered, in beats per minute. Keep min_bpm and max_bpm less than an octave apart: then a half or double tempo cannot be taken"),
        )
        .param(
            ParamSpec::float("max_bpm", 180.0)
                .clamp(20.0, 400.0)
                .range(80.0, 300.0)
                .more()
                .doc("The fastest tempo considered, in beats per minute; see min_bpm"),
        )
        .param(
            ParamSpec::bool("normalize", true)
                .doc("A quiet input still spans 0 to 1: levels and spectrum are measured against their range over the last 4 seconds. Off: level and bands are the raw RMS, full scale is 1"),
        )
        .param(
            ParamSpec::float("floor", -60.0)
                .clamp(-120.0, 0.0)
                .range(-90.0, -20.0)
                .unit(Unit::Decibels)
                .group("levels")
                .doc("Quieter than this counts as silence: with normalize, levels and spectrum read 0. Set it above the room's noise, so a silent room reads 0"),
        )
        .param(
            ParamSpec::float("trim", 0.0)
                .clamp(-penumbra_audio::analysis::MAX_TRIM_DB, penumbra_audio::analysis::MAX_TRIM_DB)
                .unit(Unit::Decibels)
                .group("levels")
                .doc("Makes the sound louder or quieter for the analysis only. Lift a quiet microphone until it is above floor. The rms and peak channels still show the sound as it comes in"),
        )
        .param(
            ParamSpec::float("low", 150.0)
                .clamp(20.0, 20_000.0)
                .range(40.0, 500.0)
                .unit(Unit::Hertz)
                .group("levels")
                .doc("Where the low band ends: the low channel is the level below this frequency"),
        )
        .param(
            ParamSpec::float("high", 2500.0)
                .clamp(20.0, 20_000.0)
                .range(1000.0, 8000.0)
                .unit(Unit::Hertz)
                .group("levels")
                .doc("Where the high band starts: the high channel is the level above this frequency. The mid channel covers the band between low and high"),
        )
        .param(
            ParamSpec::int("bands", 32)
                .clamp(1.0, penumbra_audio::analysis::MAX_BANDS as f64)
                .range(4.0, 128.0)
                .group("levels")
                .doc("Bands of the spectrum output, log-spaced from 30 Hz to 16 kHz: its samples per frame"),
        )
        .param(width)
        .param(height)
        .param(keep_size)
        .input("audio", DataKind::Audio)
        .describe_input("audio", "The sound to listen to, e.g. from an audio.device_in")
        .output("out", DataKind::Chan)
        .output("spectrum", DataKind::Chan)
        .output("meters", DataKind::Tex)
        .lazy_output("meters")
        .describe_output(
            "out",
            "Channels to bind: level, low, mid, high, onset, kick, tick, bpm, confidence, locked, beat, bar, bar_beat; also rms, peak, and beats_per_bar, low_hz, high_hz as the params set them",
        )
        .describe_output("spectrum", "One channel whose samples are log-spaced bands from 30 Hz to 16 kHz")
        .describe_output(
            "meters",
            "The meters as in the viewer, on transparent black: level and band bars with peaks, tempo, beat light, bar, hits, spectrum. Drawn only while something reads this output",
        ),
        || Box::new(Analysis::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(meters_picture::warm_up)))?;
    oscillator::register(types)?;
    mix::register(types)?;
    filter::register(types)?;
    instrument::register(types)?;
    ambience::register(types)?;
    envelope::register(types)?;
    spectrum::register(types)?;
    Ok(())
}

/// The devices the network's `audio.device_in` nodes want (stored params).
pub fn wants(net: &Network, types: &NodeTypes) -> Vec<String> {
    let Some(desc) = types.desc(AUDIO_IN) else { return Vec::new() };
    let mut keys: Vec<String> = net
        .nodes()
        .filter(|n| n.kind == AUDIO_IN)
        .filter_map(|n| n.param(desc, "device").and_then(|v| v.as_str()).map(|s| s.trim().to_string()))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

fn hub_of<'a>(cx: &'a CookContext<'_>) -> Result<&'a Hub, CookError> {
    cx.resource::<Hub>().ok_or_else(|| CookError::failed("audio input is not available in this process"))
}

#[derive(Default)]
struct AudioIn {
    watch: Option<Watch>,
    cursor: Cursor,
    /// Channel names made for a channel count.
    names: Vec<Name>,
}

impl Operator for AudioIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let key = cx.text("device")?.trim();
        if self.watch.as_ref().is_none_or(|w| w.key() != key) {
            let waker = cx.waker();
            self.watch = Some(hub.watch(key, Arc::new(move || waker.wake())));
            self.cursor = Cursor::default();
        }
        if let Some(w) = &self.watch {
            w.ack();
        }
        let label = if key.is_empty() { "the default input".to_string() } else { format!("`{key}`") };
        let Some(status) = hub.stream_status(key) else {
            return Err(CookError::failed(if hub.config().enabled {
                format!("not reading {label} yet")
            } else {
                "audio input is switched off (audio.config enabled)".to_string()
            }));
        };
        if !status.open
            && let Some(e) = status.error
        {
            return Err(CookError::failed(e));
        }
        let max = (f64::from(status.rate.max(1)) * MAX_BLOCK_SECONDS) as usize;
        let Some(block) = hub.read(key, &mut self.cursor, max) else {
            return Err(CookError::Pending);
        };
        if block.frames() == 0 {
            return if cx.first_cook() || cx.params_changed() {
                Err(CookError::Pending)
            } else {
                Ok(Cooked::Unchanged)
            };
        }
        if self.names.len() != block.channels.len() {
            self.names =
                (1..=block.channels.len()).filter_map(|i| Name::new(&format!("ch{i}")).ok()).collect();
        }
        let channels = self
            .names
            .iter()
            .zip(block.channels)
            .map(|(name, samples)| Channel {
                name: name.clone(),
                samples: samples.into_iter().map(f64::from).collect(),
            })
            .collect();
        let chans = Chans::new(f64::from(block.rate), channels)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}

/// The channel `wanted` of `audio` (empty: all channels averaged) into `mono`, as f32 for the
/// analyses.
pub(crate) fn mono(audio: &Chans, wanted: &str, mono: &mut Vec<f32>) -> Result<(), CookError> {
    let wanted = wanted.trim();
    mono.clear();
    if wanted.is_empty() {
        let n = audio.channels().len().max(1) as f64;
        mono.resize(audio.len(), 0.0);
        for c in audio.channels() {
            for (m, s) in mono.iter_mut().zip(&c.samples) {
                // `as`: samples to f32 for the analysis.
                *m += (s / n) as f32;
            }
        }
    } else {
        let Some(c) = audio.get(wanted) else {
            let names: Vec<String> = audio.names().map(ToString::to_string).collect();
            return Err(CookError::failed(format!(
                "no channel `{wanted}`; the input has {}",
                names.join(", ")
            )));
        };
        mono.extend(c.samples.iter().map(|s| *s as f32));
    }
    Ok(())
}

#[derive(Default)]
struct Analysis {
    analyzer: Option<Analyzer>,
    names: Vec<Name>,
    spectrum_name: Option<Name>,
    mono: Vec<f32>,
    /// The `out` and `spectrum` made last, to hand on again when only the meters are drawn anew.
    last: Option<(Data, Data)>,
    /// The `meters` output's drawing state, made when it is first read.
    picture: Option<Box<MetersPicture>>,
}

impl Analysis {
    fn settings(cx: &CookContext<'_>) -> Result<Settings, CookError> {
        Ok(Settings {
            low_hz: cx.float("low")?,
            high_hz: cx.float("high")?,
            sensitivity: cx.float("sensitivity")?,
            min_bpm: cx.float("min_bpm")?,
            max_bpm: cx.float("max_bpm")?,
            beats_per_bar: u32::try_from(cx.int("beats_per_bar")?.clamp(1, 64)).unwrap_or(4),
            bands: usize::try_from(cx.int("bands")?).unwrap_or(32),
            normalize: cx.bool("normalize")?,
            floor_db: cx.float("floor")?,
            trim_db: cx.float("trim")?,
            offset: cx.float("offset")? / 1000.0,
        })
    }
}

impl Analysis {
    /// The `meters` output: drawn while something reads it, else [`Data::skipped`]. A failure
    /// to draw is a warning, so the channels keep flowing.
    fn meters(&mut self, cx: &CookContext<'_>, out: &Data, spectrum: &Data, fresh: bool) -> Data {
        if !cx.output_wanted(METERS) {
            // Nobody reads it: the textures and the glyph atlas go too.
            self.picture = None;
            return Data::skipped(DataKind::Tex);
        }
        let (Some(out), Some(spectrum)) = (out.get::<Chans>(), spectrum.get::<Chans>()) else {
            return Data::skipped(DataKind::Tex);
        };
        let bands = spectrum.channels().first().map(|c| c.samples.as_slice());
        match self.picture.get_or_insert_with(Box::default).draw(cx, out, bands, fresh) {
            Ok(tex) => Data::new(DataKind::Tex, tex),
            Err(e) => {
                let why = match e {
                    CookError::Failed(m) => m,
                    CookError::Pending => "not ready".to_string(),
                };
                cx.warn(format!("the meters output is not drawn: {why}"));
                Data::skipped(DataKind::Tex)
            }
        }
    }
}

impl Operator for Analysis {
    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(audio) = cx.input_as::<Chans>(0) else {
            return Err(crate::waiting(cx));
        };
        if !cx.inputs_changed() && !cx.params_changed() && !cx.first_cook() {
            // No new audio: only the meters may want drawing, because they came to be read or
            // their text arrived.
            let redraw = cx.outputs_wanted_changed() || self.picture.as_ref().is_some_and(|p| p.job_done());
            return match self.last.clone() {
                Some((out, spectrum)) if redraw => {
                    let meters = self.meters(cx, &out, &spectrum, false);
                    Ok(Cooked::Outputs(vec![out, spectrum, meters]))
                }
                _ => Ok(Cooked::Unchanged),
            };
        }
        let settings = Self::settings(cx)?;
        let rate = audio.rate();
        if self.analyzer.as_ref().is_none_or(|a| a.rate() != rate) {
            self.analyzer = Some(Analyzer::new(rate).map_err(CookError::failed)?);
        }
        mono(audio, cx.text("channel")?, &mut self.mono)?;
        let Some(analyzer) = self.analyzer.as_mut() else { return Err(CookError::Pending) };
        let f = analyzer.process(&self.mono, &settings);
        if self.names.len() != FEATURES.len() {
            self.names = FEATURES.iter().filter_map(|n| Name::new(n).ok()).collect();
        }
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let values = [
            f.level,
            f.low,
            f.mid,
            f.high,
            flag(f.onset),
            flag(f.kick),
            f.beat,
            flag(f.tick),
            f.bar,
            f64::from(f.bar_beat),
            f.bpm,
            f.confidence,
            f.rms,
            f.peak,
            f64::from(settings.beats_per_bar),
            settings.low_hz,
            settings.high_hz,
            flag(f.locked),
        ];
        let channels = self
            .names
            .iter()
            .zip(values)
            .map(|(name, v)| Channel { name: name.clone(), samples: vec![v] })
            .collect();
        let out = Chans::new(cx.rate().as_f64(), channels)?;
        let name = match &self.spectrum_name {
            Some(n) => n.clone(),
            None => {
                let n = Name::new("spectrum")?;
                self.spectrum_name = Some(n.clone());
                n
            }
        };
        let spectrum = if f.spectrum.is_empty() { vec![0.0] } else { f.spectrum };
        let spectrum = Chans::new(cx.rate().as_f64(), vec![Channel { name, samples: spectrum }])?
            .with_axis(SampleAxis::Index);
        let (out, spectrum) = (Data::new(DataKind::Chan, out), Data::new(DataKind::Chan, spectrum));
        let meters = self.meters(cx, &out, &spectrum, true);
        self.last = Some((out.clone(), spectrum.clone()));
        Ok(Cooked::Outputs(vec![out, spectrum, meters]))
    }
}
