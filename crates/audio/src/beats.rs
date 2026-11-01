//! Everything around the Beat This! network (an offline beat and downbeat tracker by CPJKU,
//! MIT licence): the input it expects, the chunks it runs on, and the beat times read from
//! its output. The network itself is a closure the caller passes in, so this module needs no
//! inference runtime.
//!
//! - **Front end:** a [`Frontend`] takes decoded samples at any rate and any channel count
//!   in blocks: channel mean, non-numbers as 0, resampled to [`SAMPLE_RATE`] with the
//!   [`Kernel`] of [`crate::resample`] read at exact positions (no delay), then a log-mel
//!   spectrogram that matches upstream's torchaudio `MelSpectrogram` followed by
//!   `log1p(1000 x)`: [`N_MELS`] bands, [`FPS`] frames per second, `1 + samples / HOP` frames.
//!   It keeps under 100 000 samples however long the sound is; only the
//!   spectrogram grows (about 25 kB per second).
//! - **Chunks:** a [`ChunkPlan`] cuts the spectrogram into the pieces of [`CHUNK_FRAMES`]
//!   frames the network was trained on, exactly as upstream. The chunks are independent, so
//!   a caller may run them in parallel; a [`Stitcher`] puts the outputs back together in any
//!   order. [`run_network`] does both in sequence, cancellable between chunks.
//! - **Peaks:** [`pick`] turns the per-frame logits into beat and downbeat times as
//!   upstream's "minimal" post-processing does.
//! - **Position:** a [`BeatGrid`] answers, for any time, which beat and bar it falls in and
//!   how far through them it is.
//!
//! [`track`] runs the whole chain on one block of samples. [`analyze_file`] runs it on a sound
//! file with the shipped network ([`BeatModel`]), its chunks in parallel, and keeps the result
//! in a cache.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

mod analyze;
mod cache;

pub use analyze::{
    BeatModel, IN_FLIGHT, MAX_SECONDS, MODEL_FILE, Progress, analyze_file, analyze_file_with, f_measure,
    model_path, run_parallel,
};

use crate::fft::{Complex, Fft};
use crate::resample::{CLIP, Kernel, MAX_CHANNELS, MAX_RATE, MIN_RATE, dot};

/// The rate the network was trained at, samples per second.
pub const SAMPLE_RATE: f64 = 22_050.0;
/// Samples per spectrogram frame window.
pub const N_FFT: usize = 1024;
/// Samples from one frame to the next (50 frames per second).
pub const HOP: usize = 441;
/// Spectrogram frames per second.
pub const FPS: f64 = 50.0;
/// Mel bands per frame.
pub const N_MELS: usize = 128;
/// Lowest and highest frequency of the mel bands, Hz.
pub const F_MIN: f64 = 30.0;
pub const F_MAX: f64 = 11_000.0;
/// Fewest samples (at [`SAMPLE_RATE`]) that give a spectrogram; a shorter sound has no frames.
/// The first frame mirrors 512 samples beyond the start, so it needs 513.
pub const MIN_SAMPLES: usize = N_FFT / 2 + 1;
/// Frames per network input.
pub const CHUNK_FRAMES: usize = 1500;
/// Frames at each end of a chunk whose output is dropped; neighbouring chunks overlap by this.
pub const BORDER_FRAMES: usize = 6;
/// The logit of a frame no chunk covered (and of a non-number from the network).
pub const NO_BEAT: f32 = -1000.0;
/// A peak is the largest logit within this many frames on either side.
pub const PEAK_RADIUS: usize = 3;
/// Beats per bar assumed where the downbeats give no bar length.
pub const DEFAULT_BEATS_PER_BAR: usize = 4;

/// Why beat tracking stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BeatError {
    /// The cancel flag was set.
    Cancelled,
    /// The samples cannot be read: rate, channel count or block shape.
    Input(String),
    /// The network failed or returned the wrong number of frames.
    Network(String),
}

impl fmt::Display for BeatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeatError::Cancelled => write!(f, "beat tracking was cancelled"),
            BeatError::Input(e) => write!(f, "{e}"),
            BeatError::Network(e) => write!(f, "beat network: {e}"),
        }
    }
}

impl std::error::Error for BeatError {}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// A log-mel spectrogram: `frames` rows of [`N_MELS`] values, row-major.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spectrogram {
    frames: usize,
    data: Vec<f32>,
}

impl Spectrogram {
    /// A spectrogram from rows of [`N_MELS`] values; a last incomplete row is dropped.
    pub fn from_rows(mut data: Vec<f32>) -> Self {
        let frames = data.len() / N_MELS;
        data.truncate(frames * N_MELS);
        Self { frames, data }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// All rows, `frames * N_MELS` values.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Frame `t`'s [`N_MELS`] values.
    pub fn row(&self, t: usize) -> Option<&[f32]> {
        self.data.get(t.checked_mul(N_MELS)?..(t + 1).checked_mul(N_MELS)?)
    }

    /// Seconds of sound the spectrogram covers.
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / FPS
    }
}

/// The number of spectrogram frames of `samples` samples at [`SAMPLE_RATE`].
pub fn frame_count(samples: usize) -> usize {
    if samples < MIN_SAMPLES { 0 } else { 1 + samples / HOP }
}

/// Turns decoded samples, pushed in blocks of any length, into the network's spectrogram.
/// Splitting the sound into other blocks gives the same spectrogram, value for value.
pub struct Frontend {
    channels: usize,
    /// Samples of an incomplete interleaved frame, kept for the next block.
    carry: Vec<f32>,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    resample: ToModelRate,
    mel: Mel,
}

impl Frontend {
    /// A front end for `channels` channels at `rate` samples per second. Errors for rates
    /// outside [`MIN_RATE`]..[`MAX_RATE`] and for 0 or more than [`MAX_CHANNELS`] channels.
    pub fn new(rate: f64, channels: usize) -> Result<Self, BeatError> {
        if !(rate.is_finite() && (MIN_RATE..=MAX_RATE).contains(&rate)) {
            return Err(BeatError::Input(format!(
                "a sample rate of {rate} Hz cannot be tracked (from {MIN_RATE} to {MAX_RATE})"
            )));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(BeatError::Input(format!(
                "{channels} channels cannot be tracked (from 1 to {MAX_CHANNELS})"
            )));
        }
        Ok(Self {
            channels,
            carry: Vec::new(),
            mono: Vec::new(),
            resampled: Vec::new(),
            resample: ToModelRate::new(rate),
            mel: Mel::new(),
        })
    }

    /// Adds interleaved samples. A block may end inside a frame; the rest waits for the next.
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        let ch = self.channels;
        self.mono.clear();
        let mut rest = samples;
        if !self.carry.is_empty() {
            let need = ch - self.carry.len();
            let (head, tail) = rest.split_at(need.min(rest.len()));
            self.carry.extend_from_slice(head);
            rest = tail;
            if self.carry.len() < ch {
                return;
            }
            self.mono.push(mean(&self.carry));
            self.carry.clear();
        }
        let frames = rest.chunks_exact(ch);
        let tail = frames.remainder();
        self.mono.extend(frames.map(mean));
        self.carry.extend_from_slice(tail);
        self.feed();
    }

    /// Adds one slice per channel, all of one length. Errors when the channel count or the
    /// lengths differ, or when interleaved samples of an incomplete frame are waiting.
    pub fn push_planar(&mut self, channels: &[&[f32]]) -> Result<(), BeatError> {
        if channels.len() != self.channels {
            return Err(BeatError::Input(format!(
                "{} channels for a front end of {}",
                channels.len(),
                self.channels
            )));
        }
        let n = channels.first().map_or(0, |c| c.len());
        if channels.iter().any(|c| c.len() != n) {
            return Err(BeatError::Input("the channels of a block differ in length".into()));
        }
        if !self.carry.is_empty() {
            return Err(BeatError::Input("an interleaved frame is incomplete".into()));
        }
        self.mono.clear();
        self.mono.resize(n, 0.0);
        let mut sums = vec![0.0f64; n];
        for c in channels {
            for (s, v) in sums.iter_mut().zip(c.iter()) {
                *s += f64::from(clean(*v));
            }
        }
        let k = self.channels as f64;
        for (m, s) in self.mono.iter_mut().zip(sums) {
            // `as`: the mean of clipped samples, within f32 range.
            *m = (s / k) as f32;
        }
        self.feed();
        Ok(())
    }

    /// Spectrogram frames finished so far.
    pub fn frames(&self) -> usize {
        self.mel.frames()
    }

    /// The rows of the frames finished so far, `frames() * N_MELS` values. They stay as they
    /// are: [`Frontend::finish`] only adds frames.
    pub fn rows(&self) -> &[f32] {
        &self.mel.out
    }

    /// Seconds of input taken so far (at the input rate).
    pub fn seconds_in(&self) -> f64 {
        self.resample.received as f64 / self.resample.from
    }

    /// Ends the sound: the last frames mirror the samples before the end, as upstream pads.
    pub fn finish(mut self) -> Spectrogram {
        self.resampled.clear();
        self.resample.finish(&mut self.resampled);
        self.mel.push(&self.resampled);
        self.mel.finish()
    }

    fn feed(&mut self) {
        self.resampled.clear();
        self.resample.push(&self.mono, &mut self.resampled);
        self.mel.push(&self.resampled);
    }
}

/// A sample as the front end reads it: non-numbers count as 0, the rest is clipped to
/// [`CLIP`] so no later sum overflows.
fn clean(v: f32) -> f32 {
    // `as`: CLIP is 1e6, exact in f32.
    if v.is_finite() { v.clamp(-(CLIP as f32), CLIP as f32) } else { 0.0 }
}

fn mean(frame: &[f32]) -> f32 {
    let sum: f64 = frame.iter().map(|v| f64::from(clean(*v))).sum();
    // `as`: the mean of clipped samples, within f32 range.
    (sum / frame.len().max(1) as f64) as f32
}

/// Mono samples at any rate to [`SAMPLE_RATE`]: output sample `n` is the kernel read at the
/// exact source position `n * from / SAMPLE_RATE`, with silence before the start and after
/// the end. The streaming [`crate::resample::Resampler`] lags by half its kernel; reading at
/// the position itself has no lag at all.
struct ToModelRate {
    from: f64,
    /// `None` when the input is at the model's rate already: the samples pass unchanged.
    kernel: Option<Kernel>,
    /// Source samples per output sample.
    step: f64,
    /// Source samples kept, and the source index of the first.
    buf: Vec<f32>,
    buf_start: u64,
    received: u64,
    made: u64,
    coeffs: Vec<f32>,
    window: Vec<f32>,
}

impl ToModelRate {
    fn new(from: f64) -> Self {
        let same = (from - SAMPLE_RATE).abs() < 1e-9;
        let kernel = (!same).then(|| Kernel::new(from, SAMPLE_RATE));
        let taps = kernel.as_ref().map_or(0, Kernel::taps);
        Self {
            from,
            kernel,
            step: from / SAMPLE_RATE,
            buf: Vec::new(),
            buf_start: 0,
            received: 0,
            made: 0,
            coeffs: vec![0.0; taps],
            window: vec![0.0; taps],
        }
    }

    /// The first source sample output `n` reads, and the fraction past the play point's tap.
    fn window_of(&self, n: u64, kernel: &Kernel) -> (i64, f64) {
        let x = n as f64 * self.step;
        let whole = x.floor();
        // `as`: a source index, far inside i64 for any sound that fits in memory.
        let start = whole as i64 - (kernel.delay() as i64 - 1);
        (start, x - whole)
    }

    fn push(&mut self, x: &[f32], out: &mut Vec<f32>) {
        self.received += x.len() as u64;
        let Some(kernel) = self.kernel.take() else {
            out.extend_from_slice(x);
            self.made += x.len() as u64;
            return;
        };
        self.buf.extend_from_slice(x);
        let taps = kernel.taps() as i64;
        loop {
            let (start, frac) = self.window_of(self.made, &kernel);
            // `as`: the counts stay far below i64::MAX.
            if start + taps > self.received as i64 {
                break;
            }
            out.push(self.sample(&kernel, start, frac));
            self.made += 1;
        }
        self.trim(&kernel);
        self.kernel = Some(kernel);
    }

    fn finish(&mut self, out: &mut Vec<f32>) {
        let Some(kernel) = self.kernel.take() else {
            return;
        };
        // `as`: a positive count of samples.
        let target = ((self.received as f64 / self.step).round() as u64).max(self.made);
        while self.made < target {
            let (start, frac) = self.window_of(self.made, &kernel);
            out.push(self.sample(&kernel, start, frac));
            self.made += 1;
        }
        self.kernel = Some(kernel);
    }

    /// The output for the window from source index `start`; zeros outside the sound.
    fn sample(&mut self, kernel: &Kernel, start: i64, frac: f64) -> f32 {
        kernel.coefficients(frac, &mut self.coeffs);
        let taps = kernel.taps();
        // `as`: buf_start and the lengths stay far below i64::MAX.
        let off = start - self.buf_start as i64;
        let end = self.buf_start as i64 + self.buf.len() as i64;
        if off >= 0 && start + taps as i64 <= end {
            // `as`: non-negative, checked above.
            let o = off as usize;
            if let Some(w) = self.buf.get(o..o + taps) {
                return dot(&self.coeffs, w);
            }
        }
        for (k, w) in self.window.iter_mut().enumerate() {
            let i = start + k as i64 - self.buf_start as i64;
            *w = if i < 0 {
                0.0
            } else {
                usize::try_from(i).ok().and_then(|i| self.buf.get(i)).copied().unwrap_or(0.0)
            };
        }
        dot(&self.coeffs, &self.window)
    }

    /// Drops source samples no later output reads.
    fn trim(&mut self, kernel: &Kernel) {
        let (start, _) = self.window_of(self.made, kernel);
        let keep_from = u64::try_from(start).unwrap_or(0);
        let drop = keep_from.saturating_sub(self.buf_start);
        if drop >= 8192 {
            // `as`: at most the buffer's length.
            let d = (drop as usize).min(self.buf.len());
            self.buf.drain(..d);
            self.buf_start += d as u64;
        }
    }
}

/// The front end's conversion to [`SAMPLE_RATE`] alone, for tests.
#[cfg(test)]
pub(crate) fn resample_for_tests(x: &[f32], from: f64) -> Vec<f32> {
    let mut r = ToModelRate::new(from);
    let mut out = Vec::new();
    for b in x.chunks(1000) {
        r.push(b, &mut out);
    }
    r.finish(&mut out);
    out
}

/// The fixed parts of a spectrogram frame: window, transform, twiddles and mel bands.
struct MelKernel {
    /// Periodic Hann window over [`N_FFT`] samples.
    window: Vec<f32>,
    /// The half-size complex transform a real frame is packed into (always made: 512 is a
    /// valid size; without it every frame would be silent).
    fft: Option<Fft>,
    /// `exp(-2 pi i k / N_FFT)` for `k` in `0..=N_FFT / 2`.
    twiddles: Vec<(f64, f64)>,
    /// Per mel band: its first frequency bin and the weights from there on.
    bands: Vec<(usize, Vec<f32>)>,
}

/// Hz to mel on the Slaney scale (linear below 1 kHz, logarithmic above), as torchaudio.
fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if f >= min_log_hz { min_log_mel + (f / min_log_hz).ln() / logstep } else { f / f_sp }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if m >= min_log_mel { min_log_hz * (logstep * (m - min_log_mel)).exp() } else { f_sp * m }
}

impl MelKernel {
    fn new() -> Self {
        let n = N_FFT as f64;
        let window = (0..N_FFT)
            // `as`: designed in f64, used in f32 like torch's window.
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos()) as f32)
            .collect();
        let half = N_FFT / 2;
        let twiddles = (0..=half)
            .map(|k| {
                let a = -std::f64::consts::TAU * k as f64 / n;
                (a.cos(), a.sin())
            })
            .collect();
        // torchaudio.functional.melscale_fbanks(513, 30, 11000, 128, 22050, None, "slaney")
        let bins = half + 1;
        let (lo, hi) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
        let points: Vec<f64> =
            (0..N_MELS + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64)).collect();
        // torchaudio spaces the bins from 0 to `sample_rate // 2`.
        let nyquist = (SAMPLE_RATE / 2.0).floor();
        let bands = points
            .windows(3)
            .map(|p| {
                let (a, b, c) = match p {
                    [a, b, c] => (*a, *b, *c),
                    _ => (0.0, 0.0, 0.0),
                };
                let weights: Vec<(usize, f32)> = (0..bins)
                    .filter_map(|k| {
                        let f = nyquist * k as f64 / half as f64;
                        let w = ((f - a) / (b - a)).min((c - f) / (c - b)).max(0.0);
                        // `as`: a weight in 0..1.
                        (w > 0.0).then_some((k, w as f32))
                    })
                    .collect();
                let first = weights.first().map_or(0, |w| w.0);
                (first, weights.into_iter().map(|w| w.1).collect())
            })
            .collect();
        let fft = Fft::new(half).ok();
        Self { window, fft, twiddles, bands }
    }

    /// One spectrogram row from the [`N_FFT`] samples around a frame's centre.
    fn row(&self, samples: &[f32], packed: &mut [Complex], mags: &mut [f32], row: &mut [f32]) {
        let (pairs, _) = samples.as_chunks::<2>();
        let (weights, _) = self.window.as_chunks::<2>();
        for (z, ([a, b], [wa, wb])) in packed.iter_mut().zip(pairs.iter().zip(weights)) {
            *z = Complex { re: a * wa, im: b * wb };
        }
        match &self.fft {
            Some(fft) => fft.forward(packed),
            None => packed.fill(Complex::default()),
        }
        // Split the packed transform into the even and odd samples' halves and join them.
        let half = packed.len();
        // torch.stft(normalized=True): the spectrum divided by sqrt(N_FFT).
        let scale = 1.0 / (N_FFT as f64).sqrt();
        for (k, (m, tw)) in mags.iter_mut().zip(&self.twiddles).enumerate() {
            let zk = packed.get(k % half.max(1)).copied().unwrap_or_default();
            let zc = packed.get((half - k % half.max(1)) % half.max(1)).copied().unwrap_or_default();
            let (zr, zi) = (f64::from(zk.re), f64::from(zk.im));
            let (cr, ci) = (f64::from(zc.re), -f64::from(zc.im));
            let (er, ei) = (0.5 * (zr + cr), 0.5 * (zi + ci));
            // (zk - conj zc) / 2i
            let (or, oi) = (0.5 * (zi - ci), -0.5 * (zr - cr));
            let (c, s) = *tw;
            let xr = er + c * or - s * oi;
            let xi = ei + c * oi + s * or;
            // `as`: a magnitude of clipped samples, within f32 range.
            *m = (xr.hypot(xi) * scale) as f32;
        }
        for (out, (first, weights)) in row.iter_mut().zip(&self.bands) {
            let sum: f64 = mags
                .get(*first..)
                .unwrap_or(&[])
                .iter()
                .zip(weights)
                .map(|(m, w)| f64::from(*m) * f64::from(*w))
                .sum();
            // `as`: back to the network's f32.
            *out = (1000.0 * sum as f32).ln_1p();
        }
    }
}

/// Mono samples at [`SAMPLE_RATE`], in blocks, to spectrogram rows.
struct Mel {
    kernel: MelKernel,
    /// The first [`MIN_SAMPLES`] samples: the first frames mirror them.
    head: Vec<f32>,
    buf: Vec<f32>,
    buf_start: usize,
    total: usize,
    next: usize,
    out: Vec<f32>,
    frame: Vec<f32>,
    packed: Vec<Complex>,
    mags: Vec<f32>,
}

impl Mel {
    fn new() -> Self {
        Self {
            kernel: MelKernel::new(),
            head: Vec::with_capacity(MIN_SAMPLES),
            buf: Vec::new(),
            buf_start: 0,
            total: 0,
            next: 0,
            out: Vec::new(),
            frame: vec![0.0; N_FFT],
            packed: vec![Complex::default(); N_FFT / 2],
            mags: vec![0.0; N_FFT / 2 + 1],
        }
    }

    fn frames(&self) -> usize {
        self.next
    }

    fn push(&mut self, x: &[f32]) {
        let room = MIN_SAMPLES - self.head.len().min(MIN_SAMPLES);
        self.head.extend(x.iter().take(room));
        self.buf.extend_from_slice(x);
        self.total += x.len();
        if self.total < MIN_SAMPLES {
            return;
        }
        // A frame is ready once every sample it reads has arrived.
        while self.next * HOP + N_FFT / 2 <= self.total {
            self.emit();
        }
        let keep_from = (self.next * HOP).saturating_sub(N_FFT);
        let drop = keep_from.saturating_sub(self.buf_start);
        if drop >= 1 << 16 {
            let d = drop.min(self.buf.len());
            self.buf.drain(..d);
            self.buf_start += d;
        }
    }

    fn finish(mut self) -> Spectrogram {
        let frames = frame_count(self.total);
        while self.next < frames {
            self.emit();
        }
        Spectrogram::from_rows(self.out)
    }

    /// The sample at `i` of the sound mirrored at both ends (torch's reflect padding).
    fn at(&self, i: isize) -> f32 {
        // `as`: total is far below isize::MAX.
        let n = self.total as isize;
        let j = if i < 0 {
            -i
        } else if i >= n {
            2 * (n - 1) - i
        } else {
            i
        };
        let Ok(j) = usize::try_from(j) else { return 0.0 };
        if let Some(v) = self.head.get(j) {
            return *v;
        }
        j.checked_sub(self.buf_start).and_then(|k| self.buf.get(k)).copied().unwrap_or(0.0)
    }

    fn emit(&mut self) {
        let centre = self.next * HOP;
        let half = N_FFT / 2;
        let inner = centre >= half && centre + half <= self.total;
        let direct = if inner {
            centre.checked_sub(half + self.buf_start).and_then(|o| self.buf.get(o..o + N_FFT))
        } else {
            None
        };
        let mut frame = std::mem::take(&mut self.frame);
        match direct {
            Some(s) => {
                for (d, v) in frame.iter_mut().zip(s) {
                    *d = *v;
                }
            }
            None => {
                for (k, v) in frame.iter_mut().enumerate() {
                    // `as`: small offsets around a centre far below isize::MAX.
                    *v = self.at(centre as isize + k as isize - half as isize);
                }
            }
        }
        let start = self.out.len();
        self.out.resize(start + N_MELS, 0.0);
        if let Some(row) = self.out.get_mut(start..) {
            self.kernel.row(&frame, &mut self.packed, &mut self.mags, row);
        }
        self.frame = frame;
        self.next += 1;
    }
}

/// The whole front end on one block of interleaved samples. Checks `cancel` about once per
/// second of sound.
pub fn spectrogram(
    samples: &[f32],
    channels: usize,
    rate: f64,
    cancel: Option<&AtomicBool>,
) -> Result<Spectrogram, BeatError> {
    let mut fe = Frontend::new(rate, channels)?;
    // `as`: a positive block length of about one second.
    let block = (rate.round() as usize).max(1) * channels;
    for b in samples.chunks(block) {
        if cancelled(cancel) {
            return Err(BeatError::Cancelled);
        }
        fe.push_interleaved(b);
    }
    Ok(fe.finish())
}

/// One network input: where it starts in the spectrogram and how many frames it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The spectrogram frame of the chunk's first row; negative for the first chunk, whose
    /// rows before frame 0 are zero.
    pub start: isize,
    /// Rows of the chunk: [`CHUNK_FRAMES`], or the piece plus both borders for a piece of at
    /// most `CHUNK_FRAMES - 2 * BORDER_FRAMES` frames.
    pub frames: usize,
}

/// How a spectrogram is cut into network inputs (upstream's `split_piece` with
/// `avoid_short_end`). Chunks start at `-6, 1482, 2970, ...`; the last one is moved back to
/// end where the piece ends. Rows beyond the piece are zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    frames: usize,
    chunks: Vec<Chunk>,
}

impl ChunkPlan {
    pub fn new(frames: usize) -> Self {
        // `as`: frame counts are far below isize::MAX.
        let (c, b, n) = (CHUNK_FRAMES as isize, BORDER_FRAMES as isize, frames as isize);
        let step = c - 2 * b;
        let mut starts = Vec::new();
        let mut s = -b;
        while s < n - b {
            starts.push(s);
            s += step;
        }
        if n > step
            && let Some(last) = starts.last_mut()
        {
            *last = n - (c - b);
        }
        let chunks = starts
            .into_iter()
            .map(|s| {
                let rows = (s + c).min(n) - s.max(0) + (-s).max(0) + (s + c - n).clamp(0, b);
                // `as`: positive, at most CHUNK_FRAMES.
                Chunk { start: s, frames: rows.max(0) as usize }
            })
            .collect();
        Self { frames, chunks }
    }

    /// Frames of the whole piece.
    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Chunk `k`'s input: `frames * N_MELS` values, row-major. `None` past the last chunk.
    pub fn input(&self, spect: &Spectrogram, k: usize) -> Option<Vec<f32>> {
        let chunk = self.chunks.get(k)?;
        let mut buf = vec![0.0f32; chunk.frames * N_MELS];
        for (i, dst) in buf.as_chunks_mut::<N_MELS>().0.iter_mut().enumerate() {
            // `as`: row offsets are far below isize::MAX.
            let f = chunk.start + i as isize;
            if let Some(src) = usize::try_from(f).ok().and_then(|f| spect.row(f)) {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = *s;
                }
            }
        }
        Some(buf)
    }

    pub fn stitcher(&self) -> Stitcher {
        Stitcher { plan: self.clone(), parts: vec![None; self.chunks.len()] }
    }
}

/// The network's per-frame output for a whole piece: one beat and one downbeat logit per
/// spectrogram frame (above 0: more likely than not).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Logits {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
}

/// Collects the chunks' outputs in any order and joins them as upstream does: the border
/// frames of each chunk are dropped, and where chunks overlap the earlier one counts.
pub struct Stitcher {
    plan: ChunkPlan,
    parts: Vec<Option<(Vec<f32>, Vec<f32>)>>,
}

impl Stitcher {
    /// Takes chunk `k`'s output, one beat and one downbeat logit per input row. Errors for a
    /// chunk the plan does not have and for the wrong number of logits.
    pub fn put(&mut self, k: usize, beat: Vec<f32>, downbeat: Vec<f32>) -> Result<(), BeatError> {
        let rows = self.plan.chunks.get(k).map(|c| c.frames);
        let Some(slot) = self.parts.get_mut(k) else {
            return Err(BeatError::Network(format!("there is no chunk {k}")));
        };
        let rows = rows.unwrap_or(0);
        if beat.len() != rows || downbeat.len() != rows {
            return Err(BeatError::Network(format!(
                "chunk {k} of {rows} frames gave {} beat and {} downbeat logits",
                beat.len(),
                downbeat.len()
            )));
        }
        *slot = Some((beat, downbeat));
        Ok(())
    }

    /// Chunks whose output is still missing.
    pub fn missing(&self) -> usize {
        self.parts.iter().filter(|p| p.is_none()).count()
    }

    /// The logits of the whole piece. Errors while a chunk is missing.
    pub fn finish(self) -> Result<Logits, BeatError> {
        if let Some(k) = self.parts.iter().position(Option::is_none) {
            return Err(BeatError::Network(format!("chunk {k} has no output")));
        }
        let n = self.plan.frames;
        let mut beat = vec![NO_BEAT; n];
        let mut downbeat = vec![NO_BEAT; n];
        // Later chunks first, so earlier ones overwrite them where they overlap.
        for (chunk, part) in self.plan.chunks.iter().zip(&self.parts).rev() {
            let Some((b, d)) = part else { continue };
            let keep = BORDER_FRAMES..chunk.frames.saturating_sub(BORDER_FRAMES);
            for i in keep {
                // `as`: row offsets are far below isize::MAX.
                let Ok(f) = usize::try_from(chunk.start + i as isize) else { continue };
                if let (Some(bo), Some(dout), Some(bv), Some(dv)) =
                    (beat.get_mut(f), downbeat.get_mut(f), b.get(i), d.get(i))
                {
                    *bo = if bv.is_nan() { NO_BEAT } else { *bv };
                    *dout = if dv.is_nan() { NO_BEAT } else { *dv };
                }
            }
        }
        Ok(Logits { beat, downbeat })
    }
}

/// Runs every chunk through `net` in order and joins the outputs. `net` gets a chunk's rows
/// (`frames * N_MELS` values) and its frame count, and returns one beat and one downbeat
/// logit per frame. `cancel` is checked before each chunk; `progress` hears
/// `(chunks done, chunks in all)` after each.
pub fn run_network<F, E>(
    spect: &Spectrogram,
    mut net: F,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Logits, BeatError>
where
    F: FnMut(&[f32], usize) -> Result<(Vec<f32>, Vec<f32>), E>,
    E: fmt::Display,
{
    let plan = ChunkPlan::new(spect.frames());
    let mut stitcher = plan.stitcher();
    let total = plan.len();
    for (k, chunk) in plan.chunks().iter().enumerate() {
        if cancelled(cancel) {
            return Err(BeatError::Cancelled);
        }
        let input = plan.input(spect, k).unwrap_or_default();
        let (b, d) = net(&input, chunk.frames).map_err(|e| BeatError::Network(e.to_string()))?;
        stitcher.put(k, b, d)?;
        progress(k + 1, total);
    }
    stitcher.finish()
}

/// Beat and downbeat times, seconds from the start of the sound, ascending.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Beats {
    pub beats: Vec<f64>,
    /// Each one is also in `beats` (when there are any beats).
    pub downbeats: Vec<f64>,
}

/// Peak frames merged into one: its first and last frame and their mean. A frame joins when it
/// is at most one frame after the mean so far, so the frames of a group are consecutive.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PeakGroup {
    first: usize,
    last: usize,
    mean: f64,
}

/// The peak frames of one logit track, adjacent ones merged.
fn peak_groups(v: &[f32]) -> Vec<PeakGroup> {
    let n = v.len();
    let frames = v.iter().enumerate().filter_map(|(i, x)| {
        let window = v.get(i.saturating_sub(PEAK_RADIUS)..(i + PEAK_RADIUS + 1).min(n))?;
        let max = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (*x == max && *x > 0.0).then_some(i)
    });
    // Peak frames at most one apart become their running mean (upstream `deduplicate_peaks`).
    let mut out = Vec::new();
    let mut current: Option<(PeakGroup, f64)> = None;
    for f in frames {
        let x = f as f64;
        current = match current {
            Some((g, c)) if x - g.mean <= 1.0 => {
                Some((PeakGroup { last: f, mean: g.mean + (x - g.mean) / (c + 1.0), ..g }, c + 1.0))
            }
            Some((g, _)) => {
                out.push(g);
                Some((PeakGroup { first: f, last: f, mean: x }, 1.0))
            }
            None => Some((PeakGroup { first: f, last: f, mean: x }, 1.0)),
        };
    }
    if let Some((g, _)) = current {
        out.push(g);
    }
    out
}

/// Peak frames of one logit track, adjacent ones merged, in seconds.
fn peaks(v: &[f32]) -> Vec<f64> {
    peak_groups(v).into_iter().map(|g| g.mean / FPS).collect()
}

/// The index of the beat nearest to `t` in ascending `beats`; the earlier one on a tie.
fn nearest(beats: &[f64], t: f64) -> Option<usize> {
    let i = beats.partition_point(|b| *b < t);
    match (i.checked_sub(1).and_then(|j| beats.get(j)), beats.get(i)) {
        (Some(a), Some(b)) => Some(if (a - t).abs() <= (b - t).abs() { i - 1 } else { i }),
        (Some(_), None) => Some(i - 1),
        (None, Some(_)) => Some(i),
        (None, None) => None,
    }
}

/// Beat and downbeat times from per-frame logits (upstream's "minimal" post-processing): a
/// frame is a peak when its logit is above 0 and the largest within [`PEAK_RADIUS`] frames;
/// peaks at most one frame apart merge into their mean; each downbeat moves to the nearest
/// beat, and downbeats that land on the same beat become one.
pub fn pick(beat: &[f32], downbeat: &[f32]) -> Beats {
    let beats = peaks(beat);
    let mut downbeats: Vec<f64> = peaks(downbeat)
        .into_iter()
        .map(|t| nearest(&beats, t).and_then(|i| beats.get(i)).copied().unwrap_or(t))
        .collect();
    downbeats.sort_by(f64::total_cmp);
    downbeats.dedup();
    Beats { beats, downbeats }
}

/// Beats known for certain while only the start of the piece has been through the network.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Partial {
    /// A prefix of what [`pick`] gives for the whole piece: the same first beats and the same
    /// first downbeats.
    pub beats: Beats,
    /// Seconds before which a [`BeatGrid`] of `beats` gives the same [`Position`] as the grid
    /// of the whole piece: the last downbeat known, once two are known. `None` before that.
    pub final_until: Option<f64>,
}

/// What [`pick`] will certainly give for a piece whose first `beat.len()` frames of logits
/// are final (the rest still unknown). A frame's peak depends on [`PEAK_RADIUS`] frames on
/// either side, and a merged peak is complete once the frame after it is decided, so peaks
/// ending within `PEAK_RADIUS + 2` frames of the end wait. A downbeat waits while a beat not
/// known yet could lie nearer to it than the known ones.
pub fn pick_final(beat: &[f32], downbeat: &[f32]) -> Partial {
    let frames = beat.len().min(downbeat.len());
    let (beat, downbeat) = (beat.get(..frames).unwrap_or(&[]), downbeat.get(..frames).unwrap_or(&[]));
    // A group ending at frame p is complete when frame p + 1 is decided: p + 1 + R < frames.
    let complete = |g: &PeakGroup| g.last + PEAK_RADIUS + 2 <= frames;
    let groups = peak_groups(beat);
    let known = groups.iter().take_while(|g| complete(g)).count();
    let beats: Vec<f64> = groups.iter().take(known).map(|g| g.mean / FPS).collect();
    // Every beat found later lies at or after this frame: the first undecided group, or the
    // first frame whose peak is not decided yet.
    let undecided = frames.saturating_sub(PEAK_RADIUS);
    let later = groups.get(known).map_or(undecided, |g| g.first.min(undecided)) as f64 / FPS;
    let mut downbeats: Vec<f64> = Vec::new();
    for g in peak_groups(downbeat) {
        if !complete(&g) {
            break;
        }
        let t = g.mean / FPS;
        let Some(&at) = nearest(&beats, t).and_then(|i| beats.get(i)) else { break };
        // A beat found later is at least `later - t` away; a tie would go to the earlier beat.
        if (at - t).abs() >= later - t {
            break;
        }
        if downbeats.last() != Some(&at) {
            downbeats.push(at);
        }
    }
    let final_until = (downbeats.len() >= 2).then(|| downbeats.last().copied()).flatten();
    Partial { beats: Beats { beats, downbeats }, final_until }
}

/// The whole chain on one block of interleaved samples: front end, every chunk through `net`
/// in order, peaks. See [`spectrogram`] and [`run_network`] for `cancel` and `progress`.
pub fn track<F, E>(
    samples: &[f32],
    channels: usize,
    rate: f64,
    net: F,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Beats, BeatError>
where
    F: FnMut(&[f32], usize) -> Result<(Vec<f32>, Vec<f32>), E>,
    E: fmt::Display,
{
    let spect = spectrogram(samples, channels, rate, cancel)?;
    let logits = run_network(&spect, net, cancel, progress)?;
    Ok(pick(&logits.beat, &logits.downbeat))
}

/// Where a moment falls among the beats and bars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// The beat the moment is in: 0 is the first beat found, negative before it.
    pub beat: i64,
    /// How far through that beat, 0 (on the beat) to below 1.
    pub beat_phase: f64,
    /// Seconds of the beat the moment is in.
    pub beat_seconds: f64,
    /// Seconds until the next beat.
    pub to_next_beat: f64,
    /// The bar: 0 begins at the first downbeat, negative before it.
    pub bar: i64,
    /// The beat within the bar, 0 on the downbeat.
    pub beat_in_bar: usize,
    pub beats_in_bar: usize,
    /// How far through the bar, counted in beats: 0 to below 1.
    pub bar_phase: f64,
    /// The moment lies between the first and the last beat. Outside them, beats continue at
    /// the spacing of the first two (before) or the last two (after).
    pub inside: bool,
}

/// Beat and downbeat times arranged for fast lookup of a [`Position`] (binary search).
///
/// Bars run from one downbeat to the next and have as many beats as lie between them.
/// Before the first downbeat, bars count back with the length of the first bar; after the
/// last, they go on with the length of the last bar. With fewer than two downbeats every
/// such bar has [`DEFAULT_BEATS_PER_BAR`] beats, and with none, bar 0 starts on the first
/// beat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeatGrid {
    beats: Vec<f64>,
    /// Indices into `beats` of the downbeats, ascending, no repeats.
    downs: Vec<i64>,
}

impl BeatGrid {
    /// Non-numbers are dropped, the beats sorted and repeats removed. Each downbeat counts
    /// at the beat nearest to it.
    pub fn new(beats: &[f64], downbeats: &[f64]) -> Self {
        let mut b: Vec<f64> = beats.iter().copied().filter(|t| t.is_finite()).collect();
        b.sort_by(f64::total_cmp);
        b.dedup();
        let mut downs: Vec<i64> = downbeats
            .iter()
            .filter(|t| t.is_finite())
            .filter_map(|t| nearest(&b, *t))
            .filter_map(|i| i64::try_from(i).ok())
            .collect();
        downs.sort_unstable();
        downs.dedup();
        Self { beats: b, downs }
    }

    pub fn beats(&self) -> &[f64] {
        &self.beats
    }

    /// The downbeats, seconds.
    pub fn downbeats(&self) -> Vec<f64> {
        self.downs
            .iter()
            .filter_map(|i| usize::try_from(*i).ok().and_then(|i| self.beats.get(i)))
            .copied()
            .collect()
    }

    /// Where `t` (seconds) falls. `None` for fewer than two beats or a `t` that is no number.
    pub fn at(&self, t: f64) -> Option<Position> {
        if !t.is_finite() {
            return None;
        }
        let n = self.beats.len();
        let (first, second) = (*self.beats.first()?, *self.beats.get(1)?);
        let (before_last, last) = (*self.beats.get(n.checked_sub(2)?)?, *self.beats.last()?);
        let (beat, phase, seconds, inside) = if t < first || t >= last {
            // `as`: a count of beats, far below i64::MAX.
            let (origin, base, p) = if t < first {
                (first, 0i64, second - first)
            } else {
                (last, n as i64 - 1, last - before_last)
            };
            let q = (t - origin) / p;
            let k = q.floor();
            // `as`: saturates far outside the piece, where the count no longer matters.
            (base.saturating_add(k as i64), q - k, p, t == last)
        } else {
            let i = self.beats.partition_point(|b| *b <= t).saturating_sub(1).min(n - 2);
            let (a, b) = (*self.beats.get(i)?, *self.beats.get(i + 1)?);
            (i as i64, (t - a) / (b - a), b - a, true)
        };
        let phase = phase.clamp(0.0, 1.0 - f64::EPSILON);
        let (bar, beat_in_bar, beats_in_bar) = self.bar_of(beat);
        Some(Position {
            beat,
            beat_phase: phase,
            beat_seconds: seconds,
            to_next_beat: (1.0 - phase) * seconds,
            bar,
            beat_in_bar,
            beats_in_bar,
            bar_phase: (beat_in_bar as f64 + phase) / beats_in_bar.max(1) as f64,
            inside,
        })
    }

    /// The bar of beat `k`, its place in the bar and the bar's length in beats.
    fn bar_of(&self, k: i64) -> (i64, usize, usize) {
        let default = DEFAULT_BEATS_PER_BAR as i64;
        let d = &self.downs;
        let (Some(&d0), Some(&dl)) = (d.first(), d.last()) else {
            return split(k, 0, 0, default);
        };
        let j = d.partition_point(|x| *x <= k);
        if j == 0 {
            let len = match d.get(1) {
                Some(d1) => d1 - d0,
                None => default,
            };
            return split(k, d0, 0, len);
        }
        if j >= d.len() {
            let len = match d.len().checked_sub(2).and_then(|i| d.get(i)) {
                Some(dp) => dl - dp,
                None => default,
            };
            return split(k, dl, d.len() as i64 - 1, len);
        }
        let (Some(a), Some(b)) = (d.get(j - 1), d.get(j)) else {
            return split(k, 0, 0, default);
        };
        split(k, *a, j as i64 - 1, b - a)
    }
}

/// Beat `k` in bars of `len` beats, bar `bar0` starting on beat `origin`.
fn split(k: i64, origin: i64, bar0: i64, len: i64) -> (i64, usize, usize) {
    let len = len.max(1);
    let off = k.saturating_sub(origin);
    let bar = bar0.saturating_add(off.div_euclid(len));
    // `as`: both in 0..len, and len is a count of beats.
    (bar, off.rem_euclid(len) as usize, len as usize)
}
