//! The room reverb: decay times against the mapping, silence at the end, no ringing, width,
//! energy, stability under noise with params jumping, cost and every rate.

use std::time::Instant;

use crate::fft::{Complex, Fft};
use crate::reverb::{DEFAULT_DAMPING, DEFAULT_ROOM, Params, Reverb, mix_gains, rt60};

fn block_len(rate: u32) -> usize {
    (rate as usize / 60).max(1)
}

/// The wet impulse response of `seconds`, one vector per output channel.
fn impulse(rate: u32, p: Params, inputs: usize, outputs: usize, seconds: f64) -> Vec<Vec<f64>> {
    let mut r = Reverb::new(rate, inputs, outputs);
    // One silent block first: the mix ramps from the dry sound to `p` there.
    let n = block_len(rate);
    r.process(&vec![vec![0.0; n]; inputs], &mut vec![vec![0.0; n]; outputs], &p);
    let total = (seconds * f64::from(rate)) as usize;
    let n = block_len(rate);
    let mut out = vec![Vec::with_capacity(total); outputs];
    let mut done = 0;
    while done < total {
        let len = n.min(total - done);
        let mut input = vec![vec![0.0; len]; inputs];
        if done == 0 {
            for c in &mut input {
                c[0] = 1.0;
            }
        }
        let mut block = vec![vec![0.0; len]; outputs];
        r.process(&input, &mut block, &p);
        for (o, b) in out.iter_mut().zip(block) {
            o.extend(b);
        }
        done += len;
    }
    out
}

fn wet(room: f64, damping: f64) -> Params {
    Params { mix: 1.0, room, damping }
}

/// T30 from the Schroeder integral: the time from -5 to -35 dB, doubled.
fn t30(h: &[f64], rate: u32) -> f64 {
    let mut edc = vec![0.0; h.len()];
    let mut acc = 0.0;
    for (i, v) in h.iter().enumerate().rev() {
        acc += v * v;
        edc[i] = acc;
    }
    let total = edc[0];
    let at = |db: f64| edc.iter().position(|e| 10.0 * (e / total).log10() <= db).unwrap() as f64;
    2.0 * (at(-35.0) - at(-5.0)) / f64::from(rate)
}

/// A band-pass (RBJ, constant peak gain) at `f0` with `q`.
fn band(x: &[f64], rate: u32, f0: f64, q: f64) -> Vec<f64> {
    let w = std::f64::consts::TAU * f0 / f64::from(rate);
    let alpha = w.sin() / (2.0 * q);
    let a0 = 1.0 + alpha;
    let (b0, b2) = (alpha / a0, -alpha / a0);
    let (a1, a2) = (-2.0 * w.cos() / a0, (1.0 - alpha) / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    x.iter()
        .map(|&v| {
            let y = b0 * v + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, v, y1, y);
            y
        })
        .collect()
}

fn energy(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum()
}

#[test]
fn the_decay_time_follows_room_within_a_fifth() {
    let rate = 48_000;
    for room in [0.0, 0.25, DEFAULT_ROOM, 0.7, 1.0] {
        let want = rt60(room);
        let secs = want * 1.6 + 0.3;
        // Undamped: the broadband decay.
        let h = impulse(rate, wet(room, 0.0), 1, 2, secs);
        let got = t30(&h[0], rate);
        eprintln!("room {room}: RT60 {want:.2} s, undamped T30 {got:.2} s");
        assert!((got / want - 1.0).abs() < 0.2, "room {room}: {got} vs {want}");
        // Damped as by default: the 500 Hz band keeps the decay.
        let h = impulse(rate, wet(room, DEFAULT_DAMPING), 1, 2, secs);
        let got = t30(&band(&h[0], rate, 500.0, 1.4), rate);
        let broad = t30(&h[0], rate);
        let high = t30(&band(&h[0], rate, 6000.0, 1.4), rate);
        eprintln!("  damped: 500 Hz {got:.2} s, broadband {broad:.2} s, 6 kHz {high:.2} s");
        assert!((got / want - 1.0).abs() < 0.2, "room {room}, 500 Hz: {got} vs {want}");
        assert!(high < got * 0.85, "high frequencies die faster: {high} vs {got}");
    }
}

#[test]
fn the_impulse_response_carries_the_energy_of_the_input() {
    for room in [0.0, 0.2, DEFAULT_ROOM, 0.7, 1.0] {
        for damping in [0.0, DEFAULT_DAMPING] {
            let h = impulse(48_000, wet(room, damping), 1, 2, rt60(room) * 2.5 + 0.2);
            for c in &h {
                let db = 10.0 * energy(c).log10();
                // Damping takes energy out of the high frequencies: a little less in all.
                let low = if damping > 0.0 { -3.5 } else { -1.0 };
                assert!(db > low && db < 2.0, "room {room} damping {damping}: {db} dB");
            }
        }
    }
    // The default mix: the reverberant energy about 11 dB below the dry.
    let p = Params::default();
    let h = impulse(48_000, p, 1, 2, 3.0);
    let dry = h[0][0].powi(2);
    let rest = energy(&h[0][1..]);
    let drr = 10.0 * (dry / rest).log10();
    eprintln!("default: dry {:.2} dB over the reverberant", drr);
    assert!((9.0..13.0).contains(&drr), "{drr} dB");
}

#[test]
fn a_tail_ends_in_exact_silence() {
    let rate = 48_000;
    let p = Params { mix: 0.5, room: 0.6, damping: 0.3 };
    let mut r = Reverb::new(rate, 1, 2);
    let n = 800;
    let mut out = Vec::new();
    let mut block = vec![vec![0.0; n]; 2];
    let mut first = vec![0.0; n];
    first[0] = 0.9;
    r.process(&[first], &mut block, &p);
    out.extend_from_slice(&block[0]);
    assert!(!r.is_silent());
    let mut blocks = 0;
    while !r.is_silent() {
        r.process(&[vec![0.0; n]], &mut block, &p);
        out.extend_from_slice(&block[0]);
        blocks += 1;
        assert!(blocks < 60 * 30, "no silence after 30 s");
    }
    let secs = blocks as f64 / 60.0;
    // 0.9 down to -140 dBFS at 60 dB per RT60, plus the buffers.
    let rt = rt60(p.room);
    eprintln!("silent after {secs:.2} s (RT60 {rt:.2} s)");
    assert!(secs < rt * 140.0 / 60.0 + 0.5, "{secs} s");
    // Before the reset the tail was already below -120 dBFS.
    let last = &out[out.len() - 2 * n..out.len() - n];
    assert!(last.iter().all(|v| v.abs() < 1e-6), "{:?}", last.iter().fold(0.0f64, |m, v| m.max(v.abs())));
    for _ in 0..10 {
        r.process(&[vec![0.0; n]], &mut block, &p);
        assert!(block.iter().flatten().all(|v| *v == 0.0));
    }
}

/// Spectral flatness (geometric over arithmetic mean of the power) of the tail in 200..4000 Hz,
/// and its highest bin over the median, dB.
fn flatness(h: &[Vec<f64>], rate: u32, from: usize) -> (f64, f64) {
    let size = 8192;
    let fft = Fft::new(size).unwrap();
    let mut power = vec![0.0; size / 2];
    for c in h {
        for w in 0..2 {
            let start = from + w * size / 2;
            let mut data: Vec<Complex> = (0..size)
                .map(|i| {
                    let hann = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / size as f64).cos();
                    Complex { re: (c[start + i] * hann * 1e3) as f32, im: 0.0 }
                })
                .collect();
            fft.forward(&mut data);
            for (p, d) in power.iter_mut().zip(&data) {
                *p += f64::from(d.re).powi(2) + f64::from(d.im).powi(2);
            }
        }
    }
    let bin = |f: f64| (f * size as f64 / f64::from(rate)) as usize;
    let mut sel: Vec<f64> = power[bin(200.0)..bin(4000.0)].to_vec();
    let geo = (sel.iter().map(|p| p.ln()).sum::<f64>() / sel.len() as f64).exp();
    let mean = sel.iter().sum::<f64>() / sel.len() as f64;
    sel.sort_by(f64::total_cmp);
    let median = sel[sel.len() / 2];
    let max = sel[sel.len() - 1];
    (geo / mean, 10.0 * (max / median).log10())
}

#[test]
fn the_tail_does_not_ring_at_one_frequency() {
    let rate = 48_000;
    for room in [0.0, DEFAULT_ROOM, 1.0] {
        let h = impulse(rate, wet(room, 0.0), 1, 2, 0.5);
        let (flat, peak) = flatness(&h, rate, rate as usize / 20);
        eprintln!("room {room}: flatness {flat:.2}, highest bin {peak:.1} dB over the median");
        // Noise averaged over four periodograms gives about 0.8 and 10 to 12 dB.
        assert!(flat > 0.5, "room {room}: flatness {flat}");
        assert!(peak < 16.0, "room {room}: a peak of {peak} dB");
    }
}

#[test]
fn left_and_right_are_decorrelated_also_from_mono() {
    let rate = 48_000;
    for room in [0.0, DEFAULT_ROOM, 1.0] {
        let h = impulse(rate, wet(room, DEFAULT_DAMPING), 1, 2, 1.0);
        let from = rate as usize / 20;
        let (l, r) = (&h[0][from..], &h[1][from..]);
        let corr = l.iter().zip(r).map(|(a, b)| a * b).sum::<f64>() / (energy(l) * energy(r)).sqrt();
        eprintln!("room {room}: correlation of the tail {corr:.3}");
        assert!(corr.abs() < 0.3, "room {room}: {corr}");
    }
    // A stereo input: each side keeps its own dry sound, and the tails differ as well.
    let h = impulse(rate, Params { mix: 0.5, ..Params::default() }, 2, 2, 0.5);
    assert!((h[0][0] - mix_gains(0.5).0).abs() < 1e-12);
    let from = rate as usize / 20;
    let (l, r) = (&h[0][from..], &h[1][from..]);
    let corr = l.iter().zip(r).map(|(a, b)| a * b).sum::<f64>() / (energy(l) * energy(r)).sqrt();
    assert!(corr.abs() < 0.3, "stereo: {corr}");
}

#[test]
fn mix_zero_is_the_dry_sound_exactly() {
    let mut r = Reverb::new(48_000, 1, 2);
    let p = Params { mix: 0.0, ..Params::default() };
    let x: Vec<f64> = (0..800).map(|i| (i as f64 * 0.37).sin() * 0.8).collect();
    let mut out = vec![vec![0.0; 800]; 2];
    for _ in 0..5 {
        r.process(std::slice::from_ref(&x), &mut out, &p);
        assert_eq!(out[0], x);
        assert_eq!(out[1], x);
    }
    assert_eq!(mix_gains(0.0), (1.0, 0.0));
}

/// A small deterministic noise source.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

#[test]
fn ten_minutes_of_full_scale_noise_with_jumping_params_stay_finite_and_bounded() {
    let rate = 48_000;
    let mut r = Reverb::new(rate, 2, 2);
    let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
    let n = 800;
    let mut input = vec![vec![0.0; n]; 2];
    let mut out = vec![vec![0.0; n]; 2];
    let mut p = Params::default();
    let mut peak = 0.0f64;
    let extremes = [0.0, 1.0, 0.5, f64::NAN, -3.0, 7.0];
    for b in 0..60 * 600 {
        if b % 30 == 0 {
            let pick = |noise: &mut Noise| {
                let u = noise.next();
                if u > 0.6 { extremes[((u * 1000.0) as usize) % extremes.len()] } else { u.abs() }
            };
            p = Params { mix: pick(&mut noise), room: pick(&mut noise), damping: pick(&mut noise) };
        }
        for c in &mut input {
            for v in c.iter_mut() {
                *v = noise.next();
            }
        }
        r.process(&input, &mut out, &p);
        for v in out.iter().flatten() {
            assert!(v.is_finite(), "block {b}");
            peak = peak.max(v.abs());
        }
    }
    eprintln!("peak over 10 min: {peak:.2}");
    assert!(peak < 8.0, "{peak}");
    // And then it falls silent.
    let p = Params { mix: 1.0, room: 1.0, damping: 0.0 };
    let zeros = vec![vec![0.0; n]; 2];
    let mut blocks = 0;
    while !r.is_silent() {
        r.process(&zeros, &mut out, &p);
        blocks += 1;
        assert!(blocks < 60 * 20, "no silence after 20 s");
    }
}

#[test]
fn a_block_costs_well_under_half_a_millisecond() {
    let rate = 48_000;
    let mut noise = Noise(7);
    for inputs in [1, 2] {
        let mut r = Reverb::new(rate, inputs, 2);
        let input: Vec<Vec<f64>> =
            (0..inputs).map(|_| (0..800).map(|_| noise.next() * 0.5).collect()).collect();
        let mut out = vec![vec![0.0; 800]; 2];
        let p = Params::default();
        for _ in 0..60 {
            r.process(&input, &mut out, &p);
        }
        let blocks = 600;
        let t = Instant::now();
        for _ in 0..blocks {
            r.process(&input, &mut out, &p);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(blocks);
        eprintln!("{inputs} in, 2 out: {ms:.3} ms per block of 800 samples");
        assert!(ms < 0.5, "{ms} ms");
    }
}

#[test]
fn every_rate_decays_as_mapped_and_falls_silent() {
    for rate in [1_000, 8_000, 11_025, 22_050, 44_100, 48_000, 96_000, 192_000, 384_000, 768_000] {
        let p = wet(DEFAULT_ROOM, DEFAULT_DAMPING);
        let h = impulse(rate, p, 1, 2, 1.8);
        assert!(h.iter().flatten().all(|v| v.is_finite()), "{rate}");
        if (8_000..=192_000).contains(&rate) {
            let got = t30(&band(&h[0], rate, 500.0, 1.4), rate);
            eprintln!("{rate} Hz: T30 at 500 Hz {got:.2} s");
            assert!((got / rt60(DEFAULT_ROOM) - 1.0).abs() < 0.2, "{rate}: {got}");
        }
        let mut r = Reverb::new(rate, 1, 2);
        let n = block_len(rate);
        let mut block = vec![vec![0.0; n]; 2];
        let mut first = vec![0.0; n];
        first[0] = 1.0;
        r.process(&[first], &mut block, &p);
        let zeros = vec![vec![0.0; n]];
        let mut blocks = 0;
        while !r.is_silent() {
            r.process(&zeros, &mut block, &p);
            blocks += 1;
            assert!(blocks < 60 * 10, "{rate}: no silence after 10 s");
        }
    }
}
