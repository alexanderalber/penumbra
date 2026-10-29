//! `audio.ambience`: gapless blocks, smooth glides, gate timing inside the frame, every
//! preset's level, the water's bands, the swarm's cloud, hostile params; and ignored tests that
//! write listening files and measure the cook's cost.

use penumbra_core::Value;
use penumbra_graph::Op;

use super::ambience::{AMBIENCE, PRESETS};
use super::instrument_tests::{
    LISTEN_DIR, RATE, assert_gapless, cents, db, frequency, lanes, onsets, peak, run, write_wav,
};
use super::synth::CEILING;
use super::tests::{F, connect, create, set};

fn ambience(path: &str, preset: &str) -> Vec<Op> {
    vec![create(path, AMBIENCE), set(&format!("{path}.preset"), preset)]
}

/// The largest step from one sample to the next.
fn worst_step(x: &[f64]) -> f64 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max)
}

/// Nothing wired: one tone sounds, and its blocks join while note, brightness and level move.
#[test]
fn drone_and_pad_run_on_across_blocks() {
    for preset in ["drone", "pad"] {
        let mut f = F::new(&ambience("/a", preset));
        let mut all = f.block("/a");
        run(&mut f, "/a", 30, &mut all);
        assert!(peak(&all[12_000..]) > 0.01, "{preset} sounds without input");
        f.edit(&[set("/a.note", 0.7), set("/a.level", -6.0), set("/a.brightness", 1.0)]);
        run(&mut f, "/a", 30, &mut all);
        f.edit(&[set("/a.note", 0.2), set("/a.brightness", 0.0)]);
        run(&mut f, "/a", 30, &mut all);
        assert_gapless(&all[800..], 800, preset);
    }
}

/// A new note glides there: no step in the samples, the pitch at the end is the new note's.
#[test]
fn pitch_and_level_glide_without_steps() {
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.brightness", 0.0), set("/a.level", 0.0)]);
    f.block("/a");
    let mut a = Vec::new();
    run(&mut f, "/a", 60, &mut a);
    // C4; the two sines beat, the fifth is weaker.
    let low = frequency(&a[24_000..], 240.0, 280.0);
    assert!(cents(low, 261.63).abs() < 5.0, "{low}");
    let steady = worst_step(&a[24_000..]);
    f.edit(&[set("/a.note", 0.5), set("/a.level", -20.0)]);
    let mut b = Vec::new();
    run(&mut f, "/a", 90, &mut b);
    // An octave up at most doubles the step; a jump would be far larger.
    let gliding = worst_step(&b[..9600]);
    assert!(gliding < 2.0 * steady, "{gliding} against {steady}");
    let high = frequency(&b[48_000..], 500.0, 550.0);
    assert!(cents(high, 523.25).abs() < 5.0, "{high}");
    // The level arrives at -20 dB: the peak falls by about 20 dB less the overtone shift.
    let quiet = peak(&b[48_000..]);
    assert!((db(quiet) - db(peak(&a[24_000..]))).abs() > 15.0, "{quiet}");
}

/// A sine lfo into `gate`: each tone starts where the lfo rises past 0 and ends a release after
/// it falls past 0, within well under a millisecond, though the lfo has one sample per frame.
#[test]
fn gates_open_and_close_at_the_crossing_inside_the_frame() {
    let phase = 0.0123;
    let ops = vec![
        create("/l", "chan.lfo"),
        set("/l.rate", 2.5),
        set("/l.phase", phase),
        create("/a", AMBIENCE),
        set("/a.attack", 0.005),
        set("/a.release", 0.005),
        set("/a.level", 0.0),
        connect("/l", "/a", 0),
    ];
    let mut f = F::new(&ops);
    let mut all = f.block("/a");
    let first = -800_i64;
    run(&mut f, "/a", 180, &mut all);
    // Rising through 0 at 2.5 t + phase = k, falling at k + 1/2.
    let rises: Vec<f64> =
        (1..9).map(|k| (k as f64 - phase) / 2.5 * RATE).filter(|s| *s < 3.0 * RATE).collect();
    // The fresh first frame has the lfo just above 0: open from the start, closing at 0.195 s.
    let found: Vec<usize> = onsets(&all, 500).into_iter().filter(|&i| i > 1000).collect();
    assert_eq!(found.len(), rises.len(), "{found:?}");
    let mut worst = 0.0_f64;
    for (&i, e) in found.iter().zip(&rises) {
        // onsets() gives the sample before the first one heard; the gate opens on that one.
        worst = worst.max((((i + 1) as i64 + first) as f64 - e).abs());
    }
    // The ends: the last sample heard is a release (240 samples) after the fall.
    let mut ends = Vec::new();
    for (i, w) in all.windows(2).enumerate() {
        if w[0] != 0.0 && w[1] == 0.0 && all[i + 1..].iter().take(500).all(|s| *s == 0.0) {
            ends.push(i);
        }
    }
    // The last fall at 2.995 s ends too close to the end to see the silence after it.
    let falls: Vec<f64> =
        (0..8).map(|k| (k as f64 + 0.5 - phase) / 2.5 * RATE).filter(|s| *s + 800.0 < 3.0 * RATE).collect();
    assert_eq!(ends.len(), falls.len(), "{ends:?}");
    for (&i, e) in ends.iter().zip(&falls) {
        worst = worst.max(((i as i64 + first) as f64 - (e + 240.0)).abs());
    }
    let ms = worst / RATE * 1000.0;
    eprintln!("gate: worst error {worst:.2} samples = {ms:.4} ms over {} opens", found.len());
    assert!(ms < 0.5, "{ms} ms");
}

/// Every preset: finite, one tone at the default level well under -6 dBFS, 128 lanes at 0 dB
/// under full scale, and silent once every gate has closed.
#[test]
fn every_preset_is_finite_moderate_and_closes() {
    for preset in PRESETS {
        let mut f = F::new(&ambience("/a", preset));
        f.block("/a");
        let mut all = Vec::new();
        run(&mut f, "/a", 120, &mut all);
        let p = peak(&all);
        let mean = all.iter().sum::<f64>() / all.len() as f64;
        eprintln!("{preset}: one tone peaks {:.1} dBFS at -12 dB, mean {mean:.1e}", db(p));
        assert!(all.iter().all(|s| s.is_finite()), "{preset}");
        assert!(p > 0.01 && db(p) < -6.0, "{preset}: {p}");
        assert!(mean.abs() < 0.01, "{preset}: DC {mean}");
        // 128 lanes (two constants merged), all open, at 0 dB.
        let mut ops = lanes("/c", 64);
        ops.extend(lanes("/d", 64));
        ops.extend((0..64).map(|k| set(&format!("/d.name{k}"), format!("d{k}"))));
        ops.extend([create("/m", "chan.merge"), connect("/c", "/m", 0), connect("/d", "/m", 1)]);
        ops.extend(ambience("/a", preset));
        ops.extend([set("/a.level", 0.0), connect("/m", "/a", 0)]);
        let mut f = F::new(&ops);
        f.block("/a");
        let up: Vec<Op> = (0..64)
            .flat_map(|k| [set(&format!("/c.value{k}"), 1.0), set(&format!("/d.value{k}"), 1.0)])
            .collect();
        f.edit(&up);
        let mut all = Vec::new();
        run(&mut f, "/a", 60, &mut all);
        let p = peak(&all);
        eprintln!("{preset}: 128 tones peak {:.2} dBFS at 0 dB", db(p));
        assert!(p <= CEILING && p > 0.1 && all.iter().all(|s| s.is_finite()), "{preset}: {p}");
        // Every gate shut: silence after the release.
        let down: Vec<Op> = (0..64)
            .flat_map(|k| [set(&format!("/c.value{k}"), 0.0), set(&format!("/d.value{k}"), 0.0)])
            .collect();
        f.edit(&down);
        let mut tail = Vec::new();
        run(&mut f, "/a", 60, &mut tail);
        assert!(tail[tail.len() - 4800..].iter().all(|s| *s == 0.0), "{preset} still sounds");
    }
}

/// A recook at the same tick gives the same block; after a seek the tone fades in again.
#[test]
fn again_repeats_and_a_seek_fades_in() {
    let mut f = F::new(&ambience("/a", "pad"));
    f.block("/a");
    run(&mut f, "/a", 30, &mut Vec::new());
    f.clock.advance(1);
    let a = f.block("/a");
    f.edit(&[set("/a.threshold", 0.0001)]);
    assert_eq!(a, f.block("/a"));
    f.clock.seek(0);
    let b = f.block("/a");
    assert!(b[0].abs() < 1e-6 && peak(&b[..100]) < 0.01 * peak(&a), "a seek starts from silence");
}

/// Switching the preset crossfades: no step beyond the sounds' own.
#[test]
fn a_preset_change_crossfades() {
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.level", 0.0), set("/a.octave", 2)]);
    f.block("/a");
    let mut all = Vec::new();
    run(&mut f, "/a", 30, &mut all);
    let before = worst_step(&all);
    f.edit(&[set("/a.preset", "pad")]);
    let mut after = Vec::new();
    run(&mut f, "/a", 30, &mut after);
    let pad = worst_step(&after[4800..]);
    let swap = worst_step(&after[..4800]);
    assert!(swap <= 1.5 * before.max(pad), "{swap} against {before} and {pad}");
}

#[test]
fn hostile_params_do_not_break_it() {
    for rate in [1_i64, 8_000, 192_000, i64::MAX] {
        for preset in PRESETS {
            let mut ops = lanes("/c", 64);
            ops.extend(ambience("/a", preset));
            ops.extend([
                set("/a.rate", rate),
                set("/a.octave", 9),
                set("/a.spread", 128),
                set("/a.note", 10.0),
                set("/a.range", 10.0),
                set("/a.glide", 0.0),
                set("/a.attack", 1e9),
                set("/a.release", -5.0),
                set("/a.level", 1e9),
                set("/a.brightness", 7.0),
                set("/a.swarm", 1e9),
                set("/a.density", 1e9),
                set("/a.threshold", Value::from(-1e300)),
                connect("/c", "/a", 0),
            ]);
            let mut f = F::new(&ops);
            let mut all = f.block("/a");
            run(&mut f, "/a", 5, &mut all);
            f.edit(&[
                set("/a.octave", -1),
                set("/a.spread", -128),
                set("/a.note", -10.0),
                set("/a.swarm", -1e9),
                set("/a.density", -1e9),
                set("/a.brightness", -3.0),
            ]);
            run(&mut f, "/a", 5, &mut all);
            assert!(all.iter().all(|s| s.is_finite() && s.abs() <= CEILING), "{preset} at {rate}");
        }
    }
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.name", "")]);
    assert!(f.cook("/a").is_err());
}

/// Listening files in the listening folder: drone, pad and noise for 10 s with note and
/// brightness rising slowly, a pad chord whose three gates open and close, water with
/// brightness rising, rain with density rising, and a swarm resolving into a chord. Run with
/// `cargo test -p penumbra-ops write_ambience_files -- --ignored`.
#[test]
#[ignore]
fn write_ambience_files() {
    let dir = std::path::Path::new(LISTEN_DIR);
    std::fs::create_dir_all(dir).unwrap();
    for preset in ["drone", "pad", "noise"] {
        let mut f = F::new(&[
            create("/a", AMBIENCE),
            set("/a.preset", preset),
            set("/a.key", "F"),
            set("/a.octave", 3),
        ]);
        let mut all = f.block("/a");
        for k in 0..600 {
            let x = f64::from(k) / 600.0;
            f.edit(&[set("/a.note", x), set("/a.brightness", x)]);
            run(&mut f, "/a", 1, &mut all);
        }
        write_wav(&dir.join(format!("ambience_{preset}.wav")), &all);
    }
    // A pad chord, F A C: one gate after the other opens, then they close in turn.
    let mut ops = lanes("/c", 3);
    ops.extend(ambience("/a", "pad"));
    ops.extend([
        set("/a.key", "F"),
        set("/a.octave", 3),
        set("/a.scale", "major"),
        set("/a.spread", 2),
        set("/a.attack", 0.4),
        set("/a.release", 1.0),
        connect("/c", "/a", 0),
    ]);
    let mut f = F::new(&ops);
    let mut all = f.block("/a");
    for (lane, v) in
        [(0, 1.0), (1, 1.0), (2, 1.0), (0, 0.0), (1, 0.0), (2, 0.0), (0, 1.0), (2, 1.0), (0, 0.0), (2, 0.0)]
    {
        f.edit(&[set(&format!("/c.value{lane}"), v)]);
        run(&mut f, "/a", 60, &mut all);
    }
    run(&mut f, "/a", 120, &mut all);
    write_wav(&dir.join("ambience_gate.wav"), &all);

    // Water for 20 s, brightness rising from 0 (a river) to 1 (a trickle).
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.preset", "water"), set("/a.brightness", 0.0)]);
    let mut all = f.block("/a");
    for k in 0..1200 {
        f.edit(&[set("/a.brightness", f64::from(k) / 1200.0)]);
        run(&mut f, "/a", 1, &mut all);
    }
    write_wav(&dir.join("ambience_water.wav"), &all);

    // Rain for 20 s, density rising from 0 (a few drops) to 1 (a downpour).
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.preset", "rain"), set("/a.density", 0.0)]);
    let mut all = f.block("/a");
    for k in 0..1200 {
        f.edit(&[set("/a.density", f64::from(k) / 1200.0)]);
        run(&mut f, "/a", 1, &mut all);
    }
    write_wav(&dir.join("ambience_rain.wav"), &all);

    // A pad chord in D major pentatonic (D3 F#3 A3 D4: spread 2 skips E and B): swarm 1 for
    // 6 s, down to 0 over 8 s, then 0 for 6 s.
    let mut ops = lanes("/c", 4);
    ops.extend(ambience("/a", "pad"));
    ops.extend([
        set("/a.key", "D"),
        set("/a.octave", 3),
        set("/a.spread", 2),
        set("/a.swarm", 1.0),
        set("/a.attack", 1.0),
        connect("/c", "/a", 0),
    ]);
    ops.extend((0..4).map(|k| set(&format!("/c.value{k}"), 1.0)));
    let mut f = F::new(&ops);
    let mut all = f.block("/a");
    run(&mut f, "/a", 360, &mut all);
    for k in 0..480 {
        f.edit(&[set("/a.swarm", 1.0 - f64::from(k + 1) / 480.0)]);
        run(&mut f, "/a", 1, &mut all);
    }
    run(&mut f, "/a", 360, &mut all);
    write_wav(&dir.join("ambience_swarm.wav"), &all);
}

/// The power of `x` at `hz`: a Hann-windowed DFT bin.
fn power_at(x: &[f64], hz: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, s) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
        let (si, co) = (std::f64::consts::TAU * hz * i as f64 / RATE).sin_cos();
        re += s * w * co;
        im += s * w * si;
    }
    re * re + im * im
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|s| s * s).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// A rough mean frequency, Hz: from the RMS of the slope against the RMS of the signal (exact
/// for a sine).
fn centroid(x: &[f64]) -> f64 {
    let d: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
    2.0 * (rms(&d) / rms(x) / 2.0).min(1.0).asin() * RATE / std::f64::consts::TAU
}

/// Water: finite, below the ceiling, no DC, gapless; brightness moves it up, the loudness holds.
#[test]
fn water_is_clean_and_brightness_moves_it_up() {
    let mut centres = Vec::new();
    let mut levels = Vec::new();
    for b in [0.0, 0.5, 1.0] {
        let mut f = F::new(&[create("/a", AMBIENCE), set("/a.preset", "water"), set("/a.brightness", b)]);
        let mut all = f.block("/a");
        run(&mut f, "/a", 240, &mut all);
        let tail = &all[9600..];
        let mean = tail.iter().sum::<f64>() / tail.len() as f64;
        assert!(all.iter().all(|s| s.is_finite() && s.abs() < CEILING), "{b}");
        assert!(db(peak(tail)) < -6.0, "{b}: {}", db(peak(tail)));
        assert!(mean.abs() < 0.002, "{b}: DC {mean}");
        assert_gapless(&all, 800, "water");
        centres.push(centroid(tail));
        levels.push(db(rms(tail)));
    }
    eprintln!("water: mean frequency {centres:.0?} Hz, rms {levels:.1?} dBFS at brightness 0, 0.5, 1");
    assert!(centres[1] > 1.5 * centres[0] && centres[2] > 1.5 * centres[1], "{centres:?}");
    assert!(levels.iter().all(|l| (l - levels[1]).abs() < 4.0), "{levels:?}");
}

/// The bands' levels wander: the loudness of 50 ms windows changes by several dB.
#[test]
fn water_moves() {
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.preset", "water")]);
    let mut all = f.block("/a");
    run(&mut f, "/a", 300, &mut all);
    let windows: Vec<f64> = all[9600..].chunks(2400).map(|w| db(rms(w))).collect();
    let (lo, hi) = windows.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    eprintln!("water: 50 ms loudness from {lo:.1} to {hi:.1} dBFS");
    assert!(hi - lo > 3.0, "{lo} {hi}");
}

/// Rain: finite, below the ceiling, no DC, gapless at every density; more density gives a
/// louder rain, more brightness a higher one. The droplets' rate is tested in `rain`.
#[test]
fn rain_is_clean_and_density_and_brightness_steer_it() {
    let mut levels = Vec::new();
    let mut centres = Vec::new();
    for (d, b) in [(0.0, 0.5), (0.5, 0.5), (1.0, 0.5), (0.5, 0.0), (0.5, 1.0)] {
        let mut f = F::new(&[
            create("/a", AMBIENCE),
            set("/a.preset", "rain"),
            set("/a.density", d),
            set("/a.brightness", b),
        ]);
        let mut all = f.block("/a");
        run(&mut f, "/a", 240, &mut all);
        let tail = &all[9600..];
        let mean = tail.iter().sum::<f64>() / tail.len() as f64;
        assert!(all.iter().all(|s| s.is_finite() && s.abs() < CEILING), "{d} {b}");
        assert!(db(peak(tail)) < -6.0, "{d} {b}: {}", db(peak(tail)));
        assert!(mean.abs() < 0.002, "{d} {b}: DC {mean}");
        assert_gapless(&all, 800, "rain");
        levels.push(db(rms(tail)));
        centres.push(centroid(tail));
    }
    eprintln!(
        "rain: rms {levels:.1?} dBFS, mean frequency {centres:.0?} Hz at density 0, 0.5, 1 and brightness 0, 1"
    );
    assert!(levels[0] < levels[1] && levels[1] < levels[2], "{levels:?}");
    assert!(centres[3] < centres[1] && centres[1] < centres[4], "{centres:?}");
}

/// Four lanes held open (spread 2), then the samples of 80 frames with note and brightness
/// moving halfway.
fn chord(preset: &str, extra: &[Op]) -> Vec<f64> {
    let mut ops = lanes("/c", 4);
    ops.extend(ambience("/a", preset));
    ops.push(set("/a.spread", 2));
    ops.extend(extra.iter().cloned());
    ops.extend([
        set("/c.value0", 1.0),
        set("/c.value1", 1.0),
        set("/c.value2", 1.0),
        set("/c.value3", 1.0),
        connect("/c", "/a", 0),
    ]);
    let mut f = F::new(&ops);
    let mut all = f.block("/a");
    run(&mut f, "/a", 40, &mut all);
    f.edit(&[set("/a.note", 0.6), set("/a.brightness", 0.9)]);
    run(&mut f, "/a", 40, &mut all);
    all
}

/// At swarm 0 drone and pad sound as they did before swarm existed: these samples were taken
/// from the renderer without it.
#[test]
fn swarm_zero_is_the_plain_sound() {
    let reference: [(&str, [f64; 12], f64, f64); 2] = [
        (
            "drone",
            [
                6.912212432576408e-8,
                0.14121514640477784,
                0.13504945697771759,
                0.034872774914389414,
                -0.0601123878637803,
                -0.13437650128131226,
                -0.056667435949702194,
                0.07465618256455678,
                -0.14534533845848824,
                -0.16820644067215182,
                -0.2651389359966517,
                -0.10784696191037785,
            ],
            1.2136134977606352,
            1133.5552578656127,
        ),
        (
            "pad",
            [
                5.109629609148159e-9,
                -0.13729254014070197,
                0.1165009646859322,
                -0.21440472879638908,
                0.17688624821755952,
                0.18030998961866873,
                -0.0243402788395071,
                0.21051500237748283,
                0.3477776717536241,
                0.1451340525667034,
                -0.017521195992258584,
                -0.030401204974938454,
            ],
            -0.275567953215961,
            941.7660024711257,
        ),
    ];
    for (preset, picks, sum, squares) in reference {
        for extra in [vec![], vec![set("/a.swarm", 0.0)]] {
            let all = chord(preset, &extra);
            assert_eq!(all.len(), 64_800);
            for (k, p) in picks.iter().enumerate() {
                let s = all[k * all.len() / 12 + 7];
                assert!((s - p).abs() < 1e-12, "{preset} sample {k}: {s} against {p}");
            }
            assert!((all.iter().sum::<f64>() - sum).abs() < 1e-9, "{preset}");
            assert!((all.iter().map(|x| x * x).sum::<f64>() - squares).abs() < 1e-9, "{preset}");
        }
    }
}

/// The share of the power between 150 and 550 Hz that lies within 30 cents of C4, and the
/// power-weighted mean distance from C4 in cents.
fn near_c4(x: &[f64]) -> (f64, f64) {
    let c4 = 261.63;
    let (mut near, mut total, mut moment) = (0.0, 0.0, 0.0);
    let mut hz = 150.0;
    while hz <= 550.0 {
        let p = power_at(x, hz);
        let c = cents(hz, c4);
        total += p;
        moment += p * c;
        if c.abs() <= 30.0 {
            near += p;
        }
        hz += 1.0;
    }
    (near / total, moment / total)
}

/// One drone tone (C4) at swarm 0 has its power on the note; at swarm 1 it spreads around the
/// note at about the same loudness. Moving swarm back to 0 brings the pitch back onto C4 with
/// no step.
#[test]
fn swarm_spreads_around_the_note_and_comes_back() {
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.brightness", 0.0)]);
    let mut plain = f.block("/a");
    run(&mut f, "/a", 150, &mut plain);
    let (share0, _) = near_c4(&plain[48_000..]);
    let steady = worst_step(&plain[24_000..]);
    f.edit(&[set("/a.swarm", 1.0)]);
    let mut cloud = Vec::new();
    run(&mut f, "/a", 300, &mut cloud);
    let (share1, mean1) = near_c4(&cloud[48_000..144_000]);
    let (l0, l1) = (db(rms(&plain[48_000..])), db(rms(&cloud[48_000..])));
    eprintln!(
        "swarm: power within 30 cents of C4 {share0:.3} at 0, {share1:.3} at 1 (centred {mean1:.0} cents); rms {l0:.1} and {l1:.1} dBFS; worst step {:.4} against {steady:.4}",
        worst_step(&cloud)
    );
    assert!(share0 > 0.9 && share1 < 0.5, "{share0} {share1}");
    assert!(mean1.abs() < 300.0, "{mean1}");
    assert!((l0 - l1).abs() < 3.0, "{l0} {l1}");
    assert!(worst_step(&cloud) < 4.0 * steady, "{} against {steady}", worst_step(&cloud));
    f.edit(&[set("/a.swarm", 0.0)]);
    let mut back = Vec::new();
    run(&mut f, "/a", 120, &mut back);
    assert!(worst_step(&back) < 4.0 * steady, "{} against {steady}", worst_step(&back));
    let hz = frequency(&back[48_000..], 240.0, 280.0);
    assert!(cents(hz, 261.63).abs() < 5.0, "{hz}");
    let (share, _) = near_c4(&back[48_000..]);
    assert!(share > 0.9, "{share}");
}

/// Swarm 1 on a chord of drone and pad, and swarm moving on one tone: blocks join, nothing
/// reaches the ceiling, and the same frames again give the same samples.
#[test]
fn swarm_runs_on_across_blocks_and_repeats() {
    for preset in ["drone", "pad"] {
        let a = chord(preset, &[set("/a.swarm", 1.0)]);
        assert_gapless(&a[800..], 800, preset);
        assert!(a.iter().all(|s| s.is_finite() && s.abs() < CEILING), "{preset}");
        assert_eq!(a, chord(preset, &[set("/a.swarm", 1.0)]), "{preset}");
        let mut f = F::new(&[create("/a", AMBIENCE), set("/a.preset", preset), set("/a.swarm", 0.3)]);
        let mut all = f.block("/a");
        for k in 0..60 {
            f.edit(&[set("/a.swarm", f64::from(k % 20) / 19.0)]);
            run(&mut f, "/a", 2, &mut all);
        }
        assert_gapless(&all[800..], 800, preset);
    }
}

/// The cook's cost with 128 lanes: drone and pad at swarm 0 and 1 (eight voices each), noise
/// and water. Run with
/// `cargo test --release -p penumbra-ops measure_ambience_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_ambience_cost() {
    let cases = [
        ("drone", 0.0),
        ("drone", 1.0),
        ("pad", 0.0),
        ("pad", 1.0),
        ("noise", 0.0),
        ("water", 0.0),
        ("rain", 0.0),
    ];
    for (preset, swarm) in cases {
        let mut ops = lanes("/c", 64);
        ops.extend(lanes("/d", 64));
        ops.extend((0..64).map(|k| set(&format!("/d.name{k}"), format!("d{k}"))));
        ops.extend(
            (0..64).flat_map(|k| [set(&format!("/c.value{k}"), 1.0), set(&format!("/d.value{k}"), 1.0)]),
        );
        ops.extend([create("/m", "chan.merge"), connect("/c", "/m", 0), connect("/d", "/m", 1)]);
        ops.extend(ambience("/a", preset));
        ops.extend([set("/a.swarm", swarm), connect("/m", "/a", 0)]);
        let mut f = F::new(&ops);
        f.block("/a");
        run(&mut f, "/a", 30, &mut Vec::new());
        let frames = 120;
        let t = std::time::Instant::now();
        run(&mut f, "/a", frames, &mut Vec::new());
        let ms = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        eprintln!("cost: {preset} swarm {swarm}, 128 lanes at 48 kHz: {ms:.2} ms per 60 fps frame (16.7 ms)");
    }
}
