//! Tracks the pitch of a sound file ([`PitchTracker`]) and writes one row per show frame as CSV:
//! time, held note, held Hz, clarity, sure, heard Hz. Prints the time it took.
//!
//! `cargo run -p penumbra-audio --release --example pitch_file -- SOUND OUT.csv [MIN_HZ MAX_HZ]`
//!
//! The channels are averaged, blocks are one frame at 60 fps, the other settings are the
//! defaults.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use penumbra_audio::file::decode_each;
use penumbra_audio::{PitchSettings, PitchTracker};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [sound, out] => run(Path::new(sound), Path::new(out), None),
        [sound, out, lo, hi] => match (lo.parse(), hi.parse()) {
            (Ok(lo), Ok(hi)) => run(Path::new(sound), Path::new(out), Some((lo, hi))),
            _ => Err("MIN_HZ and MAX_HZ are numbers".into()),
        },
        _ => Err("usage: pitch_file SOUND OUT.csv [MIN_HZ MAX_HZ]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(sound: &Path, out: &Path, range: Option<(f64, f64)>) -> Result<(), String> {
    let mut settings = PitchSettings::default();
    if let Some((lo, hi)) = range {
        settings.min_hz = lo;
        settings.max_hz = hi;
    }
    let mut tracker: Option<PitchTracker> = None;
    let mut mono = Vec::new();
    let mut pending: Vec<f32> = Vec::new();
    let mut csv = String::from("seconds,note,hz,clarity,sure,heard_hz\n");
    let mut frames = 0u64;
    let mut busy = Duration::ZERO;
    let mut rate = 0.0;
    let mut error = None;
    decode_each(sound, &mut |info, block| {
        let channels = info.channels.max(1);
        rate = f64::from(info.rate);
        if tracker.is_none() {
            match PitchTracker::new(rate) {
                Ok(t) => tracker = Some(t),
                Err(e) => {
                    error = Some(e);
                    return false;
                }
            }
        }
        let Some(t) = tracker.as_mut() else { return false };
        mono.clear();
        mono.extend(block.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32));
        pending.extend_from_slice(&mono);
        let per_frame = (rate / 60.0).round() as usize;
        while pending.len() >= per_frame {
            let start = Instant::now();
            let p = t.process(pending.get(..per_frame).unwrap_or(&[]), &settings);
            busy += start.elapsed();
            pending.drain(..per_frame);
            frames += 1;
            let _ = writeln!(
                csv,
                "{:.4},{:.3},{:.2},{:.3},{},{:.2}",
                frames as f64 / 60.0,
                p.note,
                p.frequency,
                p.clarity,
                u8::from(p.sure),
                p.heard_frequency
            );
        }
        true
    })?;
    if let Some(e) = error {
        return Err(e);
    }
    std::fs::write(out, csv).map_err(|e| e.to_string())?;
    let seconds = frames as f64 / 60.0;
    println!(
        "{seconds:.1} s of sound at {rate} Hz, tracked in {busy:.2?}: {:.2} ms per second of sound, {:.0}x real time",
        busy.as_secs_f64() * 1000.0 / seconds.max(1e-9),
        seconds / busy.as_secs_f64().max(1e-9)
    );
    Ok(())
}
