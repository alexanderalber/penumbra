//! Runs the beat tracker with the shipped network and compares the result with reference beat
//! times (text files, one time in seconds per line).
//!
//! `cargo run -p penumbra-audio --release --example beat_file -- spect SPECT.f32 BEATS DOWNBEATS`
//! reads a spectrogram (little-endian f32, 128 values per frame) and runs only the network and
//! the peaks.
//!
//! `cargo run -p penumbra-audio --release --example beat_file -- file SOUND BEATS DOWNBEATS`
//! runs the whole chain from the sound file, without the cache.
//!
//! Both print the time taken, the counts and the F-measure within 20 ms and 70 ms of beats and
//! downbeats, and the bar lengths in beats: between the downbeats the network marks, and of the
//! bars voted from its logits (`beats::bars`). Set `BEAT_MODEL` to another network file to use
//! it instead, and `BEAT_OUT` to a path prefix to write the times found to `PREFIX_beats.txt`,
//! `PREFIX_downs.txt`, `PREFIX_bars.txt` (the voted bar starts) and `PREFIX_logits.txt` (one
//! downbeat logit per beat).

use std::path::{Path, PathBuf};
use std::time::Instant;

use std::collections::BTreeMap;

use penumbra_audio::beats::{self, BeatGrid, BeatModel, Beats, Spectrogram};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [mode, input, b, d] if mode == "spect" || mode == "file" => run(mode, Path::new(input), b, d),
        _ => Err("usage: beat_file spect|file INPUT REF_BEATS REF_DOWNBEATS".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn times(path: &str) -> Result<Vec<f64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    text.split_whitespace().map(|t| t.parse::<f64>().map_err(|e| format!("{path}: {t:?}: {e}"))).collect()
}

fn run(mode: &str, input: &Path, ref_beats: &str, ref_downs: &str) -> Result<(), String> {
    let (want_b, want_d) = (times(ref_beats)?, times(ref_downs)?);
    let start = Instant::now();
    let model = match std::env::var_os("BEAT_MODEL") {
        Some(p) => BeatModel::load(&PathBuf::from(p)),
        None => BeatModel::shipped(),
    }
    .map_err(|e| e.to_string())?;
    println!(
        "model {} loaded in {:.2?}, {} threads",
        model.id(),
        start.elapsed(),
        rayon::current_num_threads()
    );
    let start = Instant::now();
    let got: Beats = if mode == "spect" {
        let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let rows: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let spect = Spectrogram::from_rows(rows);
        println!("spectrogram: {} frames, {:.1} s", spect.frames(), spect.seconds());
        let logits = beats::run_parallel(&spect, &model, None, &mut |_, _| {}).map_err(|e| e.to_string())?;
        beats::pick(&logits.beat, &logits.downbeat)
    } else {
        let mut next = 0.0;
        let mut report = |p: &beats::Progress| {
            if p.fraction() >= next {
                println!(
                    "  {:5.1} s: {:3.0} %, decoded {:.0} s, chunks {}/{}",
                    start.elapsed().as_secs_f64(),
                    100.0 * p.fraction(),
                    p.decoded,
                    p.chunks_done,
                    p.chunks
                );
                next = p.fraction() + 0.1;
            }
        };
        beats::analyze_file(input, &model, None, None, &mut report).map_err(|e| e.to_string())?
    };
    let elapsed = start.elapsed();
    println!("{mode}: {elapsed:.2?}");
    if let Some(prefix) = std::env::var_os("BEAT_OUT") {
        let prefix = prefix.to_string_lossy().into_owned();
        let bars = BeatGrid::from_beats(&got).downbeats();
        let logits: Vec<f64> = got.downbeat_logits.iter().map(|v| f64::from(*v)).collect();
        for (what, t) in
            [("beats", &got.beats), ("downs", &got.downbeats), ("bars", &bars), ("logits", &logits)]
        {
            let text: String = t.iter().map(|t| format!("{t:.4}\n")).collect();
            let path = format!("{prefix}_{what}.txt");
            std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
        }
    }
    for (what, g, w) in [("beats", &got.beats, &want_b), ("downbeats", &got.downbeats, &want_d)] {
        println!(
            "  {what}: {} (reference {}), F20 {:.4}, F70 {:.4}",
            g.len(),
            w.len(),
            beats::f_measure(w, g, 0.02),
            beats::f_measure(w, g, 0.07)
        );
    }
    bar_report("marked downbeats", &got.beats, &got.downbeats);
    bar_report("voted bars", &got.beats, &BeatGrid::from_beats(&got).downbeats());
    Ok(())
}

/// Bars and their lengths in beats between the bar starts `starts` (times of beats).
fn bar_report(what: &str, beats: &[f64], starts: &[f64]) {
    let idx: Vec<usize> = starts.iter().filter_map(|t| beats.iter().position(|b| b == t)).collect();
    let mut lengths: BTreeMap<usize, usize> = BTreeMap::new();
    for (a, b) in idx.iter().zip(idx.iter().skip(1)) {
        *lengths.entry(b - a).or_default() += 1;
    }
    println!(
        "  {what}: {} bars, first at beat {:?}, lengths {lengths:?}",
        idx.len().saturating_sub(1),
        idx.first()
    );
}
