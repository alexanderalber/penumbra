//! The resampler: response of the kernel, tones through the common rate pairs, blocks of
//! random length, drift, hostile input, and its cost.

use crate::resample::{BASE_TAPS, GLIDE_SECONDS, Kernel, MAX_DRIFT, PHASES, Resampler};

fn sine(freq: f64, rate: f64, n: usize, amp: f64) -> Vec<f64> {
    (0..n).map(|i| amp * (std::f64::consts::TAU * freq * i as f64 / rate).sin()).collect()
}

/// A small deterministic random generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Converts one channel in blocks of the given lengths (the rest as one last block).
fn convert(from: f64, to: f64, x: &[f64], blocks: &mut dyn FnMut() -> usize) -> Vec<f64> {
    let mut r = Resampler::new(from, to, 1).unwrap();
    let mut out = vec![Vec::new()];
    let mut at = 0;
    while at < x.len() {
        let n = blocks().min(x.len() - at);
        r.process(&[&x[at..at + n]], &mut out).unwrap();
        at += n;
    }
    out.pop().unwrap()
}

/// Least-squares fit of `a sin + b cos + c` at `freq`: (amplitude, RMS of what is left).
fn fit(y: &[f64], freq: f64, rate: f64) -> (f64, f64) {
    let w = std::f64::consts::TAU * freq / rate;
    // Normal equations for [sin, cos, 1].
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (i, &s) in y.iter().enumerate() {
        let b = [(w * i as f64).sin(), (w * i as f64).cos(), 1.0];
        for r in 0..3 {
            v[r] += b[r] * s;
            for c in 0..3 {
                m[r][c] += b[r] * b[c];
            }
        }
    }
    let p = solve3(m, v);
    let mut res = 0.0;
    for (i, &s) in y.iter().enumerate() {
        let e = s - (p[0] * (w * i as f64).sin() + p[1] * (w * i as f64).cos() + p[2]);
        res += e * e;
    }
    ((p[0] * p[0] + p[1] * p[1]).sqrt(), (res / y.len() as f64).sqrt())
}

#[allow(clippy::needless_range_loop)]
fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> [f64; 3] {
    for i in 0..3 {
        let p = m[i][i];
        for c in 0..3 {
            m[i][c] /= p;
        }
        v[i] /= p;
        for r in 0..3 {
            if r != i {
                let f = m[r][i];
                for c in 0..3 {
                    m[r][c] -= f * m[i][c];
                }
                v[r] -= f * v[i];
            }
        }
    }
    v
}

/// The frequency of a tone near `guess`, from the phase drift between the two halves.
fn frequency(y: &[f64], guess: f64, rate: f64) -> f64 {
    let half = y.len() / 2;
    let phase = |s: &[f64], offset: usize| {
        let w = std::f64::consts::TAU * guess / rate;
        let (mut a, mut b) = (0.0, 0.0);
        for (i, v) in s.iter().enumerate() {
            let t = w * (i + offset) as f64;
            a += v * t.sin();
            b += v * t.cos();
        }
        b.atan2(a)
    };
    let p0 = phase(&y[..half], 0);
    let p1 = phase(&y[half..], half);
    let mut d = p1 - p0;
    while d > std::f64::consts::PI {
        d -= std::f64::consts::TAU;
    }
    while d < -std::f64::consts::PI {
        d += std::f64::consts::TAU;
    }
    // The phase is measured against the guess; a drift of d over half the length.
    guess + d / std::f64::consts::TAU * rate / half as f64
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// The kernel's response at `f` cycles per source sample, from all its phases.
fn response(k: &Kernel, rows: &[Vec<f32>], f: f64) -> f64 {
    let half = (k.taps() / 2) as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (p, row) in rows.iter().enumerate() {
        let frac = p as f64 / PHASES as f64;
        for (i, &h) in row.iter().enumerate() {
            let t = i as f64 - (half - 1.0) - frac;
            let a = std::f64::consts::TAU * f * t;
            re += f64::from(h) * a.cos();
            im -= f64::from(h) * a.sin();
        }
    }
    (re * re + im * im).sqrt() / PHASES as f64
}

/// Passband flat within 0.1 dB to 0.4535 of the lower rate, stopband 80 dB down from 0.5465,
/// for the rate pairs that matter.
#[test]
fn the_kernel_is_flat_in_the_band_and_quiet_beyond() {
    for (from, to) in [
        (44_100.0, 48_000.0),
        (48_000.0, 44_100.0),
        (96_000.0, 48_000.0),
        (48_000.0, 96_000.0),
        (48_000.0, 48_000.0),
        (192_000.0, 44_100.0),
    ] {
        let k = Kernel::new(from, to);
        let rows: Vec<Vec<f32>> = (0..PHASES)
            .map(|p| {
                let mut c = vec![0.0; k.taps()];
                k.coefficients(p as f64 / PHASES as f64, &mut c);
                c
            })
            .collect();
        let scale = (to / from).min(1.0);
        let mut ripple: f64 = 0.0;
        for i in 0..=200 {
            let f = 0.4535 * scale * f64::from(i) / 200.0;
            ripple = ripple.max(db(response(&k, &rows, f)).abs());
        }
        let mut stop = f64::NEG_INFINITY;
        for i in 0..=3000 {
            let f = 0.5465 * scale + (4.0 - 0.5465 * scale) * f64::from(i) / 3000.0;
            stop = stop.max(db(response(&k, &rows, f)));
        }
        println!("{from} -> {to}: {} taps, passband within {ripple:.4} dB, stopband {stop:.1} dB", k.taps());
        assert!(ripple < 0.1, "{from} -> {to}: ripple {ripple} dB");
        assert!(stop < -80.0, "{from} -> {to}: stopband {stop} dB");
    }
}

/// Tones through 44.1 -> 48, 48 -> 96, 96 -> 48, 48 -> 44.1 kHz in blocks of random length:
/// the right frequency and level, THD+N below -80 dB.
#[test]
fn tones_keep_frequency_level_and_purity() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (from, to) in [(44_100.0, 48_000.0), (48_000.0, 96_000.0), (96_000.0, 48_000.0), (48_000.0, 44_100.0)]
    {
        let band = 0.4535 * f64::min(from, to);
        for freq in [50.0, 1000.0, 9_000.0, 19_000.0, band.min(20_000.0)] {
            let x = sine(freq, from, (from * 1.5) as usize, 0.5);
            let y = convert(from, to, &x, &mut || 1 + rng.below(1500));
            let expected = (x.len() as f64 * to / from).ceil() as usize;
            assert!((y.len() as i64 - expected as i64).abs() <= 1, "{} vs {expected}", y.len());
            // Past the start, the kernel's rise.
            let tail = &y[(to * 0.1) as usize..];
            let (amp, res) = fit(tail, freq, to);
            let f = frequency(tail, freq, to);
            let thdn = db(res / (amp / 2f64.sqrt()));
            println!(
                "{from} -> {to}, {freq} Hz: level {:+.4} dB, THD+N {thdn:.1} dB, {f:.4} Hz",
                db(amp / 0.5)
            );
            assert!(db(amp / 0.5).abs() < 0.1, "{from} -> {to}, {freq} Hz: level {} dB", db(amp / 0.5));
            assert!(thdn < -80.0, "{from} -> {to}, {freq} Hz: THD+N {thdn} dB");
            assert!((f - freq).abs() < 1e-3 * freq.max(1.0) / 100.0, "{from} -> {to}: {f} Hz for {freq}");
        }
    }
}

/// A tone above the lower rate's band is gone, so it cannot alias into the band.
#[test]
fn a_tone_beyond_the_band_does_not_alias() {
    for (from, to, freq) in
        [(96_000.0, 48_000.0, 30_000.0), (96_000.0, 44_100.0, 26_000.0), (192_000.0, 48_000.0, 60_000.0)]
    {
        let x = sine(freq, from, from as usize, 0.5);
        let y = convert(from, to, &x, &mut || 4096);
        let tail = &y[(to * 0.1) as usize..];
        let rms = (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt();
        let level = db(rms / (0.5 / 2f64.sqrt()));
        println!("{from} -> {to}, {freq} Hz: {level:.1} dB");
        assert!(level < -80.0, "{from} -> {to}, {freq} Hz leaks at {level} dB");
    }
}

/// Blocks of random length give the same samples as one block: nothing happens at an edge.
#[test]
fn block_edges_leave_no_trace() {
    let mut rng = Rng(12345);
    for (from, to) in [(44_100.0, 48_000.0), (96_000.0, 48_000.0), (48_000.0, 48_000.0), (22_050.0, 37_111.0)]
    {
        let x = sine(3_000.0, from, 30_000, 0.7);
        let whole = convert(from, to, &x, &mut || usize::MAX);
        let pieces = convert(from, to, &x, &mut || rng.below(700));
        assert_eq!(whole, pieces, "{from} -> {to}");
        // And the curve is smooth: no step larger than the tone's steepest slope allows.
        let slope = std::f64::consts::TAU * 3_000.0 / to * 0.7 * 1.01;
        for w in pieces.windows(2).skip(200) {
            assert!((w[1] - w[0]).abs() <= slope, "{from} -> {to}: step {}", (w[1] - w[0]).abs());
        }
    }
}

/// A constant stays exactly constant; the output lags by half the kernel.
#[test]
fn a_constant_stays_constant_and_the_delay_is_half_the_kernel() {
    let mut r = Resampler::new(44_100.0, 48_000.0, 2).unwrap();
    assert_eq!(r.taps(), BASE_TAPS);
    assert!((r.delay_seconds() - 32.0 / 44_100.0).abs() < 1e-12);
    assert_eq!(Resampler::new(96_000.0, 48_000.0, 1).unwrap().taps(), 128);
    let x = vec![0.25; 4000];
    let mut out = vec![Vec::new(), Vec::new()];
    r.process(&[&x, &x], &mut out).unwrap();
    for v in &out[0][100..] {
        assert!((v - 0.25).abs() < 1e-6, "{v}");
    }
    // An impulse comes out at the delay.
    let mut r = Resampler::new(48_000.0, 48_000.0, 1).unwrap();
    let mut x = vec![0.0; 200];
    x[10] = 1.0;
    let mut out = vec![Vec::new()];
    r.process(&[&x], &mut out).unwrap();
    let peak = out[0].iter().enumerate().fold((0, 0.0), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
    assert_eq!(peak.0, 10 + BASE_TAPS / 2, "{peak:?}");
    assert!((peak.1 - 1.0).abs() < 1e-6);
}

/// A drifting ratio glides: the counts follow it, the curve stays smooth, the pitch moves.
#[test]
fn the_ratio_glides_without_a_step() {
    let (from, to) = (48_000.0, 44_100.0);
    let mut r = Resampler::new(from, to, 1).unwrap();
    let x = sine(1_000.0, from, 48_000 * 4, 0.5);
    let mut out = vec![Vec::new()];
    let nominal = r.nominal_ratio();
    let mut rng = Rng(7);
    let mut at = 0;
    let mut wanted = nominal;
    let mut expected = 0.0;
    while at < x.len() {
        let n = (100 + rng.below(1500)).min(x.len() - at);
        // A device clock wandering by up to 500 ppm.
        if at > 48_000 {
            wanted = nominal * (1.0 + 500e-6 * ((at as f64 / 20_000.0).sin()));
            r.set_ratio(wanted);
        }
        let made = r.process(&[&x[at..at + n]], &mut out).unwrap();
        expected += n as f64 * r.ratio();
        assert!((r.ratio() / wanted - 1.0).abs() < 600e-6);
        at += n;
        let _ = made;
    }
    let y = &out[0];
    assert!((y.len() as f64 - expected).abs() < 50.0, "{} vs {expected}", y.len());
    let slope = std::f64::consts::TAU * 1_000.0 / to * 0.5 * 1.01;
    let mut worst: f64 = 0.0;
    for w in y.windows(3).skip(200) {
        assert!((w[1] - w[0]).abs() <= slope);
        // The second difference of a sine is bounded too: no kink.
        worst = worst.max((w[2] - 2.0 * w[1] + w[0]).abs());
    }
    let bound = (std::f64::consts::TAU * 1_000.0 / to).powi(2) * 0.5 * 1.01;
    assert!(worst <= bound, "{worst} > {bound}");
    // A big jump is clamped to the allowed drift and glides there.
    let mut r = Resampler::new(48_000.0, 48_000.0, 1).unwrap();
    r.set_ratio(2.0);
    let mut out = vec![Vec::new()];
    r.process(&[&vec![0.0; 48_000]], &mut out).unwrap();
    assert!((r.ratio() - (1.0 + MAX_DRIFT)).abs() < 1e-9, "{}", r.ratio());
    // The glide takes its time: half way after half the glide.
    let mut r = Resampler::new(48_000.0, 48_000.0, 1).unwrap();
    r.set_ratio(1.01);
    let half = (GLIDE_SECONDS * 48_000.0 / 2.0) as usize;
    r.process(&[&vec![0.0; half + BASE_TAPS]], &mut [Vec::new()]).unwrap();
    let step = 1.0 / r.ratio();
    assert!(step < 1.0 && step > 1.0 / 1.01, "{step}");
}

#[test]
fn hostile_input_stays_finite() {
    for (a, b) in
        [(0.0, 48_000.0), (f64::NAN, 48_000.0), (48_000.0, f64::INFINITY), (1e9, 48_000.0), (500.0, 48_000.0)]
    {
        assert!(Resampler::new(a, b, 1).is_err(), "{a} {b}");
    }
    assert!(Resampler::new(48_000.0, 48_000.0, 0).is_err());
    assert!(Resampler::new(48_000.0, 48_000.0, 65).is_err());
    let mut r = Resampler::new(768_000.0, 1_000.0, 2).unwrap();
    let mut out = vec![Vec::new(), Vec::new()];
    assert_eq!(r.process(&[&[], &[]], &mut out).unwrap(), 0);
    let bad = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300, f64::MIN_POSITIVE];
    let x: Vec<f64> = (0..100_000).map(|i| bad[i % bad.len()]).collect();
    r.process(&[&x, &x], &mut out).unwrap();
    assert!(!out[0].is_empty());
    assert!(out.iter().flatten().all(|v| v.is_finite()));
    assert!(r.process(&[&x], &mut out).is_err());
    assert!(r.process(&[&x, &x[1..]], &mut out).is_err());
    assert!(r.process(&[&x, &x], &mut out[..1]).is_err());
    // Odd rates both ways, tiny blocks.
    for (from, to) in [(1_000.0, 768_000.0), (768_000.0, 1_000.0), (11_025.0, 8_000.0)] {
        let mut r = Resampler::new(from, to, 1).unwrap();
        let mut out = vec![Vec::new()];
        let x = sine(100.0, from, 5_000, 0.5);
        for c in x.chunks(3) {
            r.process(&[c], &mut out).unwrap();
        }
        let expected = (x.len() as f64 * to / from).ceil();
        assert!(
            (out[0].len() as f64 - expected).abs() <= 1.0,
            "{from} -> {to}: {} vs {expected}",
            out[0].len()
        );
        assert!(out[0].iter().all(|v| v.is_finite() && v.abs() < 1.0));
    }
    r.set_ratio(f64::NAN);
    r.set_ratio(-1.0);
    r.reset();
}

/// The cost per channel and second of sound. Run with
/// `cargo test --release -p penumbra-audio measure_resampler_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_resampler_cost() {
    for (from, to) in [
        (44_100.0, 48_000.0),
        (48_000.0, 44_100.0),
        (48_000.0, 96_000.0),
        (96_000.0, 48_000.0),
        (48_000.0, 48_000.0),
    ] {
        for channels in [1usize, 2, 8] {
            let x = sine(1_000.0, from, from as usize / 60, 0.5);
            let inputs: Vec<&[f64]> = (0..channels).map(|_| x.as_slice()).collect();
            let mut r = Resampler::new(from, to, channels).unwrap();
            let mut out = vec![Vec::new(); channels];
            let seconds = 20.0;
            let t = std::time::Instant::now();
            for _ in 0..(seconds * 60.0) as usize {
                for o in &mut out {
                    o.clear();
                }
                r.process(std::hint::black_box(&inputs), &mut out).unwrap();
            }
            let us = t.elapsed().as_secs_f64() * 1e6 / seconds / channels as f64;
            eprintln!(
                "{from} -> {to}, {channels} ch, {} taps: {us:.0} us per channel per second of sound",
                r.taps()
            );
        }
    }
}
