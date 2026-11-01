//! Rotary position embeddings as `rotary_embedding_torch` 0.9.1 applies them: positions
//! `0..len`, each frequency used for one interleaved pair of features `(2i, 2i + 1)`.

use crate::{Result, shape_err};

/// The cosines and sines for positions `0..len`, one per frequency.
#[derive(Clone, Debug)]
pub struct Rope {
    len: usize,
    freqs: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Rope {
    /// The table for `len` positions. Position `p` turns pair `i` by `p * freqs[i]`, the
    /// product taken in f32 as PyTorch does.
    pub fn new(freqs: &[f32], len: usize) -> Result<Rope> {
        let Some(n) = len.checked_mul(freqs.len()) else {
            return shape_err(format!("rotary table for {len} positions is too large"));
        };
        let mut cos = Vec::with_capacity(n);
        let mut sin = Vec::with_capacity(n);
        for p in 0..len {
            for &f in freqs {
                let angle = f64::from(p as f32 * f);
                cos.push(angle.cos() as f32);
                sin.push(angle.sin() as f32);
            }
        }
        Ok(Rope { len, freqs: freqs.len(), cos, sin })
    }

    /// Positions in the table.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Features turned per position (two per frequency).
    pub fn rotated_dim(&self) -> usize {
        2 * self.freqs
    }

    /// Turns a sequence in place: `x` holds `self.len()` rows of `dim` features, the first
    /// [`Self::rotated_dim`] of which are turned and the rest left alone.
    pub fn apply(&self, x: &mut [f32], dim: usize) -> Result<()> {
        if dim < self.rotated_dim() || Some(x.len()) != self.len.checked_mul(dim) {
            return shape_err(format!(
                "rotary over {} positions of {} features, got {} values of width {dim}",
                self.len,
                self.rotated_dim(),
                x.len()
            ));
        }
        if self.freqs == 0 || dim == 0 {
            return Ok(());
        }
        let rows = x
            .chunks_exact_mut(dim)
            .zip(self.cos.chunks_exact(self.freqs))
            .zip(self.sin.chunks_exact(self.freqs));
        for ((row, cos), sin) in rows {
            let (pairs, _) = row.as_chunks_mut::<2>();
            for ((pair, &c), &s) in pairs.iter_mut().zip(cos).zip(sin) {
                let [a, b] = *pair;
                *pair = [a * c - b * s, b * c + a * s];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_pairs() {
        let rope = Rope::new(&[1.0, 0.5], 3).unwrap();
        // Three positions of [1, 0, 1, 0, 7]: the fifth feature is not rotated.
        let mut x = [1.0, 0.0, 1.0, 0.0, 7.0].repeat(3);
        rope.apply(&mut x, 5).unwrap();
        for p in 0..3 {
            let row = &x[p * 5..p * 5 + 5];
            let (a1, a2) = (p as f32, p as f32 * 0.5);
            let want = [a1.cos(), a1.sin(), a2.cos(), a2.sin(), 7.0];
            for (g, w) in row.iter().zip(want) {
                assert!((g - w).abs() < 1e-6, "position {p}: {row:?}");
            }
        }
        // (a, b) at angle 1: (a cos - b sin, b cos + a sin).
        let rope = Rope::new(&[1.0], 2).unwrap();
        let mut x = vec![0.0, 0.0, 2.0, 3.0];
        rope.apply(&mut x, 2).unwrap();
        let (c, s) = (1f32.cos(), 1f32.sin());
        assert!((x[2] - (2.0 * c - 3.0 * s)).abs() < 1e-6 && (x[3] - (3.0 * c + 2.0 * s)).abs() < 1e-6);
    }

    #[test]
    fn refuses_bad_shapes() {
        let rope = Rope::new(&[1.0, 0.5], 3).unwrap();
        assert!(rope.apply(&mut [0.0; 9], 3).is_err());
        assert!(rope.apply(&mut [0.0; 8], 4).is_err());
    }
}
