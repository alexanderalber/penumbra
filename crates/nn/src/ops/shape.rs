//! Moving axes: swapping the first two axes of a 3-axis tensor and general permutations.

use crate::{Result, Tensor, shape_err};

/// `[a, b, c]` to `[b, a, c]`. Rows of `c` values move as a whole.
pub fn swap01(x: &Tensor) -> Result<Tensor> {
    let (a, b, c) = x.dims3()?;
    let mut out = Tensor::zeros(vec![b, a, c])?;
    if a == 0 || c == 0 {
        return Ok(out);
    }
    let src = x.data();
    for (j, block) in out.data_mut().chunks_exact_mut(a * c).enumerate() {
        for (i, dst) in block.chunks_exact_mut(c).enumerate() {
            let at = (i * b + j) * c;
            if let Some(s) = src.get(at..at + c) {
                dst.copy_from_slice(s);
            }
        }
    }
    Ok(out)
}

/// The axes of `x` in the order `perm`: output axis `i` is input axis `perm[i]`.
pub fn permute(x: &Tensor, perm: &[usize]) -> Result<Tensor> {
    let shape = x.shape();
    let rank = shape.len();
    let mut seen = vec![false; rank];
    for &p in perm {
        match seen.get_mut(p) {
            Some(s) if !*s => *s = true,
            _ => return shape_err(format!("{perm:?} is not a permutation of {rank} axes")),
        }
    }
    if perm.len() != rank {
        return shape_err(format!("{perm:?} is not a permutation of {rank} axes"));
    }
    // Strides of the input, then reordered as the output walks them.
    let mut strides = vec![1usize; rank];
    for i in (0..rank.saturating_sub(1)).rev() {
        let next = strides.get(i + 1).copied().unwrap_or(1) * shape.get(i + 1).copied().unwrap_or(1);
        if let Some(s) = strides.get_mut(i) {
            *s = next;
        }
    }
    let out_shape: Vec<usize> = perm.iter().map(|&p| shape.get(p).copied().unwrap_or(0)).collect();
    let out_strides: Vec<usize> = perm.iter().map(|&p| strides.get(p).copied().unwrap_or(0)).collect();
    let mut out = Tensor::zeros(out_shape.clone())?;
    if out.is_empty() {
        return Ok(out);
    }
    let src = x.data();
    let mut index = vec![0usize; rank];
    let mut at = 0usize;
    for dst in out.data_mut() {
        *dst = src.get(at).copied().unwrap_or(0.0);
        // Advance the odometer from the last axis.
        for ((i, &n), &s) in index.iter_mut().zip(&out_shape).zip(&out_strides).rev() {
            *i += 1;
            at += s;
            if *i < n {
                break;
            }
            at -= s * n;
            *i = 0;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iota(shape: Vec<usize>) -> Tensor {
        let n = shape.iter().product::<usize>();
        Tensor::new(shape, (0..n).map(|v| v as f32).collect()).unwrap()
    }

    #[test]
    fn swap01_moves_rows() {
        let x = iota(vec![2, 3, 2]);
        let y = swap01(&x).unwrap();
        assert_eq!(y.shape(), &[3, 2, 2]);
        assert_eq!(y.data(), &[0.0, 1.0, 6.0, 7.0, 2.0, 3.0, 8.0, 9.0, 4.0, 5.0, 10.0, 11.0]);
        assert_eq!(permute(&x, &[1, 0, 2]).unwrap(), y);
        assert_eq!(swap01(&iota(vec![0, 3, 2])).unwrap().shape(), &[3, 0, 2]);
        assert_eq!(swap01(&iota(vec![2, 3, 0])).unwrap().shape(), &[3, 2, 0]);
    }

    #[test]
    fn permute_values() {
        let x = iota(vec![2, 3, 4]);
        let y = permute(&x, &[2, 0, 1]).unwrap();
        assert_eq!(y.shape(), &[4, 2, 3]);
        for a in 0..2 {
            for b in 0..3 {
                for c in 0..4 {
                    assert_eq!(y.data()[c * 6 + a * 3 + b], x.data()[a * 12 + b * 4 + c]);
                }
            }
        }
        assert!(permute(&x, &[0, 0, 1]).is_err());
        assert!(permute(&x, &[0, 1]).is_err());
        assert!(permute(&x, &[0, 1, 3]).is_err());
    }
}
