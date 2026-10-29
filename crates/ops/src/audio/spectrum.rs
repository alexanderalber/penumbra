//! `chan.audio_spectrum`: the spectrum of a sound in bands, as numbers and as a texture.
//!
//! Every cook transforms the newest samples once ([`Spectrum`], `penumbra-audio`'s `spectrum`
//! module: Hann window, the transform size following the bands and the rate). A band's value
//! is the amplitude of a sine holding the band's power, smoothed with `attack` and `release`
//! over the block's duration (so the smoothing does not depend on the frame rate), then put in
//! the `unit` asked for.
//!
//! `out` is one channel, `spectrum`, whose samples are the bands, low to high
//! ([`SampleAxis::Index`]). `texture` is the same numbers as a picture, `bands` pixels wide and
//! `history` rows high, the value in red, green and blue (grey) and alpha 1. Row 0 at the top is
//! the newest block; every new block moves the older rows down by one, so with `history` above 1
//! it scrolls as a spectrogram and `v` reads as "how long ago". The texture is made only while
//! something reads it: its rows start empty (0) when it is read again.
//!
//! Blocks: a new input block is pushed into the transform and smoothed from the bands at the
//! end of the last one. The same input again (a param changed, or a device that delivered
//! nothing) is transformed again without pushing it twice and smoothed from the bands at the
//! start of that block, and it replaces the newest texture row instead of adding one. Other
//! bands (a change of `bands`, `min`, `max` or `spacing`) start the smoothing over.

use half::f16;
use penumbra_audio::Spectrum;
use penumbra_audio::spectrum::{Bands, fft_size};
use penumbra_core::{Channel, Chans, Curve, Error, Name, ParamSpec, SampleAxis, Unit};
use penumbra_gpu::TexRing;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use super::envelope::{Scale, floor_param, time_param, unit_param};

/// Kind of the spectrum.
pub const SPECTRUM: &str = "chan.audio_spectrum";
/// Fewest and most bands.
pub const MIN_BANDS: usize = 4;
pub const MAX_BANDS: usize = penumbra_audio::analysis::MAX_BANDS;
/// Most texture rows.
pub const MAX_HISTORY: usize = 2048;
/// The index of the `texture` output.
const TEXTURE: usize = 1;
/// Bytes per texel: RGBA half floats.
const TEXEL: usize = 8;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(
        SPECTRUM,
        "The spectrum of a sound: how loud each frequency band is now, as numbers and as a texture for shaders",
    )
    .stateful()
    .sampled()
    .param(ParamSpec::text("channel", "").doc("The channel to listen to, e.g. `ch1`; empty: all mixed"))
    .param(
        ParamSpec::int("bands", 32)
            // `as`: small counts to a param range.
            .clamp(MIN_BANDS as f64, MAX_BANDS as f64)
            .range(4.0, 128.0)
            .curve(Curve::Log)
            .doc("How many bands the spectrum has: samples of out, pixels across texture"),
    )
    .param(
        ParamSpec::float("min", 30.0)
            .clamp(1.0, 96_000.0)
            .range(20.0, 1000.0)
            .curve(Curve::Log)
            .unit(Unit::Hertz)
            .doc("Where the lowest band starts, in Hz"),
    )
    .param(
        ParamSpec::float("max", 16_000.0)
            .clamp(1.0, 96_000.0)
            .range(1000.0, 20_000.0)
            .curve(Curve::Log)
            .unit(Unit::Hertz)
            .doc("Where the highest band ends, in Hz. Above half the sample rate the spectrum ends there"),
    )
    .param(
        ParamSpec::choice("spacing", &["log", "linear"])
            .doc("How the bands share the range from min to max")
            .choice_doc("log", "every band the same share of an octave, as the ear hears: more bands for bass and mids")
            .choice_doc("linear", "every band the same width in Hz: the highs take most of the bands"),
    )
    .param(time_param(
        "attack",
        0.0,
        "Seconds a band takes to get about two thirds of the way up when it gets louder. 0 follows at once",
    ))
    .param(time_param(
        "release",
        0.2,
        "Seconds a band takes to fall to about a third when it gets quieter. Longer lets bars sink slowly",
    ))
    .param(unit_param("level", "a band"))
    .param(floor_param(-60.0))
    .param(
        ParamSpec::int("history", 1)
            // `as`: a small count to a param range.
            .clamp(1.0, MAX_HISTORY as f64)
            .range(1.0, 512.0)
            .curve(Curve::Log)
            .group("texture")
            .doc("Rows of texture: 1 is the spectrum now; more keep that many blocks of the past below it, newest at the top, as a scrolling spectrogram"),
    )
    .input("audio", DataKind::Audio)
    .describe_input("audio", "The sound to look at, e.g. from an audio.device_in")
    .output("out", DataKind::Chan)
    .output("texture", DataKind::Tex)
    .lazy_output("texture")
    .describe_output("out", "One channel, spectrum, whose samples are the bands, low to high")
    .describe_output(
        "texture",
        "The bands as a picture, bands pixels wide: the value in grey, the newest row at the top, older rows below. Made only while something reads this output",
    );
    types.register(FnNodeType::new(desc, || Box::new(SpectrumNode::default()) as Box<dyn Operator>))
}

/// The params' bands at `rate`, made sane, and a warning when `max` was above Nyquist.
pub fn bands(count: i64, min: f64, max: f64, log: bool, rate: f64) -> (Bands, Option<String>) {
    // `as`: clamped to a small count first.
    let count = count.clamp(MIN_BANDS as i64, MAX_BANDS as i64) as usize;
    let nyquist = rate / 2.0;
    let (min, max) = (if min.is_finite() { min } else { 30.0 }, if max.is_finite() { max } else { 16_000.0 });
    let (low, high) = if min <= max { (min, max) } else { (max, min) };
    let mut warning = None;
    let high = if high > nyquist {
        warning =
            Some(format!("max is above half the sample rate; the spectrum ends at {} Hz", nyquist.round()));
        nyquist
    } else {
        high
    };
    let low = low.max(1.0);
    // A range too narrow (or upside down after the clamps) gets an octave below its top.
    let low = if high - low < 1.0 { (high / 2.0).max(0.5) } else { low };
    (Bands { count, low, high, log }, warning)
}

/// The texture's rows on the CPU and the textures they go to.
#[derive(Default)]
struct Picture {
    width: usize,
    height: usize,
    /// RGBA half floats, row 0 the newest.
    bytes: Vec<u8>,
    ring: TexRing,
}

impl Picture {
    /// Puts `row` at the top: a new row (`fresh`) moves the others down, else it replaces the
    /// top row. Starts over when the size changed.
    fn put(&mut self, row: &[f64], height: usize, fresh: bool) {
        let width = row.len().max(1);
        let stride = width * TEXEL;
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            let one: Vec<u8> =
                [0.0f32, 0.0, 0.0, 1.0].iter().flat_map(|v| f16::from_f32(*v).to_le_bytes()).collect();
            self.bytes = one.repeat(width * height);
        }
        if fresh && height > 1 {
            let len = self.bytes.len();
            self.bytes.copy_within(0..len - stride, stride);
        }
        for (texel, v) in self.bytes.as_chunks_mut::<TEXEL>().0.iter_mut().take(width).zip(row) {
            let [a, b] =
                f16::from_f64(if v.is_finite() { v.clamp(-65504.0, 65504.0) } else { 0.0 }).to_le_bytes();
            let [one_a, one_b] = f16::ONE.to_le_bytes();
            *texel = [a, b, a, b, a, b, one_a, one_b];
        }
    }

    fn upload(&mut self, cx: &CookContext<'_>) -> Result<penumbra_gpu::Tex, CookError> {
        let gpu = crate::gpu(cx)?;
        // `as`: both at most MAX_BANDS and MAX_HISTORY.
        let tex = self.ring.next(gpu, self.width as u32, self.height as u32, SPECTRUM)?;
        // The upload clears non-finite halves in place; there are none.
        gpu.write_rgba16f(&tex, &mut self.bytes)?;
        Ok(tex)
    }
}

#[derive(Default)]
struct SpectrumNode {
    spectrum: Option<Spectrum>,
    mono: Vec<f32>,
    raw: Vec<f64>,
    /// The smoothed band amplitudes at the start and at the end of the last block.
    start: Vec<f64>,
    end: Vec<f64>,
    /// The last input block.
    last: Option<Data>,
    /// The bands `start` and `end` are of; other bands start smoothing over.
    layout: Option<Bands>,
    name: Option<Name>,
    /// The texture's state, made when it is first read.
    picture: Option<Box<Picture>>,
}

impl SpectrumNode {
    /// The `texture` output: made while something reads it, else [`Data::skipped`]. A failure is
    /// a warning, so the channels keep flowing.
    fn texture(&mut self, cx: &CookContext<'_>, row: &[f64], fresh: bool) -> Result<Data, CookError> {
        if !cx.output_wanted(TEXTURE) {
            self.picture = None;
            return Ok(Data::skipped(DataKind::Tex));
        }
        // `as`: clamped to a small count first.
        let height = cx.int("history")?.clamp(1, MAX_HISTORY as i64) as usize;
        let picture = self.picture.get_or_insert_with(Box::default);
        picture.put(row, height, fresh);
        Ok(match picture.upload(cx) {
            Ok(tex) => Data::new(DataKind::Tex, tex),
            Err(e) => {
                let why = match e {
                    CookError::Failed(m) => m,
                    CookError::Pending => "not ready".to_string(),
                };
                cx.warn(format!("the texture is not made: {why}"));
                Data::skipped(DataKind::Tex)
            }
        })
    }
}

impl Operator for SpectrumNode {
    fn trim(&mut self) {
        if let Some(p) = &mut self.picture {
            p.ring.trim();
        }
    }

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
        let (bands, warning) = bands(
            cx.int("bands")?,
            cx.float("min")?,
            cx.float("max")?,
            cx.text("spacing")? != "linear",
            rate,
        );
        if let Some(w) = warning {
            cx.warn(w);
        }
        let size = fft_size(rate, &bands);
        let repeat = self.last.as_ref().is_some_and(|l| l.same(data));
        super::mono(input, cx.text("channel")?, &mut self.mono)?;
        if self.spectrum.as_ref().is_none_or(|s| s.rate() != rate || s.size() != size) {
            let mut s = Spectrum::new(rate, size).map_err(CookError::failed)?;
            // A new transform has not seen this block yet.
            s.push(&self.mono);
            self.spectrum = Some(s);
        } else if !repeat && let Some(s) = self.spectrum.as_mut() {
            s.push(&self.mono);
        }
        let Some(spectrum) = self.spectrum.as_mut() else { return Err(CookError::Pending) };
        spectrum.bands(&bands, &mut self.raw);
        if self.layout != Some(bands) {
            self.layout = Some(bands);
            self.start.clear();
            self.end.clear();
        }
        let mut smooth = if repeat { self.start.clone() } else { self.end.clone() };
        smooth.resize(self.raw.len(), 0.0);
        let seconds = input.len() as f64 / rate;
        let rise = penumbra_audio::envelope::coefficient(cx.float("attack")?, 1.0 / seconds.max(1e-9));
        let fall = penumbra_audio::envelope::coefficient(cx.float("release")?, 1.0 / seconds.max(1e-9));
        let start = smooth.clone();
        for (s, &v) in smooth.iter_mut().zip(&self.raw) {
            let k = if v > *s { rise } else { fall };
            *s = v + (*s - v) * k;
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        self.start = start;
        self.end = smooth;
        self.last = Some(data.clone());
        let scale = Scale::parse(cx.text("unit")?);
        let floor = cx.float("floor")?;
        let values: Vec<f64> = self.end.iter().map(|a| scale.apply(*a, floor)).collect();
        let name = match &self.name {
            Some(n) => n.clone(),
            None => {
                let n = Name::new("spectrum")?;
                self.name = Some(n.clone());
                n
            }
        };
        let texture = self.texture(cx, &values, !repeat)?;
        let out = Chans::new(cx.rate().as_f64(), vec![Channel { name, samples: values }])?
            .with_axis(SampleAxis::Index);
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Chan, out), texture]))
    }
}
