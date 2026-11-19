//! Points to and from meshes, channels and tables, and several sets as one, on WARP:
//! `pts.merge` and `pts.switch`, `pts.from_geo`, `chan.from_pts` with an attribute,
//! `table.from_pts`, `pts.from_chan` and `pts.from_table`.

use std::time::{Duration, Instant};

use penumbra_core::{Chans, Table, Value};
use penumbra_gpu::Points;
use penumbra_gpu::points::Strips;
use penumbra_graph::Port;

use super::pts::points;
use super::{Rig, connect, create, gpu, set};

/// The live count: the header's where it is on the GPU.
fn live(rig: &Rig, p: &Points) -> usize {
    match p.count().header() {
        Some(h) => h.read::<u32>(&rig.gpu).unwrap()[0] as usize,
        None => p.count().known().unwrap() as usize,
    }
}

/// The first `live` values of `name`, four numbers each (two for a vec2, one for a scalar).
fn values(rig: &Rig, p: &Points, name: &str) -> Vec<Vec<f32>> {
    let n = live(rig, p);
    let a = p.attr(name).unwrap_or_else(|| panic!("no {name}"));
    let per = (a.ty().stride() / 4) as usize;
    let words = a.buffer().read::<f32>(&rig.gpu).unwrap();
    words.chunks(per).take(n).map(<[f32]>::to_vec).collect()
}

fn xyz(rig: &Rig, p: &Points) -> Vec<[f32; 3]> {
    values(rig, p, "position").iter().map(|v| [v[0], v[1], v[2]]).collect()
}

/// A line of `n` points along x from `x0` to `x1`.
fn line(path: &str, n: i64, x0: f64, x1: f64) -> Vec<penumbra_graph::Op> {
    let source = format!("{path}_gen");
    vec![
        create(
            &source,
            "pts.generate",
            &[("shape", "line".into()), ("count", Value::Int(n)), ("size", Value::Vec3([x1 - x0, 0.0, 0.0]))],
        ),
        create(path, "pts.transform", &[("translate", Value::Vec3([(x0 + x1) / 2.0, 0.0, 0.0]))]),
        connect(&source, path, 0),
    ]
}

/// The status of `path` once it is in error (or after 30 s).
fn errored(rig: &mut Rig, path: &str) -> penumbra_graph::NodeStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        rig.gpu.poll();
        let s = rig.cooker.status(rig.id(path)).unwrap();
        if s.error.is_some() || Instant::now() > deadline {
            return s;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn merge_puts_sets_one_after_the_other() {
    let Some(gpu) = gpu() else { return };
    let mut ops = line("/a", 3, 0.0, 2.0);
    ops.extend(line("/b", 2, 10.0, 11.0));
    ops.extend([
        // Only the second set has a colour.
        create(
            "/red",
            "pts.math",
            &[("expression", "vec4f(1.0, 0.0, 0.0, 1.0)".into()), ("result", "color".into())],
        ),
        connect("/b", "/red", 0),
        create("/m", "pts.merge", &[]),
        connect("/a", "/m", 0),
        connect("/red", "/m", 1),
    ]);
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/m");
    assert_eq!(p.count().known(), Some(5));
    let x: Vec<f32> = xyz(&rig, &p).iter().map(|q| q[0]).collect();
    assert_eq!(x, [0.0, 1.0, 2.0, 10.0, 11.0]);
    // The first set lacks colour: white, the default.
    let c = values(&rig, &p, "color");
    assert_eq!((c[0].clone(), c[4].clone()), (vec![1.0; 4], vec![1.0, 0.0, 0.0, 1.0]));
    // Two lines of different lengths are no common lines; the box holds both sets.
    assert_eq!(p.strips(), Strips::None);
    let b = p.bounds().unwrap();
    assert_eq!((b.min[0], b.max[0]), (0.0, 11.0));

    // A set whose count is on the GPU: the merged count is too, and the points close up.
    rig.edit(&[
        create(
            "/del",
            "pts.delete",
            &[("by", "index".into()), ("first", Value::Int(0)), ("last", Value::Int(1))],
        ),
        connect("/a", "/del", 0),
        connect("/del", "/m", 0),
    ]);
    points(&mut rig, "/del");
    let p = points(&mut rig, "/m");
    assert!(p.count().known().is_none());
    assert_eq!(p.count().at_most(), 5);
    let x: Vec<f32> = xyz(&rig, &p).iter().map(|q| q[0]).collect();
    assert_eq!(x, [2.0, 10.0, 11.0], "the one point left of the first set, then the second");

    // A set with no points left adds nothing.
    rig.edit(&[set("/del.last", Value::Int(10))]);
    let p = points(&mut rig, "/m");
    let x: Vec<f32> = xyz(&rig, &p).iter().map(|q| q[0]).collect();
    assert_eq!(x, [10.0, 11.0]);

    // An attribute of two types is an error in words.
    rig.edit(&[
        create(
            "/f3",
            "pts.math",
            &[("expression", "vec3f(1.0)".into()), ("result", "foo".into()), ("type", "vec3".into())],
        ),
        connect("/a", "/f3", 0),
        create(
            "/f1",
            "pts.math",
            &[("expression", "1.0".into()), ("result", "foo".into()), ("type", "float".into())],
        ),
        connect("/b", "/f1", 0),
        connect("/f3", "/m", 0),
        connect("/f1", "/m", 1),
    ]);
    points(&mut rig, "/f3");
    points(&mut rig, "/f1");
    let s = errored(&mut rig, "/m");
    assert!(s.error.as_deref().is_some_and(|e| e.contains("`foo`") && e.contains("types")), "{s:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn merged_lines_stay_lines() {
    let Some(gpu) = gpu() else { return };
    let mut ops = line("/a", 3, 0.0, 2.0);
    ops.extend(line("/b", 3, 5.0, 7.0));
    ops.extend([create("/m", "pts.merge", &[]), connect("/a", "/m", 0), connect("/b", "/m", 1)]);
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/m");
    assert_eq!(p.strips(), Strips::regular(3), "two lines of three");
    // Proximity's lines of two points, merged with their count on the GPU: still pairs, and the
    // header says how many lines.
    rig.edit(&[
        create("/near_a", "pts.proximity", &[("distance", Value::Float(1.5))]),
        connect("/a", "/near_a", 0),
        create("/near_b", "pts.proximity", &[("distance", Value::Float(1.5))]),
        connect("/b", "/near_b", 0),
        connect("/near_a", "/m", 0),
        connect("/near_b", "/m", 1),
    ]);
    let p = points(&mut rig, "/m");
    assert_eq!(p.strips(), Strips::Pairs);
    let words = p.count().header().unwrap().read::<u32>(&rig.gpu).unwrap();
    assert_eq!((words[0], words[1]), (8, 4), "two lines in each set of three points 1 m apart");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn switch_passes_one_set_on() {
    let Some(gpu) = gpu() else { return };
    let mut ops = line("/a", 2, 0.0, 1.0);
    ops.extend(line("/b", 3, 0.0, 1.0));
    ops.extend([create("/s", "pts.switch", &[]), connect("/a", "/s", 0), connect("/b", "/s", 1)]);
    let mut rig = Rig::new(gpu, None, &ops);
    points(&mut rig, "/b");
    assert_eq!(points(&mut rig, "/s").count().known(), Some(2));
    rig.edit(&[set("/s.index", Value::Int(1))]);
    let p = points(&mut rig, "/s");
    assert_eq!(p.count().known(), Some(3));
    let b = points(&mut rig, "/b");
    assert!(
        p.attr("position").unwrap().buffer().same(b.attr("position").unwrap().buffer()),
        "passed on as it is"
    );
    // Past the last: the last.
    rig.edit(&[set("/s.index", Value::Int(7))]);
    assert_eq!(points(&mut rig, "/s").count().known(), Some(3));
}

#[test]
fn points_come_from_a_meshes_vertices_and_surface() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create("/box", "geo.primitive", &[("size", Value::Vec3([2.0, 1.0, 0.5]))]),
        create("/moved", "geo.transform", &[("translate", Value::Vec3([0.0, 0.0, 3.0]))]),
        connect("/box", "/moved", 0),
        create("/pts", "pts.from_geo", &[]),
        connect("/moved", "/pts", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/pts");
    // As stored: a box has four corners per face.
    assert_eq!(p.count().known(), Some(24));
    let q = xyz(&rig, &p);
    assert!(q.iter().all(|v| (v[0].abs() - 1.0).abs() < 1e-5 && (v[1].abs() - 0.5).abs() < 1e-5), "{q:?}");
    assert!(q.iter().all(|v| (v[2] - 3.0).abs() - 0.25 < 1e-5), "moved up by its placement: {q:?}");
    let b = p.bounds().unwrap();
    assert!((b.min[2] - 2.75).abs() < 1e-5 && (b.max[2] - 3.25).abs() < 1e-5, "{b:?}");
    assert!(p.attr("normal").is_some() && p.attr("uv").is_some() && p.attr("color").is_none());

    // Scattered over the surface: as many points on a face as its share of the area. The box's
    // faces are 2 x 1 (two), 2 x 0.5 (two) and 1 x 0.5 (two): 4 + 2 + 1 = 7 square metres.
    let n = 7000;
    rig.edit(&[set("/pts.mode", "surface".into()), set("/pts.count", Value::Int(n))]);
    let p = points(&mut rig, "/pts");
    assert_eq!(p.count().known(), Some(n as u32));
    let normals = values(&rig, &p, "normal");
    let along = |axis: usize| normals.iter().filter(|v| v[axis].abs() > 0.99).count() as f64;
    let share = [along(0), along(1), along(2)].map(|c| c / n as f64);
    // x faces 1 x 0.5 each: 1 / 7; y faces 2 x 0.5: 2 / 7; z faces 2 x 1: 4 / 7.
    let want = [1.0 / 7.0, 2.0 / 7.0, 4.0 / 7.0];
    for (s, w) in share.iter().zip(want) {
        assert!((s - w).abs() < 0.02, "shares {share:?}, wanted {want:?}");
    }
    // Every point lies on the box's surface, its normal pointing out of the face it is on.
    for (v, nrm) in xyz(&rig, &p).iter().zip(&normals) {
        let local = [v[0], v[1], v[2] - 3.0];
        let half = [1.0, 0.5, 0.25];
        let axis = (0..3).find(|a| nrm[*a].abs() > 0.99).unwrap();
        assert!((local[axis] - half[axis] * nrm[axis]).abs() < 1e-4, "{v:?} {nrm:?}");
    }
    // The same seed the same points.
    let again = points(&mut rig, "/pts");
    let again = xyz(&rig, &again);
    assert_eq!(again, xyz(&rig, &p));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// The channels of `path` once it has them, and after `frames` more cooks.
fn chans(rig: &mut Rig, path: &str) -> Chans {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        rig.gpu.poll();
        if let Some(d) = rig.cooker.output(Port { node: rig.id(path), output: 0 }) {
            let s = rig.cooker.status(rig.id(path)).unwrap();
            assert_eq!((s.error, s.warning), (None, None), "{path}");
            return d.get::<Chans>().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "{path} has no channels: {:?}", rig.cooker.status(rig.id(path)));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn value(c: &Chans, name: &str) -> f64 {
    c.channels().iter().find(|ch| ch.name.as_str() == name).unwrap_or_else(|| panic!("no {name}")).value()
}

#[test]
fn an_attribute_comes_out_as_channels() {
    let Some(gpu) = gpu() else { return };
    let mut ops = line("/a", 5, 0.0, 4.0);
    ops.extend([
        create("/c", "chan.from_pts", &[("attribute", "P".into()), ("points", Value::Int(2))]),
        connect("/a", "/c", 0),
    ]);
    let mut rig = Rig::new(gpu, None, &ops);
    let c = chans(&mut rig, "/c");
    assert_eq!(value(&c, "count"), 5.0);
    assert_eq!((value(&c, "tx0"), value(&c, "tx1")), (0.0, 1.0));
    assert_eq!((value(&c, "tx_mean"), value(&c, "tx_min"), value(&c, "tx_max")), (2.0, 0.0, 4.0));
    assert!(c.channels().iter().all(|ch| ch.name.as_str() != "tx2"), "two points only");

    // A count on the GPU: the summary goes over the live points only.
    rig.edit(&[
        create(
            "/del",
            "pts.delete",
            &[("by", "index".into()), ("first", Value::Int(3)), ("last", Value::Int(4))],
        ),
        connect("/a", "/del", 0),
        connect("/del", "/c", 0),
    ]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let c = loop {
        let c = chans(&mut rig, "/c");
        if value(&c, "count") == 3.0 || Instant::now() > deadline {
            break c;
        }
    };
    assert_eq!((value(&c, "count"), value(&c, "tx_mean"), value(&c, "tx_max")), (3.0, 1.0, 2.0));

    // No points left: count 0, the summary zeros, the first points zeros.
    rig.edit(&[set("/del.first", Value::Int(0))]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let c = loop {
        let c = chans(&mut rig, "/c");
        if value(&c, "count") == 0.0 || Instant::now() > deadline {
            break c;
        }
    };
    assert_eq!((value(&c, "count"), value(&c, "tx_mean"), value(&c, "tx0")), (0.0, 0.0, 0.0));

    // A scalar by its own name, a standard one the points lack by its default.
    rig.edit(&[
        connect("/a", "/c", 0),
        set("/c.attribute", "PointScale".into()),
        set("/c.points", Value::Int(1)),
    ]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let c = loop {
        let c = chans(&mut rig, "/c");
        if c.channels().iter().any(|ch| ch.name.as_str() == "scale0") || Instant::now() > deadline {
            break c;
        }
    };
    assert_eq!((value(&c, "scale0"), value(&c, "scale_max")), (1.0, 1.0));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

fn table(rig: &mut Rig, path: &str) -> Table {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        rig.clock.advance(1);
        rig.cook(path);
        rig.gpu.poll();
        if let Some(d) = rig.cooker.output(Port { node: rig.id(path), output: 0 }) {
            return d.get::<Table>().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "{path} has no table");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn points_go_into_a_table_and_come_back() {
    let Some(gpu) = gpu() else { return };
    let mut ops = line("/a", 4, -1.0, 2.0);
    ops.extend([
        create("/colour", "pts.random", &[]),
        connect("/a", "/colour", 0),
        create("/t", "table.from_pts", &[]),
        connect("/colour", "/t", 0),
        create("/back", "pts.from_table", &[]),
        connect("/t", "/back", 0),
    ]);
    let mut rig = Rig::new(gpu, None, &ops);
    let src = points(&mut rig, "/colour");
    let t = table(&mut rig, "/t");
    assert_eq!(t.rows().len(), 5, "a row of names and four points");
    let header: Vec<&str> = t.rows()[0].iter().map(String::as_str).collect();
    assert!(["tx", "ty", "tz", "r", "a", "u"].iter().all(|n| header.contains(n)), "{header:?}");
    let tx = header.iter().position(|n| *n == "tx").unwrap();
    assert!(t.rows()[2][tx].parse::<f32>().unwrap().abs() < 1e-6, "{:?}", t.rows()[2]);
    let back = points(&mut rig, "/back");
    assert_eq!(back.count().known(), Some(4));
    for name in ["position", "uv", "color"] {
        assert_eq!(values(&rig, &back, name), values(&rig, &src, name), "{name}");
    }

    // A typed table: names as a person writes them, missing cells take the default.
    rig.edit(&[
        create(
            "/typed",
            "table.text",
            &[("text", "x,y,z,Color(0),size\n1,2,3,0.5,7\n4,5\n".into()), ("format", "csv".into())],
        ),
        connect("/typed", "/back", 0),
    ]);
    let p = points(&mut rig, "/back");
    assert_eq!(p.count().known(), Some(2));
    assert_eq!(xyz(&rig, &p), [[1.0, 2.0, 3.0], [4.0, 5.0, 0.0]]);
    assert_eq!(values(&rig, &p, "color")[0], [0.5, 1.0, 1.0, 1.0]);
    assert_eq!(values(&rig, &p, "size"), [vec![7.0], vec![0.0]]);
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn channels_become_points_by_their_names() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create(
            "/c",
            "chan.constant",
            &[
                ("rows", Value::Int(3)),
                ("name0", "tx".into()),
                ("value0", Value::Float(2.0)),
                ("name1", "P2".into()),
                ("value1", Value::Float(-1.0)),
                ("name2", "level".into()),
                ("value2", Value::Float(0.25)),
            ],
        ),
        create("/p", "pts.from_chan", &[]),
        connect("/c", "/p", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let p = points(&mut rig, "/p");
    assert_eq!(p.count().known(), Some(1));
    assert_eq!(xyz(&rig, &p), [[2.0, 0.0, -1.0]]);
    assert_eq!(values(&rig, &p, "level"), [vec![0.25]]);
}

#[test]
fn names_find_their_attributes() {
    use crate::pts::from_values::target;
    let t = target;
    let is = |a: &str, k: usize| Some((a.to_string(), k));
    assert_eq!(t("x"), is("position", 0));
    assert_eq!(t("tz"), is("position", 2));
    assert_eq!(t("P(1)"), is("position", 1));
    assert_eq!(t("P1"), is("position", 1));
    assert_eq!(t("position.z"), is("position", 2));
    assert_eq!(t("Color[3]"), is("color", 3));
    assert_eq!(t("a"), is("color", 3));
    assert_eq!(t("velocity_y"), is("velocity", 1));
    assert_eq!(t("PartVel(2)"), is("velocity", 2));
    assert_eq!(t("near_velocity_x"), is("near_velocity", 0));
    assert_eq!(t("chan1"), is("chan1", 0));
    assert_eq!(t("age"), is("age", 0));
    assert_eq!((t("index"), t("1st"), t("a b")), (None, None, None));
}
