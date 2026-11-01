//! 2-D convolution on channel-last tensors, as patches times a weight matrix.

use rayon::prelude::*;

use super::matmul::{PackedB, gemm};
use crate::{Result, Tensor, shape_err};

/// Kernel size, stride and zero padding of a convolution, per axis (height, width).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvGeometry {
    pub kernel: (usize, usize),
    pub stride: (usize, usize),
    pub padding: (usize, usize),
}

impl ConvGeometry {
    /// The output size along one axis, `None` when the kernel does not fit or the stride is 0.
    fn out_len(size: usize, kernel: usize, stride: usize, padding: usize) -> Option<usize> {
        let padded = size.checked_add(padding.checked_mul(2)?)?;
        if stride == 0 || kernel == 0 || padded < kernel {
            return None;
        }
        Some((padded - kernel) / stride + 1)
    }

    /// The output height and width for an input of `h` x `w`.
    pub fn output(&self, h: usize, w: usize) -> Result<(usize, usize)> {
        let oh = Self::out_len(h, self.kernel.0, self.stride.0, self.padding.0);
        let ow = Self::out_len(w, self.kernel.1, self.stride.1, self.padding.1);
        match (oh, ow) {
            (Some(oh), Some(ow)) => Ok((oh, ow)),
            _ => shape_err(format!("convolution {self:?} does not fit an input of {h} x {w}")),
        }
    }
}

/// Convolves `x` (`h` x `w` x `ci`, channel last) with a weight packed as the B of a product
/// whose shared axis runs over (kernel row, kernel column, input channel) and whose columns are
/// the output channels. Returns `oh` x `ow` x `co`, channel last.
pub fn conv2d(x: &Tensor, geometry: ConvGeometry, weight: &PackedB, bias: Option<&[f32]>) -> Result<Tensor> {
    let (h, w, ci) = x.dims3()?;
    let (kh, kw) = geometry.kernel;
    let (oh, ow) = geometry.output(h, w)?;
    let Some(patch) = kh.checked_mul(kw).and_then(|v| v.checked_mul(ci)) else {
        return shape_err("convolution kernel is too large");
    };
    if weight.k() != patch {
        return shape_err(format!("convolution weight has {} rows, the patch {patch}", weight.k()));
    }
    let Some(rows) = oh.checked_mul(ow) else {
        return shape_err("convolution output is too large");
    };
    let Some(cols_len) = rows.checked_mul(patch) else {
        return shape_err("convolution patches are too large");
    };
    let mut cols = vec![0.0f32; cols_len];
    let src = x.data();
    let (sh, sw) = geometry.stride;
    let (ph, pw) = geometry.padding;
    if patch > 0 {
        cols.par_chunks_mut(ow * patch).enumerate().for_each(|(oy, line)| {
            for (ox, p) in line.chunks_exact_mut(patch).enumerate() {
                for (ky, kline) in p.chunks_exact_mut(kw * ci).enumerate() {
                    let iy = (oy * sh + ky).checked_sub(ph).filter(|&v| v < h);
                    for (kx, dst) in kline.chunks_exact_mut(ci).enumerate() {
                        let ix = (ox * sw + kx).checked_sub(pw).filter(|&v| v < w);
                        let s = match (iy, ix) {
                            (Some(iy), Some(ix)) => src.get((iy * w + ix) * ci..(iy * w + ix + 1) * ci),
                            _ => None,
                        };
                        // The zeros of `cols` are the padding.
                        if let Some(s) = s {
                            dst.copy_from_slice(s);
                        }
                    }
                }
            }
        });
    }
    let mut out = Tensor::zeros(vec![oh, ow, weight.n()])?;
    gemm(&cols, rows, weight, bias, out.data_mut())?;
    Ok(out)
}

/// Reorders a PyTorch convolution weight (`co` x `ci` x `kh` x `kw`) into the B that
/// [`conv2d`] takes.
pub fn pack_conv_weight(weight: &Tensor) -> Result<PackedB> {
    let (co, ci, kh, kw) = weight.dims4()?;
    let Some(patch) = kh.checked_mul(kw).and_then(|v| v.checked_mul(ci)) else {
        return shape_err("convolution kernel is too large");
    };
    let Some(len) = co.checked_mul(patch) else {
        return shape_err("convolution weight is too large");
    };
    // Each output channel's row reordered from (ci, ky, kx) to (ky, kx, ci).
    let mut bt = vec![0.0f32; len];
    let src = weight.data();
    for (o, row) in bt.chunks_exact_mut(patch.max(1)).enumerate() {
        for (j, dst) in row.iter_mut().enumerate() {
            let (ky, kx, c) = (j / (kw * ci), j / ci % kw, j % ci);
            *dst = src.get(((o * ci + c) * kh + ky) * kw + kx).copied().unwrap_or(0.0);
        }
    }
    PackedB::from_transposed(&bt, co, patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_direct_convolution() {
        let (h, w, ci, co) = (9, 7, 3, 5);
        let g = ConvGeometry { kernel: (2, 3), stride: (2, 1), padding: (0, 1) };
        let x: Vec<f32> = (0..h * w * ci).map(|i| (i % 13) as f32 - 6.0).collect();
        let wt: Vec<f32> = (0..co * ci * 6).map(|i| (i % 7) as f32 / 7.0 - 0.5).collect();
        let xt = Tensor::new(vec![h, w, ci], x.clone()).unwrap();
        let weight = Tensor::new(vec![co, ci, 2, 3], wt.clone()).unwrap();
        let packed = pack_conv_weight(&weight).unwrap();
        let bias = [0.5, -1.0, 0.0, 2.0, 1.0];
        let out = conv2d(&xt, g, &packed, Some(&bias)).unwrap();
        let (oh, ow) = (4, 7);
        assert_eq!(out.shape(), &[oh, ow, co]);
        for oy in 0..oh {
            for ox in 0..ow {
                for o in 0..co {
                    let mut s = bias[o];
                    for c in 0..ci {
                        for ky in 0..2 {
                            for kx in 0..3 {
                                let iy = oy * 2 + ky;
                                let ix = ox as isize + kx as isize - 1;
                                if ix < 0 || ix >= w as isize {
                                    continue;
                                }
                                s +=
                                    x[(iy * w + ix as usize) * ci + c] * wt[((o * ci + c) * 2 + ky) * 3 + kx];
                            }
                        }
                    }
                    let got = out.data()[(oy * ow + ox) * co + o];
                    assert!((got - s).abs() < 1e-4, "{oy} {ox} {o}: {got} vs {s}");
                }
            }
        }
    }

    #[test]
    fn refuses_bad_shapes() {
        let g = ConvGeometry { kernel: (4, 3), stride: (4, 1), padding: (0, 1) };
        let packed = pack_conv_weight(&Tensor::zeros(vec![2, 1, 4, 3]).unwrap()).unwrap();
        assert!(conv2d(&Tensor::zeros(vec![3, 5, 1]).unwrap(), g, &packed, None).is_err());
        assert!(conv2d(&Tensor::zeros(vec![8, 5, 2]).unwrap(), g, &packed, None).is_err());
        let zero_stride = ConvGeometry { stride: (0, 1), ..g };
        assert!(conv2d(&Tensor::zeros(vec![8, 5, 1]).unwrap(), zero_stride, &packed, None).is_err());
        assert!(conv2d(&Tensor::zeros(vec![8, 5, 1]).unwrap(), g, &packed, None).is_ok());
    }
}
