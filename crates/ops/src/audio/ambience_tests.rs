//! `audio.ambience`: gapless blocks, smooth glides, gate timing inside the frame, every
//! preset's level, hostile params; and an ignored test that writes listening files.

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
                set("/a.threshold", Value::from(-1e300)),
                connect("/c", "/a", 0),
            ]);
            let mut f = F::new(&ops);
            let mut all = f.block("/a");
            run(&mut f, "/a", 5, &mut all);
            f.edit(&[set("/a.octave", -1), set("/a.spread", -128), set("/a.note", -10.0)]);
            run(&mut f, "/a", 5, &mut all);
            assert!(all.iter().all(|s| s.is_finite() && s.abs() <= CEILING), "{preset} at {rate}");
        }
    }
    let mut f = F::new(&[create("/a", AMBIENCE), set("/a.name", "")]);
    assert!(f.cook("/a").is_err());
}

/// Listening files: each preset for 10 s with note and brightness rising slowly, and a pad
/// chord whose three gates open and close, in the listening folder. Run with
/// `cargo test -p penumbra-ops write_ambience_files -- --ignored`.
#[test]
#[ignore]
fn write_ambience_files() {
    let dir = std::path::Path::new(LISTEN_DIR);
    std::fs::create_dir_all(dir).unwrap();
    for preset in PRESETS {
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
}
