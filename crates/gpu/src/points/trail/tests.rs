//! Trails on WARP against a model on the CPU: a fixed set matched by index, a set whose points
//! are born and die matched by id with its count on the GPU, steps that do not shift, the
//! attribute along the trails and the cap.

use std::collections::HashMap;

use super::*;
use crate::points::Seen;
use crate::testing::gpu;

/// Points at `places` with ids `ids`, in buffers of `capacity`; their count on the GPU when
/// `on_gpu` (more room than points, as particles have).
fn set(gpu: &Gpu, places: &[[f32; 3]], ids: &[u32], capacity: u32, on_gpu: bool) -> Points {
    let n = places.len() as u32;
    let mut pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    pos.resize(capacity as usize, [9e9; 4]);
    let mut words: Vec<u32> = ids.to_vec();
    words.resize(capacity as usize, 0xdead);
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "position").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let i = Buffer::of::<u32>(gpu, u64::from(capacity), "id").unwrap();
    i.write(gpu, 0, &words).unwrap();
    let attrs =
        vec![Attr::new("position", AttrType::Vec3, p).unwrap(), Attr::new("id", AttrType::U32, i).unwrap()];
    if !on_gpu {
        return Points::new(capacity, n, attrs).unwrap();
    }
    let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
    header.write(gpu, 0, &[n, 0, 0, 0]).unwrap();
    Points::new(capacity, 0, attrs)
        .unwrap()
        .with_count(Count::gpu(header, capacity, Seen::default()).unwrap())
        .unwrap()
}

fn live(gpu: &Gpu, p: &Points) -> usize {
    match p.count().header() {
        Some(h) => h.read::<u32>(gpu).unwrap()[0] as usize,
        None => p.count().known().unwrap() as usize,
    }
}

/// The trails as read back: per trail its id and its places, head first.
fn read(gpu: &Gpu, p: &Points, len: usize) -> Vec<(u32, Vec<[f32; 3]>)> {
    let n = live(gpu, p);
    assert_eq!(n % len, 0, "whole trails");
    let pos = p.attr("position").unwrap().buffer().read::<[f32; 4]>(gpu).unwrap();
    let ids = p.attr("id").unwrap().buffer().read::<u32>(gpu).unwrap();
    (0..n / len)
        .map(|t| {
            let places: Vec<[f32; 3]> =
                (0..len).map(|k| pos[t * len + k]).map(|v| [v[0], v[1], v[2]]).collect();
            let id = ids[t * len];
            assert!(ids[t * len..(t + 1) * len].iter().all(|i| *i == id), "one id along trail {t}");
            (id, places)
        })
        .collect()
}

/// The CPU's trails: a step from `before` (id to places, head first).
fn model(
    before: &HashMap<u32, Vec<[f32; 3]>>,
    now: &[(u32, [f32; 3])],
    len: usize,
    shift: bool,
) -> Vec<(u32, Vec<[f32; 3]>)> {
    now.iter()
        .map(|(id, p)| {
            let places = match before.get(id) {
                None => vec![*p; len],
                Some(old) => {
                    let rest = if shift { &old[..len - 1] } else { &old[1..] };
                    std::iter::once(*p).chain(rest.iter().copied()).collect()
                }
            };
            (*id, places)
        })
        .collect()
}

fn settings(len: u32, key: Key, shift: bool) -> Settings {
    Settings { len, max: 1_000_000, key, shift, attrs: Vec::new() }
}

#[test]
fn a_fixed_set_trails_its_places_by_index() {
    let Some(gpu) = gpu() else { return };
    let mut t = Trailer::new();
    let len = 4usize;
    let mut before: Option<Points> = None;
    let mut cpu: HashMap<u32, Vec<[f32; 3]>> = HashMap::new();
    for step in 0..7u32 {
        let now: Vec<(u32, [f32; 3])> =
            (0..3u32).map(|i| (i, [step as f32, i as f32, 0.5 * step as f32])).collect();
        let places: Vec<[f32; 3]> = now.iter().map(|(_, p)| *p).collect();
        // The ids are not a key here: they only label the points for the check.
        let input = set(&gpu, &places, &[0, 1, 2], 3, false);
        let shift = step % 3 != 2;
        let set = Settings { attrs: vec!["id".into()], ..settings(len as u32, Key::Index, shift) };
        let out = t.step(&gpu, &input, before.as_ref(), &set, None).unwrap();
        let want = model(&cpu, &now, len, shift);
        assert_eq!(read(&gpu, &out, len), want, "step {step}");
        assert_eq!(
            (out.count().known(), out.dims(), out.strips()),
            (Some(12), Some([3, 4, 1]), Strips::regular(4))
        );
        let along = out.attr(ALONG).unwrap().buffer().read::<f32>(&gpu).unwrap();
        let third = 1.0 / 3.0;
        assert_eq!(along[..4], [0.0, third, 2.0 * third, 1.0]);
        assert_eq!(along[8..12], [0.0, third, 2.0 * third, 1.0]);
        cpu = want.into_iter().collect();
        before = Some(out);
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn points_that_come_and_go_keep_their_own_trails_by_id() {
    let Some(gpu) = gpu() else { return };
    let mut t = Trailer::new();
    let len = 5usize;
    let mut before: Option<Points> = None;
    let mut cpu: HashMap<u32, Vec<[f32; 3]>> = HashMap::new();
    // Ids rising along the list as particles have them: some die at every step, new ones are
    // born at the end.
    let mut alive: Vec<u32> = (0..6).collect();
    let mut next = 6u32;
    for step in 0..12u32 {
        if step > 0 {
            alive.retain(|id| (id * 7 + step) % 5 != 0);
            for _ in 0..(step % 3) {
                alive.push(next);
                next += 1;
            }
        }
        let now: Vec<(u32, [f32; 3])> =
            alive.iter().map(|id| (*id, [*id as f32 + 0.25, step as f32, 1.0])).collect();
        let places: Vec<[f32; 3]> = now.iter().map(|(_, p)| *p).collect();
        let input = set(&gpu, &places, &alive, 16, true);
        let out = t
            .step(&gpu, &input, before.as_ref(), &settings(len as u32, Key::Attr("id".into()), true), None)
            .unwrap();
        let got = read(&gpu, &out, len);
        let want = model(&cpu, &now, len, true);
        assert_eq!(got, want, "step {step}");
        // No trail runs through the origin or through another point's places.
        for (id, places) in &got {
            assert!(places.iter().all(|p| p[0] == *id as f32 + 0.25), "trail {id}: {places:?}");
        }
        assert_eq!(out.strips(), Strips::regular(len as u32), "kept with a count on the GPU");
        assert_eq!((out.count().known(), out.count().at_most(), out.capacity()), (None, 16 * 5, 16 * 5));
        cpu = want.into_iter().collect();
        before = Some(out);
    }
    // At most `max` trails: the first points keep theirs.
    let input = set(&gpu, &[[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]], &[0, 1, 2], 3, false);
    let capped = Settings { max: 2, ..settings(3, Key::Attr("id".into()), true) };
    let out = t.step(&gpu, &input, None, &capped, None).unwrap();
    assert_eq!((out.count().known(), out.capacity()), (Some(6), 6));
    assert_eq!(read(&gpu, &out, 3)[1], (1, vec![[2.0, 0.0, 0.0]; 3]));
    // A key that is no whole number is refused in words.
    let mut floats = input.clone();
    let b = Buffer::of::<f32>(&gpu, 3, "f").unwrap();
    floats = floats.with_attr(Attr::new("id", AttrType::F32, b).unwrap()).unwrap();
    let e = t.step(&gpu, &floats, None, &capped, None).err().unwrap();
    assert!(e.0.contains("whole number"), "{}", e.0);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
