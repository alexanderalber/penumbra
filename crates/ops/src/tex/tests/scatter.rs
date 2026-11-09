//! `tex.scatter` on WARP: points from a 32-bit float shader add up exactly, land on the pixel
//! `textureLoad` reads at their position, dead ones add nothing, `chan.from_tex` counts them, an
//! unwired node gives an empty picture, and a GPU that cannot blend 32-bit floats gets 16f with
//! a warning.

use penumbra_core::Value;
use penumbra_gpu::{AdapterChoice, FLOAT_FORMAT, FORMAT, Gpu};
use penumbra_graph::Op;

use super::{Rig, chans_of, connect, create, gpu, scratch};

/// 256 x 256 points. Rows 0 to 127: all at (7.6, 3.2). Rows 128 to 255: one per pixel of a
/// 256 x 128 block from (0, 100), each at a fraction inside its pixel. Columns from 250 on are
/// dead (alpha -1).
const POINTS: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let t = vec2u(v.pos.xy); \
    var p = vec2f(7.6, 3.2); \
    if t.y >= 128u { p = vec2f(f32(t.x) + 0.9, f32(t.y - 128u) + 100.05); } \
    let alive = select(1.0, -1.0, t.x >= 250u); \
    return vec4f(p, 0.0, alive); }";

const W: i64 = 300;
const H: i64 = 240;

fn ops(dead: &str) -> Vec<Op> {
    vec![
        create(
            "/p",
            "tex.shader",
            &[
                ("file", "points.wgsl".into()),
                ("resolution", "custom".into()),
                ("width", Value::Int(256)),
                ("height", Value::Int(256)),
                ("format", "32f".into()),
            ],
        ),
        create(
            "/s",
            "tex.scatter",
            &[("width", Value::Int(W)), ("height", Value::Int(H)), ("dead", dead.into())],
        ),
        connect("/p", "/s", 0),
        create("/sum", "chan.from_tex", &[("measure", "sum".into())]),
        connect("/s", "/sum", 0),
    ]
}

#[test]
fn points_add_up_exactly_where_their_positions_say() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("scatter_exact");
    std::fs::write(dir.join("points.wgsl"), POINTS).unwrap();
    let mut rig = Rig::new(gpu, Some(&dir), &ops("none"));
    let tex = rig.cook_until_new("/s", 0);
    assert_eq!(tex.format(), FLOAT_FORMAT);
    assert_eq!(tex.size(), (W as u32, H as u32));
    let px = rig.gpu.read(&tex).unwrap();
    assert_eq!(px.get(7, 3).unwrap(), [32768.0; 4]);
    // The block: one point per pixel, y down from the top.
    assert_eq!(px.get(0, 100).unwrap()[0], 1.0);
    assert_eq!(px.get(255, 227).unwrap()[0], 1.0);
    assert_eq!(px.get(256, 100).unwrap()[0], 0.0);
    assert_eq!(px.get(0, 99).unwrap()[0], 0.0);
    assert_eq!(px.get(0, 228).unwrap()[0], 0.0);
    let mut total = 0.0;
    for y in 0..H as u32 {
        for x in 0..W as u32 {
            total += f64::from(px.get(x, y).unwrap()[0]);
        }
    }
    assert_eq!(total, 65536.0);
    let s = rig.cooker.status(rig.id("/s")).unwrap();
    assert!(s.warning.is_none() && s.error.is_none(), "{s:?}");

    // Dead by alpha: columns 250 to 255 of every row add nothing.
    let mut rig = Rig::new(rig.gpu.clone(), Some(&dir), &ops("alpha"));
    let sum = chans_of(&mut rig, "/sum");
    assert_eq!(sum.value(Some("r")), Some(65536.0 - 6.0 * 256.0));
    let px = rig.pixels("/s");
    assert_eq!(px.get(7, 3).unwrap()[0], 250.0 * 128.0);
    assert_eq!(px.get(250, 100).unwrap()[0], 0.0);
}

#[test]
fn unwired_it_is_an_empty_picture() {
    let Some(gpu) = gpu() else { return };
    let ops = [create("/s", "tex.scatter", &[("width", Value::Int(8)), ("height", Value::Int(4))])];
    let mut rig = Rig::new(gpu, None, &ops);
    let px = rig.pixels("/s");
    assert_eq!(px.get(3, 2), Some([0.0; 4]));
    let s = rig.cooker.status(rig.id("/s")).unwrap();
    assert!(s.warning.is_none() && s.error.is_none(), "{s:?}");
}

#[test]
fn without_32_bit_float_blending_it_adds_in_half_floats_and_warns() {
    let Ok(gpu) = Gpu::headless_without_float_blending(AdapterChoice::Software) else { return };
    assert!(gpu.float_targets() && !gpu.float_blending());
    let dir = scratch("scatter_half");
    std::fs::write(dir.join("points.wgsl"), POINTS).unwrap();
    let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops("none"));
    let tex = rig.cook_until_new("/s", 0);
    assert_eq!(tex.format(), FORMAT);
    let px = rig.gpu.read(&tex).unwrap();
    // Half floats hold whole numbers exactly up to 2048: one per pixel stays exact, 32768 on
    // one pixel does not get counted to the end.
    assert_eq!(px.get(0, 100).unwrap()[0], 1.0);
    assert!(px.get(7, 3).unwrap()[0] < 32768.0);
    let s = rig.cooker.status(rig.id("/s")).unwrap();
    assert!(s.warning.as_deref().is_some_and(|w| w.contains("16f")), "{s:?}");
    // 16f asked for: no warning.
    rig.edit(&[super::set("/s.format", "16f".into())]);
    let v = rig.version("/s");
    rig.cook_until_new("/s", v);
    let s = rig.cooker.status(rig.id("/s")).unwrap();
    assert!(s.warning.is_none(), "{s:?}");
    assert_eq!(gpu.errors().count, 0);
}
