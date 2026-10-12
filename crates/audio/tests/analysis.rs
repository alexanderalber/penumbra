//! The analysis against synthetic signals with known levels, onsets and tempo.

use penumbra_audio::{Analyzer, Features, Settings};

/// A small deterministic noise source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// A house-like loop: a kick on every beat, a hi-hat between, a clap on beats 2 and 4, a bass
/// note per bar that changes on the downbeat. The tempo may change over time. Returns the
/// samples and the beat times.
fn house(rate: f64, seconds: f64, bpm: impl Fn(f64) -> f64, start: f64) -> (Vec<f32>, Vec<f64>) {
    let n = (rate * seconds) as usize;
    let mut out = vec![0.0f32; n];
    let mut beats = Vec::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut t = start;
    let mut k = 0usize;
    while t < seconds {
        beats.push(t);
        let period = 60.0 / bpm(t);
        let at = (t * rate) as usize;
        // Kick: a falling sine, 120 ms.
        for i in 0..(0.12 * rate) as usize {
            let s = i as f64 / rate;
            let f = 50.0 + 100.0 * (-s * 30.0).exp();
            let v = (std::f64::consts::TAU * f * s).sin() * (-s * 25.0).exp() * 0.8;
            if let Some(o) = out.get_mut(at + i) {
                *o += v as f32;
            }
        }
        // Hi-hat on the off-beat: noise, 50 ms, high-passed by differencing.
        let off = ((t + period / 2.0) * rate) as usize;
        let mut last = 0.0f32;
        for i in 0..(0.05 * rate) as usize {
            let s = i as f64 / rate;
            let x = rng.next();
            let v = (x - last) * 0.15 * (-s * 60.0).exp() as f32;
            last = x;
            if let Some(o) = out.get_mut(off + i) {
                *o += v;
            }
        }
        // Clap on 2 and 4.
        if k % 2 == 1 {
            for i in 0..(0.08 * rate) as usize {
                let s = i as f64 / rate;
                if let Some(o) = out.get_mut(at + i) {
                    *o += rng.next() * 0.25 * (-s * 40.0).exp() as f32;
                }
            }
        }
        // Bass: a note per bar, held.
        let note = [55.0, 55.0 * 1.335, 55.0 * 1.189, 55.0 * 1.498].get((k / 4) % 4).copied().unwrap_or(55.0);
        for i in (0.12 * rate) as usize..(period * rate) as usize {
            let s = (at + i) as f64 / rate;
            if let Some(o) = out.get_mut(at + i) {
                *o += ((std::f64::consts::TAU * note * s).sin() * 0.2) as f32;
            }
        }
        t += period;
        k += 1;
    }
    (out, beats)
}

/// Runs `samples` through an analysis in blocks of a 60 Hz frame; the features after each
/// block with the time at its end.
fn run(rate: f64, samples: &[f32], settings: &Settings) -> Vec<(f64, Features)> {
    let Ok(mut a) = Analyzer::new(rate) else { return Vec::new() };
    let block = (rate / 60.0).round() as usize;
    let mut out = Vec::new();
    let mut done = 0usize;
    for chunk in samples.chunks(block) {
        let f = a.process(chunk, settings);
        done += chunk.len();
        out.push((done as f64 / rate, f));
    }
    out
}

/// The beat phase the beats say at time `t`.
fn true_phase(beats: &[f64], t: f64) -> Option<f64> {
    let i = beats.iter().rposition(|&b| b <= t)?;
    let next = *beats.get(i + 1)?;
    let this = *beats.get(i)?;
    Some((t - this) / (next - this))
}

fn wrap(x: f64) -> f64 {
    (x + 0.5).rem_euclid(1.0) - 0.5
}

#[test]
fn finds_tempo_and_phase_of_a_house_loop() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let (samples, beats) = house(rate, 20.0, |_| 124.0, 0.3);
        let frames = run(rate, &samples, &Settings::default());
        let end = *beats.last().unwrap();
        let late: Vec<&(f64, Features)> = frames.iter().filter(|(t, _)| *t > 12.0 && *t < end).collect();
        let mut worst = 0.0f64;
        let mut mean = 0.0f64;
        for (t, f) in &late {
            assert!((f.bpm - 124.0).abs() < 0.7, "{rate} Hz at {t:.2}: {} BPM", f.bpm);
            let p = true_phase(&beats, *t).unwrap();
            worst = worst.max(wrap(f.beat - p).abs());
            mean += wrap(f.beat - p) / late.len() as f64;
        }
        let last = &late.last().unwrap().1;
        eprintln!(
            "{rate} Hz: bpm {:.2} confidence {:.2} phase error mean {mean:.3} worst {worst:.3}",
            last.bpm, last.confidence
        );
        assert!(last.confidence > 0.5, "{rate} Hz: confidence {}", last.confidence);
        assert!(worst < 0.06, "{rate} Hz: phase off by {worst:.3} of a beat");
        let ticks = late.iter().filter(|(_, f)| f.tick).count();
        let (from, to) = (late[0].0, late.last().unwrap().0);
        let expected = beats.iter().filter(|&&b| b > from && b <= to).count();
        assert!(ticks.abs_diff(expected) <= 1, "{ticks} ticks for {expected} beats");
    }
}

#[test]
fn an_offset_moves_the_phase_ahead() {
    let rate = 48_000.0;
    let (samples, _) = house(rate, 15.0, |_| 120.0, 0.3);
    let plain = run(rate, &samples, &Settings::default());
    let ahead = run(rate, &samples, &Settings { offset: 0.125, ..Settings::default() });
    // 125 ms at 120 BPM is a quarter beat.
    let (a, b) = (&plain.last().unwrap().1, &ahead.last().unwrap().1);
    assert!((wrap(b.beat - a.beat) - 0.25).abs() < 2e-3, "{} {}", a.beat, b.beat);
}

#[test]
fn follows_a_tempo_change() {
    let rate = 48_000.0;
    let (samples, _) = house(rate, 30.0, |t| if t < 12.0 { 120.0 } else { 128.0 }, 0.1);
    let frames = run(rate, &samples, &Settings::default());
    let at = |s: f64| frames.iter().find(|(t, _)| *t >= s).unwrap().1.bpm;
    assert!((at(11.0) - 120.0).abs() < 0.7, "{}", at(11.0));
    assert!((at(29.0) - 128.0).abs() < 0.7, "{}", at(29.0));
}

#[test]
fn counts_bars_from_the_bass_changes() {
    let rate = 48_000.0;
    let (samples, beats) = house(rate, 30.0, |_| 124.0, 0.3);
    let frames = run(rate, &samples, &Settings::default());
    // The bass changes on beat 0 of 4; every beat after 15 s is counted right.
    let mut checked = 0;
    for (t, f) in frames.iter().filter(|(t, f)| f.tick && *t > 15.0) {
        let i = beats.iter().rposition(|&b| b <= *t + 0.02).unwrap();
        assert_eq!(f.bar_beat as usize, i % 4, "at {t:.2}");
        assert!((f.bar - (f.bar_beat as f64 + f.beat) / 4.0).abs() < 1e-9);
        checked += 1;
    }
    assert!(checked > 20);
}

#[test]
fn levels_of_sines() {
    let rate = 48_000.0;
    let sine = |f: f64, a: f64| -> Vec<f32> {
        (0..(rate * 2.0) as usize)
            .map(|i| (a * (std::f64::consts::TAU * f * i as f64 / rate).sin()) as f32)
            .collect()
    };
    let raw = Settings { normalize: false, ..Settings::default() };
    let f = run(rate, &sine(1000.0, 1.0), &raw).pop().unwrap().1;
    assert!((f.rms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01, "{}", f.rms);
    assert!((f.peak - 1.0).abs() < 0.01);
    assert!((f.mid - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.1, "mid {}", f.mid);
    assert!(f.low < 0.01 && f.high < 0.01, "{} {}", f.low, f.high);
    let f = run(rate, &sine(60.0, 0.5), &raw).pop().unwrap().1;
    assert!((f.low - 0.3536).abs() < 0.05, "low {}", f.low);
    let f = run(rate, &sine(6000.0, 0.5), &raw).pop().unwrap().1;
    assert!((f.high - 0.3536).abs() < 0.05, "high {}", f.high);
    // The spectrum's loudest band holds the sine.
    let peak = f.spectrum.iter().cloned().enumerate().fold((0, 0.0), |b, c| if c.1 > b.1 { c } else { b });
    assert_eq!(f.spectrum.len(), 32);
    assert!(peak.0 > 24, "band {}", peak.0);
    // Normalised, a quiet sine still reaches 1.
    let f = run(rate, &sine(1000.0, 0.01), &Settings::default()).pop().unwrap().1;
    assert!(f.level > 0.95 && f.mid > 0.95, "{} {}", f.level, f.mid);
}

#[test]
fn onsets_of_clicks() {
    let rate = 48_000.0;
    let mut samples = vec![0.0f32; (rate * 10.0) as usize];
    let mut clicks = 0;
    // Irregular clicks, 150 to 600 ms apart.
    let mut t = 0.5;
    let mut rng = Rng(7);
    while t < 9.5 {
        let at = (t * rate) as usize;
        for i in 0..200 {
            samples[at + i] += rng.next() * 0.5 * (-(i as f32) / 40.0).exp();
        }
        clicks += 1;
        t += 0.15 + 0.45 * (rng.next() as f64 * 0.5 + 0.5);
    }
    let frames = run(rate, &samples, &Settings::default());
    let found = frames.iter().filter(|(_, f)| f.onset).count();
    assert!(found.abs_diff(clicks) <= 1, "{found} onsets for {clicks} clicks");
}

#[test]
fn silence_noise_and_garbage() {
    let rate = 48_000.0;
    let frames = run(rate, &vec![0.0; 48_000 * 10], &Settings::default());
    let f = &frames.last().unwrap().1;
    assert_eq!((f.bpm, f.confidence, f.level), (0.0, 0.0, 0.0));
    assert!(f.spectrum.iter().all(|v| *v == 0.0));
    assert!(!frames.iter().any(|(_, f)| f.onset || f.tick));
    let mut rng = Rng(3);
    let noise: Vec<f32> = (0..48_000 * 10).map(|_| rng.next() * 0.3).collect();
    let f = run(rate, &noise, &Settings::default()).pop().unwrap().1;
    assert!(f.confidence < 0.4, "noise confidence {}", f.confidence);
    let garbage: Vec<f32> = (0..48_000).map(|i| [f32::NAN, f32::INFINITY, -1e30, 0.5][i % 4]).collect();
    let f = run(rate, &garbage, &Settings::default()).pop().unwrap().1;
    assert!(f.rms.is_finite() && f.level.is_finite() && f.spectrum.iter().all(|v| v.is_finite()));
    // Odd settings do not break it.
    let odd = Settings {
        low_hz: -5.0,
        high_hz: 1e9,
        sensitivity: f64::NAN,
        min_bpm: 500.0,
        max_bpm: -3.0,
        beats_per_bar: 0,
        bands: 100_000,
        normalize: true,
        floor_db: f64::NAN,
        offset: f64::INFINITY,
    };
    let f = run(rate, &noise, &odd).pop().unwrap().1;
    assert!(f.spectrum.len() <= penumbra_audio::analysis::MAX_BANDS);
    assert!(Analyzer::new(60.0).is_err());
    assert!(Analyzer::new(f64::NAN).is_err());
}

#[test]
fn blocks_shorter_than_a_hop_keep_the_bands() {
    let rate = 96_000.0;
    let sine: Vec<f32> = (0..96_000)
        .map(|i| (0.5 * (std::f64::consts::TAU * 1000.0 * i as f64 / rate).sin()) as f32)
        .collect();
    let mut a = Analyzer::new(rate).unwrap();
    let raw = Settings { normalize: false, ..Settings::default() };
    // 960 samples per block, the hop is 1024: some blocks complete no frame.
    assert_eq!(a.hop(), 1024);
    for (i, chunk) in sine.chunks(960).enumerate() {
        let f = a.process(chunk, &raw);
        // After the first frame has filled.
        if i > 5 {
            assert!(f.mid > 0.3, "mid {}", f.mid);
        }
    }
}
