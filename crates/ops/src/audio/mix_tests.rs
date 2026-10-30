//! `audio.mix` with inputs at different rates and on a device clock.

use penumbra_audio::Hub;
use penumbra_core::Chans;
use penumbra_graph::Port;

use super::tests::{F, connect, create, set};

fn sine_osc(path: &str, rate: i64, frequency: f64, amplitude: f64) -> Vec<penumbra_graph::Op> {
    vec![
        create(path, "audio.oscillator"),
        set(&format!("{path}.shape"), "sine"),
        set(&format!("{path}.frequency"), frequency),
        set(&format!("{path}.amplitude"), amplitude),
        set(&format!("{path}.rate"), rate),
    ]
}

/// Amplitude of the tone at `freq` in `y`, by projection (exact for a whole number of
/// periods of every tone in it).
fn amplitude(y: &[f64], freq: f64, rate: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / rate;
    let (mut a, mut b) = (0.0, 0.0);
    for (i, v) in y.iter().enumerate() {
        a += v * (w * i as f64).sin();
        b += v * (w * i as f64).cos();
    }
    2.0 * (a * a + b * b).sqrt() / y.len() as f64
}

#[allow(clippy::needless_range_loop)]
/// Least-squares fit of tones at `freqs` (and a constant): the RMS of what is left.
fn residual(y: &[f64], freqs: &[f64], rate: f64) -> f64 {
    let n = 2 * freqs.len() + 1;
    let basis = |i: usize| -> Vec<f64> {
        let mut b = Vec::with_capacity(n);
        for f in freqs {
            let w = std::f64::consts::TAU * f / rate * i as f64;
            b.push(w.sin());
            b.push(w.cos());
        }
        b.push(1.0);
        b
    };
    let mut m = vec![vec![0.0; n]; n];
    let mut v = vec![0.0; n];
    for (i, s) in y.iter().enumerate() {
        let b = basis(i);
        for r in 0..n {
            v[r] += b[r] * s;
            for c in 0..n {
                m[r][c] += b[r] * b[c];
            }
        }
    }
    for i in 0..n {
        let p = m[i][i];
        for c in 0..n {
            m[i][c] /= p;
        }
        v[i] /= p;
        for r in 0..n {
            if r != i {
                let f = m[r][i];
                for c in 0..n {
                    m[r][c] -= f * m[i][c];
                }
                v[r] -= f * v[i];
            }
        }
    }
    let mut res = 0.0;
    for (i, s) in y.iter().enumerate() {
        let b = basis(i);
        let e = s - b.iter().zip(&v).map(|(x, p)| x * p).sum::<f64>();
        res += e * e;
    }
    (res / y.len() as f64).sqrt()
}

/// The frequency of a tone near `guess`, from the phase drift between the two halves.
fn frequency(y: &[f64], guess: f64, rate: f64) -> f64 {
    let half = y.len() / 2;
    let w = std::f64::consts::TAU * guess / rate;
    let phase = |s: &[f64], offset: usize| {
        let (mut a, mut b) = (0.0, 0.0);
        for (i, v) in s.iter().enumerate() {
            let t = w * (i + offset) as f64;
            a += v * t.sin();
            b += v * t.cos();
        }
        b.atan2(a)
    };
    let mut d = phase(&y[half..], half) - phase(&y[..half], 0);
    d = (d + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    guess + d / std::f64::consts::TAU * rate / half as f64
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// A 96 kHz and a 48 kHz tone mixed: both at their frequency and level, at the first input's
/// rate or at the one `rate` asks for, without gaps.
#[test]
fn a_96_and_a_48_khz_input_mix_at_one_rate() {
    for (asked, rate) in [(0_i64, 96_000.0), (48_000, 48_000.0), (44_100, 44_100.0)] {
        let mut ops = sine_osc("/a", 96_000, 1_000.0, 0.5);
        ops.extend(sine_osc("/b", 48_000, 3_000.0, 0.25));
        ops.extend([
            create("/m", "audio.mix"),
            set("/m.rate", asked),
            connect("/a", "/m", 0),
            connect("/b", "/m", 1),
        ]);
        let mut f = F::new(&ops);
        let mut all = Vec::new();
        for _ in 0..120 {
            f.clock.advance(1);
            let c = f.cook("/m").unwrap();
            assert_eq!(c.rate(), rate);
            all.extend(c.channels()[0].samples.iter().copied());
        }
        // Past the first frames; a whole number of periods of both tones.
        let start = (rate * 0.2) as usize;
        let len = (rate * 1.0) as usize;
        let tail = &all[start..start + len];
        let a = amplitude(tail, 1_000.0, rate);
        let b = amplitude(tail, 3_000.0, rate);
        let res = residual(tail, &[1_000.0, 3_000.0], rate);
        println!(
            "rate {rate}: 1 kHz at {:+.3} dB, 3 kHz at {:+.3} dB, rest {:.1} dB",
            db(a / 0.5),
            db(b / 0.25),
            db(res / 0.25)
        );
        assert!(db(a / 0.5).abs() < 0.05, "{rate}: {a}");
        assert!(db(b / 0.25).abs() < 0.05, "{rate}: {b}");
        // No gap, no click: what is left is tiny.
        assert!(db(res / 0.25) < -70.0, "{rate}: rest {} dB", db(res / 0.25));
        let id = f.net.resolve(&"/m".parse().unwrap()).unwrap();
        assert_eq!(f.cooker.status(id).and_then(|s| s.warning), None);
    }
}

/// Two inputs on the show clock at the mix's rate are added sample for sample, with no delay.
#[test]
fn inputs_in_step_are_added_without_delay() {
    let mut ops = sine_osc("/a", 48_000, 1_000.0, 0.5);
    ops.extend(sine_osc("/b", 48_000, 1_000.0, 0.25));
    ops.extend([create("/m", "audio.mix"), connect("/a", "/m", 0), connect("/b", "/m", 1)]);
    let mut f = F::new(&ops);
    for _ in 0..5 {
        f.clock.advance(1);
        let a = f.block("/a");
        let m = f.block("/m");
        for (x, y) in a.iter().zip(&m) {
            assert!((x * 1.5 - y).abs() < 1e-12);
        }
    }
    // Paused, the second input cooked again with another amplitude: its new block replaces
    // the old one in the mix, which stays in step.
    for step in [0, 1, 1] {
        if step == 0 {
            f.edit(&[set("/b.amplitude", 0.5)]);
        }
        f.clock.advance(step);
        let a = f.block("/a");
        let b = f.block("/b");
        let m = f.block("/m");
        for ((x, y), z) in a.iter().zip(&b).zip(&m) {
            assert!((x + y - z).abs() < 1e-12, "{step}");
        }
    }
}

/// A device at 96 kHz on its own clock (300 ppm fast, 10 ms buffers arriving with jitter) into
/// a 48 kHz mix on the show clock: gapless after the start, the tone at its level.
#[test]
fn a_jittery_device_mixes_without_gaps() {
    for ppm in [300.0, -300.0, 0.0] {
        let mut ops = sine_osc("/a", 48_000, 1_000.0, 0.0);
        ops.extend([
            create("/in", "audio.device_in"),
            set("/in.device", "test"),
            create("/m", "audio.mix"),
            connect("/a", "/m", 0),
            connect("/in", "/m", 1),
        ]);
        let mut f = F::new(&ops);
        f.cooker.provide(Hub::new());
        let hub = f.cooker.resource::<Hub>().unwrap().clone();
        let id = f.net.resolve(&"/m".parse().unwrap()).unwrap();
        let device_rate = 96_000.0 * (1.0 + ppm * 1e-6);
        let freq = 2_000.0;
        let mut sent = 0usize;
        let mut rng = 0x2545_f491_4f6c_dd1d_u64;
        let mut out: Vec<f64> = Vec::new();
        let mut warned_late = Vec::new();
        let frames = 60 * 30;
        for frame in 1..=frames {
            // The device's buffers that have arrived by now, late by up to 4 ms.
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let jitter = (rng % 4000) as f64 * 1e-6;
            let now = f64::from(frame) / 60.0 - jitter;
            let due = ((now * device_rate) / 960.0).floor().max(0.0) as usize * 960;
            if due > sent {
                let block: Vec<f32> = (sent..due)
                    .map(|n| (0.5 * (std::f64::consts::TAU * freq * n as f64 / 96_000.0).sin()) as f32)
                    .collect();
                hub.feed("test", 96_000, 1, &block).unwrap();
                sent = due;
            }
            f.clock.advance(1);
            f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
            let status = f.cooker.status(id).unwrap();
            assert_eq!(status.error, None);
            if frame > 120
                && let Some(w) = status.warning
            {
                warned_late.push((frame, w));
            }
            let data = f.cooker.output(Port { node: id, output: 0 }).unwrap();
            out.extend(data.get::<Chans>().unwrap().channels()[0].samples.iter().copied());
        }
        assert!(warned_late.is_empty(), "{ppm} ppm: {:?}", warned_late.first());
        // The tone as the device's clock plays it, at the show's 48 kHz; while the drift loop
        // settles, a little off.
        let heard = freq * (1.0 + ppm * 1e-6);
        let mut pitch: f64 = 0.0;
        for (k, s) in out[48_000 * 2..].as_chunks::<48_000>().0.iter().enumerate() {
            let fk = frequency(s, heard, 48_000.0);
            if k % 5 == 0 {
                println!("{ppm} ppm, second {}: pitch {:+.0} ppm off", k + 2, (fk / heard - 1.0) * 1e6);
            }
            pitch = pitch.max((fk / heard - 1.0).abs());
        }
        // Gapless: every 10 ms (20 periods) at the tone's level, and no step or kink.
        let mut level: f64 = 0.0;
        for s in out[48_000 * 2..].as_chunks::<480>().0.iter() {
            let rms = (s.iter().map(|v| v * v).sum::<f64>() / s.len() as f64).sqrt();
            level = level.max(db(rms / (0.5 / 2f64.sqrt())).abs());
        }
        let w = std::f64::consts::TAU * heard / 48_000.0;
        let kink = out[48_000 * 2..].windows(3).map(|s| (s[2] - 2.0 * s[1] + s[0]).abs()).fold(0.0, f64::max);
        println!(
            "{ppm} ppm: level within {level:.3} dB, largest second difference {:.4} of a sine's",
            kink / (w * w * 0.5)
        );
        assert!(level < 0.1, "{ppm} ppm: level off by {level} dB");
        assert!(kink < w * w * 0.5 * 1.01, "{ppm} ppm: a kink of {kink}");
        // The drift loop moves the pitch by at most its limit, 2000 ppm; here far less.
        assert!(pitch < 1e-3, "{ppm} ppm: pitch off by {} ppm", pitch * 1e6);
        // The output follows the show clock: one frame's worth per frame.
        assert_eq!(out.len(), frames as usize * 800);
    }
}

/// A device that delivered nothing: the mix hands on its last block, and nothing downstream
/// hears it twice.
#[test]
fn a_repeating_first_input_hands_on_the_same_block() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/m", "audio.mix"),
        connect("/in", "/m", 0),
    ]);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    let id = f.net.resolve(&"/m".parse().unwrap()).unwrap();
    hub.feed("test", 44_100, 2, &[0.0; 16]).unwrap();
    f.clock.advance(1);
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    hub.feed("test", 44_100, 2, &[0.25; 1000]).unwrap();
    f.clock.advance(1);
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    let a = f.cooker.output(Port { node: id, output: 0 }).unwrap().clone();
    assert_eq!(a.get::<Chans>().unwrap().len(), 500);
    assert_eq!(a.get::<Chans>().unwrap().rate(), 44_100.0);
    f.edit(&[set("/m.gain", 0.5)]);
    f.clock.advance(1);
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    let b = f.cooker.output(Port { node: id, output: 0 }).unwrap();
    assert!(a.same(b));
}

/// Non-numbers, huge values and a device at an odd rate stay finite; an empty mix waits.
#[test]
fn hostile_input_stays_finite() {
    let mut ops = sine_osc("/a", 48_000, 440.0, 0.5);
    ops.extend([
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/m", "audio.mix"),
        connect("/a", "/m", 0),
        connect("/in", "/m", 1),
    ]);
    let mut f = F::new(&ops);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    let id = f.net.resolve(&"/m".parse().unwrap()).unwrap();
    let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.5];
    for (k, rate) in [11_025_u32, 11_025, 7_000, 1_000_000, 192_000, 192_000].into_iter().enumerate() {
        let block: Vec<f32> = (0..1500).map(|i| bad[(i + k) % bad.len()]).collect();
        hub.feed("test", rate, 1, &block).unwrap();
        f.clock.advance(1);
        f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
        if let Some(d) = f.cooker.output(Port { node: id, output: 0 }) {
            assert!(
                d.get::<Chans>().unwrap().channels().iter().flat_map(|c| &c.samples).all(|v| v.is_finite())
            );
        }
    }
    let mut f = F::new(&[create("/m", "audio.mix")]);
    let e = f.cook("/m").unwrap_err();
    assert!(e.contains("connect sound"), "{e}");
}
