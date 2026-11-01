//! Runs Beat This! on one chunk and compares it with a reference.
//!
//! `cargo run -p penumbra-nn --release --example beat_this -- WEIGHTS IO [RUNS] [PARALLEL]`
//!
//! WEIGHTS is a converted checkpoint (tools/beat_this_convert.py). IO is a safetensors file with
//! `spect` (frames x 128) and the reference logits `beat` and `downbeat`. The example prints the
//! model's sizes, the largest logit difference and the time per run. Set `RAYON_NUM_THREADS=1`
//! to time a single thread. With PARALLEL, it then runs that many copies of the chunk at once
//! (as a caller with many chunks would) and prints the time per chunk.

use std::path::PathBuf;
use std::time::Instant;

use penumbra_nn::SafeTensors;
use penumbra_nn::models::BeatThis;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(weights), Some(io)) = (args.first(), args.get(1)) else {
        eprintln!("usage: beat_this WEIGHTS.safetensors IO.safetensors [RUNS] [PARALLEL]");
        std::process::exit(2);
    };
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let parallel: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    if let Err(e) = run(PathBuf::from(weights), PathBuf::from(io), runs, parallel) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(weights: PathBuf, io: PathBuf, runs: usize, parallel: usize) -> penumbra_nn::Result<()> {
    let start = Instant::now();
    let model = BeatThis::load(&weights)?;
    println!("loaded in {:.2?}: {:?}", start.elapsed(), model.config());
    let io = SafeTensors::read(&io)?;
    let spect = io.get("spect")?;
    println!("threads: {}, input {:?}", rayon::current_num_threads(), spect.shape());
    for _ in 0..runs.max(1) {
        let start = Instant::now();
        let out = model.forward(spect)?;
        let elapsed = start.elapsed();
        let diff = |got: &[f32], want: &[f32]| {
            got.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max)
        };
        let beat = diff(&out.beat, io.get("beat")?.data());
        let down = diff(&out.downbeat, io.get("downbeat")?.data());
        println!("forward {elapsed:.2?}: max |diff| beat {beat:.2e}, downbeat {down:.2e}");
    }
    if parallel > 0 {
        use rayon::prelude::*;
        let start = Instant::now();
        (0..parallel)
            .into_par_iter()
            .map(|_| model.forward(spect).map(|_| ()))
            .collect::<penumbra_nn::Result<()>>()?;
        let elapsed = start.elapsed();
        println!("{parallel} chunks at once: {elapsed:.2?}, {:.2?} per chunk", elapsed / parallel as u32);
    }
    Ok(())
}
