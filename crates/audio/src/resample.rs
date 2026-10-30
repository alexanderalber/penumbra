//! Sample-rate conversion: a windowed-sinc [`Kernel`] and a streaming [`Resampler`] on it.
//!
//! - **Kernel:** a sinc cut off at half the lower of the two rates, under a Kaiser window
//!   (beta [`KAISER_BETA`], about 90 dB stopband). [`BASE_TAPS`] taps when the rate goes up;
//!   when it goes down the kernel is stretched by the ratio (96 to 48 kHz: 128 taps), so the
//!   transition band stays the same share of the lower rate, up to [`MAX_TAPS`]. It is tabled
//!   at [`PHASES`] positions between two source samples and interpolated linearly in between.
//!   The response is flat within 0.01 dB to 0.4535 of the lower rate (20 kHz at 44.1 kHz) and
//!   down by at least 80 dB from 0.5465 of it, so nothing above the band aliases into it.
//!   Every phase sums to 1, so a constant stays exactly constant.
//! - **Resampler:** per channel a short history of source samples; one position shared by all
//!   channels. Blocks of any length join without a seam: the output of many small blocks is
//!   the same, sample for sample, as that of one large block. The output lags the input by
//!   half the kernel ([`Resampler::delay_seconds`]); from the first block on, every input
//!   sample yields `ratio` output samples, so the counts never drift.
//! - **Drift:** [`Resampler::set_ratio`] glides to a new ratio over [`GLIDE_SECONDS`], so a
//!   loop following a device clock changes the pitch smoothly. The ratio may move at most
//!   [`MAX_DRIFT`] from the rates the kernel was made for.

/// Taps of the kernel when the rate goes up or stays.
pub const BASE_TAPS: usize = 64;
/// Most taps of a kernel (a fall in rate of 16 to 1 still has its full length).
pub const MAX_TAPS: usize = 1024;
/// Kernel positions tabled between two source samples.
pub const PHASES: usize = 256;
/// Kaiser window shape: about 90 dB stopband.
pub const KAISER_BETA: f64 = 9.0;
/// Seconds of output over which [`Resampler::set_ratio`] glides to the new ratio.
pub const GLIDE_SECONDS: f64 = 0.05;
/// How far the ratio may move from the one the kernel was made for (5 %).
pub const MAX_DRIFT: f64 = 0.05;
/// Rates accepted, samples per second.
pub const MIN_RATE: f64 = 1_000.0;
pub const MAX_RATE: f64 = 768_000.0;
/// Most channels of a [`Resampler`].
pub const MAX_CHANNELS: usize = 64;
/// Input samples beyond this are clipped to it; non-numbers count as 0.
pub const CLIP: f64 = 1e6;

/// The interpolation kernel for one pair of rates.
#[derive(Clone, Debug)]
pub struct Kernel {
    taps: usize,
    /// `PHASES + 1` rows of `taps` coefficients; the last row is the play point one source
    /// sample on, so the last phase interpolates towards it.
    rows: Vec<f32>,
}

impl Kernel {
    /// The kernel for sound at `from` samples per second played at `to`. Rates that are not
    /// positive numbers count as equal.
    pub fn new(from: f64, to: f64) -> Self {
        let ratio =
            if from.is_finite() && to.is_finite() && from > 0.0 && to > 0.0 { to / from } else { 1.0 };
        let scale = ratio.min(1.0);
        // `as`: a positive count of taps, capped before the conversion.
        let wanted = (BASE_TAPS as f64 / scale).ceil().min(MAX_TAPS as f64) as usize;
        let taps = wanted.div_ceil(8).saturating_mul(8).clamp(8, MAX_TAPS);
        // Cutoff in cycles per source sample: half the lower rate.
        let fc = 0.5 * scale;
        let half = (taps / 2) as f64;
        let i0b = bessel_i0(KAISER_BETA);
        let mut rows = vec![0.0f32; (PHASES + 1) * taps];
        for (p, row) in rows.chunks_exact_mut(taps).enumerate() {
            let frac = p as f64 / PHASES as f64;
            let mut values = Vec::with_capacity(taps);
            for k in 0..taps {
                // Distance of tap `k` from the play point at `taps / 2 - 1 + frac`.
                let t = k as f64 - (half - 1.0) - frac;
                let x = t / half;
                let w = if x.abs() < 1.0 { bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / i0b } else { 0.0 };
                values.push(2.0 * fc * sinc(2.0 * fc * t) * w);
            }
            let sum: f64 = values.iter().sum();
            let norm = if sum.abs() > 1e-9 { 1.0 / sum } else { 1.0 };
            for (h, v) in row.iter_mut().zip(values) {
                // `as`: designed in f64, stored as f32.
                *h = (v * norm) as f32;
            }
        }
        Self { taps, rows }
    }

    /// Source samples one output sample reads.
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// Source samples from the start of the window to the play point: the window over source
    /// samples `i .. i + taps` plays the position `i + delay() - 1 + frac`.
    pub fn delay(&self) -> usize {
        self.taps / 2
    }

    /// The coefficients for the play point `frac` (0 to 1) past tap `delay() - 1`, into `out`
    /// (at least [`Kernel::taps`] long; extra entries are left alone).
    pub fn coefficients(&self, frac: f64, out: &mut [f32]) {
        let p = frac.clamp(0.0, 1.0) * PHASES as f64;
        // `as`: a phase index in 0..PHASES.
        let i = (p.floor() as usize).min(PHASES - 1);
        // `as`: the fraction between two phases.
        let t = (p - i as f64) as f32;
        let n = self.taps;
        let a = self.rows.get(i * n..(i + 1) * n).unwrap_or(&[]);
        let b = self.rows.get((i + 1) * n..(i + 2) * n).unwrap_or(&[]);
        for ((c, x), y) in out.iter_mut().zip(a).zip(b) {
            *c = x + (y - x) * t;
        }
    }
}

/// The sum of `h[k] * x[k]` (as far as both reach), in eight lanes for the vector unit.
pub fn dot(h: &[f32], x: &[f32]) -> f32 {
    let (hc, hr) = h.as_chunks::<8>();
    let (xc, xr) = x.as_chunks::<8>();
    let mut acc = [0.0f32; 8];
    for (a, b) in hc.iter().zip(xc) {
        for ((s, p), q) in acc.iter_mut().zip(a).zip(b) {
            *s += p * q;
        }
    }
    let mut sum: f32 = acc.iter().sum();
    if hc.len() == xc.len() {
        for (p, q) in hr.iter().zip(xr) {
            sum += p * q;
        }
    }
    sum
}

/// A streaming sample-rate converter for a fixed number of channels.
#[derive(Clone, Debug)]
pub struct Resampler {
    from: f64,
    to: f64,
    kernel: Kernel,
    /// Per channel: source samples from the start of the next window on.
    history: Vec<Vec<f32>>,
    /// The play point past the window's start, `delay() - 1 + frac` source samples in.
    frac: f64,
    /// Source samples per output sample: now, the goal of a glide, and the kernel's.
    step: f64,
    goal: f64,
    nominal: f64,
    /// Change of `step` per output sample while gliding, and the samples left of the glide.
    slope: f64,
    glide_left: usize,
    /// Source samples still to skip before the next window (a step longer than the history).
    skip: usize,
    coeffs: Vec<f32>,
}

impl Resampler {
    /// Converts `channels` channels from `from` to `to` samples per second. Errors for rates
    /// outside [`MIN_RATE`]..[`MAX_RATE`] and for 0 or more than [`MAX_CHANNELS`] channels.
    pub fn new(from: f64, to: f64, channels: usize) -> Result<Self, String> {
        for r in [from, to] {
            if !(r.is_finite() && (MIN_RATE..=MAX_RATE).contains(&r)) {
                return Err(format!(
                    "a sample rate of {r} Hz cannot be converted (from {MIN_RATE} to {MAX_RATE})"
                ));
            }
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(format!("{channels} channels cannot be converted (from 1 to {MAX_CHANNELS})"));
        }
        let kernel = Kernel::new(from, to);
        let taps = kernel.taps();
        let step = from / to;
        Ok(Self {
            from,
            to,
            history: vec![vec![0.0; taps - 1]; channels],
            kernel,
            frac: 0.0,
            step,
            goal: step,
            nominal: step,
            slope: 0.0,
            glide_left: 0,
            skip: 0,
            coeffs: vec![0.0; taps],
        })
    }

    pub fn from_rate(&self) -> f64 {
        self.from
    }

    pub fn to_rate(&self) -> f64 {
        self.to
    }

    pub fn channels(&self) -> usize {
        self.history.len()
    }

    pub fn taps(&self) -> usize {
        self.kernel.taps()
    }

    /// The group delay: the output lags the input by this many seconds.
    pub fn delay_seconds(&self) -> f64 {
        self.kernel.delay() as f64 / self.from
    }

    /// Output samples per input sample now (the kernel's ratio `to / from` plus any drift).
    pub fn ratio(&self) -> f64 {
        1.0 / self.step
    }

    /// The ratio the kernel was made for, `to / from`.
    pub fn nominal_ratio(&self) -> f64 {
        1.0 / self.nominal
    }

    /// Glides to `ratio` output samples per input sample over [`GLIDE_SECONDS`]. Clamped to
    /// within [`MAX_DRIFT`] of the nominal ratio; a non-number is ignored.
    pub fn set_ratio(&mut self, ratio: f64) {
        if !(ratio.is_finite() && ratio > 0.0) {
            return;
        }
        let goal = (1.0 / ratio).clamp(self.nominal / (1.0 + MAX_DRIFT), self.nominal * (1.0 + MAX_DRIFT));
        if goal == self.goal {
            return;
        }
        self.goal = goal;
        // `as`: a positive count of samples.
        let n = (GLIDE_SECONDS * self.to).ceil().max(1.0) as usize;
        self.slope = (goal - self.step) / n as f64;
        self.glide_left = n;
    }

    /// Starts over: as new, at the nominal ratio.
    pub fn reset(&mut self) {
        let taps = self.kernel.taps();
        for h in &mut self.history {
            h.clear();
            h.resize(taps - 1, 0.0);
        }
        self.frac = 0.0;
        self.step = self.nominal;
        self.goal = self.nominal;
        self.glide_left = 0;
        self.skip = 0;
    }

    /// Converts one block, one slice per channel, all of one length, and appends the result to
    /// `out` (one `Vec` per channel). Returns the samples appended per channel. Errors when
    /// the channel counts or the lengths of the slices differ.
    pub fn process(&mut self, input: &[&[f64]], out: &mut [Vec<f64>]) -> Result<usize, String> {
        let channels = self.history.len();
        if input.len() != channels || out.len() != channels {
            return Err(format!(
                "{} channels in and {} out for a converter of {channels}",
                input.len(),
                out.len()
            ));
        }
        let frames = input.first().map_or(0, |c| c.len());
        if input.iter().any(|c| c.len() != frames) {
            return Err("the channels of a block differ in length".into());
        }
        for (h, x) in self.history.iter_mut().zip(input) {
            // `as`: clipped first, so the narrowing to f32 stays finite.
            h.extend(x.iter().map(|v| if v.is_finite() { v.clamp(-CLIP, CLIP) as f32 } else { 0.0 }));
        }
        if self.skip > 0 {
            let n = self.skip.min(self.history.first().map_or(0, Vec::len));
            for h in &mut self.history {
                h.drain(..n);
            }
            self.skip -= n;
        }
        let taps = self.kernel.taps();
        let len = self.history.first().map_or(0, Vec::len);
        let mut start = 0usize;
        let mut made = 0usize;
        while start + taps <= len {
            self.kernel.coefficients(self.frac, &mut self.coeffs);
            for (h, o) in self.history.iter().zip(out.iter_mut()) {
                let window = h.get(start..start + taps).unwrap_or(&[]);
                o.push(f64::from(dot(&self.coeffs, window)));
            }
            made += 1;
            if self.glide_left > 0 {
                self.glide_left -= 1;
                self.step = if self.glide_left == 0 { self.goal } else { self.step + self.slope };
            }
            self.frac += self.step;
            let advance = self.frac.floor();
            self.frac -= advance;
            // `as`: a small non-negative whole number.
            start += advance as usize;
        }
        let keep_from = start.min(len);
        self.skip = start - keep_from;
        for h in &mut self.history {
            h.drain(..keep_from);
        }
        Ok(made)
    }
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
