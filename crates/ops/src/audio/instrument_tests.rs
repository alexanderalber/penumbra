//! `audio.instrument`: pitch, timing inside the frame, gapless blocks, polyphony, every
//! preset's level, hostile params; and an ignored test that writes listening files.

use penumbra_core::{Rate, Value};
use penumbra_graph::Op;

use super::instrument::INSTRUMENT;
use super::instrument::voice::PRESETS;
use super::synth::CEILING;
use super::tests::{F, connect, create, set};

pub(super) const RATE: f64 = 48_000.0;
/// Where the listening files go.
pub(super) const LISTEN_DIR: &str = "E:/misc/temp/s99p3";

fn instrument(path: &str, preset: &str) -> Vec<Op> {
    vec![create(path, INSTRUMENT), set(&format!("{path}.preset"), preset)]
}

/// A `chan.constant` at `path` with `rows` channels, all 0.
pub(super) fn lanes(path: &str, rows: usize) -> Vec<Op> {
    vec![create(path, "chan.constant"), set(&format!("{path}.rows"), rows as i64)]
}

/// Cooks `frames` more frames of `path` and appends them to `all`.
pub(super) fn run(f: &mut F, path: &str, frames: usize, all: &mut Vec<f64>) {
    for _ in 0..frames {
        f.clock.advance(1);
        all.extend(f.block(path));
    }
}

pub(super) fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0_f64, |m, s| m.max(s.abs()))
}

pub(super) fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// The strongest frequency of `x` between `lo` and `hi` Hz: a Hann-windowed DFT searched in
/// 0.5 Hz steps, then 0.01 Hz steps around the best.
pub(super) fn frequency(x: &[f64], lo: f64, hi: f64) -> f64 {
    let n = x.len() as f64;
    let w: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, s)| s * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos()))
        .collect();
    let power = |f: f64| {
        let (c, s) = (std::f64::consts::TAU * f / RATE).sin_cos();
        let (mut re, mut im, mut pr, mut pi) = (0.0, 0.0, 1.0, 0.0);
        for v in &w {
            re += v * pi;
            im += v * pr;
            let r = pr * s - pi * c;
            pi = pr * c + pi * s;
            pr = r;
        }
        re * re + im * im
    };
    let best = |from: f64, to: f64, step: f64| {
        let mut f = from;
        let (mut at, mut most) = (from, -1.0);
        while f <= to {
            let p = power(f);
            if p > most {
                (at, most) = (f, p);
            }
            f += step;
        }
        at
    };
    let coarse = best(lo, hi, 0.5);
    best(coarse - 0.5, coarse + 0.5, 0.01)
}

pub(super) fn cents(f: f64, expect: f64) -> f64 {
    1200.0 * (f / expect).log2()
}

/// The samples where a note starts: the sample before the first non-zero one after silence
/// (a voice's first sample is 0, its attack starts there). Indexes into `all`.
pub(super) fn onsets(all: &[f64], min_gap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut zeros = 0;
    for (i, s) in all.iter().enumerate() {
        if *s == 0.0 {
            zeros += 1;
        } else {
            if zeros >= min_gap {
                out.push(i.saturating_sub(1));
            }
            zeros = 0;
        }
    }
    out
}

/// Key F, octave 3, spread 1: ten lanes of pulses play the benchmark's ten pitches.
#[test]
fn ten_lanes_play_f_major_pentatonic_in_tune() {
    let expect = [174.61, 196.00, 220.00, 261.63, 293.66, 349.23, 392.00, 440.00, 523.25, 587.33];
    for (lane, e) in expect.iter().enumerate() {
        let mut ops = lanes("/c", 10);
        ops.extend(instrument("/i", "pluck"));
        ops.extend([
            set("/i.key", "F"),
            set("/i.octave", 3),
            set("/i.brightness", 0.0),
            set("/i.decay", 2.0),
            connect("/c", "/i", 0),
        ]);
        let mut f = F::new(&ops);
        f.block("/i");
        f.edit(&[set(&format!("/c.value{lane}"), 1.0)]);
        let mut all = Vec::new();
        run(&mut f, "/i", 30, &mut all);
        // From 50 ms on, past the attack: the fundamental dominates at brightness 0.
        let hz = frequency(&all[2400..], e * 0.9, e * 1.1);
        let c = cents(hz, *e);
        eprintln!("lane {lane}: {hz:.2} Hz, {c:+.2} cents");
        assert!(c.abs() < 3.0, "lane {lane}: {hz} Hz against {e}");
    }
}

/// A sine `chan.lfo` at 2.5 Hz crosses 0 every 0.2 s: with `trigger: both` each crossing starts
/// a note within well under a millisecond of it, though the lfo has one sample per frame. A
/// phase shift puts the crossings between frames.
#[test]
fn notes_start_at_the_crossing_inside_the_frame() {
    let phase = 0.0123;
    let ops = vec![
        create("/l", "chan.lfo"),
        set("/l.rate", 2.5),
        set("/l.phase", phase),
        create("/i", INSTRUMENT),
        set("/i.trigger", "both"),
        set("/i.decay", 0.02),
        set("/i.level", 0.0),
        connect("/l", "/i", 0),
    ];
    let mut f = F::new(&ops);
    // Frame 0: a fresh block of samples -800..0.
    let mut all = f.block("/i");
    let first = -800_i64;
    run(&mut f, "/i", 180, &mut all);
    let found = onsets(&all, 500);
    // 2.5 t + phase = k / 2: crossings at 0.2 k - phase / 2.5, k = 1, 2, ... up to 3 s.
    let expect: Vec<f64> =
        (1..20).map(|k| (0.2 * k as f64 - phase / 2.5) * RATE).filter(|s| *s < 3.0 * RATE).collect();
    assert_eq!(found.len(), expect.len(), "{found:?}");
    let mut worst = 0.0_f64;
    for (&i, e) in found.iter().zip(&expect) {
        let err = (i as i64 + first) as f64 - e;
        worst = worst.max(err.abs());
    }
    let ms = worst / RATE * 1000.0;
    eprintln!("worst onset error {worst:.2} samples = {ms:.4} ms over {} notes", found.len());
    assert!(ms < 0.5, "{ms} ms");
}

/// A pulse from 0 to 1 only knows its frame: its note starts at the block's first sample.
#[test]
fn a_pulse_starts_its_note_at_the_block_start() {
    let mut ops = lanes("/c", 1);
    ops.extend(instrument("/i", "pluck"));
    ops.push(connect("/c", "/i", 0));
    let mut f = F::new(&ops);
    f.block("/i");
    f.clock.advance(1);
    assert!(f.block("/i").iter().all(|s| *s == 0.0));
    f.edit(&[set("/c.value0", 1.0)]);
    f.clock.advance(1);
    let b = f.block("/i");
    assert_eq!(b[0], 0.0);
    assert!(b[1] != 0.0, "starts at sample 0 of the block");
    // Staying at 1 plays no further note; going back to 0 and up again does.
    let mut more = Vec::new();
    run(&mut f, "/i", 120, &mut more);
    assert_eq!(onsets(&more, 2000).len(), 0);
}

/// The step from one block's last sample to the next block's first is like the steps inside.
pub(super) fn assert_gapless(all: &[f64], block: usize, what: &str) {
    let steps: Vec<f64> = all.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    let inside =
        steps.iter().enumerate().filter(|(i, _)| (i + 1) % block != 0).map(|(_, s)| *s).fold(0.0, f64::max);
    let edge =
        steps.iter().enumerate().filter(|(i, _)| (i + 1) % block == 0).map(|(_, s)| *s).fold(0.0, f64::max);
    assert!(inside > 0.0, "{what}: silent");
    assert!(edge <= inside * 1.01, "{what}: {edge} at a block edge, {inside} inside");
}

/// A recook at the same tick gives the same block; after a seek the voices are gone.
#[test]
fn again_repeats_and_a_seek_drops_the_voices() {
    let mut ops = lanes("/c", 1);
    ops.extend(instrument("/i", "bell"));
    ops.push(connect("/c", "/i", 0));
    let mut f = F::new(&ops);
    f.block("/i");
    f.edit(&[set("/c.value0", 1.0)]);
    f.clock.advance(1);
    let a = f.block("/i");
    // Something else changes, so the node cooks again at the same tick.
    f.edit(&[set("/i.threshold", 0.0001)]);
    let again = f.block("/i");
    assert_eq!(a, again);
    assert!(peak(&a) > 0.01);
    f.clock.advance(1);
    assert!(peak(&f.block("/i")) > 0.01, "the bell rings on");
    f.clock.seek(0);
    assert!(f.block("/i").iter().all(|s| *s == 0.0), "a fresh block has no voices");
    // History is dropped too: the lane at 1 since before the seek starts nothing.
    f.clock.advance(1);
    assert!(f.block("/i").iter().all(|s| *s == 0.0));
}

/// More notes than `voices`: the oldest fade out in 5 ms, without a jump.
#[test]
fn stolen_voices_fade_without_a_click() {
    let mut ops = lanes("/c", 8);
    ops.extend(instrument("/i", "pluck"));
    ops.extend([
        set("/i.voices", 2),
        set("/i.octave", 2),
        set("/i.decay", 10.0),
        set("/i.brightness", 0.0),
        set("/i.level", 0.0),
        connect("/c", "/i", 0),
    ]);
    let mut f = F::new(&ops);
    f.block("/i");
    let mut all = Vec::new();
    let mut alone = 0.0_f64;
    for k in 0..8 {
        f.edit(&[set(&format!("/c.value{k}"), 1.0)]);
        run(&mut f, "/i", 6, &mut all);
        if k == 0 {
            alone = peak(&all);
        }
    }
    let worst = all.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
    // Two low plucks near 65 to 150 Hz: a step of a sample is a few percent of the peak.
    eprintln!("steal: worst step {worst:.4}, one voice peaks {alone:.3}, all {:.3}", peak(&all));
    assert!(worst < 0.1 * alone, "{worst}");
    // Two voices at most: never much louder than two together.
    assert!(peak(&all[6 * 800 * 3..]) < 2.2 * alone);
}

/// Every preset: finite, a single note at the default level well under -6 dBFS, and silent
/// once it has rung out. 32 notes at once at 0 dB stay under full scale.
#[test]
fn every_preset_is_finite_moderate_and_ends() {
    for preset in PRESETS {
        let mut one = lanes("/c", 1);
        one.extend(instrument("/i", preset));
        one.push(connect("/c", "/i", 0));
        let mut ops = lanes("/c", 32);
        ops.extend(instrument("/i", preset));
        ops.push(connect("/c", "/i", 0));
        let mut f = F::new(&one);
        f.block("/i");
        f.edit(&[set("/c.value0", 1.0)]);
        let mut all = Vec::new();
        // The bell rings about 2.4 s to -20 dB, the sweep lasts 3 s: 15 s is past all of them.
        run(&mut f, "/i", 900, &mut all);
        assert!(all.iter().all(|s| s.is_finite()), "{preset}");
        let p = peak(&all);
        let mean = all.iter().sum::<f64>() / all.len() as f64;
        eprintln!("{preset}: one note peaks {:.1} dBFS at -12 dB, mean {mean:.1e}", db(p));
        assert!(p > 0.01 && db(p) < -6.0, "{preset}: {p}");
        let tail = &all[all.len() - 4800..];
        assert!(tail.iter().all(|s| *s == 0.0), "{preset} still sounds: {}", peak(tail));
        // 32 notes at once, at 0 dB.
        let mut f = F::new(&ops);
        f.edit(&[set("/i.level", 0.0)]);
        f.block("/i");
        let all32: Vec<Op> = (0..32).map(|k| set(&format!("/c.value{k}"), 1.0)).collect();
        f.edit(&all32);
        let mut all = Vec::new();
        run(&mut f, "/i", 60, &mut all);
        let p = peak(&all);
        eprintln!("{preset}: 32 notes peak {:.2} dBFS at 0 dB", db(p));
        assert!(p <= CEILING && all.iter().all(|s| s.is_finite()), "{preset}: {p}");
    }
}

/// Hostile params: extreme rates and pitches, a huge spread over 128 lanes, an empty name.
#[test]
fn hostile_params_do_not_break_it() {
    for rate in [1_i64, 8_000, 192_000, i64::MAX] {
        for preset in PRESETS {
            let mut ops = lanes("/a", 64);
            ops.extend(lanes("/b", 64));
            ops.extend((0..64).map(|k| set(&format!("/b.name{k}"), format!("b{k}"))));
            ops.extend([create("/m", "chan.merge"), connect("/a", "/m", 0), connect("/b", "/m", 1)]);
            ops.extend(instrument("/i", preset));
            ops.extend([
                set("/i.rate", rate),
                set("/i.octave", 9),
                set("/i.spread", 128),
                set("/i.note", 10.0),
                set("/i.range", 10.0),
                set("/i.decay", 1e9),
                set("/i.level", 1e9),
                set("/i.voices", 1_000_000),
                set("/i.brightness", -5.0),
                set("/i.threshold", Value::from(-1e300)),
                connect("/m", "/i", 0),
            ]);
            let mut f = F::new(&ops);
            f.block("/i");
            let up: Vec<Op> = (0..64)
                .flat_map(|k| [set(&format!("/a.value{k}"), 1e300), set(&format!("/b.value{k}"), 1e300)])
                .collect();
            f.edit(&up);
            let mut all = Vec::new();
            run(&mut f, "/i", 5, &mut all);
            assert!(all.iter().all(|s| s.is_finite() && s.abs() <= CEILING), "{preset} at {rate}");
            // Low octave and spread 0: everything below 8 Hz is dropped, not played.
            f.edit(&[set("/i.octave", -1), set("/i.spread", -128), set("/i.note", -10.0)]);
            run(&mut f, "/i", 3, &mut all);
            assert!(all.iter().all(|s| s.is_finite()));
        }
    }
    let mut f = F::new(&[create("/i", INSTRUMENT), set("/i.name", "  ")]);
    assert!(f.cook("/i").is_err());
    // A frame rate far above the sample rate still makes a sample per cook.
    let mut f = F::at(Rate::fps(1000).unwrap(), &[create("/i", INSTRUMENT), set("/i.rate", 8000)]);
    for _ in 0..20 {
        f.clock.advance(1);
        assert!(!f.block("/i").is_empty());
    }
}

/// Listening files: one WAV per preset and the pendulum wave, in `E:/misc/temp/s99p3/`. Run
/// with `cargo test -p penumbra-ops write_listening_files -- --ignored`.
#[test]
#[ignore]
fn write_listening_files() {
    let dir = std::path::Path::new(LISTEN_DIR);
    std::fs::create_dir_all(dir).unwrap();
    // 150 BPM: a beat every 0.4 s, 24 frames at 60 fps.
    for preset in PRESETS {
        let mut ops = lanes("/c", 10);
        ops.extend(instrument("/i", preset));
        ops.extend([set("/i.key", "F"), set("/i.octave", 3), connect("/c", "/i", 0)]);
        let mut f = F::new(&ops);
        f.block("/i");
        let mut all = Vec::new();
        match preset {
            "kick" | "hat" | "sweep" => {
                // Kick on every beat, hat on every eighth, two sweeps of 4 s.
                let (hits, frames) = match preset {
                    "kick" => (16, 24),
                    "hat" => (32, 12),
                    _ => (2, 240),
                };
                for _ in 0..hits {
                    f.edit(&[set("/c.value0", 1.0)]);
                    run(&mut f, "/i", 1, &mut all);
                    f.edit(&[set("/c.value0", 0.0)]);
                    run(&mut f, "/i", frames - 1, &mut all);
                }
                run(&mut f, "/i", 60, &mut all);
            }
            _ => {
                // The ten notes up and down, one every 12 frames (0.2 s).
                let order: Vec<usize> = (0..10).chain((0..9).rev()).collect();
                let mut state = [0.0; 10];
                for k in order {
                    state[k] = 1.0 - state[k];
                    // A lane crosses upwards only from 0: give it a frame at 0 first.
                    if state[k] == 0.0 {
                        f.edit(&[set(&format!("/c.value{k}"), 0.0)]);
                        run(&mut f, "/i", 1, &mut all);
                        state[k] = 1.0;
                    }
                    f.edit(&[set(&format!("/c.value{k}"), 1.0)]);
                    run(&mut f, "/i", 12, &mut all);
                }
                let tail = if preset == "bell" { 300 } else { 90 };
                run(&mut f, "/i", tail, &mut all);
            }
        }
        write_wav(&dir.join(format!("instrument_{preset}.wav")), &all);
    }
    // The pendulum wave: ring k swings (80 - k) times in 64 s and crosses the bottom twice per
    // swing. Lane i is ring 9 - i, so lane 0 (F3) is the slowest, lane 9 (D5) the fastest.
    let mut ops = Vec::new();
    for i in 0..10 {
        let p = format!("/l{i}");
        ops.extend([
            create(&p, "chan.lfo"),
            set(&format!("{p}.rate"), (71 + i) as f64 / 64.0),
            set(&format!("{p}.name"), format!("ring{}", 9 - i)),
            connect(&p, "/m", i as u32),
        ]);
    }
    ops.insert(0, create("/m", "chan.merge"));
    ops.extend(instrument("/i", "pluck"));
    ops.extend([
        set("/i.key", "F"),
        set("/i.octave", 3),
        set("/i.trigger", "both"),
        set("/i.level", -22.0),
        connect("/m", "/i", 0),
    ]);
    ops.extend(instrument("/k", "kick"));
    ops.extend([set("/k.trigger", "both"), set("/k.level", -12.0), connect("/l9", "/k", 0)]);
    ops.extend([create("/x", "audio.mix"), connect("/i", "/x", 0), connect("/k", "/x", 1)]);
    let mut f = F::new(&ops);
    f.block("/x");
    let mut all = Vec::new();
    run(&mut f, "/x", 20 * 60, &mut all);
    eprintln!("pendulum: peak {:.1} dBFS", db(peak(&all)));
    write_wav(&dir.join("pendulum.wav"), &all);
}

/// A mono 16-bit WAV at 48 kHz.
pub(super) fn write_wav(path: &std::path::Path, x: &[f64]) {
    let data_len = (x.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + x.len() * 2);
    b.extend(b"RIFF");
    b.extend((36 + data_len).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(48_000u32.to_le_bytes());
    b.extend((48_000u32 * 2).to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data_len.to_le_bytes());
    for s in x {
        b.extend(((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
    eprintln!("wrote {} ({:.1} s)", path.display(), x.len() as f64 / RATE);
}
