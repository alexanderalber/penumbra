//! The pitch tracker on synthetic sound: sines across the range, harmonic tones with a weak or
//! missing fundamental, vibrato, a glide, noise, silence, two tones, tones outside the range,
//! and hostile input. `cargo test -p penumbra-audio pitch -- --nocapture` prints a table with
//! the error in cents, the latency and the clarity of each case.

use std::f64::consts::TAU;

use crate::pitch::{Pitch, PitchSettings, PitchTracker, hertz_of, note_of};

/// A small, fixed noise source (xorshift), uniform in -1..1.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// The tracker's output after each block of `block` samples of `signal(i)` over `seconds`.
fn track(
    rate: f64,
    seconds: f64,
    block: usize,
    s: &PitchSettings,
    mut signal: impl FnMut(usize) -> f64,
) -> Vec<(f64, Pitch)> {
    let mut t = PitchTracker::new(rate).unwrap();
    let total = (seconds * rate) as usize;
    let mut out = Vec::new();
    let mut i = 0;
    let mut buf = Vec::with_capacity(block);
    while i < total {
        buf.clear();
        for _ in 0..block.min(total - i) {
            buf.push(signal(i) as f32);
            i += 1;
        }
        out.push((i as f64 / rate, t.process(&buf, s)));
    }
    out
}

/// A sum of harmonics of `f0` with these amplitudes (index 0 is the fundamental), phase `ph`.
fn harmonics(f0: f64, amps: &[f64], rate: f64, i: usize) -> f64 {
    let t = i as f64 / rate;
    amps.iter().enumerate().map(|(k, a)| a * (TAU * f0 * (k + 1) as f64 * t + 0.3 * k as f64).sin()).sum()
}

/// What a steady case measured after `from` seconds.
struct Steady {
    /// Median and worst error of the held note in cents, against `target` Hz.
    median: f64,
    worst: f64,
    /// Seconds from the tone's start (time 0 or `onset`) to the first sure estimate within
    /// 50 cents.
    latency: f64,
    clarity: f64,
    /// Share of blocks after `from` that were sure.
    sure: f64,
}

fn steady(out: &[(f64, Pitch)], target: f64, onset: f64, from: f64) -> Steady {
    let late: Vec<&Pitch> = out.iter().filter(|(t, _)| *t >= from).map(|(_, p)| p).collect();
    let mut cents: Vec<f64> = late.iter().map(|p| 100.0 * (p.note - note_of(target)).abs()).collect();
    cents.sort_by(f64::total_cmp);
    let mut clar: Vec<f64> = late.iter().map(|p| p.clarity).collect();
    clar.sort_by(f64::total_cmp);
    let latency = out
        .iter()
        .find(|(t, p)| *t >= onset && p.sure && (p.note - note_of(target)).abs() < 0.5)
        .map_or(f64::INFINITY, |(t, _)| t - onset);
    Steady {
        median: cents.get(cents.len() / 2).copied().unwrap_or(f64::INFINITY),
        worst: cents.last().copied().unwrap_or(f64::INFINITY),
        latency,
        clarity: clar.get(clar.len() / 2).copied().unwrap_or(0.0),
        sure: late.iter().filter(|p| p.sure).count() as f64 / late.len().max(1) as f64,
    }
}

fn row(name: &str, s: &Steady) {
    println!(
        "| {name} | {:.1} | {:.1} | {:.0} | {:.3} | {:.0} % |",
        s.median,
        s.worst,
        s.latency * 1000.0,
        s.clarity,
        s.sure * 100.0
    );
}

/// 0.3 s of silence, then the tone, at 48 kHz in blocks of 800 (one 60 fps frame).
fn tone_case(f: impl FnMut(usize) -> f64, seconds: f64, s: &PitchSettings) -> Vec<(f64, Pitch)> {
    let rate = 48_000.0;
    let onset = (0.3 * rate) as usize;
    let mut f = f;
    track(rate, seconds, 800, s, move |i| if i < onset { 0.0 } else { f(i - onset) })
}

#[test]
fn sines_across_the_range_read_within_a_few_cents() {
    let s = PitchSettings::default();
    println!("| case | median cents | worst cents | latency ms | clarity | sure |");
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for f0 in [55.0, 82.41, 110.0, 196.0, 261.63, 440.0, 880.0, 1318.5, 1760.0, 2637.0, 3520.0] {
            let onset = (0.3 * rate) as usize;
            let block = (rate / 60.0) as usize;
            let out = track(rate, 1.3, block, &s, |i| {
                if i < onset { 0.0 } else { 0.5 * (TAU * f0 * (i - onset) as f64 / rate).sin() }
            });
            let r = steady(&out, f0, 0.3, 0.6);
            row(&format!("sine {f0} Hz at {} kHz", rate / 1000.0), &r);
            assert!(r.worst < 5.0, "{f0} Hz at {rate}: {} cents", r.worst);
            assert!(r.latency < 0.08, "{f0} Hz at {rate}: latency {}", r.latency);
            assert!(r.clarity > 0.95 && r.sure > 0.99, "{f0} Hz at {rate}");
        }
    }
}

#[test]
fn a_weak_or_missing_fundamental_reads_at_the_fundamental() {
    let s = PitchSettings::default();
    let cases: [(&str, f64, &[f64]); 5] = [
        ("110 Hz, harmonics 2 to 6, no fundamental", 110.0, &[0.0, 0.3, 0.3, 0.3, 0.3, 0.3]),
        ("220 Hz, harmonics 3 to 8, no fundamental", 220.0, &[0.0, 0.0, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2]),
        (
            "98 Hz, fundamental at -26 dB, harmonics 1/k",
            98.0,
            &[0.02, 0.25, 0.17, 0.12, 0.1, 0.08, 0.07, 0.06],
        ),
        ("330 Hz, strong 2nd harmonic", 330.0, &[0.2, 0.5, 0.1]),
        ("146.8 Hz, odd harmonics (clarinet-like)", 146.8, &[0.4, 0.0, 0.2, 0.0, 0.12, 0.0, 0.08]),
    ];
    for (name, f0, amps) in cases {
        let out = tone_case(|i| harmonics(f0, amps, 48_000.0, i), 1.3, &s);
        let r = steady(&out, f0, 0.3, 0.6);
        row(name, &r);
        assert!(r.worst < 5.0, "{name}: {} cents", r.worst);
        assert!(r.sure > 0.99, "{name}: sure {}", r.sure);
    }
}

/// The worst and median distance in cents of the held note from `truth(t)` delayed by
/// `delay` seconds, over blocks after `from`.
fn follow(out: &[(f64, Pitch)], from: f64, delay: f64, truth: impl Fn(f64) -> f64) -> (f64, f64) {
    let mut e: Vec<f64> = out
        .iter()
        .filter(|(t, _)| *t >= from)
        .map(|(t, p)| 100.0 * (p.note - note_of(truth(t - delay))).abs())
        .collect();
    e.sort_by(f64::total_cmp);
    (e.get(e.len() / 2).copied().unwrap_or(f64::INFINITY), e.last().copied().unwrap_or(f64::INFINITY))
}

#[test]
fn vibrato_and_a_glide_are_followed() {
    let rate = 48_000.0;
    let s = PitchSettings::default();
    // The held note trails the sound by half the window (20 ms at 50 Hz), up to a hop, and the
    // smoothing (50 ms).
    let delay = 0.02 + 0.005 + 0.05;
    // 440 Hz with a vibrato of +-50 cents at 5.5 Hz, harmonics as a voice.
    let vib = |t: f64| hertz_of(69.0 + 0.5 * (TAU * 5.5 * t).sin());
    let mut phase = 0.0;
    let out = track(rate, 2.0, 800, &s, |i| {
        phase += TAU * vib(i as f64 / rate) / rate;
        0.5 * phase.sin() + 0.2 * (2.0 * phase).sin() + 0.1 * (3.0 * phase).sin()
    });
    let (median, worst) = follow(&out, 0.5, delay, vib);
    println!("| vibrato 440 Hz +-50 cents at 5.5 Hz | {median:.1} | {worst:.1} | | | |");
    // With no smoothing, the error left is the window's own lag.
    let raw = PitchSettings { smooth: 0.0, ..s.clone() };
    let mut phase = 0.0;
    let out0 = track(rate, 2.0, 800, &raw, |i| {
        phase += TAU * vib(i as f64 / rate) / rate;
        0.5 * phase.sin() + 0.2 * (2.0 * phase).sin() + 0.1 * (3.0 * phase).sin()
    });
    let (m0, w0) = follow(&out0, 0.5, 0.025, vib);
    println!("| vibrato, smooth 0 | {m0:.1} | {w0:.1} | | | |");
    assert!(w0 < 15.0, "vibrato with smooth 0: {w0} cents");
    // Smoothing trails the vibrato and flattens it, but stays within its depth.
    assert!(worst < 50.0, "vibrato: {worst} cents");
    // A glide from 110 to 880 Hz over 2 s, three octaves, exponential.
    let glide = |t: f64| 110.0 * (3.0 * (t / 2.0).clamp(0.0, 1.0)).exp2();
    let mut phase = 0.0;
    let out = track(rate, 2.5, 800, &raw, |i| {
        phase += TAU * glide(i as f64 / rate) / rate;
        0.5 * phase.sin() + 0.2 * (2.0 * phase).sin()
    });
    let (m, w) = follow(&out, 0.2, 0.025, glide);
    println!("| glide 110 to 880 Hz in 2 s, smooth 0 | {m:.1} | {w:.1} | | | |");
    assert!(w < 20.0, "glide: {w} cents");
}

#[test]
fn a_tone_in_noise_holds_down_to_about_0_db() {
    let s = PitchSettings::default();
    for snr in [30.0, 20.0, 10.0, 6.0, 0.0] {
        // A voice-like 220 Hz tone, RMS 0.25; white noise at the same RMS minus `snr` dB.
        let amps = [0.25, 0.15, 0.1, 0.05];
        let power: f64 = amps.iter().map(|a| a * a / 2.0).sum();
        let noise_rms = (power / 10f64.powf(snr / 10.0)).sqrt();
        let mut n = Noise(0x9e37_79b9_7f4a_7c15);
        let out =
            tone_case(|i| harmonics(220.0, &amps, 48_000.0, i) + noise_rms * 3f64.sqrt() * n.next(), 2.3, &s);
        let r = steady(&out, 220.0, 0.3, 0.6);
        row(&format!("220 Hz + white noise, SNR {snr} dB"), &r);
        if snr >= 10.0 {
            assert!(r.worst < 10.0 && r.sure > 0.9, "SNR {snr}: {} cents, sure {}", r.worst, r.sure);
        }
        // Whatever the noise, the held note is never far off: unsure estimates do not move it.
        assert!(r.worst < 50.0 || r.sure == 0.0, "SNR {snr}: {} cents", r.worst);
    }
}

#[test]
fn silence_and_noise_keep_the_held_note() {
    let rate = 48_000.0;
    let s = PitchSettings::default();
    // A second of 330 Hz, then a second of silence, then white and brown noise.
    let mut n = Noise(12345);
    let mut brown = 0.0;
    let out = track(rate, 4.0, 800, &s, |i| {
        let t = i as f64 / rate;
        if t < 1.0 {
            0.4 * (TAU * 330.0 * t).sin()
        } else if t < 2.0 {
            0.0
        } else if t < 3.0 {
            0.3 * n.next()
        } else {
            brown = 0.995 * brown + 0.05 * n.next();
            brown
        }
    });
    let held = note_of(330.0);
    let part = |a: f64, b: f64| out.iter().filter(move |(t, _)| *t >= a && *t < b).map(|(_, p)| *p);
    for (name, a, b) in [("silence", 1.1, 2.0), ("white noise", 2.1, 3.0), ("brown noise", 3.1, 4.0)] {
        let clarity = part(a, b).map(|p| p.clarity).fold(0.0, f64::max);
        let sure = part(a, b).filter(|p| p.sure).count();
        let moved = part(a, b).map(|p| (p.note - held).abs() * 100.0).fold(0.0, f64::max);
        println!(
            "| {name} after 330 Hz | held moved {moved:.1} cents | max clarity {clarity:.3} | sure {sure} |"
        );
        assert_eq!(sure, 0, "{name}");
        assert!(moved < 1.0, "{name}: held moved {moved} cents");
        assert!(clarity < 0.8, "{name}: clarity {clarity}");
    }
    assert!(part(1.1, 2.0).all(|p| p.clarity == 0.0 && p.heard_frequency == 0.0));
    // Before any tone, everything reads 0.
    let quiet = track(rate, 0.5, 800, &s, |_| 0.0);
    assert!(quiet.iter().all(|(_, p)| *p == Pitch::default()));
}

#[test]
fn two_tones_at_once() {
    let s = PitchSettings::default();
    let sine = |f: f64, a: f64| move |i: usize| a * (TAU * f * i as f64 / 48_000.0).sin();
    println!("| two tones | heard Hz (median) | clarity | sure |");
    let cases: [(&str, f64, f64, f64, f64); 5] = [
        ("440 + 660 Hz, a fifth, equal", 440.0, 0.3, 660.0, 0.3),
        ("440 + 554.4 Hz, a major third, equal", 440.0, 0.3, 554.37, 0.3),
        ("440 + 466.2 Hz, a semitone, equal", 440.0, 0.3, 466.16, 0.3),
        ("440 Hz + 660 Hz at -12 dB", 440.0, 0.4, 660.0, 0.1),
        ("110 Hz bass + 880 Hz melody at +6 dB", 110.0, 0.2, 880.0, 0.4),
    ];
    let mut medians = Vec::new();
    for (name, f1, a1, f2, a2) in cases {
        let (x, y) = (sine(f1, a1), sine(f2, a2));
        let out = tone_case(|i| x(i) + y(i), 1.3, &s);
        let late: Vec<&Pitch> = out.iter().filter(|(t, _)| *t > 0.6).map(|(_, p)| p).collect();
        let mut hz: Vec<f64> = late.iter().map(|p| p.heard_frequency).collect();
        hz.sort_by(f64::total_cmp);
        let mut cl: Vec<f64> = late.iter().map(|p| p.clarity).collect();
        cl.sort_by(f64::total_cmp);
        let sure = late.iter().filter(|p| p.sure).count() as f64 / late.len() as f64;
        let median = hz[hz.len() / 2];
        println!("| {name} | {median:.1} | {:.3} | {:.0} % |", cl[cl.len() / 2], sure * 100.0);
        medians.push(median);
    }
    // A fifth repeats at the common fundamental, an octave below the lower tone.
    assert!((medians[0] - 220.0).abs() < 2.0, "fifth: {}", medians[0]);
    // Even a quiet fifth makes the sum repeat at the common fundamental: at the louder tone's
    // period the fifth is inverted, and that peak is below the pick share of the full one.
    assert!((medians[3] - 220.0).abs() < 2.0, "quiet fifth: {}", medians[3]);
    // Two tones a semitone apart beat: they read near their mean.
    assert!(medians[2] > 440.0 && medians[2] < 466.2, "semitone: {}", medians[2]);
    // A melody louder than the bass under it wins with the full range.
    assert!((medians[4] - 880.0).abs() < 10.0, "bass and melody: {}", medians[4]);
    // A range for the bass picks the bass under a louder melody that is not a multiple of it.
    let bass = PitchSettings { min_hz: 40.0, max_hz: 200.0, ..s.clone() };
    let (b, m) = (sine(98.0, 0.2), sine(740.0, 0.4));
    let out = tone_case(|i| b(i) + m(i), 1.3, &bass);
    let r = steady(&out, 98.0, 0.3, 0.6);
    row("98 Hz bass + 740 Hz melody at +6 dB, range 40 to 200", &r);
    assert!(r.worst < 5.0 && r.sure > 0.9, "bass range: {} cents, sure {}", r.worst, r.sure);
}

#[test]
fn a_tone_outside_the_range_does_not_move_the_held_note() {
    let s = PitchSettings { min_hz: 80.0, max_hz: 1000.0, ..PitchSettings::default() };
    // 440 Hz for half a second, then 2 kHz above the range, then 60 Hz below it.
    let rate = 48_000.0;
    let out = track(rate, 1.5, 800, &s, |i| {
        let t = i as f64 / rate;
        let f = if t < 0.5 {
            440.0
        } else if t < 1.0 {
            2000.0
        } else {
            60.0
        };
        0.4 * (TAU * f * t).sin()
    });
    let above: Vec<&Pitch> = out.iter().filter(|(t, _)| *t > 0.6 && *t < 1.0).map(|(_, p)| p).collect();
    assert!(above.iter().all(|p| !p.sure && p.clarity == 0.0));
    assert!(above.iter().all(|p| (p.note - 69.0).abs() < 0.01));
    // The tone above the range is still heard as itself.
    assert!(above.iter().any(|p| (p.heard_frequency - 2000.0).abs() < 5.0));
    // Below the range there is no period within the window: nothing sure, held stays.
    let below: Vec<&Pitch> = out.iter().filter(|(t, _)| *t > 1.1).map(|(_, p)| p).collect();
    assert!(below.iter().all(|p| !p.sure && (p.note - 69.0).abs() < 0.01));
}

#[test]
fn hostile_input_and_settings_do_no_harm() {
    let mut t = PitchTracker::new(48_000.0).unwrap();
    let s = PitchSettings::default();
    let tone: Vec<f32> =
        (0..9600).map(|i| (0.5 * (TAU * 440.0 * i as f64 / 48_000.0).sin()) as f32).collect();
    t.process(&tone, &s);
    let good = t.process(&tone, &s);
    assert!(good.sure && (good.note - 69.0).abs() < 0.05);
    // Empty blocks, non-numbers, huge values.
    assert_eq!(t.process(&[], &s), good);
    let mut bad = vec![f32::NAN; 4800];
    bad.extend([f32::INFINITY, f32::NEG_INFINITY, f32::MAX, -f32::MAX, 1e30, f32::MIN_POSITIVE]);
    let p = t.process(&bad, &s);
    assert!(p.note.is_finite() && p.frequency.is_finite() && p.clarity.is_finite());
    assert!((p.note - 69.0).abs() < 0.05, "held through garbage: {}", p.note);
    // Settings of any kind.
    for (min, max, th, sm, fl) in [
        (f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        (f64::INFINITY, f64::NEG_INFINITY, 5.0, -1.0, 100.0),
        (0.0, 0.0, -1.0, 1e300, -1e300),
        (5000.0, 30.0, 0.0, 0.0, 0.0),
        (1e9, 1e9, 1.0, 0.1, -60.0),
    ] {
        let s = PitchSettings { min_hz: min, max_hz: max, threshold: th, smooth: sm, floor_db: fl };
        let p = t.process(&tone, &s);
        for v in [p.note, p.frequency, p.clarity, p.heard_frequency, p.heard_note] {
            assert!(v.is_finite(), "{s:?}: {p:?}");
        }
        assert!(t.window(&s) > 0);
    }
    // Rates outside audio are refused; every accepted one works, the window bounded.
    for r in [0.0, -1.0, f64::NAN, f64::INFINITY, 7999.0, 400_000.0] {
        assert!(PitchTracker::new(r).is_err(), "{r}");
    }
    for r in [8000.0, 22_050.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0, 384_000.0] {
        let mut t = PitchTracker::new(r).unwrap();
        let low = PitchSettings { min_hz: 20.0, max_hz: 10_000.0, ..PitchSettings::default() };
        assert!(t.window(&low) as f64 <= r * 0.11, "{r}: window {}", t.window(&low));
        let n = (r * 0.5) as usize;
        let x: Vec<f32> = (0..n).map(|i| (0.4 * (TAU * 220.0 * i as f64 / r).sin()) as f32).collect();
        let p = t.process(&x, &low);
        assert!((p.note - note_of(220.0)).abs() < 0.1, "{r}: {p:?}");
    }
}

#[test]
fn a_snapshot_analyses_the_same_block_again_the_same_way() {
    let mut t = PitchTracker::new(48_000.0).unwrap();
    let s = PitchSettings::default();
    let block = |k: usize| -> Vec<f32> {
        (k * 800..(k + 1) * 800).map(|i| (0.3 * (TAU * 300.0 * i as f64 / 48_000.0).sin()) as f32).collect()
    };
    for k in 0..10 {
        t.process(&block(k), &s);
    }
    let snap = t.snapshot();
    let a = t.process(&block(10), &s);
    t.restore(&snap);
    let b = t.process(&block(10), &s);
    assert_eq!(a, b);
    // A snapshot of a tracker at another rate is ignored.
    let other = PitchTracker::new(96_000.0).unwrap().snapshot();
    t.restore(&other);
    let p = t.process(&block(11), &s);
    assert!(p.sure && (p.note - note_of(300.0)).abs() < 0.05, "{p:?}");
}

#[test]
fn latency_from_silence_to_a_sure_note() {
    // Blocks of 1 ms, so the latency is the tracker's own, not a show frame's.
    println!("| min Hz | tone Hz | latency ms |");
    for (min, f0) in [
        (50.0, 55.0),
        (50.0, 110.0),
        (50.0, 440.0),
        (50.0, 1760.0),
        (80.0, 220.0),
        (200.0, 440.0),
        (400.0, 1760.0),
    ] {
        let s = PitchSettings { min_hz: min, ..PitchSettings::default() };
        let rate = 48_000.0;
        let onset = (0.3 * rate) as usize;
        let out = track(rate, 0.6, 48, &s, |i| {
            if i < onset { 0.0 } else { 0.4 * (TAU * f0 * (i - onset) as f64 / rate).sin() }
        });
        let r = steady(&out, f0, 0.3, 0.45);
        println!("| {min} | {f0} | {:.0} |", r.latency * 1000.0);
        // Two periods of min fill the window; then three hops in a row must agree.
        let bound = 2.0 / min + 4.0 * crate::pitch::HOP_SECONDS;
        assert!(r.latency < bound, "min {min}, {f0} Hz: {} s", r.latency);
    }
}
