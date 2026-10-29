//! A room reverb for the output chain: a feedback delay network of eight lines.
//!
//! - **Input:** every input channel passes a short pre-delay and four allpass diffusers, which
//!   smear a click into a dense burst. Each channel then enters the eight lines with its own
//!   pattern of signs.
//! - **The network:** each line is a delay of a prime number of samples (about 11 to 62 ms,
//!   scaled by `room` and by the rate). What leaves the lines is damped (a one-pole low-pass
//!   whose gains give the decay time at low and at high frequencies, after Jot), mixed by an
//!   8 x 8 Hadamard matrix (orthogonal, so the mix itself neither adds nor loses energy) and
//!   written back with the input.
//! - **Output:** every output channel reads the lines through its own row of signs; the rows
//!   are orthogonal, so left and right are decorrelated and a mono input gets width. The wet
//!   signal is scaled so its impulse response carries about the energy of the input: `mix`
//!   then sets the ratio of dry to reverberant energy directly.
//! - **Changes:** a new `room` glides the line lengths (at most 2 % of a sample per sample, a
//!   short bend of the tail's pitch instead of a click) and ramps the gains across one block.
//! - **Silence:** once the input and everything written into the network stay below -140 dBFS
//!   for as long as the longest buffer, the state is cleared and the output is exactly 0. So a
//!   tail never decays into denormal numbers, and a silent input costs almost nothing.
//!
//! Nothing here allocates after [`Reverb::new`].

use std::f64::consts::FRAC_PI_2;

/// Lines of the network.
pub const LINES: usize = 8;
/// The decay time of `room` 0, seconds (time to fall by 60 dB, at low frequencies).
pub const MIN_RT60: f64 = 0.3;
/// The decay time of `room` 1, seconds.
pub const MAX_RT60: f64 = 4.0;
/// The default share of the reverb (`mix`): the reverberant energy about 10 dB below the dry.
pub const DEFAULT_MIX: f64 = 0.2;
/// The default `room`: a decay of about 0.85 s.
pub const DEFAULT_ROOM: f64 = 0.4;
/// The default `damping`: high frequencies die about twice as fast as low ones.
pub const DEFAULT_DAMPING: f64 = 0.5;
/// The frequency at which `damping` sets the decay, Hz (lower at rates below 18 kHz).
pub const DAMPING_HZ: f64 = 8000.0;

/// The line lengths at the largest room, milliseconds (made prime in samples).
const LINE_MS: [f64; LINES] = [31.1, 35.3, 39.7, 43.1, 47.9, 52.3, 56.9, 61.7];
/// The line lengths of `room` 0, as a share of [`LINE_MS`].
const MIN_SIZE: f64 = 0.35;
/// The input diffusers, milliseconds (stretched a little per channel).
const DIFFUSER_MS: [f64; 4] = [1.13, 2.41, 3.77, 5.29];
const DIFFUSER_GAIN: f64 = 0.6;
/// The pre-delay of `room` 0 and of `room` 1, milliseconds.
const PRE_MS: (f64, f64) = (3.0, 20.0);
/// The most a line length changes per sample while it glides, samples.
const GLIDE: f64 = 0.02;
/// Below this an input sample counts as silence.
const INPUT_QUIET: f64 = 1e-9;
/// Below this a sample written into the network counts as silence (-140 dBFS).
const QUIET: f64 = 1e-7;
/// Sign patterns that make the input and output rows of each channel differ (neither is a row
/// of the Hadamard matrix, so a channel does not land in a single line after one pass).
const OUT_SIGNS: [f64; LINES] = [1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0];
const IN_SIGNS: [f64; LINES] = [1.0, -1.0, -1.0, 1.0, 1.0, 1.0, -1.0, 1.0];

/// `room` (0..1) to the decay time, seconds: 0.3 s at 0, 0.85 s at 0.4, 4 s at 1, evenly on a
/// log scale.
pub fn rt60(room: f64) -> f64 {
    MIN_RT60 * (MAX_RT60 / MIN_RT60).powf(unit(room))
}

/// `damping` (0..1) to the decay time at [`DAMPING_HZ`] as a share of the decay time at low
/// frequencies: 1 at 0, 0.55 at 0.5, 0.1 at 1.
pub fn damping_ratio(damping: f64) -> f64 {
    1.0 - 0.9 * unit(damping)
}

/// `mix` (0..1) to the gains of the dry and the reverberant sound: equal power, so the loudness
/// stays when `mix` changes. 0 is (1, 0) exactly.
pub fn mix_gains(mix: f64) -> (f64, f64) {
    let m = unit(mix);
    if m >= 1.0 { (0.0, 1.0) } else { ((m * FRAC_PI_2).cos(), (m * FRAC_PI_2).sin()) }
}

fn unit(x: f64) -> f64 {
    if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }
}

/// What the reverb does, all 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// The share of the reverb: 0 dry only, 1 only the room.
    pub mix: f64,
    /// The size and decay time, see [`rt60`].
    pub room: f64,
    /// How much faster high frequencies die, see [`damping_ratio`].
    pub damping: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self { mix: DEFAULT_MIX, room: DEFAULT_ROOM, damping: DEFAULT_DAMPING }
    }
}

/// A delay line read at a gliding, fractional length.
struct Delay {
    buf: Vec<f64>,
    /// Where the next sample is written.
    pos: usize,
    /// The delay now, samples (at least 1).
    len: f64,
    target: f64,
}

impl Delay {
    fn new(capacity: usize) -> Self {
        Self { buf: vec![0.0; capacity.max(4)], pos: 0, len: 1.0, target: 1.0 }
    }

    fn longest(&self) -> f64 {
        // `as`: a buffer length.
        (self.buf.len() - 2) as f64
    }

    fn set_target(&mut self, samples: f64) {
        self.target = samples.clamp(1.0, self.longest());
    }

    fn snap(&mut self) {
        self.len = self.target;
    }

    /// The sample written `back` samples ago (1: the last one).
    fn at(&self, back: usize) -> f64 {
        let n = self.buf.len();
        let i = (self.pos + n - back % n) % n;
        self.buf.get(i).copied().unwrap_or(0.0)
    }

    /// The sample of the current length ago, interpolated between two samples while gliding.
    fn read(&self) -> f64 {
        // `as`: the whole part of a length in 1..buffer.
        let whole = self.len.floor();
        let frac = self.len - whole;
        let a = self.at(whole as usize);
        if frac == 0.0 { a } else { a + (self.at(whole as usize + 1) - a) * frac }
    }

    fn write(&mut self, x: f64) {
        if let Some(s) = self.buf.get_mut(self.pos) {
            *s = x;
        }
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
    }

    fn glide(&mut self) {
        let d = self.target - self.len;
        if d != 0.0 {
            self.len = if d.abs() <= GLIDE { self.target } else { self.len + GLIDE.copysign(d) };
        }
    }

    /// The length after `n` more glide steps.
    fn len_after(&self, n: usize) -> f64 {
        // `as`: a block length.
        let most = GLIDE * n as f64;
        self.len + (self.target - self.len).clamp(-most, most)
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// A Schroeder allpass of a fixed length.
struct Allpass {
    buf: Vec<f64>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], pos: 0 }
    }

    fn tick(&mut self, x: f64) -> f64 {
        let Some(slot) = self.buf.get_mut(self.pos) else { return x };
        let delayed = *slot;
        let w = x + DIFFUSER_GAIN * delayed;
        *slot = w;
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
        delayed - DIFFUSER_GAIN * w
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// One line of the network: its delay, the state of its damping and its coefficients, ramped
/// across a block.
struct Line {
    delay: Delay,
    damp: f64,
    gain: f64,
    gain_step: f64,
    pole: f64,
    pole_step: f64,
}

/// The coefficients of the network, as they stand at the end of a block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Coefs {
    /// Per line: the gain at low frequencies.
    gain: [f64; LINES],
    /// Per line: the pole of the damping low-pass (0: none).
    pole: [f64; LINES],
    /// The scale of the wet output, for an impulse response of the input's energy.
    wet: f64,
    /// The mix gains.
    dry_mix: f64,
    wet_mix: f64,
}

/// One reverb at one rate, for a fixed number of input and output channels.
pub struct Reverb {
    rate: u32,
    inputs: usize,
    outputs: usize,
    pre: Vec<Delay>,
    diffusers: Vec<[Allpass; 4]>,
    lines: [Line; LINES],
    /// Per input channel: its signs into the lines, scaled.
    into: Vec<[f64; LINES]>,
    /// Per output channel: its signs out of the lines, scaled.
    out_of: Vec<[f64; LINES]>,
    /// The coefficients at the end of the last block; `None` after a reset (no ramp then).
    coefs: Option<Coefs>,
    /// The params whose line lengths are set, to skip the prime search when they stay.
    room: Option<f64>,
    /// Samples in a row in which nothing above the silence threshold went in or around.
    quiet: usize,
    /// More quiet samples than this: everything stored is below the threshold.
    window: usize,
    /// The state is all zeros.
    silent: bool,
}

impl Reverb {
    /// A reverb at `rate` from `inputs` to `outputs` channels (each at least 1). Output channel
    /// `c` gets the dry sound of input `c`, of the only input when there is one, or none.
    pub fn new(rate: u32, inputs: usize, outputs: usize) -> Self {
        let rate = rate.max(1);
        let fs = f64::from(rate);
        let inputs = inputs.max(1);
        let outputs = outputs.max(1);
        let ms = |ms: f64| ms * fs / 1000.0;
        // `as`: lengths in samples, small and positive.
        let line_cap = |j: usize| ms(LINE_MS.get(j).copied().unwrap_or(62.0)) as usize + 64;
        let lines = std::array::from_fn(|j| Line {
            delay: Delay::new(line_cap(j)),
            damp: 0.0,
            gain: 0.0,
            gain_step: 0.0,
            pole: 0.0,
            pole_step: 0.0,
        });
        let pre = (0..inputs).map(|_| Delay::new(ms(PRE_MS.1) as usize + 4)).collect();
        let diffusers: Vec<[Allpass; 4]> = (0..inputs)
            .map(|c| {
                // `as`: a channel number.
                let stretch = 1.0 + 0.07 * (c % 8) as f64;
                std::array::from_fn(|k| {
                    let len = ms(DIFFUSER_MS.get(k).copied().unwrap_or(1.0) * stretch).round() as usize;
                    Allpass::new(len.max(1))
                })
            })
            .collect();
        let row = |r: usize, signs: &[f64; LINES], shift: usize, scale: f64| -> [f64; LINES] {
            std::array::from_fn(|j| {
                let walsh = if (r & j).count_ones().is_multiple_of(2) { 1.0 } else { -1.0 };
                walsh * signs.get((j + shift) % LINES).copied().unwrap_or(1.0) * scale
            })
        };
        let per = 1.0 / (LINES as f64).sqrt();
        // `as`: a channel count.
        let in_scale = per / (inputs as f64).sqrt();
        let into = (0..inputs).map(|c| row((c + 5) % LINES, &IN_SIGNS, 3 * (c / LINES), in_scale)).collect();
        let out_of = (0..outputs).map(|c| row(c % LINES, &OUT_SIGNS, 3 * (c / LINES), per)).collect();
        let window = line_cap(LINES - 1)
            + ms(PRE_MS.1) as usize
            + 4
            + DIFFUSER_MS.iter().map(|m| ms(m * 1.5) as usize + 1).sum::<usize>();
        let mut r = Self {
            rate,
            inputs,
            outputs,
            pre,
            diffusers,
            lines,
            into,
            out_of,
            coefs: None,
            room: None,
            quiet: 0,
            window,
            silent: true,
        };
        r.set_lengths(DEFAULT_ROOM);
        r.snap();
        r
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn inputs(&self) -> usize {
        self.inputs
    }

    pub fn outputs(&self) -> usize {
        self.outputs
    }

    /// The share of the reverb at the end of the last block (0 before the first).
    pub fn wet(&self) -> f64 {
        self.coefs.map_or(0.0, |c| c.wet_mix)
    }

    /// Whether no tail is left: the state is all zeros.
    pub fn is_silent(&self) -> bool {
        self.silent
    }

    /// Clears the tail; the next block starts in an empty room, without ramps.
    pub fn reset(&mut self) {
        if !self.silent {
            for d in self.pre.iter_mut().chain(self.lines.iter_mut().map(|l| &mut l.delay)) {
                d.clear();
            }
            for a in self.diffusers.iter_mut().flatten() {
                a.clear();
            }
            for l in &mut self.lines {
                l.damp = 0.0;
            }
        }
        self.silent = true;
        self.quiet = 0;
        self.coefs = None;
        self.snap();
    }

    fn snap(&mut self) {
        for d in self.pre.iter_mut().chain(self.lines.iter_mut().map(|l| &mut l.delay)) {
            d.snap();
        }
    }

    fn set_lengths(&mut self, room: f64) {
        if self.room == Some(room) {
            return;
        }
        self.room = Some(room);
        let fs = f64::from(self.rate);
        let size = MIN_SIZE + (1.0 - MIN_SIZE) * room;
        for (d, ms) in self.lines.iter_mut().map(|l| &mut l.delay).zip(LINE_MS) {
            // `as`: a length in samples, small and positive.
            let n = (ms * size * fs / 1000.0).round().max(2.0) as u64;
            // `as`: back to a length, well below 2^52.
            d.set_target(next_prime(n) as f64);
        }
        let pre = (PRE_MS.0 + (PRE_MS.1 - PRE_MS.0) * room) * fs / 1000.0;
        for d in &mut self.pre {
            d.set_target(pre.round().max(1.0));
        }
    }

    /// The coefficients at the end of a block of `frames` samples.
    fn coefs_for(&self, p: &Params, frames: usize) -> Coefs {
        let fs = f64::from(self.rate);
        let rt = rt60(p.room);
        let ratio = damping_ratio(p.damping);
        // The damping is set at 8 kHz, or below 0.45 of the rate at low rates.
        let w = std::f64::consts::TAU * DAMPING_HZ.min(0.45 * fs) / fs;
        let mut c = Coefs::default();
        let mut sum_sq = 0.0;
        for ((g, pole), d) in c.gain.iter_mut().zip(c.pole.iter_mut()).zip(&self.lines) {
            let len = d.delay.len_after(frames);
            *g = 10f64.powf(-3.0 * len / (fs * rt));
            let high = 10f64.powf(-3.0 * len / (fs * rt * ratio));
            *pole = one_pole_for(high / *g, w.cos());
            sum_sq += *g * *g;
        }
        // `as`: the number of lines.
        let mean_sq = sum_sq / LINES as f64;
        // The impulse response of one input into one output: (1 / N) / (1 - g^2) of energy.
        c.wet = (LINES as f64 * (1.0 - mean_sq)).max(0.0).sqrt();
        (c.dry_mix, c.wet_mix) = mix_gains(p.mix);
        c
    }

    /// Plays one block: `input` holds one slice per input channel, `output` one per output
    /// channel, each as long as the block (shorter ones are read as silence and written only
    /// as far as they go). Non-finite input samples count as silence.
    pub fn process(&mut self, input: &[Vec<f64>], output: &mut [Vec<f64>], p: &Params) {
        let frames = input.iter().chain(output.iter()).map(Vec::len).max().unwrap_or(0);
        let room = unit(p.room);
        self.set_lengths(room);
        if self.coefs.is_none() {
            // A fresh start: the lengths need not glide.
            self.snap();
        }
        let end = self.coefs_for(p, frames);
        // After a reset the mix ramps up from the dry sound, so turning the reverb on does not
        // step the dry level.
        let start = self.coefs.unwrap_or(Coefs { dry_mix: 1.0, wet_mix: 0.0, ..end });
        self.coefs = Some(end);
        let sample = |c: usize, k: usize| -> f64 {
            let v = input.get(c).and_then(|x| x.get(k)).copied().unwrap_or(0.0);
            if v.is_finite() { v } else { 0.0 }
        };
        let dry_of = |o: usize, k: usize| -> f64 {
            if o < input.len() {
                sample(o, k)
            } else if input.len() == 1 {
                sample(0, k)
            } else {
                0.0
            }
        };
        // `as`: a block length.
        let n = frames.max(1) as f64;
        let quiet_input = input.iter().flatten().all(|v| !v.is_finite() || v.abs() < INPUT_QUIET);
        if self.silent && quiet_input {
            // An empty room and nothing coming in: only the dry sound.
            self.snap();
            for (o, out) in output.iter_mut().enumerate() {
                for (k, v) in out.iter_mut().enumerate() {
                    // `as`: positions in a block.
                    let t = (k + 1) as f64 / n;
                    *v = dry_of(o, k) * lerp(start.dry_mix, end.dry_mix, t);
                }
            }
            return;
        }
        self.silent = false;
        for (line, ((g0, g1), (p0, p1))) in
            self.lines.iter_mut().zip(start.gain.iter().zip(end.gain).zip(start.pole.iter().zip(end.pole)))
        {
            line.gain = *g0;
            line.gain_step = (g1 - g0) / n;
            line.pole = *p0;
            line.pole_step = (p1 - p0) / n;
        }
        let mut taps = [0.0; LINES];
        for k in 0..frames {
            // `as`: positions in a block.
            let t = (k + 1) as f64 / n;
            let mut feed = [0.0; LINES];
            let mut loud = false;
            let mut written = 0.0f64;
            for (c, ((pre, diffusers), into)) in
                self.pre.iter_mut().zip(self.diffusers.iter_mut()).zip(&self.into).enumerate()
            {
                let mut x = sample(c, k);
                if x.abs() < INPUT_QUIET {
                    x = 0.0;
                } else {
                    loud = true;
                }
                let mut u = pre.read();
                pre.write(x);
                pre.glide();
                for a in diffusers.iter_mut() {
                    u = a.tick(u);
                    written = written.max(u.abs());
                }
                for (f, b) in feed.iter_mut().zip(into) {
                    *f += b * u;
                }
            }
            let mut v = [0.0; LINES];
            for ((line, out), vj) in self.lines.iter_mut().zip(taps.iter_mut()).zip(v.iter_mut()) {
                *out = line.delay.read();
                line.gain += line.gain_step;
                line.pole += line.pole_step;
                line.damp = (1.0 - line.pole) * *out + line.pole * line.damp;
                *vj = line.gain * line.damp;
            }
            let mixed = hadamard(v);
            for ((line, m), f) in self.lines.iter_mut().zip(mixed).zip(feed) {
                let w = m + f;
                written = written.max(w.abs());
                line.delay.write(w);
                line.delay.glide();
            }
            let wet_scale = lerp(start.wet, end.wet, t) * lerp(start.wet_mix, end.wet_mix, t);
            let dry = lerp(start.dry_mix, end.dry_mix, t);
            for (o, (out, row)) in output.iter_mut().zip(&self.out_of).enumerate() {
                let wet: f64 = row.iter().zip(&taps).map(|(r, s)| r * s).sum();
                if let Some(slot) = out.get_mut(k) {
                    *slot = dry_of(o, k) * dry + wet * wet_scale;
                }
            }
            if loud || written >= QUIET {
                self.quiet = 0;
            } else {
                self.quiet += 1;
            }
        }
        if self.quiet > self.window {
            // Everything stored is below -140 dBFS: an empty room from here on.
            let coefs = self.coefs;
            self.reset();
            self.coefs = coefs;
        }
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The Hadamard matrix of order 8, scaled to be orthogonal, times `v`.
fn hadamard(v: [f64; LINES]) -> [f64; LINES] {
    let [a0, a1, a2, a3, a4, a5, a6, a7] = v;
    let (b0, b1, b2, b3) = (a0 + a1, a0 - a1, a2 + a3, a2 - a3);
    let (b4, b5, b6, b7) = (a4 + a5, a4 - a5, a6 + a7, a6 - a7);
    let (c0, c1, c2, c3) = (b0 + b2, b1 + b3, b0 - b2, b1 - b3);
    let (c4, c5, c6, c7) = (b4 + b6, b5 + b7, b4 - b6, b5 - b7);
    let s = std::f64::consts::FRAC_1_SQRT_2 * 0.5;
    [
        (c0 + c4) * s,
        (c1 + c5) * s,
        (c2 + c6) * s,
        (c3 + c7) * s,
        (c0 - c4) * s,
        (c1 - c5) * s,
        (c2 - c6) * s,
        (c3 - c7) * s,
    ]
}

/// The pole `p` of `(1 - p) / (1 - p z^-1)` whose magnitude at the angle with cosine `cos_w` is
/// `r` (0 < r < 1); 0 when `r` is 1 or more.
fn one_pole_for(r: f64, cos_w: f64) -> f64 {
    if !r.is_finite() || r >= 1.0 {
        return 0.0;
    }
    let r2 = r.max(1e-6).powi(2);
    // (1 - p)^2 = r^2 (1 - 2 p cos w + p^2), the root inside the unit circle.
    let a = 1.0 - r2;
    let b = 1.0 - r2 * cos_w;
    let disc = (b * b - a * a).max(0.0);
    ((b - disc.sqrt()) / a).clamp(0.0, 0.999)
}

/// The smallest prime at least `n` (small `n` only).
fn next_prime(n: u64) -> u64 {
    let mut k = n.max(2);
    while !is_prime(k) {
        k += 1;
    }
    k
}

fn is_prime(n: u64) -> bool {
    if n < 4 {
        return n >= 2;
    }
    if n.is_multiple_of(2) {
        return false;
    }
    let mut d = 3;
    while d * d <= n {
        if n.is_multiple_of(d) {
            return false;
        }
        d += 2;
    }
    true
}
