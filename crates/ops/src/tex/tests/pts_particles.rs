//! `pts.particles` on WARP: births at a rate, ids, death at the life span, a full set, motion by
//! the numbers, the same particles for the same steps, time that stands, the loop through a
//! target, reset, and a component's reset.

use penumbra_core::Value;
use penumbra_gpu::Points;
use penumbra_graph::Port;

use super::{Rig, connect, create, gpu, set};

const KIND: &str = crate::pts::particles::KIND;

/// One frame at 60 fps.
const DT: f64 = 1.0 / 60.0;

/// A rig with the given nodes, the particles' programs compiled as at start-up.
fn rig_of(ops: &[penumbra_graph::Op]) -> Option<Rig> {
    let gpu = gpu()?;
    let rig = Rig::new(gpu, None, ops);
    for kind in [KIND, "pts.math", crate::pts::force::KIND, crate::pts::field::KIND] {
        rig.types.get(kind).unwrap().warm_up(&rig.gpu).unwrap();
    }
    Some(rig)
}

/// One source point at the origin.
fn one_point() -> penumbra_graph::Op {
    create(
        "/src",
        "pts.generate",
        &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
    )
}

fn output(rig: &Rig, path: &str) -> Points {
    rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap().get::<Points>().unwrap().clone()
}

/// Moves the clock one frame and cooks `path`, which must cook without a word.
fn step(rig: &mut Rig, path: &str) -> Points {
    rig.clock.advance(1);
    rig.cook(path);
    let s = rig.cooker.status(rig.id(path)).unwrap();
    assert_eq!((s.error, s.warning), (None, None), "{path}");
    output(rig, path)
}

/// The live count.
pub(super) fn live(rig: &Rig, p: &Points) -> usize {
    match p.count().header() {
        Some(h) => h.read::<u32>(&rig.gpu).unwrap()[0] as usize,
        None => p.count().known().unwrap() as usize,
    }
}

/// The first `live` elements of an attribute, as four numbers each (a scalar in the first).
pub(super) fn values(rig: &Rig, p: &Points, name: &str) -> Vec<[f32; 4]> {
    let n = live(rig, p);
    let a = p.attr(name).unwrap_or_else(|| panic!("no {name}"));
    let words = a.buffer().read::<u32>(&rig.gpu).unwrap();
    let per = (a.ty().stride() / 4) as usize;
    (0..n)
        .map(|i| {
            let mut v = [0.0f32; 4];
            for k in 0..per.min(4) {
                let w = words[i * per + k];
                v[k] =
                    if a.ty() == penumbra_gpu::points::AttrType::U32 { w as f32 } else { f32::from_bits(w) };
            }
            v
        })
        .collect()
}

fn ids(rig: &Rig, p: &Points) -> Vec<u32> {
    values(rig, p, "id").iter().map(|v| v[0] as u32).collect()
}

/// Particles born from one point at 30 a second (one every second frame) that live 0.51 s
/// (31 frames): the count, the ids and the ages step by step, against the same rule on the CPU.
#[test]
fn births_at_a_rate_age_and_die_with_their_ids() {
    let ops = [
        one_point(),
        create(
            "/p",
            KIND,
            &[("rate", Value::Float(30.0)), ("life", Value::Float(0.51)), ("max", Value::Int(1000))],
        ),
        connect("/src", "/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    // The CPU's model: birth steps of the particles alive, oldest first.
    let mut alive: Vec<(u32, u32)> = Vec::new();
    let mut carry = 0.0f64;
    let mut next = 0u32;
    for n in 1..=80u32 {
        let p = step(&mut rig, "/p");
        alive.retain(|(b, _)| f64::from(n - b) * DT < 0.51);
        let total = carry + 30.0 * DT;
        let born = total.floor() as u32;
        carry = total - total.floor();
        for _ in 0..born {
            alive.push((n, next));
            next += 1;
        }
        assert_eq!(live(&rig, &p), alive.len(), "step {n}");
        let want: Vec<u32> = alive.iter().map(|(_, id)| *id).collect();
        assert_eq!(ids(&rig, &p), want, "step {n}: the ids of those alive, oldest first");
        let ages: Vec<f32> = values(&rig, &p, "age").iter().map(|v| v[0]).collect();
        for ((b, _), a) in alive.iter().zip(&ages) {
            assert!((f64::from(*a) - f64::from(n - b) * DT).abs() < 1e-4, "step {n}: {ages:?}");
        }
        assert!(values(&rig, &p, "life").iter().all(|v| v[0] == 0.51));
    }
    assert_eq!(alive.len(), 16, "a steady state: 31 frames of one birth every second frame");
    let p = output(&rig, "/p");
    assert_eq!((p.capacity(), p.count().at_most()), (1000, 1000.min(p.count().at_most())));
    assert!(p.count().at_most() <= 41, "the bound grows with the births only: {}", p.count().at_most());
    for name in ["position", "velocity", "force", "age", "life", "id", "normal"] {
        assert!(p.attr(name).is_some() || name == "normal", "{name}");
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// A set that is full takes no births until particles die; ids go on where they stopped.
#[test]
fn a_full_set_waits_for_deaths() {
    let ops = [
        one_point(),
        create(
            "/p",
            KIND,
            &[("rate", Value::Float(600.0)), ("life", Value::Float(0.051)), ("max", Value::Int(5))],
        ),
        connect("/src", "/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let p = step(&mut rig, "/p");
    assert_eq!(ids(&rig, &p), [0, 1, 2, 3, 4], "ten wanted, five fit");
    assert_eq!(p.count().at_most(), 5, "never more than the capacity");
    for n in 2..=3 {
        let p = step(&mut rig, "/p");
        assert_eq!(ids(&rig, &p), [0, 1, 2, 3, 4], "step {n}: full, nothing born");
    }
    // At step 4 the first five are 3 frames old (0.05 s, below 0.051) ... at step 5, 4 frames:
    // they die, five new ones are born.
    let p = step(&mut rig, "/p");
    assert_eq!(ids(&rig, &p), [0, 1, 2, 3, 4]);
    let p = step(&mut rig, "/p");
    assert_eq!(ids(&rig, &p), [5, 6, 7, 8, 9]);
    assert_eq!(p.attr("position").unwrap().buffer().bytes(), 5 * 16, "buffers of the capacity");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The path of one particle against the numbers worked out by hand: gravity, drag and the
/// velocity it starts with.
#[test]
fn a_particle_moves_by_gravity_and_drag() {
    let ops = [
        one_point(),
        create(
            "/p",
            KIND,
            &[
                ("rate", Value::Float(0.0)),
                ("burst", Value::Int(1)),
                ("fire", Value::Bool(true)),
                ("life", Value::Float(100.0)),
                ("velocity", Value::Vec3([1.0, 0.0, 2.0])),
                ("gravity", Value::Vec3([0.0, 0.0, -10.0])),
                ("drag", Value::Float(0.5)),
            ],
        ),
        connect("/src", "/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let p = step(&mut rig, "/p");
    let start = values(&rig, &p, "position")[0];
    assert_eq!(live(&rig, &p), 1, "one burst, at the first step");
    let (mut v, mut x) =
        ([1.0f64, 0.0, 2.0], [f64::from(start[0]), f64::from(start[1]), f64::from(start[2])]);
    for n in 2..=40 {
        let p = step(&mut rig, "/p");
        assert_eq!(live(&rig, &p), 1, "fire stays on: no second burst");
        let g = [0.0, 0.0, -10.0];
        let shrink = (-0.5 * DT).exp();
        for k in 0..3 {
            v[k] = (v[k] + g[k] * DT) * shrink;
            x[k] += v[k] * DT;
        }
        let got = values(&rig, &p, "position")[0];
        let vel = values(&rig, &p, "velocity")[0];
        for k in 0..3 {
            assert!((f64::from(got[k]) - x[k]).abs() < 1e-4, "step {n}: {got:?} {x:?}");
            assert!((f64::from(vel[k]) - v[k]).abs() < 1e-4, "step {n}: {vel:?} {v:?}");
        }
    }
    // Off, `integrate` leaves the particle where it is.
    rig.edit(&[set("/p.integrate", Value::Bool(false))]);
    let p = step(&mut rig, "/p");
    let a = values(&rig, &p, "position")[0];
    let p = step(&mut rig, "/p");
    let b = values(&rig, &p, "position")[0];
    assert_eq!(a, b);
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// What 30 steps from a fresh start give.
fn run(rig: &mut Rig, steps: usize) -> Vec<Vec<[f32; 4]>> {
    let id = rig.id("/p");
    rig.cooker.reset(id);
    let mut out = Vec::new();
    for _ in 0..steps {
        let p = step(rig, "/p");
        out = vec![values(rig, &p, "position"), values(rig, &p, "velocity"), values(rig, &p, "life")];
    }
    out
}

/// The same seed and steps give the same particles, another seed others; a frozen clock moves
/// nothing; `reset` and `node.reset` start over.
#[test]
fn the_same_steps_give_the_same_particles_and_time_that_stands_moves_nothing() {
    let ops = [
        create("/src", "pts.generate", &[("shape", "sphere".into()), ("count", Value::Int(50))]),
        create(
            "/p",
            KIND,
            &[
                ("rate", Value::Float(500.0)),
                ("life", Value::Float(0.2)),
                ("life_variance", Value::Float(0.1)),
                ("along_normal", Value::Float(1.0)),
                ("spread", Value::Float(0.5)),
                ("gravity", Value::Vec3([0.0, 0.0, -9.81])),
            ],
        ),
        connect("/src", "/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let a = run(&mut rig, 30);
    assert!(a[0].len() > 50, "{}", a[0].len());
    let lives: Vec<f32> = a[2].iter().map(|v| v[0]).collect();
    assert!(lives.iter().all(|l| (0.1..=0.3).contains(l)), "{lives:?}");
    assert!(lives.iter().any(|l| *l < 0.15) && lives.iter().any(|l| *l > 0.25), "lives vary");
    assert_eq!(run(&mut rig, 30), a, "the same steps, the same particles");
    rig.edit(&[set("/p.seed", Value::Int(3))]);
    assert_ne!(run(&mut rig, 30), a, "another seed");

    // The clock stands: nothing cooks, nothing moves.
    let before = rig.version("/p");
    let p = output(&rig, "/p");
    let pos = values(&rig, &p, "position");
    for _ in 0..3 {
        rig.cook("/p");
    }
    assert_eq!(rig.version("/p"), before, "no new result while time stands");
    // A param that changes while time stands gives no births and moves nothing.
    rig.edit(&[
        set("/p.rate", Value::Float(5000.0)),
        set("/p.burst", Value::Int(10)),
        set("/p.fire", Value::Bool(true)),
    ]);
    rig.cook("/p");
    let q = output(&rig, "/p");
    assert_eq!(values(&rig, &q, "position"), pos, "the same particles where they were");

    // reset: none while on, a fresh start after.
    rig.edit(&[set("/p.reset", Value::Bool(true))]);
    let p = step(&mut rig, "/p");
    assert_eq!(live(&rig, &p), 0);
    rig.edit(&[
        set("/p.reset", Value::Bool(false)),
        set("/p.burst", Value::Int(0)),
        set("/p.rate", Value::Float(60.0)),
    ]);
    let p = step(&mut rig, "/p");
    assert_eq!(ids(&rig, &p), [0], "ids from 0 again");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Particles in a component start over when the component's `reset` turns on.
#[test]
fn a_components_reset_starts_the_particles_over() {
    let ops = [
        create("/c", crate::comp::COMP_KIND, &[]),
        create(
            "/c/src",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
        ),
        create("/c/p", KIND, &[("rate", Value::Float(60.0)), ("life", Value::Float(10.0))]),
        connect("/c/src", "/c/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    for _ in 0..5 {
        step(&mut rig, "/c/p");
    }
    let p = output(&rig, "/c/p");
    assert_eq!(ids(&rig, &p), [0, 1, 2, 3, 4]);
    // While on, the particles start over at every step; then they carry on.
    rig.edit(&[set("/c.reset", Value::Bool(true))]);
    for _ in 0..3 {
        let p = step(&mut rig, "/c/p");
        assert_eq!(ids(&rig, &p), [0], "started over");
    }
    rig.edit(&[set("/c.reset", Value::Bool(false))]);
    let p = step(&mut rig, "/c/p");
    assert_eq!(ids(&rig, &p), [0, 1]);
}

/// The loop: a force added between the particles and the end of the loop bends the path, as
/// worked out by hand; mass divides it. A chain that drops points is refused in words, and
/// `kill` ends particles from the loop.
#[test]
fn a_force_in_the_loop_moves_the_particles_and_a_delete_is_refused() {
    let ops = [
        one_point(),
        create(
            "/p",
            KIND,
            &[
                ("rate", Value::Float(0.0)),
                ("burst", Value::Int(1)),
                ("fire", Value::Bool(true)),
                ("life", Value::Float(100.0)),
                ("mass", Value::Float(2.0)),
            ],
        ),
        connect("/src", "/p", 0),
        create(
            "/push",
            "pts.math",
            &[("expression", "force + vec3f(1.0, 0.0, 0.0)".into()), ("result", "force".into())],
        ),
        connect("/p", "/push", 0),
        create("/end", "pts.null", &[]),
        connect("/push", "/end", 0),
        connect("/end", "/p", 1),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    // The math compiles off the cook thread: until then the loop has no end, and the particle
    // waits where it was born. Started over once the loop is there.
    for _ in 0..200 {
        rig.clock.advance(1);
        rig.cook("/end");
        let s = rig.cooker.status(rig.id("/push")).unwrap();
        if s.has_output && super::pts::compiled(&s) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let id = rig.id("/p");
    rig.cooker.reset(id);
    let p = step(&mut rig, "/end");
    let x0 = f64::from(values(&rig, &p, "position")[0][0]);
    let (mut v, mut x) = (0.0f64, x0);
    for n in 2..=20 {
        let p = step(&mut rig, "/end");
        assert_eq!(live(&rig, &p), 1);
        // a = force / mass = 0.5.
        v += 0.5 * DT;
        x += v * DT;
        let got = f64::from(values(&rig, &p, "position")[0][0]);
        assert!((got - x).abs() < 1e-5, "step {n}: {got} {x}");
    }
    let p = output(&rig, "/p");
    assert!(values(&rig, &p, "force").iter().all(|f| f[..3] == [0.0; 3]), "the node gives force out as 0");

    // A delete in the loop: refused in words, the particles step on without the loop.
    rig.edit(&[
        create("/del", "pts.delete", &[("expression", "false".into())]),
        connect("/push", "/del", 0),
        connect("/del", "/end", 0),
    ]);
    for _ in 0..100 {
        rig.clock.advance(1);
        rig.cook("/end");
        let s = rig.cooker.status(id).unwrap();
        if let Some(w) = s.warning {
            assert!(w.contains("pts.delete") && w.contains("kill"), "{w}");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(rig.cooker.status(id).unwrap().warning.is_some(), "the delete is named");
    assert_eq!(live(&rig, &output(&rig, "/p")), 1, "the particle lives on");

    // `kill` set by a node in the loop ends the particle at the next step.
    rig.edit(&[
        penumbra_graph::Op::Delete { path: "/del".parse().unwrap() },
        connect("/push", "/end", 0),
        create("/doom", "pts.math", &[("expression", "1.0".into()), ("result", "kill".into())]),
    ]);
    rig.edit(&[connect("/push", "/doom", 0), connect("/doom", "/end", 0)]);
    let mut gone = false;
    for _ in 0..200 {
        rig.clock.advance(1);
        rig.cook("/end");
        if live(&rig, &output(&rig, "/p")) == 0 {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(gone, "killed from the loop");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// `rand_key(id, k)` stays with a particle while those born before it die; `rand(k)` does not.
#[test]
fn numbers_keyed_by_id_stay_with_the_particle() {
    let ops = [
        one_point(),
        create("/p", KIND, &[("rate", Value::Float(60.0)), ("life", Value::Float(0.05))]),
        connect("/src", "/p", 0),
        create(
            "/m",
            "pts.math",
            &[("expression", "vec2f(rand_key(id, 0u), rand(0u))".into()), ("result", "uv".into())],
        ),
        connect("/p", "/m", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let mut first: std::collections::BTreeMap<u32, (f32, f32)> = Default::default();
    let mut moved = false;
    for _ in 0..300 {
        rig.clock.advance(1);
        rig.cook("/m");
        let s = rig.cooker.status(rig.id("/m")).unwrap();
        if !(s.has_output && super::pts::compiled(&s)) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        let p = output(&rig, "/m");
        let uv = values(&rig, &p, "uv");
        for (id, v) in ids(&rig, &p).iter().zip(&uv) {
            let e = first.entry(*id).or_insert((v[0], v[1]));
            assert_eq!(e.0, v[0], "the id's number stays");
            moved |= e.1 != v[1];
        }
        if first.len() > 12 && moved {
            break;
        }
    }
    assert!(moved && first.len() > 12, "the index's number changes as the particles move up");
}

/// Times particles with a curl noise and a vortex in the loop on the hardware GPU, at about
/// 100 000 and 1 000 000 alive: wall time per frame waited for (the count read back after each
/// frame), and per frame of frames cooked back to back.
#[test]
#[ignore = "times particles on the hardware GPU: cargo test -p penumbra-ops particles_timing -- --ignored --nocapture"]
fn particles_timing() {
    let gpu = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Hardware).unwrap();
    eprintln!("{}", gpu.name());
    for (alive, rate) in [(100_000i64, 50_000.0), (1_000_000, 500_000.0)] {
        let ops = [
            create("/src", "pts.generate", &[("shape", "sphere".into()), ("count", Value::Int(10_000))]),
            create(
                "/p",
                KIND,
                &[
                    ("rate", Value::Float(rate)),
                    ("life", Value::Float(2.0)),
                    ("along_normal", Value::Float(0.3)),
                    ("drag", Value::Float(1.0)),
                    ("max", Value::Int(alive + alive / 10)),
                ],
            ),
            connect("/src", "/p", 0),
            create(
                "/curl",
                "pts.noise",
                &[("flavour", "curl".into()), ("attribute", "force".into()), ("speed", Value::Float(0.2))],
            ),
            connect("/p", "/curl", 0),
            create("/swirl", crate::pts::force::KIND, &[("kind", "vortex".into())]),
            connect("/curl", "/swirl", 0),
            create("/end", "pts.null", &[]),
            connect("/swirl", "/end", 0),
            connect("/end", "/p", 1),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        // Compiled, then two lives long to the steady count.
        for _ in 0..400 {
            rig.clock.advance(1);
            rig.cook("/end");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let p = output(&rig, "/end");
        eprintln!("alive {} of at most {}", live(&rig, &p), p.count().at_most());
        let frames = 120;
        let t = std::time::Instant::now();
        for _ in 0..frames {
            rig.clock.advance(1);
            rig.cook("/end");
            let p = output(&rig, "/end");
            let _ = live(&rig, &p);
        }
        let waited = t.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
        let t = std::time::Instant::now();
        for _ in 0..frames {
            rig.clock.advance(1);
            rig.cook("/end");
        }
        let p = output(&rig, "/end");
        let _ = live(&rig, &p);
        let back = t.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);
        eprintln!("{alive}: {waited:.2} ms per frame waited for, {back:.2} ms back to back");
    }
}

/// `per_point`: every source point gives as many births, each particle starts on its point
/// with the point's attributes.
#[test]
fn per_point_births_come_from_every_source_point() {
    let ops = [
        create(
            "/src",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(4)), ("size", Value::Vec3([3.0, 0.0, 0.0]))],
        ),
        create(
            "/p",
            KIND,
            &[("emission", "per_point".into()), ("rate", Value::Float(120.0)), ("life", Value::Float(10.0))],
        ),
        connect("/src", "/p", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let p = step(&mut rig, "/p");
    let xs: Vec<f32> =
        values(&rig, &p, "position").iter().map(|v| (v[0] * 1000.0).round() / 1000.0).collect();
    assert_eq!(xs, [-1.5, -0.5, 0.5, 1.5, -1.5, -0.5, 0.5, 1.5], "two a frame from each point, in turn");
    let p = step(&mut rig, "/p");
    assert_eq!(live(&rig, &p), 16);
    assert_eq!(ids(&rig, &p), (0..16).collect::<Vec<u32>>());
}

/// After particles, `pts.random` gives each particle one value for its whole life and
/// `pts.delete` by random keeps or drops it for its whole life: both follow the `id`.
#[test]
fn random_values_and_random_deletes_stay_with_the_particle() {
    let ops = [
        one_point(),
        create("/p", KIND, &[("rate", Value::Float(60.0)), ("life", Value::Float(0.5))]),
        connect("/src", "/p", 0),
        create("/rnd", "pts.random", &[("attribute", "heat".into())]),
        connect("/p", "/rnd", 0),
        create("/del", "pts.delete", &[("by", "random".into())]),
        connect("/rnd", "/del", 0),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    let mut heat: std::collections::BTreeMap<u32, f32> = Default::default();
    let mut kept: std::collections::BTreeMap<u32, bool> = Default::default();
    let mut frames = 0;
    for _ in 0..400 {
        rig.clock.advance(1);
        rig.cook("/del");
        let ready = ["/rnd", "/del"].iter().all(|n| {
            let s = rig.cooker.status(rig.id(n)).unwrap();
            s.has_output && super::pts::compiled(&s)
        });
        if !ready {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        let all = output(&rig, "/rnd");
        let left: Vec<u32> = ids(&rig, &output(&rig, "/del"));
        for (id, v) in ids(&rig, &all).iter().zip(values(&rig, &all, "heat")) {
            assert_eq!(*heat.entry(*id).or_insert(v[0]), v[0], "particle {id}: its value stays");
            let k = left.contains(id);
            assert_eq!(*kept.entry(*id).or_insert(k), k, "particle {id}: kept or dropped for good");
        }
        frames += 1;
        if frames > 60 {
            break;
        }
    }
    assert!(frames > 60 && heat.len() > 40, "{frames} frames, {} particles", heat.len());
    let share = kept.values().filter(|k| **k).count() as f64 / kept.len() as f64;
    assert!((0.2..0.8).contains(&share), "about half kept: {share}");
}

/// Under exact frames (stepping, recording) a frame waits for the programs it needs: the first
/// frame of a loop nobody compiled yet and the frame after an expression changed run the new
/// programs, so stepping gives the same particles however long a compile takes.
#[test]
fn exact_frames_wait_for_every_compile() {
    let ops = [
        one_point(),
        create("/p", KIND, &[("rate", Value::Float(60.0)), ("life", Value::Float(100.0))]),
        connect("/src", "/p", 0),
        // An expression no other test compiles, so it is not in the process's cache.
        create("/m", "pts.math", &[("expression", "0.0 * 0.12345".into()), ("result", "kill".into())]),
        connect("/p", "/m", 0),
        create(
            "/f",
            crate::pts::force::KIND,
            &[("kind", "vortex".into()), ("strength", Value::Float(0.4321))],
        ),
        connect("/m", "/f", 0),
        create("/end", "pts.null", &[]),
        connect("/f", "/end", 0),
        connect("/end", "/p", 1),
    ];
    let Some(mut rig) = rig_of(&ops) else { return };
    rig.cooker.set_exact_frames(true);
    for n in 1..=12usize {
        if n == 10 {
            rig.edit(&[set("/m.expression", "1.0 + 0.12345".into())]);
        }
        rig.clock.advance(1);
        rig.cook("/end");
        for path in ["/p", "/m", "/f", "/end"] {
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert_eq!((s.error.as_deref(), s.warning.as_deref()), (None, None), "frame {n}: {path}");
            assert!(s.has_output, "frame {n}: {path} has its result");
        }
        let p = output(&rig, "/p");
        // One birth a frame; from frame 10 on the loop kills every particle, so from frame 11
        // on only the one born in the frame is there.
        let want = if n <= 10 { n } else { 1 };
        assert_eq!(live(&rig, &p), want, "frame {n}");
        assert!(live(&rig, &output(&rig, "/end")) == want, "frame {n}: the loop ran this frame");
    }
}
