//! Beat This! against PyTorch on a tiny model with random weights (tools/beat_this_fixture.py
//! writes the fixtures), and the real checkpoints when they are available.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use penumbra_nn::models::BeatThis;
use penumbra_nn::{SafeTensors, Tensor};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Runs the model on `io`'s spectrogram; returns the largest logit difference to its reference.
fn compare(model: &BeatThis, io: &SafeTensors) -> f32 {
    let spect = io.get("spect").unwrap();
    let out = model.forward(spect).unwrap();
    let beat = max_diff(&out.beat, io.get("beat").unwrap().data());
    let down = max_diff(&out.downbeat, io.get("downbeat").unwrap().data());
    beat.max(down)
}

#[test]
fn tiny_model_matches_pytorch() {
    let model = BeatThis::load(&fixture("beat_this_tiny.safetensors")).unwrap();
    let c = model.config();
    assert_eq!((c.spect_dim, c.stem_dim, c.transformer_dim), (128, 8, 32));
    assert_eq!((c.n_layers, c.n_heads, c.head_dim, c.ff_dim), (2, 4, 8, 128));
    assert!(c.partial_transformers && c.sum_head);
    let io = SafeTensors::read(&fixture("beat_this_tiny_io.safetensors")).unwrap();
    let diff = compare(&model, &io);
    println!("tiny model: max |logit diff| {diff:.2e}");
    assert!(diff < 1e-4, "max |logit diff| {diff}");
}

#[test]
fn tiny_model_takes_any_length() {
    let model = BeatThis::load(&fixture("beat_this_tiny.safetensors")).unwrap();
    for frames in [1, 2, 63, 64, 65, 200] {
        let spect = Tensor::new(vec![frames, 128], vec![1.0; frames * 128]).unwrap();
        let out = model.forward(&spect).unwrap();
        assert_eq!((out.beat.len(), out.downbeat.len()), (frames, frames));
        assert!(out.beat.iter().chain(&out.downbeat).all(|v| v.is_finite()));
    }
    assert!(model.forward(&Tensor::zeros(vec![0, 128]).unwrap()).is_err());
    assert!(model.forward(&Tensor::zeros(vec![10, 127]).unwrap()).is_err());
    assert!(model.forward(&Tensor::zeros(vec![10, 128, 1]).unwrap()).is_err());
}

#[test]
fn missing_or_misshapen_weights_are_errors() {
    let full = SafeTensors::read(&fixture("beat_this_tiny.safetensors")).unwrap();
    // Without any one tensor, loading fails.
    for name in full.names() {
        let mut st = full.clone();
        st.remove(name);
        assert!(BeatThis::from_safetensors(&st).is_err(), "loaded without {name}");
    }
    // With any one tensor reshaped or one value longer, loading fails or the model still runs;
    // it never panics.
    let spect = Tensor::new(vec![20, 128], vec![1.0; 20 * 128]).unwrap();
    for name in full.names() {
        let t = full.get(name).unwrap();
        for shape in [vec![t.len() + 1], vec![t.len()], vec![1, t.len()]] {
            let mut st = full.clone();
            let n = shape.iter().product();
            st.insert(name, Tensor::new(shape, vec![0.5; n]).unwrap());
            if let Ok(model) = BeatThis::from_safetensors(&st) {
                let _ = model.forward(&spect);
            }
        }
    }
}

/// The real checkpoints, converted by tools/beat_this_convert.py, against PyTorch logits for
/// one chunk. Set `BEAT_THIS_DIR` to a folder with `beat_this-{small0,final0}.safetensors` and
/// `{small0,final0}_io.safetensors`, then run `cargo test -p penumbra-nn --release -- --ignored`.
#[test]
#[ignore]
fn real_checkpoints_match_pytorch() {
    let Some(dir) = std::env::var_os("BEAT_THIS_DIR") else {
        println!("BEAT_THIS_DIR is not set");
        return;
    };
    let dir = PathBuf::from(dir);
    for name in ["small0", "final0"] {
        let model = BeatThis::load(&dir.join(format!("beat_this-{name}.safetensors"))).unwrap();
        let io = SafeTensors::read(&dir.join(format!("{name}_io.safetensors"))).unwrap();
        let start = std::time::Instant::now();
        let diff = compare(&model, &io);
        println!("{name}: {:?}, max |logit diff| {diff:.2e}, {:.2?}", model.config(), start.elapsed());
        assert!(diff < 1e-3, "{name}: max |logit diff| {diff}");
    }
}
