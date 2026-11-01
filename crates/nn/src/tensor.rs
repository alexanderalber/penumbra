//! The f32 tensor: a shape and its values in row-major order.

use crate::{Result, shape_err};

/// A dense f32 tensor, row-major (the last axis varies fastest).
///
/// The constructors check that the values fill the shape exactly, so every tensor holds
/// `shape.iter().product()` values.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    shape: Vec<usize>,
    data: Vec<f32>,
}

/// The number of values in a shape, `None` on overflow.
pub(crate) fn numel(shape: &[usize]) -> Option<usize> {
    shape.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))
}

impl Tensor {
    /// A tensor from its shape and values. Fails when the count of values differs from the shape.
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Result<Tensor> {
        match numel(&shape) {
            Some(n) if n == data.len() => Ok(Tensor { shape, data }),
            Some(n) => shape_err(format!("shape {shape:?} needs {n} values, got {}", data.len())),
            None => shape_err(format!("shape {shape:?} is too large")),
        }
    }

    /// A tensor of zeros.
    pub fn zeros(shape: Vec<usize>) -> Result<Tensor> {
        match numel(&shape) {
            Some(n) => Ok(Tensor { shape, data: vec![0.0; n] }),
            None => shape_err(format!("shape {shape:?} is too large")),
        }
    }

    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    pub fn data(&self) -> &[f32] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The same values under another shape with the same count.
    pub fn reshape(self, shape: Vec<usize>) -> Result<Tensor> {
        Tensor::new(shape, self.data)
    }

    /// The two axes of a matrix.
    pub fn dims2(&self) -> Result<(usize, usize)> {
        match self.shape.as_slice() {
            &[a, b] => Ok((a, b)),
            s => shape_err(format!("expected 2 axes, got {s:?}")),
        }
    }

    /// The three axes of a 3-axis tensor.
    pub fn dims3(&self) -> Result<(usize, usize, usize)> {
        match self.shape.as_slice() {
            &[a, b, c] => Ok((a, b, c)),
            s => shape_err(format!("expected 3 axes, got {s:?}")),
        }
    }

    /// The four axes of a 4-axis tensor.
    pub fn dims4(&self) -> Result<(usize, usize, usize, usize)> {
        match self.shape.as_slice() {
            &[a, b, c, d] => Ok((a, b, c, d)),
            s => shape_err(format!("expected 4 axes, got {s:?}")),
        }
    }

    /// Fails unless the tensor has exactly this shape.
    pub fn expect_shape(&self, shape: &[usize]) -> Result<()> {
        if self.shape == shape {
            Ok(())
        } else {
            shape_err(format!("expected shape {shape:?}, got {:?}", self.shape))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_count() {
        assert!(Tensor::new(vec![2, 3], vec![0.0; 6]).is_ok());
        assert!(Tensor::new(vec![2, 3], vec![0.0; 5]).is_err());
        assert!(Tensor::new(vec![usize::MAX, 2], vec![]).is_err());
        assert!(Tensor::zeros(vec![usize::MAX, usize::MAX]).is_err());
        let t = Tensor::zeros(vec![2, 3]).unwrap();
        assert_eq!(t.clone().reshape(vec![3, 2]).unwrap().shape(), &[3, 2]);
        assert!(t.reshape(vec![4]).is_err());
    }
}
