//! `pts.force` and `pts.field` by the numbers, `pts.feedback` carrying points over frames, and
//! particles drawn end to end, on WARP.

use penumbra_core::Value;
use penumbra_gpu::Points;

use super::geo::{render, rig};
use super::pts::{coverage, points};
use super::pts_particles::{live, values};
use super::{Rig, connect, create, gpu, set};

/// Five points on the x axis at -2, -1, 0, 1, 2.
fn line() -> penumbra_graph::Op {
    create(
        "/line",
        "pts.generate",
        &[("shape", "line".into()), ("count", Value::Int(5)), ("size", Value::Vec3([4.0, 0.0, 0.0]))],
    )
}

fn close(a: [f32; 4], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-4)
}

/// The `force` of the five points after `edits`.
fn forces(rig: &mut Rig, edits: Vec<penumbra_graph::Op>) -> Vec<[f32; 4]> {
    rig.edit(&edits);
    let p = points(rig, "/f");
    values(rig, &p, "force")
}

#[test]
fn each_force_pushes_its_way_and_falls_off_with_distance() {
    let Some(gpu) = gpu() else { return };
    let ops =
        [line(), create("/f", "pts.force", &[("strength", Value::Float(2.0))]), connect("/line", "/f", 0)];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/line");
    let xs: Vec<f32> = values(&rig, &p, "position").iter().map(|v| v[0]).collect();
    assert_eq!(xs, [-2.0, -1.0, 0.0, 1.0, 2.0]);
    let check = |got: &[[f32; 4]], want: &[[f32; 3]], what: &str| {
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, *w), "{what}: {got:?}");
        }
    };
    let f = forces(&mut rig, vec![]);
    check(&f, &[[2.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0; 3], [-2.0, 0.0, 0.0], [-2.0, 0.0, 0.0]], "attract");
    let f = forces(&mut rig, vec![set("/f.kind", "repel".into())]);
    check(&f, &[[-2.0, 0.0, 0.0], [-2.0, 0.0, 0.0], [0.0; 3], [2.0, 0.0, 0.0], [2.0, 0.0, 0.0]], "repel");
    let f = forces(&mut rig, vec![set("/f.falloff", "linear".into()), set("/f.radius", Value::Float(2.0))]);
    check(&f, &[[0.0; 3], [-1.0, 0.0, 0.0], [0.0; 3], [1.0, 0.0, 0.0], [0.0; 3]], "linear");
    let f = forces(&mut rig, vec![set("/f.falloff", "smooth".into())]);
    check(
        &f,
        &[[0.0; 3], [-1.0, 0.0, 0.0], [0.0; 3], [1.0, 0.0, 0.0], [0.0; 3]],
        "smooth: half way at half the radius",
    );
    let f = forces(
        &mut rig,
        vec![set("/f.falloff", "inverse_square".into()), set("/f.radius", Value::Float(1.0))],
    );
    check(
        &f,
        &[[-0.4, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0; 3], [1.0, 0.0, 0.0], [0.4, 0.0, 0.0]],
        "inverse square",
    );
    // Towards and around the z axis through x = 1.
    let f = forces(
        &mut rig,
        vec![
            set("/f.falloff", "none".into()),
            set("/f.kind", "axis".into()),
            set("/f.center", Value::Vec3([1.0, 0.0, 5.0])),
        ],
    );
    check(&f, &[[2.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0; 3], [-2.0, 0.0, 0.0]], "axis");
    let f = forces(&mut rig, vec![set("/f.kind", "vortex".into())]);
    check(
        &f,
        &[[0.0, -2.0, 0.0], [0.0, -2.0, 0.0], [0.0, -2.0, 0.0], [0.0; 3], [0.0, 2.0, 0.0]],
        "vortex, anticlockwise",
    );
    let f = forces(
        &mut rig,
        vec![set("/f.kind", "direction".into()), set("/f.direction", Value::Vec3([0.0, 0.0, -3.0]))],
    );
    check(&f, &[[0.0, 0.0, -2.0]; 5], "direction: its length does not count");
    // Wind of 3 m/s along x on points at rest: strength times the difference in speed.
    let f = forces(
        &mut rig,
        vec![
            set("/f.kind", "wind".into()),
            set("/f.direction", Value::Vec3([3.0, 0.0, 0.0])),
            set("/f.strength", Value::Float(0.5)),
        ],
    );
    check(&f, &[[1.5, 0.0, 0.0]; 5], "wind");
    // A mask: weight 0 at the first point, 1 at the last.
    rig.edit(&[
        create("/w", "pts.math", &[("expression", "index01".into()), ("result", "weight".into())]),
        connect("/line", "/w", 0),
        connect("/w", "/f", 0),
    ]);
    points(&mut rig, "/w");
    let f = forces(&mut rig, vec![]);
    check(
        &f,
        &[[0.0; 3], [0.375, 0.0, 0.0], [0.75, 0.0, 0.0], [1.125, 0.0, 0.0], [1.5, 0.0, 0.0]],
        "masked by weight",
    );
    let f = forces(&mut rig, vec![set("/f.mask", "".into())]);
    check(&f, &[[1.5, 0.0, 0.0]; 5], "no mask");
    // Into velocity, added at once; an existing value is added to.
    let p = {
        rig.edit(&[set("/f.attribute", "velocity".into()), set("/f.kind", "direction".into())]);
        points(&mut rig, "/f")
    };
    let v = values(&rig, &p, "velocity");
    check(&v, &[[0.5, 0.0, 0.0]; 5], "velocity: strength 0.5 along x");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn each_field_shape_weighs_the_points() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        line(),
        create(
            "/field",
            "pts.field",
            &[("radius", Value::Float(1.0)), ("falloff", Value::Float(0.0)), ("distance", "gap".into())],
        ),
        connect("/line", "/field", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let mut weights = |edits: Vec<penumbra_graph::Op>| -> (Vec<f32>, Points) {
        rig.edit(&edits);
        let p = points(&mut rig, "/field");
        (values(&rig, &p, "weight").iter().map(|v| v[0]).collect(), p)
    };
    let (w, p) = weights(vec![]);
    assert_eq!(w, [0.0, 1.0, 1.0, 1.0, 0.0], "sphere");
    let _ = p;
    let (w, _) = weights(vec![set("/field.falloff", Value::Float(2.0))]);
    assert_eq!(w, [0.5, 1.0, 1.0, 1.0, 0.5], "a soft edge two metres wide");
    let (w, _) = weights(vec![
        set("/field.falloff", Value::Float(0.0)),
        set("/field.shape", "box".into()),
        set("/field.size", Value::Vec3([2.2, 1.0, 1.0])),
    ]);
    assert_eq!(w, [0.0, 1.0, 1.0, 1.0, 0.0], "box");
    let (w, _) = weights(vec![
        set("/field.shape", "plane".into()),
        set("/field.normal", Value::Vec3([2.0, 0.0, 0.0])),
    ]);
    assert_eq!(w, [0.0, 0.0, 1.0, 1.0, 1.0], "plane: the side the normal points to");
    let (w, _) = weights(vec![
        set("/field.shape", "capsule".into()),
        set("/field.axis", Value::Vec3([1.0, 0.0, 0.0])),
        set("/field.length", Value::Float(2.0)),
        set("/field.radius", Value::Float(0.5)),
    ]);
    assert_eq!(w, [0.0, 1.0, 1.0, 1.0, 0.0], "capsule");
    let (w, _) = weights(vec![set("/field.invert", Value::Bool(true))]);
    assert_eq!(w, [1.0, 0.0, 0.0, 0.0, 1.0], "inverted");
    // The distance from the edge: half a metre beyond the round ends at the outer points.
    let (_, p) = weights(vec![]);
    let gap: Vec<f32> = values(&rig, &p, "gap").iter().map(|v| v[0]).collect();
    assert_eq!(gap, [0.5, -0.5, -0.5, -0.5, 0.5]);
    // Combined with a weight there already: 0.5 everywhere.
    rig.edit(&[
        create("/half", "pts.math", &[("expression", "0.5".into()), ("result", "weight".into())]),
        connect("/line", "/half", 0),
        connect("/half", "/field", 0),
        set("/field.invert", Value::Bool(false)),
    ]);
    points(&mut rig, "/half");
    let mut w_of = |combine: &str| {
        rig.edit(&[set("/field.combine", combine.into())]);
        let p = points(&mut rig, "/field");
        values(&rig, &p, "weight").iter().map(|v| v[0]).collect::<Vec<f32>>()
    };
    assert_eq!(w_of("multiply"), [0.0, 0.5, 0.5, 0.5, 0.0]);
    assert_eq!(w_of("add"), [0.5, 1.5, 1.5, 1.5, 0.5]);
    assert_eq!(w_of("max"), [0.5, 1.0, 1.0, 1.0, 0.5]);
    assert_eq!(w_of("min"), [0.0, 0.5, 0.5, 0.5, 0.0]);
    assert_eq!(w_of("set"), [0.0, 1.0, 1.0, 1.0, 0.0]);
}

/// A loop of points: each frame lifts them by 10 cm and deletes the first one; the first frame
/// passes `initial`, a reset starts from it again.
#[test]
fn feedback_carries_points_over_frames_whatever_the_loop_does_to_them() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        line(),
        create("/fb", "pts.feedback", &[]),
        connect("/line", "/fb", 0),
        create("/lift", "pts.transform", &[("translate", Value::Vec3([0.0, 0.0, 0.1]))]),
        connect("/fb", "/lift", 0),
        create("/del", "pts.delete", &[("by", "index".into())]),
        connect("/lift", "/del", 0),
        create("/heat", "pts.math", &[("expression", "1.0".into()), ("result", "heat".into())]),
        connect("/del", "/heat", 0),
        connect("/heat", "/fb", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    // Until the loop's programs compile it carries nothing back; then frame by frame.
    let first = points(&mut rig, "/heat");
    let n0 = live(&rig, &first);
    let z0 = values(&rig, &first, "position")[0][2];
    let mut want = (n0, z0);
    for _ in 0..3 {
        let p = points(&mut rig, "/heat");
        want = (want.0 - 1, want.1 + 0.1);
        assert_eq!(live(&rig, &p), want.0, "one point fewer each frame");
        let pos = values(&rig, &p, "position");
        assert!((pos[0][2] - want.1).abs() < 1e-5, "{pos:?}");
        assert!(p.attr("heat").is_some(), "an attribute the input lacks comes round");
    }
    // Reset: `initial` again.
    rig.edit(&[set("/fb.reset", Value::Bool(true))]);
    let p = points(&mut rig, "/fb");
    assert_eq!(live(&rig, &p), 5);
    assert!(p.attr("heat").is_none());
    rig.edit(&[set("/fb.reset", Value::Bool(false))]);
    let id = rig.id("/fb");
    rig.cooker.reset(id);
    let p = points(&mut rig, "/fb");
    assert_eq!(
        (live(&rig, &p), values(&rig, &p, "position")[0][2]),
        (5, 0.0),
        "node.reset starts from `initial`"
    );
}

/// A box that a loop grows every frame (noise added to the positions widens it by the amplitude
/// each time) does not come round: what comes back has no box, so a picture of it is framed by
/// the box read back from the GPU.
#[test]
fn a_loop_does_not_grow_its_box() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        line(),
        create("/fb", "pts.feedback", &[]),
        connect("/line", "/fb", 0),
        create("/drift", "pts.noise", &[("amplitude", Value::Float(0.5)), ("speed", Value::Float(1.0))]),
        connect("/fb", "/drift", 0),
        connect("/drift", "/fb", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let first = points(&mut rig, "/fb");
    assert!(first.bounds().is_some(), "`initial` keeps its box");
    for frame in 0..12 {
        let p = points(&mut rig, "/drift");
        // At most the line's box grown once by the amplitude.
        let b = p.bounds().map(|b| b.max[0] - b.min[0]);
        assert!(b.is_none_or(|w| w <= 5.0 + 1e-4), "frame {frame}: a box {b:?} m wide");
    }
}

/// A loop that keeps its points where they are keeps their exact box frame after frame, so a
/// node after it that fits the box (pts.lookup_tex) has it to fit. One that shrinks the box keeps
/// the smaller box.
#[test]
fn a_loop_that_keeps_its_box_passes_it_on() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        line(),
        create("/fb", "pts.feedback", &[]),
        connect("/line", "/fb", 0),
        create("/still", "pts.null", &[]),
        connect("/fb", "/still", 0),
        connect("/still", "/fb", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for frame in 0..6 {
        let p = points(&mut rig, "/fb");
        let b = p.bounds().map(|b| (b.min[0], b.max[0]));
        assert_eq!(b, Some((-2.0, 2.0)), "frame {frame}");
    }
    let ops = [
        line(),
        create("/fb", "pts.feedback", &[]),
        connect("/line", "/fb", 0),
        create("/shrink", "pts.transform", &[("scale", Value::Vec3([0.5; 3]))]),
        connect("/fb", "/shrink", 0),
        connect("/shrink", "/fb", 1),
    ];
    let mut rig = Rig::new(rig.gpu.clone(), None, &ops);
    for frame in 0..6 {
        let p = points(&mut rig, "/fb");
        let w = p.bounds().map(|b| b.max[0] - b.min[0]);
        assert!(w.is_some_and(|w| w <= 4.0 + 1e-4), "frame {frame}: {w:?}");
    }
}

/// Particles from a point, fanned out, drawn as sprites: more of the picture covered as they
/// spread; none drawn while `reset` holds them.
#[test]
fn particles_draw_end_to_end() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create(
            "/src",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
        ),
        create(
            "/p",
            crate::pts::particles::KIND,
            &[
                ("rate", Value::Float(600.0)),
                ("life", Value::Float(5.0)),
                ("spread", Value::Float(2.0)),
                ("max", Value::Int(400)),
            ],
        ),
        connect("/src", "/p", 0),
        // Drawn from the end of a loop, as a network with forces is.
        create("/end", "pts.null", &[]),
        connect("/p", "/end", 0),
        connect("/end", "/p", 1),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.1)), ("sprite", "square".into())]),
        connect("/end", "/sprites", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/white", "/lit", 1),
        render("/front", &[("position", Value::Vec3([0.0, -8.0, 0.0])), ("antialias", Value::Bool(false))]),
        connect("/lit", "/front", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    rig.types.get(crate::pts::particles::KIND).unwrap().warm_up(&rig.gpu).unwrap();
    let mut seen = Vec::new();
    for _ in 0..3 {
        for _ in 0..10 {
            rig.clock.advance(1);
            rig.cook("/front");
        }
        let px = rig.gpu.read(&rig.tex("/front").unwrap()).unwrap();
        let (n, b) = coverage(&px);
        seen.push((n, b[2].saturating_sub(b[0])));
    }
    assert!(seen[0].0 > 10, "{seen:?}");
    assert!(seen[2].1 > seen[0].1 * 3 / 2, "the cloud widens: {seen:?}");
    let p = rig.cooker.output(penumbra_graph::Port { node: rig.id("/p"), output: 0 }).unwrap();
    assert_eq!(live(&rig, p.get::<Points>().unwrap()), 300, "ten births a frame, thirty frames");
    rig.edit(&[set("/p.reset", Value::Bool(true))]);
    rig.clock.advance(1);
    rig.cook("/front");
    let px = rig.gpu.read(&rig.tex("/front").unwrap()).unwrap();
    assert_eq!(coverage(&px).0, 0, "no particles, nothing drawn");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// One particle in a loop with wind, until the loop's program has compiled; then started over.
fn windy(strength: f64, mass: f64) -> Option<(Rig, Vec<[f32; 4]>)> {
    let gpu = gpu()?;
    let ops = [
        create(
            "/src",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
        ),
        create(
            "/p",
            crate::pts::particles::KIND,
            &[
                ("rate", Value::Float(0.0)),
                ("burst", Value::Int(1)),
                ("fire", Value::Bool(true)),
                ("life", Value::Float(100.0)),
                ("mass", Value::Float(mass)),
            ],
        ),
        connect("/src", "/p", 0),
        create(
            "/wind",
            "pts.force",
            &[
                ("kind", "wind".into()),
                ("direction", Value::Vec3([3.0, 0.0, 0.0])),
                ("strength", Value::Float(strength)),
            ],
        ),
        connect("/p", "/wind", 0),
        create("/end", "pts.null", &[]),
        connect("/wind", "/end", 0),
        connect("/end", "/p", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    rig.cooker.set_exact_frames(true);
    for _ in 0..500 {
        rig.clock.advance(1);
        rig.cook("/end");
        let s = rig.cooker.status(rig.id("/wind")).unwrap();
        if s.has_output && super::pts::compiled(&s) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let id = rig.id("/p");
    rig.cooker.reset(id);
    let mut speeds = Vec::new();
    for _ in 0..40 {
        rig.clock.advance(1);
        rig.cook("/end");
        let p = rig
            .cooker
            .output(penumbra_graph::Port { node: id, output: 0 })
            .unwrap()
            .get::<Points>()
            .unwrap()
            .clone();
        speeds.push(values(&rig, &p, "velocity")[0]);
    }
    Some((rig, speeds))
}

/// Wind takes a light particle to the wind's speed without overshooting it, however light; a
/// negative strength pushes nothing.
#[test]
fn wind_never_overshoots_and_a_negative_strength_pushes_nothing() {
    let Some((rig, v)) = windy(2.0, 0.01) else { return };
    assert!(v.iter().all(|s| s[0].is_finite() && (0.0..=3.0 + 1e-4).contains(&s[0])), "{v:?}");
    assert!((v[39][0] - 3.0).abs() < 1e-3, "the wind's speed: {v:?}");
    assert!(v.windows(2).all(|w| w[1][0] >= w[0][0] - 1e-6), "never past it and back: {v:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
    // A heavy particle follows slowly, as before: about strength / mass of the difference a second.
    let Some((_, v)) = windy(0.5, 10.0) else { return };
    assert!(v[0][0] < 0.01 && v[39][0] > 0.05 && v[39][0] < 0.2, "{v:?}");
    let Some((_, v)) = windy(-2.0, 0.01) else { return };
    assert!(v.iter().all(|s| s[0] == 0.0), "{v:?}");
}

/// The pull wind leaves for the particles stays in their loop: the loop carries it round, but a
/// merge and a table of every attribute after the loop show none, and wind on points that are no
/// particles leaves none.
#[test]
fn the_pull_of_wind_does_not_leave_the_loop() {
    let Some((mut rig, _)) = windy(2.0, 0.01) else { return };
    let wind = crate::pts::force::WIND;
    let end = points(&mut rig, "/end");
    assert!(end.attr(wind).is_some(), "the loop carries the pull round");
    rig.edit(&[
        create("/m", "pts.merge", &[]),
        connect("/end", "/m", 0),
        create("/t", "table.from_pts", &[]),
        connect("/end", "/t", 0),
        line(),
        create("/f", "pts.force", &[("kind", "wind".into())]),
        connect("/line", "/f", 0),
    ]);
    let merged = points(&mut rig, "/m");
    assert!(merged.attr(wind).is_none() && merged.attr("velocity").is_some(), "one input");
    rig.edit(&[connect("/line", "/m", 1)]);
    let merged = points(&mut rig, "/m");
    assert!(merged.attr(wind).is_none() && merged.attr("velocity").is_some(), "two inputs");
    let t = super::pts_bridges::table(&mut rig, "/t");
    assert!(!t.rows()[0].iter().any(|n| n.starts_with(wind)), "{:?}", t.rows()[0]);
    let plain = points(&mut rig, "/f");
    assert!(plain.attr("force").is_some() && plain.attr(wind).is_none());
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}
