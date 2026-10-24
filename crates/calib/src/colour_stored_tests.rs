use super::*;
use crate::colour::{IDENTITY, mul};

/// A synthetic projector seen by a camera: light `P s^gamma`, the camera reading
/// `light^kappa` per channel (not linear, but monotone and the same for every projector).
struct Synth {
    p: Mat3,
    gamma: [f64; 3],
}

const KAPPA: [f64; 3] = [1.4, 1.6, 1.3];

fn camera(light: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|c| light[c].max(0.0).powf(KAPPA[c]))
}

impl Synth {
    fn read(&self, s: [f64; 3]) -> [f64; 3] {
        let d = [0, 1, 2].map(|c| s[c].clamp(0.0, 1.0).powf(self.gamma[c]));
        camera(apply(&self.p, d))
    }

    fn stored(&self, levels: usize, greys: bool) -> Stored {
        let levels: Vec<f64> = (1..=levels).map(|i| i as f64 / levels as f64).collect();
        let ramps = [0, 1, 2].map(|c| {
            levels
                .iter()
                .map(|l| {
                    let mut s = [0.0; 3];
                    s[c] = *l;
                    self.read(s)
                })
                .collect()
        });
        let greys = greys.then(|| levels.iter().map(|l| self.read([*l; 3])).collect());
        Stored { measured: Measured { levels, ramps }, greys }
    }

    /// The camera's reading of grey `x` through `corr`, as the output pass computes the signal.
    fn grey(&self, corr: &FromStored, x: f64) -> [f64; 3] {
        let d = apply(&corr.matrix, [x; 3]).map(|v| v.clamp(0.0, 1.0));
        self.read([0, 1, 2].map(|c| lookup(&corr.curves[c], encode(d[c]))))
    }
}

fn diag(r: f64, g: f64, b: f64) -> Mat3 {
    [[r, 0.0, 0.0], [0.0, g, 0.0], [0.0, 0.0, b]]
}

fn within(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol * y.abs().max(1e-3))
}

#[test]
fn the_dimmest_projector_keeps_its_picture_and_the_other_follows_it() {
    let dim = Synth { p: diag(0.35, 0.43, 0.44), gamma: [2.2; 3] };
    let bright = Synth { p: diag(0.56, 0.80, 0.86), gamma: [2.4, 2.0, 2.6] };
    let m = match_stored(&[bright.stored(16, true), dim.stored(16, true)], 33).unwrap();
    assert_eq!(m.limited_by, [1, 1, 1]);
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let d = &m.projectors[1];
    assert_eq!(d.matrix, IDENTITY);
    for c in d.curves.iter() {
        for (i, v) in c.iter().enumerate() {
            assert!((v - i as f64 / 32.0).abs() < 1e-9, "{c:?}");
        }
    }
    assert!((d.peak[0] - 1.0).abs() < 1e-9);
    let b = &m.projectors[0];
    assert!(b.peak[0] < 0.8 && b.peak[1] > b.peak[0], "{:?}", b.peak);
    for x in [0.1, 0.25, 0.5, 0.75, 1.0] {
        let want = dim.grey(d, x);
        let got = bright.grey(b, x);
        assert!(within(got, want, 0.02), "grey {x}: {got:?} against {want:?}");
    }
}

#[test]
fn crossed_whites_meet_between_the_white_points() {
    // One is dimmer in red, the other in blue: the common white takes each from the dimmer.
    let warm = Synth { p: diag(0.60, 0.50, 0.30), gamma: [2.2; 3] };
    let cold = Synth { p: diag(0.40, 0.55, 0.60), gamma: [2.2; 3] };
    let m = match_stored(&[warm.stored(16, true), cold.stored(16, true)], 33).unwrap();
    assert_eq!(m.limited_by, [1, 0, 0]);
    let white = |s: &Synth| s.read([1.0; 3]);
    assert!(within(m.target_white, [white(&cold)[0], white(&warm)[1], white(&warm)[2]], 1e-9));
    for (s, corr) in [&warm, &cold].into_iter().zip(&m.projectors) {
        for x in [0.25, 0.5, 1.0] {
            // Each channel follows the projector that limits its white.
            let want =
                [cold.read([encode(x); 3])[0], warm.read([encode(x); 3])[1], warm.read([encode(x); 3])[2]];
            let got = s.grey(corr, x);
            assert!(within(got, want, 0.02), "grey {x}: {got:?} against {want:?}");
        }
    }
    // Neither keeps all of its white, and matching to either one's white would lose more.
    let gain_over_warm = between_gain(&[white(&warm), white(&cold)], white(&warm), 0.4 / 0.6);
    assert!(gain_over_warm > 1.05, "{gain_over_warm}");
    assert!(m.projectors.iter().all(|p| p.peak[0] < 1.0));
    assert!((between_gain(&[white(&warm), white(&warm)], white(&warm), 1.0) - 1.0).abs() < 1e-12);
}

#[test]
fn crosstalk_leaves_greys_close() {
    let p0 = [[0.50, 0.04, 0.01], [0.05, 0.60, 0.03], [0.01, 0.05, 0.55]];
    let narrow = [[0.70, 0.03, 0.01], [0.04, 0.62, 0.03], [0.01, 0.03, 0.66]];
    let a = Synth { p: p0, gamma: [2.2; 3] };
    let b = Synth { p: mul(&p0, &narrow), gamma: [2.4, 2.0, 2.6] };
    let m = match_stored(&[a.stored(8, true), b.stored(8, true)], 17).unwrap();
    for x in [0.25, 0.5, 0.75, 1.0] {
        let got = a.grey(&m.projectors[0], x);
        let want = b.grey(&m.projectors[1], x);
        assert!(within(got, want, 0.05), "grey {x}: {got:?} against {want:?}");
    }
}

#[test]
fn without_greys_the_primaries_stand_in_and_say_so() {
    let dim = Synth { p: diag(0.3, 0.3, 0.3), gamma: [2.2; 3] };
    let bright = Synth { p: diag(0.6, 0.6, 0.6), gamma: [2.2; 3] };
    let m = match_stored(&[dim.stored(16, false), bright.stored(16, true)], 33).unwrap();
    assert_eq!(m.warnings.len(), 1);
    assert_eq!(m.warnings[0].projector, Some(0));
    // Without crosstalk a primary is its channel of grey: the same match.
    let got = bright.grey(&m.projectors[1], 0.5);
    let want = dim.grey(&m.projectors[0], 0.5);
    assert!(within(got, want, 0.02), "{got:?} against {want:?}");
}

#[test]
fn hostile_measurements_are_refused() {
    let s = Synth { p: diag(0.5, 0.5, 0.5), gamma: [2.2; 3] }.stored(4, true);
    assert!(match_stored(&[], 17).is_err());
    assert!(match_stored(std::slice::from_ref(&s), 1).is_err());
    let mut short = s.clone();
    short.greys = Some(vec![[0.1; 3]]);
    assert!(match_stored(&[short], 17).unwrap_err().text.contains("greys"));
    let mut nan = s.clone();
    if let Some(g) = nan.greys.as_mut() {
        g[1][2] = f64::NAN;
    }
    assert!(match_stored(&[nan], 17).is_err());
    let mut dark = s.clone();
    dark.greys = Some(vec![[0.0; 3]; 4]);
    assert!(match_stored(&[s.clone(), dark], 17).unwrap_err().text.contains("reads no red"));
    let mut clipped = s;
    clipped.greys = Some(vec![[0.2; 3], [0.5; 3], [0.9; 3], [1.0; 3]]);
    let m = match_stored(&[clipped], 17).unwrap();
    assert_eq!(m.warnings.len(), 3, "{:?}", m.warnings);
}
