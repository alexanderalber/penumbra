//! Flat field and black lift against synthetic projectors seen through a camera with a steep,
//! unknown curve.

use crate::colour::decode;
use crate::flatfield::{
    MAX_LIFT, MAX_RATIO, Surface, blacks, colour_only, common_scale, cover, gain_map, lift, quantile,
    relative_gain, smooth_within, to_projector,
};
use crate::objects::Map;

/// What a camera with the curve `x^k` (unknown to the code) reads of light `f * decode(d)`.
fn reading(f: f64, d: f64, k: f64) -> f64 {
    (f * decode(d)).powf(k) * 0.4
}

const LEVELS: [f64; 5] = [1.0, 0.9, 0.8, 0.7, 0.6];

fn readings(f: f64, k: f64) -> Vec<f64> {
    LEVELS.iter().map(|d| reading(f, *d, k)).collect()
}

#[test]
fn relative_gain_is_the_ratio_of_the_light_whatever_the_camera_curve() {
    for k in [0.8, 1.0, 1.7, 2.4] {
        let reference = readings(1.0, k);
        // Inside the levels' range, beyond it (dimmer and brighter): exact for a power camera.
        for f in [1.0, 0.9, 0.62, 0.4, 0.2, 1.3, 2.0, 5.0] {
            let g = relative_gain(&LEVELS, &readings(f, k), &reference).unwrap();
            assert!((g - 1.0 / f).abs() < 1e-3 * (1.0 / f), "k {k} f {f}: {g}");
        }
    }
}

#[test]
fn relative_gain_copes_with_noise_dark_and_junk() {
    let reference = readings(1.0, 1.5);
    // A level reading less than the one below it (noise) still gives about the ratio.
    let mut noisy = readings(0.8, 1.5);
    noisy[2] = noisy[3] * 0.99;
    let g = relative_gain(&LEVELS, &noisy, &reference).unwrap();
    assert!((g - 1.25).abs() < 0.08, "{g}");
    // Nothing lit, too few levels, junk: no answer, no panic.
    assert_eq!(relative_gain(&LEVELS, &[0.0; 5], &reference), None);
    assert_eq!(relative_gain(&[1.0], &[0.5], &[0.5]), None);
    assert_eq!(relative_gain(&LEVELS, &[f64::NAN; 5], &reference), None);
    assert_eq!(relative_gain(&[], &[], &[]), None);
    assert_eq!(relative_gain(&LEVELS, &readings(1.0, 1.5), &[0.0; 5]), None);
    // Far out: clamped.
    let g = relative_gain(&LEVELS, &readings(0.001, 1.0), &readings(1.0, 1.0)).unwrap();
    assert_eq!(g, MAX_RATIO);
}

/// A hot spot and a cast towards the left, as the log of the light (0 at the centre).
fn field(u: f64, v: f64, c: usize) -> f64 {
    let spot = -0.5 * ((u - 0.55).powi(2) + (v - 0.45).powi(2));
    let cast = if c == 0 { -0.2 * (1.0 - u) } else { 0.0 };
    spot + cast
}

#[test]
fn a_surface_fits_a_smooth_field_through_a_patterned_surface() {
    // Samples on a grid, a fifth of them on a "painting" that reads far off.
    let mut samples = Vec::new();
    for y in 0..60 {
        for x in 0..80 {
            let (u, v) = ((x as f64 + 0.5) / 80.0, (y as f64 + 0.5) / 60.0);
            let painted = (20..36).contains(&x) && (10..40).contains(&y);
            let off = if painted { 0.4 } else { 0.0 };
            samples.push([u, v, -field(u, v, 0) + off]);
        }
    }
    let s = Surface::fit(&samples, 3, 0.25, 3).unwrap();
    for (u, v) in [(0.1, 0.1), (0.5, 0.5), (0.9, 0.7), (0.3, 0.9)] {
        assert!(
            (s.value(u, v) + field(u, v, 0)).abs() < 0.01,
            "{u} {v}: {} vs {}",
            s.value(u, v),
            -field(u, v, 0)
        );
    }
    let (all, best) = s.residual(&samples, 0.25);
    assert!(best < 0.01 && all > 0.1, "{all} {best}");
    // Beyond the samples it stays within what it saw.
    let far = s.value(5.0, -3.0);
    assert!(far >= s.range[0] && far <= s.range[1]);
    // Refused: too few samples, a silly degree or trim, NaN dropped.
    assert!(Surface::fit(&samples[..10], 3, 0.2, 1).is_err());
    assert!(Surface::fit(&samples, 99, 0.2, 1).is_err());
    assert!(Surface::fit(&samples, 2, 1.5, 1).is_err());
    let mut with_nan = samples.clone();
    with_nan.push([f64::NAN, 0.5, 1.0]);
    assert!(Surface::fit(&with_nan, 2, 0.0, 0).is_ok());
}

#[test]
fn gains_are_scaled_so_the_dimmest_needs_full_drive_and_never_more() {
    let mut fitted = Vec::new();
    let mut surfaces = Vec::new();
    for p in 0..2 {
        // Projector 1 is 30 % dimmer relative to its reference spot overall.
        let offset = if p == 1 { 0.3_f64.ln_1p() } else { 0.0 };
        let mut per = Vec::new();
        let mut logs = Vec::new();
        for c in 0..3 {
            let samples: Vec<[f64; 3]> = (0..2000)
                .map(|i| {
                    let (u, v) = ((i % 50) as f64 / 49.0, (i / 50) as f64 / 39.0);
                    [u, v, -field(u, v, c) + offset]
                })
                .collect();
            let s = Surface::fit(&samples, 3, 0.0, 0).unwrap();
            logs.extend(samples.iter().map(|x| s.value(x[0], x[1])));
            per.push(s);
        }
        fitted.push(logs);
        surfaces.push(per);
    }
    let k = common_scale(&fitted, 0.0).unwrap();
    let highest = fitted.iter().filter_map(|f| quantile(f, 1.0)).fold(f64::MIN, f64::max);
    assert!((k - (-highest).exp()).abs() < 1e-12);
    for per in surfaces {
        let [r, g, b]: [Surface; 3] = per.try_into().unwrap();
        let map = gain_map(&[r, g, b], [k; 3], 40, 30);
        assert_eq!(map.len(), 1200);
        let max = map.iter().flatten().fold(0f32, |a, v| a.max(*v));
        let min = map.iter().flatten().fold(1f32, |a, v| a.min(*v));
        assert!(max <= 1.0 && min > 0.3, "{min} {max}");
        // The hot spot gets the least gain, red more than green at the left (the cast).
        let at = |x: usize, y: usize| map[y * 40 + x];
        assert!(at(22, 13)[1] < at(2, 2)[1]);
        assert!(at(2, 15)[0] > at(2, 15)[1]);
    }
    assert_eq!(common_scale(&[], 0.02), None);
}

#[test]
fn black_is_lifted_where_another_projector_does_not_reach() {
    // A strip of 13 pixels: 0..3 lit by projector 0 only, 4..6 by both, 7..9 by projector 1
    // only, 10..12 by none (room light 0.001).
    let n = 13;
    let room = 0.001f32;
    let b0 = [0.004f32, 0.004, 0.004];
    let b1 = [0.002f32, 0.003, 0.006];
    let covers: Vec<Vec<bool>> =
        vec![(0..n).map(|i| i < 7).collect(), (0..n).map(|i| (4..10).contains(&i)).collect()];
    let mut black = vec![[room; 3]; n];
    let mut whites = vec![vec![[0f32; 3]; n]; 2];
    for i in 0..n {
        for (p, b) in [b0, b1].iter().enumerate() {
            if covers[p][i] {
                for c in 0..3 {
                    black[i][c] += b[c];
                }
                whites[p][i] = [0.5, 0.4, 0.4];
            }
        }
    }
    // Need MIN_PIXELS samples: repeat the strip.
    let reps = 6;
    let rep = |v: &Vec<bool>| v.iter().copied().cycle().take(n * reps).collect::<Vec<bool>>();
    let covers: Vec<Vec<bool>> = covers.iter().map(rep).collect();
    let black: Vec<[f32; 3]> = black.iter().copied().cycle().take(n * reps).collect();
    let whites: Vec<Vec<[f32; 3]>> =
        whites.iter().map(|w| w.iter().copied().cycle().take(n * reps).collect()).collect();
    let (per, measured_room) = blacks(&black, &covers);
    assert!((measured_room[2] - f64::from(room)).abs() < 1e-6, "{measured_room:?}");
    let got0 = per[0].unwrap();
    let got1 = per[1].unwrap();
    for c in 0..3 {
        assert!((got0[c] - f64::from(b0[c])).abs() < 1e-6 && (got1[c] - f64::from(b1[c])).abs() < 1e-6);
    }
    let l = lift(&covers, &whites, &[got0, got1]);
    // Projector 0 alone: lifted by projector 1's black over its own white; in the overlap not at all.
    let l0 = l[0][1].unwrap();
    assert!((l0[2] - 0.006 / 0.4).abs() < 1e-4, "{l0:?}");
    assert_eq!(l[0][5], Some([0.0; 3]));
    assert_eq!(l[0][8], None);
    let l1 = l[1][8].unwrap();
    assert!((l1[0] - 0.004 / 0.5).abs() < 1e-4, "{l1:?}");
    // Never more than MAX_LIFT; a dark white gives nothing.
    let huge = lift(&covers, &whites, &[[1.0; 3], [1.0; 3]]);
    assert_eq!(huge[0][1], Some([MAX_LIFT; 3]));
    let dark = vec![vec![[0f32; 3]; n * reps]; 2];
    assert_eq!(lift(&covers, &dark, &[got0, got1])[0][1], None);
}

#[test]
fn cover_takes_the_faint_edge_too() {
    let black = vec![[0.01f32; 3]; 100];
    let white: Vec<[f32; 3]> = (0..100).map(|i| [0.01 + i as f32 / 100.0; 3]).collect();
    let c = cover(&white, &black, 0.1);
    assert!(!c[5] && c[15] && c[99]);
    assert!(cover(&white, &black[..3], 0.1).iter().all(|v| !v));
    assert!(cover(&black, &black, 0.1).iter().all(|v| !v));
}

#[test]
fn camera_values_land_in_projector_pixels_and_holes_fill_from_the_nearest() {
    // A 4 x 2 projector seeing a 2 x 1 camera: left half on camera pixel 0, right on 1; one
    // projector pixel without a camera position.
    let mut rgba = Vec::new();
    for y in 0..2 {
        for x in 0..4 {
            let u = if x < 2 { 0.25 } else { 0.75 };
            let missing = x == 3 && y == 1;
            rgba.push(if missing { [-1.0, -1.0, 0.0, 1.0] } else { [u, 0.5, 1.0, 1.0] });
        }
    }
    let map = Map::new(4, 2, rgba).unwrap();
    let values = vec![Some([0.1f32, 0.2, 0.3]), Some([0.5f32; 3])];
    let out = to_projector(&values, 2, 1, &map, 0.0, [1.0; 3]);
    assert_eq!(out[0], [0.1, 0.2, 0.3]);
    assert_eq!(out[2], [0.5; 3]);
    assert_eq!(out[7], [0.5; 3]);
    // Nothing known: the fallback; wrong sizes: the fallback, no panic.
    let none = to_projector(&[None, None], 2, 1, &map, 1.0, [1.0; 3]);
    assert!(none.iter().flatten().all(|v| (v - 1.0).abs() < 1e-5));
    let wrong = to_projector(&values, 3, 3, &map, 0.0, [0.7; 3]);
    assert!(wrong.iter().all(|v| *v == [0.7; 3]));
}

#[test]
fn levels_where_the_reference_saturates_are_left_out() {
    // The top level clips (reads as the one below) for every pixel: the ratio still comes out.
    let clip = |f: f64| {
        let mut r = readings(f, 1.4);
        r[0] = r[1];
        r
    };
    for f in [0.7, 0.9, 1.2] {
        let g = relative_gain(&LEVELS, &clip(f), &clip(1.0)).unwrap();
        assert!((g - 1.0 / f).abs() < 2e-3 / f, "{f}: {g}");
    }
    // Flat from the start: nothing to compare.
    let flat = [0.3; 5];
    assert_eq!(relative_gain(&LEVELS, &flat, &flat), None);
}

#[test]
fn colour_only_drops_what_all_channels_share() {
    let shading = 0.37_f64.ln();
    let cast = [0.1, -0.02, -0.08];
    let a = colour_only(cast);
    let b = colour_only(cast.map(|v| v + shading));
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-12);
    }
    assert!(a.iter().sum::<f64>().abs() < 1e-12);
}

#[test]
fn smoothing_within_a_mask_keeps_the_outside_out() {
    // Left half 1 inside the mask, right half 9 outside it.
    let (w, h) = (20, 10);
    let mask: Vec<bool> = (0..w * h).map(|i| i % w < 10).collect();
    let values: Vec<[f32; 3]> = (0..w * h).map(|i| if i % w < 10 { [1.0; 3] } else { [9.0; 3] }).collect();
    let s = smooth_within(w, h, &values, &mask, 3.0);
    assert!((s[5 * w + 9][0] - 1.0).abs() < 1e-4, "{:?}", s[5 * w + 9]);
    assert_eq!(s[5 * w + 15], [9.0; 3]);
    // Wrong sizes: unchanged.
    assert_eq!(smooth_within(w, h, &values[..3], &mask, 3.0).len(), 3);
}
