//! Points: the alias table, what a set of points accepts, and attributes turned into pictures.

use super::*;
use crate::testing::gpu;

#[test]
fn touchdesigner_names_mean_penumbra_attributes() {
    assert_eq!(canonical("P"), "position");
    assert_eq!(canonical(" Color "), "color");
    assert_eq!(canonical("PointScale"), "scale");
    assert_eq!(canonical("PartLifeSpan"), "life");
    assert_eq!(canonical("position"), "position");
    assert_eq!(canonical("my_attr"), "my_attr");
    // Exactly as TouchDesigner writes them: `p` is no alias.
    assert_eq!(canonical("p"), "p");
    for (td, ours) in ALIASES {
        let s = default_of(td).unwrap_or_else(|| panic!("{td} means {ours}, which has no default"));
        assert_eq!(s.name, *ours);
    }
    assert_eq!(default_of("Color").map(|s| s.default), Some([1.0; 4]));
    assert_eq!(default_of("scale").map(|s| (s.ty, s.default[0])), Some((AttrType::F32, 1.0)));
    assert!(default_of("nothing").is_none());
    // Every standard name is unique and usable in a program.
    for (k, s) in STANDARD.iter().enumerate() {
        assert!(valid_name(s.name), "{}", s.name);
        assert!(!STANDARD.iter().skip(k + 1).any(|t| t.name == s.name), "{}", s.name);
    }
    assert!(!valid_name("2d") && !valid_name("a b") && !valid_name(""));
}

#[test]
fn points_check_their_attributes() {
    let Some(gpu) = gpu() else { return };
    let vec3 = |n: u64| Buffer::of::<[f32; 4]>(&gpu, n, "p").unwrap();
    let position = Attr::new("P", AttrType::Vec3, vec3(8)).unwrap();
    assert_eq!(position.name(), "position");
    assert_eq!(position.len(), 8);
    assert!(Attr::new("not a name", AttrType::F32, vec3(1)).is_err());
    let p = Points::new(8, 5, vec![position.clone()]).unwrap();
    assert_eq!((p.capacity(), p.count().known(), p.count().at_most()), (8, Some(5), 5));
    assert!(p.attr("P").is_some_and(|a| a.buffer().same(position.buffer())));
    assert!(p.attr("color").is_none());
    // More points than room, a buffer too small, two of one name.
    assert!(Points::new(4, 5, vec![]).is_err());
    assert!(Points::new(9, 5, vec![position.clone()]).is_err());
    let twice = Attr::new("position", AttrType::Vec3, vec3(8)).unwrap();
    assert!(Points::new(8, 5, vec![position.clone(), twice.clone()]).is_err());
    // A new attribute of a name replaces the old one, the others stay the same buffers.
    let scale = Attr::new("PointScale", AttrType::F32, Buffer::of::<f32>(&gpu, 8, "s").unwrap()).unwrap();
    let q = p.with_attr(scale).unwrap().with_attr(twice.clone()).unwrap();
    assert_eq!(q.attrs().len(), 2);
    assert!(q.attr("position").is_some_and(|a| a.buffer().same(twice.buffer())));
    assert!(q.attr("scale").is_some());
    assert!(
        p.attr("position").is_some_and(|a| a.buffer().same(position.buffer())),
        "the input is left alone"
    );
    assert!(q.without("PointScale").attr("scale").is_none());
    let short = Attr::new("color", AttrType::Vec4, vec3(4)).unwrap();
    assert!(p.with_attr(short).is_err());
    // Dims that do not multiply to the count are dropped.
    assert_eq!(p.clone().with_dims(Some([5, 1, 1])).dims(), Some([5, 1, 1]));
    assert_eq!(p.with_dims(Some([2, 2, 1])).dims(), None);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_ring_hands_out_buffers_nobody_holds() {
    let Some(gpu) = gpu() else { return };
    let mut ring = BufferRing::new();
    let a = ring.next(&gpu, 64, "a").unwrap();
    let b = ring.next(&gpu, 64, "b").unwrap();
    assert!(!a.same(&b), "a is still held");
    drop(a);
    let c = ring.next(&gpu, 64, "c").unwrap();
    assert!(ring.iter().any(|x| x.same(&c)));
    assert_eq!(ring.bytes(), 128, "the dropped one came back");
    let d = ring.next(&gpu, 32, "d").unwrap();
    assert_eq!(ring.bytes(), 32, "another size replaces them");
    drop((b, c, d));
    ring.trim();
    assert_eq!(ring.bytes(), 0);
}

#[test]
fn attributes_become_pictures_of_one_texel_per_point() {
    let Some(gpu) = gpu() else { return };
    assert_eq!(texel_size(1), (16, 1));
    assert_eq!(texel_size(20), (32, 1));
    assert_eq!(texel_size(1024), (1024, 1));
    assert_eq!(texel_size(1030), (1024, 2));
    // Two rows: a whole one and a part of the next, from buffers larger than the count.
    let (n, capacity) = (1030u32, 1100u64);
    let position: Vec<[f32; 4]> = (0..capacity).map(|i| [i as f32, -(i as f32), 0.5, 1.0]).collect();
    let scale: Vec<f32> = (0..capacity).map(|i| i as f32 * 0.25).collect();
    let uv: Vec<[f32; 2]> = (0..capacity).map(|i| [i as f32, 2.0]).collect();
    let id: Vec<i32> = (0..capacity).map(|i| -(i as i32)).collect();
    let buffer = |bytes: u64| Buffer::new(&gpu, bytes, "attr").unwrap();
    let p_buf = buffer(capacity * 16);
    p_buf.write(&gpu, 0, &position).unwrap();
    let s_buf = buffer(capacity * 4);
    s_buf.write(&gpu, 0, &scale).unwrap();
    let uv_buf = buffer(capacity * 8);
    uv_buf.write(&gpu, 0, &uv).unwrap();
    let id_buf = buffer(capacity * 4);
    id_buf.write(&gpu, 0, &id).unwrap();
    let points = Points::new(
        capacity as u32,
        n,
        vec![
            Attr::new("position", AttrType::Vec3, p_buf).unwrap(),
            Attr::new("scale", AttrType::F32, s_buf).unwrap(),
            Attr::new("uv", AttrType::Vec2, uv_buf).unwrap(),
            Attr::new("offset", AttrType::I32, id_buf).unwrap(),
        ],
    )
    .unwrap();
    let mut ring = TexRing::new();
    let mut scratch = BufferRing::new();
    let mut read = |name: &str| {
        let tex = points.attr_texture(&gpu, name, &mut ring, &mut scratch).unwrap().unwrap();
        assert_eq!(tex.size(), (1024, 2));
        assert_eq!(tex.format(), FLOAT_FORMAT);
        gpu.read(&tex).unwrap()
    };
    let at = |i: u32| (i % 1024, i / 1024);
    let px = read("P");
    for i in [0, 1, 1023, 1024, 1029] {
        let (x, y) = at(i);
        assert_eq!(px.get(x, y), Some([i as f32, -(i as f32), 0.5, 1.0]), "position {i}");
    }
    let px = read("scale");
    for i in [0, 7, 1023, 1029] {
        let (x, y) = at(i);
        let s = i as f32 * 0.25;
        assert_eq!(px.get(x, y), Some([s, s, s, 1.0]), "scale {i}");
    }
    let px = read("uv");
    let (x, y) = at(1029);
    assert_eq!(px.get(x, y), Some([1029.0, 2.0, 0.0, 1.0]));
    let px = read("offset");
    assert_eq!(px.get(5, 0), Some([-5.0, -5.0, -5.0, 1.0]));
    // Nothing to draw: no attribute, no points.
    assert!(points.attr_texture(&gpu, "color", &mut ring, &mut scratch).unwrap().is_none());
    assert!(Points::empty().attr_texture(&gpu, "position", &mut ring, &mut scratch).unwrap().is_none());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn replacing_or_removing_positions_drops_the_bounds() {
    let Some(gpu) = gpu() else { return };
    let vec3 = || Buffer::of::<[f32; 4]>(&gpu, 4, "p").unwrap();
    let position = Attr::new("position", AttrType::Vec3, vec3()).unwrap();
    let bounds = penumbra_mesh::Bounds::of([[0.0; 3], [1.0; 3]]);
    let p = Points::new(4, 4, vec![position]).unwrap().with_bounds(bounds);
    let normal = Attr::new("normal", AttrType::Vec3, vec3()).unwrap();
    assert!(p.with_attr(normal).unwrap().bounds().is_some(), "another attribute keeps them");
    assert!(p.without("normal").bounds().is_some());
    let moved = Attr::new("P", AttrType::Vec3, vec3()).unwrap();
    assert!(p.with_attr(moved).unwrap().bounds().is_none(), "new positions, no box known");
    assert!(p.without("P").bounds().is_none(), "no positions, no box");
}

#[test]
fn a_ring_whose_buffers_are_all_held_forgets_the_oldest() {
    let Some(gpu) = gpu() else { return };
    let mut ring = BufferRing::new();
    let held: Vec<Buffer> = (0..3).map(|_| ring.next(&gpu, 16, "held").unwrap()).collect();
    let fourth = ring.next(&gpu, 16, "fourth").unwrap();
    assert!(held.iter().all(|b| !b.same(&fourth)));
    assert_eq!(ring.bytes(), 48, "still three");
    assert!(!ring.iter().any(|b| b.same(&held[0])), "the oldest is forgotten, its holder keeps it");
    assert!(ring.iter().any(|b| b.same(&fourth)));
}

#[test]
fn every_count_and_type_lands_on_its_texel() {
    let Some(gpu) = gpu() else { return };
    let mut ring = TexRing::new();
    let mut scratch = BufferRing::new();
    // One point; part of a row; exactly two whole rows. As u32 (expanded) and as vec3f (copied).
    for n in [1u32, 20, 2048] {
        let ids: Vec<u32> = (0..n).map(|i| i * 3).collect();
        let id = Buffer::new(&gpu, u64::from(n) * 4, "id").unwrap();
        id.write(&gpu, 0, &ids).unwrap();
        let pos: Vec<[f32; 4]> = (0..n).map(|i| [i as f32, 1.0, 2.0, 1.0]).collect();
        let p = Buffer::new(&gpu, u64::from(n) * 16, "p").unwrap();
        p.write(&gpu, 0, &pos).unwrap();
        let points = Points::new(
            n,
            n,
            vec![
                Attr::new("id", AttrType::U32, id).unwrap(),
                Attr::new("position", AttrType::Vec3, p).unwrap(),
            ],
        )
        .unwrap();
        let (w, h) = texel_size(n);
        for name in ["id", "position"] {
            let tex = points.attr_texture(&gpu, name, &mut ring, &mut scratch).unwrap().unwrap();
            assert_eq!(tex.size(), (w, h), "{n}");
            let px = gpu.read(&tex).unwrap();
            for i in [0, n / 2, n - 1] {
                let want = if name == "id" {
                    let v = (i * 3) as f32;
                    [v, v, v, 1.0]
                } else {
                    [i as f32, 1.0, 2.0, 1.0]
                };
                assert_eq!(px.get(i % w, i / w), Some(want), "{name} {i} of {n}");
            }
        }
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// A readback gives its own count once it arrived; the one after it shows the earlier count
/// until its own arrives, and never passes it off as its own.
#[test]
fn seen_counts_arrive_as_their_own() {
    let Some(gpu) = gpu() else { return };
    let header = Buffer::of::<u32>(&gpu, 4, "count").unwrap();
    header.write(&gpu, 0, &[5u32, 0, 0, 0]).unwrap();
    let a = Seen::reading(&gpu, &header, None).unwrap();
    // A blocking read waits for the GPU, which brings the earlier readback in too.
    header.read::<u32>(&gpu).unwrap();
    assert_eq!((a.get(), a.arrived()), (Some(5), Some(5)));
    header.write(&gpu, 0, &[7u32, 0, 0, 0]).unwrap();
    let b = Seen::reading(&gpu, &header, Some(&a)).unwrap();
    assert_eq!(a.arrived(), Some(5), "the earlier one keeps its value when its readback is reused");
    if b.arrived().is_none() {
        assert_eq!(b.get(), Some(5), "the earlier count until its own arrives");
    }
    header.read::<u32>(&gpu).unwrap();
    assert_eq!((b.get(), b.arrived()), (Some(7), Some(7)));
    assert_eq!(Seen::default().arrived(), None);
}
