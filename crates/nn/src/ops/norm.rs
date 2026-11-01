//! Normalisation over the last axis: RMSNorm (in the form BS-RoFormer uses) and LayerNorm.

use rayon::prelude::*;

use crate::{Result, Tensor, shape_err};

/// Values per parallel task.
const PAR_CHUNK: usize = 1 << 15;

fn rows_of(t: &Tensor, width: usize, what: &str) -> Result<()> {
    match t.shape().last() {
        Some(&w) if w == width && width > 0 => Ok(()),
        Some(&w) => shape_err(format!("{what} over {width} values, the last axis has {w}")),
        None => shape_err(format!("{what} of a tensor without axes")),
    }
}

fn for_rows(out: &mut [f32], src: &[f32], width: usize, f: impl Fn(&mut [f32], &[f32]) + Sync) {
    let rows_per_task = (PAR_CHUNK / width).max(1);
    out.par_chunks_mut(rows_per_task * width).zip(src.par_chunks(rows_per_task * width)).for_each(
        |(o, s)| {
            for (orow, srow) in o.chunks_exact_mut(width).zip(s.chunks_exact(width)) {
                f(orow, srow);
            }
        },
    );
}

/// `x / max(|x|, 1e-12) * scale` per row, where `scale` holds one factor per value of the last
/// axis. With `scale = sqrt(width) * gamma` this is the RMSNorm of BS-RoFormer
/// (`F.normalize(x) * sqrt(dim) * gamma`).
pub fn l2_normalize(x: &Tensor, scale: &[f32]) -> Result<Tensor> {
    let width = scale.len();
    rows_of(x, width, "l2 normalisation")?;
    let mut out = Tensor::zeros(x.shape().to_vec())?;
    for_rows(out.data_mut(), x.data(), width, |o, s| {
        let sq: f64 = s.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        let norm = (sq.sqrt() as f32).max(1e-12);
        for ((o, &v), &g) in o.iter_mut().zip(s).zip(scale) {
            *o = v / norm * g;
        }
    });
    Ok(out)
}

/// LayerNorm over the last axis: `(x - mean) / sqrt(var + eps) * weight + bias`.
pub fn layer_norm(x: &Tensor, weight: &[f32], bias: &[f32], eps: f32) -> Result<Tensor> {
    let width = weight.len();
    rows_of(x, width, "layer norm")?;
    if bias.len() != width {
        return shape_err(format!("layer norm: {width} weights but {} biases", bias.len()));
    }
    let mut out = Tensor::zeros(x.shape().to_vec())?;
    for_rows(out.data_mut(), x.data(), width, |o, s| {
        let n = width as f64;
        let mean = s.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
        let var = s.iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / n;
        let inv = (1.0 / (var + f64::from(eps)).sqrt()) as f32;
        let mean = mean as f32;
        for (((o, &v), &w), &b) in o.iter_mut().zip(s).zip(weight).zip(bias) {
            *o = (v - mean) * inv * w + b;
        }
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_values() {
        let x = Tensor::new(vec![2, 2], vec![3.0, 4.0, 0.0, 0.0]).unwrap();
        let out = l2_normalize(&x, &[2.0, 1.0]).unwrap();
        assert_eq!(out.data(), &[1.2, 0.8, 0.0, 0.0]);
        assert!(l2_normalize(&x, &[1.0; 3]).is_err());
    }

    #[test]
    fn layer_norm_values() {
        let x = Tensor::new(vec![1, 4], vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        let out = layer_norm(&x, &[1.0; 4], &[0.5; 4], 0.0).unwrap();
        // mean 2.5, variance 1.25
        let s = 1.25f32.sqrt();
        for (o, v) in out.data().iter().zip([1.0f32, 2.0, 3.0, 4.0]) {
            assert!((o - ((v - 2.5) / s + 0.5)).abs() < 1e-6);
        }
        assert!(layer_norm(&x, &[1.0; 4], &[0.0; 3], 1e-5).is_err());
    }
}
