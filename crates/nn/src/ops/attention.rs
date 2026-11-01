//! Scaled dot-product attention over a batch of sequences with several heads, optionally with
//! rotary embeddings on queries and keys and a gate per head and position.

use rayon::prelude::*;

use super::act::softmax_row;
use super::matmul::{PackedB, gemm};
use super::rotary::Rope;
use crate::{Result, Tensor, shape_err};

/// Queries per task: the scores of one block stay small (64 x len).
const QUERY_BLOCK: usize = 64;

/// The layout of one attention call.
#[derive(Clone, Copy, Debug)]
pub struct AttentionShape {
    /// Independent sequences.
    pub batch: usize,
    /// Positions per sequence.
    pub len: usize,
    pub heads: usize,
    pub head_dim: usize,
}

struct Head {
    /// Queries, scaled by `1 / sqrt(head_dim)`, `len` x `head_dim`.
    q: Vec<f32>,
    /// Keys as the B of `Q K^T`.
    k: PackedB,
    /// Values as the B of `P V`.
    v: PackedB,
}

/// Attention from projected queries, keys and values.
///
/// `qkv` has `batch * len` rows of `3 * heads * head_dim` values: all queries, then all keys,
/// then all values, each head's `head_dim` features side by side (einops `(qkv h d)`). `rope`,
/// when given, turns queries and keys and must cover `len` positions. `gates`, when given, has
/// `batch * len` rows of `heads` factors that scale each head's output.
///
/// Returns `batch * len` rows of `heads * head_dim` values, heads side by side.
pub fn attention(
    qkv: &Tensor,
    shape: AttentionShape,
    rope: Option<&Rope>,
    gates: Option<&Tensor>,
) -> Result<Tensor> {
    let AttentionShape { batch, len, heads, head_dim } = shape;
    let inner = heads.checked_mul(head_dim);
    let rows = batch.checked_mul(len);
    let (Some(inner), Some(rows)) = (inner, rows) else {
        return shape_err("attention shape is too large");
    };
    let Some(width) = inner.checked_mul(3) else {
        return shape_err("attention shape is too large");
    };
    qkv.expect_shape(&[rows, width])?;
    if let Some(g) = gates {
        g.expect_shape(&[rows, heads])?;
    }
    if let Some(r) = rope
        && (r.len() != len || r.rotated_dim() > head_dim)
    {
        return shape_err(format!(
            "rotary table for {} positions of {} features, attention over {len} of {head_dim}",
            r.len(),
            r.rotated_dim()
        ));
    }
    let mut out = Tensor::zeros(vec![rows, inner])?;
    if rows == 0 || inner == 0 {
        return Ok(out);
    }
    let scale = 1.0 / (head_dim as f32).sqrt();
    let data = qkv.data();

    // Gather, turn and pack each (sequence, head).
    let prepared: Vec<Head> = (0..batch * heads)
        .into_par_iter()
        .map(|bh| {
            let (b, h) = (bh / heads, bh % heads);
            let gather = |part: usize| -> Vec<f32> {
                let mut m = Vec::with_capacity(len * head_dim);
                let col = part * inner + h * head_dim;
                for row in data.chunks_exact(width).skip(b * len).take(len) {
                    m.extend_from_slice(row.get(col..col + head_dim).unwrap_or(&[]));
                }
                m
            };
            let mut q = gather(0);
            let mut k = gather(1);
            let v = gather(2);
            if let Some(r) = rope {
                r.apply(&mut q, head_dim)?;
                r.apply(&mut k, head_dim)?;
            }
            for x in &mut q {
                *x *= scale;
            }
            Ok(Head {
                q,
                k: PackedB::from_transposed(&k, len, head_dim)?,
                v: PackedB::from_rows(&v, len, head_dim)?,
            })
        })
        .collect::<Result<_>>()?;

    let blocks = len.div_ceil(QUERY_BLOCK);
    let gate_data = gates.map(Tensor::data);
    let results: Vec<(usize, usize, Vec<f32>)> = (0..prepared.len() * blocks)
        .into_par_iter()
        .map(|task| {
            let (bh, blk) = (task / blocks, task % blocks);
            let Some(head) = prepared.get(bh) else {
                return shape_err("attention task out of range");
            };
            let q0 = blk * QUERY_BLOCK;
            let qn = QUERY_BLOCK.min(len - q0);
            let Some(q) = head.q.get(q0 * head_dim..(q0 + qn) * head_dim) else {
                return shape_err("attention query block out of range");
            };
            let mut scores = vec![0.0f32; qn * len];
            gemm(q, qn, &head.k, None, &mut scores)?;
            for row in scores.chunks_exact_mut(len) {
                softmax_row(row);
            }
            let mut o = vec![0.0f32; qn * head_dim];
            gemm(&scores, qn, &head.v, None, &mut o)?;
            if let Some(g) = gate_data {
                let (b, h) = (bh / heads, bh % heads);
                for (i, orow) in o.chunks_exact_mut(head_dim).enumerate() {
                    let factor = g.get((b * len + q0 + i) * heads + h).copied().unwrap_or(1.0);
                    for x in orow {
                        *x *= factor;
                    }
                }
            }
            Ok((bh, q0, o))
        })
        .collect::<Result<_>>()?;

    let out_data = out.data_mut();
    for (bh, q0, o) in results {
        let (b, h) = (bh / heads, bh % heads);
        for (i, orow) in o.chunks_exact(head_dim).enumerate() {
            let at = (b * len + q0 + i) * inner + h * head_dim;
            if let Some(dst) = out_data.get_mut(at..at + head_dim) {
                dst.copy_from_slice(orow);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attention written out in f64 for one sequence and one head, no rotary.
    fn naive(q: &[f32], k: &[f32], v: &[f32], len: usize, d: usize) -> Vec<f64> {
        let mut out = vec![0.0; len * d];
        for i in 0..len {
            let s: Vec<f64> = (0..len)
                .map(|j| {
                    (0..d).map(|x| q[i * d + x] as f64 * k[j * d + x] as f64).sum::<f64>() / (d as f64).sqrt()
                })
                .collect();
            let max = s.iter().cloned().fold(f64::MIN, f64::max);
            let e: Vec<f64> = s.iter().map(|x| (x - max).exp()).collect();
            let sum: f64 = e.iter().sum();
            for x in 0..d {
                out[i * d + x] = (0..len).map(|j| e[j] / sum * v[j * d + x] as f64).sum();
            }
        }
        out
    }

    #[test]
    fn matches_the_naive_attention() {
        let (batch, len, heads, d) = (2, 70, 3, 4);
        let inner = heads * d;
        let n = batch * len * 3 * inner;
        let qkv: Vec<f32> = (0..n).map(|i| ((i * 37 % 101) as f32 / 50.0) - 1.0).collect();
        let gates: Vec<f32> = (0..batch * len * heads).map(|i| (i % 7) as f32 / 7.0).collect();
        let t = Tensor::new(vec![batch * len, 3 * inner], qkv.clone()).unwrap();
        let g = Tensor::new(vec![batch * len, heads], gates.clone()).unwrap();
        let shape = AttentionShape { batch, len, heads, head_dim: d };
        let out = attention(&t, shape, None, Some(&g)).unwrap();
        for b in 0..batch {
            for h in 0..heads {
                let part = |p: usize| -> Vec<f32> {
                    (0..len)
                        .flat_map(|i| {
                            let at = (b * len + i) * 3 * inner + p * inner + h * d;
                            qkv[at..at + d].to_vec()
                        })
                        .collect()
                };
                let want = naive(&part(0), &part(1), &part(2), len, d);
                for i in 0..len {
                    let gate = gates[(b * len + i) * heads + h] as f64;
                    for x in 0..d {
                        let got = out.data()[(b * len + i) * inner + h * d + x] as f64;
                        assert!((got - want[i * d + x] * gate).abs() < 1e-5);
                    }
                }
            }
        }
        let bad = AttentionShape { batch, len: len + 1, heads, head_dim: d };
        assert!(attention(&t, bad, None, None).is_err());
        let rope = Rope::new(&[1.0], 5).unwrap();
        assert!(attention(&t, shape, Some(&rope), None).is_err());
    }
}
