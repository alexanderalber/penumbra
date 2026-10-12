//! A small radix-2 FFT, enough for analysis frames of a few thousand samples.

use std::f64::consts::TAU;

/// Largest transform size.
pub const MAX_SIZE: usize = 1 << 16;

/// One complex number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    pub fn norm(self) -> f32 {
        self.re.hypot(self.im)
    }
}

/// A forward transform of one size, with its twiddles and bit-reversal table.
pub struct Fft {
    size: usize,
    /// `exp(-2 pi i k / size)` for `k < size / 2`.
    twiddles: Vec<Complex>,
    /// Pairs to swap for the bit-reversed order.
    swaps: Vec<(usize, usize)>,
}

impl Fft {
    /// A transform of `size` points: a power of two from 2 to [`MAX_SIZE`].
    pub fn new(size: usize) -> Result<Fft, String> {
        if !size.is_power_of_two() || !(2..=MAX_SIZE).contains(&size) {
            return Err(format!("FFT size {size} is not a power of two from 2 to {MAX_SIZE}"));
        }
        let twiddles = (0..size / 2)
            .map(|k| {
                // `as` is the conversion wanted: index to angle, then to f32 for the samples.
                let a = -TAU * k as f64 / size as f64;
                Complex { re: a.cos() as f32, im: a.sin() as f32 }
            })
            .collect();
        let bits = size.trailing_zeros();
        let swaps = (0..size)
            .filter_map(|i| {
                let j = i.reverse_bits() >> (usize::BITS - bits);
                (j > i).then_some((i, j))
            })
            .collect();
        Ok(Fft { size, twiddles, swaps })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Transforms `data` in place; does nothing when its length is not the size.
    pub fn forward(&self, data: &mut [Complex]) {
        if data.len() != self.size {
            return;
        }
        for &(i, j) in &self.swaps {
            // Both below `size` by construction.
            data.swap(i, j);
        }
        let mut len = 2;
        while len <= self.size {
            let step = self.size / len;
            for chunk in data.chunks_exact_mut(len) {
                let (a, b) = chunk.split_at_mut(len / 2);
                for (k, (x, y)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
                    let w = self.twiddles.get(k * step).copied().unwrap_or_default();
                    let t = Complex { re: w.re * y.re - w.im * y.im, im: w.re * y.im + w.im * y.re };
                    *y = Complex { re: x.re - t.re, im: x.im - t.im };
                    *x = Complex { re: x.re + t.re, im: x.im + t.im };
                }
            }
            len *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[Complex]) -> Vec<Complex> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut s = (0.0f64, 0.0f64);
                for (t, v) in x.iter().enumerate() {
                    let a = -TAU * (k * t) as f64 / n as f64;
                    s.0 += v.re as f64 * a.cos() - v.im as f64 * a.sin();
                    s.1 += v.re as f64 * a.sin() + v.im as f64 * a.cos();
                }
                Complex { re: s.0 as f32, im: s.1 as f32 }
            })
            .collect()
    }

    #[test]
    fn matches_the_dft() {
        for size in [2, 4, 8, 64, 256] {
            let fft = Fft::new(size).unwrap();
            let x: Vec<Complex> = (0..size)
                .map(|i| Complex { re: ((i * 7919) % 13) as f32 - 6.0, im: ((i * 31) % 5) as f32 - 2.0 })
                .collect();
            let mut y = x.clone();
            fft.forward(&mut y);
            for (a, b) in y.iter().zip(dft(&x)) {
                assert!((a.re - b.re).abs() < 1e-2 && (a.im - b.im).abs() < 1e-2, "size {size}");
            }
        }
    }

    #[test]
    fn refuses_bad_sizes() {
        assert!(Fft::new(0).is_err());
        assert!(Fft::new(3).is_err());
        assert!(Fft::new(MAX_SIZE * 2).is_err());
        let fft = Fft::new(8).unwrap();
        let mut short = vec![Complex::default(); 4];
        fft.forward(&mut short);
    }
}
