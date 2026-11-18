//! Near points on WARP: `pts.proximity`'s lines against a model on the CPU, the same lines from
//! the same points bit for bit, lines between near points drawn; `pts.neighbor`'s counts and
//! means by the numbers on a grid, and a flock of particles steered by it in their loop.

use std::collections::HashMap;

use penumbra_core::Value;
use penumbra_gpu::Points;
use penumbra_gpu::points::Strips;
use penumbra_graph::Port;

use super::geo::{render, rig};
use super::pts::{coverage, drawn, points};
use super::pts_particles::{live, values};
use super::{Rig, connect, create, gpu, set};

const PROXIMITY: &str = crate::pts::proximity::KIND;
const NEIGHBOR: &str = crate::pts::neighbor::KIND;

fn output(rig: &Rig, path: &str) -> Points {
    rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap().get::<Points>().unwrap().clone()
}

fn xyz(v: [f32; 4]) -> [f32; 3] {
    [v[0], v[1], v[2]]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]) * (a[k] - b[k])).sum::<f32>().sqrt()
}

/// 300 points at random in a ball of 1 m.
fn ball() -> penumbra_graph::Op {
    create(
        "/ball",
        "pts.generate",
        &[
            ("shape", "sphere".into()),
            ("fill", "inside".into()),
            ("distribution", "random".into()),
            ("count", Value::Int(300)),
            ("size", Value::Vec3([2.0; 3])),
        ],
    )
}

/// The lines the CPU expects from `places`: each point's `most` nearest within `distance` and
/// beyond `least`, a pair once.
fn model(places: &[[f32; 3]], distance: f32, least: f32, most: usize) -> Vec<(usize, usize)> {
    let chosen: Vec<Vec<usize>> = (0..places.len())
        .map(|i| {
            let mut c: Vec<(f32, usize)> = (0..places.len())
                .filter(|j| *j != i)
                .map(|j| (dist(places[i], places[j]), j))
                .filter(|(d, _)| *d <= distance && *d >= least)
                .collect();
            c.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            c.into_iter().take(most).map(|(_, j)| j).collect()
        })
        .collect();
    let mut lines = Vec::new();
    for (i, mine) in chosen.iter().enumerate() {
        for j in mine {
            if i < *j || !chosen[*j].contains(&i) {
                lines.push((i, *j));
            }
        }
    }
    lines
}

#[test]
fn proximity_joins_each_point_to_its_nearest_once() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        ball(),
        create(
            "/near",
            PROXIMITY,
            &[
                ("distance", Value::Float(0.3)),
                ("min_distance", Value::Float(0.05)),
                ("max_lines", Value::Int(3)),
            ],
        ),
        connect("/ball", "/near", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let input = points(&mut rig, "/ball");
    let places: Vec<[f32; 3]> = super::pts::read(&rig, &input, "position").into_iter().map(xyz).collect();
    let index: HashMap<[u32; 3], usize> =
        places.iter().enumerate().map(|(i, p)| (p.map(f32::to_bits), i)).collect();
    let out = points(&mut rig, "/near");
    assert_eq!(out.strips(), Strips::Pairs);
    let ends = values(&rig, &out, "position");
    let got: Vec<(usize, usize)> = ends
        .chunks(2)
        .map(|e| (index[&xyz(e[0]).map(f32::to_bits)], index[&xyz(e[1]).map(f32::to_bits)]))
        .collect();
    let want = model(&places, 0.3, 0.05, 3);
    assert!(want.len() > 200, "{}", want.len());
    assert_eq!(got, want);
    let span = values(&rig, &out, "span");
    let span01 = values(&rig, &out, "span01");
    for (k, (a, b)) in got.iter().enumerate() {
        let d = dist(places[*a], places[*b]);
        assert!(
            (span[2 * k][0] - d).abs() < 1e-5 && (span01[2 * k + 1][0] - d / 0.3).abs() < 1e-5,
            "line {k}"
        );
    }
    // The same points again, in another node: the same lines, bit for bit.
    rig.edit(&[
        create(
            "/again",
            PROXIMITY,
            &[
                ("distance", Value::Float(0.3)),
                ("min_distance", Value::Float(0.05)),
                ("max_lines", Value::Int(3)),
            ],
        ),
        connect("/ball", "/again", 0),
    ]);
    let again = points(&mut rig, "/again");
    for name in ["position", "span", "span01"] {
        let a = out.attr(name).unwrap().buffer().read::<u32>(&rig.gpu).unwrap();
        let b = again.attr(name).unwrap().buffer().read::<u32>(&rig.gpu).unwrap();
        assert!(a == b, "{name}");
    }
    assert_eq!(live(&rig, &out), live(&rig, &again));
    // Fewer lines than wanted: a word about it, once the count came back.
    rig.edit(&[set("/near.max", Value::Int(10))]);
    let mut warned = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for seed in 0.. {
        rig.clock.advance(1);
        rig.gpu.poll();
        rig.cook("/near");
        warned = rig.cooker.status(rig.id("/near")).unwrap().warning;
        std::thread::sleep(std::time::Duration::from_millis(20));
        rig.gpu.poll();
        if warned.is_some() || std::time::Instant::now() > deadline {
            break;
        }
        // Nothing changes upstream: a param change makes the node cook again.
        rig.edit(&[set("/near.seed", Value::Int(seed + 1))]);
    }
    assert!(warned.as_deref().is_some_and(|w| w.contains("`max` keeps 10")), "{warned:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn lines_between_near_points_are_drawn() {
    let Some(gpu) = gpu() else { return };
    // Four points at the corners of a 2 m square: its sides are near enough, its diagonals not.
    let ops = vec![
        create(
            "/grid",
            "pts.generate",
            &[
                ("rows", Value::Int(2)),
                ("cols", Value::Int(2)),
                ("size", Value::Vec3([2.0, 1.0, 2.0])),
                ("lines", "none".into()),
            ],
        ),
        create("/near", PROXIMITY, &[("distance", Value::Float(2.1))]),
        connect("/grid", "/near", 0),
        create("/lines", "geo.from_pts", &[("mode", "lines".into()), ("width", Value::Float(0.2))]),
        connect("/near", "/lines", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/lines", "/lit", 0),
        connect("/white", "/lit", 1),
        render(
            "/front",
            &[
                ("position", Value::Vec3([0.0, -8.0, 0.0])),
                ("projection", "orthographic".into()),
                ("size", Value::Float(3.0)),
                ("antialias", Value::Bool(false)),
            ],
        ),
        connect("/lit", "/front", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    let out = points(&mut rig, "/near");
    assert_eq!(live(&rig, &out), 8, "four lines of two ends");
    let px = drawn(&mut rig, "/front");
    let covered = |x: u32, y: u32| px.get(x, y).is_some_and(|p| p[3] > 0.5);
    // About 21 pixels a metre: the corners 43 pixels apart around the centre (48, 32).
    for (x, y) in [(48, 11), (48, 53), (27, 32), (69, 32)] {
        assert!(covered(x, y), "a side at {x}, {y}");
    }
    for (x, y) in [(48, 32), (40, 25), (56, 39)] {
        assert!(!covered(x, y), "no diagonal at {x}, {y}");
    }
    let (n, _) = coverage(&px);
    assert!(n > 300, "{n}");
    let geo = rig
        .cooker
        .output(Port { node: rig.id("/lines"), output: 0 })
        .unwrap()
        .get::<penumbra_gpu::Geo>()
        .unwrap()
        .clone();
    // A copy for each line there may be (four points of up to four lines each), none spent on a
    // first point: a quad of two triangles each.
    assert_eq!(geo.triangle_count(), 2 * 4 * 4);
    let s = rig.cooker.status(rig.id("/lines")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn neighbor_counts_and_averages_by_the_numbers() {
    let Some(gpu) = gpu() else { return };
    // A grid of 5 x 5 points 1 m apart in the xz plane, coloured by index.
    let ops = [
        create(
            "/grid",
            "pts.generate",
            &[("rows", Value::Int(5)), ("cols", Value::Int(5)), ("size", Value::Vec3([4.0, 1.0, 4.0]))],
        ),
        create(
            "/tint",
            "pts.math",
            &[("expression", "vec4f(f32(index), 0.0, 0.0, 1.0)".into()), ("result", "color".into())],
        ),
        connect("/grid", "/tint", 0),
        create(
            "/near",
            NEIGHBOR,
            &[
                ("distance", Value::Float(1.05)),
                ("separation", Value::Float(2.0)),
                ("average", "Color".into()),
            ],
        ),
        connect("/tint", "/near", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let input = points(&mut rig, "/tint");
    let places: Vec<[f32; 3]> = super::pts::read(&rig, &input, "position").into_iter().map(xyz).collect();
    let out = points(&mut rig, "/near");
    assert_eq!(
        (out.count(), out.dims(), out.strips()),
        (input.count(), input.dims(), input.strips()),
        "count and order stay"
    );
    let count = values(&rig, &out, "neighbors");
    let cohesion = values(&rig, &out, "cohesion");
    let separation = values(&rig, &out, "separation");
    let mean = values(&rig, &out, "near_color");
    // The push of a neighbour 1 m away, with `separation` cut to `distance`.
    let push = 1.0 - 1.0 / 1.05;
    for (i, p) in places.iter().enumerate() {
        let near: Vec<usize> =
            (0..places.len()).filter(|j| *j != i && dist(*p, places[*j]) <= 1.05).collect();
        assert_eq!(count[i][0], near.len() as f32, "point {i}");
        let n = near.len() as f32;
        for k in 0..3 {
            let toward: f32 = near.iter().map(|j| places[*j][k] - p[k]).sum::<f32>() / n;
            let away: f32 = near.iter().map(|j| (p[k] - places[*j][k]) * push).sum();
            assert!((cohesion[i][k] - toward).abs() < 1e-5, "point {i}: cohesion {:?}", cohesion[i]);
            assert!((separation[i][k] - away).abs() < 1e-5, "point {i}: separation {:?}", separation[i]);
        }
        let red: f32 = near.iter().map(|j| *j as f32).sum::<f32>() / n;
        assert!((mean[i][0] - red).abs() < 1e-4 && mean[i][3] == 1.0, "point {i}: {:?}", mean[i]);
    }
    // A corner has two neighbours, the middle four.
    assert_eq!((count[0][0], count[12][0]), (2.0, 4.0));
    // Nothing to average: no `near_` attribute; an attribute of whole numbers is refused.
    rig.edit(&[set("/near.average", "".into())]);
    let out = points(&mut rig, "/near");
    assert!(out.attrs().iter().all(|a| !a.name().starts_with("near_")));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_flock_steers_by_its_neighbours_in_a_particle_loop() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create(
            "/src",
            "pts.generate",
            &[
                ("shape", "sphere".into()),
                ("distribution", "random".into()),
                ("count", Value::Int(64)),
                ("size", Value::Vec3([1.0; 3])),
            ],
        ),
        create(
            "/p",
            crate::pts::particles::KIND,
            &[
                ("rate", Value::Float(600.0)),
                ("life", Value::Float(100.0)),
                ("velocity", Value::Vec3([0.3, 0.0, 0.0])),
                ("spread", Value::Float(1.0)),
                ("max", Value::Int(200)),
            ],
        ),
        connect("/src", "/p", 0),
        create("/near", NEIGHBOR, &[("distance", Value::Float(0.4)), ("separation", Value::Float(0.1))]),
        connect("/p", "/near", 0),
        create(
            "/steer",
            "pts.math",
            &[
                ("expression", "2.0 * separation + 0.5 * (near_velocity - velocity) + 0.3 * cohesion".into()),
                ("result", "force".into()),
            ],
        ),
        connect("/near", "/steer", 0),
        create("/target", "pts.null", &[]),
        connect("/steer", "/target", 0),
        connect("/target", "/p", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for kind in [crate::pts::particles::KIND, "pts.math", NEIGHBOR] {
        rig.types.get(kind).unwrap().warm_up(&rig.gpu).unwrap();
    }
    points(&mut rig, "/target");
    for _ in 0..20 {
        rig.clock.advance(1);
        rig.cook("/target");
        for path in ["/p", "/near", "/steer"] {
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert_eq!((s.error, s.warning), (None, None), "{path}: the loop takes the neighbours");
        }
    }
    let out = output(&rig, "/target");
    let n = live(&rig, &out);
    assert!(n > 100, "{n}");
    let count = values(&rig, &out, "neighbors");
    assert!(count.iter().any(|c| c[0] > 0.0), "some particles have neighbours");
    let force = values(&rig, &out, "force");
    assert!(force.iter().all(|f| f.iter().all(|v| v.is_finite())));
    assert!(force.iter().any(|f| f[0] != 0.0 || f[1] != 0.0 || f[2] != 0.0), "the flock steers");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_program_walks_its_neighbours_as_pts_neighbor_does() {
    let Some(gpu) = gpu() else { return };
    let dir = super::scratch("pts-near-shader");
    // Counts the neighbours, averages their colour and sums the way to them, as pts.neighbor does.
    let walk = "// @read position\n// @read color\n// @write heat float\n// @write blur vec4\n// @write way vec3\n// @param reach float = 0.3\n// @neighbors reach\nfn main() {\n    var c = 0.0;\n    var sum = vec4f(0.0);\n    way = vec3f(0.0);\n    var n = near_begin(position, index);\n    while near_next(&n) {\n        c += 1.0;\n        sum += color_at(n.index);\n        way += n.position - position;\n    }\n    heat = c;\n    blur = color;\n    if c > 0.0 {\n        blur = sum / c;\n        way = way / c;\n    }\n}\n";
    std::fs::write(dir.join("walk.wgsl"), walk).unwrap();
    let ops = [
        ball(),
        create("/tint", "pts.random", &[("attribute", "color".into())]),
        connect("/ball", "/tint", 0),
        create("/walk", "pts.shader", &[("file", "walk.wgsl".into())]),
        connect("/tint", "/walk", 0),
        create("/near", NEIGHBOR, &[("distance", Value::Float(0.3)), ("average", "color".into())]),
        connect("/tint", "/near", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let walked = points(&mut rig, "/walk");
    let near = points(&mut rig, "/near");
    let heat = values(&rig, &walked, "heat");
    let count = values(&rig, &near, "neighbors");
    assert!(count.iter().any(|c| c[0] >= 3.0), "points have neighbours");
    for (name, other) in [("heat", "neighbors"), ("blur", "near_color"), ("way", "cohesion")] {
        let a = values(&rig, &walked, name);
        let b = values(&rig, &near, other);
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            for k in 0..3 {
                assert!((x[k] - y[k]).abs() < 1e-4, "{name} at {i}: {x:?} against {other} {y:?}");
            }
        }
    }
    assert_eq!(heat.len(), 300);
    // The radius follows the param: none within a millimetre.
    rig.edit(&[set("/walk.reach", Value::Float(0.001))]);
    let walked = points(&mut rig, "/walk");
    assert!(values(&rig, &walked, "heat").iter().all(|h| h[0] == 0.0));
    // A header mistake names the line.
    std::fs::write(dir.join("walk.wgsl"), "// @write heat float\n// @neighbors far\nfn main() {}\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        rig.clock.advance(1);
        rig.cook("/walk");
        let s = rig.cooker.status(rig.id("/walk")).unwrap();
        if let Some(w) = s.warning.or(s.error) {
            assert!(w.contains("line 2") && w.contains("`far`"), "{w}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no word about the header");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}
