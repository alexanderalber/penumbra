//! Beat This! (Foscarin, Schlüter and Widmer, ISMIR 2024; code and weights MIT, CPJKU): beat
//! and downbeat logits per frame from a log-mel spectrogram.
//!
//! The network: a BatchNorm per mel band, a stem convolution (4 bands to 1, stride 4), three
//! frontend blocks (attention across bands, then across time, then a convolution halving the
//! bands and doubling the channels), a linear projection, a RoFormer and a linear head. Weight
//! names are those of the PyTorch module without the `model.` prefix of the checkpoints.
//!
//! The sizes come from the weights, so the full model (`final*`, width 512) and the small one
//! (`small*`, width 128) both load. Every tensor of the PyTorch model is required. Three
//! metadata keys, all optional, are read:
//!
//! - `sum_head`: whether the head sums the downbeat logit into the beat logit, which the weights
//!   cannot show. `"false"` selects the plain head; without the key the sum head is used, as in
//!   every published checkpoint.
//! - `head_dim`: features per attention head. Without it, twice the count of rotary frequencies.
//! - `n_layers`: RoFormer layers the file must hold, so a file missing a whole layer fails.
//!
//! Input: a chunk of `T` frames x 128 mel bands (`log1p(1000 * mel)`, 50 frames per second;
//! upstream runs chunks of 1500 frames). Output: two logits per frame.

use std::path::Path;

use super::roformer::{Attention, FeedForward, Transformer};
use crate::layers::{BatchNorm, Conv2d, Linear};
use crate::ops::{self, ConvGeometry};
use crate::{Error, Result, SafeTensors, Tensor, shape_err};

/// PyTorch's BatchNorm default.
const BN_EPS: f32 = 1e-5;
const FRONTEND_BLOCKS: usize = 3;

/// The sizes of a loaded model, as read from its weights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatThisConfig {
    /// Mel bands per frame.
    pub spect_dim: usize,
    /// Channels after the stem.
    pub stem_dim: usize,
    /// Width of the RoFormer.
    pub transformer_dim: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub head_dim: usize,
    /// Width of the RoFormer's feed-forward layers.
    pub ff_dim: usize,
    /// Whether the frontend blocks attend across bands and time before their convolution.
    pub partial_transformers: bool,
    /// Whether the beat logit is the sum of the head's beat and downbeat outputs.
    pub sum_head: bool,
}

/// The logits of one chunk, one value per input frame. Positive means a beat (or downbeat) is
/// more likely than not at that frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeatLogits {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
}

#[derive(Clone, Debug)]
struct Partial {
    attn_f: Attention,
    ff_f: FeedForward,
    attn_t: Attention,
    ff_t: FeedForward,
}

#[derive(Clone, Debug)]
struct FrontendBlock {
    partial: Option<Partial>,
    conv: Conv2d,
}

/// A loaded Beat This! network. Immutable after loading; [`BeatThis::forward`] can run from
/// several threads at once.
#[derive(Clone, Debug)]
pub struct BeatThis {
    config: BeatThisConfig,
    bn1d: BatchNorm,
    stem: Conv2d,
    blocks: Vec<FrontendBlock>,
    linear: Linear,
    transformer: Transformer,
    head: Linear,
}

fn batch_norm(st: &SafeTensors, prefix: &str) -> Result<BatchNorm> {
    let get = |n: &str| st.get(&format!("{prefix}.{n}"));
    BatchNorm::new(get("weight")?, get("bias")?, get("running_mean")?, get("running_var")?, BN_EPS)
}

fn weights_err<T>(msg: String) -> Result<T> {
    Err(Error::Weights(msg))
}

/// A whole number from the file's metadata, `None` when the key is absent.
fn meta_number(st: &SafeTensors, key: &str) -> Result<Option<usize>> {
    match st.metadata().get(key) {
        Some(s) => match s.parse::<usize>() {
            Ok(v) => Ok(Some(v)),
            Err(_) => weights_err(format!("metadata {key} {s:?} is not a whole number")),
        },
        None => Ok(None),
    }
}

impl BeatThis {
    /// Loads a converted checkpoint from a `.safetensors` file.
    pub fn load(path: &Path) -> Result<BeatThis> {
        BeatThis::from_safetensors(&SafeTensors::read(path)?)
    }

    pub fn from_safetensors(st: &SafeTensors) -> Result<BeatThis> {
        let bn1d = batch_norm(st, "frontend.stem.bn1d")?;
        let spect_dim = bn1d.len();
        let stem_w = st.get("frontend.stem.conv2d.weight")?;
        let (stem_dim, stem_in, kh, kw) = stem_w.dims4()?;
        if stem_in != 1 || (kh, kw) != (4, 3) {
            return weights_err(format!("stem convolution is {:?}, expected [C, 1, 4, 3]", stem_w.shape()));
        }
        let stem_geometry = ConvGeometry { kernel: (4, 3), stride: (4, 1), padding: (0, 1) };
        let stem = Conv2d::new(stem_w, None, stem_geometry, Some(&batch_norm(st, "frontend.stem.bn2d")?))?;
        let mut bands = stem_geometry.output(spect_dim, 1)?.0;

        let freqs = st.get("transformer_blocks.layers.0.0.rotary_embed.freqs")?;
        let head_dim = meta_number(st, "head_dim")?.unwrap_or(2 * freqs.len());
        let partial_transformers = st.names().any(|n| n.starts_with("frontend.blocks.0.partial."));
        let block_geometry = ConvGeometry { kernel: (2, 3), stride: (2, 1), padding: (0, 1) };
        let mut blocks = Vec::with_capacity(FRONTEND_BLOCKS);
        let mut channels = stem_dim;
        for i in 0..FRONTEND_BLOCKS {
            let p = format!("frontend.blocks.{i}");
            let partial = if partial_transformers {
                Some(Partial {
                    attn_f: Attention::load(st, &format!("{p}.partial.attnF"), head_dim)?,
                    ff_f: FeedForward::load(st, &format!("{p}.partial.ffF"))?,
                    attn_t: Attention::load(st, &format!("{p}.partial.attnT"), head_dim)?,
                    ff_t: FeedForward::load(st, &format!("{p}.partial.ffT"))?,
                })
            } else {
                None
            };
            let w = st.get(&format!("{p}.conv2d.weight"))?;
            let (_, ci, _, _) = w.dims4()?;
            if ci != channels {
                return weights_err(format!(
                    "{p}.conv2d takes {ci} channels, the block before gives {channels}"
                ));
            }
            let conv = Conv2d::new(w, None, block_geometry, Some(&batch_norm(st, &format!("{p}.norm"))?))?;
            channels = conv.out_channels();
            bands = block_geometry.output(bands, 1)?.0;
            blocks.push(FrontendBlock { partial, conv });
        }

        let linear = Linear::new(st.get("frontend.linear.weight")?, Some(st.get("frontend.linear.bias")?))?;
        if Some(linear.in_features()) != channels.checked_mul(bands) {
            return weights_err(format!(
                "frontend.linear takes {} features, the frontend gives {channels} x {bands}",
                linear.in_features()
            ));
        }
        let transformer_dim = linear.out_features();
        let transformer =
            Transformer::load(st, "transformer_blocks", head_dim, meta_number(st, "n_layers")?)?;
        let head = Linear::new(
            st.get_shaped("task_heads.beat_downbeat_lin.weight", &[2, transformer_dim])?,
            Some(st.get("task_heads.beat_downbeat_lin.bias")?),
        )?;
        let sum_head = st.metadata().get("sum_head").is_none_or(|v| v != "false");
        let first = Attention::load(st, "transformer_blocks.layers.0.0", head_dim)?;
        let ff_dim = FeedForward::load(st, "transformer_blocks.layers.0.1")?.inner_dim();
        let config = BeatThisConfig {
            spect_dim,
            stem_dim,
            transformer_dim,
            n_layers: transformer.depth(),
            n_heads: first.heads(),
            head_dim,
            ff_dim,
            partial_transformers,
            sum_head,
        };
        Ok(BeatThis { config, bn1d, stem, blocks, linear, transformer, head })
    }

    pub fn config(&self) -> &BeatThisConfig {
        &self.config
    }

    /// The logits for one chunk: `spect` is `T` frames x `spect_dim` bands, `T` at least 1.
    pub fn forward(&self, spect: &Tensor) -> Result<BeatLogits> {
        let (frames, bands) = spect.dims2()?;
        if bands != self.config.spect_dim || frames == 0 {
            return shape_err(format!(
                "Beat This! takes T x {} with T >= 1, got {:?}",
                self.config.spect_dim,
                spect.shape()
            ));
        }
        // The frontend works channel last: bands x frames x channels.
        let mut x = vec![0.0f32; bands * frames];
        for (t, row) in spect.data().chunks_exact(bands).enumerate() {
            for (f, ((&v, &s), &b)) in row.iter().zip(&self.bn1d.scale).zip(&self.bn1d.shift).enumerate() {
                if let Some(d) = x.get_mut(f * frames + t) {
                    *d = v * s + b;
                }
            }
        }
        let mut x = self.stem.forward(&Tensor::new(vec![bands, frames, 1], x)?)?;
        ops::gelu_in_place(&mut x);
        for block in &self.blocks {
            if let Some(p) = &block.partial {
                // Across bands: one sequence per frame.
                let mut y = ops::swap01(&x)?;
                residual(&mut y, |v| p.attn_f.forward(v))?;
                residual(&mut y, |v| p.ff_f.forward(v))?;
                // Across time: one sequence per band.
                x = ops::swap01(&y)?;
                residual(&mut x, |v| p.attn_t.forward(v))?;
                residual(&mut x, |v| p.ff_t.forward(v))?;
            }
            x = block.conv.forward(&x)?;
            ops::gelu_in_place(&mut x);
        }
        // Each frame's features ordered channel-major, then band (einops `b t (c f)`).
        let (f, t, c) = x.dims3()?;
        let x = ops::permute(&x, &[1, 2, 0])?.reshape(vec![t, c * f])?;
        let x = self.linear.forward(&x)?.reshape(vec![1, t, self.config.transformer_dim])?;
        let x = self.transformer.forward(x)?;
        let logits = self.head.forward(&x)?;
        let mut out = BeatLogits { beat: Vec::with_capacity(t), downbeat: Vec::with_capacity(t) };
        let (pairs, _) = logits.data().as_chunks::<2>();
        for &[beat, downbeat] in pairs {
            out.beat.push(if self.config.sum_head { beat + downbeat } else { beat });
            out.downbeat.push(downbeat);
        }
        Ok(out)
    }
}

/// `x += branch(x)`.
fn residual(x: &mut Tensor, branch: impl Fn(&Tensor) -> Result<Tensor>) -> Result<()> {
    let delta = branch(x)?;
    ops::add_in_place(x, &delta)
}
