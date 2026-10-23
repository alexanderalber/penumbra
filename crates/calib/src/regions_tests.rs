//! Regions against the ray-cast room of [`crate::synthetic`]: the crate's front cut into a rib
//! strip and two halves of its panel, the floor into two faces, ragged edges.

use crate::regions::{Kind, ShapeOptions, classify, face_at, face_centres, fit_faces, shape, suggestions};
use crate::synthetic::{BOX, Room, Surface};

/// Face labels of the default room as a face search might leave them: 1 wall, 2 the top (the
/// support), 3 the front's rib strip along its top edge, 4 and 5 the left and right half of the
/// front's panel, 6 the side, 7 and 8 the floor left and right of the crate's middle. A seam of
/// unlabelled pixels runs between the strip and the panel, and the front's edge is ragged.
fn labels(room: &Room) -> (Vec<u16>, Vec<bool>) {
    let truth = room.camera_truth();
    let (w, _) = room.camera;
    let height = BOX[1][1] - BOX[1][0];
    let mut planes = vec![0u16; truth.len()];
    let mut objects = vec![false; truth.len()];
    for (i, t) in truth.iter().enumerate() {
        let Some((s, p)) = t else { continue };
        planes[i] = match s {
            Surface::Wall => 1,
            Surface::Top => 2,
            Surface::Front => {
                let v = (BOX[1][1] - p[1]) / height;
                let u = (p[0] - BOX[0][0]) / (BOX[0][1] - BOX[0][0]);
                if v < 0.12 {
                    3
                } else if v < 0.16 {
                    0
                } else if u < 0.5 {
                    4
                } else {
                    5
                }
            }
            Surface::Side => 6,
            Surface::Floor if p[0] < 0.0 => 7,
            Surface::Floor => 8,
            _ => {
                objects[i] = true;
                0
            }
        };
    }
    // A ragged edge: front pixels next to another surface dropped in a pseudo-random pattern
    // up to 3 pixels deep.
    let front: Vec<bool> = planes.iter().map(|l| (3..=5).contains(l)).collect();
    let mut seed = 7u64;
    for i in 0..planes.len() {
        if !front[i] {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let near_other = (1..=3usize).any(|d| {
            [(x.wrapping_sub(d), y), (x + d, y), (x, y.wrapping_sub(d)), (x, y + d)].iter().any(|(xx, yy)| {
                *xx < w && *yy < room.camera.1 && !front[yy * w + xx] && planes[yy * w + xx] != 0
            })
        });
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if near_other && (seed >> 33).is_multiple_of(2) {
            planes[i] = 0;
        }
    }
    (planes, objects)
}

#[test]
fn faces_are_classed_floor_level_and_upright() {
    let room = Room::default();
    let (planes, _) = labels(&room);
    let (w, h) = room.camera;
    let maps: Vec<_> = (0..2).map(|p| room.from_camera(p)).collect();
    let refs: Vec<_> = maps.iter().collect();
    let fits = fit_faces(room.camera, &planes, 8, &refs, &[room.projector, room.projector]);
    let camera_y: Vec<f64> = (1..=8u16)
        .map(|k| {
            let ys: Vec<f64> =
                planes.iter().enumerate().filter(|(_, l)| **l == k).map(|(i, _)| (i / w) as f64).collect();
            ys.iter().sum::<f64>() / ys.len().max(1) as f64
        })
        .collect();
    let (kinds, heights, floor) = classify((w, h), &fits, Some(2), &camera_y);
    eprintln!("{kinds:?} {heights:?} floor {floor:?}");
    assert!(floor == Some(7) || floor == Some(8), "{floor:?}");
    assert_eq!(kinds[6], Kind::Floor);
    assert_eq!(kinds[7], Kind::Floor);
    assert_eq!(kinds[1], Kind::Level);
    // The side (6) faces away from the camera.
    for k in [0, 2, 3, 4] {
        assert_eq!(kinds[k], Kind::Upright, "face {}", k + 1);
    }
    let top = heights[1].unwrap();
    assert!((top - 1.0).abs() < 0.02, "{top}");
}

#[test]
fn a_crate_front_of_strips_and_halves_becomes_one_straight_region() {
    let room = Room::default();
    let (planes, objects) = labels(&room);
    let (w, h) = room.camera;
    // Region 1 the front (strip and both halves), 2 the top, 3 the wall; side and floor
    // disabled (0).
    let region_of = [0u16, 3, 2, 1, 1, 1, 0, 0, 0];
    let base: Vec<u16> = planes.iter().map(|l| region_of[usize::from(*l)]).collect();
    let free: Vec<bool> = planes.iter().zip(&objects).map(|(l, o)| *l == 0 && !o).collect();
    let s = shape(w, h, &base, &free, &objects, 3, &ShapeOptions::default());
    // The front is one piece now, the seam and the ragged edge filled.
    let truth = room.camera_truth();
    let front: Vec<bool> = truth.iter().map(|t| matches!(t, Some((Surface::Front, _)))).collect();
    let region: Vec<bool> = s.labels.iter().map(|l| *l == 1).collect();
    let both = front.iter().zip(&region).filter(|(a, b)| **a && **b).count();
    let either = front.iter().zip(&region).filter(|(a, b)| **a || **b).count();
    let iou = both as f64 / either as f64;
    assert!(iou > 0.97, "front against its region: {iou}");
    let (parts, _) = crate::raster::components(w, h, &region);
    assert_eq!(parts.iter().copied().max(), Some(1), "one piece");
    // Its corners are the front's corners.
    let corners = s.straight[0].expect("the front is straight");
    for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
        let want = room.camera_point(room.front_point(0, u, v)).unwrap();
        let d = corners.iter().map(|c| (c[0] - want[0]).hypot(c[1] - want[1])).fold(f64::INFINITY, f64::min);
        assert!(d < 2.0, "corner {u} {v}: {d} px off ({corners:?})");
    }
    // Nothing on the disabled floor and side, nothing on the ball.
    for (i, l) in s.labels.iter().enumerate() {
        if *l > 0 {
            assert!(!objects[i], "pixel {i} on the ball");
            assert!(!matches!(planes[i], 6..=8), "pixel {i} on a disabled face");
        }
    }
}

#[test]
fn the_rib_strip_is_suggested_for_its_panel() {
    let room = Room::default();
    let (planes, objects) = labels(&room);
    let (w, _) = room.camera;
    let maps: Vec<_> = (0..2).map(|p| room.from_camera(p)).collect();
    let refs: Vec<_> = maps.iter().collect();
    let fits = fit_faces(room.camera, &planes, 8, &refs, &[room.projector, room.projector]);
    let kinds = vec![Kind::Unknown; 8];
    let s = suggestions(w, room.camera.1, &planes, &objects, &fits, &kinds);
    eprintln!("{s:?}");
    let strip = s.iter().find(|x| x.strip == 3).expect("the strip");
    assert!(strip.panel == 4 || strip.panel == 5, "{strip:?}");
    assert!(strip.error_px < 1.5, "{strip:?}");
    // The wall and the top are no strips of anything.
    assert!(s.iter().all(|x| x.strip != 1 && x.strip != 2), "{s:?}");
}

#[test]
fn faces_are_found_by_pixel_and_have_centres() {
    let planes = vec![0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0];
    assert_eq!(face_at(4, 4, &planes, [2.5, 2.5], 0), Some(1));
    assert_eq!(face_at(4, 4, &planes, [0.5, 0.5], 0), None);
    assert_eq!(face_at(4, 4, &planes, [0.5, 0.5], 3), Some(1));
    assert_eq!(face_at(4, 4, &planes, [f64::NAN, 0.5], 2), None);
    assert_eq!(face_at(4, 4, &planes, [9.0, 0.5], 2), None);
    let c = face_centres(4, 4, &planes, 1);
    assert!(matches!(c[0], Some([1..=2, 1..=2])), "{c:?}");
}
