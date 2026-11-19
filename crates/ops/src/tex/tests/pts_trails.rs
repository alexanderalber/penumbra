//! Lines and trails on WARP: `geo.from_pts` in lines mode (width, facing the camera from the
//! front and from above, lines not joined to each other, a count on the GPU, width and colour per
//! point), `pts.trail` against a model on the CPU (a fixed set, particles that are born and die,
//! `every`, `reset`, time that stands, a reset of the node), and which nodes keep the lines.

use std::collections::HashMap;

use penumbra_core::Value;
use penumbra_gpu::Points;
use penumbra_gpu::points::Strips;
use penumbra_graph::Port;

use super::geo::{render, rig};
use super::pts::{coverage, drawn, points};
use super::pts_particles::{live, values};
use super::{Rig, connect, create, gpu, set};

const TRAIL: &str = crate::pts::trail::KIND;

/// An orthographic camera at `position` looking at the origin, seeing `height` metres from the
/// bottom of the picture (96 x 64) to its top; back faces culled, so a ribbon turned away would
/// vanish.
fn ortho(path: &str, position: [f64; 3], height: f64) -> penumbra_graph::Op {
    render(
        path,
        &[
            ("position", Value::Vec3(position)),
            ("projection", "orthographic".into()),
            ("size", Value::Float(height)),
            ("antialias", Value::Bool(false)),
            ("cull", Value::Bool(true)),
        ],
    )
}

/// `source` drawn as lines of `width`, white, into `/lit`.
fn lines(source: &str, width: f64) -> Vec<penumbra_graph::Op> {
    vec![
        create("/lines", "geo.from_pts", &[("mode", "lines".into()), ("width", Value::Float(width))]),
        connect(source, "/lines", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/lines", "/lit", 0),
        connect("/white", "/lit", 1),
    ]
}

fn output(rig: &Rig, path: &str) -> Points {
    rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap().get::<Points>().unwrap().clone()
}

/// Width and height of the box around the covered pixels.
fn extent(b: [u32; 4]) -> (u32, u32) {
    (b[2] + 1 - b[0], b[3] + 1 - b[1])
}

#[test]
fn a_line_is_a_ribbon_of_its_width_that_faces_the_camera() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![create(
        "/line",
        "pts.generate",
        &[("shape", "line".into()), ("count", Value::Int(2)), ("size", Value::Vec3([2.0, 0.0, 0.0]))],
    )];
    ops.extend(lines("/line", 0.25));
    // 32 pixels a metre: the line 64 long, 8 wide.
    ops.extend([
        ortho("/front", [0.0, -8.0, 0.0], 2.0),
        connect("/lit", "/front", 0),
        ortho("/above", [0.0, 0.0, 8.0], 2.0),
        connect("/lit", "/above", 0),
    ]);
    let mut rig = rig(gpu, None, &ops);
    let p = points(&mut rig, "/line");
    assert_eq!(p.strips(), Strips::regular(2), "a generated line is one line");
    for camera in ["/front", "/above"] {
        let px = drawn(&mut rig, camera);
        let (n, b) = coverage(&px);
        let (w, h) = extent(b);
        let (long, short) = (w.max(h), w.min(h));
        assert!((62..=66).contains(&long) && (7..=9).contains(&short), "{camera}: {w} x {h}");
        assert!((440..=600).contains(&n), "{camera}: {n} pixels");
    }
    let geo = rig
        .cooker
        .output(Port { node: rig.id("/lines"), output: 0 })
        .unwrap()
        .get::<penumbra_gpu::Geo>()
        .unwrap()
        .clone();
    assert_eq!(geo.triangle_count(), 2, "a line of two points is one quad");
    // The box reaches out by the corners' reach beyond the points.
    let b = geo.bounds().unwrap();
    assert!(b.max[0] >= 1.25 && b.min[0] <= -1.25, "{b:?}");
    let s = rig.cooker.status(rig.id("/lines")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn the_lines_of_a_grid_join_only_their_own_points() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![create(
        "/grid",
        "pts.generate",
        &[
            ("rows", Value::Int(2)),
            ("cols", Value::Int(2)),
            ("size", Value::Vec3([2.0, 1.0, 2.0])),
            ("lines", "rows".into()),
        ],
    )];
    ops.extend(lines("/grid", 0.2));
    // About 21 pixels a metre: the points at the corners of a square 43 pixels wide.
    ops.extend([ortho("/front", [0.0, -8.0, 0.0], 3.0), connect("/lit", "/front", 0)]);
    let mut rig = rig(gpu, None, &ops);
    assert_eq!(points(&mut rig, "/grid").strips(), Strips::regular(2));
    let px = drawn(&mut rig, "/front");
    let covered = |px: &penumbra_gpu::Pixels, x: u32, y: u32| px.get(x, y).is_some_and(|p| p[3] > 0.5);
    // The two rows: covered at their middles above and below the centre; nothing joins the end
    // of the first row to the start of the second.
    assert!(covered(&px, 48, 11) && covered(&px, 48, 53), "both rows");
    for (x, y) in [(48, 32), (27, 32), (69, 32), (40, 28)] {
        assert!(!covered(&px, x, y), "nothing at {x}, {y}");
    }
    // Rows and columns: the sides are there too, the middle stays empty.
    rig.edit(&[set("/grid.lines", "both".into())]);
    let p = points(&mut rig, "/grid");
    assert_eq!(p.strips(), Strips::Grid { cols: 2, rows: true, columns: true });
    let v = rig.version("/front");
    let tex = rig.cook_until_new("/front", v);
    let px = rig.gpu.read(&tex).unwrap();
    assert!(covered(&px, 27, 32) && covered(&px, 69, 32) && !covered(&px, 48, 32), "a square outline");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_count_on_the_gpu_draws_the_live_segments_only() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        create(
            "/line",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(4)), ("size", Value::Vec3([3.0, 0.0, 0.0]))],
        ),
        // The last two go: their count is on the GPU, and their lines are gone with the order.
        create(
            "/two",
            "pts.delete",
            &[("by", "index".into()), ("first", Value::Int(2)), ("last", Value::Int(3))],
        ),
        connect("/line", "/two", 0),
    ];
    ops.extend(lines("/two", 0.25));
    ops.extend([ortho("/front", [0.0, -8.0, 0.0], 2.0), connect("/lit", "/front", 0)]);
    let mut rig = rig(gpu, None, &ops);
    let p = points(&mut rig, "/two");
    assert_eq!((p.strips(), p.count().known()), (Strips::None, None));
    let px = drawn(&mut rig, "/front");
    let (_, b) = coverage(&px);
    // From x -1.5 to -0.5 only: 32 pixels, starting at the picture's left third.
    let (w, h) = extent(b);
    assert!((30..=35).contains(&w) && (7..=9).contains(&h), "{w} x {h} at {b:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn width_and_colour_come_from_each_point() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        create(
            "/line",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(2)), ("size", Value::Vec3([2.0, 0.0, 0.0]))],
        ),
        // TouchDesigner's name for the width.
        create(
            "/wide",
            "pts.math",
            &[("expression", "0.5 + 1.5 * f32(index)".into()), ("result", "LineWidth".into())],
        ),
        connect("/line", "/wide", 0),
        create(
            "/tint",
            "pts.math",
            &[
                ("expression", "vec4f(1.0 - f32(index), f32(index), 0.0, 1.0)".into()),
                ("result", "color".into()),
            ],
        ),
        connect("/wide", "/tint", 0),
    ];
    ops.extend(lines("/tint", 0.2));
    ops.extend([ortho("/front", [0.0, -8.0, 0.0], 2.0), connect("/lit", "/front", 0)]);
    let mut rig = rig(gpu, None, &ops);
    let p = points(&mut rig, "/tint");
    assert!(p.attr("width").is_some() && p.strips() == Strips::regular(2));
    let px = drawn(&mut rig, "/front");
    let column = |x: u32| (0..px.height).filter(|y| px.get(x, *y).is_some_and(|p| p[3] > 0.5)).count();
    // 0.1 m at the left end, 0.4 m at the right: 3 and 13 pixels.
    let (left, right) = (column(18), column(78));
    assert!((2..=5).contains(&left) && (11..=15).contains(&right), "{left} {right}");
    let at = |x: u32| px.get(x, 32).unwrap();
    assert!(at(18)[0] > 0.8 && at(18)[1] < 0.2, "red at the left: {:?}", at(18));
    assert!(at(78)[1] > 0.8 && at(78)[0] < 0.2, "green at the right: {:?}", at(78));
    assert!(at(48)[0] > 0.3 && at(48)[1] > 0.3, "blended between: {:?}", at(48));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_turn_closes_its_outer_corner() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        create("/line", "pts.generate", &[("shape", "line".into()), ("count", Value::Int(3))]),
        // An L: from x -1 to the origin, then up to z 1.
        create(
            "/bend",
            "pts.math",
            &[
                ("expression", "vec3f(min(f32(index), 1.0) - 1.0, 0.0, max(f32(index), 1.0) - 1.0)".into()),
                ("result", "position".into()),
            ],
        ),
        connect("/line", "/bend", 0),
    ];
    ops.extend(lines("/bend", 0.25));
    ops.extend([ortho("/front", [0.0, -8.0, 0.0], 2.0), connect("/lit", "/front", 0)]);
    let mut rig = rig(gpu, None, &ops);
    points(&mut rig, "/bend");
    let px = drawn(&mut rig, "/front");
    let covered = |x: u32, y: u32| px.get(x, y).is_some_and(|p| p[3] > 0.5);
    // The corner outside the turn, below and right of the origin (48, 32): the ends of two plain
    // ribbons would leave it open, a mitred joint fills it.
    assert!(covered(51, 35), "the outer corner is filled");
    assert!(!covered(54, 38), "and no further than the mitre");
    // The inside of the turn is no thicker than the ribbon.
    assert!(!covered(42, 26), "nothing inside the L");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn lines_cast_shadows_of_their_width() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create(
            "/grid",
            "pts.generate",
            &[
                ("rows", Value::Int(3)),
                ("cols", Value::Int(3)),
                ("size", Value::Vec3([2.0, 1.0, 2.0])),
                ("lines", "rows".into()),
            ],
        ),
        // Flat, a metre above the ground.
        create(
            "/flat",
            "pts.transform",
            &[("rotate", Value::Vec3([-90.0, 0.0, 0.0])), ("translate", Value::Vec3([3.0, 3.0, 1.0]))],
        ),
        connect("/grid", "/flat", 0),
        create("/lines", "geo.from_pts", &[("mode", "lines".into()), ("width", Value::Float(0.15))]),
        connect("/flat", "/lines", 0),
        create("/red", "mat.constant", &[("color", Value::Vec4([1.0, 0.0, 0.0, 1.0]))]),
        create("/red_lines", "geo.assign_material", &[]),
        connect("/lines", "/red_lines", 0),
        connect("/red", "/red_lines", 1),
        create("/ground", "geo.primitive", &[("size", Value::Vec3([4.0, 4.0, 0.02]))]),
        create("/ground_at", "geo.transform", &[("translate", Value::Vec3([3.0, 3.0, -0.01]))]),
        connect("/ground", "/ground_at", 0),
        create("/white", "mat.phong", &[]),
        create("/lit_ground", "geo.assign_material", &[]),
        connect("/ground_at", "/lit_ground", 0),
        connect("/white", "/lit_ground", 1),
        create("/scene", "geo.merge", &[]),
        connect("/red_lines", "/scene", 0),
        connect("/lit_ground", "/scene", 1),
        create(
            "/sun",
            "light.directional",
            &[
                ("position", Value::Vec3([5.0, 3.0, 4.0])),
                ("target", Value::Vec3([3.0, 3.0, 0.0])),
                ("shadows", Value::Bool(true)),
            ],
        ),
        render(
            "/down",
            &[
                ("width", Value::Int(192)),
                ("height", Value::Int(192)),
                ("position", Value::Vec3([3.0, 3.0, 6.0])),
                ("target", Value::Vec3([3.0, 3.0, 0.0])),
                ("fov", Value::Float(45.0)),
                ("antialias", Value::Bool(false)),
            ],
        ),
        connect("/scene", "/down", 0),
        connect("/sun", "/down", 2),
    ];
    let mut rig = rig(gpu, None, &ops);
    points(&mut rig, "/flat");
    let px = drawn(&mut rig, "/down");
    let count = |f: &dyn Fn(&[f32; 4]) -> bool| px.data.iter().filter(|p| p[3] > 0.5 && f(p)).count();
    let red = count(&|p| p[0] > 0.5 && p[1] < 0.2);
    let shadow = count(&|p| p[0] < 0.2 && p[1] < 0.2 && p[2] < 0.2);
    // Three ribbons facing the light throw three bands about as wide as the ribbons; ribbons
    // edge-on to the light would throw slivers, and a ribbon in its own shadow would show black.
    assert!(red > 300, "{red}");
    let ratio = shadow as f32 / red as f32;
    assert!((0.4..1.3).contains(&ratio), "{shadow} shadow pixels for {red} line pixels");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The trails as read back: per trail its key (the id where the trail carries one, else its
/// index) and its places, head first.
fn trails(rig: &Rig, p: &Points, len: usize) -> Vec<(u32, Vec<[f32; 3]>)> {
    let n = live(rig, p);
    assert_eq!(n % len, 0, "whole trails");
    let pos = values(rig, p, "position");
    let ids: Vec<u32> =
        if p.attr("id").is_some() { raw_ids(rig, p, n) } else { (0..n).map(|j| (j / len) as u32).collect() };
    (0..n / len)
        .map(|t| {
            let id = ids[t * len];
            assert!(ids[t * len..(t + 1) * len].iter().all(|i| *i == id), "one id along trail {t}");
            (id, (0..len).map(|k| pos[t * len + k]).map(|v| [v[0], v[1], v[2]]).collect())
        })
        .collect()
}

/// A step of the trails on the CPU: the points now (key, place) from the trails before.
fn model(
    before: &HashMap<u32, Vec<[f32; 3]>>,
    now: &[(u32, [f32; 3])],
    len: usize,
    shift: bool,
) -> Vec<(u32, Vec<[f32; 3]>)> {
    now.iter()
        .map(|(key, p)| {
            let places = match before.get(key) {
                None => vec![*p; len],
                Some(old) => {
                    let rest = if shift { &old[..len - 1] } else { &old[1..] };
                    std::iter::once(*p).chain(rest.iter().copied()).collect()
                }
            };
            (*key, places)
        })
        .collect()
}

/// The points of `p` as (key, place): their id, or their index.
/// The first `n` ids as stored: a picture of floats would round ids past 2^24.
fn raw_ids(rig: &Rig, p: &Points, n: usize) -> Vec<u32> {
    let a = p.attr("id").unwrap();
    match a.ty() {
        penumbra_gpu::points::AttrType::U32 => {
            a.buffer().read::<u32>(&rig.gpu).unwrap().into_iter().take(n).collect()
        }
        _ => values(rig, p, "id").iter().map(|v| v[0] as u32).collect(),
    }
}

fn keyed(rig: &Rig, p: &Points) -> Vec<(u32, [f32; 3])> {
    let pos = values(rig, p, "position");
    let ids: Vec<u32> = match p.attr("id") {
        Some(_) => raw_ids(rig, p, pos.len()),
        None => (0..pos.len() as u32).collect(),
    };
    ids.into_iter().zip(pos.iter().map(|v| [v[0], v[1], v[2]])).collect()
}

/// Steps the clock `steps` times and checks the trail at `path` (of `len`, input `source`)
/// against the model each time; `shift` says which steps keep a place.
fn follow(
    rig: &mut Rig,
    path: &str,
    source: &str,
    len: usize,
    steps: usize,
    cpu: &mut HashMap<u32, Vec<[f32; 3]>>,
    shift: &dyn Fn(usize) -> bool,
) {
    for n in 0..steps {
        rig.clock.advance(1);
        rig.cook(path);
        let s = rig.cooker.status(rig.id(path)).unwrap();
        assert_eq!((s.error, s.warning), (None, None), "{path}");
        let input = output(rig, source);
        let out = output(rig, path);
        let want = model(cpu, &keyed(rig, &input), len, shift(n));
        let got = trails(rig, &out, len);
        let close = got.len() == want.len()
            && got.iter().zip(&want).all(|(g, w)| {
                g.0 == w.0
                    && g.1.iter().zip(&w.1).all(|(a, b)| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5))
            });
        assert!(close, "step {n}:\n{got:?}\n{want:?}");
        assert_eq!(out.strips(), Strips::regular(len as u32));
        *cpu = want.into_iter().collect();
    }
}

#[test]
fn a_trail_follows_a_moving_set_by_index_every_step_or_fewer() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create(
            "/line",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(3)), ("size", Value::Vec3([2.0, 0.0, 0.0]))],
        ),
        create(
            "/move",
            "pts.math",
            &[
                ("expression", "position + vec3f(0.0, sin(time * 3.0), time)".into()),
                ("result", "position".into()),
            ],
        ),
        connect("/line", "/move", 0),
        create("/trail", TRAIL, &[("length", Value::Int(5))]),
        connect("/move", "/trail", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    rig.types.get(TRAIL).unwrap().warm_up(&rig.gpu).unwrap();
    points(&mut rig, "/move");
    let id = rig.id("/trail");
    rig.cooker.reset(id);
    let mut cpu = HashMap::new();
    follow(&mut rig, "/trail", "/move", 5, 9, &mut cpu, &|_| true);
    let out = output(&rig, "/trail");
    assert_eq!((out.count().known(), out.dims()), (Some(15), Some([3, 5, 1])));
    assert_eq!(crate::pts::to_tex::picture_size(15, out.dims(), true, 0), (5, 3), "a row per trail");
    let along: Vec<f32> = values(&rig, &out, "trail").iter().map(|v| v[0]).collect();
    assert_eq!(along[5..10], [0.0, 0.25, 0.5, 0.75, 1.0]);

    // Time stands: nothing new.
    let v = rig.version("/trail");
    for _ in 0..3 {
        rig.cook("/trail");
    }
    assert_eq!(rig.version("/trail"), v, "no new result while time stands");

    // Every third step keeps a place, from a fresh start.
    rig.edit(&[set("/trail.every", Value::Int(3))]);
    rig.cooker.reset(id);
    let mut cpu = HashMap::new();
    follow(&mut rig, "/trail", "/move", 5, 10, &mut cpu, &|n| n % 3 == 0);

    // reset: each trail a single place while on.
    rig.edit(&[set("/trail.reset", Value::Bool(true))]);
    let mut cpu = HashMap::new();
    for _ in 0..2 {
        follow(&mut rig, "/trail", "/move", 5, 1, &mut cpu, &|_| true);
        cpu.clear();
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Ids that do not rise along the list (turned around, or repeated by a trail before) keep no
/// trail: the node says so. An attribute of floats to match by is no key: the trails follow
/// the index, with a word.
#[test]
fn ids_that_do_not_rise_are_named() {
    let Some(gpu) = gpu() else { return };
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
                ("rate", Value::Float(600.0)),
                ("velocity", Value::Vec3([1.0, 0.0, 0.0])),
                ("max", Value::Int(500)),
            ],
        ),
        connect("/src", "/p", 0),
        create("/down", "pts.math", &[("expression", "4294967295u - id".into()), ("result", "id".into())]),
        connect("/p", "/down", 0),
        create("/turned", TRAIL, &[("length", Value::Int(4))]),
        connect("/down", "/turned", 0),
        create("/inner", TRAIL, &[("length", Value::Int(4))]),
        connect("/p", "/inner", 0),
        create("/outer", TRAIL, &[("length", Value::Int(4))]),
        connect("/inner", "/outer", 0),
        create("/row", "pts.generate", &[("shape", "line".into()), ("count", Value::Int(50))]),
        create(
            "/float",
            "pts.math",
            &[("expression", "f32(index)".into()), ("result", "tag".into()), ("type", "float".into())],
        ),
        connect("/row", "/float", 0),
        create("/floats", TRAIL, &[("length", Value::Int(4)), ("match", "tag".into())]),
        connect("/float", "/floats", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for kind in [crate::pts::particles::KIND, TRAIL, "pts.math"] {
        rig.types.get(kind).unwrap().warm_up(&rig.gpu).unwrap();
    }
    rig.cooker.set_exact_frames(true);
    let mut said: HashMap<&str, String> = HashMap::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while said.len() < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
        rig.clock.advance(1);
        for path in ["/turned", "/outer", "/floats"] {
            rig.cook(path);
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert_eq!(s.error, None, "{path}");
            if let Some(w) = s.warning {
                said.insert(path, w);
            }
        }
        rig.gpu.poll();
    }
    for path in ["/turned", "/outer"] {
        assert!(said.get(path).is_some_and(|w| w.contains("do not rise")), "{path}: {said:?}");
    }
    assert!(said.get("/floats").is_some_and(|w| w.contains("by their index")), "{said:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Two particle systems merged keep ids apart that rise along the list: one trail node follows
/// both without a word, and every trail stays with its particle.
#[test]
fn merged_particle_systems_trail_as_one() {
    let Some(gpu) = gpu() else { return };
    let system = |path: &str, x: f64| {
        let src = format!("{path}_src");
        vec![
            create(
                &src,
                "pts.generate",
                &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
            ),
            create(
                path,
                crate::pts::particles::KIND,
                &[
                    ("rate", Value::Float(120.0)),
                    ("life", Value::Float(0.1)),
                    ("velocity", Value::Vec3([x, 0.0, 1.0])),
                    ("max", Value::Int(64)),
                ],
            ),
            connect(&src, path, 0),
        ]
    };
    let mut ops = system("/a", 1.0);
    ops.extend(system("/b", -1.0));
    ops.extend([
        create("/m", "pts.merge", &[]),
        connect("/a", "/m", 0),
        connect("/b", "/m", 1),
        create("/trail", TRAIL, &[("length", Value::Int(4))]),
        connect("/m", "/trail", 0),
    ]);
    let mut rig = Rig::new(gpu, None, &ops);
    for kind in [crate::pts::particles::KIND, TRAIL] {
        rig.types.get(kind).unwrap().warm_up(&rig.gpu).unwrap();
    }
    let mut cpu = HashMap::new();
    follow(&mut rig, "/trail", "/m", 4, 16, &mut cpu, &|_| true);
    let ids: Vec<u32> = keyed(&rig, &output(&rig, "/m")).into_iter().map(|(id, _)| id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "rising: {ids:?}");
    assert!(ids.iter().any(|i| *i >= 1 << 31), "the second system's ids moved up: {ids:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Long trails behind many particles would take more memory than a set of trails may: fewer
/// particles get one, and the node says how much it would have taken.
#[test]
fn trails_too_long_for_memory_are_cut_with_a_word() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create(
            "/src",
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(1)), ("size", Value::Vec3([0.0; 3]))],
        ),
        create("/p", crate::pts::particles::KIND, &[("max", Value::Int(40_000))]),
        connect("/src", "/p", 0),
        create("/trail", TRAIL, &[("length", Value::Int(1024))]),
        connect("/p", "/trail", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    rig.clock.advance(1);
    rig.cook("/trail");
    let s = rig.cooker.status(rig.id("/trail")).unwrap();
    assert_eq!(s.error, None);
    assert!(s.warning.as_deref().is_some_and(|w| w.contains("MB")), "{:?}", s.warning);
    // A set of 512 MB at most, the trails given out (no set before at the first step).
    assert!(s.gpu_bytes <= (512 << 20) + (1 << 20), "{}", s.gpu_bytes);
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn trails_of_particles_stay_with_their_particle_as_others_are_born_and_die() {
    let Some(gpu) = gpu() else { return };
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
                ("rate", Value::Float(120.0)),
                ("life", Value::Float(0.1)),
                ("life_variance", Value::Float(0.05)),
                ("velocity", Value::Vec3([2.0, 0.0, 1.0])),
                ("spread", Value::Float(1.0)),
                ("max", Value::Int(64)),
            ],
        ),
        connect("/src", "/p", 0),
        create("/trail", TRAIL, &[("length", Value::Int(4))]),
        connect("/p", "/trail", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for kind in [crate::pts::particles::KIND, TRAIL] {
        rig.types.get(kind).unwrap().warm_up(&rig.gpu).unwrap();
    }
    let mut cpu = HashMap::new();
    follow(&mut rig, "/trail", "/p", 4, 24, &mut cpu, &|_| true);
    let out = output(&rig, "/trail");
    assert_eq!(out.count().known(), None, "the count stays on the GPU");
    let got = trails(&rig, &out, 4);
    assert!(got.len() >= 8, "particles born and dying: {}", got.len());
    // Every particle starts at the origin and moves off at once: a trail's places lie on its own
    // particle's path, in order, and no segment runs back to the origin from a place away from it.
    for (id, places) in &got {
        for pair in places.windows(2) {
            let back = pair[1][0] <= pair[0][0] + 1e-6;
            assert!(back, "trail {id} runs backwards in time along x: {places:?}");
        }
    }
    // The node's reset starts the trails over from the particles' places.
    let id = rig.id("/trail");
    rig.cooker.reset(id);
    let mut cpu = HashMap::new();
    follow(&mut rig, "/trail", "/p", 4, 1, &mut cpu, &|_| true);
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn nodes_that_keep_count_and_order_keep_the_lines() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/ring", "pts.generate", &[("shape", "circle".into()), ("count", Value::Int(12))]),
        create("/noise", "pts.noise", &[]),
        connect("/ring", "/noise", 0),
        create("/move", "pts.transform", &[("translate", Value::Vec3([0.0, 0.0, 1.0]))]),
        connect("/noise", "/move", 0),
        create("/math", "pts.math", &[("expression", "1.0".into()), ("result", "width".into())]),
        connect("/move", "/math", 0),
        create("/random", "pts.random", &[]),
        connect("/math", "/random", 0),
        create("/field", crate::pts::field::KIND, &[]),
        connect("/random", "/field", 0),
        create("/force", crate::pts::force::KIND, &[]),
        connect("/field", "/force", 0),
        create("/picture", "tex.constant", &super::size()),
        create("/look", "pts.lookup_tex", &[]),
        connect("/force", "/look", 0),
        connect("/picture", "/look", 1),
        create("/null", "pts.null", &[]),
        connect("/look", "/null", 0),
        create("/delete", "pts.delete", &[("expression", "index == 3u".into())]),
        connect("/null", "/delete", 0),
        create("/trail", TRAIL, &[("length", Value::Int(3))]),
        connect("/null", "/trail", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let ring = Strips::Regular { len: 12, closed: true };
    for path in ["/ring", "/noise", "/move", "/math", "/random", "/field", "/force", "/look", "/null"] {
        assert_eq!(points(&mut rig, path).strips(), ring, "{path} keeps the ring");
    }
    assert_eq!(points(&mut rig, "/delete").strips(), Strips::None, "a delete drops the lines");
    let t = points(&mut rig, "/trail");
    assert_eq!((t.strips(), t.count().known()), (Strips::regular(3), Some(36)), "a trail makes its own");
    // Random places and an inside fill are no lines; a random grid neither.
    rig.edit(&[set("/ring.distribution", "random".into())]);
    assert_eq!(points(&mut rig, "/ring").strips(), Strips::None);
}
