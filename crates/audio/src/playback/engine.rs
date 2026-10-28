//! The playback engine without a device: a ring of source samples, a resampler, the drift loop,
//! fades and counters. The device callback and the tests drive it through the same
//! [`Engine::pull`].
//!
//! - **Ring:** source frames, interleaved, preallocated when the source format changes (on the
//!   producer's side, never in the device callback). The first [`TAPS`] frames are mirrored past
//!   the end, so the resampler's window is always one contiguous slice.
//! - **Resampler:** windowed sinc, [`TAPS`] taps, a Kaiser window, [`PHASES`] phases with linear
//!   interpolation between them. The cutoff is half the lower of the two rates, so a 48 kHz
//!   source on a 44.1 kHz device is filtered before it is decimated. The same path runs when
//!   both rates are equal; the ratio is then 1 plus the drift correction.
//! - **Drift loop:** the producer follows the show clock, the device its own crystal. A slow PI
//!   loop steers the smoothed fill to the target by nudging the ratio, at most
//!   [`MAX_CORRECTION`].
//! - **Underrun:** when the ring is about to run dry the output fades out ([`FADE_SECONDS`]),
//!   then plays silence until the fill is back at the target, then fades in.
//! - **Overrun:** when the fill runs far beyond the target, the output fades out, the oldest
//!   frames are dropped down to the target, and it fades in again.

use serde::{Deserialize, Serialize};

/// Taps of the resampling filter.
pub const TAPS: usize = 32;
/// Filter phases between two source samples (interpolated linearly in between).
pub const PHASES: usize = 256;
/// Kaiser window shape: about 70 dB stopband.
const KAISER_BETA: f64 = 7.0;
/// Most source or device channels.
pub const MAX_CHANNELS: usize = 64;
/// Source rates accepted, Hz.
pub const MIN_RATE: u32 = 1_000;
pub const MAX_RATE: u32 = 768_000;
/// Length of a fade at a gap, a drop or a start.
pub const FADE_SECONDS: f64 = 0.004;
/// Largest change of the resampling ratio the drift loop may make (2000 ppm, about 3.5 cents
/// of pitch). Crystals differ by up to a few hundred ppm; a show clock counting frames at 60 Hz
/// on a display that runs at 59.94 Hz is 1000 ppm off.
pub const MAX_CORRECTION: f64 = 0.002;
/// Proportional gain of the drift loop: ratio change per second of fill error.
const KP: f64 = 0.1;
/// Integral gain, per second squared. With [`KP`] the loop is critically damped at 0.05 rad/s:
/// it settles in about two minutes, a drift of 200 ppm moves the fill by about 1.5 ms on the
/// way, and late or skipped show frames move the ratio by well under 100 ppm.
const KI: f64 = 0.0025;
/// Time constant of the fill's smoothing, seconds. The fill is a sawtooth (a block arrives per
/// show frame, late by a jitter, the device takes its own blocks); the loop sees its mean.
const SMOOTH_SECONDS: f64 = 4.0;
/// The ring holds this much beyond three times the target, seconds.
const RING_SECONDS: f64 = 1.0;
/// Latency targets accepted, milliseconds.
pub const MIN_LATENCY_MS: f64 = 5.0;
pub const MAX_LATENCY_MS: f64 = 2000.0;

/// Settings of a player.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// How far ahead of the device the queued samples should run, in milliseconds (three show
    /// frames at 60 fps by default). The device's own buffer comes on top.
    pub latency_ms: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { latency_ms: 50.0 }
    }
}

impl Settings {
    /// The target clamped to what the engine accepts (a non-number is the default).
    pub fn latency_seconds(&self) -> f64 {
        let ms = if self.latency_ms.is_finite() { self.latency_ms } else { Settings::default().latency_ms };
        ms.clamp(MIN_LATENCY_MS, MAX_LATENCY_MS) / 1000.0
    }
}

/// Where the output stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Nothing plays: no samples yet, or waiting for the fill to reach the target after a gap.
    #[default]
    Silent,
    FadingIn,
    Playing,
    /// Fading out because the samples run out.
    FadingOut,
    /// Fading out before the oldest samples are dropped.
    Dropping,
}

/// What the engine reports.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EngineStatus {
    pub state: State,
    /// Source format (0 before the first push).
    pub source_rate: u32,
    pub source_channels: u16,
    /// Device format (0 without a device).
    pub device_rate: u32,
    pub device_channels: u16,
    pub target_ms: f64,
    /// Queued ahead of what plays now, milliseconds, smoothed.
    pub fill_ms: f64,
    /// Queued ahead right now.
    pub fill_now_ms: f64,
    /// Source frames per device frame, drift correction included.
    pub ratio: f64,
    /// The drift correction, ppm (negative: the device runs fast against the show clock).
    pub correction_ppm: f64,
    /// Gaps: the producer was late and the output faded to silence.
    pub underruns: u64,
    /// Drops: the fill ran far beyond the target and the oldest samples were dropped.
    pub overruns: u64,
    /// Source frames dropped (by overruns and by a full ring).
    pub dropped_frames: u64,
    /// Source frames pushed.
    pub pushed_frames: u64,
    /// Device frames produced.
    pub played_frames: u64,
}

/// The engine. Not thread-safe by itself; the [`super::Player`] keeps it behind a lock.
pub struct Engine {
    settings: Settings,
    rate: u32,
    channels: usize,
    /// `(cap + TAPS) * channels` samples: `cap` frames and the first `TAPS` mirrored.
    ring: Vec<f32>,
    cap: usize,
    /// The oldest frame still needed (the start of the resampler's window).
    head: usize,
    len: usize,
    device_rate: u32,
    device_channels: usize,
    /// One row of coefficients per phase, and one more for the interpolation.
    kernel: Vec<[f32; TAPS]>,
    coeffs: [f32; TAPS],
    mix: [f32; MAX_CHANNELS],
    base_step: f64,
    frac: f64,
    state: State,
    gain: f64,
    smoothed: Option<f64>,
    integral: f64,
    correction: f64,
    underruns: u64,
    overruns: u64,
    dropped: u64,
    pushed: u64,
    played: u64,
    /// Frames of the latest block pushed.
    last_block: usize,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            rate: 0,
            channels: 0,
            ring: Vec::new(),
            cap: 0,
            head: 0,
            len: 0,
            device_rate: 0,
            device_channels: 0,
            kernel: vec![[0.0; TAPS]; PHASES + 1],
            coeffs: [0.0; TAPS],
            mix: [0.0; MAX_CHANNELS],
            base_step: 1.0,
            frac: 0.0,
            state: State::Silent,
            gain: 0.0,
            smoothed: None,
            integral: 0.0,
            correction: 0.0,
            underruns: 0,
            overruns: 0,
            dropped: 0,
            pushed: 0,
            played: 0,
            last_block: 0,
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// A new target takes effect at once; the drift loop moves the fill there.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        let cap = self.capacity_for(self.rate);
        if self.rate != 0 && self.channels != 0 && cap != self.cap {
            // Keeps the newest frames that fit; the output fades in again.
            let keep = self.len.min(cap);
            let mut ring = vec![0.0; (cap + TAPS) * self.channels];
            let ch = self.channels;
            for i in 0..keep {
                let from = (self.head + self.len - keep + i) % self.cap.max(1);
                for c in 0..ch {
                    let v = self.ring.get(from * ch + c).copied().unwrap_or(0.0);
                    write(&mut ring, cap, ch, i, c, v);
                }
            }
            self.dropped += (self.len - keep) as u64;
            self.ring = ring;
            self.cap = cap;
            self.head = 0;
            self.len = keep;
            self.silence();
        }
    }

    /// The device's format: called when it opens. Excess queued samples are dropped down to
    /// the target and the output fades in.
    pub fn set_device(&mut self, rate: u32, channels: usize) {
        self.device_rate = rate;
        self.device_channels = channels.min(MAX_CHANNELS);
        // A new device has its own crystal: the drift is learnt again.
        self.integral = 0.0;
        self.correction = 0.0;
        self.design();
        self.silence();
        let target = self.target_len();
        if self.len > target {
            let drop = self.len - target;
            self.consume(drop);
            self.dropped += drop as u64;
        }
    }

    /// No device: the output stops; pushes keep filling the ring up to its size.
    pub fn detach(&mut self) {
        self.device_rate = 0;
        self.device_channels = 0;
        self.silence();
    }

    fn silence(&mut self) {
        self.state = State::Silent;
        self.gain = 0.0;
        self.smoothed = None;
    }

    fn capacity_for(&self, rate: u32) -> usize {
        let seconds = RING_SECONDS + 3.0 * self.settings.latency_seconds();
        // `as`: a positive count of frames, well inside `usize`.
        ((f64::from(rate) * seconds).ceil() as usize).max(TAPS * 4)
    }

    /// Frames in the ring when the fill is at the target.
    fn target_len(&self) -> usize {
        // `as`: a positive count of frames.
        (self.target_frames().round() as usize) + TAPS / 2
    }

    fn target_frames(&self) -> f64 {
        f64::from(self.rate) * self.settings.latency_seconds()
    }

    /// Queued frames ahead of what plays now (the play point sits in the window's middle).
    fn fill_frames(&self) -> f64 {
        // `as`: frame counts to a fractional position.
        (self.len as f64 - (TAPS / 2 - 1) as f64 - self.frac).max(0.0)
    }

    /// Queues one block, one slice per channel, all of one length. Non-numbers play as
    /// silence, samples beyond full scale are clipped. A new rate or channel count drops what
    /// was queued and fades in again. Returns the frames queued.
    pub fn push<T: Copy + Into<f64>>(&mut self, rate: u32, channels: &[&[T]]) -> Result<usize, String> {
        check(rate, channels.len())?;
        let frames = channels.first().map_or(0, |c| c.len());
        if channels.iter().any(|c| c.len() != frames) {
            return Err("the channels of a block differ in length".into());
        }
        self.queue(rate, channels.len(), frames, |f, c| {
            channels.get(c).and_then(|s| s.get(f)).map_or(0.0, |v| (*v).into())
        });
        Ok(frames)
    }

    /// Queues interleaved samples (`channels` per frame; a trailing partial frame is ignored).
    pub fn push_interleaved(&mut self, rate: u32, channels: usize, samples: &[f32]) -> Result<usize, String> {
        check(rate, channels)?;
        let frames = samples.len() / channels;
        self.queue(rate, channels, frames, |f, c| {
            samples.get(f * channels + c).map_or(0.0, |v| f64::from(*v))
        });
        Ok(frames)
    }

    fn queue(&mut self, rate: u32, channels: usize, frames: usize, sample: impl Fn(usize, usize) -> f64) {
        if rate != self.rate || channels != self.channels {
            self.rate = rate;
            self.channels = channels;
            self.cap = self.capacity_for(rate);
            self.ring = vec![0.0; (self.cap + TAPS) * channels];
            self.head = 0;
            self.len = 0;
            self.frac = 0.0;
            self.design();
            self.silence();
        }
        self.pushed += frames as u64;
        self.last_block = frames;
        // A block larger than the ring keeps its newest part.
        let skip = frames.saturating_sub(self.cap);
        if skip > 0 {
            self.dropped += skip as u64;
        }
        let incoming = frames - skip;
        let overflow = (self.len + incoming).saturating_sub(self.cap);
        if overflow > 0 {
            self.consume(overflow);
            self.dropped += overflow as u64;
        }
        if skip + overflow > 0 && self.state != State::Silent {
            // Nobody pulled for a long while: what was playing jumped. Fade in from nothing.
            self.overruns += 1;
            self.state = State::FadingIn;
            self.gain = 0.0;
            self.smoothed = None;
        }
        for f in skip..frames {
            let at = (self.head + self.len) % self.cap;
            for c in 0..channels {
                let v = sample(f, c);
                // `as`: clipped to full scale first, so the narrowing is exact enough.
                let v = if v.is_finite() { v.clamp(-1.0, 1.0) as f32 } else { 0.0 };
                write(&mut self.ring, self.cap, channels, at, c, v);
            }
            self.len += 1;
        }
    }

    fn consume(&mut self, frames: usize) {
        let frames = frames.min(self.len);
        if self.cap > 0 {
            self.head = (self.head + frames) % self.cap;
        }
        self.len -= frames;
    }

    /// Builds the filter for the current pair of rates.
    fn design(&mut self) {
        if self.rate == 0 || self.device_rate == 0 {
            return;
        }
        let ratio = f64::from(self.device_rate) / f64::from(self.rate);
        self.base_step = 1.0 / ratio;
        // Cutoff in cycles per source sample: half the lower rate.
        let fc = 0.5 * ratio.min(1.0);
        let half = (TAPS / 2) as f64;
        let i0b = bessel_i0(KAISER_BETA);
        for (p, row) in self.kernel.iter_mut().enumerate() {
            let frac = p as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (k, h) in row.iter_mut().enumerate() {
                // Distance of tap `k` from the play point at `TAPS / 2 - 1 + frac`.
                let t = k as f64 - (half - 1.0) - frac;
                let x = t / half;
                let w = if x.abs() < 1.0 { bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / i0b } else { 0.0 };
                let v = 2.0 * fc * sinc(2.0 * fc * t) * w;
                sum += v;
                *h = v as f32;
            }
            if sum.abs() > 1e-9 {
                for h in row.iter_mut() {
                    // `as`: the normalisation in f64, stored as f32.
                    *h = (f64::from(*h) / sum) as f32;
                }
            }
        }
    }

    /// Fills `out` (interleaved, the device's channel count from [`Engine::set_device`]) with
    /// what plays next. Silence without a device or samples. Never allocates.
    pub fn pull(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let dc = self.device_channels;
        if dc == 0 || self.device_rate == 0 {
            return;
        }
        let frames = out.len() / dc;
        self.played += frames as u64;
        if self.rate == 0 || self.channels == 0 || self.cap == 0 {
            return;
        }
        self.control(frames);
        let step = self.base_step * (1.0 + self.correction);
        let fade = 1.0 / (FADE_SECONDS * f64::from(self.device_rate)).max(1.0);
        let start = self.start_len(step, fade);
        let target = self.target_len();
        let ch = self.channels;
        for frame in out.chunks_exact_mut(dc) {
            match self.state {
                State::Silent => {
                    if self.len >= start {
                        // Far ahead after a gap (a stalled producer caught up): the excess
                        // goes before anything plays.
                        let excess = self.len.saturating_sub(start.max(target));
                        self.consume(excess);
                        self.dropped += excess as u64;
                        self.state = State::FadingIn;
                        self.gain = 0.0;
                    } else {
                        continue;
                    }
                }
                State::Playing | State::FadingIn => {
                    // Starts the fade while enough is left to finish it.
                    let needed = (self.gain / fade) * step + 2.0;
                    // `as`: a frame count to compare with a fractional need.
                    if ((self.len - TAPS.min(self.len)) as f64) < needed {
                        self.state = State::FadingOut;
                        self.underruns += 1;
                    }
                }
                State::FadingOut | State::Dropping => {}
            }
            if self.len < TAPS {
                self.silence();
                continue;
            }
            self.interpolate();
            let g = smoothstep(self.gain) as f32;
            map(&self.mix, ch, frame, g);
            self.frac += step;
            let advance = self.frac.floor();
            self.frac -= advance;
            // `as`: a small non-negative whole number.
            self.consume(advance as usize);
            match self.state {
                State::FadingIn => {
                    self.gain += fade;
                    if self.gain >= 1.0 {
                        self.gain = 1.0;
                        self.state = State::Playing;
                    }
                }
                State::FadingOut | State::Dropping => {
                    self.gain -= fade;
                    if self.gain <= 0.0 {
                        self.gain = 0.0;
                        if self.state == State::Dropping {
                            let drop = self.len.saturating_sub(target);
                            self.consume(drop);
                            self.dropped += drop as u64;
                            self.state = State::FadingIn;
                            self.smoothed = None;
                        } else {
                            self.state = State::Silent;
                        }
                    }
                }
                State::Playing | State::Silent => {}
            }
        }
        for v in out.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
    }

    /// Frames needed before the output starts: the target plus half a block, at most half the
    /// target (the fill is a sawtooth that falls by a block between pushes, so its mean then
    /// starts near the target), and at least enough for a fade.
    fn start_len(&self, step: f64, fade: f64) -> usize {
        // `as`: a positive count of frames.
        let fade_len = (step / fade).ceil() as usize + TAPS + 4;
        // `as`: a positive count of frames.
        let half_block = self.last_block.min(self.target_frames() as usize) / 2;
        (self.target_len() + half_block).max(fade_len)
    }

    /// The drift loop and the overrun check, once per pull of `frames`.
    fn control(&mut self, frames: usize) {
        if self.state != State::Playing {
            return;
        }
        let dt = frames as f64 / f64::from(self.device_rate);
        let fill = self.fill_frames();
        let smoothed = match self.smoothed {
            Some(s) => s + (fill - s) * (1.0 - (-dt / SMOOTH_SECONDS).exp()),
            None => fill,
        };
        self.smoothed = Some(smoothed);
        let error = (smoothed - self.target_frames()) / f64::from(self.rate);
        self.integral = (self.integral + KI * error * dt).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        self.correction = (KP * error + self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let target = self.target_frames();
        let high = target + target.max(0.05 * f64::from(self.rate));
        if fill > high {
            self.state = State::Dropping;
            self.overruns += 1;
        }
    }

    /// The source channels at the play point, into `mix`.
    fn interpolate(&mut self) {
        let p = self.frac * PHASES as f64;
        let i = (p.floor() as usize).min(PHASES - 1);
        // `as`: the fraction between two phases.
        let t = (p - i as f64) as f32;
        let (Some(a), Some(b)) = (self.kernel.get(i), self.kernel.get(i + 1)) else {
            return;
        };
        for ((c, x), y) in self.coeffs.iter_mut().zip(a).zip(b) {
            *c = x + (y - x) * t;
        }
        let ch = self.channels;
        let window = self.ring.get(self.head * ch..(self.head + TAPS) * ch).unwrap_or(&[]);
        let mix = self.mix.get_mut(..ch).unwrap_or(&mut []);
        mix.fill(0.0);
        for (frame, h) in window.chunks_exact(ch).zip(self.coeffs.iter()) {
            for (m, v) in mix.iter_mut().zip(frame) {
                *m += h * v;
            }
        }
    }

    pub fn status(&self) -> EngineStatus {
        let rate = f64::from(self.rate.max(1));
        let fill_now = self.fill_frames();
        EngineStatus {
            state: self.state,
            source_rate: self.rate,
            source_channels: u16::try_from(self.channels).unwrap_or(u16::MAX),
            device_rate: self.device_rate,
            device_channels: u16::try_from(self.device_channels).unwrap_or(u16::MAX),
            target_ms: self.settings.latency_seconds() * 1000.0,
            fill_ms: self.smoothed.unwrap_or(fill_now) / rate * 1000.0,
            fill_now_ms: fill_now / rate * 1000.0,
            ratio: self.base_step * (1.0 + self.correction),
            correction_ppm: self.correction * 1e6,
            underruns: self.underruns,
            overruns: self.overruns,
            dropped_frames: self.dropped,
            pushed_frames: self.pushed,
            played_frames: self.played,
        }
    }
}

fn check(rate: u32, channels: usize) -> Result<(), String> {
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(format!("a sample rate of {rate} Hz cannot be played (from {MIN_RATE} to {MAX_RATE})"));
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(format!("{channels} channels cannot be played (from 1 to {MAX_CHANNELS})"));
    }
    Ok(())
}

/// Writes one sample of frame `at`, mirrored past the end for the first [`TAPS`] frames.
fn write(ring: &mut [f32], cap: usize, ch: usize, at: usize, c: usize, v: f32) {
    if let Some(s) = ring.get_mut(at * ch + c) {
        *s = v;
    }
    if at < TAPS
        && let Some(s) = ring.get_mut((cap + at) * ch + c)
    {
        *s = v;
    }
}

/// Source channels onto device channels: mono to all, a stereo source onto a mono device as
/// the mean of both, else by index (extra device channels stay silent).
fn map(mix: &[f32], ch: usize, frame: &mut [f32], g: f32) {
    let first = mix.first().copied().unwrap_or(0.0);
    match (ch, frame.len()) {
        (1, _) => frame.fill(first * g),
        (_, 1) => {
            let second = mix.get(1).copied().unwrap_or(0.0);
            frame.fill((first + second) * 0.5 * g);
        }
        _ => {
            for (o, v) in frame.iter_mut().zip(mix.iter().take(ch)) {
                *o = v * g;
            }
        }
    }
}

/// A gentle S curve for the fades (no kink at either end).
fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// The modified Bessel function of the first kind, order zero (for the Kaiser window).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        let f = half / f64::from(k);
        term *= f * f;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}
