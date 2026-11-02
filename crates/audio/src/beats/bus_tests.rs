//! The bus on synthetic levels and grids: the normalisation's quantiles, the follower's times,
//! the travel's integral, clamps and smoothing, energies and drops of a quiet and a loud half.

use super::super::{ENVELOPE_RATE, envelopes_of};
use super::*;

/// The quantiles map to 0 and 1, values beyond them clamp, silence stays 0.
#[test]
fn levels_normalise_between_the_quantiles() {
    // 100 levels from -60 to -10.5 dB in steps of 0.5: the 5th lies at -58, the 95th at -13.
    let mut db: Vec<f64> = (0..100).map(|i| -60.0 + f64::from(i) * 0.5).collect();
    normalise(&mut db);
    assert_eq!(db[0], 0.0);
    assert_eq!(db[4], 0.0);
    assert!((db[50] - (-35.0 + 58.0) / 45.0).abs() < 1e-9, "{}", db[50]);
    assert_eq!(db[94], 1.0);
    assert_eq!(db[99], 1.0);
    assert!(db.windows(2).all(|w| w[1] >= w[0]));

    // Order does not matter, non-numbers read 0, a constant reads 0.
    let mut mixed = vec![-20.0, f64::NAN, -80.0, -50.0, f64::INFINITY];
    normalise(&mut mixed);
    assert!(mixed.iter().all(|v| (0.0..=1.0).contains(v)), "{mixed:?}");
    assert_eq!(mixed[1], 0.0);
    let mut flat = vec![-30.0; 50];
    normalise(&mut flat);
    assert!(flat.iter().all(|v| *v == 0.0));
    let mut none: Vec<f64> = Vec::new();
    normalise(&mut none);
}

/// A step up reaches 63 % after `rise`, a step down after `fall`.
#[test]
fn the_follower_rises_and_falls_at_its_times() {
    let step = 0.001;
    let mut x = vec![0.0; 2000];
    x.extend(vec![1.0; 2000]);
    x.extend(vec![0.0; 2000]);
    follow(&mut x, step, 0.05, 0.3);
    assert_eq!(x[0], 0.0);
    // 50 steps after the rise, 300 after the fall: 1 - 1/e and 1/e.
    let up = x[2000 + 49];
    assert!((up - (1.0 - (-1.0f64).exp())).abs() < 0.01, "{up}");
    let down = x[4000 + 299] / x[3999];
    assert!((down - (-1.0f64).exp()).abs() < 0.01, "{down}");
    // The first value passes unchanged; 0 times follow at once.
    let mut y = vec![0.7, 0.2, 0.9];
    follow(&mut y, step, 0.0, 0.0);
    assert_eq!(y, [0.7, 0.2, 0.9]);
    let mut e: Vec<f64> = Vec::new();
    follow(&mut e, step, 0.05, 0.3);
}

/// Beats every `gap` seconds from 0 to `end`.
fn steady(gap: f64, end: f64) -> BeatGrid {
    let beats: Vec<f64> = (0..).map(|i| f64::from(i) * gap).take_while(|t| *t <= end).collect();
    let downs: Vec<f64> = beats.iter().step_by(4).copied().collect();
    BeatGrid::new(&beats, &downs)
}

#[test]
fn travel_adds_up_a_smoothed_and_clamped_tempo() {
    let step = 4.0 * 512.0 / 48_000.0;
    // 120 beats per minute: two beats per second, from the start.
    let g = steady(0.5, 60.0);
    let t = Travel::new(&g, 60.0, step, TRAVEL_FLOOR, TRAVEL_CAP);
    for s in [0.0, 1.0, 10.3, 59.9] {
        let (beats, bpm) = t.at(s);
        assert!((beats - 2.0 * s).abs() < 1e-6, "{s}: {beats}");
        assert!((bpm - 120.0).abs() < 1e-3, "{s}: {bpm}");
    }
    // Beyond the ends the tempo holds.
    assert!((t.at(70.0).0 - 140.0).abs() < 1e-6);
    assert!((t.at(-1.0).0 + 2.0).abs() < 1e-6);
    assert_eq!(t.at(f64::NAN), (0.0, 0.0));
    // A floor of 200 pins it; no clamp leaves 120.
    let pinned = Travel::new(&g, 60.0, step, 200.0, TRAVEL_CAP);
    assert!((pinned.at(30.0).1 - 200.0).abs() < 1e-3);
    assert!((pinned.at(30.0).0 - 100.0).abs() < 1e-6);
    let capped = Travel::new(&g, 60.0, step, 0.0, 90.0);
    assert!((capped.at(30.0).1 - 90.0).abs() < 1e-3);
    assert_eq!(capped.clamps(), (0.0, 90.0));

    // 100 then 140 beats per minute: the tempo covers 63 % of the step in TRAVEL_SMOOTH
    // seconds, never overshoots, and the beats rise without a jump.
    let mut beats: Vec<f64> = (0..50).map(|i| f64::from(i) * 0.6).collect();
    let turn = 30.0;
    beats.extend((0..70).map(|i| turn + f64::from(i) * 60.0 / 140.0));
    let g = BeatGrid::new(&beats, &[]);
    let t = Travel::new(&g, 60.0, step, TRAVEL_FLOOR, TRAVEL_CAP);
    let mid = t.at(turn + TRAVEL_SMOOTH).1;
    assert!((mid - (100.0 + 0.632 * 40.0)).abs() < 1.5, "{mid}");
    let mut before = t.at(0.0).0;
    for k in 1..6000 {
        let s = f64::from(k) * 0.01;
        let (b, bpm) = t.at(s);
        assert!(b >= before && b - before < 0.03, "{s}: {before} to {b}");
        assert!((99.0..=141.0).contains(&bpm), "{s}: {bpm}");
        before = b;
    }
    // No beats: no tempo; and a grid with a long gap is lifted to the floor.
    let none = Travel::new(&BeatGrid::default(), 10.0, step, TRAVEL_FLOOR, TRAVEL_CAP);
    assert_eq!(none.at(5.0), (0.0, 0.0));
    let sparse = Travel::new(&BeatGrid::new(&[0.0, 10.0], &[]), 10.0, step, TRAVEL_FLOOR, TRAVEL_CAP);
    assert!((sparse.at(5.0).1 - 80.0).abs() < 1e-3);
    // Hostile sizes.
    let odd = Travel::new(&g, f64::NAN, f64::NAN, f64::NAN, f64::INFINITY);
    assert!(odd.at(1.0).0.is_finite());
}

/// 20 s quiet, then 20 s loud: the energies tell them apart and the loud half begins with a
/// drop that falls over half a second.
#[test]
fn energy_and_drops_of_a_quiet_and_a_loud_half() {
    use std::f64::consts::TAU;
    let rate = ENVELOPE_RATE;
    let n = (40.0 * rate) as usize;
    let x: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / rate;
            let beat = t % 0.5;
            let kick = (-beat / 0.08).exp() * (TAU * 60.0 * beat).sin();
            let g = if t < 20.0 { 0.03 } else { 0.6 };
            (g * kick + 0.02 * (TAU * 440.0 * t).sin()) as f32
        })
        .collect();
    let env = envelopes_of(&x, 1, rate).unwrap();
    let g = steady(0.5, 40.0);
    let sections = Sections::find(&g.downbeats(), 40.0, &env).ok();
    let bus = Bus::new(&g, &env, sections.as_ref(), TRAVEL_FLOOR, TRAVEL_CAP);
    let quiet = bus.at(10.0);
    let loud = bus.at(30.0);
    assert!(loud.energy[0] > quiet.energy[0] + 0.3, "{quiet:?} {loud:?}");
    assert!(loud.energy[1] > quiet.energy[1] + 0.3, "{quiet:?} {loud:?}");
    assert!(bus.at(5.0).energy.iter().chain(&loud.energy).all(|v| (0.0..=1.0).contains(v)));
    assert_eq!(bus.bar_beats(), 4);
    assert!((bus.at(12.0).travelled - 24.0).abs() < 1e-3);
    assert!(bus.bytes() > 0);

    let list = bus.sections();
    let k = list.iter().position(|s| (s.start - 20.0).abs() <= 2.0).unwrap_or_else(|| panic!("{list:?}"));
    assert!(list[k].drop && !list[0].drop, "{list:?}");
    assert!(list[k].role == Role::Drop && list[k].change >= CHANGE_JUMP * DROP_RISE - 1e-9, "{list:?}");
    assert!(list[k].bass > list[0].bass + 0.3 && list[k].kick > 0.5, "{list:?}");
    let at = list[k].start;
    assert!((bus.at(at).drop - 1.0).abs() < 1e-9);
    assert!((bus.at(at + DROP_DECAY).drop - (-1.0f64).exp()).abs() < 1e-6);
    assert_eq!(bus.at(at - 0.1).drop, 0.0);
    assert!(bus.at(30.0).section_energy > bus.at(10.0).section_energy + DROP_RISE);
    assert_eq!(bus.at(f64::NAN), BusReading::default());

    // Without sections: energies, no section energy, no drops.
    let plain = Bus::new(&g, &env, None, TRAVEL_FLOOR, TRAVEL_CAP);
    assert_eq!(plain.at(30.0).energy, loud.energy);
    assert_eq!((plain.at(at).drop, plain.at(30.0).section_energy), (0.0, 0.0));
    // No hops at all.
    let empty = envelopes_of(&[], 1, rate).unwrap();
    let none = Bus::new(&BeatGrid::default(), &empty, None, TRAVEL_FLOOR, TRAVEL_CAP);
    assert_eq!(none.at(1.0).energy, [0.0; 4]);
}

/// A section of `track` with this energy and kick share, its novelty `strength` and no rise.
fn traits(track: usize, track_start: bool, strength: f64, energy: f64, kick: f64) -> Traits {
    Traits { track, track_start, strength, energy, kick, rise: 0.0 }
}

/// Roles by precedence: intro, drop, outro, build, break, main.
#[test]
fn roles_follow_their_precedence() {
    let mut t = vec![
        // Track 0: a first section without a kick is still the intro.
        traits(0, true, 0.0, 0.6, 0.0),
        traits(0, false, 0.3, 0.7, 0.9),
        // Kick gone: a break; then a rise into a drop: a build.
        traits(0, false, 0.4, 0.65, 0.1),
        traits(0, false, 0.2, 0.6, 0.9),
        traits(0, false, 0.6, 0.85, 0.95),
        // Far below the track's median energy with the kick on: a break.
        traits(0, false, 0.3, 0.4, 0.9),
        // The last section begins with a drop: a drop, not an outro.
        traits(0, false, 0.5, 0.8, 0.9),
        // Track 1, one section: neither intro nor outro.
        traits(1, true, 0.7, 0.75, 0.9),
        // Track 2: intro, outro.
        traits(2, true, 0.5, 0.7, 0.9),
        traits(2, false, 0.2, 0.65, 0.9),
    ];
    t[3].rise = BUILD_RISE + 0.01;
    let roles: Vec<Role> = judge(&t).iter().map(|v| v.role).collect();
    use Role::*;
    assert_eq!(roles, [Intro, Main, Break, Build, Drop, Break, Drop, Main, Intro, Outro]);
    // A rise without a drop after it is no build.
    t[4].energy = 0.7;
    assert_eq!(judge(&t)[3].role, Main);
    // Numbers and names.
    assert_eq!(Role::ALL.map(Role::index), [0, 1, 2, 3, 4, 5]);
    assert_eq!(Role::ALL.map(Role::name), ["intro", "break", "build", "drop", "outro", "main"]);
    assert!(judge(&[]).is_empty());
}

/// change = clamp(max(novelty weight x strength, jump weight x |jump|) + track weight at a track
/// start); 0 for the first section.
#[test]
fn change_combines_novelty_jump_and_track_start() {
    let t = [
        traits(0, true, 0.9, 0.5, 1.0),
        // A small change: novelty alone.
        traits(0, false, 0.3, 0.55, 1.0),
        // An energy jump down outweighs the novelty.
        traits(0, false, 0.3, 0.25, 0.0),
        // A track start with a weak boundary.
        traits(1, true, 0.2, 0.3, 1.0),
        // Everything at once clamps to 1.
        traits(2, true, 1.0, 0.9, 1.0),
        traits(2, false, f64::NAN, 0.9, 1.0),
    ];
    let v = judge(&t);
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert_eq!(v[0].change, 0.0);
    assert_eq!(v[0].jump, 0.0);
    assert!(close(v[1].change, CHANGE_NOVELTY * 0.3), "{v:?}");
    assert!(close(v[1].jump, 0.05), "{v:?}");
    assert!(close(v[2].change, CHANGE_JUMP * 0.3), "{v:?}");
    assert!(close(v[3].change, (CHANGE_NOVELTY * 0.2).max(CHANGE_JUMP * 0.05) + CHANGE_TRACK), "{v:?}");
    assert_eq!(v[4].change, 1.0);
    assert!(v[4].drop && !v[1].drop);
    // A broken strength gives a finite change.
    assert!((0.0..=1.0).contains(&v[5].change), "{v:?}");
}

/// Onsets per beat, and the share of beats with a kick, counted from a quarter beat early.
#[test]
fn rhythm_counts_onsets_per_beat_and_beats_with_a_kick() {
    let beats: Vec<f64> = (0..9).map(|i| f64::from(i) * 0.5).collect();
    // Kicks a little early on beats 0 to 3, none after; two onsets per beat.
    let kicks: Vec<f64> = (0..4).map(|i| f64::from(i) * 0.5 - 0.05).collect();
    let onsets: Vec<f64> = (0..16).map(|i| f64::from(i) * 0.25).collect();
    let (density, kick) = rhythm(&beats, &onsets, &kicks, 0.0, 4.0);
    assert!((density - 2.0).abs() < 1e-9, "{density}");
    assert!((kick - 0.5).abs() < 1e-9, "{kick}");
    assert_eq!(rhythm(&beats, &onsets, &kicks, 2.0, 4.0).1, 0.0);
    // No beats: nothing.
    assert_eq!(rhythm(&[], &onsets, &kicks, 0.0, 4.0), (0.0, 0.0));
    assert_eq!(rhythm(&beats, &onsets, &kicks, 10.0, 12.0), (0.0, 0.0));
}
