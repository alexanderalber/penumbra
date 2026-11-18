//! Compaction on WARP against a reference on the CPU, and what reads a count on the GPU: programs,
//! pictures, the count read back.

use std::time::{Duration, Instant};

use super::*;
use crate::points::program::{self, Decl, Outputs, Program, Run, Shape, Spec};
use crate::points::{AttrType, capacity_for};
use crate::testing::gpu;

/// One attribute per type, then more of them until there are `extra` beyond what one gather
/// pass binds: element i of attribute k holds numbers made from i and k.
fn points(gpu: &Gpu, n: u32, capacity: u32, extra: usize) -> Points {
    let types = [AttrType::F32, AttrType::Vec2, AttrType::Vec3, AttrType::Vec4, AttrType::U32, AttrType::I32];
    let total = (attrs_per_pass(gpu) + extra).max(types.len());
    let mut attrs = Vec::new();
    for k in 0..total {
        let ty = if k == 0 { AttrType::Vec3 } else { types[k % types.len()] };
        let words = (ty.stride() / 4) as u32;
        let data: Vec<u32> = (0..capacity * words).map(|w| word(k, w / words, w % words)).collect();
        let b = Buffer::of::<u32>(gpu, u64::from(capacity * words), "attr").unwrap();
        b.write(gpu, 0, &data).unwrap();
        let name = if k == 0 { "position".to_string() } else { format!("a{k}") };
        attrs.push(Attr::new(&name, ty, b).unwrap());
    }
    Points::new(capacity, n, attrs).unwrap().with_bounds(penumbra_mesh::Bounds::of([[0.0; 3], [1.0; 3]]))
}

fn word(k: usize, i: u32, w: u32) -> u32 {
    (k as u32) << 24 | i << 2 | w
}

fn flags(gpu: &Gpu, keep: &[bool]) -> Buffer {
    let words: Vec<u32> = keep.iter().map(|k| u32::from(*k)).collect();
    let b = Buffer::of::<u32>(gpu, words.len().max(1) as u64, "keep").unwrap();
    b.write(gpu, 0, &words).unwrap();
    b
}

fn header(gpu: &Gpu, p: &Points) -> u32 {
    p.count().header().unwrap().read::<u32>(gpu).unwrap()[0]
}

/// Every attribute of `out` holds, at its first places, the elements of `input` at `kept`.
fn check(gpu: &Gpu, input: &Points, out: &Points, kept: &[u32]) {
    assert_eq!(header(gpu, out), kept.len() as u32);
    assert_eq!(out.attrs().len(), input.attrs().len(), "every attribute moves");
    for a in input.attrs() {
        let b = out.attr(a.name()).unwrap();
        assert_eq!(b.ty(), a.ty());
        let words = (a.ty().stride() / 4) as usize;
        let src = a.buffer().read::<u32>(gpu).unwrap();
        let dst = b.buffer().read::<u32>(gpu).unwrap();
        for (j, i) in kept.iter().enumerate() {
            let (i, j) = (*i as usize, j);
            assert_eq!(
                dst[j * words..(j + 1) * words],
                src[i * words..(i + 1) * words],
                "{} at {j}",
                a.name()
            );
        }
    }
}

#[test]
fn compaction_keeps_the_order_and_every_attribute() {
    let Some(gpu) = gpu() else { return };
    // Not a multiple of the block, three blocks, two attributes more than one pass moves.
    let (n, capacity) = (2500u32, 2600u32);
    let input = points(&gpu, n, capacity, 2);
    assert!(input.attrs().len() > attrs_per_pass(&gpu));
    let mut c = Compactor::new();
    type Keep = Box<dyn Fn(u32) -> bool>;
    let cases: [(&str, Keep); 5] = [
        ("none", Box::new(|_| false)),
        ("all", Box::new(|_| true)),
        ("one", Box::new(|i| i == 1777)),
        ("odd", Box::new(|i| i % 2 == 1)),
        ("random", Box::new(|i: u32| (i.wrapping_mul(2654435761) >> 7) % 7 < 3)),
    ];
    for (name, keep) in &cases {
        // Flags past the count say keep: those points must stay out all the same.
        let wanted: Vec<bool> = (0..capacity).map(|i| i >= n || keep(i)).collect();
        let out = c.compact(&gpu, &input, &flags(&gpu, &wanted)).unwrap();
        let kept: Vec<u32> = (0..n).filter(|i| keep(*i)).collect();
        check(&gpu, &input, &out, &kept);
        assert_eq!(
            (out.capacity(), out.count().at_most(), out.count().known()),
            (capacity, n, None),
            "{name}"
        );
        assert_eq!(out.bounds(), input.bounds(), "the old box still holds the points");
        assert_eq!((out.dims(), out.strips()), (None, crate::points::Strips::None));
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_compacted_set_compacts_again_within_its_count() {
    let Some(gpu) = gpu() else { return };
    let (n, capacity) = (1500u32, 1500u32);
    let input = points(&gpu, n, capacity, 0);
    let mut first = Compactor::new();
    let mut second = Compactor::new();
    let a =
        first.compact(&gpu, &input, &flags(&gpu, &(0..n).map(|i| i % 3 != 0).collect::<Vec<_>>())).unwrap();
    let kept_a: Vec<u32> = (0..n).filter(|i| i % 3 != 0).collect();
    // Every flag set: only the 1000 live points of `a` come out, not the stale ones behind them.
    let b = second.compact(&gpu, &a, &flags(&gpu, &vec![true; n as usize])).unwrap();
    check(&gpu, &input, &b, &kept_a);
    // Every second one of those.
    let c = second.compact(&gpu, &a, &flags(&gpu, &(0..n).map(|j| j % 2 == 0).collect::<Vec<_>>())).unwrap();
    let kept_c: Vec<u32> = kept_a.iter().copied().step_by(2).collect();
    check(&gpu, &input, &c, &kept_c);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// `n` points at (i, 0, 0) with scale i, in buffers of `capacity`.
fn simple(gpu: &Gpu, n: u32, capacity: u32) -> Points {
    let pos: Vec<[f32; 4]> = (0..capacity).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
    let scale: Vec<f32> = (0..capacity).map(|i| i as f32).collect();
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "p").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let s = Buffer::of::<f32>(gpu, u64::from(capacity), "s").unwrap();
    s.write(gpu, 0, &scale).unwrap();
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, p).unwrap(),
        Attr::new("scale", AttrType::F32, s).unwrap(),
    ];
    Points::new(capacity, n, attrs).unwrap()
}

#[test]
fn programs_and_pictures_stop_at_a_count_on_the_gpu() {
    let Some(gpu) = gpu() else { return };
    let (n, capacity) = (40u32, 64u32);
    let input = simple(&gpu, n, capacity);
    let keep: Vec<bool> = (0..capacity).map(|i| i % 4 == 0).collect();
    let mut c = Compactor::new();
    let live = c.compact(&gpu, &input, &flags(&gpu, &keep)).unwrap();
    assert_eq!(header(&gpu, &live), 10);

    // `count` and `index01` are the live ones; nothing past the count is written.
    let spec = Spec {
        label: "test".into(),
        source: "fn main() { weight = f32(count) + index01; }".into(),
        decls: vec![Decl::set("weight")],
        params: vec![],
    };
    let generated = program::generate(&spec, &Shape::of(&live), program::storage_budget(&gpu)).unwrap();
    let program = Program::compile(&gpu, generated).unwrap();
    let mut outputs = Outputs::default();
    let param = |_: &str| None;
    let run = Run { seconds: 0.0, frame: 0, dt: 0.0, param: &param, pictures: &[] };
    let out = program.run(&gpu, &live, &mut outputs, &run).unwrap();
    assert_eq!(out.count(), live.count(), "the count passes on");
    let weight = out.attr("weight").unwrap().buffer().read::<f32>(&gpu).unwrap();
    for (i, w) in weight.iter().enumerate() {
        let want = if i < 10 { 10.0 + i as f32 / 9.0 } else { 0.0 };
        assert!((w - want).abs() < 1e-5, "{i}: {w}");
    }

    // A picture: the live points, then transparent black.
    let mut textures = crate::TexRing::new();
    let mut scratch = crate::BufferRing::new();
    let tex = live.attr_picture(&gpu, "scale", (8, 8), &mut textures, &mut scratch).unwrap();
    let px = gpu.read(&tex).unwrap();
    for j in 0..64u32 {
        let want = if j < 10 { [(j * 4) as f32, (j * 4) as f32, (j * 4) as f32, 1.0] } else { [0.0; 4] };
        assert_eq!(px.get(j % 8, j / 8), Some(want), "{j}");
    }
    // The drawing's picture too, sized by the capacity.
    let pos = live.attr_texture(&gpu, "position", &mut textures, &mut scratch).unwrap().unwrap();
    assert_eq!(pos.size(), crate::points::texel_size(capacity));
    let px = gpu.read(&pos).unwrap();
    assert_eq!(px.get(3, 0), Some([12.0, 0.0, 0.0, 1.0]));
    assert_eq!(px.get(10, 0), Some([0.0; 4]));
    assert_eq!(px.get(63, 0), Some([0.0; 4]));
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn the_count_comes_back_without_a_wait() {
    let Some(gpu) = gpu() else { return };
    let input = simple(&gpu, 100, 100);
    let mut c = Compactor::new();
    let out = c.compact(&gpu, &input, &flags(&gpu, &(0..100).map(|i| i < 37).collect::<Vec<_>>())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while out.count().seen().is_none() {
        gpu.poll();
        assert!(Instant::now() < deadline, "the count never came back");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(out.count().seen(), Some(37));
    // The next one shows the last value until its own arrives.
    let next = c.compact(&gpu, &input, &flags(&gpu, &(0..100).map(|i| i < 5).collect::<Vec<_>>())).unwrap();
    assert!(matches!(next.count().seen(), Some(37 | 5)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while next.count().seen() != Some(5) {
        gpu.poll();
        assert!(Instant::now() < deadline, "the second count never came back");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn capacities_round_up_so_small_changes_keep_them() {
    let most = u32::MAX;
    assert_eq!(capacity_for(0, most), 0);
    assert_eq!(capacity_for(1, most), 1024);
    assert_eq!(capacity_for(1024, most), 1024);
    assert_eq!(capacity_for(1025, most), 2048);
    assert_eq!(capacity_for(20_000, most), 20_480);
    assert_eq!(capacity_for(20_400, most), 20_480);
    assert_eq!(capacity_for(1_000_000, most), 1_048_576);
    for n in [3u32, 999, 9_000, 77_777, 1_000_001, 4_194_304, u32::MAX - 5] {
        let c = capacity_for(n, most);
        assert!(c >= n, "{n}");
        if n > 8192 {
            assert!(u64::from(c) * 4 < u64::from(n) * 5 + 4, "{n}: {c} is more than a quarter over");
        }
    }
    assert_eq!(capacity_for(5000, 4000), 5000, "never below the count");
    assert_eq!(capacity_for(3000, 3050), 3050);
}
