use super::*;

/// A synthetic projector: primaries (columns, camera RGB), a power-law response per channel.
struct Synth {
    p: Mat3,
    gamma: [f64; 3],
}

impl Synth {
    /// The camera's linear RGB (black subtracted) for signal `s`.
    fn light(&self, s: [f64; 3]) -> [f64; 3] {
        let d = [0, 1, 2].map(|c| s[c].clamp(0.0, 1.0).powf(self.gamma[c]));
        apply(&self.p, d)
    }

    fn measure(&self, levels: usize) -> Measured {
        let levels: Vec<f64> = (1..=levels).map(|i| i as f64 / levels as f64).collect();
        let ramps = [0, 1, 2].map(|c| {
            levels
                .iter()
                .map(|l| {
                    let mut s = [0.0; 3];
                    s[c] = *l;
                    self.light(s)
                })
                .collect()
        });
        Measured { levels, ramps }
    }

    /// The light for content `c` through `corr`, as the output pass computes the signal.
    fn corrected(&self, corr: &Correction, c: [f64; 3]) -> [f64; 3] {
        let d = apply(&corr.matrix, c).map(|v| v.clamp(0.0, 1.0));
        let s = [0, 1, 2].map(|i| lookup(&corr.curves[i], encode(d[i])));
        self.light(s)
    }
}

fn synth_set() -> Vec<Synth> {
    let p0 = [[0.80, 0.30, 0.15], [0.35, 0.95, 0.10], [0.03, 0.10, 0.90]];
    // Narrower primaries (each a little of the others mixed in), dimmer.
    let narrow = [[0.60, 0.05, 0.02], [0.06, 0.55, 0.05], [0.02, 0.04, 0.62]];
    // The same primaries in another balance.
    let balance = [[0.9, 0.0, 0.0], [0.0, 1.1, 0.0], [0.0, 0.0, 0.8]];
    vec![
        // Bright, slightly warm, gamma 2.2.
        Synth { p: p0, gamma: [2.2; 3] },
        // Dimmer and less saturated, gammas differ per channel.
        Synth { p: mul(&p0, &narrow), gamma: [2.4, 2.0, 2.6] },
        // Another white balance, another gamma.
        Synth { p: mul(&p0, &balance), gamma: [1.8, 2.2, 2.2] },
    ]
}

/// Projectors whose primaries no one else can reach exactly.
fn wide_set() -> Vec<Synth> {
    vec![
        Synth { p: [[0.80, 0.30, 0.15], [0.35, 0.95, 0.10], [0.03, 0.10, 0.90]], gamma: [2.2; 3] },
        Synth { p: [[0.50, 0.25, 0.10], [0.20, 0.70, 0.12], [0.02, 0.06, 0.75]], gamma: [2.4, 2.0, 2.6] },
    ]
}

#[test]
fn srgb_round_trip() {
    for i in 0..=100 {
        let x = i as f64 / 100.0;
        assert!((decode(encode(x)) - x).abs() < 1e-12);
    }
}

#[test]
fn isotonic_pools_violators() {
    assert_eq!(isotonic(&[1.0, 3.0, 2.0, 4.0]), vec![1.0, 2.5, 2.5, 4.0]);
    assert_eq!(isotonic(&[3.0, 2.0, 1.0]), vec![2.0, 2.0, 2.0]);
    assert!(isotonic(&[]).is_empty());
}

#[test]
fn an_srgb_projector_gets_straight_curves() {
    // Responds exactly like the sRGB curve: the identity table.
    let levels: Vec<f64> = (1..=8).map(|i| i as f64 / 8.0).collect();
    let p = [[0.6, 0.2, 0.1], [0.3, 0.7, 0.1], [0.0, 0.1, 0.8]];
    let ramps = [0, 1, 2].map(|c| {
        levels
            .iter()
            .map(|l| {
                let mut d = [0.0; 3];
                d[c] = decode(*l);
                apply(&p, d)
            })
            .collect()
    });
    let m = match_projectors(&[Measured { levels, ramps }], &[None], 17, Target::Srgb).unwrap();
    assert_eq!(m.reference, 0);
    assert!((m.alpha - 1.0).abs() < 1e-9);
    let c = &m.projectors[0];
    for (i, row) in c.matrix.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            assert!((v - IDENTITY[i][j]).abs() < 1e-9, "{:?}", c.matrix);
        }
    }
    for curve in &c.curves {
        for (i, v) in curve.iter().enumerate() {
            assert!((v - i as f64 / 16.0).abs() < 1e-9, "{curve:?}");
        }
    }
    assert!((c.peak[0] - 1.0).abs() < 1e-9);
}

#[test]
fn synthetic_projectors_match_after_correction() {
    let set = synth_set();
    let measured: Vec<Measured> = set.iter().map(|s| s.measure(8)).collect();
    let whites: Vec<Option<[f64; 3]>> = set.iter().map(|s| Some(s.light([1.0; 3]))).collect();
    let m = match_projectors(&measured, &whites, 17, Target::Srgb).unwrap();
    assert!(m.alpha > 0.3 && m.alpha <= 1.0, "{}", m.alpha);
    // The target: alpha times the reference's primaries.
    let q = set[m.reference].p.map(|r| r.map(|v| v * m.alpha));
    let white = luminance(apply(&q, [1.0; 3]));
    let mut worst: f64 = 0.0;
    for (k, s) in set.iter().enumerate() {
        for i in 0..=4 {
            for j in 0..=4 {
                for l in 0..=4 {
                    let c = [i as f64 / 4.0, j as f64 / 4.0, l as f64 / 4.0];
                    let want = apply(&q, c);
                    let got = s.corrected(&m.projectors[k], c);
                    for (a, b) in want.iter().zip(got) {
                        worst = worst.max((a - b).abs() / white);
                    }
                }
            }
        }
        let add = m.projectors[k].additivity.unwrap();
        assert!(add.iter().all(|v| (v - 1.0).abs() < 1e-9), "{add:?}");
        let [full, half] = m.projectors[k].peak;
        assert!(full <= half + 1e-9 && half <= 1.0 + 1e-9, "{full} {half}");
    }
    assert!(worst < 0.02, "worst error {worst} of white");
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.reference, 1, "the narrowest gamut is the one all can reach");
}

#[test]
fn unreachable_primaries_are_clamped_and_keep_grey() {
    let set = wide_set();
    let measured: Vec<Measured> = set.iter().map(|s| s.measure(8)).collect();
    let m = match_projectors(&measured, &[], 17, Target::Srgb).unwrap();
    assert!(m.warnings.iter().any(|w| w.contains("clamped")), "{:?}", m.warnings);
    let q = set[m.reference].p.map(|r| r.map(|v| v * m.alpha));
    let white = luminance(apply(&q, [1.0; 3]));
    for (k, s) in set.iter().enumerate() {
        for i in 0..=8 {
            let c = [i as f64 / 8.0; 3];
            let want = apply(&q, c);
            let got = s.corrected(&m.projectors[k], c);
            for (a, b) in want.iter().zip(got) {
                assert!((a - b).abs() / white < 0.02, "projector {k} grey {i}: {want:?} {got:?}");
            }
        }
    }
}

#[test]
fn blend_matrix_goes_from_identity_to_full() {
    let m = [[0.5, 0.1, 0.0], [0.0, 0.6, 0.1], [0.05, 0.0, 0.7]];
    assert_eq!(blend_matrix(&m, 0.0), IDENTITY);
    assert_eq!(blend_matrix(&m, 1.0), m);
    let h = blend_matrix(&m, 0.5);
    assert!((h[0][0] - 0.75).abs() < 1e-12 && (h[0][1] - 0.05).abs() < 1e-12);
    assert_eq!(blend_matrix(&m, f64::NAN), m);
}

#[test]
fn bad_measurements_are_errors() {
    let good = synth_set()[0].measure(4);
    let mut short = good.clone();
    short.ramps[1].pop();
    assert!(match_projectors(&[short], &[], 17, Target::Srgb).is_err());
    let mut nan = good.clone();
    nan.ramps[0][1][2] = f64::NAN;
    assert!(match_projectors(&[nan], &[], 17, Target::Srgb).is_err());
    let mut dark = good.clone();
    dark.ramps[2] = vec![[0.0; 3]; 4];
    assert!(match_projectors(&[dark], &[], 17, Target::Srgb).unwrap_err().contains("not brighter"));
    let mut alike = good.clone();
    alike.ramps[1] = alike.ramps[0].clone();
    assert!(match_projectors(&[alike], &[], 17, Target::Srgb).unwrap_err().contains("alike"));
    assert!(match_projectors(std::slice::from_ref(&good), &[], 1, Target::Srgb).is_err());
    assert!(match_projectors(&[], &[], 17, Target::Srgb).is_err());
    let mut levels = good;
    levels.levels = vec![0.5, 0.25, 0.75, 1.0];
    assert!(match_projectors(&[levels], &[], 17, Target::Srgb).is_err());
}

#[test]
fn a_noisy_ramp_still_gives_a_monotone_curve() {
    let s = &synth_set()[0];
    let mut m = s.measure(8);
    // A mid level reads brighter than the next (camera noise).
    m.ramps[1][3] = m.ramps[1][5];
    let r = match_projectors(&[m], &[], 17, Target::Srgb).unwrap();
    for curve in &r.projectors[0].curves {
        assert!(curve.windows(2).all(|w| w[1] >= w[0]), "{curve:?}");
        assert_eq!(curve[0], 0.0);
        assert!((curve[16] - 1.0).abs() < 1e-9);
    }
}

fn picture(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Picture {
    let mut rgb = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgb.push([f(x, y); 3]);
        }
    }
    Picture { width: w, height: h, rgb }
}

#[test]
fn masks_spots_and_means() {
    let (w, h) = (40, 20);
    let black = picture(w, h, |_, _| 0.01);
    // Projector a lights x < 25, b lights x >= 15: an overlap of 10 columns.
    let a = picture(w, h, |x, _| if x < 25 { 0.5 } else { 0.01 });
    let b = picture(w, h, |x, _| if x >= 15 { 0.4 } else { 0.01 });
    let la = lit(&a, &black).unwrap();
    let lb = lit(&b, &black).unwrap();
    assert_eq!(count(&la), 25 * 20);
    let s = spots(&[la, lb], w, h);
    assert!(s.shared);
    assert!(s.masks[0].iter().enumerate().filter(|(_, m)| **m).all(|(i, _)| (15..25).contains(&(i % w))));
    assert!(count(&s.masks[0]) >= MIN_PIXELS);
    // Apart: each in its own middle.
    let c = picture(w, h, |x, _| if x < 10 { 0.5 } else { 0.01 });
    let d = picture(w, h, |x, _| if x >= 30 { 0.5 } else { 0.01 });
    let s = spots(&[lit(&c, &black).unwrap(), lit(&d, &black).unwrap()], w, h);
    assert!(!s.shared);
    assert!(s.masks[1].iter().enumerate().filter(|(_, m)| **m).all(|(i, _)| i % w >= 30));
    // Nothing lit.
    assert!(lit(&black, &black).is_none());
    // A trimmed mean ignores a few hot pixels.
    let mut hot = picture(w, h, |_, _| 0.3);
    hot.rgb[5] = [100.0; 3];
    hot.rgb[6] = [f32::NAN; 3];
    let mean = trimmed_mean(&hot, &vec![true; w * h]).unwrap();
    assert!((mean[0] - 0.3).abs() < 1e-6, "{mean:?}");
    assert!(trimmed_mean(&hot, &vec![false; w * h]).is_none());
}

#[test]
fn camera_planes_to_linear() {
    // Full range BT.709 grey 128 is sRGB 128 / 255.
    let (w, h) = (4usize, 2usize);
    let y = vec![128u8; w * h];
    let cb = vec![128u8; 2];
    let cr = vec![128u8; 2];
    let p = Planes {
        width: 4,
        height: 2,
        y: &y,
        cb: &cb,
        cr: &cr,
        shift: (1, 1),
        kr_kb: (0.2126, 0.0722),
        full_range: true,
    };
    let pic = linear_picture(&p, 2).unwrap();
    assert_eq!((pic.width, pic.height), (2, 1));
    assert!((f64::from(pic.rgb[0][1]) - decode(128.0 / 255.0)).abs() < 1e-5);
    let short = Planes { y: &y[..3], ..p };
    assert!(linear_picture(&short, 1).is_err());
}

/// The camera's own tone curve per channel on top of the light (an uncalibrated camera).
fn through_camera(m: &Measured, gamma: [f64; 3]) -> Measured {
    let ramps = m
        .ramps
        .clone()
        .map(|r| r.into_iter().map(|v| [0, 1, 2].map(|c| v[c].max(0.0).powf(gamma[c]))).collect());
    Measured { levels: m.levels.clone(), ramps }
}

#[test]
fn reference_target_holds_through_a_nonlinear_camera() {
    // Same primaries (no crosstalk), different tone curves per channel.
    let p = [[0.6, 0.0, 0.0], [0.0, 0.7, 0.0], [0.0, 0.0, 0.5]];
    let a = Synth { p, gamma: [2.2, 2.2, 2.2] };
    let b = Synth { p, gamma: [2.6, 1.9, 3.0] };
    let camera = [1.6, 1.3, 1.9];
    let measured: Vec<Measured> = [&a, &b].iter().map(|s| through_camera(&s.measure(8), camera)).collect();
    let m = match_projectors(&measured, &[], 17, Target::Reference).unwrap();
    let r = m.reference;
    for (i, v) in m.projectors[r].curves.iter().flatten().enumerate() {
        assert!((v - (i % 17) as f64 / 16.0).abs() < 1e-12, "the reference keeps straight curves");
    }
    let mut worst: f64 = 0.0;
    for i in 0..=8 {
        for c in [[1.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.2, 0.5, 0.9]] {
            let c = c.map(|v| v * i as f64 / 8.0);
            let la = a.corrected(&m.projectors[0], c);
            let lb = b.corrected(&m.projectors[1], c);
            for (x, y) in la.iter().zip(lb) {
                worst = worst.max((x - y).abs() / 0.5);
            }
        }
    }
    assert!(worst < 0.02, "worst {worst}");
    // The sRGB target bends the reference too, by the camera's curve.
    let s = match_projectors(&measured, &[], 17, Target::Srgb).unwrap();
    assert!((s.projectors[r].curves[0][8] - 0.5).abs() > 0.05, "{:?}", s.projectors[r].curves[0]);
}

#[test]
fn broyden_solves_coupled_channels_through_a_nonlinear_camera() {
    // White through rows scaled by e^x, seen by a camera with crosstalk and a curve per channel.
    let p = [[0.30, 0.10, 0.05], [0.12, 0.35, 0.04], [0.01, 0.04, 0.32]];
    let rows = [0.5, 0.45, 0.4];
    let camera = [1.5, 2.0, 2.5];
    let read = |x: [f64; 3]| {
        let d = [0, 1, 2].map(|c| rows[c] * x[c].exp());
        let light = apply(&p, d);
        [0, 1, 2].map(|c| light[c].powf(camera[c]))
    };
    let want = read([0.3, 0.6, 0.9]);
    let mut b = Broyden::new();
    let mut x = [0.0; 3];
    let mut worst = f64::MAX;
    for _ in 0..6 {
        let got = read(x);
        let f = [0, 1, 2].map(|c| (got[c] / want[c]).ln());
        worst = f.iter().fold(0.0_f64, |a, v| a.max(v.abs()));
        x = b.step(x, f, -3.0, [1.5; 3]);
    }
    assert!(worst < 0.01, "{worst}");
    // Limits hold; nonsense does not move it.
    assert_eq!(Broyden::new().step([0.0; 3], [-5.0, 0.0, 0.0], -1.0, [0.2; 3])[0], 0.2);
    assert_eq!(Broyden::new().step([0.0; 3], [f64::NAN, 0.0, 0.0], -1.0, [1.0; 3]), [0.0; 3]);
}

#[test]
fn signal_follows_the_output_pass() {
    let curves = [vec![0.0, 1.0], vec![0.0, 0.5], vec![0.0, 1.0]];
    let s = signal(&[[0.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0]], &curves, [1.0, 1.0, 0.25]);
    assert!(
        (s[0] - encode(0.5)).abs() < 1e-12
            && (s[1] - 0.5).abs() < 1e-12
            && (s[2] - encode(0.5)).abs() < 1e-12
    );
}
