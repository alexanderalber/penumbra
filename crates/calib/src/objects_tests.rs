//! Objects and blend against a synthetic room: a wall, two boxes standing in front of it one
//! above the other at different depths, two projectors left and right of a camera, all looking
//! straight at the wall. Maps are ray cast exactly, with shadows and occlusion.

#![allow(clippy::needless_range_loop)] // pixels by index read clearer in these checks

use crate::objects::{
    BlendInput, BlendOptions, Map, ObjectOptions, blend, camera_labels, number, segment, smooth,
};
use crate::raster;

const WALL: f32 = 10.0;
/// Front faces: depth, x range, y range (y down).
const BOXES: [(f32, [f32; 2], [f32; 2]); 2] =
    [(6.0, [-1.0, 1.0], [-4.0, -1.0]), (8.0, [-1.5, 1.5], [-1.33, 3.0])];
const CAMERA: (usize, usize) = (200, 160);
const PROJECTOR: (usize, usize) = (160, 120);
const FOCAL: f32 = 100.0;
const OFFSETS: [f32; 2] = [-3.0, 3.0];

/// The first surface along a ray from `(ox, 0, 0)` in direction `(dx, dy, 1)`: its point.
fn hit(ox: f32, d: [f32; 2], boxes: bool) -> [f32; 3] {
    if boxes {
        for (z, xs, ys) in BOXES {
            let (x, y) = (ox + d[0] * z, d[1] * z);
            if (xs[0]..=xs[1]).contains(&x) && (ys[0]..=ys[1]).contains(&y) {
                return [x, y, z];
            }
        }
    }
    [ox + d[0] * WALL, d[1] * WALL, WALL]
}

fn seen_from(ox: f32, p: [f32; 3], boxes: bool) -> bool {
    let q = hit(ox, [(p[0] - ox) / p[2], p[1] / p[2]], boxes);
    (q[2] - p[2]).abs() < 1e-3
}

/// The projector's `proj_to_cam` map.
fn to_camera(ox: f32, boxes: bool) -> Map {
    let (pw, ph) = PROJECTOR;
    let mut rgba = Vec::with_capacity(pw * ph);
    for v in 0..ph {
        for u in 0..pw {
            let d = [(u as f32 + 0.5 - pw as f32 / 2.0) / FOCAL, (v as f32 + 0.5 - ph as f32 / 2.0) / FOCAL];
            let p = hit(ox, d, boxes);
            let cx = FOCAL * p[0] / p[2] + CAMERA.0 as f32 / 2.0;
            let cy = FOCAL * p[1] / p[2] + CAMERA.1 as f32 / 2.0;
            let inside = (0.0..CAMERA.0 as f32).contains(&cx) && (0.0..CAMERA.1 as f32).contains(&cy);
            rgba.push(if seen_from(0.0, p, boxes) && inside {
                [cx / CAMERA.0 as f32, cy / CAMERA.1 as f32, 1.0, 1.0]
            } else {
                [-1.0, -1.0, 0.0, 1.0]
            });
        }
    }
    Map::new(pw, ph, rgba).unwrap()
}

/// The projector's `cam_to_proj` map.
fn from_camera(ox: f32) -> Map {
    let (cw, ch) = CAMERA;
    let mut rgba = Vec::with_capacity(cw * ch);
    for y in 0..ch {
        for x in 0..cw {
            let d = [(x as f32 + 0.5 - cw as f32 / 2.0) / FOCAL, (y as f32 + 0.5 - ch as f32 / 2.0) / FOCAL];
            let p = hit(0.0, d, true);
            let u = FOCAL * (p[0] - ox) / p[2] + PROJECTOR.0 as f32 / 2.0;
            let v = FOCAL * p[1] / p[2] + PROJECTOR.1 as f32 / 2.0;
            let inside = (0.0..PROJECTOR.0 as f32).contains(&u) && (0.0..PROJECTOR.1 as f32).contains(&v);
            rgba.push(if seen_from(ox, p, true) && inside {
                [u / PROJECTOR.0 as f32, v / PROJECTOR.1 as f32, 1.0, 1.0]
            } else {
                [-1.0, -1.0, 0.0, 1.0]
            });
        }
    }
    Map::new(cw, ch, rgba).unwrap()
}

/// The projector pixel lighting the centre of box `b`, and the camera pixel seeing it.
fn centre(b: usize, ox: f32) -> (usize, usize) {
    let (z, xs, ys) = BOXES[b];
    let (x, y) = ((xs[0] + xs[1]) / 2.0, (ys[0] + ys[1]) / 2.0);
    let u = (FOCAL * (x - ox) / z + PROJECTOR.0 as f32 / 2.0) as usize;
    let v = (FOCAL * y / z + PROJECTOR.1 as f32 / 2.0) as usize;
    let cx = (FOCAL * x / z + CAMERA.0 as f32 / 2.0) as usize;
    let cy = (FOCAL * y / z + CAMERA.1 as f32 / 2.0) as usize;
    (v * PROJECTOR.0 + u, cy * CAMERA.0 + cx)
}

fn options() -> ObjectOptions {
    ObjectOptions { threshold: 3.0, split: 2.0, min_area: 200, erode: 1, unmeasured: 4.0, rim: 25.0 }
}

#[test]
fn parts_are_separated_and_numbered_alike_in_every_projector() {
    let o = options();
    let scans: Vec<Map> = OFFSETS.iter().map(|ox| to_camera(*ox, true)).collect();
    let mut parts: Vec<_> = OFFSETS
        .iter()
        .zip(&scans)
        .map(|(ox, scan)| segment(&to_camera(*ox, false), scan, CAMERA, &o).unwrap())
        .collect();
    for p in &parts {
        assert_eq!(p.count, 2, "two parts per projector");
        assert_eq!(p.dropped.iter().filter(|d| **d).count(), 0, "everything measured, nothing dropped");
    }
    let refs: Vec<&Map> = scans.iter().collect();
    let found = number(&mut parts, &refs, CAMERA, o.erode).unwrap();
    assert_eq!(found.count, 2);
    assert!(found.camera_y[0] < found.camera_y[1], "top first: {:?}", found.camera_y);
    for (p, ox) in OFFSETS.iter().enumerate() {
        for b in 0..2 {
            let (pi, _) = centre(b, *ox);
            assert_eq!(parts[p].labels[pi], b as u16 + 1, "projector {p}, box {b}");
        }
        // The wall stays background.
        assert_eq!(parts[p].labels[5 * PROJECTOR.0 + 5], 0);
        // Each part about the size of its box's face as the projector sees it.
        let px = parts[p].pixels();
        for (b, (z, xs, ys)) in BOXES.iter().enumerate() {
            let area = (xs[1] - xs[0]) * (ys[1] - ys[0]) * (FOCAL / z).powi(2);
            let got = px[b] as f32;
            assert!((got / area - 1.0).abs() < 0.15, "projector {p} box {b}: {got} px, face {area}");
        }
    }
    let cam = camera_labels(&parts, &refs, CAMERA, o.erode).unwrap();
    for b in 0..2 {
        let (_, ci) = centre(b, 0.0);
        assert_eq!(cam[ci], b as u16 + 1, "camera, box {b}");
        let (z, xs, ys) = BOXES[b];
        let area = (xs[1] - xs[0]) * (ys[1] - ys[0]) * (FOCAL / z).powi(2);
        let got = cam.iter().filter(|l| **l == b as u16 + 1).count() as f32;
        // Smaller by the erosion: the edge pixels are half wall.
        assert!((got / area - 1.0).abs() < 0.25, "camera box {b}: {got} px, face {area}");
    }
    assert_eq!(cam[5 * CAMERA.0 + 5], 0);
}

#[test]
fn filled_in_pixels_at_the_rim_are_dropped_inside_kept() {
    let o = ObjectOptions { rim: 6.0, ..options() };
    let ox = OFFSETS[0];
    let clean = segment(&to_camera(ox, false), &to_camera(ox, true), CAMERA, &o).unwrap();
    // A band of columns only filled in, across the whole picture.
    let mut scan = to_camera(ox, true);
    let (pi, _) = centre(1, ox);
    let u0 = pi % PROJECTOR.0 - 6;
    for (i, px) in scan.rgba.iter_mut().enumerate() {
        if (u0..u0 + 13).contains(&(i % PROJECTOR.0)) && px[2] > 0.0 {
            px[2] = 0.5;
        }
    }
    let banded = segment(&to_camera(ox, false), &scan, CAMERA, &o).unwrap();
    let dropped = banded.dropped.iter().filter(|d| **d).count();
    assert!(dropped > 0);
    let outside: Vec<bool> = clean.labels.iter().map(|l| *l == 0).collect();
    let (to_edge, _) = raster::distance(PROJECTOR.0, PROJECTOR.1, &outside);
    let mut deep = 0;
    for i in 0..clean.labels.len() {
        let col = i % PROJECTOR.0;
        if clean.labels[i] > 0 && (u0 + 5..u0 + 8).contains(&col) {
            if to_edge[i] > o.rim + 2.0 {
                assert!(banded.labels[i] > 0, "deep inside a part, kept");
                deep += 1;
            }
            if to_edge[i] < o.rim - 1.0 {
                assert_eq!(banded.labels[i], 0, "at the rim, dropped");
            }
        }
    }
    assert!(deep > 0);
}

#[test]
fn blend_weights_add_up_to_one_at_every_camera_pixel() {
    let p2c: Vec<Map> = OFFSETS.iter().map(|ox| to_camera(*ox, true)).collect();
    let c2p: Vec<Map> = OFFSETS.iter().map(|ox| from_camera(*ox)).collect();
    let o = BlendOptions { ramp: 40.0, reach: 2.0, blur: 0.0 };
    let check = |inputs: &[BlendInput<'_>], label: &str| {
        let b = blend(inputs, &o).unwrap();
        let (mut n, mut err, mut bad) = (0usize, 0f32, 0usize);
        for c in 0..CAMERA.0 * CAMERA.1 {
            let mut sum = 0f32;
            let mut all = true;
            for (k, input) in inputs.iter().enumerate() {
                match input.from_camera.pixel_in(c, PROJECTOR.0, PROJECTOR.1) {
                    Some(p) if input.mask.is_none_or(|m| m[p] > 0) => sum += b.weights[k][p],
                    _ => all = false,
                }
            }
            if !all {
                continue;
            }
            n += 1;
            err += (sum - 1.0).abs();
            if (sum - 1.0).abs() > 0.1 {
                bad += 1;
            }
        }
        assert!(n > 2000, "{label}: {n} camera pixels lit by both");
        assert!(err / (n as f32) < 0.02, "{label}: mean error {}", err / n as f32);
        assert!(bad * 100 < n, "{label}: {bad} of {n} off by more than 0.1");
        b
    };
    let plain: Vec<BlendInput<'_>> =
        p2c.iter().zip(&c2p).map(|(t, f)| BlendInput { to_camera: t, from_camera: f, mask: None }).collect();
    let b = check(&plain, "no masks");
    assert!(b.shared.iter().all(|s| *s > 2000));
    // A projector alone counts fully: projector A's left edge is seen by A only.
    let (pi, _) = centre(0, OFFSETS[0]);
    assert!(b.weights[0][pi] > 0.0 && b.weights[0][pi] < 1.0);
    assert_eq!(b.weights[0][60 * PROJECTOR.0 + 3], 1.0);

    // With the object labels as masks.
    let o2 = options();
    let refs: Vec<&Map> = p2c.iter().collect();
    let mut parts: Vec<_> = OFFSETS
        .iter()
        .zip(&p2c)
        .map(|(ox, scan)| segment(&to_camera(*ox, false), scan, CAMERA, &o2).unwrap())
        .collect();
    number(&mut parts, &refs, CAMERA, o2.erode).unwrap();
    let masked: Vec<BlendInput<'_>> = p2c
        .iter()
        .zip(&c2p)
        .zip(&parts)
        .map(|((t, f), p)| BlendInput { to_camera: t, from_camera: f, mask: Some(&p.labels) })
        .collect();
    let b = check(&masked, "masks");
    assert_eq!(b.weights[0][5 * PROJECTOR.0 + 5], 0.0, "outside the mask");
}

#[test]
fn smoothing_takes_out_noise_and_stays_in_its_part() {
    let truth = to_camera(OFFSETS[0], true);
    let mut noisy = truth.clone();
    let mut seed = 7u64;
    for px in &mut noisy.rgba {
        if px[0] >= 0.0 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 2.0 / CAMERA.0 as f32;
            px[0] += r;
        }
    }
    let o = options();
    let parts = segment(&to_camera(OFFSETS[0], false), &truth, CAMERA, &o).unwrap();
    let out = smooth(&noisy, Some(&parts.labels), 2.0).unwrap();
    let (mut before, mut after, mut n) = (0f32, 0f32, 0usize);
    let inner =
        raster::erode(PROJECTOR.0, PROJECTOR.1, &parts.labels.iter().map(|l| *l > 0).collect::<Vec<_>>(), 6);
    for i in 0..out.len() {
        if parts.labels[i] == 0 {
            assert_eq!(out[i], noisy.rgba[i], "outside the parts unchanged");
        } else if inner[i] {
            before += (noisy.rgba[i][0] - truth.rgba[i][0]).abs();
            after += (out[i][0] - truth.rgba[i][0]).abs();
            n += 1;
        }
    }
    assert!(n > 500);
    assert!(after < before / 3.0, "mean error {} before, {} after", before / n as f32, after / n as f32);
    // Without parts: within the decoded area.
    let all = smooth(&noisy, None, 2.0).unwrap();
    assert_eq!(all.len(), noisy.len());
    assert!(smooth(&noisy, None, f32::NAN).is_err());
}

#[test]
fn hostile_sizes_are_refused() {
    assert!(Map::new(0, 10, vec![]).is_err());
    assert!(Map::new(1 << 13, 1 << 13, vec![]).is_err());
    assert!(Map::new(2, 2, vec![[0.0; 4]; 3]).is_err());
    let a = Map::new(2, 2, vec![[0.5, 0.5, 1.0, 1.0]; 4]).unwrap();
    let b = Map::new(3, 2, vec![[0.5, 0.5, 1.0, 1.0]; 6]).unwrap();
    assert!(segment(&a, &b, CAMERA, &options()).is_err());
    let nan = Map::new(2, 1, vec![[f32::NAN, 0.5, 1.0, 1.0], [2.0, 0.5, 1.0, 1.0]]).unwrap();
    assert_eq!(nan.uv(0), None);
    assert_eq!(nan.uv(1), None);
    assert!(blend(&[], &BlendOptions::default()).is_err());
    assert!(ObjectOptions { threshold: f32::NAN, ..options() }.check().is_err());
}

#[test]
fn distances_match_brute_force() {
    let (w, h) = (23, 17);
    let mut seed = 3u64;
    let src: Vec<bool> = (0..w * h)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 60) == 0
        })
        .collect();
    let (d, near) = raster::distance(w, h, &src);
    for i in 0..w * h {
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        let best = (0..w * h)
            .filter(|j| src[*j])
            .map(|j| ((j % w) as f32 - x).hypot((j / w) as f32 - y))
            .fold(f32::INFINITY, f32::min);
        assert!((d[i] - best).abs() < 1e-3, "pixel {i}: {} vs {best}", d[i]);
        let j = near[i].unwrap() as usize;
        assert!(src[j]);
        assert!((((j % w) as f32 - x).hypot((j / w) as f32 - y) - best).abs() < 1e-3);
    }
    let (d, near) = raster::distance(4, 4, &[false; 16]);
    assert!(d.iter().all(|v| v.is_infinite()) && near.iter().all(Option::is_none));
}

#[test]
fn morphology_holes_and_parts() {
    let (w, h) = (9, 7);
    // A ring with a hole, and a dot.
    let mut m = vec![false; w * h];
    for y in 1..6 {
        for x in 1..6 {
            m[y * w + x] = !(x == 3 && y == 3);
        }
    }
    m[3 * w + 8] = true;
    let filled = raster::fill_holes(w, h, &m);
    assert!(filled[3 * w + 3]);
    assert!(!filled[0]);
    let (labels, sizes) = raster::components(w, h, &filled);
    assert_eq!(sizes, vec![25, 1]);
    assert_eq!(labels[3 * w + 8], 2);
    let opened = raster::open(w, h, &filled, 1);
    assert!(!opened[3 * w + 8], "the dot goes");
    assert!(opened[3 * w + 3]);
    let eroded = raster::erode(w, h, &filled, 1);
    assert_eq!(eroded.iter().filter(|v| **v).count(), 9);
    let med = raster::median(3, 3, &[0., 0., 0., 0., 9., 0., 0., 0., 0.], 3);
    assert_eq!(med[4], 0.0);
}
