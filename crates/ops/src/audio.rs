//! Audio nodes.
//!
//! - **`chan.audio_in`** reads an input device (by part of its name; empty is the system's
//!   default input) and outputs what arrived since its last cook, one channel per device
//!   channel (`ch1`, `ch2`, ...) at the device's sample rate. It recooks when samples arrive,
//!   not on the show clock, so it keeps listening while the show is paused.
//! - **`chan.audio_analysis`** turns such audio into channels to bind: levels (`level`, `low`,
//!   `mid`, `high`), events (`onset`, `kick`, `tick` are 1 in the frame they happen), the
//!   tempo (`bpm`, `confidence`) and the phases (`beat`, `bar` rising from 0 to 1, `bar_beat`
//!   counting). Its second output is a spectrum: one channel whose samples are log-spaced bands
//!   from 30 Hz to 16 kHz. See `penumbra-audio`'s `analysis` module for how.
//!
//! The device streams belong to the engine's [`Hub`], which opens what [`wants`] lists after
//! every change of the network.

use penumbra_audio::{Analyzer, Cursor, Hub, Settings, Watch};
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, SampleAxis, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use std::sync::Arc;

/// Kind of the audio input node.
pub const AUDIO_IN: &str = "chan.audio_in";
/// Kind of the analysis node.
pub const AUDIO_ANALYSIS: &str = "chan.audio_analysis";
/// Most seconds of audio one cook of `chan.audio_in` hands on (after a long pause, the rest is
/// dropped).
pub const MAX_BLOCK_SECONDS: f64 = 0.5;

/// The channels `chan.audio_analysis` outputs, in order. The last three repeat params
/// (`beats_per_bar`, `low`, `high`), so what draws the analysis (`tex.audio_meters`, the
/// viewer) knows them without the node, also while a param is bound to a channel.
pub const FEATURES: [&str; 17] = [
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
];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_IN,
            "Audio from an input device: one channel per device channel, at its sample rate",
        )
        .sampled()
        .param(ParamSpec::text("device", "").doc(
            "Part of the input's name (Helpers > Audio lists them), e.g. `Focusrite`; empty: the system's \
                 default input",
        ))
        .output("out", DataKind::Chan),
        || Box::new(AudioIn::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_ANALYSIS,
            "Levels, onsets, tempo, beat and bar phase, and a spectrum, from audio (a chan.audio_in)",
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
                .doc("The slowest tempo considered; with max_bpm less than an octave apart, so a half or double tempo is not taken"),
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
                .doc("Levels and spectrum relative to the loudest of the last seconds, so a quiet input still spans 0 to 1"),
        )
        .param(
            ParamSpec::float("floor", -60.0)
                .clamp(-120.0, 0.0)
                .range(-90.0, -20.0)
                .unit(Unit::Decibels)
                .group("levels")
                .doc("Quieter than this counts as silence and is not raised by normalize"),
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
                .doc("Where the high band starts: the high channel is the level above this; mid lies between low and high"),
        )
        .param(
            ParamSpec::int("bands", 32)
                .clamp(1.0, penumbra_audio::analysis::MAX_BANDS as f64)
                .range(4.0, 128.0)
                .group("levels")
                .doc("Bands of the spectrum output, log-spaced from 30 Hz to 16 kHz: its samples per frame"),
        )
        .input("audio", DataKind::Chan)
        .output("out", DataKind::Chan)
        .output("spectrum", DataKind::Chan)
        .describe_output(
            "out",
            "Channels to bind: level, low, mid, high, onset, kick, tick, bpm, confidence, beat, bar, bar_beat; also rms, peak, and beats_per_bar, low_hz, high_hz as the params set them",
        )
        .describe_output("spectrum", "One channel whose samples are log-spaced bands from 30 Hz to 16 kHz"),
        || Box::new(Analysis::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The devices the network's `chan.audio_in` nodes want (stored params).
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
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, chans)]))
    }
}

#[derive(Default)]
struct Analysis {
    analyzer: Option<Analyzer>,
    names: Vec<Name>,
    spectrum_name: Option<Name>,
    mono: Vec<f32>,
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
            offset: cx.float("offset")? / 1000.0,
        })
    }
}

impl Operator for Analysis {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let Some(audio) = cx.input_as::<Chans>(0) else {
            return Err(crate::chan::waiting(cx));
        };
        if !cx.inputs_changed() && !cx.params_changed() && !cx.first_cook() {
            return Ok(Cooked::Unchanged);
        }
        let settings = Self::settings(cx)?;
        let rate = audio.rate();
        if self.analyzer.as_ref().is_none_or(|a| a.rate() != rate) {
            self.analyzer = Some(Analyzer::new(rate).map_err(CookError::failed)?);
        }
        let wanted = cx.text("channel")?.trim();
        self.mono.clear();
        if wanted.is_empty() {
            let n = audio.channels().len().max(1) as f64;
            self.mono.resize(audio.len(), 0.0);
            for c in audio.channels() {
                for (m, s) in self.mono.iter_mut().zip(&c.samples) {
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
            self.mono.extend(c.samples.iter().map(|s| *s as f32));
        }
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
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, out), Data::new(DataKind::Chan, spectrum)]))
    }
}
