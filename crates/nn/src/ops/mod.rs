//! The operations on [`Tensor`]s. Each checks its shapes first and returns an error on a
//! mismatch; none panics.

pub mod act;
pub mod attention;
pub mod conv;
pub mod matmul;
pub mod norm;
pub mod rotary;
pub mod shape;

pub use act::{erf, exp, gelu, gelu_in_place, sigmoid, sigmoid_in_place, softmax_in_place, softmax_row};
pub use attention::{AttentionShape, attention};
pub use conv::{ConvGeometry, conv2d, pack_conv_weight};
pub use matmul::{PackedB, gemm, matmul};
pub use norm::{l2_normalize, layer_norm};
pub use rotary::Rope;
pub use shape::{permute, swap01};

use crate::{Result, Tensor, shape_err};

/// `a += b` for two tensors of the same shape (a residual connection).
pub fn add_in_place(a: &mut Tensor, b: &Tensor) -> Result<()> {
    if a.shape() != b.shape() {
        return shape_err(format!("adding {:?} to {:?}", b.shape(), a.shape()));
    }
    for (x, &y) in a.data_mut().iter_mut().zip(b.data()) {
        *x += y;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_checks_shapes() {
        let mut a = Tensor::new(vec![2], vec![1.0, 2.0]).unwrap();
        add_in_place(&mut a, &Tensor::new(vec![2], vec![0.5, 0.5]).unwrap()).unwrap();
        assert_eq!(a.data(), &[1.5, 2.5]);
        assert!(add_in_place(&mut a, &Tensor::new(vec![1, 2], vec![0.0; 2]).unwrap()).is_err());
    }
}
