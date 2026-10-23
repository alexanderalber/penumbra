//! Planes, objects, height and outline against the ray-cast room of [`crate::synthetic`].

use crate::scene::{SceneOptions, analyse, outline, relief};
use crate::synthetic::{BOX, Room, Surface};
use crate::{Homography, raster};

fn options() -> SceneOptions {
    SceneOptions { min_object: 200, ..SceneOptions::default() }
}

/// Per label 1..=count the surface most of its pixels light, and that share.
fn purity(labels: &[u16], count: usize, truth: &[Option<(Surface, [f64; 3])>]) -> Vec<(Surface, f64, usize)> {
    let kinds = [Surface::Floor, Surface::Wall, Surface::Top, Surface::Front, Surface::Side, Surface::Ball];
    (1..=count)
        .map(|k| {
            let mut tally = [0usize; 6];
            let mut all = 0usize;
            for (l, t) in labels.iter().zip(truth) {
                if usize::from(*l) == k
                    && let Some((s, _)) = t
                {
                    all += 1;
                    tally[kinds.iter().position(|x| x == s).unwrap()] += 1;
                }
            }
            let (best, n) = tally.iter().enumerate().max_by_key(|(_, n)| **n).unwrap();
            (kinds[best], *n as f64 / all.max(1) as f64, all)
        })
        .collect()
}

#[test]
fn planes_and_the_ball_from_one_scan() {
    let room = Room::default();
    for p in 0..2 {
        let (map, truth) = room.to_camera(p);
        let s = analyse(&map, room.camera, &options()).unwrap();
        let planes = purity(&s.plane_labels, s.planes.len(), &truth);
        eprintln!("projector {p}: {:?} dropped {} objects {}", planes, s.dropped_planes, s.objects);
        for (k, pl) in s.planes.iter().enumerate() {
            eprintln!("  plane {k}: {} px, error {:?}, bend {:?}", pl.pixels, pl.error_px, pl.bend);
        }
        // Floor, wall, the box's top and front, each its own plane and nearly pure.
        for want in [Surface::Floor, Surface::Wall, Surface::Top, Surface::Front] {
            let hits: Vec<_> = planes.iter().filter(|(s, share, _)| *s == want && *share > 0.97).collect();
            assert_eq!(hits.len(), 1, "projector {p}: {want:?} in {planes:?}");
        }
        assert!(planes.iter().all(|(s, _, _)| *s != Surface::Ball), "{planes:?}");
        // The ball is the one object, nearly all of it.
        assert_eq!(s.objects, 1, "projector {p}");
        let ball = truth.iter().filter(|t| matches!(t, Some((Surface::Ball, _)))).count();
        let found = s
            .object_labels
            .iter()
            .zip(&truth)
            .filter(|(l, t)| **l > 0 && matches!(t, Some((Surface::Ball, _))))
            .count();
        let off = s
            .object_labels
            .iter()
            .zip(&truth)
            .filter(|(l, t)| **l > 0 && !matches!(t, Some((Surface::Ball, _))))
            .count();
        assert!(found as f64 > 0.9 * ball as f64, "projector {p}: {found} of {ball} ball pixels");
        let unlit = s.object_labels.iter().zip(&truth).filter(|(l, t)| **l > 0 && t.is_none()).count();
        assert!(
            ((off - unlit) as f64) < 0.01 * found as f64,
            "projector {p}: {off} object pixels off the ball, {unlit} unlit"
        );
        assert!((unlit as f64) < 0.05 * found as f64, "projector {p}: {unlit} object pixels unlit");
    }
}

#[test]
fn the_ball_rises_from_the_top() {
    let room = Room::default();
    for p in 0..2 {
        let (map, truth) = room.to_camera(p);
        let s = analyse(&map, room.camera, &options()).unwrap();
        let planes = purity(&s.plane_labels, s.planes.len(), &truth);
        let top = planes.iter().position(|(s, _, _)| *s == Surface::Top).unwrap();
        let r = relief(
            &map,
            room.camera,
            &s.planes[top].homography,
            &s.object_labels,
            &s.plane_labels,
            top as u16 + 1,
            2,
        )
        .unwrap();
        assert_eq!(r.heuristic.name(), "planes below", "{:?}", r.heuristic);
        let (mut h, mut t) = (Vec::new(), Vec::new());
        for ((m, v), tr) in r.mask.iter().zip(&r.heights).zip(&truth) {
            if *m && let Some((Surface::Ball, pt)) = tr {
                h.push(f64::from(*v));
                t.push(pt[1] - BOX[1][1]);
            }
        }
        let c = correlation(&h, &t);
        eprintln!("projector {p}: correlation {c}, {} pixels", h.len());
        assert!(c > 0.97, "projector {p}: {c}");
    }
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma).powi(2);
        bb += (y - mb).powi(2);
    }
    ab / (aa * bb).sqrt()
}

/// The top's outline in the camera picture from the camera's own view of the room.
fn outline_of(room: &Room) -> ([[f64; 2]; 4], [[f64; 2]; 4]) {
    let (cw, ch) = room.camera;
    let truth = room.camera_truth();
    let top: Vec<bool> = truth.iter().map(|t| matches!(t, Some((Surface::Top, _)))).collect();
    let ball: Vec<bool> = truth.iter().map(|t| matches!(t, Some((Surface::Ball, _)))).collect();
    let hidden = raster::dilate(cw, ch, &ball, 4);
    let o = outline(cw, ch, &top, &hidden).unwrap();
    let want = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .map(|(u, v)| room.camera_point(room.top_point(0, u, v)).unwrap());
    (o.corners, want)
}

#[test]
fn the_tops_outline_runs_on_under_the_ball() {
    let room = Room::default();
    let (got, want) = outline_of(&room);
    for (g, w) in got.iter().zip(&want) {
        assert!((g[0] - w[0]).hypot(g[1] - w[1]) < 1.5, "{got:?} vs {want:?}");
    }
    // The rectifying homography puts a point of the top where it belongs, within the half
    // pixel the border's pixels leave open (the top is only 31 camera pixels deep here).
    let to_top = Homography::square_to_quad(got).unwrap().inverse().unwrap();
    let c = room.camera_point(room.top_point(0, 0.3, 0.7)).unwrap();
    let uv = to_top.apply(c).unwrap();
    assert!((uv[0] - 0.3).abs() < 0.01 && (uv[1] - 0.7).abs() < 0.02, "{uv:?}");

    // A ball on the far right corner hides it; the corner is still found.
    let room = Room { ball: [2.4, 4.6, 14.4, 1.6], ..Room::default() };
    let (got, want) = outline_of(&room);
    for (g, w) in got.iter().zip(&want) {
        assert!((g[0] - w[0]).hypot(g[1] - w[1]) < 2.0, "{got:?} vs {want:?}");
    }
}

#[test]
fn bad_input_is_refused() {
    let room = Room::default();
    let (map, _) = room.to_camera(0);
    for o in [
        SceneOptions { tolerance: 0.0, ..options() },
        SceneOptions { min_plane: 1.0, ..options() },
        SceneOptions { max_planes: 0, ..options() },
    ] {
        assert!(analyse(&map, room.camera, &o).is_err(), "{o:?}");
    }
    assert!(analyse(&map, (0, 10), &options()).is_err());
    let (w, h) = (20, 10);
    assert!(outline(w, h, &[false; 200], &[false; 200]).is_err());
    assert!(outline(w, h, &[true; 10], &[false; 200]).is_err());
    // A disc has no four straight edges.
    let disc: Vec<bool> =
        (0..200).map(|i| ((i % w) as f64 - 10.0).hypot((i / w) as f64 - 5.0) < 4.0).collect();
    assert!(outline(w, h, &disc, &[false; 200]).is_err());
}

/// Pictures of the room and of what the analysis found, for a look: `cargo test -p
/// penumbra-calib dump_pictures -- --ignored`, PPM files in `PENUMBRA_DUMP` (default the temp
/// folder).
#[test]
#[ignore]
fn dump_pictures() {
    let dir = std::path::PathBuf::from(
        std::env::var("PENUMBRA_DUMP").unwrap_or_else(|_| std::env::temp_dir().display().to_string()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let colour = |s: Option<Surface>| match s {
        None => [0u8, 0, 0],
        Some(Surface::Floor) => [90, 70, 50],
        Some(Surface::Wall) => [120, 120, 140],
        Some(Surface::Top) => [200, 160, 60],
        Some(Surface::Front) => [140, 90, 30],
        Some(Surface::Side) => [100, 60, 20],
        Some(Surface::Ball) => [255, 120, 0],
    };
    let ppm = |name: &str, w: usize, h: usize, px: Vec<[u8; 3]>| {
        let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
        bytes.extend(px.iter().flatten());
        std::fs::write(dir.join(name), bytes).unwrap();
    };
    let room = if std::env::var("PENUMBRA_LANDSCAPE").is_ok() { Room::landscape() } else { Room::default() };
    let (cw, ch) = room.camera;
    ppm("camera_truth.ppm", cw, ch, room.camera_truth().iter().map(|t| colour(t.map(|t| t.0))).collect());
    for p in 0..2 {
        let (map, truth) = room.to_camera(p);
        let (pw, ph) = room.projector;
        ppm(&format!("proj{p}_truth.ppm"), pw, ph, truth.iter().map(|t| colour(t.map(|t| t.0))).collect());
        let s = analyse(&map, room.camera, &options()).unwrap();
        let px = s
            .plane_labels
            .iter()
            .zip(&s.object_labels)
            .map(|(l, o)| {
                if *o > 0 {
                    [255, 255, 255]
                } else if *l > 0 {
                    let h = f32::from(*l) * 0.618;
                    let f = |k: f32| ((((h + k) % 1.0) * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
                    [(f(0.0) * 200.0) as u8, (f(2.0 / 3.0) * 200.0) as u8, (f(1.0 / 3.0) * 200.0) as u8]
                } else {
                    [0, 0, 0]
                }
            })
            .collect();
        ppm(&format!("proj{p}_found.ppm"), pw, ph, px);
    }
}

#[test]
fn coplanar_faces_of_different_crates_are_faces_of_their_own() {
    let room = Room::landscape();
    for p in 0..2 {
        let (map, truth) = room.to_camera(p);
        // Bigger than the sliver of a side seen through the gap between two crates.
        let o = SceneOptions { min_object: 1500, ..options() };
        let s = analyse(&map, room.camera, &o).unwrap();
        // Per face: its surface, box and share.
        let faces: Vec<(Surface, Option<usize>, f64, usize)> = (1..=s.planes.len())
            .map(|k| {
                let mut tally: std::collections::HashMap<(Surface, Option<usize>), usize> =
                    Default::default();
                let mut all = 0;
                for (l, t) in s.plane_labels.iter().zip(&truth) {
                    if usize::from(*l) == k
                        && let Some((sf, pt)) = t
                    {
                        let b = matches!(sf, Surface::Top | Surface::Front | Surface::Side)
                            .then(|| room.box_of(*pt))
                            .flatten();
                        *tally.entry((*sf, b)).or_default() += 1;
                        all += 1;
                    }
                }
                let ((sf, b), n) = tally.into_iter().max_by_key(|(_, n)| *n).unwrap();
                (sf, b, n as f64 / all as f64, all)
            })
            .collect();
        eprintln!("projector {p}: {faces:?}, dropped {}", s.dropped_planes);
        for want in [
            (Surface::Top, Some(0)),
            (Surface::Top, Some(1)),
            (Surface::Top, Some(2)),
            (Surface::Front, Some(0)),
            (Surface::Front, Some(1)),
            (Surface::Front, Some(2)),
        ] {
            let n = faces.iter().filter(|f| (f.0, f.1) == want && f.2 > 0.9).count();
            assert_eq!(n, 1, "projector {p}: {want:?} in {faces:?}");
        }
        assert!(faces.iter().all(|f| f.0 != Surface::Ball), "{faces:?}");
        // The two coplanar tops came from one fit.
        let group = |b: usize| {
            let k = faces.iter().position(|f| (f.0, f.1) == (Surface::Top, Some(b))).unwrap();
            s.planes[k].group
        };
        assert_eq!(group(0), group(1));
        assert_eq!(s.objects, 1, "projector {p}");
    }
}
