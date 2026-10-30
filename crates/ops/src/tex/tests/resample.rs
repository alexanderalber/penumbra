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
    // Shrinking it averages: stripes are grey, with taps on every texel (12 to 1) and on every
    // other texel's edge (24 to 1). The held edges shade the outer pixels.
    let Some(row) = scaled("cubic_stripes", "stripes.wgsl", 96, 8, "cubic") else { return };
    assert!(close(&row[2..6], &[1.0 / 3.0; 4], 0.01), "{row:?}");
    let Some(row) = scaled("cubic_stripes_far", "stripes.wgsl", 384, 16, "cubic") else { return };
    assert!(close(&row[2..14], &[1.0 / 3.0; 12], 0.01), "{row:?}");
}
