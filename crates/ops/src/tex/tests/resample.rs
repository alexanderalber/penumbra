//! `sampling` on `tex.resolution` on WARP: nearest copies pixels, linear averages the whole
//! footprint, cubic keeps a ramp a ramp.

use penumbra_core::Value;
use penumbra_graph::Op;

use super::{Rig, connect, create, gpu, scratch};

/// Red is the column index: 0, 1, 2, ...
const INDEX: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    return vec4f(floor(v.uv.x * pn.resolution.x), 0.0, 0.0, 1.0); }";

/// Stripes one column wide, every third column lit: the mean is 1/3.
const STRIPES: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let x = u32(floor(v.uv.x * pn.resolution.x)); \
    return vec4f(select(0.0, 1.0, x % 3u == 0u), 0.0, 0.0, 1.0); }";

fn source(shader: &str, width: i64) -> Op {
    create(
        "/src",
        "tex.shader",
        &[
            ("file", shader.into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(width)),
            ("height", Value::Int(4)),
        ],
    )
}

/// The red of row 1 of `/src` scaled to `width` x 4 by `sampling`.
fn scaled(name: &str, shader: &str, from: i64, width: i64, sampling: &str) -> Option<Vec<f32>> {
    let gpu = gpu()?;
    let dir = scratch(name);
    std::fs::write(dir.join("index.wgsl"), INDEX).unwrap();
    std::fs::write(dir.join("stripes.wgsl"), STRIPES).unwrap();
    let ops = [
        source(shader, from),
        create(
            "/res",
            "tex.resolution",
            &[
                ("mode", "custom".into()),
                ("width", Value::Int(width)),
                ("height", Value::Int(4)),
                ("sampling", sampling.into()),
            ],
        ),
        connect("/src", "/res", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let px = rig.pixels("/res");
    Some((0..px.width).map(|x| px.get(x, 1).unwrap()[0]).collect())
}

fn close(got: &[f32], want: &[f32], tolerance: f32) -> bool {
    got.len() == want.len() && got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tolerance)
}

#[test]
fn threshold_and_composite_keep_exact_pixels_at_the_same_resolution() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("exact_pixels");
    std::fs::write(
        dir.join("levels.wgsl"),
        "@fragment fn main(v: PnVertex) -> @location(0) vec4f {
            let x = u32(v.pos.x) % 3u;
            let c = select(select(0.75, 0.5, x == 0u), 0.25, x == 1u);
            return vec4f(vec3f(c), 1.0);
        }",
    )
    .unwrap();
    let ops = [
        source("levels.wgsl", 96),
        create("/cut", "tex.threshold", &[("output", "alpha".into())]),
        create(
            "/white",
            "tex.constant",
            &[("width", Value::Int(96)), ("height", Value::Int(4)), ("color", Value::Vec4([1.0; 4]))],
        ),
        create("/outside", "tex.composite", &[("op", "outside".into())]),
        connect("/src", "/cut", 0),
        connect("/cut", "/outside", 0),
        connect("/white", "/outside", 1),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let cut = rig.pixels("/cut");
    let outside = rig.pixels("/outside");
    assert_eq!((cut.width, cut.height), (96, 4));
    assert_eq!((outside.width, outside.height), (96, 4));
    for (i, (cut, outside)) in cut.data.iter().zip(&outside.data).enumerate() {
        let mask = if i % 96 % 3 == 1 { 0.0 } else { 1.0 };
        assert_eq!(cut[3], mask, "threshold pixel {i}: {cut:?}");
        assert_eq!(*outside, [1.0 - mask; 4], "outside pixel {i}");
    }
}

#[test]
fn nearest_repeats_each_pixel() {
    let Some(row) = scaled("nearest", "index.wgsl", 4, 8, "nearest") else { return };
    assert_eq!(row, [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
}

#[test]
fn linear_shrinking_is_the_mean_of_the_footprint() {
    // 4 to 1: columns 0 to 3 and 4 to 7.
    let Some(row) = scaled("box", "index.wgsl", 8, 2, "linear") else { return };
    assert!(close(&row, &[1.5, 5.5], 0.01), "{row:?}");
    // 48 to 1, beyond the 16 a pass averaged before: stripes every third column are grey.
    let Some(row) = scaled("stripes", "stripes.wgsl", 384, 8, "linear") else { return };
    assert!(close(&row, &[1.0 / 3.0; 8], 0.01), "{row:?}");
    // 100 to 1 (a resolution of 1 %): still grey, and a ramp keeps its place.
    let Some(row) = scaled("stripes_far", "stripes.wgsl", 1600, 16, "linear") else { return };
    assert!(close(&row, &[1.0 / 3.0; 16], 0.01), "{row:?}");
    let Some(row) = scaled("box_far", "index.wgsl", 1600, 16, "linear") else { return };
    let ramp: Vec<f32> = (0..16).map(|j| (j as f32 + 0.5) * 100.0 - 0.5).collect();
    assert!(close(&row, &ramp, 1.0), "{row:?}");
}

#[test]
fn cubic_keeps_a_ramp_straight_growing_and_shrinking() {
    // Column x of the source has value x at its centre, x + 0.5 texels from the edge; output
    // pixel j's centre lies (j + 0.5) * footprint texels in. Edges are held, so only the middle.
    let ramp = |footprint: f32, js: std::ops::Range<usize>| -> Vec<f32> {
        js.map(|j| (j as f32 + 0.5) * footprint - 0.5).collect()
    };
    let Some(row) = scaled("cubic_up", "index.wgsl", 8, 16, "cubic") else { return };
    assert!(close(&row[4..12], &ramp(0.5, 4..12), 0.01), "{row:?}");
    let Some(row) = scaled("cubic_down", "index.wgsl", 64, 8, "cubic") else { return };
    assert!(close(&row[2..6], &ramp(8.0, 2..6), 0.05), "{row:?}");
    // Far beyond 16 texels per pixel the kernel still sits on the pixel, not beside it.
    let Some(row) = scaled("cubic_far", "index.wgsl", 1600, 16, "cubic") else { return };
    assert!(close(&row[2..14], &ramp(100.0, 2..14), 1.0), "{row:?}");
    // Shrinking it averages: stripes are grey, with taps on every texel (12 to 1) and on every
    // other texel's edge (24 to 1). The held edges shade the outer pixels.
    let Some(row) = scaled("cubic_stripes", "stripes.wgsl", 96, 8, "cubic") else { return };
    assert!(close(&row[2..6], &[1.0 / 3.0; 4], 0.01), "{row:?}");
    let Some(row) = scaled("cubic_stripes_far", "stripes.wgsl", 384, 16, "cubic") else { return };
    assert!(close(&row[2..14], &[1.0 / 3.0; 12], 0.01), "{row:?}");
    let Some(row) = scaled("cubic_stripes_100", "stripes.wgsl", 1600, 16, "cubic") else { return };
    assert!(close(&row[2..14], &[1.0 / 3.0; 12], 0.01), "{row:?}");
}

/// `tex.fit` at native size puts a 4 texel ramp in the middle of 12 pixels: beside it the edge
/// modes repeat, mirror or hold the ramp, the same in every sampling.
#[test]
fn fit_extends_the_input_by_its_edge_mode_in_every_sampling() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("resample_fit_extend");
    std::fs::write(dir.join("index.wgsl"), INDEX).unwrap();
    let modes = [
        ("repeat", [0.0, 1.0, 2.0, 3.0, 0.0, 1.0, 2.0, 3.0, 0.0, 1.0, 2.0, 3.0]),
        ("mirror", [3.0, 2.0, 1.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0, 2.0, 1.0, 0.0]),
        ("hold", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0, 3.0, 3.0, 3.0]),
    ];
    for sampling in ["linear", "cubic", "nearest"] {
        for (extend, want) in &modes {
            let ops = [
                source("index.wgsl", 4),
                create(
                    "/fit",
                    "tex.fit",
                    &[
                        ("fit", "native".into()),
                        ("width", Value::Int(12)),
                        ("height", Value::Int(4)),
                        ("extend", (*extend).into()),
                        ("sampling", sampling.into()),
                    ],
                ),
                connect("/src", "/fit", 0),
            ];
            let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
            let px = rig.pixels("/fit");
            let got: Vec<f32> = (0..px.width).map(|x| px.get(x, 1).unwrap()[0]).collect();
            assert!(close(&got, want, 0.01), "{sampling} {extend}: {got:?}");
        }
    }
}
