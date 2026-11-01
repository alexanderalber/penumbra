//! Layers: weights prepared for their op. Built once at load time, then shared by every call.

use crate::ops::{self, ConvGeometry, PackedB};
use crate::{Error, Result, Tensor, shape_err};

/// `y = x W^T + b` over the last axis, PyTorch's `nn.Linear`.
#[derive(Clone, Debug)]
pub struct Linear {
    weight: PackedB,
    bias: Option<Vec<f32>>,
}

impl Linear {
    /// From a PyTorch weight (`out` x `in`) and an optional bias of `out` values.
    pub fn new(weight: &Tensor, bias: Option<&Tensor>) -> Result<Linear> {
        let (out, inp) = weight.dims2()?;
        let bias = match bias {
            Some(b) => {
                b.expect_shape(&[out])?;
                Some(b.data().to_vec())
            }
            None => None,
        };
        Ok(Linear { weight: PackedB::from_transposed(weight.data(), out, inp)?, bias })
    }

    pub fn in_features(&self) -> usize {
        self.weight.k()
    }

    pub fn out_features(&self) -> usize {
        self.weight.n()
    }

    /// Applies the layer to every row: `x` has any shape whose last axis is `in`; the result
    /// has the same shape with `out` as the last axis.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let inp = self.in_features();
        let mut shape = x.shape().to_vec();
        match shape.last_mut() {
            Some(last) if *last == inp => *last = self.out_features(),
            _ => return shape_err(format!("linear layer takes {inp} features, got {:?}", x.shape())),
        }
        let rows = x.len().checked_div(inp).unwrap_or(0);
        let mut out = Tensor::zeros(shape)?;
        ops::gemm(x.data(), rows, &self.weight, self.bias.as_deref(), out.data_mut())?;
        Ok(out)
    }
}

/// BatchNorm in eval mode as a scale and shift per channel.
#[derive(Clone, Debug)]
pub struct BatchNorm {
    pub scale: Vec<f32>,
    pub shift: Vec<f32>,
}

impl BatchNorm {
    /// From PyTorch's weight, bias, running mean and running variance:
    /// `y = (x - mean) / sqrt(var + eps) * weight + bias`.
    pub fn new(weight: &Tensor, bias: &Tensor, mean: &Tensor, var: &Tensor, eps: f32) -> Result<BatchNorm> {
        let n = weight.len();
        for t in [weight, bias, mean, var] {
            t.expect_shape(&[n])?;
        }
        let mut scale = Vec::with_capacity(n);
        let mut shift = Vec::with_capacity(n);
        for (((&w, &b), &m), &v) in weight.data().iter().zip(bias.data()).zip(mean.data()).zip(var.data()) {
            let s = (f64::from(w) / (f64::from(v) + f64::from(eps)).sqrt()) as f32;
            scale.push(s);
            shift.push((f64::from(b) - f64::from(m) * f64::from(s)) as f32);
        }
        Ok(BatchNorm { scale, shift })
    }

    /// Channels.
    pub fn len(&self) -> usize {
        self.scale.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scale.is_empty()
    }
}

/// A 2-D convolution on channel-last tensors (`h` x `w` x `ci` to `oh` x `ow` x `co`), with an
/// optional BatchNorm after it folded into the weights.
#[derive(Clone, Debug)]
pub struct Conv2d {
    weight: PackedB,
    bias: Option<Vec<f32>>,
    geometry: ConvGeometry,
}

impl Conv2d {
    /// From a PyTorch weight (`co` x `ci` x `kh` x `kw`), an optional bias of `co` values, and
    /// an optional BatchNorm over the `co` outputs.
    pub fn new(
        weight: &Tensor,
        bias: Option<&Tensor>,
        geometry: ConvGeometry,
        norm: Option<&BatchNorm>,
    ) -> Result<Conv2d> {
        let (co, _, kh, kw) = weight.dims4()?;
        if (kh, kw) != geometry.kernel {
            return Err(Error::Weights(format!(
                "convolution weight is {kh} x {kw}, the layer wants {:?}",
                geometry.kernel
            )));
        }
        let mut bias = match bias {
            Some(b) => {
                b.expect_shape(&[co])?;
                Some(b.data().to_vec())
            }
            None => None,
        };
        let mut weight = weight.clone();
        if let Some(bn) = norm {
            if bn.len() != co {
                return shape_err(format!("batch norm over {} channels after {co}", bn.len()));
            }
            let per_out = weight.len() / co.max(1);
            if per_out > 0 {
                for (row, &s) in weight.data_mut().chunks_exact_mut(per_out).zip(&bn.scale) {
                    for v in row {
                        *v *= s;
                    }
                }
            }
            let b = bias.get_or_insert_with(|| vec![0.0; co]);
            for ((b, &s), &t) in b.iter_mut().zip(&bn.scale).zip(&bn.shift) {
                *b = *b * s + t;
            }
        }
        Ok(Conv2d { weight: ops::pack_conv_weight(&weight)?, bias, geometry })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        ops::conv2d(x, self.geometry, &self.weight, self.bias.as_deref())
    }

    pub fn out_channels(&self) -> usize {
        self.weight.n()
    }
}

/// `F.normalize(x) * sqrt(dim) * gamma` over the last axis: the RMSNorm of BS-RoFormer and
/// Beat This!.
#[derive(Clone, Debug)]
pub struct RmsNorm {
    scale: Vec<f32>,
}

impl RmsNorm {
    pub fn new(gamma: &Tensor) -> Result<RmsNorm> {
        let n = gamma.len();
        gamma.expect_shape(&[n])?;
        let root = (n as f32).sqrt();
        Ok(RmsNorm { scale: gamma.data().iter().map(|&g| root * g).collect() })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        ops::l2_normalize(x, &self.scale)
    }
}

/// PyTorch's `nn.LayerNorm` over the last axis.
#[derive(Clone, Debug)]
pub struct LayerNorm {
    weight: Vec<f32>,
    bias: Vec<f32>,
    eps: f32,
}

impl LayerNorm {
    pub fn new(weight: &Tensor, bias: &Tensor, eps: f32) -> Result<LayerNorm> {
        let n = weight.len();
        weight.expect_shape(&[n])?;
        bias.expect_shape(&[n])?;
        Ok(LayerNorm { weight: weight.data().to_vec(), bias: bias.data().to_vec(), eps })
    }

    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        ops::layer_norm(x, &self.weight, &self.bias, self.eps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_on_rows() {
        let w = Tensor::new(vec![2, 3], vec![1.0, 0.0, 0.0, 0.0, 1.0, 1.0]).unwrap();
        let b = Tensor::new(vec![2], vec![0.5, -0.5]).unwrap();
        let l = Linear::new(&w, Some(&b)).unwrap();
        let x = Tensor::new(vec![2, 1, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        let y = l.forward(&x).unwrap();
        assert_eq!(y.shape(), &[2, 1, 2]);
        assert_eq!(y.data(), &[1.5, 4.5, 4.5, 10.5]);
        assert!(l.forward(&Tensor::zeros(vec![2, 2]).unwrap()).is_err());
        assert!(Linear::new(&w, Some(&Tensor::zeros(vec![3]).unwrap())).is_err());
    }

    #[test]
    fn batch_norm_folds_into_the_convolution() {
        let g = ConvGeometry { kernel: (1, 1), stride: (1, 1), padding: (0, 0) };
        let w = Tensor::new(vec![1, 1, 1, 1], vec![2.0]).unwrap();
        let one = |v: f32| Tensor::new(vec![1], vec![v]).unwrap();
        let bn = BatchNorm::new(&one(3.0), &one(1.0), &one(0.5), &one(4.0), 0.0).unwrap();
        let conv = Conv2d::new(&w, None, g, Some(&bn)).unwrap();
        let y = conv.forward(&Tensor::new(vec![1, 1, 1], vec![5.0]).unwrap()).unwrap();
        // (2 * 5 - 0.5) / 2 * 3 + 1
        assert!((y.data()[0] - 15.25).abs() < 1e-6);
    }
}
