//! Sections on a synthetic piece with known boundaries, the lookup, the clustering and hostile
//! input.

use super::super::{ENVELOPE_RATE, envelopes_of};
use super::*;

/// Seconds per bar of the synthetic piece (120 beats per minute, 4/4).
const BAR: f64 = 2.0;

/// The sound of one kind of section at `t` seconds: two tracks, each with a bed of its own and
/// two kinds of section on it. Kinds 0 and 1 are track one (kick and bass, then hats or a
/// 1 kHz lead on top), kinds 2 and 3 track two (a chord and a snare, then a 3 kHz lead or a
/// quiet breakdown).
fn voice(kind: usize, t: f64, noise: f64) -> f64 {
    use std::f64::consts::TAU;
    let beat = t % 0.5;
    let eighth = t % 0.25;
    match kind {
        0 | 1 => {
            let kick = 0.6 * (-beat / 0.07).exp() * (TAU * 55.0 * beat).sin();
            let bass = 0.2 * (TAU * 82.0 * t).sin();
            let top = if kind == 0 {
                0.25 * noise * (-eighth / 0.02).exp()
            } else {
                0.12 * (TAU * 1000.0 * t).sin()
            };
            kick + bass + top
        }
        _ => {
            let chord = 0.08 * [262.0, 330.0, 392.0].iter().map(|f| (TAU * f * t).sin()).sum::<f64>();
            let snare = if (t % 1.0) >= 0.5 { 0.3 * noise * (-beat / 0.05).exp() } else { 0.0 };
            if kind == 2 { chord + snare + 0.1 * (TAU * 3000.0 * t).sin() } else { 0.3 * chord }
        }
    }
}

/// The synthetic piece: `kinds` lists each section's kind, 16 bars each; the section boundary
/// at `blend` (a bar index) is a crossfade over `blend_bars` bars around it, the others are cuts.
fn piece(kinds: &[usize], blend: usize, blend_bars: usize) -> Vec<f32> {
    let rate = ENVELOPE_RATE;
    let bars = kinds.len() * 16;
    let n = (bars as f64 * BAR * rate) as usize;
    let mut seed = 7u32;
    (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = f64::from(seed >> 8) / f64::from(1u32 << 23) - 1.0;
            let bar = t / BAR;
            let k = kinds[((bar / 16.0) as usize).min(kinds.len() - 1)];
            let (a, b) = ((blend as f64 - blend_bars as f64 / 2.0), (blend as f64 + blend_bars as f64 / 2.0));
            let v = if blend_bars > 0 && bar >= a && bar < b {
                let before = kinds[blend / 16 - 1];
                let after = kinds[blend / 16];
                let w = (bar - a) / (b - a);
                (1.0 - w) * voice(before, t, noise) + w * voice(after, t, noise)
            } else {
                voice(k, t, noise)
            };
            v as f32
        })
        .collect()
}

const KINDS_OF_PIECE: [usize; 12] = [0, 1, 0, 1, 0, 1, 2, 3, 2, 3, 2, 3];

fn synthetic() -> Sections {
    let x = piece(&KINDS_OF_PIECE, 96, 16);
    let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
    let bars: Vec<f64> = (0..KINDS_OF_PIECE.len() * 16).map(|i| i as f64 * BAR).collect();
    Sections::find(&bars, x.len() as f64 / ENVELOPE_RATE, &env).unwrap()
}

#[test]
fn a_synthetic_piece_gives_its_tracks_sections_and_kinds() {
    let s = synthetic();
    let starts: Vec<usize> = s.list().iter().map(|x| x.first_bar).collect();
    // Two tracks; the second begins within the crossfade from bar 88 to 104.
    assert_eq!(s.tracks(), 2);
    let second = s.list().iter().find(|x| x.track == 1).unwrap();
    assert!(second.track_start && (88..=104).contains(&second.first_bar), "{second:?}");
    // Every 16 bars a section, within a bar, the crossfade one change.
    assert_eq!(starts.len(), 12, "{starts:?}");
    for (k, b) in starts.iter().enumerate().filter(|(_, b)| **b != second.first_bar) {
        assert!(b.abs_diff(16 * k) <= 1, "section {k} starts at bar {b}: {starts:?}");
    }
    // The same kind, the same label; another kind, another label.
    for (a, ka) in s.list().iter().zip(KINDS_OF_PIECE) {
        for (b, kb) in s.list().iter().zip(KINDS_OF_PIECE) {
            assert_eq!(a.label == b.label, ka == kb, "{a:?} {b:?}");
            if a.track == b.track {
                assert_eq!(a.letter == b.letter, ka == kb);
            }
        }
    }
    assert_eq!(s.kinds(), 4);
    let seeds: Vec<u32> = s.list().iter().map(|x| x.seed).collect();
    assert_eq!(seeds[0], seeds[2]);
    assert_ne!(seeds[0], seeds[1]);
    // The crossfade is the zone of the track change, the cuts have none.
    let (a, b) = second.zone;
    assert!(a <= second.start && b >= second.start, "{:?}", second.zone);
    assert!((a / BAR - 88.0).abs() <= 2.0 && (b / BAR - 104.0).abs() <= 2.0, "{:?}", second.zone);
    for x in s.list().iter().filter(|x| x.first_bar != second.first_bar) {
        assert!(x.zone.1 - x.zone.0 <= 1.0 * BAR, "cut at bar {} has zone {:?}", x.first_bar, x.zone);
    }
}

#[test]
fn the_lookup_counts_down_to_the_next_boundary() {
    let s = synthetic();
    let list = s.list();
    let second = &list[1];
    // Two and a half bars before the second section.
    let t = second.start - 2.5 * BAR;
    let p = s.at(t).unwrap();
    assert_eq!(p.section_index, 0);
    assert_eq!(p.section_label, list[0].label);
    assert!((p.bars_to_boundary - 2.5).abs() < 1e-9, "{p:?}");
    assert!((p.seconds_to_boundary - 2.5 * BAR).abs() < 1e-9);
    assert!((p.build - (1.0 - 2.5 / BUILD_BARS as f64)).abs() < 1e-9);
    assert!(p.inside);
    assert_eq!(p.transition, 0.0);
    assert_eq!((p.transition_from, p.transition_to), (p.section_label, p.section_label));
    // At the start of a section the build is 0 again, and a phrase starts there.
    let p = s.at(second.start + 0.01).unwrap();
    assert_eq!(p.section_index, 1);
    assert_eq!(p.build, 0.0);
    assert_eq!(p.phrase_bar, 0);
    assert_eq!(s.at(second.start + 3.5 * BAR).unwrap().phrase_bar, 3);
    // Through the crossfade the transition rises from the first track's label to the second's.
    let track = list.iter().find(|x| x.track == 1).unwrap();
    let before = list.iter().rev().find(|x| x.track == 0).unwrap();
    let (a, b) = track.zone;
    let mut last = -1.0;
    let mut t = a;
    while t < b {
        let p = s.at(t).unwrap();
        assert!(p.transition >= last && p.transition < 1.0, "{t}: {p:?}");
        assert_eq!((p.transition_from, p.transition_to), (before.label, track.label));
        last = p.transition;
        t += 0.25;
    }
    assert!(last > 0.9);
    assert_eq!(s.at(b).unwrap().transition, 0.0);
    // Outside the bars.
    let p = s.at(-5.0).unwrap();
    assert!(!p.inside && p.section_index == 0);
    let end = s.end();
    let p = s.at(end + 10.0).unwrap();
    assert!(!p.inside && p.section_index == list.len() - 1 && p.bars_to_boundary == 0.0 && p.build == 1.0);
    assert!(s.at(f64::NAN).is_none());
    assert!(s.at(f64::INFINITY).is_none());
    // The table for a picture.
    let spans = s.spans();
    assert_eq!(spans.len(), list.len());
    assert_eq!(spans[0].0, 0.0);
    assert_eq!(spans.last().unwrap().1, end);
    assert!(spans.windows(2).all(|w| w[0].1 == w[1].0));
}

#[test]
fn sections_are_deterministic() {
    let x = piece(&[0, 1, 2, 3, 0, 1], 48, 8);
    let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
    let bars: Vec<f64> = (0..96).map(|i| i as f64 * BAR).collect();
    let a = Sections::find(&bars, 192.0, &env).unwrap();
    let b = Sections::find(&bars, 192.0, &env).unwrap();
    assert_eq!(a, b);
    // The bars in another order, with repeats and non-numbers, give the same.
    let mut messy: Vec<f64> = bars.iter().rev().copied().collect();
    messy.extend([f64::NAN, 4.0, f64::INFINITY, f64::NEG_INFINITY]);
    assert_eq!(Sections::find(&messy, 192.0, &env).unwrap(), a);
}

#[test]
fn hostile_input_gives_an_error_or_one_section() {
    let x = piece(&[0], 0, 0);
    let env = envelopes_of(&x, 1, ENVELOPE_RATE).unwrap();
    assert!(Sections::find(&[], 10.0, &env).is_err());
    assert!(Sections::find(&[f64::NAN], 10.0, &env).is_err());
    let many: Vec<f64> = (0..=MAX_BARS).map(|i| i as f64).collect();
    assert!(Sections::find(&many, 1e9, &env).is_err());
    let empty = envelopes_of(&[], 1, ENVELOPE_RATE).unwrap();
    assert!(Sections::find(&[0.0, 2.0], 4.0, &empty).is_err());
    // One bar: one section to the end given, or one bar's guess.
    for end in [32.0, f64::NAN, -1.0] {
        let s = Sections::find(&[0.0], end, &env).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.tracks(), 1);
        let p = s.at(1.0).unwrap();
        assert_eq!((p.section_index, p.section_label, p.phrase_bar), (0, 0, 0));
        assert!(p.bars_to_boundary.is_finite() && p.seconds_to_boundary.is_finite());
    }
    // Bars far beyond the sound: they have no frames and take the mean.
    let bars: Vec<f64> = (0..40).map(|i| i as f64 * BAR * 10.0).collect();
    let s = Sections::find(&bars, 1e6, &env).unwrap();
    assert!(!s.is_empty());
    assert!(s.at(5e5).unwrap().bars_to_boundary.is_finite());
    // Huge times.
    let s = Sections::find(&[0.0, 1e300], f64::MAX, &env).unwrap();
    assert!(s.at(1e301).is_some());
    // Features that are no numbers.
    let width: usize = GROUPS.iter().sum();
    let rows: Vec<Vec<f64>> = (0..50)
        .map(|i| (0..width).map(|j| if (i + j) % 7 == 0 { f64::NAN } else { (i * j) as f64 }).collect())
        .collect();
    let s = Sections::from_features((0..50).map(|i| i as f64).collect(), 50.0, &rows);
    assert!(!s.is_empty());
    assert!(s.track_novelty().iter().chain(s.section_novelty()).all(|v| v.is_finite()));
    // Rows too short.
    let s = Sections::from_features(vec![0.0, 1.0, 2.0], 3.0, &[vec![1.0], vec![], vec![f64::INFINITY]]);
    assert!(!s.is_empty());
    // A bar on the file's very end: left out, so the last section ends with the sound.
    let end = env.len() as f64 * env.hop_seconds();
    let beats: Vec<f64> = (0..).map(|i| f64::from(i) * 0.5).take_while(|t| *t <= end).collect();
    let downbeats: Vec<f64> = beats.iter().copied().step_by(4).collect();
    assert_eq!(downbeats.last(), Some(&end), "the piece ends on a bar");
    let grid = BeatGrid::from_beats(&super::super::Beats { beats, downbeats, downbeat_logits: Vec::new() });
    let s = Sections::of(&grid, &env).unwrap();
    assert!(s.bars().iter().all(|b| *b < end) && s.end() == end, "{:?} {}", s.bars(), s.end());
}

/// Ward by brute force: merge the pair whose merge adds the least variance, until `k` are left.
fn ward_brute(points: &[Vec<f64>], k: usize) -> Vec<usize> {
    let mut clusters: Vec<Vec<usize>> = (0..points.len()).map(|i| vec![i]).collect();
    let mean = |c: &[usize]| {
        let mut m = vec![0.0; points[0].len()];
        for i in c {
            for (s, v) in m.iter_mut().zip(&points[*i]) {
                *s += v / c.len() as f64;
            }
        }
        m
    };
    while clusters.len() > k {
        let mut best = (0, 1, f64::INFINITY);
        for i in 0..clusters.len() {
            for j in i + 1..clusters.len() {
                let (a, b) = (mean(&clusters[i]), mean(&clusters[j]));
                let (na, nb) = (clusters[i].len() as f64, clusters[j].len() as f64);
                let d2: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum();
                let cost = na * nb / (na + nb) * d2;
                if cost < best.2 {
                    best = (i, j, cost);
                }
            }
        }
        let gone = clusters.remove(best.1);
        clusters[best.0].extend(gone);
    }
    let mut out = vec![0; points.len()];
    for (c, members) in clusters.iter().enumerate() {
        for i in members {
            out[*i] = c;
        }
    }
    out
}

#[test]
fn nearest_neighbour_chains_match_brute_force_ward() {
    let mut state = 12_345u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 10_000) as f64 / 1000.0
    };
    for n in [2usize, 3, 7, 20, 41] {
        let points: Vec<Vec<f64>> = (0..n).map(|_| vec![next(), next(), next()]).collect();
        let merges = nn_chain(&points, Linkage::Ward);
        assert_eq!(merges.len(), n - 1);
        for k in 1..=n.min(6) {
            let got = first_appearance(&cut(&merges, n, k, 0.0));
            let want = first_appearance(&ward_brute(&points, k));
            assert_eq!(got, want, "n {n}, k {k}");
        }
    }
    // Pairs index a triangle without gaps.
    let n = 9;
    let mut seen: Vec<usize> = (0..n).flat_map(|i| (i + 1..n).map(move |j| pair(n, i, j))).collect();
    seen.sort_unstable();
    assert_eq!(seen, (0..n * (n - 1) / 2).collect::<Vec<_>>());
    assert!(nn_chain(&[], Linkage::Ward).is_empty());
    assert!(nn_chain(&[vec![1.0]], Linkage::Cosine).is_empty());
    assert_eq!(cut(&[], 1, 10, 0.25), vec![0]);
}

#[test]
fn peaks_keep_their_distance() {
    let mut nov = vec![0.0; 100];
    for (t, v) in [(20, 1.0), (24, 0.9), (60, 0.8), (61, 0.8)] {
        nov[t] = v;
    }
    assert_eq!(pick(&nov, 4, 8, 0.5), vec![20, 60]);
    assert!(pick(&[], 4, 8, 0.5).is_empty());
    assert!(pick(&[1.0, 2.0], 4, 8, 0.5).is_empty());
    assert!(pick(&[0.0; 50], 4, 8, 0.5).is_empty());
}

#[test]
fn seeds_differ_by_label_and_fit_an_f32() {
    let seeds: Vec<u32> = (0..64).map(seed).collect();
    for (i, a) in seeds.iter().enumerate() {
        assert!(*a < 1 << 24);
        assert!(seeds[i + 1..].iter().all(|b| b != a));
    }
}
