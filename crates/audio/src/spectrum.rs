//! A spectrum of the newest samples in bands, and the band code the analysis shares.
//!
//! [`Spectrum`] keeps the newest [`Spectrum::size`] samples and transforms them on request:
//! Hann-windowed, one transform per call, so a call per show frame costs one FFT however long
//! the block was. A band's value is the amplitude of a sine holding the band's power: a sine of
//! amplitude 0.1 inside a band reads 0.1 there.
//!
//! **Transform size** ([`fft_size`]): long enough that the narrowest band spans an FFT bin,
//! between about 11 ms and 85 ms of sound (512 and 4096 samples at 44.1 and 48 kHz). Longer
//! transforms would resolve low log-spaced bands better but smear hits over a longer time. A
//! band narrower than a bin takes the bin nearest its middle, so neighbouring low bands may read
//! the same value.

use crate::fft::{Complex, Fft};

/// Equivalent noise bandwidth of the Hann window, in bins.
pub const HANN_ENBW: f32 = 1.5;
/// Samples beyond this are clipped to it; non-finite ones count as 0.
const CLIP: f32 = 16.0;

/// A Hann window of `size` points.
pub fn hann(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            // `as`: an index to an angle, then to f32 for the samples.
            let x = std::f64::consts::TAU * i as f64 / size.max(1) as f64;
            (0.5 - 0.5 * x.cos()) as f32
        })
        .collect()
}

/// Windows the ring `ring` (oldest sample at `pos`), transforms it and writes the bins'
/// magnitudes to `magnitudes` (`size / 2 + 1` of them), scaled so a full-scale sine reads about
/// 1 in its bin.
pub fn magnitudes(
    fft: &Fft,
    ring: &[f32],
    pos: usize,
    window: &[f32],
    scratch: &mut [Complex],
    magnitudes: &mut [f32],
) {
    let size = ring.len();
    for ((c, x), w) in scratch.iter_mut().zip(ring.iter().cycle().skip(pos).take(size)).zip(window) {
        *c = Complex { re: x * w, im: 0.0 };
    }
    fft.forward(scratch);
    // The window sums to size / 2, and half the sine's energy sits in the negative bins.
    // `as`: a size to a scale.
    let scale = 4.0 / size.max(1) as f32;
    for (m, c) in magnitudes.iter_mut().zip(scratch.iter()) {
        *m = c.norm() * scale;
    }
}

/// The mean-square power a bin of magnitude `m` stands for.
pub fn bin_power(m: f32) -> f64 {
    f64::from(m * m / HANN_ENBW) / 2.0
}

/// The first and last bin of each of `bands` bands from `low` to `high` Hz, log-spaced (else
/// evenly), for bins `bin_hz` apart up to `top` (the Nyquist bin). A band narrower than a bin
/// takes the bin nearest its middle; one starting above `top` has no bins (an empty range).
pub fn band_bins(
    bands: usize,
    low: f64,
    high: f64,
    log: bool,
    bin_hz: f64,
    top: usize,
) -> Vec<(usize, usize)> {
    let bands = bands.max(1);
    let low = if low.is_finite() { low.max(1e-3) } else { 1.0 };
    let high = if high.is_finite() { high.max(low) } else { low };
    let ratio = (high / low).max(1.0);
    // `as`: band indices to shares.
    let edge = |i: usize| {
        let t = i as f64 / bands as f64;
        if log { low * ratio.powf(t) } else { low + (high - low) * t }
    };
    let bin_hz = if bin_hz.is_finite() && bin_hz > 0.0 { bin_hz } else { 1.0 };
    (0..bands)
        .map(|i| {
            let (a, b) = (edge(i) / bin_hz, edge(i + 1) / bin_hz);
            if a > top as f64 {
                return (1, 0);
            }
            // `as`: non-negative bin positions, rounded towards the band's inside.
            let lo = (a.ceil() as usize).min(top);
            let hi = (b.floor() as usize).min(top);
            if hi >= lo {
                (lo, hi)
            } else {
                let mid = (((a + b) / 2.0).round() as usize).min(top);
                (mid, mid)
            }
        })
        .collect()
}

/// The summed power of each band's bins, into `out`.
pub fn band_power(magnitudes: &[f32], edges: &[(usize, usize)], out: &mut [f64]) {
    for (o, &(lo, hi)) in out.iter_mut().zip(edges) {
        let bins = magnitudes.get(lo..=hi).unwrap_or(&[]);
        *o = bins.iter().map(|m| bin_power(*m)).sum();
    }
}

/// Where the bands lie.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    pub count: usize,
    /// Lower edge of the first band, Hz.
    pub low: f64,
    /// Upper edge of the last band, Hz.
    pub high: f64,
    /// Log-spaced (each band the same share of an octave), else evenly spaced in Hz.
    pub log: bool,
}

impl Bands {
    /// The width of the narrowest band, Hz.
    fn narrowest(&self) -> f64 {
        let n = self.count.max(1) as f64;
        if self.log {
            self.low * ((self.high / self.low).max(1.0).powf(1.0 / n) - 1.0)
        } else {
            (self.high - self.low) / n
        }
    }
}

/// The transform size for `bands` at `rate` Hz: the smallest power of two whose bins are no
/// wider than the narrowest band, between about 11 ms and 85 ms of sound.
pub fn fft_size(rate: f64, bands: &Bands) -> usize {
    let rate = if rate.is_finite() && rate > 0.0 { rate } else { 48_000.0 };
    // `as`: rounded exponents in 6..=13 and 9..=16.
    let shortest = ((rate / 93.75).log2().round().clamp(6.0, 13.0)) as u32;
    let longest = shortest + 3;
    let narrowest = bands.narrowest();
    let want = if narrowest.is_finite() && narrowest > 0.0 { (rate / narrowest).log2().ceil() } else { 99.0 };
    let bits = want.clamp(f64::from(shortest), f64::from(longest)) as u32;
    1usize << bits
}

/// The newest samples of a signal and their spectrum in bands.
pub struct Spectrum {
    rate: f64,
    fft: Fft,
    window: Vec<f32>,
    /// The newest `size` samples, a ring whose oldest sample is at `pos`.
    ring: Vec<f32>,
    pos: usize,
    scratch: Vec<Complex>,
    magnitudes: Vec<f32>,
    /// Band edges in bins for the bands they were made for.
    edges: Option<(Bands, Vec<(usize, usize)>)>,
    power: Vec<f64>,
}

impl Spectrum {
    /// A spectrum over `size` samples (a power of two, see [`fft_size`]) of a signal at `rate`.
    pub fn new(rate: f64, size: usize) -> Result<Spectrum, String> {
        if !(rate.is_finite() && rate > 0.0) {
            return Err(format!("a sample rate of {rate} Hz is not audio"));
        }
        let fft = Fft::new(size)?;
        Ok(Spectrum {
            rate,
            fft,
            window: hann(size),
            ring: vec![0.0; size],
            pos: 0,
            scratch: vec![Complex::default(); size],
            magnitudes: vec![0.0; size / 2 + 1],
            edges: None,
            power: Vec::new(),
        })
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Samples transformed.
    pub fn size(&self) -> usize {
        self.ring.len()
    }

    /// Takes the next samples; only the newest [`Spectrum::size`] of them count.
    pub fn push(&mut self, samples: &[f32]) {
        let size = self.ring.len();
        if size == 0 {
            return;
        }
        let skip = samples.len().saturating_sub(size);
        for &x in samples.iter().skip(skip) {
            let x = if x.is_finite() { x.clamp(-CLIP, CLIP) } else { 0.0 };
            if let Some(slot) = self.ring.get_mut(self.pos) {
                *slot = x;
            }
            self.pos = (self.pos + 1) % size;
        }
    }

    /// The amplitude in each band of the newest samples, low to high, into `out` (resized to
    /// the bands). Bands above the Nyquist frequency read 0.
    pub fn bands(&mut self, bands: &Bands, out: &mut Vec<f64>) {
        let size = self.ring.len();
        magnitudes(&self.fft, &self.ring, self.pos, &self.window, &mut self.scratch, &mut self.magnitudes);
        if self.edges.as_ref().is_none_or(|(b, _)| b != bands) {
            let bin_hz = self.rate / size.max(1) as f64;
            let edges = band_bins(bands.count, bands.low, bands.high, bands.log, bin_hz, size / 2);
            self.edges = Some((*bands, edges));
        }
        let Some((_, edges)) = &self.edges else { return };
        self.power.resize(edges.len(), 0.0);
        band_power(&self.magnitudes, edges, &mut self.power);
        out.clear();
        out.extend(self.power.iter().map(|p| (2.0 * p).sqrt()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, amp: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (amp * (std::f64::consts::TAU * hz * i as f64 / rate).sin()) as f32).collect()
    }

    #[test]
    fn a_sine_reads_its_amplitude_in_its_band() {
        let b = Bands { count: 32, low: 30.0, high: 16_000.0, log: true };
        let size = fft_size(48_000.0, &b);
        assert_eq!(size, 4096);
        let mut s = Spectrum::new(48_000.0, size).unwrap();
        s.push(&sine(3000.0, 0.1, 48_000.0, 8192));
        let mut out = Vec::new();
        s.bands(&b, &mut out);
        let (i, v) = out.iter().copied().enumerate().fold((0, 0.0), |m, c| if c.1 > m.1 { c } else { m });
        let want = (32.0 * (3000.0f64 / 30.0).ln() / (16_000.0f64 / 30.0).ln()).floor() as usize;
        assert_eq!(i, want);
        assert!((20.0 * (v / 0.1).log10()).abs() < 0.5, "{v}");
    }

    #[test]
    fn sizes_follow_bands_and_rate() {
        let lin = Bands { count: 32, low: 30.0, high: 16_000.0, log: false };
        assert_eq!(fft_size(48_000.0, &lin), 512);
        assert_eq!(fft_size(96_000.0, &lin), 1024);
        let many = Bands { count: 512, low: 30.0, high: 16_000.0, log: true };
        assert_eq!(fft_size(48_000.0, &many), 4096);
        assert_eq!(fft_size(f64::NAN, &many), 4096);
        let odd = Bands { count: 4, low: 1000.0, high: 1000.0, log: true };
        assert_eq!(fft_size(48_000.0, &odd), 4096);
    }

    #[test]
    fn hostile_samples_stay_finite() {
        let b = Bands { count: 8, low: 30.0, high: 16_000.0, log: true };
        let mut s = Spectrum::new(48_000.0, 512).unwrap();
        s.push(&[f32::NAN, f32::INFINITY, 1e30, -1e30, 0.5]);
        let mut out = Vec::new();
        s.bands(&b, &mut out);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(Spectrum::new(0.0, 512).is_err());
        assert!(Spectrum::new(48_000.0, 500).is_err());
    }
}
