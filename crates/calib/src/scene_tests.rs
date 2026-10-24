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
        Some(Surface::Pumpkin) => [255, 90, 0],
    };
    let ppm = |name: &str, w: usize, h: usize, px: Vec<[u8; 3]>| {
        let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
        bytes.extend(px.iter().flatten());
        std::fs::write(dir.join(name), bytes).unwrap();
    };
    let room = match std::env::var("PENUMBRA_LANDSCAPE").as_deref() {
        Ok("pumpkin") => Room::pumpkin(2.0),
        Ok(_) => Room::landscape(),
        Err(_) => Room::default(),
    };
    let (cw, ch) = room.camera;
    ppm("camera_truth.ppm", cw, ch, room.camera_truth().iter().map(|t| colour(t.map(|t| t.0))).collect());
    let gamma = |v: f32| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
    for p in 0..2 {
        let photo = room.photo(p).iter().map(|c| [gamma(c[0]), gamma(c[1]), gamma(c[2])]).collect();
        ppm(&format!("photo{p}.ppm"), cw, ch, photo);
    }
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

#[test]
fn the_pumpkin_is_ray_cast_with_ribs_and_furrows() {
    use crate::synthetic::Pumpkin;
    let k = Pumpkin::on(0.0, 0.0, 0.0);
    let [cx, cy, cz] = k.centre;
    assert!(k.field(k.centre) < 0.0 && k.field([cx + 2.0, cy, cz]) > 0.0);
    // From the side at mid height, towards the centre: the crest of a rib at the radius, the
    // bottom of a furrow `furrow` deeper.
    let crest = k.turn + std::f64::consts::PI / f64::from(k.ribs);
    for (phi, want) in [(crest, k.radius), (k.turn, k.radius * (1.0 - k.furrow))] {
        let from = [cx + 5.0 * phi.cos(), cy, cz + 5.0 * phi.sin()];
        let d = [-phi.cos(), 0.0, -phi.sin()];
        let t = k.enter(from, d).unwrap();
        assert!((5.0 - t - want).abs() < 1e-3, "{phi}: {} vs {want}", 5.0 - t);
        // The normal points back along the ray.
        let n = k.normal([from[0] + t * d[0], cy, from[2] + t * d[2]]);
        assert!(n[0] * d[0] + n[2] * d[2] < -0.9, "{n:?}");
    }
    // From above: the top sunk round the stalk, below the half height.
    let t = k.enter([cx, cy + 5.0, cz], [0.0, -1.0, 0.0]).unwrap();
    let top = cy + 5.0 - t;
    assert!((top - (cy + k.half_height * (1.0 - k.dimple))).abs() < 1e-3, "{top}");
    // Missed: beside it, and away from it.
    assert!(k.enter([cx + 2.0, cy + 5.0, cz], [0.0, -1.0, 0.0]).is_none());
    assert!(k.enter([cx, cy, cz + 5.0], [0.0, 0.0, 1.0]).is_none());
    // In the room: the camera sees it, orange in the photo.
    let room = Room::pumpkin(1.0);
    let c = room.camera_point([cx + 3.2, 3.1 + k.half_height, 13.3 - 0.5]).unwrap();
    let (cw, _) = room.camera;
    let i = c[1] as usize * cw + c[0] as usize;
    assert_eq!(room.camera_truth()[i].map(|t| t.0), Some(Surface::Pumpkin));
    let px = room.photo(0)[i];
    assert!(px[0] > 2.0 * px[2] && px[0] > 0.1, "{px:?}");
}

#[test]
fn the_pumpkin_with_real_noise_is_one_object_and_no_face() {
    // Twice the size of the default room's pictures; noise 0.3 camera pixels per axis.
    let room = Room::pumpkin(2.0);
    for p in 0..2 {
        let (map, truth) = room.to_camera(p);
        let o = SceneOptions { min_object: (map.rgba.len() / 256).max(1), ..SceneOptions::default() };
        let s = analyse(&map, room.camera, &o).unwrap();
        // The plane fits' errors as a real decode's: 0.6 to 1 pixels median.
        let median = s.planes.iter().map(|pl| pl.error_px[0]).fold(0.0, f64::max);
        assert!(median > 0.3 && median < 1.0, "projector {p}: {median}");
        let pumpkin = truth.iter().filter(|t| matches!(t, Some((Surface::Pumpkin, _)))).count();
        let on_object = s
            .object_labels
            .iter()
            .zip(&truth)
            .filter(|(l, t)| **l > 0 && matches!(t, Some((Surface::Pumpkin, _))))
            .count();
        let on_planes = s
            .plane_labels
            .iter()
            .zip(&truth)
            .filter(|(l, t)| **l > 0 && matches!(t, Some((Surface::Pumpkin, _))))
            .count();
        eprintln!(
            "projector {p}: {} planes, {} objects, pumpkin {pumpkin} px, {on_object} as object, {on_planes} on planes",
            s.planes.len(),
            s.objects
        );
        assert_eq!(s.objects, 1, "projector {p}");
        assert!(on_object * 100 > pumpkin * 95, "projector {p}: {on_object} of {pumpkin}");
        assert!(on_planes * 100 < pumpkin * 2, "projector {p}: {on_planes} of {pumpkin}");
    }
}

/// Adds noise to projector pixels on a box's top: Gaussian of `sigma` camera pixels per axis
/// (sum of uniforms), as light thrown back by the walls makes a real decode noisier there.
fn noisy_top(
    room: &Room,
    map: &mut crate::objects::Map,
    truth: &[Option<(Surface, [f64; 3])>],
    b: usize,
    sigma: f64,
) {
    let (cw, ch) = (room.camera.0 as f64, room.camera.1 as f64);
    let mut state = 0x1234_5678_u64;
    let mut uniform = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    for (px, t) in map.rgba.iter_mut().zip(truth) {
        if let Some((Surface::Top, at)) = t
            && room.box_of(*at) == Some(b)
            && px[0] >= 0.0
        {
            // Twelve uniforms of variance 1/12 each: about Gaussian with variance 1.
            let mut g = || (0..12).map(|_| uniform()).sum::<f64>();
            px[0] = (f64::from(px[0]) + sigma * g() / cw).clamp(0.0, 1.0) as f32;
            px[1] = (f64::from(px[1]) + sigma * g() / ch).clamp(0.0, 1.0) as f32;
        }
    }
}

/// Share of a box top's measured pixels on a face and on an object.
fn top_shares(
    s: &crate::scene::SceneParts,
    map: &crate::objects::Map,
    truth: &[Option<(Surface, [f64; 3])>],
    room: &Room,
    b: usize,
) -> (f64, f64) {
    let (mut all, mut face, mut object) = (0usize, 0usize, 0usize);
    for (i, t) in truth.iter().enumerate() {
        if let Some((Surface::Top, at)) = t
            && room.box_of(*at) == Some(b)
            && map.measured(i)
        {
            all += 1;
            face += usize::from(s.plane_labels[i] > 0);
            object += usize::from(s.object_labels[i] > 0);
        }
    }
    (face as f64 / all.max(1) as f64, object as f64 / all.max(1) as f64)
}

#[test]
fn a_noisy_top_is_found_by_the_rough_pass() {
    let room = Room::pumpkin(2.0);
    let (mut map, truth) = room.to_camera(0);
    // The pumpkin's crate: noise ten times a clean face's (0.3 px per axis); without the
    // rough pass the top is an object with the pumpkin (measured: 3 % on a face).
    noisy_top(&room, &mut map, &truth, 1, 3.0);
    let o = SceneOptions { min_object: (map.rgba.len() / 256).max(1), ..SceneOptions::default() };
    let s = analyse(&map, room.camera, &o).unwrap();
    let (face, object) = top_shares(&s, &map, &truth, &room, 1);
    eprintln!("top: {face:.2} on a face, {object:.2} on an object; {} objects", s.objects);
    assert!(face > 0.8 && object < 0.05, "{face} {object}");
    assert_eq!(s.objects, 1);
}

#[test]
fn a_face_in_one_projector_beats_an_object_in_the_other() {
    let room = Room::pumpkin(2.0);
    let o = |m: &crate::objects::Map| SceneOptions {
        min_object: (m.rgba.len() / 256).max(1),
        ..SceneOptions::default()
    };
    let (mut map0, truth0) = room.to_camera(0);
    let (map1, _) = room.to_camera(1);
    // The crate on its end: projector 0 decodes its top far too noisy for any fit.
    noisy_top(&room, &mut map0, &truth0, 4, 6.0);
    let mut scenes = vec![
        analyse(&map0, room.camera, &o(&map0)).unwrap(),
        analyse(&map1, room.camera, &o(&map1)).unwrap(),
    ];
    let before = top_shares(&scenes[0], &map0, &truth0, &room, 4);
    let objects = scenes[0].objects;
    let dropped = crate::scene::drop_objects_on_faces(&mut scenes, &[&map0, &map1], room.camera);
    let after = top_shares(&scenes[0], &map0, &truth0, &room, 4);
    eprintln!(
        "before {before:?} ({objects} objects), after {after:?} ({} objects), dropped {dropped:?}",
        scenes[0].objects
    );
    // Part of the top is an object in projector 0 (the rest unexplained or rough faces).
    assert!(before.1 > 0.1, "{before:?}");
    assert!(after.1 < 0.02, "{after:?}");
    assert!(dropped[0] >= 1 && dropped[1] == 0, "{dropped:?}");
    assert_eq!(dropped[0], objects - scenes[0].objects);
    assert!(scenes[0].objects >= 1, "the pumpkin stays");
    let labels = &scenes[0].object_labels;
    assert_eq!(labels.iter().copied().max(), Some(scenes[0].objects as u16));
}

#[test]
fn a_sagging_top_given_by_its_corners_is_claimed_whole() {
    let room = Room::pumpkin(2.0);
    let (mut map, truth) = room.to_camera(0);
    // The pumpkin's crate top sags 8 camera pixels in its middle (a lid under a load, seen at a
    // grazing angle), with a real decode's noise on top: no plane holds it within the tolerance.
    let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .map(|(u, v)| room.camera_point(room.top_point(1, u, v)).unwrap());
    let to_face = Homography::square_to_quad(corners).unwrap().inverse().unwrap();
    let (cw, ch) = (room.camera.0 as f64, room.camera.1 as f64);
    let mut state = 0x9e37_79b9_u64;
    let mut uniform = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    for (px, t) in map.rgba.iter_mut().zip(&truth) {
        if let Some((Surface::Top, at)) = t
            && room.box_of(*at) == Some(1)
            && px[0] >= 0.0
        {
            let q = [f64::from(px[0]) * cw, f64::from(px[1]) * ch];
            let [u, v] = to_face.apply(q).unwrap();
            let sag = 8.0 * 16.0 * u * (1.0 - u) * v * (1.0 - v);
            let mut g = || (0..12).map(|_| uniform()).sum::<f64>();
            px[0] = ((q[0] + g()) / cw).clamp(0.0, 1.0) as f32;
            px[1] = ((q[1] + sag + g()) / ch).clamp(0.0, 1.0) as f32;
        }
    }
    let o = SceneOptions { min_object: (map.rgba.len() / 256).max(1), ..SceneOptions::default() };
    let mut s = analyse(&map, room.camera, &o).unwrap();
    let (face_before, object_before) = top_shares(&s, &map, &truth, &room, 1);
    let claim =
        crate::scene::claim_face(&mut s, &map, room.camera, &to_face, f64::from(o.tolerance), 0.01, None)
            .unwrap();
    let label = claim.plane as u16 + 1;
    let (mut top, mut on, mut pumpkin, mut pumpkin_on) = (0usize, 0usize, 0usize, 0usize);
    for (i, t) in truth.iter().enumerate() {
        if !map.measured(i) {
            continue;
        }
        match t {
            Some((Surface::Top, at)) if room.box_of(*at) == Some(1) => {
                top += 1;
                on += usize::from(s.plane_labels[i] == label);
            }
            Some((Surface::Pumpkin, _)) => {
                pumpkin += 1;
                pumpkin_on += usize::from(s.plane_labels[i] == label);
            }
            _ => {}
        }
    }
    let (on, pumpkin_on) = (on as f64 / top as f64, pumpkin_on as f64 / pumpkin as f64);
    eprintln!(
        "before: {face_before:.2} of the top on a face, {object_before:.2} on an object; claimed {on:.2} of it, {pumpkin_on:.3} of the pumpkin"
    );
    assert!(on > 0.9, "{on}");
    assert!(pumpkin_on < 0.02, "{pumpkin_on}");
    // The map with the bend lands content where the camera sees the top.
    let h = s.planes[claim.plane].homography;
    let m = crate::scene::face_map(&map, room.camera, &h, &to_face, f64::from(o.tolerance), 0.01);
    eprintln!("face map: {:?} {:?} corrected {}", m.error_px, m.bend_px, m.corrected());
    assert!(m.corrected());
    assert!(m.error_px[1] < 0.7 * m.error_px[0] && m.error_px[1] < 1.5, "{:?}", m.error_px);
}

#[test]
fn a_top_one_projector_merges_with_the_pumpkin_is_carved_out_by_the_other() {
    let room = Room::pumpkin(2.0);
    let least = |m: &crate::objects::Map| (m.rgba.len() / 256).max(1);
    let o = |m: &crate::objects::Map| SceneOptions { min_object: least(m), ..SceneOptions::default() };
    let (mut map0, truth0) = room.to_camera(0);
    let (map1, _) = room.to_camera(1);
    // The pumpkin's crate: projector 0 decodes its top far too noisy for any fit, so the top
    // joins the pumpkin into one object there; projector 1 finds the top.
    noisy_top(&room, &mut map0, &truth0, 1, 12.0);
    let mut scenes = vec![
        analyse(&map0, room.camera, &o(&map0)).unwrap(),
        analyse(&map1, room.camera, &o(&map1)).unwrap(),
    ];
    let pumpkin_share = |s: &crate::scene::SceneParts| {
        let (mut all, mut on) = (0usize, 0usize);
        for (i, t) in truth0.iter().enumerate() {
            if matches!(t, Some((Surface::Pumpkin, _))) && map0.measured(i) {
                all += 1;
                on += usize::from(s.object_labels[i] > 0);
            }
        }
        on as f64 / all.max(1) as f64
    };
    let before = (top_shares(&scenes[0], &map0, &truth0, &room, 1), pumpkin_share(&scenes[0]));
    // The whole object lies mostly on the pumpkin, which projector 1 sees as an object too: not
    // dropped whole.
    let carved = crate::scene::carve_disputed_objects(
        &mut scenes,
        &[&map0, &map1],
        room.camera,
        &[least(&map0), least(&map1)],
    );
    let after = (top_shares(&scenes[0], &map0, &truth0, &room, 1), pumpkin_share(&scenes[0]));
    eprintln!("before {before:?}, after {after:?}, carved {carved:?}, {} objects", scenes[0].objects);
    assert!(before.0.1 > 0.3, "the top is part of the object: {before:?}");
    assert!(after.0.1 < 0.05, "the top is carved out: {after:?}");
    assert!(after.1 > 0.9 * before.1, "the pumpkin stays: {before:?} {after:?}");
    assert!(carved[0] > 0);
    assert_eq!(scenes[0].object_labels.iter().copied().max(), Some(scenes[0].objects as u16));
}

#[test]
fn a_long_strip_bent_across_by_noise_stays_a_face() {
    // A crate's lid seen at a grazing angle: in the projector a strip 600 pixels long and 30
    // high, its decode noisy (0.8 camera pixels per axis) and bent across by 1.5 pixels, as the
    // stripes' noise bends it. A bowl fitted across 30 pixels bows far more than a pumpkin's.
    let (w, h) = (640usize, 360usize);
    let mut state = 0x9e37_79b9_u64;
    let mut uniform = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let mut rgba = vec![[-1.0f32, -1.0, 0.0, 1.0]; w * h];
    for y in 100..130 {
        let across = (y as f64 + 0.5 - 115.0) / 15.0;
        for x in 20..620 {
            let mut g = || (0..12).map(|_| uniform()).sum::<f64>() * 0.8;
            let (cx, cy) = (
                0.9 * (x as f64 + 0.5) + 20.0 + g(),
                0.9 * (y as f64 + 0.5) + 10.0 + 1.5 * across * across + g(),
            );
            rgba[y * w + x] = [(cx / w as f64) as f32, (cy / h as f64) as f32, 1.0, 1.0];
        }
    }
    let map = crate::objects::Map::new(w, h, rgba).unwrap();
    let s = analyse(&map, (w, h), &options()).unwrap();
    let on_face = s.plane_labels.iter().filter(|l| **l > 0).count();
    eprintln!(
        "{} planes, bend {:?}, {} objects, {on_face} px on faces",
        s.planes.len(),
        s.planes.first().map(|p| p.bend),
        s.objects
    );
    assert_eq!(s.objects, 0);
    assert!(on_face > 600 * 30 * 9 / 10, "{on_face}");
}
