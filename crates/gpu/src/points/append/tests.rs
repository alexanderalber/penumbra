//! Births on WARP: where new points land, what they take from their source, the ids, and a set
//! that fills up.

use super::*;
use crate::points::{Attr, Count, Seen};
use crate::testing::gpu;

fn buffer(gpu: &Gpu, words: &[u32]) -> Buffer {
    let b = Buffer::of::<u32>(gpu, words.len() as u64, "test").unwrap();
    b.write(gpu, 0, words).unwrap();
    b
}

fn floats(gpu: &Gpu, v: &[f32]) -> Buffer {
    buffer(gpu, &v.iter().map(|f| f.to_bits()).collect::<Vec<_>>())
}

/// A set of `capacity` points, `count` of them alive (on the GPU): positions, ids and a
/// colour.
fn state(gpu: &Gpu, capacity: u32, count: u32) -> Points {
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, floats(gpu, &vec![-1.0; capacity as usize * 4])).unwrap(),
        Attr::new("id", AttrType::U32, buffer(gpu, &vec![9999; capacity as usize])).unwrap(),
        Attr::new("color", AttrType::Vec4, floats(gpu, &vec![0.5; capacity as usize * 4])).unwrap(),
        Attr::new("heat", AttrType::F32, floats(gpu, &vec![7.0; capacity as usize])).unwrap(),
    ];
    let header = buffer(gpu, &[count, 0, 0, 0]);
    Points::new(capacity, 0, attrs)
        .unwrap()
        .with_count(Count::gpu(header, capacity, Seen::default()).unwrap())
        .unwrap()
}

/// Four source points at x = 10, 11, 12, 13, with a colour of their x.
fn source(gpu: &Gpu) -> Points {
    let pos: Vec<f32> = (0..4).flat_map(|i| [10.0 + i as f32, 0.0, 0.0, 1.0]).collect();
    let col: Vec<f32> = (0..4).flat_map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, floats(gpu, &pos)).unwrap(),
        Attr::new("color", AttrType::Vec4, floats(gpu, &col)).unwrap(),
    ];
    Points::new(4, 4, attrs).unwrap()
}

fn read(gpu: &Gpu, p: &Points, name: &str) -> Vec<u32> {
    p.attr(name).unwrap().buffer().read::<u32>(gpu).unwrap()
}

fn header(gpu: &Gpu, p: &Points) -> u32 {
    p.count().header().unwrap().read::<u32>(gpu).unwrap()[0]
}

#[test]
fn births_land_after_the_count_and_stop_at_the_room() {
    let Some(gpu) = gpu() else { return };
    let src = source(&gpu);
    let set = state(&gpu, 10, 3);
    let ids = buffer(&gpu, &[100, 0, 0, 0]);
    let none = |_: &str| None;
    let births = |wanted, room| Births {
        source: &src,
        wanted,
        per_point: false,
        pick: Pick::Cycle,
        salt: 0,
        room,
        params: &[],
        param: &none,
    };
    let items = [Item::new("id", Value::Expr { wgsl: "id".into(), reads: vec![] })];
    append(&gpu, &set, &ids, &births(5, 10), &items).unwrap();
    assert_eq!(header(&gpu, &set), 8);
    assert_eq!(ids.read::<u32>(&gpu).unwrap()[0], 105);
    let id = read(&gpu, &set, "id");
    assert_eq!(id[..9], [9999, 9999, 9999, 100, 101, 102, 103, 104, 9999], "the points alive stay");
    let pos: Vec<f32> = read(&gpu, &set, "P").iter().map(|w| f32::from_bits(*w)).collect();
    let xs: Vec<f32> = (3..8).map(|j| pos[j * 4]).collect();
    assert_eq!(xs, [10.0, 11.0, 12.0, 13.0, 10.0], "birth t from source point t % 4");
    assert_eq!(pos[3 * 4 + 3], 1.0, "a position's fourth number is 1");
    let col: Vec<f32> = read(&gpu, &set, "color").iter().map(|w| f32::from_bits(*w)).collect();
    assert_eq!(col[4 * 4], 1.0, "the colour of source point 1 is copied");
    let heat: Vec<f32> = read(&gpu, &set, "heat").iter().map(|w| f32::from_bits(*w)).collect();
    assert_eq!(
        (heat[2], heat[5]),
        (7.0, 0.0),
        "an attribute the source lacks: 0 for a name penumbra does not know"
    );

    // Two more fit; the other three are not born, and the ids do not count them.
    append(&gpu, &set, &ids, &births(5, 10), &items).unwrap();
    assert_eq!(header(&gpu, &set), 10);
    assert_eq!(ids.read::<u32>(&gpu).unwrap()[0], 107);
    assert_eq!(read(&gpu, &set, "id")[8..10], [105, 106]);
    // Full: nothing changes.
    append(&gpu, &set, &ids, &births(5, 10), &items).unwrap();
    assert_eq!((header(&gpu, &set), ids.read::<u32>(&gpu).unwrap()[0]), (10, 107));
    // A room below the capacity holds the count there.
    let set = state(&gpu, 10, 3);
    append(&gpu, &set, &ids, &births(5, 6), &items).unwrap();
    assert_eq!(header(&gpu, &set), 6);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn per_point_and_random_sources_and_expressions_of_the_source() {
    let Some(gpu) = gpu() else { return };
    let src = source(&gpu);
    let none = |_: &str| None;
    let lift = [ParamSpec::float("lift", 0.0)];
    let param = |n: &str| (n == "lift").then_some(ParamValue::Float(0.5));
    // Two births per source point; a position worked out from the source's.
    let set = state(&gpu, 16, 0);
    let ids = buffer(&gpu, &[0, 0, 0, 0]);
    let births = Births {
        source: &src,
        wanted: 2,
        per_point: true,
        pick: Pick::Cycle,
        salt: 0,
        room: 16,
        params: &lift,
        param: &param,
    };
    let items = [Item::new(
        "position",
        Value::Expr {
            wgsl: "src_position + vec3f(0.0, 0.0, params.lift + f32(born))".into(),
            reads: vec!["P".into()],
        },
    )];
    assert_eq!(births.most(), 8);
    append(&gpu, &set, &ids, &births, &items).unwrap();
    assert_eq!(header(&gpu, &set), 8);
    let pos: Vec<f32> = read(&gpu, &set, "position").iter().map(|w| f32::from_bits(*w)).collect();
    for t in 0..8 {
        assert_eq!((pos[t * 4], pos[t * 4 + 2]), (10.0 + (t % 4) as f32, 0.5 + t as f32), "{t}");
    }
    // At random: every birth from one of the sources, the same for the same salt.
    let pick = |salt| {
        let set = state(&gpu, 64, 0);
        let ids = buffer(&gpu, &[0, 0, 0, 0]);
        let births = Births {
            source: &src,
            wanted: 64,
            per_point: false,
            pick: Pick::Random,
            salt,
            room: 64,
            params: &[],
            param: &none,
        };
        append(&gpu, &set, &ids, &births, &[]).unwrap();
        let pos: Vec<f32> = read(&gpu, &set, "position").iter().map(|w| f32::from_bits(*w)).collect();
        (0..64).map(|t| pos[t * 4]).collect::<Vec<f32>>()
    };
    let a = pick(1);
    assert!(a.iter().all(|x| [10.0, 11.0, 12.0, 13.0].contains(x)), "{a:?}");
    for x in [10.0, 11.0, 12.0, 13.0] {
        assert!(a.iter().filter(|v| **v == x).count() >= 6, "each source now and then: {a:?}");
    }
    assert_eq!(a, pick(1));
    assert_ne!(a, pick(2));
    // No source points: nothing is born.
    let empty = Points::new(4, 0, src.attrs().to_vec()).unwrap();
    let set = state(&gpu, 8, 2);
    let births = Births {
        source: &empty,
        wanted: 3,
        per_point: false,
        pick: Pick::Cycle,
        salt: 0,
        room: 8,
        params: &[],
        param: &none,
    };
    append(&gpu, &set, &ids, &births, &[]).unwrap();
    assert_eq!(header(&gpu, &set), 2);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
