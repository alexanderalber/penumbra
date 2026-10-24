//! Straight borders on made-up label pictures: ragged lines come out straight, curves keep
//! their measured raster, two planes meet on their line, objects and the frame stay.

use crate::Homography;
use crate::borders::{BorderOptions, How, straighten};

struct Noise(u64);

impl Noise {
    /// -1 to 1.
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// Per pixel the label `side` gives its centre, the border moved by `noise` pixels (up to
/// `amount`, a new offset every 3 pixels along x) and `shift`.
fn picture(
    w: usize,
    h: usize,
    amount: f64,
    shift: f64,
    side: impl Fn(f64, f64) -> (f64, u16, u16),
) -> Vec<u16> {
    let mut n = Noise(99);
    let offsets: Vec<f64> = (0..(w + h) / 3 + 2).map(|_| n.next() * amount).collect();
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
            let (d, a, b) = side(x, y);
            let k = ((x + y) / 3.0) as usize;
            if d + offsets[k] + shift > 0.0 { a } else { b }
        })
        .collect()
}

/// Share of the pixels within `band` of the border (`dist`) that carry the label truth gives.
fn agreement(w: usize, labels: &[u16], band: f64, side: impl Fn(f64, f64) -> (f64, u16, u16)) -> f64 {
    let (mut near, mut right) = (0usize, 0usize);
    for (i, l) in labels.iter().enumerate() {
        let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
        let (d, a, b) = side(x, y);
        // Pixel centres right on the line belong to either side.
        if d.abs() > band || d.abs() < 1e-9 {
            continue;
        }
        near += 1;
        if *l == if d > 0.0 { a } else { b } {
            right += 1;
        }
    }
    right as f64 / near.max(1) as f64
}

#[test]
fn a_ragged_straight_border_comes_out_straight() {
    let (w, h) = (240, 160);
    // y = 0.3 x + 40: label 1 above, 2 below.
    let s = 0.3f64.hypot(1.0);
    let side = |x: f64, y: f64| ((0.3 * x + 40.0 - y) / s, 1u16, 2u16);
    let labels = picture(w, h, 1.0, 0.0, side);
    let before = agreement(w, &labels, 3.0, side);
    let out = straighten(w, h, &labels, 100, &[], &BorderOptions::default());
    let after = agreement(w, &out.labels, 3.0, side);
    assert!(before < 0.95, "the test border is ragged: {before}");
    assert!(after > 0.99, "straight: {after} (before {before}), {:?}", out.stats);
    assert!(out.runs.iter().all(|r| r.how == How::Fitted));
    assert!(!out.runs.is_empty());
}

#[test]
fn a_curve_keeps_its_measured_outline() {
    for radius in [40.0, 120.0] {
        let (w, h) = (320usize, 320usize);
        let side = |x: f64, y: f64| (radius - (x - 160.0).hypot(y - 160.0), 3u16, 0u16);
        let labels = picture(w, h, 0.0, 0.0, side);
        let out = straighten(w, h, &labels, 100, &[], &BorderOptions::default());
        let perimeter = 2.0 * std::f64::consts::PI * radius;
        assert!(
            (out.stats.changed as f64) < 0.05 * perimeter,
            "radius {radius}: {} pixels changed, runs {:?}",
            out.stats.changed,
            out.runs
        );
    }
}

#[test]
fn a_square_gets_straight_edges_and_sharp_corners() {
    let (w, h) = (200, 200);
    // A square turned by 10 degrees, 120 px a side, centred.
    let (c, s) = (10f64.to_radians().cos(), 10f64.to_radians().sin());
    let side = |x: f64, y: f64| {
        let (u, v) = ((x - 100.0) * c + (y - 100.0) * s, -(x - 100.0) * s + (y - 100.0) * c);
        (60.0 - u.abs().max(v.abs()), 5u16, 0u16)
    };
    let labels = picture(w, h, 1.0, 0.0, side);
    let before = agreement(w, &labels, 3.0, side);
    let out = straighten(w, h, &labels, 100, &[], &BorderOptions::default());
    let after = agreement(w, &out.labels, 3.0, side);
    assert!(after > 0.98 && after > before, "{after} vs {before}; {:?}", out.stats);
    assert!(out.runs.len() >= 4, "{:?}", out.runs);
}

#[test]
fn objects_and_the_frame_keep_their_pixels() {
    let (w, h) = (120, 90);
    let side = |x: f64, y: f64| ((0.5 * x + 20.0 - y) / 0.5f64.hypot(1.0), 7u16, 300u16);
    let labels = picture(w, h, 1.5, 0.0, side);
    // Label 300 is an object: its border with face 7 stays measured.
    let out = straighten(w, h, &labels, 256, &[], &BorderOptions::default());
    assert_eq!(out.labels, labels);
    assert!(out.runs.is_empty());
    // Every label fixed: nothing changes either.
    let out = straighten(w, h, &labels, 1, &[], &BorderOptions::default());
    assert_eq!(out.labels, labels);
}

#[test]
fn two_planes_meet_on_their_line() {
    let (w, h) = (300, 200);
    // The faces' maps into one view: F an affine map, G = F after a homology whose axis is the
    // line y = 0.2 x + 80 (0.2 x - y + 80 = 0), so F and G send that line to the same place.
    let f = Homography::from_rows([[1.1, 0.05, 30.0], [-0.02, 0.95, 12.0], [0.0, 0.0, 1.0]]);
    let a = [0.2, -1.0, 80.0];
    let v = [150.0, -400.0, 1.0];
    let av: f64 = a.iter().zip(&v).map(|(x, y)| x * y).sum();
    let k = 0.6 / av;
    let hom: [[f64; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|c| f64::from(u8::from(r == c)) + k * v[r] * a[c]));
    let g = f.after(&Homography::from_rows(hom));
    let views = vec![vec![Some(f)], vec![Some(g)]];
    let s = 0.2f64.hypot(1.0);
    let side = |x: f64, y: f64| ((0.2 * x + 80.0 - y) / s, 1u16, 2u16);
    // Ragged, and the measured border sits 0.7 px off the true one (pixels at a crease go to
    // whichever face fits them better).
    let labels = picture(w, h, 1.0, 0.7, side);
    let out = straighten(w, h, &labels, 256, &views, &BorderOptions::default());
    assert!(out.runs.iter().any(|r| r.how == How::Meeting), "{:?} {:?}", out.runs, out.stats);
    assert_eq!(out.stats.meeting_held, 1);
    // On the meeting line, not on the measured border's offset.
    let exact = agreement(w, &out.labels, 2.0, side);
    assert!(exact > 0.995, "{exact} {:?} {:?}", out.runs, out.stats);
    // Without the planes, the fitted line follows the offset.
    let fitted = straighten(w, h, &labels, 256, &[], &BorderOptions::default());
    assert!(agreement(w, &fitted.labels, 2.0, side) < exact);
}

#[test]
fn planes_that_do_not_meet_there_fall_back_to_the_fitted_line() {
    let (w, h) = (300, 200);
    // The same maps, but the measured border lies 30 px away from where the planes meet (one
    // face in front of the other).
    let f = Homography::from_rows([[1.1, 0.05, 30.0], [-0.02, 0.95, 12.0], [0.0, 0.0, 1.0]]);
    let a = [0.2, -1.0, 110.0];
    let v = [150.0, -400.0, 1.0];
    let av: f64 = a.iter().zip(&v).map(|(x, y)| x * y).sum();
    let k = 0.6 / av;
    let hom: [[f64; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|c| f64::from(u8::from(r == c)) + k * v[r] * a[c]));
    let g = f.after(&Homography::from_rows(hom));
    let views = vec![vec![Some(f)], vec![Some(g)]];
    let s = 0.2f64.hypot(1.0);
    let side = |x: f64, y: f64| ((0.2 * x + 80.0 - y) / s, 1u16, 2u16);
    let labels = picture(w, h, 1.0, 0.0, side);
    let out = straighten(w, h, &labels, 256, &views, &BorderOptions::default());
    assert!(out.runs.iter().all(|r| r.how == How::Fitted) && !out.runs.is_empty(), "{:?}", out.runs);
    assert!(agreement(w, &out.labels, 3.0, side) > 0.99);
}

#[test]
fn speckle_is_drawn_back_exactly() {
    // Every pixel its own pseudo-random label of four: junctions and checkerboard corners
    // everywhere, nothing straight; filling the picture again from its chains gives it back.
    let (w, h) = (97, 61);
    let mut n = Noise(5);
    let labels: Vec<u16> = (0..w * h).map(|_| ((n.next() + 1.0) * 2.0) as u16 % 4).collect();
    let out = straighten(w, h, &labels, 100, &[], &BorderOptions::default());
    assert!(out.runs.is_empty());
    assert_eq!(out.labels, labels);
}

#[test]
fn a_seam_between_two_planes_closes_on_their_line() {
    let (w, h) = (300, 200);
    let f = Homography::from_rows([[1.1, 0.05, 30.0], [-0.02, 0.95, 12.0], [0.0, 0.0, 1.0]]);
    let a = [0.2, -1.0, 80.0];
    let v = [150.0, -400.0, 1.0];
    let av: f64 = a.iter().zip(&v).map(|(x, y)| x * y).sum();
    let k = 0.6 / av;
    let hom: [[f64; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|c| f64::from(u8::from(r == c)) + k * v[r] * a[c]));
    let g = f.after(&Homography::from_rows(hom));
    let views = vec![vec![Some(f)], vec![Some(g)]];
    let s = 0.2f64.hypot(1.0);
    let side = |x: f64, y: f64| ((0.2 * x + 80.0 - y) / s, 1u16, 2u16);
    // A ragged seam 2 to 5 px wide along the line where the planes meet, nothing measured there.
    let mut labels = picture(w, h, 1.0, 0.0, side);
    let mut n = Noise(17);
    for x in 0..w {
        let (up, down) = (1.0 + n.next().abs() * 1.5, 1.0 + n.next().abs() * 1.5);
        for y in 0..h {
            let d = side(x as f64 + 0.5, y as f64 + 0.5).0;
            if d < up && d > -down {
                labels[y * w + x] = 0;
            }
        }
    }
    let out = straighten(w, h, &labels, 256, &views, &BorderOptions::default());
    assert_eq!(out.stats.seams_held, 1, "{:?}", out.stats);
    assert!(agreement(w, &out.labels, 3.0, side) > 0.995, "{:?}", out.stats);
    // Without the planes the seam stays open.
    let open = straighten(w, h, &labels, 256, &[], &BorderOptions::default());
    assert!(open.labels.iter().filter(|l| **l == 0).count() > 2 * w);
}
