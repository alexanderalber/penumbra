//! The Beat This! signal chain: spectrogram parity with torchaudio, the resampler's timing,
//! the chunk plan and stitching, the peaks against upstream's post-processing, hostile input
//! and the beat grid. The fixtures in `tests/fixtures/beats/` come from
//! `make_fixtures.py` there.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::beats::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beats").join(name)
}

fn read_f32(p: &Path) -> Vec<f32> {
    let b = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn read_f64(p: &Path) -> Vec<f64> {
    let b = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    b.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect()
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn mono_spect(x: &[f32], rate: f64) -> Spectrogram {
    spectrogram(x, 1, rate, None).unwrap()
}

/// A small deterministic random generator (xorshift).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[test]
fn spectrogram_matches_torchaudio() {
    for name in ["synth", "short"] {
        let x = read_f32(&fixture(&format!("{name}.pcm.f32")));
        let want = read_f32(&fixture(&format!("{name}.spect.f32")));
        let got = mono_spect(&x, SAMPLE_RATE);
        assert_eq!(got.frames(), frame_count(x.len()), "{name}");
        assert_eq!(got.frames() * N_MELS, want.len(), "{name}");
        let d = max_diff(got.data(), &want);
        eprintln!("{name}: {} frames, max |diff| {d:e}", got.frames());
        assert!(d < 2e-4, "{name}: max |diff| {d}");
    }
}

#[test]
fn blocks_do_not_change_the_spectrogram() {
    let x = read_f32(&fixture("synth.pcm.f32"));
    let stereo: Vec<f32> = x.iter().flat_map(|v| [*v * 0.5, *v * 1.5]).collect();
    for rate in [SAMPLE_RATE, 44_100.0, 48_000.0] {
        let whole = spectrogram(&stereo, 2, rate, None).unwrap();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut fe = Frontend::new(rate, 2).unwrap();
        let mut at = 0;
        while at < stereo.len() {
            // Odd lengths, so blocks also end inside a frame.
            let n = (1 + rng.below(3000)).min(stereo.len() - at);
            fe.push_interleaved(&stereo[at..at + n]);
            at += n;
        }
        let parts = fe.finish();
        assert_eq!(whole, parts, "{rate}");
        let mut fe = Frontend::new(rate, 2).unwrap();
        let (l, r): (Vec<f32>, Vec<f32>) = x.iter().map(|v| (*v * 0.5, *v * 1.5)).unzip();
        for (a, b) in l.chunks(777).zip(r.chunks(777)) {
            fe.push_planar(&[a, b]).unwrap();
        }
        assert_eq!(whole, fe.finish(), "planar {rate}");
    }
}

/// The resampled sound keeps its timing: a tone converted to 22.05 kHz equals the same tone
/// made at 22.05 kHz, and an impulse lands where it was.
#[test]
fn resampling_has_no_delay() {
    for from in [8_000.0, 16_000.0, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
        let f = 1_000.0;
        let n = (from * 2.0) as usize;
        let x: Vec<f32> =
            (0..n).map(|i| (std::f64::consts::TAU * f * i as f64 / from).sin() as f32 * 0.5).collect();
        let y = super::beats::resample_for_tests(&x, from);
        assert_eq!(y.len(), (n as f64 * SAMPLE_RATE / from).round() as usize, "{from}");
        let edge = (0.1 * SAMPLE_RATE) as usize;
        let mut worst = 0.0f64;
        for (i, v) in y.iter().enumerate().take(y.len() - edge).skip(edge) {
            let want = 0.5 * (std::f64::consts::TAU * f * i as f64 / SAMPLE_RATE).sin();
            worst = worst.max((f64::from(*v) - want).abs());
        }
        // A lag of d output samples gives an error up to 0.5 * 2 pi f d / rate: 1e-3 is a
        // lag below 0.007 samples (0.3 us).
        eprintln!("{from} Hz: tone error {worst:e}");
        assert!(worst < 1e-3, "{from}: {worst}");
    }
    // One-sample impulses at 44.1 kHz: every second source sample is an output sample.
    for k in [10_000usize, 10_001] {
        let mut x = vec![0.0f32; 44_100];
        x[k] = 1.0;
        let y = super::beats::resample_for_tests(&x, 44_100.0);
        let e: f64 = y.iter().map(|v| f64::from(*v).powi(2)).sum();
        let centre: f64 =
            y.iter().enumerate().map(|(i, v)| i as f64 * f64::from(*v).powi(2)).sum::<f64>() / e;
        eprintln!("impulse 44100 Hz at {k}: centre {centre:.6}, want {}", k as f64 / 2.0);
        assert!((centre - k as f64 / 2.0).abs() < 1e-3, "{k}: {centre}");
    }
    // Clicks between source samples at other rates. A click of a third of a millisecond has
    // nothing above the new Nyquist frequency, so its centre is its time, at any rate.
    for (from, at) in [(48_000.0, 0.2571234), (96_000.0, 0.3472), (32_000.0, 0.41111), (8_000.0, 0.5003)] {
        let sigma = 0.3e-3;
        let x: Vec<f32> = (0..(from as usize))
            .map(|i| (-(i as f64 / from - at).powi(2) / (2.0 * sigma * sigma)).exp() as f32)
            .collect();
        let y = super::beats::resample_for_tests(&x, from);
        let e: f64 = y.iter().map(|v| f64::from(*v)).sum();
        let centre: f64 = y.iter().enumerate().map(|(i, v)| i as f64 * f64::from(*v)).sum::<f64>() / e;
        let want = at * SAMPLE_RATE;
        eprintln!("click {from} Hz at {at} s: centre {centre:.5}, want {want:.5}");
        assert!((centre - want).abs() < 0.01, "{from} {at}: {centre} vs {want}");
    }
}

#[test]
fn the_front_end_passes_resampled_samples_on() {
    let x: Vec<f32> = (0..48_000).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
    let y = super::beats::resample_for_tests(&x, 48_000.0);
    assert_eq!(mono_spect(&x, 48_000.0), mono_spect(&y, SAMPLE_RATE));
}

#[test]
fn chunk_plans_match_upstream() {
    let text = std::fs::read_to_string(fixture("chunk_plans.txt")).unwrap();
    let mut lines = 0;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let n: usize = it.next().unwrap().parse().unwrap();
        let want: Vec<Chunk> = it
            .map(|p| {
                let (s, f) = p.split_once(':').unwrap();
                Chunk { start: s.parse().unwrap(), frames: f.parse().unwrap() }
            })
            .collect();
        assert_eq!(ChunkPlan::new(n).chunks(), &want[..], "{n} frames");
        lines += 1;
    }
    assert!(lines > 20);
    assert!(ChunkPlan::new(0).is_empty());
}

/// A network that answers each frame with its spectrogram frame index (read from the input,
/// whose first band holds it) and each chunk with its own index.
#[test]
fn stitching_puts_every_frame_back() {
    for n in [1usize, 5, 1488, 1489, 1500, 2976, 2977, 4000, 10_000] {
        let rows: Vec<f32> =
            (0..n).flat_map(|f| std::iter::once(f as f32 + 1.0).chain([0.0; N_MELS - 1])).collect();
        let spect = Spectrogram::from_rows(rows);
        let plan = ChunkPlan::new(n);
        let mut stitcher = plan.stitcher();
        // Out of order, as parallel workers would finish.
        for k in (0..plan.len()).rev() {
            let input = plan.input(&spect, k).unwrap();
            let c = plan.chunks()[k];
            assert_eq!(input.len(), c.frames * N_MELS);
            let beat: Vec<f32> = input.as_chunks::<N_MELS>().0.iter().map(|r| r[0] - 1.0).collect();
            let down = vec![k as f32; c.frames];
            stitcher.put(k, beat, down).unwrap();
        }
        let logits = stitcher.finish().unwrap();
        assert_eq!(logits.beat.len(), n);
        for (f, v) in logits.beat.iter().enumerate() {
            assert_eq!(*v, f as f32, "{n} frames, frame {f}");
        }
        // keep_first: the first chunk whose kept part covers the frame.
        for (f, v) in logits.downbeat.iter().enumerate() {
            let first = plan
                .chunks()
                .iter()
                .position(|c| {
                    let f = f as isize;
                    f >= c.start + BORDER_FRAMES as isize
                        && f < c.start + c.frames as isize - BORDER_FRAMES as isize
                })
                .unwrap();
            assert_eq!(*v, first as f32, "{n} frames, frame {f}");
        }
    }
}

#[test]
fn stitching_refuses_wrong_output() {
    let plan = ChunkPlan::new(3000);
    let mut s = plan.stitcher();
    assert!(s.put(0, vec![0.0; 10], vec![0.0; 1500]).is_err());
    assert!(s.put(9, vec![0.0; 1500], vec![0.0; 1500]).is_err());
    s.put(0, vec![0.0; 1500], vec![0.0; 1500]).unwrap();
    assert_eq!(s.missing(), plan.len() - 1);
    assert!(s.finish().is_err());
    let mut s = ChunkPlan::new(100).stitcher();
    s.put(0, vec![f32::NAN; 112], vec![f32::INFINITY; 112]).unwrap();
    let l = s.finish().unwrap();
    assert!(l.beat.iter().all(|v| *v == NO_BEAT));
    assert_eq!(pick(&l.beat, &l.downbeat).beats.len(), 0);
}

fn deinterleave(v: &[f32]) -> (Vec<f32>, Vec<f32>) {
    v.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])).unzip()
}

#[test]
fn peaks_match_upstream() {
    let (b, d) = deinterleave(&read_f32(&fixture("post.logits.f32")));
    let got = pick(&b, &d);
    assert_eq!(got.beats, read_f64(&fixture("post.beats.f64")));
    assert_eq!(got.downbeats, read_f64(&fixture("post.downs.f64")));
    // Merged plateaus give half frames; the fixture has some.
    assert!(got.beats.iter().any(|t| ((t * FPS).fract() - 0.5).abs() < 1e-9));
}

#[test]
fn peaks_edge_cases() {
    assert_eq!(pick(&[], &[]), Beats::default());
    // No beats: the downbeats stay where they were, as upstream.
    let mut d = vec![-5.0f32; 100];
    d[40] = 1.0;
    assert_eq!(pick(&vec![-5.0; 100], &d).downbeats, vec![0.8]);
    // Two downbeats near one beat become one.
    let mut b = vec![-5.0f32; 100];
    b[50] = 1.0;
    let mut d = vec![-5.0f32; 100];
    d[10] = 1.0;
    d[90] = 1.0;
    assert_eq!(pick(&b, &d), Beats { beats: vec![1.0], downbeats: vec![1.0], downbeat_logits: vec![-5.0] });
    // Beat and downbeat tracks of different lengths do not panic.
    let _ = pick(&b, &d[..3]);
}

/// `pick_final` on the first `f` frames against `pick` on all: its beats and downbeats begin
/// the whole piece's, and its grid gives the whole piece's positions before `final_until`.
fn check_partial(b: &[f32], d: &[f32], step: usize) -> usize {
    let full = pick(b, d);
    let grid = BeatGrid::from_beats(&full);
    let mut with_until = 0;
    for f in (0..=b.len()).step_by(step) {
        let p = pick_final(&b[..f], &d[..f]);
        assert!(p.beats.beats.len() <= full.beats.len(), "{f}");
        assert_eq!(p.beats.beats[..], full.beats[..p.beats.beats.len()], "beats at {f} frames");
        assert_eq!(
            p.beats.downbeats[..],
            full.downbeats[..p.beats.downbeats.len()],
            "downbeats at {f} frames"
        );
        let Some(until) = p.final_until else { continue };
        with_until += 1;
        assert!(until <= f as f64 / FPS, "{until} beyond {f} frames");
        assert_eq!(
            p.beats.downbeat_logits[..],
            full.downbeat_logits[..p.beats.downbeat_logits.len()],
            "logits at {f} frames"
        );
        let part = BeatGrid::from_beats(&p.beats);
        let mut t = -2.0;
        while t < until {
            // The tempo near the end of a part still lacks the beats after it.
            let no_bpm = |p: Option<Position>| p.map(|p| Position { bpm: 0.0, ..p });
            assert_eq!(no_bpm(part.at(t)), no_bpm(grid.at(t)), "t {t} with {f} frames final");
            t += 0.013;
        }
    }
    with_until
}

#[test]
fn partial_beats_begin_the_final_ones() {
    // Real logits, a minute of them four times over: the folded beats' tempo needs a minute
    // after each beat, so a single minute is never final before its end.
    let (b1, d1) = deinterleave(&read_f32(&fixture("post.logits.f32")));
    let (b, d): (Vec<f32>, Vec<f32>) = (b1.repeat(4), d1.repeat(4));
    let n = check_partial(&b, &d, 23);
    assert!(n > 100, "final_until was set {n} times");
    // The whole piece known but the last few frames: nearly every beat is final.
    let p = pick_final(&b, &d);
    assert!(p.beats.beats.len() + 1 >= pick(&b, &d).beats.len());
    // Random logits with plateaus (merged peaks), beats and downbeats close together.
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..4 {
        let len = 6000 + rng.below(1000);
        let mut b = vec![-3.0f32; len];
        let mut d = vec![-3.0f32; len];
        let mut i = rng.below(10);
        while i + 3 < len {
            let v = 1.0 + rng.below(4) as f32;
            for k in 0..=rng.below(3) {
                b[i + k] = v;
            }
            if rng.below(3) == 0 {
                d[i + rng.below(5)] = 2.0;
            }
            i += 3 + rng.below(30);
        }
        assert!(check_partial(&b, &d, 13) > 0);
    }
    // Nothing yet.
    assert_eq!(pick_final(&[], &[]), Partial::default());
}

#[test]
fn hostile_input_does_not_panic() {
    for (rate, ch) in
        [(f64::NAN, 1), (f64::INFINITY, 1), (0.0, 1), (-44_100.0, 1), (1e9, 1), (44_100.0, 0), (44_100.0, 65)]
    {
        assert!(matches!(Frontend::new(rate, ch), Err(BeatError::Input(_))), "{rate} {ch}");
    }
    for rate in [1_000.0, 22_050.0, 44_100.0, 768_000.0] {
        for n in [0usize, 1, 2, 100, 512, 513, 514, 1500] {
            let x = vec![0.25f32; n];
            let s = mono_spect(&x, rate);
            let at_model = (n as f64 * SAMPLE_RATE / rate).round() as usize;
            let want = if rate == SAMPLE_RATE { frame_count(n) } else { frame_count(at_model) };
            assert_eq!(s.frames(), want, "{rate} Hz, {n} samples");
        }
    }
    // Non-numbers count as silence; huge values stay finite.
    let mut x = read_f32(&fixture("synth.pcm.f32"));
    let clean = {
        let mut c = x.clone();
        for i in [5, 1000, 20_000, 30_000] {
            c[i] = 0.0;
        }
        c
    };
    x[5] = f32::NAN;
    x[1000] = f32::INFINITY;
    x[20_000] = f32::NEG_INFINITY;
    x[30_000] = f32::NAN;
    assert_eq!(mono_spect(&x, SAMPLE_RATE), mono_spect(&clean, SAMPLE_RATE));
    let loud = vec![f32::MAX; 30_000];
    assert!(mono_spect(&loud, 44_100.0).data().iter().all(|v| v.is_finite()));
    // Many channels, a block ending inside a frame.
    let mut fe = Frontend::new(48_000.0, 64).unwrap();
    fe.push_interleaved(&vec![0.1; 64 * 1000 + 17]);
    fe.push_interleaved(&vec![0.1; 64 * 1000 - 17]);
    let z: &[f32] = &[0.0; 4];
    assert!(fe.push_planar(&[z; 63]).is_err());
    let s = fe.finish();
    assert_eq!(s.frames(), frame_count((2000.0 * SAMPLE_RATE / 48_000.0f64).round() as usize));
    let mut fe = Frontend::new(48_000.0, 2).unwrap();
    assert!(fe.push_planar(&[&[0.0; 4], &[0.0; 5]]).is_err());
    fe.push_interleaved(&[0.0; 3]);
    assert!(fe.push_planar(&[&[0.0; 4], &[0.0; 4]]).is_err());
}

#[test]
fn track_runs_the_chain() {
    let never = |_: &[f32], _: usize| -> Result<(Vec<f32>, Vec<f32>), String> { panic!("no chunk expected") };
    assert_eq!(track(&[], 2, 44_100.0, never, None, &mut |_, _| {}).unwrap(), Beats::default());
    // 70 s of sound: 3501 frames, 3 chunks. Beats every 25 frames, downbeats every 100.
    let x = vec![0.0f32; 70 * 44_100];
    let mut calls = Vec::new();
    let net = |rows: &[f32], frames: usize| -> Result<(Vec<f32>, Vec<f32>), String> {
        assert_eq!(rows.len(), frames * N_MELS);
        Ok((vec![-1.0; frames], vec![-1.0; frames]))
    };
    let beats = track(&x, 1, 44_100.0, net, None, &mut |d, t| calls.push((d, t))).unwrap();
    assert_eq!(calls, vec![(1, 3), (2, 3), (3, 3)]);
    assert!(beats.beats.is_empty());
    let spect = mono_spect(&x, 44_100.0);
    assert_eq!(spect.frames(), 3501);
    let mut offset = 0;
    let net = |_: &[f32], frames: usize| -> Result<(Vec<f32>, Vec<f32>), String> {
        // Only the chunk start matters: chunks are [-6, 1482, 2007].
        let start = [-6isize, 1482, 2007][offset];
        offset += 1;
        let at = |div: isize| {
            (0..frames).map(|i| if (start + i as isize).rem_euclid(div) == 0 { 2.0 } else { -2.0 }).collect()
        };
        Ok((at(25), at(100)))
    };
    let got = run_network(&spect, net, None, &mut |_, _| {}).unwrap();
    let beats = pick(&got.beat, &got.downbeat);
    assert_eq!(beats.beats.len(), 141);
    assert_eq!(beats.downbeats.len(), 36);
    assert!(beats.beats.iter().enumerate().all(|(i, t)| (t - i as f64 * 0.5).abs() < 1e-12));
    // Errors and cancelling.
    let failing =
        |_: &[f32], _: usize| -> Result<(Vec<f32>, Vec<f32>), String> { Err("out of memory".into()) };
    assert!(
        matches!(run_network(&spect, failing, None, &mut |_, _| {}), Err(BeatError::Network(e)) if e.contains("memory"))
    );
    let short =
        |_: &[f32], _: usize| -> Result<(Vec<f32>, Vec<f32>), String> { Ok((vec![0.0; 3], vec![0.0; 3])) };
    assert!(matches!(run_network(&spect, short, None, &mut |_, _| {}), Err(BeatError::Network(_))));
    let stop = AtomicBool::new(true);
    assert_eq!(track(&x, 1, 44_100.0, never, Some(&stop), &mut |_, _| {}), Err(BeatError::Cancelled));
    let stop = AtomicBool::new(false);
    let mut n = 0;
    let net = |_: &[f32], frames: usize| -> Result<(Vec<f32>, Vec<f32>), String> {
        Ok((vec![0.0; frames], vec![0.0; frames]))
    };
    let r = run_network(&spect, net, Some(&stop), &mut |_, _| {
        n += 1;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    assert_eq!(r, Err(BeatError::Cancelled));
    assert_eq!(n, 1);
}

#[test]
fn beat_grid_positions() {
    // Beats every 0.5 s from 1 s, 20 of them; downbeats on beats 2, 6, 9 (a bar of 3), 13.
    let beats: Vec<f64> = (0..20).map(|i| 1.0 + 0.5 * i as f64).collect();
    let downs = [beats[2] + 0.01, beats[6], beats[9] - 0.02, beats[13]];
    let g = BeatGrid::new(&beats, &downs);
    assert_eq!(g.downbeats(), vec![beats[2], beats[6], beats[9], beats[13]]);
    let p = g.at(1.0 + 0.5 * 7.25).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.beats_in_bar, p.inside), (7, 1, 1, 3, true));
    assert!((p.beat_phase - 0.25).abs() < 1e-9 && (p.to_next_beat - 0.375).abs() < 1e-9);
    assert!((p.bar_phase - 1.25 / 3.0).abs() < 1e-9);
    let p = g.at(beats[6]).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.beat_phase), (6, 1, 0, 0.0));
    // Before the first beat: the first spacing and the first bar's length (4) go backwards.
    let p = g.at(0.25).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.beats_in_bar, p.inside), (-2, -1, 0, 4, false));
    assert!((p.beat_phase - 0.5).abs() < 1e-9);
    // After the last downbeat bars keep the last length (13 - 9 = 4); after the last beat
    // the last spacing continues.
    let p = g.at(beats[19]).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.inside), (19, 4, 2, true));
    let p = g.at(beats[19] + 1.1).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.inside), (21, 5, 0, false));
    // Without downbeats, bars of four from the first beat.
    let g = BeatGrid::new(&beats, &[]);
    let p = g.at(1.0 + 0.5 * 5.5).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_in_bar, p.beats_in_bar), (5, 1, 1, 4));
    // One downbeat: bars of four on either side of it.
    let g = BeatGrid::new(&beats, &[beats[3]]);
    assert_eq!(g.at(beats[1]).unwrap().beat_in_bar, 2);
    assert_eq!(g.at(beats[8]).unwrap().bar, 1);
    // Too few beats, non-numbers, far away.
    assert!(BeatGrid::new(&[], &[]).at(1.0).is_none());
    assert!(BeatGrid::new(&[1.0, 1.0, f64::NAN], &[1.0]).at(1.0).is_none());
    let g = BeatGrid::new(&[1.0, f64::INFINITY, 1.5, 2.0], &[f64::NAN, 2.0]);
    assert!(g.at(f64::NAN).is_none());
    for t in [-1e300, -1e18, 1e18, 1e300] {
        let p = g.at(t).unwrap();
        assert!((0.0..1.0).contains(&p.beat_phase) && (0.0..1.0).contains(&p.bar_phase), "{t}");
    }
}

#[test]
fn a_looped_piece_wraps_to_its_first_beat() {
    // Eight beats every 0.5 s from 0.25 s in a loop of 4 s, bars of four from the first beat.
    let beats: Vec<f64> = (0..8).map(|i| 0.25 + 0.5 * i as f64).collect();
    let g = BeatGrid::new(&beats, &[beats[0], beats[4]]);
    let at = |t: f64| {
        let (p, pass) = g.at_looped(t, 4.0).unwrap();
        (p.beat, p.bar, p.beat_in_bar, pass, (p.beat_phase * 1e6).round() / 1e6)
    };
    // Inside the beats as in the plain grid.
    assert_eq!(at(1.5), (2, 0, 2, 0, 0.5));
    // From the last beat to the end of the file and on into the next pass, one beat of 0.5 s.
    assert_eq!(at(3.9), (7, 1, 3, 0, 0.3));
    assert_eq!(at(4.1), (7, 1, 3, 0, 0.7));
    assert_eq!(at(4.25), (0, 0, 0, 1, 0.0));
    assert_eq!(at(9.8), (3, 0, 3, 2, 0.1));
    // Before the first beat of the first pass nothing came before: the beats count back.
    assert_eq!(at(0.1).0, -1);
    assert_eq!(at(0.1).3, 0);
    // With the first beat on 0 s, a beat found on the very end (or a frame before it) is the
    // next pass's first: the same as without it, but for the tempo.
    let from_zero: Vec<f64> = (0..8).map(|i| 0.5 * i as f64).collect();
    let g0 = BeatGrid::new(&from_zero, &[0.0, 2.0]);
    let no_bpm = |p: Option<(Position, i64)>| p.map(|(p, k)| (Position { bpm: 0.0, ..p }, k));
    for end in [4.0, 3.98] {
        let mut with_end = from_zero.clone();
        with_end.push(end);
        let g2 = BeatGrid::new(&with_end, &[0.0, 2.0]);
        for t in [3.6, 3.99, 4.0, 4.1, 9.8] {
            assert_eq!(no_bpm(g2.at_looped(t, 4.0)), no_bpm(g0.at_looped(t, 4.0)), "{end} {t}");
        }
    }
    let (p, pass) = g0.at_looped(4.0, 4.0).unwrap();
    assert_eq!((p.beat, p.bar, p.beat_phase, pass), (0, 0, 0.0, 1));
    // A length that does not hold two beats is no loop.
    for len in [f64::NAN, 0.0, -4.0, 0.5] {
        let (p, pass) = g.at_looped(4.1, len).unwrap();
        assert_eq!((p.beat, pass), (7, 0), "{len}");
        assert!(!p.inside);
    }
    assert!(g.at_looped(f64::NAN, 4.0).is_none());
    assert!(g.at_looped(1e300, 4.0).is_some());
}

// The study data: a 60 s excerpt of a DJ mix at 22.05 kHz with torch's spectrogram and
// final0 logits, and the whole 2 h mix.
const STUDY: &str = "E:/misc/temp/beat-study/onnx/data";
const SCRATCH: &str = "E:/misc/temp/beat-study/frontend";

fn format_times(v: &[f64]) -> String {
    v.iter().map(|t| format!("{t:.4}\n")).collect()
}

fn study_check(tag: &str) {
    let d = Path::new(STUDY);
    let pcm = read_f32(&d.join(format!("pcm_{tag}.f32")));
    let t = std::time::Instant::now();
    let spect = mono_spect(&pcm, SAMPLE_RATE);
    let secs = t.elapsed().as_secs_f64();
    let want = read_f32(&d.join(format!("spect_torch_{tag}.f32")));
    let diff = max_diff(spect.data(), &want);
    eprintln!(
        "{tag}: {} frames in {secs:.2} s ({:.3} s per audio minute), max |diff| to torch {diff:e}",
        spect.frames(),
        secs / (pcm.len() as f64 / SAMPLE_RATE / 60.0)
    );
    let _ = std::fs::create_dir_all(SCRATCH);
    let bytes: Vec<u8> = spect.data().iter().flat_map(|v| v.to_le_bytes()).collect();
    let _ = std::fs::write(Path::new(SCRATCH).join(format!("rust_spect_{tag}.f32")), bytes);
    assert!(diff < 1e-3, "{tag}: {diff}");

    let (b, dn) = deinterleave(&read_f32(&d.join(format!("logits_torch_final0_{tag}_cpu.f32"))));
    assert_eq!(b.len(), spect.frames());
    let got = pick(&b, &dn);
    let beats = std::fs::read_to_string(d.join(format!("beats_torch_final0_{tag}_cpu.txt"))).unwrap();
    let downs = std::fs::read_to_string(d.join(format!("downs_torch_final0_{tag}_cpu.txt"))).unwrap();
    eprintln!("{tag}: {} beats, {} downbeats", got.beats.len(), got.downbeats.len());
    assert_eq!(format_times(&got.beats), beats.replace("\r\n", "\n"));
    assert_eq!(format_times(&got.downbeats), downs.replace("\r\n", "\n"));
}

#[test]
#[ignore = "needs the beat study data in E:/misc/temp/beat-study"]
fn study_excerpt_matches_torch() {
    study_check("x60");
}

#[test]
#[ignore = "needs the beat study data in E:/misc/temp/beat-study; 2 h of sound, best in release"]
fn study_full_mix_matches_torch() {
    study_check("full");
}

#[test]
#[ignore = "timing only; run in release"]
fn front_end_speed() {
    let minutes = 10.0;
    for rate in [44_100.0, 48_000.0] {
        let n = (rate * 60.0 * minutes) as usize;
        let x: Vec<f32> = (0..2 * n).map(|i| ((i * 7919) % 2000) as f32 / 2000.0 - 0.5).collect();
        let t = std::time::Instant::now();
        let s = spectrogram(&x, 2, rate, None).unwrap();
        let secs = t.elapsed().as_secs_f64();
        eprintln!("{rate} Hz stereo: {} frames, {:.3} s per audio minute", s.frames(), secs / minutes);
    }
}

#[test]
fn tempo_holds_steady_on_beats_snapped_to_frames() {
    // 124 beats per minute, each beat snapped to the 20 ms frames the network sees, one
    // beat missed and one extra beat between two others.
    let period = 60.0 / 124.0;
    let mut beats: Vec<f64> =
        (0..200).map(|k| (1.0 + k as f64 * period) * FPS).map(|f| f.round() / FPS).collect();
    beats.remove(100);
    beats.push(beats[50] + 0.5 * period);
    let g = BeatGrid::new(&beats, &[]);
    let mut t = 1.0;
    while t < 90.0 {
        let p = g.at(t).unwrap();
        assert!((p.bpm - 124.0).abs() < 0.8, "t {t}: {} bpm", p.bpm);
        t += 0.05;
    }
    // Single spacings alone would jump by several beats per minute.
    let one = |i: usize| 60.0 / (beats[i + 1] - beats[i]);
    assert!((0..40).map(one).any(|b| (b - 124.0).abs() > 2.0));
}

/// Bar lengths in beats between bar starts (indices into the beats).
fn bar_lengths(starts: &[usize]) -> Vec<usize> {
    starts.windows(2).map(|w| w[1] - w[0]).collect()
}

/// How regular bars are: their count, the share of 4-beat bars, the bars whose length is no
/// multiple of 4 (a change of phase), and the share of time in runs of at least 32 4-beat bars.
fn bar_stats(what: &str, starts: &[usize], marked: &[usize]) {
    let len = bar_lengths(starts);
    let four = len.iter().filter(|n| **n == 4).count();
    let shifts = len.iter().filter(|n| **n % 4 != 0).count();
    let on = marked.iter().filter(|i| starts.binary_search(i).is_ok()).count();
    let (mut run, mut long, total) = (0usize, 0usize, len.iter().sum::<usize>());
    for n in len.iter().chain([&0]) {
        if *n == 4 {
            run += 1;
        } else {
            if run >= 32 {
                long += 4 * run;
            }
            run = 0;
        }
    }
    let mut hist = std::collections::BTreeMap::new();
    for n in &len {
        *hist.entry(*n).or_insert(0usize) += 1;
    }
    eprintln!(
        "{what}: {} bars, 4-beat {:.3}, phase shifts {shifts}, marked downbeats on the bars {:.3}, \
         time in runs of 32+ regular bars {:.2}, lengths {hist:?}",
        len.len(),
        four as f64 / len.len().max(1) as f64,
        on as f64 / marked.len().max(1) as f64,
        long as f64 / total.max(1) as f64,
    );
}

#[test]
#[ignore = "needs the beat study data in E:/misc/temp/beat-study; prints how regular the bars are"]
fn study_full_mix_bars() {
    let d = Path::new(STUDY);
    let (b, dn) = deinterleave(&read_f32(&d.join("logits_torch_final0_full_cpu.f32")));
    let got = pick(&b, &dn);
    let marked: Vec<usize> =
        got.downbeats.iter().filter_map(|t| got.beats.binary_search_by(|b| b.total_cmp(t)).ok()).collect();
    assert_eq!(marked.len(), got.downbeats.len());
    let t = std::time::Instant::now();
    let voted = bars(&got.downbeat_logits);
    eprintln!("{} beats, voted in {:.1} ms", got.beats.len(), t.elapsed().as_secs_f64() * 1e3);
    bar_stats("marked downbeats", &marked, &marked);
    bar_stats("voted bars", &voted, &marked);
    let t = std::time::Instant::now();
    let folded = fold(&got.beats, &got.downbeat_logits);
    eprintln!(
        "folded in {:.1} ms: {} beats to {}, {} dropped, {} inserted",
        t.elapsed().as_secs_f64() * 1e3,
        got.beats.len(),
        folded.beats.len(),
        folded.dropped(got.beats.len()),
        folded.inserted()
    );
    let marked_folded: Vec<usize> = folded
        .found
        .iter()
        .enumerate()
        .filter_map(|(k, f)| f.filter(|i| marked.binary_search(i).is_ok()).map(|_| k))
        .collect();
    bar_stats("folded, voted bars", &bars(&folded.logits), &marked_folded);
    let raw = BeatGrid::new(
        &got.beats,
        &voted.iter().filter_map(|i| got.beats.get(*i)).copied().collect::<Vec<_>>(),
    );
    let full = BeatGrid::from_beats(&got);
    for t in [20.0, 30.0, 230.0, 236.0, 2150.0, 2165.0, 6990.0, 7020.0] {
        let bpm = |g: &BeatGrid| g.at(t).map_or(0.0, |p| p.bpm);
        eprintln!("bpm at {t:>6} s: found {:6.1}, folded {:6.1}", bpm(&raw), bpm(&full));
    }
    // Partial results over the whole mix, every 50th chunk's worth of frames.
    for f in (0..b.len()).step_by(1488 * 50).skip(1) {
        let p = pick_final(&b[..f], &dn[..f]);
        let part = BeatGrid::from_beats(&p.beats);
        let until = p.final_until.unwrap();
        let (part, full) = (part.downbeats(), full.downbeats());
        let k = part.partition_point(|t| *t <= until);
        assert!(k > 0 && k <= full.len(), "{f} frames: {k} bars final");
        assert_eq!(part[..k], full[..k], "{f} frames");
    }
}

/// The numbers after `"key": ` in `text`, in order.
fn json_numbers(text: &str, key: &str) -> Vec<f64> {
    let pat = format!("\"{key}\": ");
    text.match_indices(&pat)
        .filter_map(|(i, _)| {
            let rest = &text[i + pat.len()..];
            let end = rest.find([',', '}', ']']).unwrap_or(rest.len());
            rest[..end].trim().parse().ok()
        })
        .collect()
}

/// Adjusted Rand index of two labelings.
fn adjusted_rand(a: &[usize], b: &[usize]) -> f64 {
    let ka = a.iter().max().map_or(0, |m| m + 1);
    let kb = b.iter().max().map_or(0, |m| m + 1);
    let mut table = vec![vec![0f64; kb]; ka];
    for (x, y) in a.iter().zip(b) {
        table[*x][*y] += 1.0;
    }
    let c2 = |n: f64| n * (n - 1.0) / 2.0;
    let sum_ij: f64 = table.iter().flatten().map(|n| c2(*n)).sum();
    let sum_a: f64 = table.iter().map(|r| c2(r.iter().sum())).sum();
    let sum_b: f64 = (0..kb).map(|j| c2(table.iter().map(|r| r[j]).sum())).sum();
    let expected = sum_a * sum_b / c2(a.len() as f64);
    (sum_ij - expected) / (0.5 * (sum_a + sum_b) - expected)
}

/// Sections of the 2 h mix against the Python prototype (`E:/misc/temp/beat-sections`, its
/// `sections.json`): boundaries found within 2 bars either way, labels compared second by
/// second. Needs the torch logits and the mix; the envelopes are cached in
/// `E:/misc/temp/s138/cache`.
#[test]
#[ignore]
fn study_full_mix_sections() {
    let d = Path::new(STUDY);
    let (b, dn) = deinterleave(&read_f32(&d.join("logits_torch_final0_full_cpu.f32")));
    let grid = BeatGrid::from_beats(&pick(&b, &dn));
    let t = std::time::Instant::now();
    let env = analyze_envelopes(
        Path::new("E:/misc/temp/beat-study/track.mp3"),
        Some(Path::new("E:/misc/temp/s138/cache")),
        None,
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    eprintln!(
        "envelopes in {:.1} s: {} hops, {} spectrum frames, {:.1} MB in memory",
        t.elapsed().as_secs_f64(),
        env.len(),
        env.spectrum_len(),
        env.bytes() as f64 / 1e6
    );
    let t = std::time::Instant::now();
    let s = Sections::of(&grid, &env).unwrap();
    let took = t.elapsed().as_secs_f64();
    let again = Sections::of(&grid, &env).unwrap();
    assert_eq!(s, again);
    let list = s.list();
    let bars = s.bars();
    eprintln!(
        "sections in {took:.2} s: {} bars, {} tracks, {} sections, {} kinds",
        bars.len(),
        s.tracks(),
        list.len(),
        s.kinds()
    );
    let mut lens: Vec<usize> = list.iter().map(|x| x.bars).collect();
    lens.sort_unstable();
    let track_starts: Vec<f64> =
        list.iter().filter(|x| x.track_start && x.first_bar > 0).map(|x| x.start).collect();
    let mut gaps: Vec<f64> = track_starts.windows(2).map(|w| (w[1] - w[0]) / 60.0).collect();
    gaps.sort_by(f64::total_cmp);
    eprintln!(
        "section bars: median {}, min {}, max {}; track gap median {:.1} min",
        lens[lens.len() / 2],
        lens[0],
        lens[lens.len() - 1],
        gaps.get(gaps.len() / 2).copied().unwrap_or(0.0)
    );
    let on_phrase = list
        .iter()
        .filter(|x| x.first_bar > 0)
        .filter(|x| s.at(x.start + 0.01).unwrap().phrase_bar == 0)
        .count();
    eprintln!("section starts on a phrase start: {on_phrase} of {}", list.len() - 1);
    let mut widths: Vec<f64> = list.iter().skip(1).map(|x| (x.zone.1 - x.zone.0) / 2.0).collect();
    widths.sort_by(f64::total_cmp);
    let zoned = widths.iter().filter(|w| **w > 0.0).count();
    let track_zones: Vec<f64> =
        list.iter().filter(|x| x.track_start && x.first_bar > 0).map(|x| x.zone.1 - x.zone.0).collect();
    eprintln!(
        "zones (s): {zoned} of {} wider than 0, median {:.1} s, 90 % {:.1} s; track changes: {} wider than 0, median {:.1} s",
        widths.len(),
        2.0 * widths[widths.len() / 2],
        2.0 * widths[widths.len() * 9 / 10],
        track_zones.iter().filter(|w| **w > 0.0).count(),
        {
            let mut z = track_zones.clone();
            z.sort_by(f64::total_cmp);
            z.get(z.len() / 2).copied().unwrap_or(0.0)
        }
    );

    // Against the prototype.
    let text = std::fs::read_to_string("E:/misc/temp/beat-sections/sections.json").unwrap();
    let coarse_part = &text[text.find("\"coarse\": [").unwrap()..text.find("\"segments\"").unwrap()];
    let seg_part = &text[text.find("\"segments\"").unwrap()..];
    let proto_tracks = json_numbers(coarse_part, "t");
    let proto_starts = json_numbers(seg_part, "t0");
    let proto_t1 = json_numbers(seg_part, "t1");
    let proto_cluster: Vec<usize> = json_numbers(seg_part, "cluster").iter().map(|c| *c as usize).collect();
    // A time in bars of this grid, with the fraction.
    let bar_at = |t: f64| {
        let i = bars.partition_point(|b| *b <= t).saturating_sub(1);
        let next = bars.get(i + 1).copied().unwrap_or(s.end());
        i as f64 + (t - bars[i]) / (next - bars[i]).max(1e-6)
    };
    let found = |want: &[f64], got: &[f64], tol: f64| {
        want.iter().filter(|w| got.iter().any(|g| (bar_at(*g) - bar_at(**w)).abs() <= tol)).count()
    };
    let ours_all: Vec<f64> = list.iter().skip(1).map(|x| x.start).collect();
    let proto_all: Vec<f64> = proto_starts.iter().skip(1).copied().collect();
    for tol in [2.0, 4.0] {
        eprintln!(
            "within {tol} bars: tracks: prototype's {} of {} found, ours {} of {} in the prototype's; \
             sections (all boundaries): prototype's {} of {}, ours {} of {}",
            found(&proto_tracks, &track_starts, tol),
            proto_tracks.len(),
            found(&track_starts, &proto_tracks, tol),
            track_starts.len(),
            found(&proto_all, &ours_all, tol),
            proto_all.len(),
            found(&ours_all, &proto_all, tol),
            ours_all.len()
        );
    }
    // Labels second by second.
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    let mut t = proto_starts[0];
    while t < *proto_t1.last().unwrap() && t < s.end() {
        let k = proto_starts.partition_point(|x| *x <= t).saturating_sub(1);
        theirs.push(proto_cluster[k]);
        ours.push(s.at(t).unwrap().section_label);
        t += 1.0;
    }
    eprintln!(
        "labels against the prototype's clusters, second by second: adjusted Rand {:.3}",
        adjusted_rand(&ours, &theirs)
    );
    let track_of = |t: f64, starts: &[f64]| starts.partition_point(|x| *x <= t);
    let (mut a, mut b2) = (Vec::new(), Vec::new());
    let mut t = 0.0;
    while t < s.end() {
        a.push(track_of(t, &track_starts));
        b2.push(track_of(t, &proto_tracks));
        t += 1.0;
    }
    eprintln!(
        "tracks against the prototype's, second by second: adjusted Rand {:.3}",
        adjusted_rand(&a, &b2)
    );
    // For a listen: every track boundary with its zone and the prototype's nearest.
    for x in list.iter().filter(|x| x.track_start && x.first_bar > 0) {
        let near = proto_tracks
            .iter()
            .map(|p| p - x.start)
            .min_by(|a, b| a.abs().total_cmp(&b.abs()))
            .unwrap_or(f64::NAN);
        eprintln!(
            "track {:>2} at {:>2}:{:04.1} zone {:>5.1} s .. {:>5.1} s, strength {:.2}, prototype's nearest {:+.1} s",
            x.track,
            (x.start / 60.0).floor(),
            x.start % 60.0,
            x.zone.0 - x.start,
            x.zone.1 - x.start,
            x.strength,
            near
        );
    }
}
