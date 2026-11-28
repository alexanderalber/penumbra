//! `pts.resample` and `pts.smooth` as nodes on WARP: lines of any length drawn apart and closed
//! where they were closed, the lines a merge keeps, smoothing in a chain, and params that are no
//! use.

use penumbra_core::Value;
use penumbra_gpu::points::{LINE, Strips};
use penumbra_graph::Port;

use super::geo::{render, rig};
use super::pts::{coverage, drawn, points};
use super::{Rig, connect, create, gpu, set};

/// An orthographic camera looking at the origin from -y, seeing `height` metres from the bottom
/// of the picture (96 x 64) to its top.
fn front(path: &str, height: f64) -> penumbra_graph::Op {
    render(
        path,
        &[
            ("position", Value::Vec3([0.0, -8.0, 0.0])),
            ("projection", "orthographic".into()),
            ("size", Value::Float(height)),
            ("antialias", Value::Bool(false)),
        ],
    )
}

/// `source` drawn as white lines of `width` seen by `camera`.
fn lines(source: &str, camera: &str, width: f64) -> Vec<penumbra_graph::Op> {
    let draw = format!("{camera}_lines");
    let lit = format!("{camera}_lit");
    let white = format!("{camera}_white");
    vec![
        create(&draw, "geo.from_pts", &[("mode", "lines".into()), ("width", Value::Float(width))]),
        connect(source, &draw, 0),
        create(&white, "mat.constant", &[]),
        create(&lit, "geo.assign_material", &[]),
        connect(&draw, &lit, 0),
        connect(&white, &lit, 1),
        front(camera, 3.0),
        connect(&lit, camera, 0),
    ]
}

fn covered(px: &penumbra_gpu::Pixels, x: u32, y: u32) -> bool {
    px.get(x, y).is_some_and(|p| p[3] > 0.5)
}

fn status(rig: &Rig, path: &str) -> (Option<String>, Option<String>) {
    let s = rig.cooker.status(rig.id(path)).unwrap();
    (s.error, s.warning)
}

#[test]
fn lines_of_any_length_are_drawn_apart_and_rings_stay_closed() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        // Two rows of a grid, each 2 m: by length they become lines of their own.
        create(
            "/grid",
            "pts.generate",
            &[
                ("rows", Value::Int(2)),
                ("cols", Value::Int(2)),
                ("size", Value::Vec3([2.0, 1.0, 2.0])),
                ("lines", "rows".into()),
            ],
        ),
        create("/even", "pts.resample", &[("by", "length".into()), ("length", Value::Float(0.1))]),
        connect("/grid", "/even", 0),
        // A hexagon, closed: its outline with and without resampling.
        create("/ring", "pts.generate", &[("shape", "circle".into()), ("count", Value::Int(6))]),
        create("/round", "pts.resample", &[("by", "length".into()), ("length", Value::Float(0.05))]),
        connect("/ring", "/round", 0),
    ];
    ops.extend(lines("/even", "/rows", 0.2));
    ops.extend(lines("/ring", "/hexagon", 0.05));
    ops.extend(lines("/round", "/resampled", 0.05));
    let mut rig = rig(gpu, None, &ops);
    let even = points(&mut rig, "/even");
    assert_eq!(even.strips(), Strips::Runs { closed: false });
    assert!(even.attr(LINE).is_some());
    let px = drawn(&mut rig, "/rows");
    // Both rows, and nothing between the end of the first and the start of the second.
    assert!(covered(&px, 48, 11) && covered(&px, 48, 53), "both rows");
    for (x, y) in [(48, 32), (27, 32), (69, 32), (40, 28)] {
        assert!(!covered(&px, x, y), "nothing at {x}, {y}");
    }
    assert_eq!(points(&mut rig, "/round").strips(), Strips::Runs { closed: true });
    let (hexagon, _) = coverage(&drawn(&mut rig, "/hexagon"));
    let (resampled, _) = coverage(&drawn(&mut rig, "/resampled"));
    // The same outline: a missing closing side would lose a sixth.
    assert!(resampled.abs_diff(hexagon) * 20 < hexagon, "{resampled} pixels resampled, {hexagon} without");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_merge_keeps_lines_of_any_length_and_smoothing_keeps_the_lines() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create("/line", "pts.generate", &[("shape", "line".into()), ("count", Value::Int(5))]),
        create("/a", "pts.resample", &[("by", "length".into()), ("length", Value::Float(0.1))]),
        connect("/line", "/a", 0),
        create("/b", "pts.resample", &[("by", "length".into()), ("length", Value::Float(0.25))]),
        connect("/line", "/b", 0),
        create("/both", "pts.merge", &[]),
        connect("/a", "/both", 0),
        connect("/b", "/both", 1),
        create("/smooth", "pts.smooth", &[("iterations", Value::Int(3))]),
        connect("/both", "/smooth", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    let merged = points(&mut rig, "/both");
    assert_eq!(merged.strips(), Strips::Runs { closed: false });
    let smooth = points(&mut rig, "/smooth");
    assert_eq!(smooth.strips(), Strips::Runs { closed: false });
    assert!(smooth.attr(LINE).is_some());
    // Params out of range are clamped (a param refuses NaN itself); no error.
    for (param, v) in [
        ("/smooth.iterations", Value::Int(-5)),
        ("/smooth.iterations", Value::Int(i64::MAX)),
        ("/smooth.strength", Value::Float(-3.0)),
        ("/a.length", Value::Float(0.0)),
        ("/a.length", Value::Float(-1.0)),
        ("/a.length", Value::Float(1e300)),
        ("/a.by", "count".into()),
        ("/a.count", Value::Int(0)),
        ("/a.count", Value::Int(i64::MIN)),
        ("/a.max", Value::Int(0)),
    ] {
        rig.edit(&[set(param, v)]);
        rig.clock.advance(1);
        rig.cook("/smooth");
        assert_eq!(status(&rig, "/smooth").0, None, "{param}");
    }
    let out = rig.cooker.output(Port { node: rig.id("/smooth"), output: 0 }).unwrap();
    assert!(out.get::<penumbra_gpu::Points>().is_some());
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}
