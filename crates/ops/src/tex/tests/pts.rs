//! `pts` on WARP: the generator's points read back from the GPU, its shapes by the numbers, and
//! `geo.from_pts` drawing sprites that face the camera and copies of a model.

use std::time::{Duration, Instant};

use penumbra_core::Value;
use penumbra_gpu::{Geo, Pixels, Points};
use penumbra_graph::Port;

use super::geo::{render, rig};
use super::{Rig, connect, create, gpu, set};
use crate::pts::generate::{MAX_POINTS, Shape, cloud};

/// Whether a node's compile, if it has one, has finished.
pub(super) fn compiled(s: &penumbra_graph::NodeStatus) -> bool {
    !matches!(s.job, Some(penumbra_graph::JobStatus::Queued | penumbra_graph::JobStatus::Running { .. }))
}

/// The points of `path`, cooked until a program it compiles off the cook thread is there.
pub(super) fn points(rig: &mut Rig, path: &str) -> Points {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        let s = rig.cooker.status(rig.id(path)).unwrap();
        if s.has_output && compiled(&s) || s.error.is_some() {
            assert_eq!((s.error, s.warning), (None, None), "{path}");
            break;
        }
        assert!(Instant::now() < deadline, "{path} did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap().get::<Points>().unwrap().clone()
}

pub(super) fn read(rig: &Rig, p: &Points, name: &str) -> Vec<[f32; 4]> {
    let a = p.attr(name).unwrap_or_else(|| panic!("no {name}"));
    let n = p.count().known().unwrap() as usize;
    match a.ty().components() {
        2 => a.buffer().read::<[f32; 2]>(&rig.gpu).unwrap()[..n]
            .iter()
            .map(|v| [v[0], v[1], 0.0, 0.0])
            .collect(),
        _ => a.buffer().read::<[f32; 4]>(&rig.gpu).unwrap()[..n].to_vec(),
    }
}

#[test]
fn the_generator_puts_its_points_where_the_shape_is() {
    let Some(gpu) = gpu() else { return };
    let ops = [create(
        "/grid",
        "pts.generate",
        &[("rows", Value::Int(3)), ("cols", Value::Int(4)), ("size", Value::Vec3([2.0, 1.0, 1.0]))],
    )];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/grid");
    assert_eq!((p.count().known(), p.dims()), (Some(12), Some([3, 4, 1])));
    let names: Vec<&str> = p.attrs().iter().map(|a| a.name()).collect();
    assert_eq!(names, ["position", "normal", "uv"]);
    let pos = read(&rig, &p, "P");
    // Row by row from the top left, upright, facing -y.
    assert_eq!(pos[0], [-1.0, 0.0, 0.5, 1.0]);
    assert_eq!(pos[3], [1.0, 0.0, 0.5, 1.0]);
    assert_eq!(pos[11], [1.0, 0.0, -0.5, 1.0]);
    assert_eq!(read(&rig, &p, "N")[5], [0.0, -1.0, 0.0, 0.0]);
    assert_eq!(read(&rig, &p, "Tex")[11][..2], [1.0, 1.0]);
    let b = p.bounds().unwrap();
    assert_eq!((b.min, b.max), ([-1.0, 0.0, -0.5], [1.0, 0.0, 0.5]));

    // A sphere's surface: every point half a metre out, its normal pointing there.
    rig.edit(&[
        set("/grid.shape", "sphere".into()),
        set("/grid.count", Value::Int(300)),
        set("/grid.size", Value::Vec3([1.0; 3])),
    ]);
    let p = points(&mut rig, "/grid");
    assert_eq!((p.count().known(), p.dims()), (Some(300), None));
    let (pos, normal) = (read(&rig, &p, "position"), read(&rig, &p, "normal"));
    for (q, n) in pos.iter().zip(&normal) {
        let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
        assert!((r - 0.5).abs() < 1e-5, "{q:?}");
        assert!((q[0] - 0.5 * n[0]).abs() < 1e-5 && (q[2] - 0.5 * n[2]).abs() < 1e-5, "{q:?} {n:?}");
    }
    // Spread over the whole sphere: about as many above as below, and on both sides.
    let above = pos.iter().filter(|q| q[2] > 0.0).count();
    let right = pos.iter().filter(|q| q[0] > 0.0).count();
    assert!((130..=170).contains(&above) && (120..=180).contains(&right), "{above} {right}");

    // At random: the same seed the same points, another seed others.
    rig.edit(&[set("/grid.distribution", "random".into()), set("/grid.fill", "inside".into())]);
    let p = points(&mut rig, "/grid");
    let a = read(&rig, &p, "position");
    rig.edit(&[set("/grid.count", Value::Int(301))]);
    let p = points(&mut rig, "/grid");
    let b = read(&rig, &p, "position");
    assert_eq!(a[..], b[..300], "one more point leaves the others where they were");
    rig.edit(&[set("/grid.seed", Value::Int(7))]);
    let p = points(&mut rig, "/grid");
    let c = read(&rig, &p, "position");
    assert_ne!(a[..10], c[..10]);
    assert!(c.iter().all(|q| q[0] * q[0] + q[1] * q[1] + q[2] * q[2] <= 0.25 + 1e-5), "inside the ball");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn every_shape_makes_what_it_says() {
    let base = Shape {
        shape: "line",
        inside: false,
        random: false,
        count: 5,
        rows: 2,
        cols: 3,
        size: [4.0, 2.0, 1.0],
        seed: 0,
    };
    let line = cloud(&base).unwrap();
    let xs: Vec<f32> = line.position.iter().map(|p| p[0]).collect();
    assert_eq!(xs, [-2.0, -1.0, 0.0, 1.0, 2.0]);
    assert_eq!(line.dims, Some([1, 5, 1]));
    assert!(line.normal.is_none());
    // A ring and a disc of diameter 4, upright.
    let ring = cloud(&Shape { shape: "circle", count: 8, ..base }).unwrap();
    assert!(
        ring.position.iter().all(|p| ((p[0] * p[0] + p[2] * p[2]).sqrt() - 2.0).abs() < 1e-5 && p[1] == 0.0)
    );
    let disc = cloud(&Shape { shape: "circle", count: 400, inside: true, ..base }).unwrap();
    let near = disc.position.iter().filter(|p| p[0] * p[0] + p[2] * p[2] < 1.0).count();
    assert!((80..=120).contains(&near), "a quarter of the area holds a quarter of the points: {near}");
    // An ordered ball fills the ball: the points towards the top lie as far out on average as
    // all of them, three quarters of the radius.
    let ball = cloud(&Shape { shape: "sphere", inside: true, count: 2000, size: [1.0; 3], ..base }).unwrap();
    let radius = |p: &[f32; 4]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    let mean = |ps: &[&[f32; 4]]| ps.iter().map(|p| radius(p)).sum::<f32>() / ps.len() as f32;
    let all: Vec<&[f32; 4]> = ball.position.iter().collect();
    let top: Vec<&[f32; 4]> = ball.position.iter().filter(|p| p[2] > 0.5 * radius(p)).collect();
    assert!((mean(&all) - 0.375).abs() < 0.01, "{}", mean(&all));
    assert!(
        top.len() > 400 && (mean(&top) - 0.375).abs() < 0.02,
        "{} near the top, {} out",
        top.len(),
        mean(&top)
    );
    assert!(ball.position.iter().all(|p| radius(p) <= 0.5 + 1e-5));
    // A box's lattice: about the count asked, its dims multiply to what it made.
    let solid = cloud(&Shape { shape: "box", inside: true, count: 1000, size: [1.0; 3], ..base }).unwrap();
    assert_eq!(solid.dims, Some([10, 10, 10]));
    assert_eq!(solid.position.len(), 1000);
    let shell = cloud(&Shape { shape: "box", count: 600, size: [1.0; 3], ..base }).unwrap();
    assert!(shell.position.len().abs_diff(600) < 120, "{}", shell.position.len());
    let normals = shell.normal.unwrap();
    for (p, n) in shell.position.iter().zip(&normals) {
        let k = n.iter().position(|c| *c != 0.0).unwrap();
        assert!((p[k] - 0.5 * n[k]).abs() < 1e-5, "on the side its normal names: {p:?} {n:?}");
    }
    let random_shell = cloud(&Shape { shape: "box", count: 500, random: true, ..base }).unwrap();
    for (p, n) in random_shell.position.iter().zip(random_shell.normal.unwrap()) {
        let k = n.iter().position(|c| *c != 0.0).unwrap();
        let half = [2.0, 1.0, 0.5][k];
        assert!((p[k] - half * n[k]).abs() < 1e-5, "{p:?} {n:?}");
    }
    // A grid at random keeps rows times cols points and loses its order.
    let scattered = cloud(&Shape { shape: "grid", random: true, ..base }).unwrap();
    assert_eq!((scattered.position.len(), scattered.dims), (6, None));
    assert!(cloud(&Shape { shape: "grid", rows: 4096, cols: 4096, ..base }).is_err());
    assert!(
        cloud(&Shape { shape: "box", inside: true, count: MAX_POINTS, size: [1.0, 0.0, 0.0], ..base })
            .is_ok()
    );
    assert!(cloud(&Shape { shape: "cone", ..base }).is_err());
}

/// Where the opaque pixels of `px` lie: their count and the box around them.
pub(super) fn coverage(px: &Pixels) -> (usize, [u32; 4]) {
    let mut n = 0;
    let mut b = [u32::MAX, u32::MAX, 0, 0];
    for y in 0..px.height {
        for x in 0..px.width {
            if px.get(x, y).is_some_and(|p| p[3] > 0.5) {
                n += 1;
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
    }
    (n, b)
}

pub(super) fn drawn(rig: &mut Rig, path: &str) -> Pixels {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = rig.cooker.status(rig.id(path)).map_or(0, |s| s.version);
        let tex = rig.cook_until_new(path, v);
        let px = rig.gpu.read(&tex).unwrap();
        if px.data.iter().any(|p| p[3] > 0.5) || Instant::now() > deadline {
            return px;
        }
        rig.clock.advance(1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn points_draw_as_sprites_that_face_the_camera_and_as_copies() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create(
            "/grid",
            "pts.generate",
            &[("rows", Value::Int(2)), ("cols", Value::Int(2)), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
        ),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.4)), ("sprite", "square".into())]),
        connect("/grid", "/sprites", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/white", "/lit", 1),
        render("/front", &[("position", Value::Vec3([0.0, -8.0, 0.0])), ("antialias", Value::Bool(false))]),
        connect("/lit", "/front", 0),
        render("/side", &[("position", Value::Vec3([8.0, 0.0, 0.0])), ("antialias", Value::Bool(false))]),
        connect("/lit", "/side", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    let front = drawn(&mut rig, "/front");
    let (n, b) = coverage(&front);
    // Four squares in the corners of a square 2 m wide: covered at the four corners of the box
    // around them, not in its middle.
    assert!(n > 40, "{n}");
    let covered = |x: u32, y: u32| front.get(x, y).is_some_and(|p| p[3] > 0.5);
    assert!(covered(b[0] + 1, b[1] + 1) && covered(b[2] - 1, b[3] - 1), "{b:?}");
    assert!(!covered((b[0] + b[2]) / 2, (b[1] + b[3]) / 2), "nothing between the points");
    // From the side the two columns stand behind each other; flat squares facing -y would be
    // edge on and vanish, sprites turn to the camera.
    let side = drawn(&mut rig, "/side");
    let (m, _) = coverage(&side);
    assert!(m * 3 > n, "the sprites face the side camera too: {m} against {n} from the front");

    // Copies of a box at each point: four boxes, one draw of 12 triangles each.
    rig.edit(&[
        create("/cube", "geo.primitive", &[("size", Value::Vec3([0.3; 3]))]),
        connect("/cube", "/sprites", 1),
        set("/sprites.mode", "copies".into()),
    ]);
    let copies = drawn(&mut rig, "/front");
    let geo = rig
        .cooker
        .output(Port { node: rig.id("/sprites"), output: 0 })
        .unwrap()
        .get::<Geo>()
        .unwrap()
        .clone();
    assert_eq!(geo.triangle_count(), 48, "four copies of a box's 12 triangles");
    let (k, _) = coverage(&copies);
    assert!(k > 40, "{k}");
    let s = rig.cooker.status(rig.id("/sprites")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    // Copies without a model say what is missing.
    rig.edit(&[penumbra_graph::Op::Disconnect { to: "/sprites".parse().unwrap(), input: 1 }]);
    rig.cook("/sprites");
    let s = rig.cooker.status(rig.id("/sprites")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("model")), "{s:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// One point moved to `at`, drawn as a sprite of `size`.
fn one_sprite(at: [f64; 3], size: f64, sprite: &str) -> Vec<penumbra_graph::Op> {
    vec![
        create("/one", "pts.generate", &[("rows", Value::Int(1)), ("cols", Value::Int(1))]),
        create("/moved", "pts.transform", &[("translate", Value::Vec3(at))]),
        connect("/one", "/moved", 0),
        create("/sprite", "geo.from_pts", &[("size", Value::Float(size)), ("sprite", sprite.into())]),
        connect("/moved", "/sprite", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprite", "/lit", 0),
        connect("/white", "/lit", 1),
    ]
}

#[test]
fn square_sprites_stay_square_to_a_camera_looking_down() {
    let Some(gpu) = gpu() else { return };
    let mut ops = one_sprite([0.8, 0.8, 0.0], 0.4, "square");
    ops.extend([
        render(
            "/down",
            &[
                ("width", Value::Int(128)),
                ("height", Value::Int(128)),
                ("position", Value::Vec3([0.0, 0.0, 4.0])),
                ("fov", Value::Float(50.0)),
                ("antialias", Value::Bool(false)),
            ],
        ),
        connect("/lit", "/down", 0),
    ]);
    let mut rig = rig(gpu, None, &ops);
    let px = drawn(&mut rig, "/down");
    let (n, b) = coverage(&px);
    // Square to the picture: the square fills the box around it. Turned by 45 degrees about the
    // view, as a sprite turned towards the eye with the world's up would be, it fills half.
    let filled = n as f32 / ((b[2] - b[0] + 1) * (b[3] - b[1] + 1)) as f32;
    assert!(n > 100 && filled > 0.9, "{n} pixels fill {filled} of their box {b:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn sprites_cast_the_shadow_of_a_disc_facing_the_light() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create(
            "/grid",
            "pts.generate",
            &[("rows", Value::Int(3)), ("cols", Value::Int(3)), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
        ),
        // Flat, a metre above the ground, away from the origin.
        create(
            "/flat",
            "pts.transform",
            &[("rotate", Value::Vec3([-90.0, 0.0, 0.0])), ("translate", Value::Vec3([3.0, 3.0, 1.0]))],
        ),
        connect("/grid", "/flat", 0),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.3))]),
        connect("/flat", "/sprites", 0),
        create("/red", "mat.constant", &[("color", Value::Vec4([1.0, 0.0, 0.0, 1.0]))]),
        create("/red_sprites", "geo.assign_material", &[]),
        connect("/sprites", "/red_sprites", 0),
        connect("/red", "/red_sprites", 1),
        create("/ground", "geo.primitive", &[("size", Value::Vec3([4.0, 4.0, 0.02]))]),
        create("/ground_at", "geo.transform", &[("translate", Value::Vec3([3.0, 3.0, -0.01]))]),
        connect("/ground", "/ground_at", 0),
        create("/white", "mat.phong", &[]),
        create("/lit_ground", "geo.assign_material", &[]),
        connect("/ground_at", "/lit_ground", 0),
        connect("/white", "/lit_ground", 1),
        create("/scene", "geo.merge", &[]),
        connect("/red_sprites", "/scene", 0),
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
    // The ground alone would do for `drawn`: wait for the points first.
    points(&mut rig, "/flat");
    let px = drawn(&mut rig, "/down");
    let count = |f: &dyn Fn(&[f32; 4]) -> bool| px.data.iter().filter(|p| p[3] > 0.5 && f(p)).count();
    let sprites = count(&|p| p[0] > 0.5 && p[1] < 0.2);
    let shadow = count(&|p| p[0] < 0.2 && p[1] < 0.2 && p[2] < 0.2);
    // Nine discs facing the light throw nine ellipses a little longer than the discs; from up
    // here the discs, a metre nearer, look larger. Discs standing edge-on to the light would
    // throw slivers.
    assert!(sprites > 200, "{sprites}");
    let ratio = shadow as f32 / sprites as f32;
    assert!((0.5..1.2).contains(&ratio), "{shadow} shadow pixels for {sprites} sprite pixels");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn sprites_let_go_of_the_model_copies_were_made_of() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create("/grid", "pts.generate", &[("rows", Value::Int(2)), ("cols", Value::Int(2))]),
        // A heavy model with a placement of its own, so the copies bake one of their own.
        create("/ball", "geo.primitive", &[("shape", "sphere".into()), ("segments", Value::Int(256))]),
        create("/ball_at", "geo.transform", &[("translate", Value::Vec3([0.0, 0.0, 0.5]))]),
        connect("/ball", "/ball_at", 0),
        create("/copies", "geo.from_pts", &[("mode", "copies".into())]),
        connect("/grid", "/copies", 0),
        connect("/ball_at", "/copies", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let bytes = |rig: &mut Rig| {
        rig.clock.advance(1);
        rig.cook("/copies");
        let s = rig.cooker.status(rig.id("/copies")).unwrap();
        assert!(s.has_output && s.error.is_none(), "{s:?}");
        s.gpu_bytes
    };
    let copies = bytes(&mut rig);
    assert!(copies > 1 << 20, "{copies}");
    rig.edit(&[set("/copies.mode", "sprites".into())]);
    let sprites = bytes(&mut rig);
    assert!(sprites < copies / 10, "{sprites} bytes after {copies}");
}
