//! The analysis against synthetic signals with known levels, onsets and tempo.

mod common;

use common::{Rng, house, pink, speech, white};
use penumbra_audio::{Analyzer, Features, Settings};

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
        assert!(late.iter().all(|(_, f)| f.locked), "{rate} Hz: not locked throughout");
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
    // Locked at the new tempo, ticking with it.
    let late: Vec<&(f64, Features)> = frames.iter().filter(|(t, _)| *t > 25.0 && *t <= 29.0).collect();
    assert!(late.iter().all(|(_, f)| f.locked));
    let ticks = late.iter().filter(|(_, f)| f.tick).count();
    assert!(ticks.abs_diff(128 * 4 / 60) <= 1, "{ticks} ticks in 4 s at 128 BPM");
}

#[test]
fn no_beat_in_noise_speech_or_silence() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let material = [
            ("white noise", white(rate, 20.0, 0.1, 3)),
            ("pink noise", pink(rate, 20.0, 0.3, 4)),
            ("speech", speech(rate, 30.0, 5)),
            ("other speech", speech(rate, 30.0, 17)),
            ("silence", vec![0.0; (rate * 10.0) as usize]),
        ];
        for (name, x) in material {
            let frames = run(rate, &x, &Settings::default());
            let late: Vec<f64> = frames.iter().filter(|(t, _)| *t > 6.0).map(|(_, f)| f.confidence).collect();
            let mean = late.iter().sum::<f64>() / late.len().max(1) as f64;
            assert!(mean < 0.15, "{name} at {rate} Hz: confidence {mean:.2}");
            for (t, f) in &frames {
                assert!(!f.locked && !f.tick, "{name} at {rate} Hz: a beat at {t:.2}");
                assert_eq!((f.bpm, f.beat), (0.0, 0.0), "{name} at {rate} Hz: a tempo before any lock");
            }
        }
    }
}

#[test]
fn music_locks_and_lets_go_when_it_stops() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let (music, _) = house(rate, 12.0, |_| 124.0, 0.3);
        // Silence, room noise below the floor, and speech after the music.
        let after = [
            ("silence", vec![0.0; (rate * 12.0) as usize]),
            ("room noise", white(rate, 12.0, 0.0003, 9)),
            ("speech", speech(rate, 15.0, 21)),
        ];
        for (name, tail) in after {
            let x: Vec<f32> = music.iter().chain(&tail).copied().collect();
            let frames = run(rate, &x, &Settings::default());
            let locked_at = frames.iter().find(|(_, f)| f.locked).map(|(t, _)| *t);
            assert!(locked_at.is_some_and(|t| t < 6.5), "{name} at {rate} Hz: locked at {locked_at:?}");
            let between: Vec<&(f64, Features)> =
                frames.iter().filter(|(t, _)| *t > 6.5 && *t < 12.0).collect();
            assert!(between.iter().all(|(_, f)| f.locked), "{name} at {rate} Hz: let go during the music");
            // Quiet lets go within 5 s; speech once the music has left most of the 8 s the
            // tempo is measured over.
            let gone = if name == "speech" { 12.0 + 11.0 } else { 12.0 + 5.0 };
            for (t, f) in frames.iter().filter(|(t, _)| *t > gone) {
                assert!(!f.locked && !f.tick, "{name} at {rate} Hz: still a beat at {t:.2}");
            }
            // The tempo stays known for when the beat comes back.
            let last = &frames.last().unwrap().1;
            assert!((last.bpm - 124.0).abs() < 1.0, "{name} at {rate} Hz: {} BPM", last.bpm);
        }
    }
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
    // Normalised, a quiet steady sine reads near the top of its range: the knee bends 1 to
    // about 0.95.
    let f = run(rate, &sine(1000.0, 0.01), &Settings::default()).pop().unwrap().1;
    assert!(f.level > 0.9 && f.mid > 0.9, "{} {}", f.level, f.mid);
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
    assert!(f.confidence < 0.15 && !f.locked, "noise confidence {}", f.confidence);
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
        trim_db: f64::INFINITY,
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

/// `x` scaled to an RMS of `db` dBFS.
fn scaled(mut x: Vec<f32>, db: f64) -> Vec<f32> {
    let rms = (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt();
    let gain = (10f64.powf(db / 20.0) / rms.max(1e-12)) as f32;
    for v in &mut x {
        *v *= gain;
    }
    x
}

/// The `p` percentile of `level`, `low`, `mid` and `high` over the frames from `from` to `to` s.
fn spread(frames: &[(f64, Features)], from: f64, to: f64, p: f64) -> [f64; 4] {
    let mut out = [0.0; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let mut v: Vec<f64> = frames
            .iter()
            .filter(|(t, _)| *t >= from && *t < to)
            .filter_map(|(_, f)| [f.level, f.low, f.mid, f.high].get(k).copied())
            .collect();
        v.sort_by(f64::total_cmp);
        *o = v.get(((v.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(f64::NAN);
    }
    out
}

/// A knock on the microphone at `at` s: 5 ms of decaying noise at half full scale.
fn knock(x: &mut [f32], rate: f64, at: f64) {
    let mut rng = Rng(11);
    let start = (at * rate) as usize;
    for i in 0..(0.005 * rate) as usize {
        if let Some(s) = x.get_mut(start + i) {
            *s += 0.5 * rng.next() * (-(i as f64) / (0.001 * rate)).exp() as f32;
        }
    }
}

#[test]
fn quiet_music_moves_over_the_range_at_any_rate() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        // A microphone in the room: the music at -45 dBFS RMS.
        let x = scaled(house(rate, 12.0, |_| 124.0, 0.3).0, -45.0);
        let frames = run(rate, &x, &Settings::default());
        // The loop is sparse (the mid band has only the claps on 2 and 4), so most of the
        // time sits low; the hits reach the top.
        let (lo, hi) = (spread(&frames, 5.0, 12.0, 0.1), spread(&frames, 5.0, 12.0, 0.99));
        for k in 0..4 {
            assert!(lo[k] < 0.3 && hi[k] > 0.9, "{rate} Hz, value {k}: P10 {} P99 {}", lo[k], hi[k]);
        }
        assert!(
            frames
                .iter()
                .all(|(_, f)| [f.level, f.low, f.mid, f.high].iter().all(|v| (0.0..=1.0).contains(v)))
        );
    }
}

#[test]
fn a_knock_holds_the_range_for_four_seconds_at_most() {
    let rate = 48_000.0;
    let mut x = scaled(house(rate, 20.0, |_| 124.0, 0.3).0, -45.0);
    knock(&mut x, rate, 8.0);
    let frames = run(rate, &x, &Settings::default());
    let clean = run(rate, &scaled(house(rate, 20.0, |_| 124.0, 0.3).0, -45.0), &Settings::default());
    // One knock is a single hop of the 4 s window, far fewer than the 5 % above the top
    // percentile: right after it, the bands read as they do without it.
    for (from, to) in [(8.1, 12.0), (12.0, 20.0)] {
        for p in [0.5, 0.9] {
            let (a, b) = (spread(&frames, from, to, p), spread(&clean, from, to, p));
            for k in 0..4 {
                assert!(
                    (a[k] - b[k]).abs() < 0.05,
                    "value {k} from {from} s, P{p}: {} against {}",
                    a[k],
                    b[k]
                );
            }
        }
    }
    // Half a second of loud noise fills more than 5 % of the window: it presses the bands
    // down while it is in the window and is gone 4 s after it ended.
    let mut loud = scaled(house(rate, 20.0, |_| 124.0, 0.3).0, -45.0);
    let mut rng = Rng(13);
    for v in loud.iter_mut().skip(8 * 48_000).take(24_000) {
        *v += rng.next() * 0.3;
    }
    let frames = run(rate, &loud, &Settings::default());
    let (a, b) = (spread(&frames, 9.0, 12.0, 0.9), spread(&clean, 9.0, 12.0, 0.9));
    assert!(a.iter().zip(&b).all(|(a, b)| a < b), "{a:?} against {b:?}");
    for p in [0.5, 0.9] {
        let (a, b) = (spread(&frames, 12.6, 20.0, p), spread(&clean, 12.6, 20.0, p));
        for k in 0..4 {
            assert!((a[k] - b[k]).abs() < 0.05, "value {k} after the noise, P{p}: {} against {}", a[k], b[k]);
        }
    }
}

#[test]
fn silence_reads_zero_and_keeps_the_range() {
    let rate = 48_000.0;
    let music = scaled(house(rate, 8.0, |_| 124.0, 0.3).0, -45.0);
    let mut rng = Rng(5);
    // Room noise 10 dB below the floor.
    let hiss: Vec<f32> = (0..(rate * 4.0) as usize).map(|_| rng.next() * 0.0005).collect();
    let x: Vec<f32> = music.iter().chain(&hiss).chain(&music).copied().collect();
    let frames = run(rate, &x, &Settings::default());
    for (t, f) in frames.iter().filter(|(t, _)| *t > 8.1 && *t < 12.0) {
        assert_eq!([f.level, f.low, f.mid, f.high], [0.0; 4], "at {t:.2}");
        assert!(f.spectrum.iter().all(|v| *v == 0.0), "spectrum at {t:.2}");
    }
    // The range is the music's from before the pause, so the music reads at once.
    let back = spread(&frames, 12.2, 13.5, 0.99);
    assert!(back.iter().all(|v| *v > 0.9), "{back:?}");
    // A trim lifts the same noise above the floor.
    let lifted = Settings { trim_db: 24.0, ..Settings::default() };
    let f = run(rate, &hiss, &lifted).pop().unwrap().1;
    assert!(f.level > 0.0 && f.spectrum.iter().any(|v| *v > 0.0), "{f:?}");
    // rms and peak are the signal as it came in.
    let plain = run(rate, &hiss, &Settings::default()).pop().unwrap().1;
    assert!((f.rms - plain.rms).abs() < 1e-9 && (f.peak - plain.peak).abs() < 1e-9);
}

#[test]
fn garbage_keeps_the_normalised_values_in_range() {
    let rate = 96_000.0;
    let mut x = scaled(house(rate, 6.0, |_| 124.0, 0.3).0, -45.0);
    for (i, v) in x.iter_mut().enumerate().skip(2 * 96_000).take(10_000) {
        *v = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30][i % 4];
    }
    for trim_db in [0.0, f64::NAN, -1e9, 24.0] {
        let s = Settings { trim_db, ..Settings::default() };
        for (t, f) in run(rate, &x, &s) {
            let all = [f.level, f.low, f.mid, f.high].into_iter().chain(f.spectrum.iter().copied());
            assert!(all.into_iter().all(|v| (0.0..=1.0).contains(&v)), "trim {trim_db} at {t:.2}: {f:?}");
        }
    }
}
