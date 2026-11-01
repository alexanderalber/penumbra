//! The RoFormer blocks of BS-RoFormer as Beat This! uses them: RMSNorm, attention with rotary
//! embeddings and a sigmoid gate per head, and a GELU feed-forward, each as a residual branch.
//!
//! Every weight of the PyTorch modules is required, so a file with a tensor missing fails to
//! load instead of running a different network. Weight names follow the PyTorch modules (`norm.gamma`, `to_qkv.weight`, `to_gates.weight`,
//! `to_out.0.weight`, `rotary_embed.freqs`; `net.0.gamma`, `net.1.*`, `net.4.*`).

use crate::layers::{Linear, RmsNorm};
use crate::ops::{self, AttentionShape, Rope};
use crate::{Error, Result, SafeTensors, Tensor};

/// Pre-norm multi-head attention, the residual branch only.
#[derive(Clone, Debug)]
pub struct Attention {
    norm: RmsNorm,
    to_qkv: Linear,
    to_gates: Linear,
    to_out: Linear,
    heads: usize,
    head_dim: usize,
    rotary: Vec<f32>,
}

impl Attention {
    /// Loads the module under `prefix` (for example `transformer_blocks.layers.0.0`) with heads
    /// of `head_dim` features.
    pub fn load(st: &SafeTensors, prefix: &str, head_dim: usize) -> Result<Attention> {
        let name = |n: &str| format!("{prefix}.{n}");
        let norm_gamma = st.get(&name("norm.gamma"))?;
        let dim = norm_gamma.len();
        let qkv = st.get(&name("to_qkv.weight"))?;
        let (rows, cols) = qkv.dims2()?;
        let inner = rows / 3;
        if cols != dim || rows % 3 != 0 || head_dim == 0 || inner % head_dim != 0 {
            return Err(Error::Weights(format!(
                "{prefix}: to_qkv is {rows} x {cols}, which does not split into heads of {head_dim} over {dim} features"
            )));
        }
        let heads = inner / head_dim;
        let to_gates = Linear::new(
            st.get_shaped(&name("to_gates.weight"), &[heads, dim])?,
            Some(st.get_shaped(&name("to_gates.bias"), &[heads])?),
        )?;
        let to_out = Linear::new(st.get_shaped(&name("to_out.0.weight"), &[dim, inner])?, None)?;
        let freqs = st.get(&name("rotary_embed.freqs"))?;
        if freqs.shape().len() != 1 || 2 * freqs.len() > head_dim {
            return Err(Error::Weights(format!(
                "{prefix}: rotary frequencies of shape {:?} do not fit heads of {head_dim}",
                freqs.shape()
            )));
        }
        let rotary = freqs.data().to_vec();
        Ok(Attention {
            norm: RmsNorm::new(norm_gamma)?,
            to_qkv: Linear::new(qkv, None)?,
            to_gates,
            to_out,
            heads,
            head_dim,
            rotary,
        })
    }

    pub fn heads(&self) -> usize {
        self.heads
    }

    /// `x` is `batch` x `len` x `dim`: attention within each of the `batch` sequences.
    /// Returns the branch to add to `x`, same shape.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (batch, len, dim) = x.dims3()?;
        let xn = self.norm.forward(x)?;
        let rows = batch * len;
        let qkv = self.to_qkv.forward(&xn)?.reshape(vec![rows, self.to_qkv.out_features()])?;
        let mut gates = self.to_gates.forward(&xn)?.reshape(vec![rows, self.heads])?;
        ops::sigmoid_in_place(&mut gates);
        let rope = Rope::new(&self.rotary, len)?;
        let shape = AttentionShape { batch, len, heads: self.heads, head_dim: self.head_dim };
        let out = ops::attention(&qkv, shape, Some(&rope), Some(&gates))?;
        self.to_out.forward(&out)?.reshape(vec![batch, len, dim])
    }
}

/// Pre-norm feed-forward: RMSNorm, linear, GELU, linear. The residual branch only.
#[derive(Clone, Debug)]
pub struct FeedForward {
    norm: RmsNorm,
    up: Linear,
    down: Linear,
}

impl FeedForward {
    /// Loads the module under `prefix` (the part before `.net.0.gamma`).
    pub fn load(st: &SafeTensors, prefix: &str) -> Result<FeedForward> {
        let name = |n: &str| format!("{prefix}.net.{n}");
        let gamma = st.get(&name("0.gamma"))?;
        let dim = gamma.len();
        let up_w = st.get(&name("1.weight"))?;
        let (inner, _) = up_w.dims2()?;
        up_w.expect_shape(&[inner, dim]).map_err(|e| Error::Weights(format!("{prefix}: {e}")))?;
        let up = Linear::new(up_w, Some(st.get(&name("1.bias"))?))?;
        let down =
            Linear::new(st.get_shaped(&name("4.weight"), &[dim, inner])?, Some(st.get(&name("4.bias"))?))?;
        Ok(FeedForward { norm: RmsNorm::new(gamma)?, up, down })
    }

    pub fn inner_dim(&self) -> usize {
        self.up.out_features()
    }

    /// Any shape with `dim` as the last axis; returns the branch, same shape.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let mut h = self.up.forward(&self.norm.forward(x)?)?;
        ops::gelu_in_place(&mut h);
        self.down.forward(&h)
    }
}

/// A stack of attention and feed-forward blocks with an RMSNorm at the end.
#[derive(Clone, Debug)]
pub struct Transformer {
    layers: Vec<(Attention, FeedForward)>,
    norm: RmsNorm,
}

impl Transformer {
    /// Loads `{prefix}.layers.{i}.0` (attention) and `.1` (feed-forward) for every `i` present,
    /// and `{prefix}.norm.gamma`. `depth`, when given, is the number of layers the file must hold.
    pub fn load(
        st: &SafeTensors,
        prefix: &str,
        head_dim: usize,
        depth: Option<usize>,
    ) -> Result<Transformer> {
        let mut layers = Vec::new();
        while st.contains(&format!("{prefix}.layers.{}.0.to_qkv.weight", layers.len())) {
            let at = format!("{prefix}.layers.{}", layers.len());
            layers.push((
                Attention::load(st, &format!("{at}.0"), head_dim)?,
                FeedForward::load(st, &format!("{at}.1"))?,
            ));
        }
        if layers.is_empty() || depth.is_some_and(|d| d != layers.len()) {
            return Err(Error::Weights(format!(
                "{prefix} has {} layers, expected {}",
                layers.len(),
                depth.map_or("at least one".to_string(), |d| d.to_string())
            )));
        }
        let norm = RmsNorm::new(st.get(&format!("{prefix}.norm.gamma"))?)?;
        Ok(Transformer { layers, norm })
    }

    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    /// `batch` x `len` x `dim` in and out.
    pub fn forward(&self, mut x: Tensor) -> Result<Tensor> {
        for (attn, ff) in &self.layers {
            let a = attn.forward(&x)?;
            ops::add_in_place(&mut x, &a)?;
            let f = ff.forward(&x)?;
            ops::add_in_place(&mut x, &f)?;
        }
        self.norm.forward(&x)
    }
}
