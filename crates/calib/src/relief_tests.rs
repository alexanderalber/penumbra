//! Relief against a synthetic room: a slightly tilted panel with a bump on it, standing far in
//! front of a wall; a camera and two projectors beside it (baselines 1 and 2), all looking
//! straight at the wall. Maps are ray cast, with occlusion.

use crate::Homography;
use crate::objects::Map;
use crate::relief::{
    Heuristic, ReliefOptions, View, analyse, camera_heights, fill, overlap, range, scale_fit,
};

const WALL: f32 = 40.0;
const CAMERA: (usize, usize) = (200, 160);
const PROJECTOR: (usize, usize) = (160, 120);
const FOCAL: f32 = 100.0;
/// Projector A one unit left of the camera, B two units right: B sees twice A's parallax.
const OFFSETS: [f32; 2] = [-1.0, 2.0];
const PANEL_X: [f32; 2] = [-6.0, 6.5];
const PANEL_Y: [f32; 2] = [-4.0, 4.0];

fn bump(x: f32, y: f32) -> f32 {
    1.5 * (-((x - 1.0).powi(2) + y * y) / 2.0).exp()
}

fn panel_z(x: f32, y: f32) -> f32 {
    8.0 + 0.05 * x - bump(x, y)
}

/// The first surface along a ray from `(ox, 0, 0)` in direction `(dx, dy, 1)`, and whether it is
/// the panel.
fn hit(ox: f32, d: [f32; 2]) -> ([f32; 3], bool) {
    let mut t = 8.0;
    for _ in 0..60 {
        t = panel_z(ox + d[0] * t, d[1] * t);
    }
    let (x, y) = (ox + d[0] * t, d[1] * t);
    if (PANEL_X[0]..=PANEL_X[1]).contains(&x) && (PANEL_Y[0]..=PANEL_Y[1]).contains(&y) {
        return ([x, y, t], true);
    }
    ([ox + d[0] * WALL, d[1] * WALL, WALL], false)
}

fn seen_from(ox: f32, p: [f32; 3]) -> bool {
    let (q, _) = hit(ox, [(p[0] - ox) / p[2], p[1] / p[2]]);
    (q[2] - p[2]).abs() < 1e-3
}

/// The projector's `proj_to_cam` map (every third pixel only filled in, as a coarser camera
/// leaves them) and per pixel the bump's height where it lights the panel.
fn to_camera(ox: f32) -> (Map, Vec<Option<f32>>) {
    let (pw, ph) = PROJECTOR;
    let mut rgba = Vec::with_capacity(pw * ph);
    let mut truth = Vec::with_capacity(pw * ph);
    for v in 0..ph {
        for u in 0..pw {
            let d = [(u as f32 + 0.5 - pw as f32 / 2.0) / FOCAL, (v as f32 + 0.5 - ph as f32 / 2.0) / FOCAL];
            let (p, on) = hit(ox, d);
            truth.push(on.then(|| bump(p[0], p[1])));
            let cx = FOCAL * p[0] / p[2] + CAMERA.0 as f32 / 2.0;
            let cy = FOCAL * p[1] / p[2] + CAMERA.1 as f32 / 2.0;
            let inside = (0.0..CAMERA.0 as f32).contains(&cx) && (0.0..CAMERA.1 as f32).contains(&cy);
            let measured = if (u + v) % 3 == 0 { 0.5 } else { 1.0 };
            rgba.push(if seen_from(0.0, p) && inside {
                [cx / CAMERA.0 as f32, cy / CAMERA.1 as f32, measured, 1.0]
            } else {
                [-1.0, -1.0, 0.0, 1.0]
            });
        }
    }
    (Map::new(pw, ph, rgba).unwrap(), truth)
}

/// The projector's `cam_to_proj` map.
fn from_camera(ox: f32) -> Map {
    let (cw, ch) = CAMERA;
    let mut rgba = Vec::with_capacity(cw * ch);
    for y in 0..ch {
        for x in 0..cw {
            let d = [(x as f32 + 0.5 - cw as f32 / 2.0) / FOCAL, (y as f32 + 0.5 - ch as f32 / 2.0) / FOCAL];
            let (p, _) = hit(0.0, d);
            let u = FOCAL * (p[0] - ox) / p[2] + PROJECTOR.0 as f32 / 2.0;
            let v = FOCAL * p[1] / p[2] + PROJECTOR.1 as f32 / 2.0;
            let inside = (0.0..PROJECTOR.0 as f32).contains(&u) && (0.0..PROJECTOR.1 as f32).contains(&v);
            rgba.push(if seen_from(ox, p) && inside {
                [u / PROJECTOR.0 as f32, v / PROJECTOR.1 as f32, 1.0, 1.0]
            } else {
                [-1.0, -1.0, 0.0, 1.0]
            });
        }
    }
    Map::new(cw, ch, rgba).unwrap()
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x) - ma, f64::from(*y) - mb);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    sab / (saa * sbb).sqrt()
}

#[test]
fn height_mask_and_sign_from_one_scan() {
    let (map, truth) = to_camera(OFFSETS[0]);
    let r = analyse(&map, &from_camera(OFFSETS[0]), &ReliefOptions::default()).unwrap();
    assert!(matches!(r.heuristic, Heuristic::Wall { .. }), "{:?}", r.heuristic);
    assert!(!r.flipped);
    let panel = truth.iter().filter(|t| t.is_some()).count();
    let lit = r.mask.iter().filter(|m| **m).count();
    // The mask is on the panel only, and most of it (shrunk at its edge).
    let off = r.mask.iter().zip(&truth).filter(|(m, t)| **m && t.is_none()).count();
    assert_eq!(off, 0, "mask pixels off the panel");
    assert!(lit as f64 > 0.8 * panel as f64, "{lit} of {panel} panel pixels lit");
    // The height follows the bump, peak up (the wall is behind).
    let (h, t): (Vec<f32>, Vec<f32>) = r
        .mask
        .iter()
        .zip(&r.heights)
        .zip(&truth)
        .filter_map(|((m, h), t)| (*m).then_some((*h, (*t)?)))
        .unzip();
    let c = correlation(&h, &t);
    assert!(c > 0.99, "correlation {c}");
    assert!(r.error_px[0] < 2.0, "{:?}", r.error_px);
    // The tilt is the plane's, not the height's: the panel's left and right edges (no bump
    // there) stand about equally high.
    let at = |u: usize, v: usize| r.heights[v * PROJECTOR.0 + u];
    assert!((at(30, 60) - at(150, 60)).abs() < 0.5, "{} {}", at(30, 60), at(150, 60));
}

#[test]
fn second_projector_in_the_first_ones_units() {
    let o = ReliefOptions::default();
    let scans: Vec<(Map, Vec<Option<f32>>)> = OFFSETS.iter().map(|ox| to_camera(*ox)).collect();
    let cams: Vec<Map> = OFFSETS.iter().map(|ox| from_camera(*ox)).collect();
    let reliefs: Vec<_> = scans.iter().zip(&cams).map(|((m, _), c)| analyse(m, c, &o).unwrap()).collect();
    let view = |k: usize| View {
        from_camera: &cams[k],
        heights: &reliefs[k].heights,
        mask: &reliefs[k].mask,
        width: PROJECTOR.0,
        height: PROJECTOR.1,
    };
    let (x, y) = overlap(&view(1), &[view(0)]);
    assert!(x.len() > 2000, "{} camera pixels shared", x.len());
    let fit = scale_fit(&x, &y, true).unwrap();
    // B sees twice A's parallax: half its height is A's.
    assert!((fit.scale - 0.5).abs() < 0.05, "{fit:?}");
    assert!(fit.correlation > 0.98, "{fit:?}");
    assert!(fit.offset.abs() < 0.3, "{fit:?}");
    // In A's units B's heights match A's over the camera.
    let converted: Vec<f32> =
        reliefs[1].heights.iter().map(|h| (f64::from(*h) * fit.scale + fit.offset) as f32).collect();
    let b = View { heights: &converted, ..view(1) };
    let both = camera_heights(&[view(0), b], CAMERA);
    let a_only = camera_heights(&[view(0)], CAMERA);
    let b_only = camera_heights(&[b], CAMERA);
    let mut worst = 0f32;
    for c in 0..CAMERA.0 * CAMERA.1 {
        if let (Some(p), Some(q), Some(m)) = (a_only[c], b_only[c], both[c]) {
            worst = worst.max((p - q).abs());
            assert!((m - (p + q) / 2.0).abs() < 1e-4);
        }
    }
    assert!(worst < 0.6, "A and B differ by {worst} (A's bump is about 2.3)");
    let [lo, hi] = range(&reliefs[0].heights, 0.01, 0.995).unwrap();
    assert!(hi - lo > 1.5, "{lo} {hi}");
}

#[test]
fn flipping_turns_the_heights_over() {
    let (map, _) = to_camera(OFFSETS[0]);
    let mut r = analyse(&map, &from_camera(OFFSETS[0]), &ReliefOptions::default()).unwrap();
    let before = r.heights.clone();
    r.flip();
    assert!(r.flipped);
    assert!(r.heights.iter().zip(&before).all(|(a, b)| *a == -*b));
}

#[test]
fn bad_input_is_an_error() {
    let empty = Map::new(8, 8, vec![[-1.0, -1.0, 0.0, 1.0]; 64]).unwrap();
    let cam = from_camera(OFFSETS[0]);
    assert!(analyse(&empty, &cam, &ReliefOptions::default()).unwrap_err().contains("measured"));
    let (map, _) = to_camera(OFFSETS[0]);
    let o = ReliefOptions { jump: f32::NAN, ..ReliefOptions::default() };
    assert!(analyse(&map, &cam, &o).unwrap_err().contains("jump"));
    assert!(scale_fit(&[1.0; 10], &[2.0; 10], true).is_none(), "too few pairs");
    assert!(scale_fit(&[1.0; 1000], &[2.0; 1000], true).is_none(), "no spread");
}

/// A small deterministic noise source.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32 / (1u64 << 31) as f32) - 0.5
    }
}

#[test]
fn the_symmetric_scale_is_not_pulled_towards_zero() {
    let mut rng = Lcg(7);
    let (mut x, mut y) = (Vec::new(), Vec::new());
    for k in 0..4000 {
        let t = (k as f32 * 0.37).sin();
        // The same noise on both sides, in each one's units: least squares drops the slope.
        x.push(t + rng.next());
        y.push(2.0 * t + 2.0 * rng.next() + 3.0);
    }
    let f = scale_fit(&x, &y, true).unwrap();
    assert!((f.scale - 2.0).abs() < 0.1, "{f:?}");
    assert!(f.least_squares < 1.8, "{f:?}");
    assert!((f.offset - 3.0).abs() < 0.1, "{f:?}");
    let g = scale_fit(&x, &y, false).unwrap();
    assert_eq!(g.scale, g.least_squares);
}

#[test]
fn fill_continues_a_ramp_into_holes() {
    let (w, h) = (60, 40);
    let mut values = vec![0f32; w * h];
    let mut weights = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            // Measured on a sparse lattice, with a large hole in the middle.
            let hole = (20..40).contains(&x) && (10..30).contains(&y);
            if x % 3 == 0 && y % 2 == 0 && !hole {
                values[y * w + x] = 0.1 * x as f32;
                weights[y * w + x] = 1.0;
            }
        }
    }
    let f = fill(w, h, &values, &weights);
    for y in 5..35 {
        for x in 5..55 {
            let want = 0.1 * x as f32;
            assert!((f[y * w + x] - want).abs() < 0.35, "({x}, {y}): {} for {want}", f[y * w + x]);
        }
    }
}

#[test]
fn cauchy_fit_ignores_a_minority_elsewhere() {
    let truth = Homography::from_rows([[1.1, 0.05, 20.0], [-0.03, 0.95, 10.0], [1e-4, -5e-5, 1.0]]);
    let (mut from, mut to) = (Vec::new(), Vec::new());
    for y in 0..60 {
        for x in 0..80 {
            let p = [x as f64 * 4.0, y as f64 * 4.0];
            let mut q = truth.apply(p).unwrap();
            // A quarter of the points on something nearer: shifted by 30 px.
            if x < 40 && y < 30 {
                q[0] += 30.0;
            }
            from.push(p);
            to.push(q);
        }
    }
    let h = Homography::fit_cauchy(&from, &to, 10).unwrap();
    for p in [[200.0, 200.0], [300.0, 50.0], [20.0, 220.0]] {
        let (a, b) = (h.apply(p).unwrap(), truth.apply(p).unwrap());
        assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 0.5, "{p:?}: {a:?} vs {b:?}");
    }
    assert!(Homography::fit_cauchy(&from[..3], &to[..3], 10).is_err());
    assert!(Homography::fit_cauchy(&from, &to[..5], 10).is_err());
}

/// `map` with every pixel without a camera position given its row's nearest one within 32
/// pixels, marked as filled in, as `scan.decode` fills holes.
fn filled_like_decode(map: &Map) -> Map {
    let (w, h) = (map.width, map.height);
    let mut rgba = map.rgba.clone();
    for y in 0..h {
        for x in 0..w {
            if map.uv(y * w + x).is_some() {
                continue;
            }
            let near = (1..=32usize).find_map(|d| {
                [x.checked_sub(d), Some(x + d).filter(|v| *v < w)]
                    .into_iter()
                    .flatten()
                    .find(|xx| map.measured(y * w + xx))
            });
            if let Some(xx) = near {
                let p = map.rgba[y * w + xx];
                rgba[y * w + x] = [p[0], p[1], 0.5, 1.0];
            }
        }
    }
    Map::new(w, h, rgba).unwrap()
}

#[test]
fn wall_hidden_from_the_camera_stays_dark() {
    // Projector B, right of the camera, lights a strip of wall beside the panel's right edge
    // that the panel hides from the camera; the decode fills it in from the panel's edge.
    let (map, truth) = to_camera(OFFSETS[1]);
    let (pw, ph) = PROJECTOR;
    let hidden: Vec<usize> = (0..pw * ph)
        .filter(|i| truth[*i].is_none() && map.uv(*i).is_none() && (i % pw) > pw / 2 && (i % pw) + 2 < pw)
        .collect();
    assert!(hidden.len() > 1000, "the scene has a hidden strip: {}", hidden.len());
    let filled = filled_like_decode(&map);
    assert!(hidden.iter().filter(|i| filled.uv(**i).is_some()).count() * 2 > hidden.len());
    let r = analyse(&filled, &from_camera(OFFSETS[1]), &ReliefOptions::default()).unwrap();
    let lit_hidden = hidden.iter().filter(|i| r.mask[**i]).count();
    assert_eq!(lit_hidden, 0, "hidden wall pixels lit");
    let off = r.mask.iter().zip(&truth).filter(|(m, t)| **m && t.is_none()).count();
    assert_eq!(off, 0, "mask pixels off the panel");
    let panel = truth.iter().filter(|t| t.is_some()).count();
    let lit = r.mask.iter().filter(|m| **m).count();
    assert!(lit as f64 > 0.8 * panel as f64, "{lit} of {panel} panel pixels lit");
}

#[test]
fn unmeasured_panel_edge_stays_lit() {
    // Patches of dark paint along the panel's right edge in A's frame (x above 4, away from the
    // corners, every other stripe of rows): neither the camera's pictures of A nor A's map
    // decode there, the decode fills A's pixels in from their neighbours. They are notches in
    // the measured outline, nothing hides them from the camera, so they are panel and stay lit.
    let ox = OFFSETS[0];
    let (pw, ph) = PROJECTOR;
    let dark = |p: [f32; 3], on: bool| on && p[0] > 4.0 && p[1].abs() < 3.0 && (p[1] * 2.0).sin() > 0.0;
    let (map, truth) = to_camera(ox);
    let mut rgba = map.rgba.clone();
    let mut band = Vec::new();
    for v in 0..ph {
        for u in 0..pw {
            let d = [(u as f32 + 0.5 - pw as f32 / 2.0) / FOCAL, (v as f32 + 0.5 - ph as f32 / 2.0) / FOCAL];
            let (p, on) = hit(ox, d);
            if dark(p, on) {
                rgba[v * pw + u] = [-1.0, -1.0, 0.0, 1.0];
                band.push(v * pw + u);
            }
        }
    }
    let map = filled_like_decode(&Map::new(pw, ph, rgba).unwrap());
    let cam = from_camera(ox);
    let (cw, ch) = CAMERA;
    let mut c = cam.rgba.clone();
    for y in 0..ch {
        for x in 0..cw {
            let d = [(x as f32 + 0.5 - cw as f32 / 2.0) / FOCAL, (y as f32 + 0.5 - ch as f32 / 2.0) / FOCAL];
            let (p, on) = hit(0.0, d);
            if dark(p, on) {
                c[y * cw + x] = [-1.0, -1.0, 0.0, 1.0];
            }
        }
    }
    let cam = Map::new(cw, ch, c).unwrap();
    let r = analyse(&map, &cam, &ReliefOptions::default()).unwrap();
    assert!(band.len() > 500, "{}", band.len());
    // The patches run to the frame edge, where erosion takes nothing.
    let lit = band.iter().filter(|i| r.mask[**i]).count();
    assert!(lit as f64 > 0.95 * band.len() as f64, "{lit} of {} band pixels lit", band.len());
    let off = r.mask.iter().zip(&truth).filter(|(m, t)| **m && t.is_none()).count();
    assert_eq!(off, 0, "mask pixels off the panel");
}

#[test]
fn wall_beside_the_panel_stays_dark_when_the_camera_decodes_nothing_there() {
    // As in the hidden-wall case, but the camera decodes no wall at all (too dark, or out of
    // focus): the occlusion test finds no other projector pixel where the strip's light would
    // land, so only the measured outline keeps it dark.
    let ox = OFFSETS[1];
    let (map, truth) = to_camera(ox);
    let (pw, ph) = PROJECTOR;
    let hidden: Vec<usize> = (0..pw * ph)
        .filter(|i| truth[*i].is_none() && map.uv(*i).is_none() && (i % pw) > pw / 2 && (i % pw) + 2 < pw)
        .collect();
    let filled = filled_like_decode(&map);
    let cam = from_camera(ox);
    let (cw, ch) = CAMERA;
    let mut c = cam.rgba.clone();
    for y in 0..ch {
        for x in 0..cw {
            let d = [(x as f32 + 0.5 - cw as f32 / 2.0) / FOCAL, (y as f32 + 0.5 - ch as f32 / 2.0) / FOCAL];
            if !hit(0.0, d).1 {
                c[y * cw + x] = [-1.0, -1.0, 0.0, 1.0];
            }
        }
    }
    let cam = Map::new(cw, ch, c).unwrap();
    let r = analyse(&filled, &cam, &ReliefOptions::default()).unwrap();
    let lit_hidden = hidden.iter().filter(|i| r.mask[**i]).count();
    assert_eq!(lit_hidden, 0, "hidden wall pixels lit");
    let off = r.mask.iter().zip(&truth).filter(|(m, t)| **m && t.is_none()).count();
    assert_eq!(off, 0, "mask pixels off the panel");
}
