//! Element-wise functions (exp, erf, GELU, sigmoid) and the softmax over rows.
//!
//! `exp` and `erf` are own approximations without branches, so the compiler can vectorise the
//! loops over a slice; the standard library calls the C runtime once per value.

use rayon::prelude::*;

use crate::{Result, Tensor, shape_err};

/// Values per parallel task of the element-wise ops.
const PAR_CHUNK: usize = 1 << 15;

const LOG2E: f32 = std::f32::consts::LOG2_E;
/// ln 2 split into a part with few mantissa bits and the rest, for an exact range reduction.
const LN2_HI: f32 = 0.693_359_4;
const LN2_LO: f32 = -2.121_944_4e-4;
/// Adding this rounds a float below 2^22 to the nearest integer and leaves it in the low bits.
const ROUND_MAGIC: f32 = 12_582_912.0;
/// The largest input whose power of two is still a normal f32 (2^127).
const EXP_MAX: f32 = 88.376_26;
/// The smallest input whose power of two is still a normal f32 (2^-126).
const EXP_MIN: f32 = -87.336_54;

/// e^x with a relative error of about 2e-7. Inputs beyond about ±88 are clamped, so the result
/// stays finite: at most 2.4e38, at least 1.2e-38. NaN gives an unspecified finite value.
#[inline(always)]
pub fn exp(x: f32) -> f32 {
    let x = x.clamp(EXP_MIN, EXP_MAX);
    // x = n ln 2 + r with |r| <= ln 2 / 2.
    let t = x * LOG2E + ROUND_MAGIC;
    let n = t - ROUND_MAGIC;
    let r = x - n * LN2_HI - n * LN2_LO;
    // The polynomial of the Cephes library's expf.
    let p = 1.987_569_1e-4;
    let p = p * r + 1.398_199_9e-3;
    let p = p * r + 8.333_452e-3;
    let p = p * r + 4.166_579_6e-2;
    let p = p * r + 1.666_666_5e-1;
    let p = p * r + 0.5;
    let e = p * r * r + r + 1.0;
    // The integer n sits in the low mantissa bits of t; 2^n is built from its exponent field.
    let ni = (t.to_bits() as i32).wrapping_sub(ROUND_MAGIC.to_bits() as i32);
    let pow2 = f32::from_bits((ni.wrapping_add(127) as u32) << 23);
    e * pow2
}

/// The error function, with an absolute error of about 1e-7: a Taylor series below 0.5 and
/// the Chebyshev fit of erfc from Numerical Recipes (relative error below 1.2e-7) above.
/// Both are evaluated and one is picked, so a loop over a slice stays branch-free.
#[inline(always)]
pub fn erf(x: f32) -> f32 {
    let a = x.abs();
    // 2 / sqrt(pi) * (a - a^3 / 3 + a^5 / 10 - ...), terms to a^13.
    let s = a * a;
    let p = 1.0 / 9360.0;
    let p = p * s - 1.0 / 1320.0;
    let p = p * s + 1.0 / 216.0;
    let p = p * s - 1.0 / 42.0;
    let p = p * s + 1.0 / 10.0;
    let p = p * s - 1.0 / 3.0;
    let p = p * s + 1.0;
    let small = std::f32::consts::FRAC_2_SQRT_PI * a * p;
    let t = 1.0 / (1.0 + 0.5 * a);
    let q = 0.170_872_77;
    let q = q * t - 0.822_152_23;
    let q = q * t + 1.488_515_9;
    let q = q * t - 1.135_204;
    let q = q * t + 0.278_868_07;
    let q = q * t - 0.186_288_06;
    let q = q * t + 0.096_784_18;
    let q = q * t + 0.374_091_96;
    let q = q * t + 1.000_023_7;
    let q = q * t - 1.265_512_2;
    let large = 1.0 - t * exp(q - s);
    let y = if a < 0.5 { small } else { large };
    y.copysign(x)
}

/// GELU in its exact form, `x / 2 (1 + erf(x / sqrt 2))` (PyTorch's default).
#[inline(always)]
pub fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2))
}

/// The logistic function `1 / (1 + e^-x)`.
#[inline(always)]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + exp(-x))
}

fn map_in_place(values: &mut [f32], f: fn(f32) -> f32) {
    let run = |chunk: &mut [f32]| {
        for v in chunk {
            *v = f(*v);
        }
    };
    if values.len() > PAR_CHUNK {
        values.par_chunks_mut(PAR_CHUNK).for_each(run);
    } else {
        run(values);
    }
}

/// GELU on every value, in place.
pub fn gelu_in_place(t: &mut Tensor) {
    map_in_place(t.data_mut(), gelu);
}

/// The sigmoid of every value, in place.
pub fn sigmoid_in_place(t: &mut Tensor) {
    map_in_place(t.data_mut(), sigmoid);
}

/// Lanes of the reductions in [`softmax_row`]: independent accumulators let the compiler use
/// vector instructions, which a single running sum or maximum forbids.
const LANES: usize = 8;

/// The softmax of one row, in place. An empty row stays empty.
pub fn softmax_row(row: &mut [f32]) {
    let (chunks, tail) = row.as_chunks::<LANES>();
    let mut lanes = [f32::NEG_INFINITY; LANES];
    for c in chunks {
        for (m, &v) in lanes.iter_mut().zip(c) {
            *m = if v > *m { v } else { *m };
        }
    }
    let max = lanes.iter().chain(tail).copied().fold(f32::NEG_INFINITY, f32::max);
    for v in row.iter_mut() {
        *v = exp(*v - max);
    }
    let (chunks, tail) = row.as_chunks::<LANES>();
    let mut sums = [0.0f32; LANES];
    for c in chunks {
        for (s, &v) in sums.iter_mut().zip(c) {
            *s += v;
        }
    }
    let sum: f32 = sums.iter().chain(tail).sum();
    let inv = 1.0 / sum;
    for v in row.iter_mut() {
        *v *= inv;
    }
}

/// The softmax over the last axis, in place.
pub fn softmax_in_place(t: &mut Tensor) -> Result<()> {
    let Some(&width) = t.shape().last() else {
        return shape_err("softmax of a tensor without axes");
    };
    if width == 0 {
        return Ok(());
    }
    let rows_per_task = (PAR_CHUNK / width).max(1);
    t.data_mut().par_chunks_mut(rows_per_task * width).for_each(|chunk| {
        for row in chunk.chunks_exact_mut(width) {
            softmax_row(row);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// erf in f64 from its Taylor series (|x| < 3) or the continued fraction of erfc.
    fn erf_ref(x: f64) -> f64 {
        let a = x.abs();
        let y = if a < 3.0 {
            let mut term = a;
            let mut sum = a;
            let mut k = 0.0;
            loop {
                k += 1.0;
                term *= -a * a / k;
                let add = term / (2.0 * k + 1.0);
                sum += add;
                if add.abs() < 1e-17 {
                    break;
                }
            }
            sum * 2.0 / std::f64::consts::PI.sqrt()
        } else {
            // Lentz-free backward evaluation of the continued fraction.
            let mut f = 0.0;
            for n in (1..60).rev() {
                f = (n as f64 / 2.0) / (a + f);
            }
            1.0 - (-a * a).exp() / (std::f64::consts::PI.sqrt() * (a + f))
        };
        y.copysign(x)
    }

    #[test]
    fn exp_is_close() {
        let mut worst = 0.0f64;
        let mut x = -87.0f32;
        while x < 88.0 {
            let want = (x as f64).exp();
            let rel = ((exp(x) as f64 - want) / want).abs();
            worst = worst.max(rel);
            x += 0.0137;
        }
        assert!(worst < 3e-7, "worst relative error {worst}");
        assert_eq!(exp(0.0), 1.0);
        assert!(exp(1000.0).is_finite() && exp(-1000.0) >= 0.0);
    }

    #[test]
    fn erf_is_close() {
        let mut worst = 0.0f64;
        let mut x = -6.0f32;
        while x < 6.0 {
            worst = worst.max((erf(x) as f64 - erf_ref(x as f64)).abs());
            x += 0.001;
        }
        assert!(worst < 1.5e-7, "worst absolute error {worst}");
        assert!((erf_ref(0.5) - 0.520_499_877_813_046_5).abs() < 1e-15);
        assert!((erf_ref(3.5) - 0.999_999_256_901_627_7).abs() < 1e-15);
    }

    #[test]
    fn gelu_and_sigmoid_values() {
        // GELU(1) = 0.5 (1 + erf(1 / sqrt 2)) = 0.841344746...
        assert!((gelu(1.0) - 0.841_344_7).abs() < 2e-7);
        assert!((gelu(-1.0) + 0.158_655_25).abs() < 2e-7);
        assert_eq!(gelu(0.0), 0.0);
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-7);
        assert!((sigmoid(2.0) - 0.880_797_1).abs() < 2e-7);
        assert!(sigmoid(-200.0) >= 0.0 && sigmoid(200.0) <= 1.0);
    }

    #[test]
    fn softmax_values() {
        let mut t = Tensor::new(vec![2, 3], vec![1.0, 2.0, 3.0, 1000.0, 1000.0, 1000.0]).unwrap();
        softmax_in_place(&mut t).unwrap();
        let want = [0.090_030_57, 0.244_728_48, 0.665_240_96];
        for (x, y) in t.data()[..3].iter().zip(want) {
            assert!((x - y).abs() < 1e-7);
        }
        for x in &t.data()[3..] {
            assert!((x - 1.0 / 3.0).abs() < 1e-7);
        }
        let mut scalar = Tensor::new(vec![], vec![1.0]).unwrap();
        assert!(softmax_in_place(&mut scalar).is_err());
    }
}
