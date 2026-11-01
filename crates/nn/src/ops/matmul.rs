//! Matrix products: `C = A B (+ bias)`, cache-blocked, spread over rayon's pool when large.
//!
//! B is packed once into panels of [`NR`] columns, each panel stored `k`-major. A linear layer
//! packs its weight at load time; attention packs its keys and values per head. The kernel
//! keeps an [`MR`] x [`NR`] block of C in registers and walks the shared axis in steps of
//! [`KC`], so a panel slice and a packed slice of A stay in the first cache levels.
//!
//! The code is plain safe Rust. The compiler turns the fixed-size inner loops into vector
//! instructions of the target (SSE2 on a default x86-64 build).

use rayon::prelude::*;

use crate::{Result, Tensor, shape_err};

/// Rows of C per kernel call.
pub const MR: usize = 4;
/// Columns of C per kernel call, and the width of a packed panel of B.
pub const NR: usize = 8;
/// Steps along the shared axis per pass.
const KC: usize = 256;
/// Rows of A per block (and per parallel task).
const MC: usize = 64;
/// Multiply-adds below which a product runs on the calling thread.
const PARALLEL_WORK: usize = 1 << 21;

/// The right-hand matrix B (`k` x `n`) of a product, packed for the kernel.
#[derive(Clone, Debug)]
pub struct PackedB {
    k: usize,
    n: usize,
    /// `n.div_ceil(NR)` panels of `k * NR` values; columns past `n` are zero.
    data: Vec<f32>,
}

fn checked_len(a: usize, b: usize, what: &str) -> Result<usize> {
    match a.checked_mul(b) {
        Some(n) => Ok(n),
        None => shape_err(format!("{what}: {a} x {b} is too large")),
    }
}

impl PackedB {
    /// Packs B from its transpose, given row-major as `n` rows of `k` values. A linear layer's
    /// weight (`out` x `in`) and the keys of attention (`len` x `head_dim`) come this way.
    pub fn from_transposed(bt: &[f32], n: usize, k: usize) -> Result<PackedB> {
        if checked_len(n, k, "matrix")? != bt.len() {
            return shape_err(format!("B^T is {n} x {k} but has {} values", bt.len()));
        }
        let panels = n.div_ceil(NR);
        let mut data = vec![0.0; checked_len(checked_len(panels, k, "packed B")?, NR, "packed B")?];
        if k == 0 {
            return Ok(PackedB { k, n, data });
        }
        for (rows, panel) in bt.chunks(NR * k).zip(data.chunks_exact_mut(k * NR)) {
            for (jj, row) in rows.chunks_exact(k).enumerate() {
                for (step, &v) in panel.as_chunks_mut::<NR>().0.iter_mut().zip(row) {
                    if let Some(d) = step.get_mut(jj) {
                        *d = v;
                    }
                }
            }
        }
        Ok(PackedB { k, n, data })
    }

    /// Packs B given row-major as `k` rows of `n` values. The values of attention come this way.
    pub fn from_rows(b: &[f32], k: usize, n: usize) -> Result<PackedB> {
        if checked_len(n, k, "matrix")? != b.len() {
            return shape_err(format!("B is {k} x {n} but has {} values", b.len()));
        }
        let panels = n.div_ceil(NR);
        let mut data = vec![0.0; checked_len(checked_len(panels, k, "packed B")?, NR, "packed B")?];
        if n == 0 || k == 0 {
            return Ok(PackedB { k, n, data });
        }
        for (p, row) in b.chunks_exact(n).enumerate() {
            for (j, part) in row.chunks(NR).enumerate() {
                let at = (j * k + p) * NR;
                if let Some(dst) = data.get_mut(at..at + part.len()) {
                    dst.copy_from_slice(part);
                }
            }
        }
        Ok(PackedB { k, n, data })
    }

    /// Rows of B (the shared axis).
    pub fn k(&self) -> usize {
        self.k
    }

    /// Columns of B (the columns of the result).
    pub fn n(&self) -> usize {
        self.n
    }
}

/// `C = A B + bias` with A row-major (`m` x `k`) and C row-major (`m` x `n`), overwriting C.
/// `bias` has `n` values, added to every row.
pub fn gemm(a: &[f32], m: usize, b: &PackedB, bias: Option<&[f32]>, c: &mut [f32]) -> Result<()> {
    let (k, n) = (b.k, b.n);
    if checked_len(m, k, "A")? != a.len() {
        return shape_err(format!("A should be {m} x {k}, has {} values", a.len()));
    }
    if checked_len(m, n, "C")? != c.len() {
        return shape_err(format!("C should be {m} x {n}, has {} values", c.len()));
    }
    if let Some(bias) = bias
        && bias.len() != n
    {
        return shape_err(format!("bias should have {n} values, has {}", bias.len()));
    }
    if m == 0 || n == 0 {
        return Ok(());
    }
    if k == 0 {
        for row in c.chunks_exact_mut(n) {
            fill_bias(row, bias);
        }
        return Ok(());
    }
    let work = m.saturating_mul(n).saturating_mul(k);
    if work >= PARALLEL_WORK && m > MC {
        c.par_chunks_mut(MC * n).zip(a.par_chunks(MC * k)).for_each(|(cb, ab)| block(ab, cb, b, bias));
    } else {
        for (cb, ab) in c.chunks_mut(MC * n).zip(a.chunks(MC * k)) {
            block(ab, cb, b, bias);
        }
    }
    Ok(())
}

fn fill_bias(row: &mut [f32], bias: Option<&[f32]>) {
    match bias {
        Some(bias) => row.copy_from_slice(bias),
        None => row.fill(0.0),
    }
}

/// One block of at most [`MC`] rows. `a` holds `rows * k` values and `c` `rows * n`, as the
/// callers' chunking guarantees.
fn block(a: &[f32], c: &mut [f32], b: &PackedB, bias: Option<&[f32]>) {
    let (k, n) = (b.k, b.n);
    let rows = c.len() / n;
    for row in c.chunks_exact_mut(n) {
        fill_bias(row, bias);
    }
    let tiles = rows.div_ceil(MR);
    let mut apack = vec![0.0f32; tiles * MR * KC.min(k)];
    let mut k0 = 0;
    while k0 < k {
        let kc = KC.min(k - k0);
        let apack = match apack.get_mut(..tiles * MR * kc) {
            Some(s) => s,
            None => return,
        };
        pack_a(a, k, k0, kc, apack);
        for (j, panel) in b.data.chunks_exact(k * NR).enumerate() {
            let Some(bp) = panel.get(k0 * NR..(k0 + kc) * NR) else {
                return;
            };
            let col0 = j * NR;
            let ncols = NR.min(n - col0);
            for (ap, crows) in apack.chunks_exact(kc * MR).zip(c.chunks_mut(MR * n)) {
                let acc = kernel(ap, bp);
                for (accrow, crow) in acc.iter().zip(crows.chunks_exact_mut(n)) {
                    if let Some(dst) = crow.get_mut(col0..col0 + ncols) {
                        for (d, &v) in dst.iter_mut().zip(accrow) {
                            *d += v;
                        }
                    }
                }
            }
        }
        k0 += kc;
    }
}

/// Packs columns `k0..k0 + kc` of the rows of `a` into tiles of [`MR`] rows, each stored
/// `k`-major. Rows past the end of `a` stay zero.
fn pack_a(a: &[f32], k: usize, k0: usize, kc: usize, apack: &mut [f32]) {
    for (tile_rows, tile) in a.chunks(MR * k).zip(apack.chunks_exact_mut(kc * MR)) {
        if tile_rows.len() < MR * k {
            tile.fill(0.0);
        }
        for (i, row) in tile_rows.chunks_exact(k).enumerate() {
            let Some(src) = row.get(k0..k0 + kc) else {
                continue;
            };
            for (step, &v) in tile.as_chunks_mut::<MR>().0.iter_mut().zip(src) {
                if let Some(d) = step.get_mut(i) {
                    *d = v;
                }
            }
        }
    }
}

/// An [`MR`] x [`NR`] block of products over the packed slices `a` (`kc * MR`) and `b`
/// (`kc * NR`).
#[inline(always)]
fn kernel(a: &[f32], b: &[f32]) -> [[f32; NR]; MR] {
    let mut acc = [[0.0f32; NR]; MR];
    let (a, _) = a.as_chunks::<MR>();
    let (b, _) = b.as_chunks::<NR>();
    for (av, bv) in a.iter().zip(b) {
        for (row, &x) in acc.iter_mut().zip(av) {
            for (c, &y) in row.iter_mut().zip(bv) {
                *c += x * y;
            }
        }
    }
    acc
}

/// `A B` for a matrix A (`m` x `k`) and a packed B, as a new `m` x `n` tensor.
pub fn matmul(a: &Tensor, b: &PackedB, bias: Option<&[f32]>) -> Result<Tensor> {
    let (m, k) = a.dims2()?;
    if k != b.k {
        return shape_err(format!("A has {k} columns, B has {} rows", b.k));
    }
    let mut out = Tensor::zeros(vec![m, b.n])?;
    gemm(a.data(), m, b, bias, out.data_mut())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(a: &[f32], m: usize, k: usize, b: &[f32], n: usize) -> Vec<f32> {
        let mut c = vec![0.0; m * n];
        for i in 0..m {
            for j in 0..n {
                let mut s = 0.0f64;
                for p in 0..k {
                    s += a[i * k + p] as f64 * b[p * n + j] as f64;
                }
                c[i * n + j] = s as f32;
            }
        }
        c
    }

    fn values(len: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn matches_the_naive_product() {
        for &(m, k, n) in &[(1, 1, 1), (3, 5, 7), (4, 8, 8), (65, 300, 17), (130, 513, 9), (200, 40, 64)] {
            let a = values(m * k, 1);
            let b = values(k * n, 2);
            let bias = values(n, 3);
            let want = naive(&a, m, k, &b, n);
            let packed = PackedB::from_rows(&b, k, n).unwrap();
            let mut c = vec![f32::NAN; m * n];
            gemm(&a, m, &packed, None, &mut c).unwrap();
            for (x, y) in c.iter().zip(&want) {
                assert!((x - y).abs() < 1e-4, "{m}x{k}x{n}: {x} vs {y}");
            }
            // The same B packed from its transpose, with a bias.
            let mut bt = vec![0.0; k * n];
            for p in 0..k {
                for j in 0..n {
                    bt[j * k + p] = b[p * n + j];
                }
            }
            let packed = PackedB::from_transposed(&bt, n, k).unwrap();
            gemm(&a, m, &packed, Some(&bias), &mut c).unwrap();
            for (i, (x, y)) in c.iter().zip(&want).enumerate() {
                assert!((x - y - bias[i % n]).abs() < 1e-4, "{m}x{k}x{n} with bias");
            }
        }
    }

    #[test]
    fn refuses_mismatched_shapes() {
        let packed = PackedB::from_rows(&[1.0; 6], 2, 3).unwrap();
        let mut c = vec![0.0; 6];
        assert!(gemm(&[1.0; 5], 2, &packed, None, &mut c).is_err());
        assert!(gemm(&[1.0; 4], 2, &packed, None, &mut c[..5]).is_err());
        assert!(gemm(&[1.0; 4], 2, &packed, Some(&[0.0; 2]), &mut c).is_err());
        assert!(PackedB::from_rows(&[1.0; 5], 2, 3).is_err());
        assert!(PackedB::from_transposed(&[1.0; 5], 3, 2).is_err());
        assert!(PackedB::from_transposed(&[], usize::MAX, 2).is_err());
        let a = Tensor::zeros(vec![2, 3]).unwrap();
        assert!(matmul(&a, &packed, None).is_err());
    }

    #[test]
    fn empty_shared_axis_gives_the_bias() {
        let packed = PackedB::from_rows(&[], 0, 3).unwrap();
        let mut c = vec![9.0; 6];
        gemm(&[], 2, &packed, Some(&[1.0, 2.0, 3.0]), &mut c).unwrap();
        assert_eq!(c, vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0]);
    }
}
