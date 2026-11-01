//! Measures scrubbing a video file: what the decoder takes per frame (in order, one back, jumps),
//! then a player scrubbed at 60 Hz by a sine over a span of the clip, and the wait for jumps.
//!
//! `cargo run --release -p penumbra-video --example scrub_file -- FILE [SPAN_SECONDS] [PERIOD_SECONDS]`

use std::time::{Duration, Instant};

use penumbra_video::{Decoder, Limits, Player, Shown, open};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        println!("usage: scrub_file FILE [SPAN_SECONDS] [PERIOD_SECONDS]");
        return;
    };
    let span: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(2.0);
    let period: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(4.0);
    let path = std::path::Path::new(&path);
    let mut d = match open(path, &Limits::default()) {
        Ok(d) => d,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let info = d.info().clone();
    println!("{}x{} {} frames {:.2} fps {}", info.width, info.height, info.frames, info.rate(), info.codec);
    decoder_costs(&mut d, info.frames);
    drop(d);

    let Ok(mut p) = Player::new(path, Limits::default(), None) else { return };
    let duration = info.duration();
    let middle = (duration / 2.0).min(span.max(0.0) + 0.5);
    let start = Instant::now();
    while !matches!(p.scrub(middle, true), Ok(Shown::New(_))) && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("first frame after {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);

    // A sine over `span` seconds around the middle, at 60 Hz in real time.
    let (mut late, mut shown, mut peak) = (0u64, 0u64, 0usize);
    let start = Instant::now();
    let steps = (period * 3.0 * 60.0) as u32;
    for step in 0..steps {
        let now = f64::from(step) / 60.0;
        if let Some(wait) = (start + Duration::from_secs_f64(now)).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let t = middle + span / 2.0 * (std::f64::consts::TAU * now / period).sin();
        match p.scrub(t, true) {
            Ok(Shown::New(_)) => shown += 1,
            Ok(Shown::Late | Shown::Waiting) => late += 1,
            _ => {}
        }
        peak = peak.max(p.cached().0);
    }
    let (bytes, frames) = p.cached();
    println!(
        "sine over {span} s, period {period} s, {steps} steps: {shown} new, {late} steps late; \
         cache {frames} frames, {:.1} MiB (peak {:.1} MiB); seeks {}",
        bytes as f64 / 1048576.0,
        peak as f64 / 1048576.0,
        p.stats().seeks
    );

    // Jumps: the wait until the frame is there.
    let mut waits = Vec::new();
    let mut x: u64 = 99;
    for _ in 0..20 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let t = (x >> 11) as f64 / (1u64 << 53) as f64 * duration;
        let begin = Instant::now();
        loop {
            match p.scrub(t, true) {
                Ok(Shown::New(_) | Shown::Same) => break,
                Err(e) => {
                    println!("{e}");
                    break;
                }
                _ if begin.elapsed() > Duration::from_secs(10) => break,
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        if begin.elapsed() > Duration::from_secs(2) {
            println!(
                "jump to {t:.3} s waited {:?}: cache {:?}, stats {:?}",
                begin.elapsed(),
                p.cached(),
                p.stats()
            );
        }
        waits.push(begin.elapsed().as_secs_f64() * 1000.0);
        // Let the cache fill around it a little, as a show would between jumps.
        std::thread::sleep(Duration::from_millis(200));
    }
    report("jump waits", &waits);
}

fn decoder_costs(d: &mut Box<dyn Decoder>, frames: usize) {
    let n = frames.min(240);
    let mut time = |i: usize| {
        let t = Instant::now();
        let _ = d.frame(i);
        t.elapsed().as_secs_f64() * 1000.0
    };
    let seq: Vec<f64> = (0..n).map(&mut time).collect();
    report("in order", &seq);
    let back: Vec<f64> = (0..n).rev().map(&mut time).collect();
    report("one back each", &back);
    let mut x: u64 = 12345;
    let jumps: Vec<f64> = (0..40)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            time((x >> 33) as usize % frames.max(1))
        })
        .collect();
    report("random jumps", &jumps);
}

fn report(what: &str, v: &[f64]) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let q = |p: f64| s.get(((s.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0.0);
    println!("{what:>14}: median {:.2} ms, 90 % {:.2}, max {:.2}, n {}", q(0.5), q(0.9), q(1.0), s.len());
}
