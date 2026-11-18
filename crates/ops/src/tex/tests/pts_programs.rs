//! Programs over points on WARP: `pts.shader` reloading and a broken file, the numbers of
//! `pts.noise`, `pts.transform`, `pts.math` and `pts.random`, and colours set by a program
//! arriving in the picture `geo.from_pts` draws.

use std::time::{Duration, Instant};

use penumbra_core::Value;
use penumbra_gpu::Points;
use penumbra_graph::{NodeStatus, Port};

use super::geo::{render, rig};
use super::pts::{coverage, drawn, points, read};
use super::{Rig, connect, create, gpu, scratch, set};

fn grid(rows: i64, cols: i64) -> penumbra_graph::Op {
    create(
        "/grid",
        "pts.generate",
        &[("rows", Value::Int(rows)), ("cols", Value::Int(cols)), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
    )
}

fn output(rig: &Rig, path: &str) -> Option<Points> {
    rig.cooker.output(Port { node: rig.id(path), output: 0 })?.get::<Points>().cloned()
}

/// Cooks `path` until its compile has finished and `done` accepts its status; the status and
/// points of that cook.
fn settled(rig: &mut Rig, path: &str, done: impl Fn(&NodeStatus) -> bool) -> (NodeStatus, Option<Points>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        let s = rig.cooker.status(rig.id(path)).unwrap();
        let finished =
            matches!(s.job, Some(penumbra_graph::JobStatus::Done | penumbra_graph::JobStatus::Failed { .. }));
        if finished && done(&s) {
            return (s, output(rig, path));
        }
        assert!(Instant::now() < deadline, "{path} did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Attribute `name` of `path`'s points, cooked anew.
fn fresh(rig: &mut Rig, path: &str, name: &str) -> Vec<[f32; 4]> {
    let p = points(rig, path);
    read(rig, &p, name)
}

/// Positions after the program, with no warning or error.
pub(super) fn clean(rig: &mut Rig, path: &str) -> Points {
    let (s, p) = settled(rig, path, |s| s.has_output);
    assert_eq!((s.error, s.warning), (None, None), "{path}");
    p.unwrap()
}

#[test]
fn a_points_shader_reloads_and_keeps_its_last_good_version() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-shader");
    let file = dir.join("bend.wgsl");
    let bend = "// @write position\n// @param height float = 0.5\nfn main() {\n    position.z += params.height * position.x * position.x;\n}\n";
    std::fs::write(&file, bend).unwrap();
    let ops = [
        grid(1, 3),
        create("/bend", "pts.shader", &[("file", "bend.wgsl".into())]),
        connect("/grid", "/bend", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let p = clean(&mut rig, "/bend");
    let z: Vec<f32> = read(&rig, &p, "position").iter().map(|q| q[2]).collect();
    // A row of three at x -1, 0, 1, z 0 on the grid: bent up at both ends.
    assert_eq!(z, [0.5, 0.0, 0.5]);
    assert!(p.bounds().is_none(), "moved positions drop the bounds");
    let s = rig.cooker.status(rig.id("/bend")).unwrap();
    assert_eq!(s.params.unwrap()[0].name, "height");
    assert!(!s.time_dependent);

    // The param from the node.
    rig.edit(&[set("/bend.height", Value::Float(2.0))]);
    let p = clean(&mut rig, "/bend");
    assert_eq!(read(&rig, &p, "position")[0][2], 2.0);

    // A broken version: the compiler's message names the file and line, the last good program
    // keeps running.
    std::fs::write(&file, "// @write position\n\nfn main() {\n    position = oops;\n}\n").unwrap();
    let (s, p) =
        settled(&mut rig, "/bend", |s| s.warning.as_deref().is_some_and(|w| w.contains("bend.wgsl:4")));
    let w = s.warning.unwrap();
    assert!(w.contains("bend.wgsl:4") && w.contains("last version that compiled"), "{w}");
    assert!(s.error.is_none());
    assert_eq!(read(&rig, &p.unwrap(), "position")[0][2], 2.0);

    // A header mistake names the line too.
    std::fs::write(&file, "// @write position\n// @wirte color\nfn main() {}\n").unwrap();
    let (s, _) = settled(&mut rig, "/bend", |s| s.warning.as_deref().is_some_and(|w| w.contains("@wirte")));
    assert!(s.warning.unwrap().contains("line 2"));

    // Fixed, reading the time and writing a new attribute with a colour from index01.
    std::fs::write(
        &file,
        "// @param height float\n// @write heat float = 0\n// @write color\nfn main() {\n    heat = time * 0.0 + f32(count);\n    color = vec4f(index01, 0.0, 0.0, 1.0);\n}\n",
    )
    .unwrap();
    let (s, p) = settled(&mut rig, "/bend", |s| s.warning.is_none() && s.time_dependent);
    assert!(s.error.is_none());
    let p = p.unwrap();
    assert_eq!(read(&rig, &p, "color").iter().map(|c| c[0]).collect::<Vec<_>>(), [0.0, 0.5, 1.0]);
    assert_eq!(p.attr("heat").unwrap().buffer().read::<f32>(&rig.gpu).unwrap()[..3], [3.0; 3]);
    // Position passes on as the generator made it, bounds and all.
    assert_eq!(read(&rig, &p, "position")[0][2], 0.0);
    assert!(p.bounds().is_some());

    // A file that never compiled is an error, not a warning.
    std::fs::write(dir.join("broken.wgsl"), "// @write position\nfn main() { nope }\n").unwrap();
    rig.edit(&[
        create("/fresh", "pts.shader", &[("file", "broken.wgsl".into())]),
        connect("/grid", "/fresh", 0),
    ]);
    let (s, _) = settled(&mut rig, "/fresh", |s| s.error.is_some());
    assert!(s.error.unwrap().contains("broken.wgsl:2"));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn noise_moves_points_within_its_amplitude_and_changes_with_time() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(8, 8),
        create("/noise", "pts.noise", &[("amplitude", Value::Float(0.2)), ("period", Value::Float(0.5))]),
        connect("/grid", "/noise", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let before = fresh(&mut rig, "/grid", "position");
    let p = points(&mut rig, "/noise");
    let after = read(&rig, &p, "position");
    let moved: Vec<[f32; 3]> =
        before.iter().zip(&after).map(|(a, b)| [b[0] - a[0], b[1] - a[1], b[2] - a[2]]).collect();
    // Scalar noise moves every point along the diagonal, within the amplitude, not all alike.
    for d in &moved {
        assert!(
            d[0].abs() <= 0.2 + 1e-6 && (d[0] - d[1]).abs() < 1e-6 && (d[0] - d[2]).abs() < 1e-6,
            "{d:?}"
        );
    }
    let spread = moved.iter().map(|d| d[0]).fold(f32::MIN, f32::max)
        - moved.iter().map(|d| d[0]).fold(f32::MAX, f32::min);
    assert!(spread > 0.02, "{spread}");
    // The box grew by the amplitude and still holds every point.
    let b = p.bounds().unwrap();
    assert!(after.iter().all(|q| (0..3).all(|k| q[k] >= b.min[k] - 1e-6 && q[k] <= b.max[k] + 1e-6)));
    assert!((b.min[0] - (-1.2)).abs() < 1e-5, "{b:?}");
    let s = rig.cooker.status(rig.id("/noise")).unwrap();
    assert!(!s.time_dependent);

    // Vector noise: each axis on its own. Curl: no box.
    rig.edit(&[set("/noise.flavour", "vector".into())]);
    let v = fresh(&mut rig, "/noise", "position");
    assert!(before.iter().zip(&v).any(|(a, b)| ((b[0] - a[0]) - (b[1] - a[1])).abs() > 1e-3));
    rig.edit(&[set("/noise.flavour", "curl".into())]);
    let c = points(&mut rig, "/noise");
    assert!(c.bounds().is_none());
    assert!(read(&rig, &c, "position").iter().all(|q| q.iter().all(|x| x.is_finite())));

    // Along the normal (the grid faces -y): only y moves.
    rig.edit(&[set("/noise.flavour", "scalar".into()), set("/noise.along_normal", Value::Bool(true))]);
    let n = fresh(&mut rig, "/noise", "position");
    assert!(before.iter().zip(&n).all(|(a, b)| (a[0] - b[0]).abs() < 1e-6 && (a[2] - b[2]).abs() < 1e-6));
    assert!(before.iter().zip(&n).any(|(a, b)| (a[1] - b[1]).abs() > 1e-3));

    // With a speed the noise follows the clock.
    rig.edit(&[set("/noise.along_normal", Value::Bool(false)), set("/noise.speed", Value::Float(2.0))]);
    let first = fresh(&mut rig, "/noise", "position");
    assert!(rig.cooker.status(rig.id("/noise")).unwrap().time_dependent);
    rig.clock.advance(10);
    let later = fresh(&mut rig, "/noise", "position");
    assert_ne!(first, later);

    // Into a colour: the alpha stays, the TouchDesigner name works.
    rig.edit(&[
        set("/noise.speed", Value::Float(0.0)),
        set("/noise.attribute", "Color".into()),
        set("/noise.operation", "set".into()),
        set("/noise.offset", Value::Float(0.5)),
    ]);
    let p = points(&mut rig, "/noise");
    let color = read(&rig, &p, "color");
    assert!(color.iter().all(|c| c[3] == 1.0 && (c[0] - 0.5).abs() <= 0.2 + 1e-6));
    assert_eq!(read(&rig, &p, "position"), before, "position passes on");
    assert!(p.bounds().is_some());
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn transform_moves_turns_and_scales_about_a_pivot() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(1, 2),
        create(
            "/t",
            "pts.transform",
            &[
                ("rotate", Value::Vec3([0.0, 0.0, 90.0])),
                ("scale", Value::Vec3([2.0, 2.0, 2.0])),
                ("translate", Value::Vec3([0.0, 0.0, 1.0])),
                ("pivot", Value::Vec3([1.0, 0.0, 0.0])),
            ],
        ),
        connect("/grid", "/t", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/t");
    let pos = read(&rig, &p, "P");
    // (-1, 0, 0) and (1, 0, 0) about the pivot (1, 0, 0): scaled by 2, turned a quarter about
    // z, moved up 1. (-1, 0, 0) is 2 from the pivot: 4 after scaling, along -y after turning.
    let near = |a: [f32; 4], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5);
    assert!(near(pos[0], [1.0, -4.0, 1.0]), "{:?}", pos[0]);
    assert!(near(pos[1], [1.0, 0.0, 1.0]), "{:?}", pos[1]);
    assert_eq!(pos[0][3], 1.0);
    // The normal (0, -1, 0) turns a quarter about z: (1, 0, 0), unit length.
    let n = read(&rig, &p, "normal");
    assert!(near(n[0], [1.0, 0.0, 0.0]), "{:?}", n[0]);
    assert_eq!(n[0][3], 0.0);
    // The box follows: exactly around the two points, as both lie on its corners.
    let b = p.bounds().unwrap();
    assert!(
        near([b.min[0], b.min[1], b.min[2], 0.0], [1.0, -4.0, 1.0])
            && near([b.max[0], b.max[1], b.max[2], 0.0], [1.0, 0.0, 1.0]),
        "{b:?}"
    );

    // A direction ignores translate and pivot.
    rig.edit(&[set("/t.attribute", "N".into()), set("/t.meaning", "direction".into())]);
    let p = points(&mut rig, "/t");
    let n = read(&rig, &p, "normal");
    assert!(near(n[0], [2.0, 0.0, 0.0]), "{:?}", n[0]);
    assert_eq!(read(&rig, &p, "position")[0], [-1.0, 0.0, 0.0, 1.0]);
    // One number is refused with a reason.
    rig.edit(&[
        create("/s", "pts.random", &[("attribute", "scale".into())]),
        connect("/grid", "/s", 0),
        connect("/s", "/t", 0),
        set("/t.attribute", "scale".into()),
    ]);
    rig.clock.advance(1);
    rig.cook("/t");
    let s = rig.cooker.status(rig.id("/t")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("three numbers")), "{s:?}");
}

#[test]
fn math_computes_attributes_from_an_expression() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(1, 3),
        create(
            "/m",
            "pts.math",
            &[
                ("expression", "P.z + index01 * a".into()),
                ("result", "position.z".into()),
                ("a", Value::Float(2.0)),
            ],
        ),
        connect("/grid", "/m", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = clean(&mut rig, "/m");
    let pos = read(&rig, &p, "position");
    assert_eq!(pos.iter().map(|q| q[2]).collect::<Vec<_>>(), [0.0, 1.0, 2.0]);
    assert_eq!(pos.iter().map(|q| q[0]).collect::<Vec<_>>(), [-1.0, 0.0, 1.0], "x untouched");
    assert!(!rig.cooker.status(rig.id("/m")).unwrap().time_dependent);

    // A new attribute of a chosen type; one number fills a colour; swizzles.
    rig.edit(&[set("/m.expression", "uv.x * 10.0".into()), set("/m.result", "heat".into())]);
    let p = clean(&mut rig, "/m");
    let heat = p.attr("heat").unwrap();
    assert_eq!(heat.ty(), penumbra_gpu::points::AttrType::F32);
    assert_eq!(heat.buffer().read::<f32>(&rig.gpu).unwrap()[..3], [0.0, 5.0, 10.0]);
    rig.edit(&[set("/m.expression", "0.25".into()), set("/m.result", "Color".into())]);
    let p = clean(&mut rig, "/m");
    assert_eq!(read(&rig, &p, "color")[1], [0.25; 4]);
    rig.edit(&[
        set("/m.expression", "vec2f(index01, 1.0 - index01)".into()),
        set("/m.result", "color.gb".into()),
    ]);
    let p = clean(&mut rig, "/m");
    assert_eq!(read(&rig, &p, "color")[2], [1.0, 1.0, 0.0, 1.0]);
    rig.edit(&[set("/m.expression", "sin(time) * 0.0".into()), set("/m.result", "scale".into())]);
    clean(&mut rig, "/m");
    assert!(rig.cooker.status(rig.id("/m")).unwrap().time_dependent);

    // An expression that does not compile: a warning, the last good one keeps running.
    rig.edit(&[set("/m.expression", "position +".into())]);
    let (s, p) = settled(&mut rig, "/m", |s| s.warning.is_some());
    assert!(s.warning.unwrap().contains("last expression that compiled"));
    assert_eq!(p.unwrap().attr("scale").unwrap().buffer().read::<f32>(&rig.gpu).unwrap()[0], 0.0);
    // A result that cannot be is a warning too.
    rig.edit(&[set("/m.expression", "1.0".into()), set("/m.result", "scale.y".into())]);
    let (s, _) = settled(&mut rig, "/m", |s| s.warning.as_deref().is_some_and(|w| w.contains("no .y")));
    assert!(s.error.is_none());
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn random_values_spread_as_asked_and_repeat_for_a_seed() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(20, 20),
        create(
            "/r",
            "pts.random",
            &[("min", Value::Vec4([0.2, 0.4, 0.0, 1.0])), ("max", Value::Vec4([0.3, 0.8, 1.0, 1.0]))],
        ),
        connect("/grid", "/r", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let a = fresh(&mut rig, "/r", "color");
    assert!(a.iter().all(|c| (0.2..0.3).contains(&c[0]) && (0.4..0.8).contains(&c[1]) && c[3] == 1.0));
    let mean = a.iter().map(|c| c[2]).sum::<f32>() / a.len() as f32;
    assert!((mean - 0.5).abs() < 0.05, "{mean}");
    rig.edit(&[set("/r.seed", Value::Int(3))]);
    let b = fresh(&mut rig, "/r", "color");
    assert_ne!(a, b);
    rig.edit(&[set("/r.seed", Value::Int(0))]);
    assert_eq!(fresh(&mut rig, "/r", "color"), a, "the same seed, the same values");

    // Directions of the radius, points inside the ball, a bell around the mean.
    rig.edit(&[
        set("/r.attribute", "velocity".into()),
        set("/r.distribution", "direction".into()),
        set("/r.radius", Value::Float(2.0)),
    ]);
    let v = fresh(&mut rig, "/r", "velocity");
    assert!(v.iter().all(|d| ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() - 2.0).abs() < 1e-4));
    let up = v.iter().filter(|d| d[2] > 0.0).count();
    assert!((160..=240).contains(&up), "{up}");
    rig.edit(&[set("/r.attribute", "P".into()), set("/r.distribution", "sphere".into())]);
    let p = points(&mut rig, "/r");
    let inside = read(&rig, &p, "position");
    assert!(inside.iter().all(|d| d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= 4.0 + 1e-4));
    let b = p.bounds().unwrap();
    assert_eq!((b.min, b.max), ([-2.0; 3], [2.0; 3]));
    rig.edit(&[
        set("/r.attribute", "scale".into()),
        set("/r.distribution", "normal".into()),
        set("/r.mean", Value::Vec4([5.0, 0.0, 0.0, 0.0])),
    ]);
    let s: Vec<f32> =
        points(&mut rig, "/r").attr("scale").unwrap().buffer().read::<f32>(&rig.gpu).unwrap()[..400].to_vec();
    let m = s.iter().sum::<f32>() / 400.0;
    let sd = (s.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / 400.0).sqrt();
    assert!((m - 5.0).abs() < 0.2 && (sd - 1.0).abs() < 0.2, "{m} {sd}");
    // Added to the positions: the box grows by the range.
    rig.edit(&[
        set("/r.attribute", "position".into()),
        set("/r.distribution", "uniform".into()),
        set("/r.operation", "add".into()),
    ]);
    let b = points(&mut rig, "/r").bounds().unwrap();
    assert!((b.min[0] - (-1.0 + 0.2)).abs() < 1e-5 && (b.max[2] - (1.0 + 1.0)).abs() < 1e-5, "{b:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The mean colour of the opaque pixels.
fn mean_colour(px: &penumbra_gpu::Pixels) -> [f32; 3] {
    let lit: Vec<&[f32; 4]> = px.data.iter().filter(|p| p[3] > 0.5).collect();
    let n = lit.len().max(1) as f32;
    [0, 1, 2].map(|k| lit.iter().map(|p| p[k]).sum::<f32>() / n)
}

#[test]
fn colours_a_program_sets_arrive_in_the_picture() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        grid(3, 3),
        create(
            "/tint",
            "pts.math",
            &[("expression", "vec4f(1.0, 0.0, 0.0, 1.0)".into()), ("result", "color".into())],
        ),
        connect("/grid", "/tint", 0),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.4)), ("sprite", "square".into())]),
        connect("/tint", "/sprites", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/white", "/lit", 1),
        render("/front", &[("position", Value::Vec3([0.0, -8.0, 0.0])), ("antialias", Value::Bool(false))]),
        connect("/lit", "/front", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    // The expression compiles off the cook thread: wait for it before drawing.
    clean(&mut rig, "/tint");
    let px = drawn(&mut rig, "/front");
    let (n, _) = coverage(&px);
    assert!(n > 40, "{n}");
    let c = mean_colour(&px);
    assert!(c[0] > 0.9 && c[1] < 0.05 && c[2] < 0.05, "red sprites: {c:?}");

    // Random colours on copies of a box: several colours, none of them white.
    rig.edit(&[
        create(
            "/random",
            "pts.random",
            &[("min", Value::Vec4([0.0, 0.0, 0.5, 1.0])), ("max", Value::Vec4([0.3, 1.0, 1.0, 1.0]))],
        ),
        connect("/grid", "/random", 0),
        connect("/random", "/sprites", 0),
        create("/cube", "geo.primitive", &[("size", Value::Vec3([0.3; 3]))]),
        connect("/cube", "/sprites", 1),
        set("/sprites.mode", "copies".into()),
    ]);
    let px = drawn(&mut rig, "/front");
    let c = mean_colour(&px);
    assert!(c[0] < 0.3 && c[2] > 0.5, "random bluish copies: {c:?}");
    let mut greens: Vec<i32> = px.data.iter().filter(|p| p[3] > 0.5).map(|p| (p[1] * 10.0) as i32).collect();
    greens.sort_unstable();
    greens.dedup();
    assert!(greens.len() >= 3, "{greens:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The correlation of two lists of numbers.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let cov: f32 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f32 = a.iter().map(|x| (x - ma) * (x - ma)).sum();
    let vb: f32 = b.iter().map(|y| (y - mb) * (y - mb)).sum();
    cov / (va * vb).sqrt()
}

#[test]
fn random_values_of_different_attributes_are_unrelated() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(20, 20),
        create("/colour", "pts.random", &[]),
        connect("/grid", "/colour", 0),
        create(
            "/speed",
            "pts.random",
            &[("attribute", "velocity".into()), ("distribution", "direction".into())],
        ),
        connect("/grid", "/speed", 0),
        create("/m", "pts.math", &[("expression", "rand(0u)".into()), ("result", "heat".into())]),
        connect("/grid", "/m", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let red: Vec<f32> = fresh(&mut rig, "/colour", "color").iter().map(|c| c[0]).collect();
    let up: Vec<f32> = fresh(&mut rig, "/speed", "velocity").iter().map(|v| v[2]).collect();
    let m = clean(&mut rig, "/m");
    let heat = m.attr("heat").unwrap().buffer().read::<f32>(&rig.gpu).unwrap()[..400].to_vec();
    // Both seed 0: still each attribute its own numbers.
    assert!(correlation(&red, &up).abs() < 0.2, "{}", correlation(&red, &up));
    assert!(correlation(&red, &heat).abs() < 0.2, "{}", correlation(&red, &heat));
    // And the same again, run after run.
    rig.edit(&[set("/colour.seed", Value::Int(5))]);
    let other: Vec<f32> = fresh(&mut rig, "/colour", "color").iter().map(|c| c[0]).collect();
    assert_ne!(other, red);
    rig.edit(&[set("/colour.seed", Value::Int(0))]);
    let again: Vec<f32> = fresh(&mut rig, "/colour", "color").iter().map(|c| c[0]).collect();
    assert_eq!(again, red);
    // A bell curve of colours keeps their alpha, as directions and balls do.
    rig.edit(&[set("/colour.distribution", "normal".into())]);
    assert!(fresh(&mut rig, "/colour", "color").iter().all(|c| c[3] == 1.0));
}
