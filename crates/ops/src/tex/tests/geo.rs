//! `geo` nodes and `tex.render` on WARP: golden pictures of a three-part scene, the data pass,
//! a calibrated camera rendering where `calib` projects, mesh files written from code; materials
//! and lights by golden picture and by the numbers.

use std::path::Path;
use std::time::{Duration, Instant};

use penumbra_calib::{Camera, Intrinsics, Pose};
use penumbra_core::{Table, Value};
use penumbra_gpu::{Geo, Pixels, Tex};
use penumbra_graph::Port;
use penumbra_mesh::{prim, write};

use super::{Rig, connect, create, golden, gpu, scratch, set};

/// Writes larger pictures to `PENUMBRA_LOOK` (a directory), for looking at by eye; then
/// renders at `back` again.
fn look(rig: &mut Rig, path: &str, name: &str, back: (i64, i64)) {
    let Some(dir) = std::env::var_os("PENUMBRA_LOOK") else { return };
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    rig.edit(&[
        set(&format!("{path}.width"), Value::Int(960)),
        set(&format!("{path}.height"), Value::Int(540)),
    ]);
    let v = rig.version(path);
    let tex = rig.cook_until_new(path, v);
    std::fs::write(dir.join(format!("{name}.png")), rig.gpu.read(&tex).unwrap().to_png().unwrap()).unwrap();
    rig.edit(&[
        set(&format!("{path}.width"), Value::Int(back.0)),
        set(&format!("{path}.height"), Value::Int(back.1)),
    ]);
    let v = rig.version(path);
    rig.cook_until_new(path, v);
}

/// A box, a sphere to its right and a wall behind them.
fn scene() -> Vec<penumbra_graph::Op> {
    vec![
        create("/box", "geo.primitive", &[("size", Value::Vec3([1.0, 1.0, 1.0]))]),
        create("/ball", "geo.primitive", &[("shape", "sphere".into()), ("size", Value::Vec3([1.0; 3]))]),
        create("/ball_at", "geo.transform", &[("translate", Value::Vec3([1.3, -0.3, 0.0]))]),
        create(
            "/wall",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([5.0, 1.0, 3.0]))],
        ),
        create("/wall_at", "geo.transform", &[("translate", Value::Vec3([0.4, 1.0, 0.5]))]),
        create("/scene", "geo.merge", &[]),
        connect("/ball", "/ball_at", 0),
        connect("/wall", "/wall_at", 0),
        connect("/box", "/scene", 0),
        connect("/ball_at", "/scene", 1),
        connect("/wall_at", "/scene", 2),
    ]
}

fn render(path: &str, params: &[(&str, Value)]) -> penumbra_graph::Op {
    let mut p: Vec<(&str, Value)> = super::size().to_vec();
    p.extend(params.iter().cloned());
    create(path, "tex.render", &p)
}

fn data(rig: &Rig, path: &str) -> Pixels {
    let tex =
        rig.cooker.output(Port { node: rig.id(path), output: 1 }).unwrap().get::<Tex>().unwrap().clone();
    rig.gpu.read(&tex).unwrap()
}

fn at(px: &Pixels, x: u32, y: u32) -> [f32; 4] {
    px.data[(y * px.width + x) as usize]
}

#[test]
fn a_scene_of_parts_renders_with_depth_and_a_data_pass() {
    let Some(gpu) = gpu() else { return };
    let mut ops = scene();
    ops.push(render("/parts", &[("surface", "parts".into())]));
    ops.push(connect("/scene", "/parts", 0));
    ops.push(create(
        "/noise",
        "tex.noise",
        &[("color", Value::Bool(true)), ("frequency", Value::Float(3.0))],
    ));
    ops.push(render("/textured", &[]));
    ops.push(connect("/scene", "/textured", 0));
    ops.push(connect("/noise", "/textured", 1));
    // The scene in phong, warm from the upper left, a cool spot from the right, a little
    // ambient.
    ops.extend([
        create("/phong", "mat.phong", &[("color", Value::Vec4([0.9, 0.85, 0.8, 1.0]))]),
        create("/shiny", "geo.material", &[]),
        connect("/scene", "/shiny", 0),
        connect("/phong", "/shiny", 1),
        create(
            "/sun",
            "light.directional",
            &[("position", Value::Vec3([-3.0, -3.0, 4.0])), ("color", Value::Vec4([1.0, 0.9, 0.75, 1.0]))],
        ),
        create(
            "/spot",
            "light.spot",
            &[
                ("position", Value::Vec3([3.0, -2.0, 1.5])),
                ("target", Value::Vec3([0.5, 1.0, 0.5])),
                ("angle", Value::Float(30.0)),
                ("color", Value::Vec4([0.3, 0.5, 1.0, 1.0])),
                ("intensity", Value::Float(1.5)),
            ],
        ),
        create("/fill", "light.ambient", &[("intensity", Value::Float(0.15))]),
        create("/lights", "light.merge", &[]),
        connect("/sun", "/lights", 0),
        connect("/spot", "/lights", 1),
        connect("/fill", "/lights", 2),
    ]);
    ops.push(render("/lit", &[]));
    ops.push(connect("/shiny", "/lit", 0));
    ops.push(connect("/lights", "/lit", 2));
    let mut rig = Rig::new(gpu, None, &ops);
    for name in ["parts", "textured", "lit"] {
        let px = rig.pixels(&format!("/{name}"));
        golden(&format!("render_{name}"), &px);
        look(&mut rig, &format!("/{name}"), &format!("render_{name}"), (super::W, super::H));
    }
    // The merge numbers the parts on: box 0, sphere 1, wall 2.
    let scene =
        rig.cooker.output(Port { node: rig.id("/scene"), output: 0 }).unwrap().get::<Geo>().unwrap().clone();
    assert_eq!(scene.part_names(), ["box", "sphere", "plane"]);
    assert!(scene.preview.is_some());
    // The data pass: the box (part id 0, stored as 1) in front of the wall, nothing in the top
    // corner? The wall covers it; the bottom left corner sees nothing.
    rig.cook("/parts");
    let d = data(&rig, "/parts");
    let ids: std::collections::BTreeSet<u32> = d.data.iter().map(|p| p[2].round() as u32).collect();
    assert_eq!(ids, [0, 1, 2, 3].into());
    // Depth grows from the box to the wall behind it.
    let box_depth = d.data.iter().filter(|p| p[2].round() as u32 == 1).map(|p| p[3]).fold(f32::MAX, f32::min);
    let wall_depth = d.data.iter().filter(|p| p[2].round() as u32 == 3).map(|p| p[3]).fold(0.0, f32::max);
    assert!(box_depth > 3.0 && box_depth < wall_depth, "{box_depth} {wall_depth}");
    // Where nothing is drawn, everything is 0.
    assert!(d.data.iter().filter(|p| p[2] == 0.0).all(|p| *p == [0.0; 4]));
}

#[test]
fn a_calibrated_camera_renders_where_calib_projects() {
    let Some(gpu) = gpu() else { return };
    // A projector with lens shift (principal point near the bottom), 4 m in front of a box.
    let mut intrinsics = Intrinsics::from_throw_ratio([1920, 1080], 1.2);
    intrinsics.principal = [960.0, 1000.0];
    let pose = Pose::look_at([0.8, -4.0, 1.2], [0.0, 0.0, 0.6], [0.0, 0.0, 1.0]).unwrap();
    let camera = Camera { intrinsics, pose };
    let json = serde_json::to_string(&camera).unwrap();
    let mut ops = scene();
    // Rendered at a quarter of the calibration's size: the camera is scaled to it.
    ops.push(create(
        "/projector",
        "tex.render",
        &[
            ("width", Value::Int(480)),
            ("height", Value::Int(270)),
            ("camera", json.as_str().into()),
            ("surface", "parts".into()),
        ],
    ));
    ops.push(connect("/scene", "/projector", 0));
    let mut rig = Rig::new(gpu, None, &ops);
    rig.pixels("/projector");
    look(&mut rig, "/projector", "render_projector", (480, 270));
    let d = data(&rig, "/projector");
    // Points on the box's top face and on the wall land where calib projects them, with the
    // depth calib computes. (The lens shift puts the picture above the axis, so the box's front
    // is below it.)
    let mut checked = 0;
    for (point, part) in [
        ([0.1, -0.2, 0.5], 1.0),
        ([-0.2, 0.2, 0.5], 1.0),
        ([-1.0, 1.0, 1.5], 3.0),
        ([1.5, 1.0, 1.0], 3.0),
        ([0.4, 1.0, 1.9], 3.0),
    ] {
        let [u, v] = camera.project(point).unwrap();
        assert!((0.0..1920.0).contains(&u) && (0.0..1080.0).contains(&v), "{point:?} at {u}, {v}");
        let (x, y) = ((u / 4.0) as u32, (v / 4.0) as u32);
        let p = at(&d, x, y);
        assert_eq!(p[2], part, "{point:?} at {x}, {y}: {p:?}");
        let depth = camera.depth(point) as f32;
        assert!((p[3] - depth).abs() < 0.02 * depth, "{point:?}: depth {} vs {depth}", p[3]);
        checked += 1;
    }
    assert_eq!(checked, 5);
    // A camera that is not one: an error naming the param.
    rig.edit(&[set("/projector.camera", "{\"nope\": 1}".into())]);
    rig.cook("/projector");
    let s = rig.cooker.status(rig.id("/projector")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("`camera`")), "{s:?}");
}

#[test]
fn a_camera_node_replaces_the_renders_own_camera() {
    let Some(gpu) = gpu() else { return };
    let look_from = [
        ("position", Value::Vec3([1.5, -4.0, 2.0])),
        ("target", Value::Vec3([0.3, 0.0, 0.4])),
        ("fov", Value::Float(50.0)),
        ("roll", Value::Float(30.0)),
    ];
    let mut ops = scene();
    // The same camera as the render's own params and as a camera node wired into another.
    ops.push(render("/own", &[&look_from[..], &[("surface", "parts".into())]].concat()));
    ops.push(render("/wired", &[("surface", "parts".into())]));
    ops.push(create("/cam", "view.camera", &look_from));
    ops.push(connect("/scene", "/own", 0));
    ops.push(connect("/scene", "/wired", 0));
    ops.push(connect("/cam", "/wired", 3));
    let mut rig = Rig::new(gpu, None, &ops);
    rig.pixels("/own");
    rig.pixels("/wired");
    let (own, wired) = (data(&rig, "/own"), data(&rig, "/wired"));
    assert!(own.data.iter().any(|p| p[2] == 3.0), "the wall is in the picture");
    assert!(own.data == wired.data, "the wired render sees as the one with its own params");
    // The roll is in the picture: unrolled, the render's own params see differently.
    rig.edit(&[set("/own.roll", Value::Float(0.0))]);
    let v = rig.version("/own");
    rig.cook_until_new("/own", v);
    assert!(data(&rig, "/own").data != wired.data);
    // Moving the camera node moves the wired render.
    let v = rig.version("/wired");
    rig.edit(&[set("/cam.position", Value::Vec3([-1.5, -4.0, 2.0]))]);
    rig.cook_until_new("/wired", v);
    assert!(data(&rig, "/wired").data != wired.data);
    // A calibrated camera on the camera node, as calib projects it.
    let mut intrinsics = Intrinsics::from_throw_ratio([1920, 1080], 1.2);
    intrinsics.principal = [960.0, 1000.0];
    let pose = Pose::look_at([0.8, -4.0, 1.2], [0.0, 0.0, 0.6], [0.0, 0.0, 1.0]).unwrap();
    let camera = Camera { intrinsics, pose };
    let json = serde_json::to_string(&camera).unwrap();
    let v = rig.version("/wired");
    rig.edit(&[set("/cam.camera", json.as_str().into())]);
    let tex = rig.cook_until_new("/wired", v);
    let d = data(&rig, "/wired");
    let point = [0.1, -0.2, 0.5];
    let [u, vv] = camera.project(point).unwrap();
    let (x, y) = (u * f64::from(tex.width()) / 1920.0, vv * f64::from(tex.height()) / 1080.0);
    let p = at(&d, x as u32, y as u32);
    assert_eq!(p[2], 1.0, "the box's top at {u}, {vv}: {p:?}");
    // A camera node that fails leaves the render with an error, not its own params.
    let v = rig.version("/wired");
    rig.edit(&[set("/cam.camera", "{\"nope\": 1}".into())]);
    rig.cook("/wired");
    let s = rig.cooker.status(rig.id("/cam")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("`camera`")), "{s:?}");
    // Unwired, the render's own params count again.
    rig.edit(&[penumbra_graph::Op::Disconnect { to: "/wired".parse().unwrap(), input: 3 }]);
    rig.cook_until_new("/wired", v);
    let s = rig.cooker.status(rig.id("/wired")).unwrap();
    assert!(s.error.is_none(), "{s:?}");
}

#[test]
fn mesh_files_load_with_their_parts_and_reload() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("geo-files");
    let mut red = prim::cuboid([1.0, 1.0, 1.0]).unwrap();
    red.materials = vec![penumbra_mesh::Material::plain("red", [0.8, 0.05, 0.05, 1.0])];
    red.parts = vec!["body".into()];
    let mut head = prim::moved(prim::sphere(0.45, 24).unwrap(), [0.0, 0.0, 0.95]);
    head.parts = vec!["head".into()];
    let model = write::combine(&[red, head]);
    std::fs::write(dir.join("figure.obj"), write::to_obj(&model, None)).unwrap();
    std::fs::write(dir.join("figure.stl"), write::to_stl(&model)).unwrap();
    std::fs::write(dir.join("figure.glb"), write::to_glb(&model)).unwrap();
    std::fs::write(dir.join("broken.glb"), &write::to_glb(&model)[..200]).unwrap();
    let ops = vec![
        create("/obj", "geo.file", &[("file", "figure.obj".into())]),
        create("/stl", "geo.file", &[("file", "figure.stl".into())]),
        create("/glb", "geo.file", &[("file", "figure.glb".into())]),
        create("/broken", "geo.file", &[("file", "broken.glb".into())]),
        render("/view", &[("surface", "material".into())]),
        connect("/glb", "/view", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let geo = |rig: &mut Rig, path: &str| -> (Geo, Table) {
        let id = rig.id(path);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            rig.cooker.cook(&rig.net, rig.clock.now(), &[id], &[]);
            if let (Some(g), Some(t)) = (
                rig.cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Geo>().cloned()),
                rig.cooker.output(Port { node: id, output: 1 }).and_then(|d| d.get::<Table>().cloned()),
            ) {
                return (g, t);
            }
            assert!(std::time::Instant::now() < deadline, "{path}: {:?}", rig.cooker.status(id));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    for path in ["/obj", "/glb"] {
        let (g, t) = geo(&mut rig, path);
        assert_eq!(g.part_names(), ["body", "head"], "{path}");
        assert_eq!(g.triangle_count(), model.triangle_count());
        assert_eq!(t.rows()[0][..3], ["id", "name", "triangles"]);
        assert_eq!(t.rows()[1][..3], ["0", "body", "12"]);
        let s = rig.cooker.status(rig.id(path)).unwrap();
        assert_eq!((s.error, s.warning), (None, None), "{path}");
    }
    let (g, _) = geo(&mut rig, "/stl");
    assert_eq!(g.part_names(), ["figure"]);
    let px = rig.pixels("/view");
    golden("render_glb_material", &px);
    look(&mut rig, "/view", "render_glb_material", (super::W, super::H));
    if let Some(dir) = std::env::var_os("PENUMBRA_LOOK") {
        let prev = geo(&mut rig, "/glb").0.preview.unwrap();
        std::fs::write(
            Path::new(&dir).join("geo_preview.png"),
            rig.gpu.read(&prev).unwrap().to_png().unwrap(),
        )
        .unwrap();
    }
    // A broken file is an error, not a crash, and names the file.
    let id = rig.id("/broken");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let error = loop {
        rig.cooker.cook(&rig.net, rig.clock.now(), &[id], &[]);
        if let Some(e) = rig.cooker.status(id).unwrap().error {
            break e;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(error.contains("broken.glb"), "{error}");
    // Rewriting the file reloads it: now only the head.
    let mut only_head = prim::sphere(0.5, 12).unwrap();
    only_head.parts = vec!["head".into()];
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(dir.join("figure.obj"), write::to_obj(&only_head, None)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (g, _) = geo(&mut rig, "/obj");
        if g.part_names() == ["head"] {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no reload");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A wall 2 m square seen head on by two cameras sharing one texture, the near one twice as
/// close: what each asks of the texture, the most of both as its need, the overlay per pixel.
#[test]
fn render_nodes_ask_of_their_texture_what_their_cameras_see() {
    use crate::out::{BoardState, DisplayInfo, ProjectorState};
    use crate::res::{Scene, Verdict, analyze};
    use crate::tex::render::Renders;
    let Some(gpu) = gpu() else { return };
    // At this distance the wall fills half the height of a 40 degree view.
    let far = 2.0 / 20f64.to_radians().tan();
    let camera = |path: &str, y: f64| {
        create(
            path,
            "tex.render",
            &[
                ("width", Value::Int(192)),
                ("height", Value::Int(108)),
                ("position", Value::Vec3([0.0, -y, 0.0])),
                ("fov", Value::Float(40.0)),
            ],
        )
    };
    let dims = |w: i64, h: i64| [("width", Value::Int(w)), ("height", Value::Int(h))];
    let mut rig = Rig::new(
        gpu,
        None,
        &[
            create(
                "/wall",
                "geo.primitive",
                &[("shape", "plane".into()), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
            ),
            create("/tex", "tex.constant", &dims(32, 32)),
            camera("/far", far),
            camera("/near", far / 2.0),
            create("/p_far", "out.projector", &[]),
            create("/p_near", "out.projector", &[]),
            connect("/wall", "/far", 0),
            connect("/tex", "/far", 1),
            connect("/wall", "/near", 0),
            connect("/tex", "/near", 1),
            connect("/far", "/p_far", 0),
            // The near one through a mapper (one to one): the render draws the overlay, not it.
            create("/m", "tex.mapper", &dims(192, 108)),
            connect("/near", "/m", 0),
            connect("/m", "/p_near", 0),
        ],
    );
    let renders = Renders::new();
    rig.cooker.provide(renders.clone());
    let shown = ProjectorState { display: Some("d".into()), ..ProjectorState::default() };
    let board = BoardState {
        managed: true,
        displays: vec![DisplayInfo {
            id: "d".into(),
            monitor: "test".into(),
            port: "0".into(),
            connector: String::new(),
            os_name: String::new(),
            size: [192, 108],
            position: [0, 0],
            refresh_hz: None,
            editor: false,
        }],
        projectors: [(rig.id("/p_far"), shown.clone()), (rig.id("/p_near"), shown)].into(),
        ..BoardState::default()
    };
    let analysis = |rig: &Rig| {
        analyze(&Scene {
            net: &rig.net,
            types: &rig.types,
            cooker: &rig.cooker,
            board: Some(&board),
            max_side: 16384,
        })
    };
    // Cooks until both fields are in (they come back without waiting, after a poll).
    let settle = |rig: &mut Rig| {
        let ids = [rig.id("/p_far"), rig.id("/p_near")];
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
            rig.clock.advance(1);
            rig.gpu.poll();
            renders.collect();
            let report = analysis(rig);
            if report.pending.is_empty() && !report.nodes.is_empty() {
                return report;
            }
            assert!(Instant::now() < deadline, "the fields did not arrive: {:?}", report.pending);
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Inactive: nothing measures, the texture has no need yet.
    rig.cooker.cook(&rig.net, rig.clock.now(), &[rig.id("/p_far"), rig.id("/p_near")], &[]);
    let before = analysis(&rig);
    let mut pending = before.pending.clone();
    pending.sort();
    assert_eq!(pending, ["/far", "/near"], "{before:?}");
    assert!(before.nodes.iter().all(|n| n.path != "/tex"), "{before:?}");

    renders.set_active(true);
    let report = settle(&mut rig);
    let close =
        |a: [f64; 2], b: [f64; 2]| (a[0] / b[0] - 1.0).abs() < 0.03 && (a[1] / b[1] - 1.0).abs() < 0.03;
    // The far camera sees the wall 54 pixels square, the near one 108.
    let (f, _) = renders.field(rig.id("/far")).unwrap();
    let (n, current) = renders.field(rig.id("/near")).unwrap();
    assert!(current && f.covered() > 2000, "{}", f.covered());
    let df = f.demand([192.0, 108.0]).unwrap();
    let dn = n.demand([192.0, 108.0]).unwrap();
    assert!(close(df, [54.0, 54.0]), "{df:?}");
    assert!(close(dn, [108.0, 108.0]), "{dn:?}");
    // Shown at half its size, a picture asks half.
    assert!(close(n.demand([96.0, 54.0]).unwrap(), [54.0, 54.0]));
    let node = |r: &crate::res::Report, p: &str| r.nodes.iter().find(|n| n.path == p).unwrap().clone();
    let t = node(&report, "/tex");
    assert!((107..=111).contains(&t.needed[0]) && (107..=111).contains(&t.needed[1]), "{t:?}");
    assert_eq!((t.verdict, t.suggested), (Verdict::Coarse, Some([128, 128])), "{t:?}");
    assert_eq!(node(&report, "/near").verdict, Verdict::Ok);

    // The overlay, per render node: both red on a texture far too coarse.
    let centre = |rig: &Rig, p: &str| {
        let px = rig.gpu.read(&rig.tex(p).unwrap()).unwrap();
        px.get(px.width / 2, px.height / 2).unwrap()
    };
    let is_red = |c: [f32; 4]| c[0] > 0.2 && c[0] > 4.0 * c[1].max(c[2]);
    let is_green = |c: [f32; 4]| c[1] > 0.2 && c[1] > 2.0 * c[0].max(c[2]);
    let is_blue = |c: [f32; 4]| c[2] > 0.2 && c[2] > 2.0 * c[0].max(c[1]);
    assert!(report.overlays.contains_key(&rig.id("/far")) && report.overlays.contains_key(&rig.id("/near")));
    assert!(!report.overlays.contains_key(&rig.id("/m")), "the render behind it draws");
    renders.set_overlays(report.overlays.clone());
    settle(&mut rig);
    assert!(is_red(centre(&rig, "/near")), "{:?}", centre(&rig, "/near"));
    assert!(is_red(centre(&rig, "/far")), "{:?}", centre(&rig, "/far"));
    // Grown to the suggestion: about right for the near camera, twice what the far one shows.
    rig.edit(&[set("/tex.width", Value::Int(128)), set("/tex.height", Value::Int(128))]);
    let after = settle(&mut rig);
    assert_eq!(node(&after, "/tex").verdict, Verdict::Ok);
    renders.set_overlays(after.overlays.clone());
    settle(&mut rig);
    assert!(is_green(centre(&rig, "/near")), "{:?}", centre(&rig, "/near"));
    assert!(is_blue(centre(&rig, "/far")), "{:?}", centre(&rig, "/far"));
    // Moving a camera makes its field outdated until it is measured again.
    rig.edit(&[set("/far.position", Value::Vec3([0.5, -far, 0.0]))]);
    rig.cooker.cook(&rig.net, rig.clock.now(), &[rig.id("/p_far")], &[]);
    assert_eq!(renders.field(rig.id("/far")).map(|(_, c)| c), Some(false));
    settle(&mut rig);
    assert_eq!(renders.field(rig.id("/far")).map(|(_, c)| c), Some(true));
}

/// A wall 2 m square facing -y, seen head on from 4 m: the centre pixel's colour under each
/// material and light, against what the shading model says.
#[test]
fn materials_answer_lights_by_the_numbers() {
    let Some(gpu) = gpu() else { return };
    let wall = create(
        "/wall",
        "geo.primitive",
        &[("shape", "plane".into()), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
    );
    let cam = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(40.0))])
    };
    let dir =
        |path: &str, from: [f64; 3]| create(path, "light.directional", &[("position", Value::Vec3(from))]);
    let mut ops = vec![
        wall,
        create("/flat_mat", "mat.phong", &[("specular", Value::Vec4([0.0, 0.0, 0.0, 1.0]))]),
        create("/flat", "geo.material", &[]),
        connect("/wall", "/flat", 0),
        connect("/flat_mat", "/flat", 1),
        create("/shiny_mat", "mat.phong", &[("specular", Value::Vec4([0.5, 0.5, 0.5, 1.0]))]),
        create("/shiny", "geo.material", &[]),
        connect("/wall", "/shiny", 0),
        connect("/shiny_mat", "/shiny", 1),
        create(
            "/green",
            "tex.constant",
            &[
                ("width", Value::Int(4)),
                ("height", Value::Int(4)),
                ("color", Value::Vec4([0.0, 1.0, 0.0, 1.0])),
            ],
        ),
        create("/red_mat", "mat.constant", &[("color", Value::Vec4([1.0, 0.0, 0.0, 1.0]))]),
        create("/red", "geo.material", &[]),
        connect("/wall", "/red", 0),
        connect("/red_mat", "/red", 1),
        create("/tex_mat", "mat.constant", &[]),
        connect("/green", "/tex_mat", 0),
        create("/textured", "geo.material", &[]),
        connect("/wall", "/textured", 0),
        connect("/tex_mat", "/textured", 1),
        // Lights: head on, from 60 degrees, ambient, a point at 2 m fading, two halves merged.
        dir("/head_on", [0.0, -5.0, 0.0]),
        dir("/slanted", [0.0, -1.0, 3f64.sqrt()]),
        create("/ambient", "light.ambient", &[("intensity", Value::Float(0.2))]),
        create(
            "/point",
            "light.point",
            &[("position", Value::Vec3([0.0, -2.0, 0.0])), ("falloff", "inverse square".into())],
        ),
        create("/half_a", "light.ambient", &[("intensity", Value::Float(0.5))]),
        create("/half_b", "light.ambient", &[("intensity", Value::Float(0.5))]),
        create("/both", "light.merge", &[]),
        connect("/half_a", "/both", 0),
        connect("/half_b", "/both", 3),
        // A narrow spot: the centre lit, a corner of the wall outside its cone.
        create(
            "/spot",
            "light.spot",
            &[
                ("position", Value::Vec3([0.0, -3.0, 0.0])),
                ("angle", Value::Float(30.0)),
                ("softness", Value::Float(0.0)),
            ],
        ),
    ];
    // (render, geo, lights)
    let cases: [(&str, &str, Option<&str>); 11] = [
        ("/plain", "/wall", None),
        ("/plain_lit", "/wall", Some("/head_on")),
        ("/dark", "/flat", None),
        ("/head", "/flat", Some("/head_on")),
        ("/slant", "/flat", Some("/slanted")),
        ("/amb", "/flat", Some("/ambient")),
        ("/near", "/flat", Some("/point")),
        ("/merged", "/flat", Some("/both")),
        ("/highlight", "/shiny", Some("/head_on")),
        ("/cone", "/flat", Some("/spot")),
        ("/red_only", "/red", Some("/head_on")),
    ];
    for (path, geo, lights) in cases {
        ops.push(cam(path));
        ops.push(connect(geo, path, 0));
        if let Some(l) = lights {
            ops.push(connect(l, path, 2));
        }
    }
    ops.push(cam("/green_tex"));
    ops.push(connect("/textured", "/green_tex", 0));
    let mut rig = Rig::new(gpu, None, &ops);
    let centre = |rig: &mut Rig, path: &str| {
        let px = rig.pixels(path);
        at(&px, px.width / 2, px.height / 2)
    };
    let near = |got: [f32; 4], want: [f32; 3], path: &str| {
        let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.02) && (got[3] - 1.0).abs() < 1e-3;
        assert!(ok, "{path}: {got:?}, want {want:?}");
    };
    // No material: the file's colour as it is, lights or not.
    near(centre(&mut rig, "/plain"), [1.0; 3], "/plain");
    near(centre(&mut rig, "/plain_lit"), [1.0; 3], "/plain_lit");
    // Phong: black without a light, then n.l of each light.
    near(centre(&mut rig, "/dark"), [0.0; 3], "/dark");
    near(centre(&mut rig, "/head"), [1.0; 3], "/head");
    near(centre(&mut rig, "/slant"), [0.5; 3], "/slant");
    near(centre(&mut rig, "/amb"), [0.2; 3], "/amb");
    near(centre(&mut rig, "/near"), [0.25; 3], "/near");
    near(centre(&mut rig, "/merged"), [1.0; 3], "/merged");
    // The highlight adds the specular colour where the light reflects into the camera.
    near(centre(&mut rig, "/highlight"), [1.5; 3], "/highlight");
    // A constant material's colour, unlit; its texture input replaces the file's.
    near(centre(&mut rig, "/red_only"), [1.0, 0.0, 0.0], "/red_only");
    near(centre(&mut rig, "/green_tex"), [0.0, 1.0, 0.0], "/green_tex");
    // The spot: 15 degrees half angle from 3 m reaches 0.8 m; the wall's corner (1.4 m off
    // the axis) is dark, the centre lit.
    let px = rig.pixels("/cone");
    near(at(&px, px.width / 2, px.height / 2), [1.0; 3], "/cone centre");
    let f = 0.5 * px.height as f64 / 20f64.to_radians().tan();
    let corner = |x: f64, z: f64| {
        let u = (0.5 * px.width as f64 + f * x / 4.0) as u32;
        let v = (0.5 * px.height as f64 - f * z / 4.0) as u32;
        at(&px, u, v)
    };
    near(corner(0.95, 0.95), [0.0; 3], "/cone corner");
    near(corner(0.5, 0.0), [1.0; 3], "/cone inside");
    // The materials' pictures, for looking at by eye.
    if let Some(dir) = std::env::var_os("PENUMBRA_LOOK") {
        for path in ["/shiny_mat", "/red_mat", "/tex_mat"] {
            rig.cook(path);
            let out = rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap();
            let prev = out.get::<penumbra_gpu::Mat>().unwrap().preview.clone().unwrap();
            let name = format!("mat_{}.png", path.trim_start_matches('/'));
            std::fs::write(Path::new(&dir).join(name), rig.gpu.read(&prev).unwrap().to_png().unwrap())
                .unwrap();
        }
    }
    // More than the render draws: a warning, not an error.
    let mut many = vec![create("/all", "light.merge", &[])];
    for k in 0..4 {
        let m = format!("/m{k}");
        many.push(create(&m, "light.merge", &[]));
        for i in 0..4 {
            let a = format!("/a{k}{i}");
            many.push(create(&a, "light.ambient", &[("intensity", Value::Float(0.01))]));
            many.push(connect(&a, &m, i));
        }
        many.push(connect(&m, "/all", k));
    }
    many.push(create("/extra", "light.ambient", &[]));
    many.push(create("/over", "light.merge", &[]));
    many.push(connect("/all", "/over", 0));
    many.push(connect("/extra", "/over", 1));
    rig.edit(&many);
    rig.edit(&[connect("/over", "/amb", 2)]);
    let v = rig.version("/amb");
    rig.cook_until_new("/amb", v);
    let s = rig.cooker.status(rig.id("/amb")).unwrap();
    assert!(s.warning.as_deref().is_some_and(|w| w.contains("17 lights")), "{s:?}");
    // geo.material without a material names what is missing.
    rig.edit(&[create("/bare", "geo.material", &[]), connect("/wall", "/bare", 0)]);
    rig.cook("/bare");
    let s = rig.cooker.status(rig.id("/bare")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("material")), "{s:?}");
}

/// A material replaces the file's colour; without one the file's shows.
#[test]
fn a_material_replaces_the_files_colour() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("geo-material-over-file");
    let mut red = prim::cuboid([1.0, 1.0, 1.0]).unwrap();
    red.materials = vec![penumbra_mesh::Material::plain("red", [0.8, 0.05, 0.05, 1.0])];
    std::fs::write(dir.join("red.glb"), write::to_glb(&red)).unwrap();
    let cam = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(40.0))])
    };
    let ops = vec![
        create("/file", "geo.file", &[("file", "red.glb".into())]),
        create("/blue_mat", "mat.constant", &[("color", Value::Vec4([0.0, 0.0, 1.0, 1.0]))]),
        create("/blue", "geo.material", &[]),
        connect("/file", "/blue", 0),
        connect("/blue_mat", "/blue", 1),
        cam("/as_file"),
        connect("/file", "/as_file", 0),
        cam("/as_mat"),
        connect("/blue", "/as_mat", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let centre = |rig: &mut Rig, path: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let px = rig.pixels(path);
            let c = at(&px, px.width / 2, px.height / 2);
            if c[3] > 0.5 || std::time::Instant::now() > deadline {
                return c;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let near = |got: [f32; 4], want: [f32; 3], path: &str| {
        let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.02);
        assert!(ok, "{path}: {got:?}, want {want:?}");
    };
    near(centre(&mut rig, "/as_file"), [0.8, 0.05, 0.05], "/as_file");
    near(centre(&mut rig, "/as_mat"), [0.0, 0.0, 1.0], "/as_mat");
}

/// A projector light throws its texture where its camera looks; directional, spot and projector
/// lights cast shadows, and what is transparent casts none.
#[test]
fn projectors_throw_their_picture_and_lights_cast_shadows() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("geo-projector");
    super::write_png16(&dir.join("ramp.png"));
    let cam = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(40.0))])
    };
    let white = |path: &str, geo: &str| {
        vec![
            create(&format!("{path}_mat"), "mat.phong", &[("specular", Value::Vec4([0.0, 0.0, 0.0, 1.0]))]),
            create(path, "geo.material", &[]),
            connect(geo, path, 0),
            connect(&format!("{path}_mat"), path, 1),
        ]
    };
    let mut ops = vec![
        // A wall 4 m wide at y = 0, facing the camera at y = -4.
        create(
            "/wall",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([4.0, 1.0, 2.0]))],
        ),
        // A box 1 m in front of the wall, and a see-through square there.
        create("/box", "geo.primitive", &[("size", Value::Vec3([0.3; 3]))]),
        create("/box_at", "geo.transform", &[("translate", Value::Vec3([0.0, -1.0, 0.0]))]),
        connect("/box", "/box_at", 0),
        create(
            "/glass",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([0.3, 1.0, 0.3]))],
        ),
        create("/glass_mat", "mat.constant", &[("color", Value::Vec4([1.0, 1.0, 1.0, 0.2]))]),
        create("/glass_on", "geo.material", &[]),
        connect("/glass", "/glass_on", 0),
        connect("/glass_mat", "/glass_on", 1),
        create("/glass_at", "geo.transform", &[("translate", Value::Vec3([0.0, -1.0, 0.0]))]),
        connect("/glass_on", "/glass_at", 0),
        // The camera of the projector: where the render camera is, narrower.
        create(
            "/beamer",
            "view.camera",
            &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(20.0))],
        ),
        create("/ramp", "tex.image", &[("file", "ramp.png".into())]),
        create("/projector", "light.projector", &[("shadows", Value::Bool(false))]),
        connect("/beamer", "/projector", 0),
        connect("/ramp", "/projector", 1),
        // From the front left at 45 degrees: the box's shadow falls about 1 m to its right.
        create(
            "/sun",
            "light.directional",
            &[("position", Value::Vec3([-3.0, -3.0, 0.0])), ("shadows", Value::Bool(true))],
        ),
        create(
            "/spot",
            "light.spot",
            &[
                ("position", Value::Vec3([-3.0, -3.0, 0.0])),
                ("angle", Value::Float(90.0)),
                ("softness", Value::Float(0.0)),
                ("shadows", Value::Bool(true)),
            ],
        ),
        create("/side_cam", "view.camera", &[("position", Value::Vec3([-3.0, -3.0, 0.0]))]),
        create("/side", "light.projector", &[]),
        connect("/side_cam", "/side", 0),
        create("/sun_plain", "light.directional", &[("position", Value::Vec3([-3.0, -3.0, 0.0]))]),
        create("/with_box", "geo.merge", &[]),
        create("/with_glass", "geo.merge", &[]),
    ];
    ops.extend(white("/lit_wall", "/wall"));
    ops.extend(white("/lit_box", "/box_at"));
    ops.extend([
        connect("/lit_wall", "/with_box", 0),
        connect("/lit_box", "/with_box", 1),
        connect("/lit_wall", "/with_glass", 0),
        connect("/glass_at", "/with_glass", 1),
    ]);
    // (render, geo, light)
    let cases = [
        ("/thrown", "/lit_wall", "/projector"),
        ("/sun_shadow", "/with_box", "/sun"),
        ("/sun_none", "/with_box", "/sun_plain"),
        ("/spot_shadow", "/with_box", "/spot"),
        ("/side_shadow", "/with_box", "/side"),
        ("/glass_shadow", "/with_glass", "/sun"),
    ];
    for (path, geo, light) in cases {
        ops.push(cam(path));
        ops.push(connect(geo, path, 0));
        ops.push(connect(light, path, 2));
    }
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let wait = |rig: &mut Rig, path: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let px = rig.pixels(path);
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert!(s.error.is_none(), "{path}: {s:?}");
            if at(&px, px.width / 2, px.height / 2)[3] > 0.5 || Instant::now() > deadline {
                return px;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Where a point (x, z) of the wall shows in a 96 x 64 picture from y = -4, fov 40.
    let px_of = |px: &Pixels, x: f64, z: f64| {
        let f = 0.5 * px.height as f64 / 20f64.to_radians().tan();
        let u = (0.5 * px.width as f64 + f * x / 4.0) as u32;
        let v = (0.5 * px.height as f64 - f * z / 4.0) as u32;
        at(px, u, v)
    };
    // The projector's picture fills a 20 degree frustum: 2.12 m by 1.41 m on the wall (48 x 32
    // texels). Red runs left to right, green top to bottom (sRGB ramps, linear here), the right
    // quarter is transparent: no light there.
    let px = wait(&mut rig, "/thrown");
    let half_w = 4.0 * 10f64.to_radians().tan() * 1.5;
    let (left, right) = (px_of(&px, -0.6 * half_w, 0.0), px_of(&px, 0.3 * half_w, 0.0));
    assert!(left[0] < 0.1 && right[0] > 0.25, "red ramp: {left:?} {right:?}");
    let (top, bottom) = (px_of(&px, -0.3, 0.25), px_of(&px, -0.3, -0.25));
    assert!(top[1] < 0.15 && bottom[1] > 0.3, "green ramp: {top:?} {bottom:?}");
    let clear = px_of(&px, 0.9 * half_w, 0.0);
    assert!(clear[0] < 0.01 && clear[1] < 0.01, "transparent quarter: {clear:?}");
    let outside = px_of(&px, 1.5, 0.0);
    assert!(outside[..3].iter().all(|c| *c < 0.01), "outside the frustum: {outside:?}");
    // The box's shadow: 0.7 to 1.3 m to its right from the directional light, 1 to 2.1 m from
    // the point-like ones; the wall elsewhere lit at 45 degrees or so.
    let lit = std::f32::consts::FRAC_1_SQRT_2;
    for (path, shadowed) in [
        ("/sun_shadow", true),
        ("/sun_none", false),
        ("/spot_shadow", true),
        ("/side_shadow", true),
        ("/glass_shadow", false),
    ] {
        let px = wait(&mut rig, path);
        let open = px_of(&px, -1.2, 0.0);
        let behind = px_of(&px, 1.15, 0.0);
        assert!(open[0] > 0.5 * lit, "{path} open wall: {open:?}");
        if shadowed {
            assert!(behind[0] < 0.05, "{path} behind the box: {behind:?}");
        } else {
            assert!(behind[0] > 0.6 * lit, "{path} behind: {behind:?}");
        }
        // No acne: the lit wall is even.
        let row: Vec<f32> = (0..8).map(|k| px_of(&px, -1.6 + 0.1 * f64::from(k), 0.3)[0]).collect();
        let (lo, hi) = row.iter().fold((f32::MAX, 0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(hi - lo < 0.1, "{path} uneven lit wall: {row:?}");
    }
    look(&mut rig, "/thrown", "projector_thrown", (96, 64));
    look(&mut rig, "/sun_shadow", "shadow_sun", (96, 64));
    look(&mut rig, "/spot_shadow", "shadow_spot", (96, 64));
    look(&mut rig, "/side_shadow", "shadow_projector", (96, 64));
    // A projector needs a camera.
    rig.edit(&[create("/lonely", "light.projector", &[])]);
    rig.cook("/lonely");
    let s = rig.cooker.status(rig.id("/lonely")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("camera")), "{s:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// See-through surfaces: drawn back to front whatever order the geometry comes in, over what is
/// opaque behind them; lights casting shadows pass through them, tinted.
#[test]
fn see_through_surfaces_sort_and_tint_the_light() {
    let Some(gpu) = gpu() else { return };
    let cam = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(40.0))])
    };
    let pane = |path: &str, rgba: [f64; 4], y: f64| {
        vec![
            create(
                &format!("{path}_shape"),
                "geo.primitive",
                &[("shape", "plane".into()), ("size", Value::Vec3([0.6, 1.0, 0.6]))],
            ),
            create(&format!("{path}_mat"), "mat.constant", &[("color", Value::Vec4(rgba))]),
            create(&format!("{path}_on"), "geo.material", &[]),
            connect(&format!("{path}_shape"), &format!("{path}_on"), 0),
            connect(&format!("{path}_mat"), &format!("{path}_on"), 1),
            create(path, "geo.transform", &[("translate", Value::Vec3([0.0, y, 0.0]))]),
            connect(&format!("{path}_on"), path, 0),
        ]
    };
    let mut ops = vec![
        create(
            "/wall",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([4.0, 1.0, 2.0]))],
        ),
        create("/wall_mat", "mat.phong", &[("specular", Value::Vec4([0.0, 0.0, 0.0, 1.0]))]),
        create("/lit_wall", "geo.material", &[]),
        connect("/wall", "/lit_wall", 0),
        connect("/wall_mat", "/lit_wall", 1),
        // Nearest first, the opaque wall last: the worst order for drawing as it comes.
        create("/stack", "geo.merge", &[]),
        connect("/red", "/stack", 0),
        connect("/blue", "/stack", 1),
        connect("/wall", "/stack", 2),
        create("/glass", "geo.merge", &[]),
        connect("/lit_wall", "/glass", 0),
        connect("/red", "/glass", 1),
        create("/gauze_scene", "geo.merge", &[]),
        connect("/lit_wall", "/gauze_scene", 0),
        connect("/gauze", "/gauze_scene", 1),
        // From the front left at 45 degrees: a pane 1 m before the wall throws its shadow
        // 0.7 to 1.3 m to the right.
        create(
            "/sun",
            "light.directional",
            &[("position", Value::Vec3([-3.0, -3.0, 0.0])), ("shadows", Value::Bool(true))],
        ),
        create("/side_cam", "view.camera", &[("position", Value::Vec3([-3.0, -3.0, 0.0]))]),
        create("/side", "light.projector", &[]),
        connect("/side_cam", "/side", 0),
    ];
    // Ops reference the panes before they exist: put them first.
    let mut panes = pane("/red", [1.0, 0.0, 0.0, 0.5], -1.0);
    panes.extend(pane("/blue", [0.0, 0.0, 1.0, 0.5], -0.5));
    panes.extend(pane("/gauze", [1.0, 1.0, 1.0, 0.5], -1.0));
    panes.append(&mut ops);
    let mut ops = panes;
    // (render, geo, light)
    let cases = [
        ("/sorted", "/stack", None),
        ("/open", "/lit_wall", Some("/sun")),
        ("/through_red", "/glass", Some("/sun")),
        ("/open_side", "/lit_wall", Some("/side")),
        ("/through_gauze", "/gauze_scene", Some("/side")),
    ];
    for (path, geo, light) in cases {
        ops.push(cam(path));
        ops.push(connect(geo, path, 0));
        if let Some(l) = light {
            ops.push(connect(l, path, 2));
        }
    }
    let mut rig = Rig::new(gpu, None, &ops);
    let px_of = |px: &Pixels, x: f64, z: f64| {
        let f = 0.5 * px.height as f64 / 20f64.to_radians().tan();
        let u = (0.5 * px.width as f64 + f * x / 4.0) as u32;
        let v = (0.5 * px.height as f64 - f * z / 4.0) as u32;
        at(px, u, v)
    };
    let near = |got: [f32; 4], want: [f32; 4], what: &str| {
        let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.02);
        assert!(ok, "{what}: {got:?}, want {want:?}");
    };
    // Blue over the white wall, red over that: (0.75, 0.25, 0.5). Drawn as they come, the
    // wall would be hidden (depth) or blue would lie over red (0.5, 0.25, 0.75).
    let px = rig.pixels("/sorted");
    near(px_of(&px, 0.0, 0.0), [0.75, 0.25, 0.5, 1.0], "red over blue over the wall");
    near(px_of(&px, 1.0, 0.0), [1.0; 4], "the wall beside");
    // Light through red glass of alpha 0.5: half of it passes, red.
    let (open, red) = (rig.pixels("/open"), rig.pixels("/through_red"));
    let (a, b) = (px_of(&open, 1.0, 0.0), px_of(&red, 1.0, 0.0));
    assert!(a[0] > 0.5, "lit: {a:?}");
    let ratio = [b[0] / a[0], b[1] / a[1], b[2] / a[2]];
    near([ratio[0], ratio[1], ratio[2], 1.0], [0.5, 0.0, 0.0, 1.0], "through red glass");
    // Beside the glass's shadow: as lit as without it.
    near(px_of(&red, -1.2, 0.0), px_of(&open, -1.2, 0.0), "beside the red glass's shadow");
    // A projector through white gauze of alpha 0.5: half its light, untinted.
    let (open, gauze) = (rig.pixels("/open_side"), rig.pixels("/through_gauze"));
    // From a point: its shadow lies 1.05 to 1.95 m to the right.
    let (a, b) = (px_of(&open, 1.5, 0.0), px_of(&gauze, 1.5, 0.0));
    assert!(a[0] > 0.3, "lit by the projector: {a:?}");
    let ratio = [b[0] / a[0], b[1] / a[1], b[2] / a[2]];
    near([ratio[0], ratio[1], ratio[2], 1.0], [0.5, 0.5, 0.5, 1.0], "through gauze");
    look(&mut rig, "/sorted", "sorted", (96, 64));
    look(&mut rig, "/through_red", "through_red", (96, 64));
    look(&mut rig, "/through_gauze", "through_gauze", (96, 64));
}

/// An environment picture of a sky, red where x > 0 and green where x < 0, black below the
/// horizon.
fn write_sky(path: &Path) {
    let (w, h) = (64u32, 32u32);
    let mut samples = Vec::new();
    for y in 0..h {
        for x in 0..w {
            // u = 0.5 looks along +x; u from 0.25 to 0.75 is the half where x > 0.
            let u = (x as f64 + 0.5) / w as f64;
            let sky = y < h / 2;
            let east = (0.25..0.75).contains(&u);
            let (r, g) = match (sky, east) {
                (false, _) => (0, 0),
                (true, true) => (65535, 0),
                (true, false) => (0, 65535),
            };
            samples.extend([r, g, 0, 65535u16]);
        }
    }
    let img =
        photocraft_codecs::Image::from_u16(w, h, photocraft_codecs::ChannelLayout::Rgba, &samples).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(path, png).unwrap();
}

/// The environment light: matte surfaces get the sky's light by their normal, mirrors reflect it
/// where the reflection points, `turn` turns it counter-clockwise seen from above; pbr materials,
/// metallic and rough.
#[test]
fn an_environment_lights_and_reflects_by_its_picture() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("geo-environment");
    write_sky(&dir.join("sky.png"));
    // From the front left, above: a floor's mirror reflection points along (1, 1, 1).
    let from_above = |path: &str| {
        render(path, &[("position", Value::Vec3([-4.0, -4.0, 4.0])), ("fov", Value::Float(30.0))])
    };
    let head_on = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("fov", Value::Float(30.0))])
    };
    let on = |path: &str, geo: &str, mat: &str, params: &[(&str, Value)]| {
        vec![
            create(&format!("{path}_mat"), mat, params),
            create(path, "geo.material", &[]),
            connect(geo, path, 0),
            connect(&format!("{path}_mat"), path, 1),
        ]
    };
    let mut ops = vec![
        create("/sky", "tex.image", &[("file", "sky.png".into())]),
        create("/env", "light.environment", &[("size", Value::Int(64))]),
        connect("/sky", "/env", 0),
        create("/env_ccw", "light.environment", &[("size", Value::Int(64)), ("turn", Value::Float(90.0))]),
        connect("/sky", "/env_ccw", 0),
        create("/env_cw", "light.environment", &[("size", Value::Int(64)), ("turn", Value::Float(-90.0))]),
        connect("/sky", "/env_cw", 0),
        create(
            "/plane",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([2.0, 1.0, 2.0]))],
        ),
        create("/floor", "geo.transform", &[("rotate", Value::Vec3([90.0, 0.0, 0.0]))]),
        connect("/plane", "/floor", 0),
    ];
    let matte = [("specular", Value::Vec4([0.0, 0.0, 0.0, 1.0]))];
    ops.extend(on("/matte_floor", "/floor", "mat.phong", &matte));
    ops.extend(on("/matte_wall", "/plane", "mat.phong", &matte));
    let metal = |rough: f64| [("metallic", Value::Float(1.0)), ("roughness", Value::Float(rough))];
    ops.extend(on("/mirror", "/floor", "mat.pbr", &metal(0.0)));
    ops.extend(on("/brushed", "/floor", "mat.pbr", &metal(1.0)));
    ops.extend(on("/plastic", "/floor", "mat.pbr", &[("roughness", Value::Float(1.0))]));
    // (render, geo, light, camera from above)
    let cases = [
        ("/floor_lit", "/matte_floor", "/env", true),
        ("/wall_lit", "/matte_wall", "/env", false),
        ("/mirrored", "/mirror", "/env", true),
        ("/mirrored_ccw", "/mirror", "/env_ccw", true),
        ("/mirrored_cw", "/mirror", "/env_cw", true),
        ("/blurred", "/brushed", "/env", true),
        ("/plastic_lit", "/plastic", "/env", true),
    ];
    for (path, geo, light, above) in cases {
        ops.push(if above { from_above(path) } else { head_on(path) });
        ops.push(connect(geo, path, 0));
        ops.push(connect(light, path, 2));
    }
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let centre = |rig: &mut Rig, path: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let px = rig.pixels(path);
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert!(s.error.is_none(), "{path}: {s:?}");
            let c = at(&px, px.width / 2, px.height / 2);
            if c[3] > 0.5 || Instant::now() > deadline {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let near = |got: [f32; 4], want: [f32; 3], tol: f32, what: &str| {
        let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() < tol);
        assert!(ok, "{what}: {got:?}, want {want:?}");
    };
    // A floor facing up gets the whole sky, half red, half green: (0.5, 0.5); a wall facing
    // the camera half of it, from above the horizon: (0.25, 0.25).
    near(centre(&mut rig, "/floor_lit"), [0.5, 0.5, 0.0], 0.04, "matte floor");
    near(centre(&mut rig, "/wall_lit"), [0.25, 0.25, 0.0], 0.04, "matte wall");
    // A mirror floor reflects along (1, 1, 1): the red half. Turned 90 degrees counter-clockwise
    // the red half covers +y too, turned the other way the green one comes round.
    let m = centre(&mut rig, "/mirrored");
    assert!(m[0] > 0.8 && m[1] < 0.1, "mirror: {m:?}");
    let m = centre(&mut rig, "/mirrored_ccw");
    assert!(m[0] > 0.8 && m[1] < 0.1, "mirror turned ccw: {m:?}");
    let m = centre(&mut rig, "/mirrored_cw");
    assert!(m[1] > 0.8 && m[0] < 0.1, "mirror turned cw: {m:?}");
    // Rough metal blurs the reflection: both halves, red still ahead.
    let b = centre(&mut rig, "/blurred");
    assert!(b[0] > 2.0 * b[1] && b[1] > 0.04, "rough metal: {b:?}");
    // Rough white plastic: about the matte floor, plus a little reflection.
    near(centre(&mut rig, "/plastic_lit"), [0.52, 0.52, 0.0], 0.08, "plastic");
    look(&mut rig, "/mirrored", "env_mirror", (96, 64));
    look(&mut rig, "/blurred", "env_rough_metal", (96, 64));
    // pbr materials' pictures, in a studio of their own, for looking at by eye.
    if let Some(look_dir) = std::env::var_os("PENUMBRA_LOOK") {
        for path in ["/mirror_mat", "/brushed_mat", "/plastic_mat"] {
            rig.cook(path);
            let out = rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap();
            let prev = out.get::<penumbra_gpu::Mat>().unwrap().preview.clone().unwrap();
            let name = format!("mat_{}.png", path.trim_start_matches('/'));
            std::fs::write(Path::new(&look_dir).join(name), rig.gpu.read(&prev).unwrap().to_png().unwrap())
                .unwrap();
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Ambient occlusion: a wall darkens beside a box standing on it, in its ambient light only;
/// open wall and the box's flat front stay as without it, direct light is left alone, and
/// without ambient light nothing changes.
#[test]
fn ambient_occlusion_darkens_creases_in_the_ambient_light() {
    let Some(gpu) = gpu() else { return };
    let cam = |path: &str, ao: bool| {
        render(
            path,
            &[
                ("position", Value::Vec3([0.0, -4.0, 0.0])),
                ("fov", Value::Float(40.0)),
                ("ao", Value::Bool(ao)),
                ("ao_radius", Value::Float(0.3)),
            ],
        )
    };
    // Wide and weak: nothing may go darker than the strength allows.
    let weak = render(
        "/weak",
        &[
            ("position", Value::Vec3([0.0, -4.0, 0.0])),
            ("fov", Value::Float(40.0)),
            ("ao", Value::Bool(true)),
            ("ao_radius", Value::Float(1.0)),
            ("ao_strength", Value::Float(0.3)),
        ],
    );
    let mut ops = vec![
        // A white wall at y = 0 facing the camera, and a 0.3 m box standing out of it.
        create(
            "/wall",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([4.0, 1.0, 2.0]))],
        ),
        create("/box", "geo.primitive", &[("size", Value::Vec3([0.3; 3]))]),
        create("/box_at", "geo.transform", &[("translate", Value::Vec3([0.0, -0.15, 0.0]))]),
        connect("/box", "/box_at", 0),
        create("/both", "geo.merge", &[]),
        connect("/wall", "/both", 0),
        connect("/box_at", "/both", 1),
        create("/white", "mat.phong", &[("specular", Value::Vec4([0.0, 0.0, 0.0, 1.0]))]),
        create("/scene", "geo.material", &[]),
        connect("/both", "/scene", 0),
        connect("/white", "/scene", 1),
        // Half ambient, half from the camera's direction.
        create("/fill", "light.ambient", &[("intensity", Value::Float(0.5))]),
        create(
            "/sun",
            "light.directional",
            &[("position", Value::Vec3([0.0, -4.0, 0.0])), ("intensity", Value::Float(0.5))],
        ),
        create("/lights", "light.merge", &[]),
        connect("/fill", "/lights", 0),
        connect("/sun", "/lights", 1),
    ];
    // (render, ambient occlusion, lights)
    let cases = [
        ("/plain", false, "/lights"),
        ("/ao", true, "/lights"),
        ("/sun_plain", false, "/sun"),
        ("/sun_ao", true, "/sun"),
    ];
    for (path, ao, lights) in cases {
        ops.push(cam(path, ao));
        ops.push(connect("/scene", path, 0));
        ops.push(connect(lights, path, 2));
    }
    ops.extend([weak, connect("/scene", "/weak", 0), connect("/lights", "/weak", 2)]);
    let mut rig = Rig::new(gpu, None, &ops);
    // Where a point (x, z) of the wall shows in the picture, from y = -4 with fov 40.
    let px_of = |px: &Pixels, x: f64, z: f64| {
        let f = 0.5 * px.height as f64 / 20f64.to_radians().tan();
        let u = (0.5 * px.width as f64 + f * x / 4.0) as u32;
        let v = (0.5 * px.height as f64 - f * z / 4.0) as u32;
        at(px, u, v)
    };
    let plain = rig.pixels("/plain");
    let ao = rig.pixels("/ao");
    for path in ["/plain", "/ao"] {
        let s = rig.cooker.status(rig.id(path)).unwrap();
        assert!(s.error.is_none() && s.warning.is_none(), "{path}: {s:?}");
    }
    // Without occlusion the wall is evenly 1 (half ambient, half head on).
    let open = px_of(&plain, -1.2, 0.3)[0];
    assert!((open - 1.0).abs() < 0.03, "plain wall: {open}");
    // Beside the box: darker, but never below the direct half; alike on all four sides.
    let sides = [(-0.2, 0.0), (0.2, 0.0), (0.0, 0.2), (0.0, -0.2)].map(|(x, z)| px_of(&ao, x, z)[0]);
    for v in sides {
        assert!((0.5..0.95).contains(&v), "beside the box: {sides:?}");
    }
    let (lo, hi) = sides.iter().fold((f32::MAX, 0f32), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(hi - lo < 0.03, "uneven around the box: {sides:?}");
    // Far from it and on the box's flat front: as without.
    for (what, x, z) in [("open wall", -1.2, 0.3), ("open wall", 1.3, -0.6), ("box front", 0.0, 0.0)] {
        let (a, b) = (px_of(&ao, x, z)[0], px_of(&plain, x, z)[0]);
        assert!((a - b).abs() < 0.03, "{what} at {x} {z}: {a} with, {b} without");
    }
    // Strength 0.3 takes at most 30 % of the ambient half, wherever the radius reaches.
    let px = rig.pixels("/weak");
    let darkest = px.data.iter().filter(|c| c[3] > 0.99).map(|c| c[0]).fold(f32::MAX, f32::min);
    assert!((0.85 - 1e-3..0.99).contains(&darkest), "weak: darkest {darkest}");
    // Direct light alone: nothing to dim, and a warning.
    let (sun_plain, sun_ao) = (rig.pixels("/sun_plain"), rig.pixels("/sun_ao"));
    let (a, b) = (px_of(&sun_ao, 0.2, 0.0)[0], px_of(&sun_plain, 0.2, 0.0)[0]);
    assert!((a - b).abs() < 1e-3, "direct light only: {a} with, {b} without");
    let s = rig.cooker.status(rig.id("/sun_ao")).unwrap();
    assert!(s.warning.as_deref().is_some_and(|w| w.contains("ambient")), "{s:?}");
    look(&mut rig, "/ao", "ambient_occlusion", (super::W, super::H));
    look(&mut rig, "/plain", "ambient_occlusion_off", (super::W, super::H));
}

/// A wireframe shows its edges only (a quad's diagonal only when asked for every triangle);
/// an occluder shows nothing, also in the data pass, and hides what is behind it even where
/// that was merged in before it.
#[test]
fn wireframes_show_edges_and_occluders_hide() {
    let Some(gpu) = gpu() else { return };
    let cam = |path: &str| {
        render(path, &[("position", Value::Vec3([0.0, -2.5, 0.0])), ("fov", Value::Float(40.0))])
    };
    let ops = vec![
        create("/box", "geo.primitive", &[("size", Value::Vec3([1.0, 1.0, 1.0]))]),
        create("/quads_mat", "mat.wireframe", &[("line_width", Value::Float(3.0))]),
        create("/quads", "geo.material", &[]),
        connect("/box", "/quads", 0),
        connect("/quads_mat", "/quads", 1),
        cam("/quads_seen"),
        connect("/quads", "/quads_seen", 0),
        create(
            "/tris_mat",
            "mat.wireframe",
            &[("line_width", Value::Float(3.0)), ("edges", "triangles".into())],
        ),
        create("/tris", "geo.material", &[]),
        connect("/box", "/tris", 0),
        connect("/tris_mat", "/tris", 1),
        cam("/tris_seen"),
        connect("/tris", "/tris_seen", 0),
        // A red wall behind an occluding box, the wall merged in first.
        create(
            "/wall",
            "geo.primitive",
            &[("shape", "plane".into()), ("size", Value::Vec3([5.0, 1.0, 3.0]))],
        ),
        create("/wall_at", "geo.transform", &[("translate", Value::Vec3([0.0, 1.0, 0.0]))]),
        connect("/wall", "/wall_at", 0),
        create("/red", "mat.constant", &[("color", Value::Vec4([1.0, 0.0, 0.0, 1.0]))]),
        create("/red_wall", "geo.material", &[]),
        connect("/wall_at", "/red_wall", 0),
        connect("/red", "/red_wall", 1),
        create("/hole", "mat.occluder", &[]),
        create("/hidden", "geo.material", &[]),
        connect("/box", "/hidden", 0),
        connect("/hole", "/hidden", 1),
        create("/both", "geo.merge", &[]),
        connect("/red_wall", "/both", 0),
        connect("/hidden", "/both", 1),
        cam("/occluded"),
        connect("/both", "/occluded", 0),
        // Faces filled in blue, and hidden faces before the wall: only the lines filled faces
        // would leave, the wall showing through.
        create(
            "/filled_mat",
            "mat.wireframe",
            &[
                ("line_width", Value::Float(3.0)),
                ("faces", "filled".into()),
                ("fill", Value::Vec4([0.0, 0.0, 1.0, 1.0])),
            ],
        ),
        create("/filled", "geo.material", &[]),
        connect("/box", "/filled", 0),
        connect("/filled_mat", "/filled", 1),
        cam("/filled_seen"),
        connect("/filled", "/filled_seen", 0),
        create(
            "/front_mat",
            "mat.wireframe",
            &[("line_width", Value::Float(3.0)), ("faces", "hidden".into())],
        ),
        create("/front", "geo.material", &[]),
        connect("/box", "/front", 0),
        connect("/front_mat", "/front", 1),
        create("/front_wall", "geo.merge", &[]),
        connect("/red_wall", "/front_wall", 0),
        connect("/front", "/front_wall", 1),
        cam("/front_seen"),
        connect("/front_wall", "/front_seen", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for path in ["/quads_seen", "/tris_seen", "/occluded", "/filled_seen", "/front_seen"] {
        rig.pixels(path);
    }
    look(&mut rig, "/quads_seen", "wireframe_polygons", (super::W, super::H));
    look(&mut rig, "/tris_seen", "wireframe_triangles", (super::W, super::H));
    look(&mut rig, "/occluded", "occluder", (super::W, super::H));
    look(&mut rig, "/filled_seen", "wireframe_filled", (super::W, super::H));
    look(&mut rig, "/front_seen", "wireframe_hidden", (super::W, super::H));
    // The materials' own pictures.
    for path in ["/quads_mat", "/hole"] {
        let out = rig.cooker.output(Port { node: rig.id(path), output: 0 }).unwrap();
        let mat = out.get::<penumbra_gpu::Mat>().unwrap();
        let px = rig.gpu.read(mat.preview.as_ref().unwrap()).unwrap();
        assert!(px.data.iter().any(|p| p[3] > 0.5), "{path} shows something of itself");
        if let Some(dir) = std::env::var_os("PENUMBRA_LOOK") {
            let file = Path::new(&dir).join(format!("{}_preview.png", path.trim_start_matches('/')));
            std::fs::write(file, px.to_png().unwrap()).unwrap();
        }
    }
    let (cx, cy) = ((super::W / 2) as u32, (super::H / 2) as u32);
    let quads = rig.pixels("/quads_seen");
    let row: Vec<f32> = (0..quads.width).map(|x| at(&quads, x, cy)[3]).collect();
    assert!(at(&quads, cx, cy)[3] < 0.05, "a quad's middle is open: {row:?}");
    // Across the middle: the front face's sides and the back face's, seen through it.
    let lines = row.windows(2).filter(|w| w[0] < 0.5 && w[1] >= 0.5).count();
    assert_eq!(lines, 4, "{row:?}");
    let full = quads.data.iter().map(|p| p[3]).fold(0.0, f32::max);
    assert!(full > 0.99, "lines are opaque in their middle: {full}");
    // Filled: blue between the lines, which hides the back face's.
    let filled = rig.pixels("/filled_seen");
    let mid = at(&filled, cx, cy);
    assert!(mid[2] > 0.99 && mid[0] < 0.01 && mid[3] > 0.99, "{mid:?}");
    let row: Vec<f32> = (0..filled.width).map(|x| at(&filled, x, cy)[0]).collect();
    assert_eq!(row.windows(2).filter(|w| w[0] < 0.5 && w[1] >= 0.5).count(), 2, "{row:?}");
    // Hidden: only the front face's lines, the wall behind showing through between them.
    let front = rig.pixels("/front_seen");
    let mid = at(&front, cx, cy);
    assert!(mid[0] > 0.99 && mid[1] < 0.01 && mid[3] > 0.99, "the wall through the middle: {mid:?}");
    let row: Vec<f32> = (0..front.width).map(|x| at(&front, x, cy)[1]).collect();
    assert_eq!(row.windows(2).filter(|w| w[0] < 0.5 && w[1] >= 0.5).count(), 2, "{row:?}");
    let tris = rig.pixels("/tris_seen");
    assert!(at(&tris, cx, cy)[3] > 0.9, "the diagonals cross in the middle: {:?}", at(&tris, cx, cy));

    let px = rig.pixels("/occluded");
    assert_eq!(at(&px, cx, cy), [0.0; 4], "the box hides the wall and shows nothing");
    let side = at(&px, 4, cy);
    assert!(side[0] > 0.99 && side[1] < 0.01 && side[3] > 0.99, "the wall beside it: {side:?}");
    rig.cook("/occluded");
    let d = data(&rig, "/occluded");
    assert_eq!(at(&d, cx, cy), [0.0; 4], "nothing in the data pass either");
    assert_eq!(at(&d, 4, cy)[2].round(), 1.0, "the wall is part 0");
}
