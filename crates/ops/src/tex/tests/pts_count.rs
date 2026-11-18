//! Counts on the GPU, on WARP: `pts.delete` by each choice, what the nodes downstream of it do
//! (programs, pictures, drawing, the count as a channel), `pts.from_tex` dropping empty texels,
//! and the capacity a generator gives.

use std::time::{Duration, Instant};

use penumbra_core::{Chans, Value};
use penumbra_gpu::{Geo, Points, Tex};
use penumbra_graph::Port;

use super::geo::{render, rig};
use super::pts::{coverage, points};
use super::{Rig, connect, create, gpu, scratch, set};

/// The live count: the header's where it is on the GPU.
fn live(rig: &Rig, p: &Points) -> usize {
    match p.count().header() {
        Some(h) => h.read::<u32>(&rig.gpu).unwrap()[0] as usize,
        None => p.count().known().unwrap() as usize,
    }
}

/// The first `live` positions.
fn positions(rig: &Rig, p: &Points) -> Vec<[f32; 4]> {
    let n = live(rig, p);
    p.attr("position").unwrap().buffer().read::<[f32; 4]>(&rig.gpu).unwrap()[..n].to_vec()
}

/// A grid of 4 rows and 5 columns: x is -2 to 2, z 1.5 to -1.5, point i in row i / 5.
fn grid() -> penumbra_graph::Op {
    create(
        "/grid",
        "pts.generate",
        &[("rows", Value::Int(4)), ("cols", Value::Int(5)), ("size", Value::Vec3([4.0, 1.0, 3.0]))],
    )
}

/// The grid's points `all` for which `kept` holds, in order.
fn pick(all: &[[f32; 4]], kept: impl Fn(usize) -> bool) -> Vec<[f32; 4]> {
    all.iter().enumerate().filter(|(i, _)| kept(*i)).map(|(_, p)| *p).collect()
}

#[test]
fn delete_drops_points_by_each_choice_and_keeps_the_order() {
    let Some(gpu) = gpu() else { return };
    let ops = [grid(), create("/del", "pts.delete", &[]), connect("/grid", "/del", 0)];
    let mut rig = Rig::new(gpu, None, &ops);
    let input = points(&mut rig, "/grid");
    let all = positions(&rig, &input);
    assert_eq!(all.len(), 20);
    assert_eq!((all[3][0], all[13][2]), (1.0, -0.50000006), "x is -2 to 2, z 1.5 to -1.5");
    let kept = |f: &dyn Fn(usize) -> bool| pick(&all, f);
    assert_eq!(input.capacity(), 1024, "the generator rounds its capacity up");

    // The default deletes points of weight 0: the grid has none, all stay.
    let p = points(&mut rig, "/del");
    assert!(p.count().known().is_none() && p.count().at_most() == 20);
    assert_eq!((p.capacity(), p.dims()), (1024, None));
    assert_eq!(positions(&rig, &p), kept(&|_| true));
    assert_eq!(p.bounds(), input.bounds(), "the input's box");

    let mut check = |edits: Vec<penumbra_graph::Op>, want: Vec<[f32; 4]>, what: &str| {
        rig.edit(&edits);
        let p = points(&mut rig, "/del");
        assert_eq!(positions(&rig, &p), want, "{what}");
    };
    check(vec![set("/del.expression", "P.x < 0.0".into())], kept(&|i| i % 5 >= 2), "expression");
    check(vec![set("/del.invert", Value::Bool(true))], kept(&|i| i % 5 < 2), "inverted");
    check(
        vec![
            set("/del.invert", Value::Bool(false)),
            set("/del.expression", "position.z > a".into()),
            set("/del.a", Value::Float(0.0)),
        ],
        kept(&|i| i >= 10),
        "a param in the expression",
    );
    check(
        vec![
            set("/del.by", "index".into()),
            set("/del.first", Value::Int(2)),
            set("/del.last", Value::Int(9)),
            set("/del.every", Value::Int(3)),
        ],
        kept(&|i| ![2, 5, 8].contains(&i)),
        "index",
    );
    check(
        vec![
            set("/del.by", "box".into()),
            set("/del.center", Value::Vec3([0.0; 3])),
            set("/del.size", Value::Vec3([2.5, 1.0, 1.2])),
        ],
        kept(&|i| !((1..=3).contains(&(i % 5)) && (1..=2).contains(&(i / 5)))),
        "box",
    );
    check(
        vec![
            set("/del.by", "sphere".into()),
            set("/del.center", Value::Vec3([2.0, 0.0, 1.5])),
            set("/del.radius", Value::Float(1.1)),
        ],
        kept(&|i| ![3, 4, 9].contains(&i)),
        "sphere",
    );
    rig.edit(&[set("/del.by", "random".into()), set("/del.share", Value::Float(0.5))]);
    let p = points(&mut rig, "/del");
    let some = positions(&rig, &p);
    assert!((3..=17).contains(&some.len()), "about half: {}", some.len());
    let places: Vec<usize> = some.iter().map(|q| all.iter().position(|a| a == q).unwrap()).collect();
    assert!(places.windows(2).all(|w| w[0] < w[1]), "in order: {places:?}");
    rig.edit(&[set("/del.invert", Value::Bool(true))]);
    let p = points(&mut rig, "/del");
    let rest = positions(&rig, &p);
    assert_eq!(some.len() + rest.len(), 20, "invert keeps the others");
    assert!(rest.iter().all(|q| !some.contains(q)));
    rig.edit(&[set("/del.invert", Value::Bool(false)), set("/del.seed", Value::Int(5))]);
    let p = points(&mut rig, "/del");
    let other = positions(&rig, &p);
    assert_ne!(other, some, "another seed deletes others");
    rig.edit(&[set("/del.seed", Value::Int(0))]);
    let p = points(&mut rig, "/del");
    assert_eq!(positions(&rig, &p), some, "the same seed the same points");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The channels of `path` once it has them.
fn chans(rig: &mut Rig, path: &str) -> Chans {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        rig.gpu.poll();
        if let Some(d) = rig.cooker.output(Port { node: rig.id(path), output: 0 }) {
            return d.get::<Chans>().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "{path} has no channels");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn value(c: &Chans, name: &str) -> f64 {
    c.channels().iter().find(|ch| ch.name.as_str() == name).unwrap().value()
}

#[test]
fn nodes_after_a_delete_stop_at_the_live_count() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        grid(),
        create("/del", "pts.delete", &[("expression", "position.x < 0.0".into())]),
        connect("/grid", "/del", 0),
        create(
            "/math",
            "pts.math",
            &[("expression", "f32(count) + index01".into()), ("result", "scale".into())],
        ),
        connect("/del", "/math", 0),
        create("/pic", "tex.from_pts", &[]),
        connect("/del", "/pic", 0),
        create("/count", "chan.from_pts", &[]),
        connect("/del", "/count", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let input = points(&mut rig, "/grid");
    let all = positions(&rig, &input);
    // A program sees the live count and leaves the dead points alone.
    let p = points(&mut rig, "/math");
    assert_eq!(live(&rig, &p), 12);
    let scale = p.attr("scale").unwrap().buffer().read::<f32>(&rig.gpu).unwrap();
    for (i, s) in scale.iter().enumerate() {
        let want = if i < 12 { 12.0 + i as f32 / 11.0 } else { 0.0 };
        assert!((s - want).abs() < 1e-5, "{i}: {s}");
    }
    // The picture: sized for the capacity of 1024, so it keeps its size while the count
    // moves; the 12 alive, then transparent black.
    let tex = rig.cook_until_new("/pic", 0);
    assert_eq!(tex.size(), (32, 32));
    let px = rig.gpu.read(&tex).unwrap();
    let want = pick(&all, |i| i % 5 >= 2);
    for j in 0..1024u32 {
        let got = px.get(j % 32, j / 32).unwrap();
        match want.get(j as usize) {
            Some(q) => assert_eq!(got, *q, "{j}"),
            None => assert_eq!(got, [0.0; 4], "{j}"),
        }
    }
    // The count as a channel, read back.
    let c = chans(&mut rig, "/count");
    assert_eq!((value(&c, "count"), value(&c, "capacity")), (12.0, 1024.0));
    // Everything deleted: nothing comes through, nothing fails.
    rig.edit(&[set("/del.expression", "true".into())]);
    points(&mut rig, "/del");
    let p = points(&mut rig, "/math");
    assert_eq!(live(&rig, &p), 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let c = chans(&mut rig, "/count");
        if value(&c, "count") == 0.0 {
            break;
        }
        assert!(Instant::now() < deadline, "the channel never reached 0");
    }
    let v = rig.cooker.status(rig.id("/pic")).map_or(0, |s| s.version);
    let tex = rig.cook_until_new("/pic", v);
    let px = rig.gpu.read(&tex).unwrap();
    assert!(px.data.iter().all(|t| *t == [0.0; 4]));
    // The count the viewer shows arrives without a wait.
    let p = points(&mut rig, "/del");
    let deadline = Instant::now() + Duration::from_secs(30);
    while p.count().seen() != Some(0) {
        rig.gpu.poll();
        assert!(Instant::now() < deadline, "the count never came back");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn the_renderer_draws_exactly_the_points_left() {
    let Some(gpu) = gpu() else { return };
    let ops = vec![
        create(
            "/line",
            "pts.generate",
            &[("rows", Value::Int(1)), ("cols", Value::Int(5)), ("size", Value::Vec3([4.0, 1.0, 1.0]))],
        ),
        // Only the point at x = 2 stays. The buffers behind it hold zeros, a point at the
        // origin if anything drew them.
        create("/del", "pts.delete", &[("expression", "position.x < 1.5".into())]),
        connect("/line", "/del", 0),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.5)), ("sprite", "square".into())]),
        connect("/del", "/sprites", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/white", "/lit", 1),
        render(
            "/front",
            &[
                ("width", Value::Int(128)),
                ("height", Value::Int(64)),
                ("position", Value::Vec3([0.0, -8.0, 0.0])),
                ("antialias", Value::Bool(false)),
            ],
        ),
        connect("/lit", "/front", 0),
    ];
    let mut rig = rig(gpu, None, &ops);
    let px = super::pts::drawn(&mut rig, "/front");
    let (n, b) = coverage(&px);
    assert!(n > 20, "{n}");
    // One square, right of the middle; the middle and the left are empty.
    assert!(b[0] > 64 + 16, "{b:?}");
    let square = (b[2] - b[0] + 1) * (b[3] - b[1] + 1);
    assert!(n as u32 * 10 > square * 9, "one square: {n} of {square}");
    let geo = rig
        .cooker
        .output(Port { node: rig.id("/sprites"), output: 0 })
        .unwrap()
        .get::<Geo>()
        .unwrap()
        .clone();
    let inst = geo.items[0].instances.clone().unwrap();
    assert!(inst.live.is_some() && inst.count == Some(5));
    // Two points left: two squares.
    rig.edit(&[set("/del.expression", "position.x < 0.5".into())]);
    points(&mut rig, "/del");
    let v = rig.cooker.status(rig.id("/front")).map_or(0, |s| s.version);
    let tex = rig.cook_until_new("/front", v);
    let px = rig.gpu.read(&tex).unwrap();
    let (m, _) = coverage(&px);
    assert!(m > n * 3 / 2 && m < n * 5 / 2, "{m} against {n}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Writes a picture of 32-bit floats, `w` texels to a row.
fn exr(dir: &std::path::Path, name: &str, w: u32, texels: &[[f32; 4]]) {
    let h = texels.len() as u32 / w;
    std::fs::write(dir.join(name), penumbra_gpu::image::encode_exr_rgba32f(w, h, texels).unwrap()).unwrap();
}

#[test]
fn a_picture_drops_its_empty_texels_on_request() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-delete-empty");
    // 4 by 2: alpha 0 at texels 1, 2 and 6; blue a depth of 1 + x.
    let texels: Vec<[f32; 4]> = (0..8)
        .map(|i| [i as f32, 0.0, 1.0 + (i % 4) as f32, if [1, 2, 6].contains(&i) { 0.0 } else { 1.0 }])
        .collect();
    exr(&dir, "p.exr", 4, &texels);
    let ops = [
        create("/pic", "tex.image_file", &[("file", "p.exr".into()), ("values", "data".into())]),
        create("/pts", "pts.from_tex", &[("sampling", "step".into()), ("step", Value::Int(1))]),
        connect("/pic", "/pts", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let _: Tex = rig.cook_until_new("/pic", 0);
    let all = points(&mut rig, "/pts");
    assert_eq!((all.count().known(), all.dims()), (Some(8), Some([2, 4, 1])), "off: as before");
    let colour = |rig: &Rig, p: &Points| -> Vec<f32> {
        let n = live(rig, p);
        p.attr("color").unwrap().buffer().read::<[f32; 4]>(&rig.gpu).unwrap()[..n]
            .iter()
            .map(|c| c[0])
            .collect()
    };
    rig.edit(&[set("/pts.delete_empty", Value::Bool(true))]);
    let p = points(&mut rig, "/pts");
    assert_eq!(p.dims(), None);
    assert_eq!(colour(&rig, &p), [0.0, 3.0, 4.0, 5.0, 7.0], "the empty texels are gone, in order");
    assert!(p.attr("pts_keep").is_none(), "the flags stay inside");
    // Depth: near and far give weight 0 too.
    rig.edit(&[
        set("/pts.mode", "depth".into()),
        set("/pts.channel", "blue".into()),
        set("/pts.near", Value::Float(1.5)),
        set("/pts.far", Value::Float(3.5)),
    ]);
    let p = points(&mut rig, "/pts");
    let depth: Vec<f32> = positions(&rig, &p).iter().map(|q| q[1]).collect();
    // Texels 0 and 4 (1 m) are too near, 3 and 7 (4 m) too far, 1, 2 and 6 empty: 5 (2 m) stays.
    assert_eq!(depth, [2.0]);
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The default expression of `pts.delete` is compiled at start-up: a new node gives its points at
/// its first cook, also behind pts.from_tex, whose points carry a `weight`.
#[test]
fn a_new_delete_of_the_default_expression_cooks_at_once() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/g", "pts.generate", &[("count", Value::Int(10))]),
        create("/d", "pts.delete", &[]),
        connect("/g", "/d", 0),
        create("/w", "pts.math", &[("expression", "1.0".into()), ("result", "weight".into())]),
        connect("/g", "/w", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    rig.types.get("pts.delete").unwrap().warm_up(&rig.gpu).unwrap();
    rig.cook("/d");
    let s = rig.cooker.status(rig.id("/d")).unwrap();
    assert!(s.has_output && s.error.is_none(), "{s:?}");
    // The same behind points with a weight.
    let _ = super::pts::points(&mut rig, "/w");
    rig.edit(&[create("/d2", "pts.delete", &[]), connect("/w", "/d2", 0)]);
    rig.cook("/d2");
    let s = rig.cooker.status(rig.id("/d2")).unwrap();
    assert!(s.has_output && s.error.is_none(), "{s:?}");
}
