//! Sprites on WARP: a disc cut out of a square in the fragment stage is round at any size and
//! from any side, a soft sprite fades from its centre, and sprites that add their colour grow
//! brighter where they overlap.

use penumbra_core::Value;
use penumbra_gpu::Pixels;

use super::geo::{render, rig};
use super::pts::{coverage, drawn};
use super::{connect, create, gpu, set};

/// One sprite of `size` at the origin, seen by `/look` from `eye` at 128 x 128.
fn one(size: f64, sprite: &str, eye: [f64; 3], antialias: bool) -> Vec<penumbra_graph::Op> {
    vec![
        create("/one", "pts.generate", &[("rows", Value::Int(1)), ("cols", Value::Int(1))]),
        create("/sprite", "geo.from_pts", &[("size", Value::Float(size)), ("sprite", sprite.into())]),
        connect("/one", "/sprite", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprite", "/lit", 0),
        connect("/white", "/lit", 1),
        render(
            "/look",
            &[
                ("width", Value::Int(128)),
                ("height", Value::Int(128)),
                ("position", Value::Vec3(eye)),
                ("target", Value::Vec3([0.0; 3])),
                ("fov", Value::Float(40.0)),
                ("antialias", Value::Bool(antialias)),
            ],
        ),
        connect("/lit", "/look", 0),
    ]
}

/// The share of the box around the drawn pixels they fill, and how many there are.
fn fill(px: &Pixels) -> (f32, usize) {
    let (n, b) = coverage(px);
    (n as f32 / ((b[2] - b[0] + 1) * (b[3] - b[1] + 1)) as f32, n)
}

#[test]
fn disc_sprites_are_round_at_any_size_and_from_any_side() {
    let Some(gpu) = gpu() else { return };
    let mut rig = rig(gpu, None, &one(0.8, "disc", [0.0, -4.0, 0.0], false));
    // About 40 pixels across: a disc fills pi / 4 of its box, a square all of it.
    let px = drawn(&mut rig, "/look");
    let (share, n) = fill(&px);
    assert!(n > 800 && (0.74..0.83).contains(&share), "{n} pixels fill {share} of their box");
    let (_, b) = coverage(&px);
    let (w, h) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
    assert!(w.abs_diff(h) <= 1, "as wide as high: {w} x {h}");

    // From above and the side the disc still faces the camera, round.
    rig.edit(&[set("/look_camera.position", Value::Vec3([2.5, -2.5, 2.0]))]);
    let (share, n) = fill(&drawn(&mut rig, "/look"));
    assert!(n > 800 && (0.74..0.83).contains(&share), "{n} pixels fill {share} of their box");

    // Four pixels across: the corners stay empty, the middle is filled.
    rig.edit(&[set("/sprite.size", Value::Float(0.08))]);
    let px = drawn(&mut rig, "/look");
    let (n, b) = coverage(&px);
    let (w, h) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
    assert!((3..=6).contains(&w) && (3..=6).contains(&h), "{w} x {h}");
    assert!((n as u32) < w * h, "a disc leaves corners of its box: {n} of {w} x {h}");

    // A square at the same size fills its whole box.
    rig.edit(&[set("/sprite.sprite", "square".into()), set("/sprite.size", Value::Float(0.8))]);
    let (share, n) = fill(&drawn(&mut rig, "/look"));
    assert!(n > 1000 && share > 0.97, "{n} pixels fill {share} of their box");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn with_four_samples_a_disc_has_a_smooth_edge() {
    let Some(gpu) = gpu() else { return };
    let mut rig = rig(gpu, None, &one(0.8, "disc", [0.0, -4.0, 0.0], true));
    let px = drawn(&mut rig, "/look");
    let (share, n) = fill(&px);
    assert!(n > 800 && (0.74..0.83).contains(&share), "{n} pixels fill {share} of their box");
    // Pixels on the edge are partly covered: the samples inside the circle count.
    let part = px.data.iter().filter(|p| p[3] > 0.1 && p[3] < 0.9).count();
    assert!(part > 40, "{part} pixels on the edge are partly covered");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_soft_sprite_fades_from_its_centre() {
    let Some(gpu) = gpu() else { return };
    let mut rig = rig(gpu, None, &one(0.8, "disc", [0.0, -4.0, 0.0], false));
    let hard = drawn(&mut rig, "/look");
    let (_, b) = coverage(&hard);
    let (cx, cy) = ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2);
    let r = (b[2] - b[0]) / 2;
    rig.edit(&[set("/sprite.softness", Value::Float(1.0))]);
    let soft = drawn(&mut rig, "/look");
    let alpha = |x: u32, y: u32| soft.get(x, y).map_or(0.0, |p| p[3]);
    assert!(alpha(cx, cy) > 0.9, "the centre is whole: {}", alpha(cx, cy));
    let half = alpha(cx + r / 2, cy);
    assert!((0.3..0.7).contains(&half), "half way out it is half gone: {half}");
    let rim = alpha(cx + r * 9 / 10, cy);
    assert!(rim < 0.1, "at the rim it is nearly gone: {rim}");
    assert!(alpha(b[0], b[1]) == 0.0, "nothing past the disc");
    // Premultiplied: the colour fades with the alpha.
    let p = soft.get(cx + r / 2, cy).unwrap_or([0.0; 4]);
    assert!((p[0] - p[3]).abs() < 0.02, "{p:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn sprites_that_add_grow_brighter_where_they_overlap() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        // Two points 0.2 m apart, sprites 0.6 m wide: they overlap in the middle.
        create(
            "/two",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(2)), ("size", Value::Vec3([0.2, 1.0, 1.0]))],
        ),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.6)), ("sprite", "square".into())]),
        connect("/two", "/sprites", 0),
        create("/dim", "mat.constant", &[("color", Value::Vec4([0.3, 0.3, 0.3, 1.0]))]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/dim", "/lit", 1),
        render(
            "/look",
            &[
                ("width", Value::Int(96)),
                ("height", Value::Int(96)),
                ("position", Value::Vec3([0.0, -4.0, 0.0])),
                ("target", Value::Vec3([0.0; 3])),
                ("fov", Value::Float(40.0)),
                ("antialias", Value::Bool(false)),
            ],
        ),
        connect("/lit", "/look", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    let over = drawn(&mut rig, "/look");
    let mid = |px: &Pixels| px.get(48, 48).unwrap_or([0.0; 4]);
    assert!((mid(&over)[0] - 0.3).abs() < 0.02, "over: the nearer covers, {:?}", mid(&over));
    rig.edit(&[set("/sprites.blend", "add".into())]);
    let added = drawn(&mut rig, "/look");
    assert!((mid(&added)[0] - 0.6).abs() < 0.03, "add: both count, {:?}", mid(&added));
    // Where one sprite is alone it adds its own colour once.
    let (n, b) = coverage(&added);
    let side = added.get(b[0] + 2, 48).unwrap_or([0.0; 4]);
    assert!(n > 100 && (side[0] - 0.3).abs() < 0.03, "{side:?}");
    assert!((mid(&added)[3] - 1.0).abs() < 0.01, "alpha is laid over: {:?}", mid(&added));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}
