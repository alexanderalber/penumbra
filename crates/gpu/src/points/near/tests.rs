//! The neighbour search on WARP against a brute-force search on the CPU: random points of several
//! seeds, a lattice whose spacing is the radius, a clump in one cube with the cap, negative and far
//! coordinates with points that are no numbers, a count on the GPU, 0, 1 and 2 points, a set large
//! enough for a sort in three passes, and the same grid from the same input, bit for bit.

use super::*;
use crate::points::{Attr, Count, HEADER_BYTES, Seen};
use crate::testing::gpu;

/// Neighbours a query lists per point in the test.
const LIST: usize = 48;

/// A program that walks the neighbours of every live point (in index order) and writes how many
/// it found and the first [`LIST`] of their indices.
fn walker() -> String {
    format!(
        "{}
@group(2) @binding(0) var<storage, read> position: array<vec4f>;
@group(2) @binding(1) var<storage, read_write> found: array<u32>;
@group(2) @binding(2) var<storage, read_write> list: array<u32>;
@group(2) @binding(3) var<uniform> header: vec4u;

@compute @workgroup_size(64)
fn walk(@builtin(global_invocation_id) id: vec3u) {{
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || i >= min(header.x, u32(params.n)) {{
        return;
    }}
    var c = 0u;
    var n = near_begin(position[i].xyz, i);
    while near_next(&n) {{
        if c < {LIST}u {{
            list[i * {LIST}u + c] = n.index;
        }}
        c += 1u;
    }}
    found[i] = c;
}}
",
        query_wgsl(4)
    )
}

/// What the GPU found per point: the count and the indices listed, sorted.
fn search(gpu: &Gpu, p: &Points, radius: f32, cap: u32) -> (Vec<Vec<u32>>, Vec<u32>, u32) {
    let mut builder = NearGrid::new();
    let n = p.count().at_most() as usize;
    let Some(near) = builder.build(gpu, p, radius, cap).unwrap() else {
        return (vec![Vec::new(); n], vec![0; n], 0);
    };
    let program = gpu.cached_compute("near test", &walker(), &[ParamSpec::int("n", 0)]).unwrap();
    let found = Buffer::of::<u32>(gpu, n as u64, "found").unwrap();
    let list = Buffer::of::<u32>(gpu, (n * LIST) as u64, "list").unwrap();
    let header = p.count().header_for(gpu).unwrap();
    let param = |name: &str| (name == "n").then_some(Value::Int(n as i64));
    let input = Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
    program
        .run_with(
            gpu,
            &[p.attr("position").unwrap().buffer(), &found, &list, &header, near.cells(), near.points()],
            &[Step { entry: "walk", out: grid(n as u32), input: &input }],
        )
        .unwrap();
    let counts = found.read::<u32>(gpu).unwrap();
    let all = list.read::<u32>(gpu).unwrap();
    let lists = (0..n)
        .map(|i| {
            let mut l: Vec<u32> = all[i * LIST..i * LIST + (counts[i] as usize).min(LIST)].to_vec();
            l.sort_unstable();
            l
        })
        .collect();
    let crowded = near.crowded().read::<u32>(gpu).unwrap()[0];
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
    (lists, counts, crowded)
}

fn finite(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

/// The CPU's answer per live point: the neighbours surely within the radius, and those at its
/// edge that either answer may hold.
fn reference(places: &[[f32; 3]], live: usize, radius: f32) -> Vec<(Vec<u32>, Vec<u32>)> {
    let r2 = radius * radius;
    (0..live)
        .map(|i| {
            let mut sure = Vec::new();
            let mut edge = Vec::new();
            if !finite(places[i]) {
                return (sure, edge);
            }
            for (j, q) in places.iter().enumerate().take(live) {
                if j == i || !finite(*q) {
                    continue;
                }
                let d: f32 = (0..3).map(|k| (q[k] - places[i][k]) * (q[k] - places[i][k])).sum();
                if (d - r2).abs() <= 1e-5 * r2 {
                    edge.push(j as u32);
                } else if d < r2 {
                    sure.push(j as u32);
                }
            }
            (sure, edge)
        })
        .collect()
}

/// Every live point found its sure neighbours and nothing beyond its edge ones.
fn agrees(got: &[Vec<u32>], counts: &[u32], want: &[(Vec<u32>, Vec<u32>)], what: &str) {
    for (i, (sure, edge)) in want.iter().enumerate() {
        let g = &got[i];
        assert!(
            counts[i] as usize <= LIST,
            "{what}: point {i} has {} neighbours, more than the test lists",
            counts[i]
        );
        assert!(sure.iter().all(|j| g.contains(j)), "{what}: point {i} misses some of {sure:?}: {g:?}");
        assert!(
            g.iter().all(|j| sure.contains(j) || edge.contains(j)),
            "{what}: point {i} found more than {sure:?} (+ {edge:?}): {g:?}"
        );
        let mut unique = g.clone();
        unique.dedup();
        assert_eq!(unique.len(), g.len(), "{what}: point {i} found one twice: {g:?}");
    }
}

/// Points at `places`, the first `live` alive, in buffers of `capacity`; the count on the GPU when
/// `on_gpu`.
fn cloud(gpu: &Gpu, places: &[[f32; 3]], live: usize, on_gpu: bool) -> Points {
    let capacity = places.len().max(1) as u32;
    let pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    let b = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "position").unwrap();
    b.write(gpu, 0, &pos).unwrap();
    let attrs = vec![Attr::new("position", AttrType::Vec3, b).unwrap()];
    if !on_gpu {
        return Points::new(capacity, live as u32, attrs).unwrap();
    }
    let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
    header.write(gpu, 0, &[live as u32, 0, 0, 0]).unwrap();
    Points::new(capacity, 0, attrs)
        .unwrap()
        .with_count(Count::gpu(header, places.len() as u32, Seen::default()).unwrap())
        .unwrap()
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn random(seed: u64, n: usize, centre: [f32; 3], half: f32) -> Vec<[f32; 3]> {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    (0..n).map(|_| std::array::from_fn(|k| centre[k] + (r.next() * 2.0 - 1.0) * half)).collect()
}

fn check(gpu: &Gpu, places: &[[f32; 3]], live: usize, on_gpu: bool, radius: f32, what: &str) {
    let p = cloud(gpu, places, live, on_gpu);
    let (got, counts, crowded) = search(gpu, &p, radius, 64);
    assert_eq!(crowded, 0, "{what}: no bucket is over the cap");
    agrees(&got, &counts, &reference(places, live, radius), what);
}

#[test]
fn random_points_find_the_same_neighbours_as_the_cpu() {
    let Some(gpu) = gpu() else { return };
    for seed in 1..=4 {
        // About six neighbours each.
        let places = random(seed, 700, [0.0; 3], 1.0);
        check(&gpu, &places, places.len(), false, 0.15, &format!("seed {seed}"));
    }
    // Flat: all in one plane, as a grid on the floor is.
    let mut flat = random(9, 600, [0.0; 3], 1.0);
    for p in &mut flat {
        p[2] = 0.0;
    }
    check(&gpu, &flat, flat.len(), false, 0.08, "flat");
}

#[test]
fn a_lattice_spaced_by_the_radius_finds_its_six_sides() {
    let Some(gpu) = gpu() else { return };
    for spacing in [0.25f32, 0.1] {
        let mut places = Vec::new();
        for z in 0..6 {
            for y in 0..6 {
                for x in 0..6 {
                    places.push([x as f32 * spacing - 0.5, y as f32 * spacing, z as f32 * spacing]);
                }
            }
        }
        let p = cloud(&gpu, &places, places.len(), false);
        let (got, counts, _) = search(&gpu, &p, spacing, 64);
        let want = reference(&places, places.len(), spacing);
        agrees(&got, &counts, &want, &format!("spacing {spacing}"));
        if spacing == 0.25 {
            // Exact in binary: each point sees exactly the points one step away.
            for (i, g) in got.iter().enumerate() {
                let (x, y, z) = (i % 6, (i / 6) % 6, i / 36);
                let sides = [x > 0, x < 5, y > 0, y < 5, z > 0, z < 5].iter().filter(|s| **s).count();
                assert_eq!(g.len(), sides, "point {i}");
            }
        }
    }
}

#[test]
fn a_clump_in_one_cube_is_cut_at_the_cap_by_index() {
    let Some(gpu) = gpu() else { return };
    let mut places = random(3, 300, [0.2, -0.1, 0.05], 0.001);
    // The cubes are counted from the first point: at the clump's low corner, the clump is in
    // one cube.
    places[0] = [0.2 - 0.001, -0.1 - 0.001, 0.05 - 0.001];
    let p = cloud(&gpu, &places, places.len(), false);
    let cap = 40;
    let (got, counts, crowded) = search(&gpu, &p, 0.5, cap);
    // One bucket of 300: the ones past the first 40 are counted.
    assert_eq!(crowded, 300 - cap);
    for (i, g) in got.iter().enumerate() {
        let want: Vec<u32> = (0..cap).filter(|j| *j as usize != i).collect();
        assert_eq!(g, &want, "point {i} sees the lowest indices of the cube");
        assert_eq!(counts[i] as usize, want.len());
    }
}

#[test]
fn far_and_negative_coordinates_and_points_that_are_no_numbers() {
    let Some(gpu) = gpu() else { return };
    let mut places = random(5, 500, [-5000.0, 3000.0, -12000.0], 0.8);
    // A second clump far from the first, and points without a place.
    places.extend(random(6, 200, [40000.0, -70000.0, 9.0], 0.5));
    places[17] = [f32::NAN, 0.0, 0.0];
    places[300] = [f32::INFINITY, 1.0, 2.0];
    places[650] = [1.0, f32::NEG_INFINITY, f32::NAN];
    check(&gpu, &places, places.len(), false, 0.12, "far");
    // Tiny radius, close to the origin and on both sides of it.
    let near_zero = random(8, 400, [0.0; 3], 0.01);
    check(&gpu, &near_zero, near_zero.len(), false, 0.0015, "small");
}

#[test]
fn a_count_on_the_gpu_leaves_the_dead_points_out() {
    let Some(gpu) = gpu() else { return };
    let places = random(11, 500, [1.0, 2.0, 3.0], 0.7);
    // 300 alive of 500: the 200 behind them are never found.
    check(&gpu, &places, 300, true, 0.15, "gpu count");
    let p = cloud(&gpu, &places, 300, true);
    let (_, counts, _) = search(&gpu, &p, 0.15, 64);
    assert!(counts[300..].iter().all(|c| *c == 0), "nothing ran for the dead points");
}

#[test]
fn none_one_and_two_points() {
    let Some(gpu) = gpu() else { return };
    let none = Points::empty();
    assert!(NearGrid::new().build(&gpu, &none, 0.1, 64).unwrap().is_none());
    let one = cloud(&gpu, &[[0.5, 0.5, 0.5]], 1, false);
    let (got, counts, crowded) = search(&gpu, &one, 0.1, 64);
    assert_eq!((got[0].len(), counts[0], crowded), (0, 0, 0));
    let two = [[0.0, 0.0, 0.0], [0.05, 0.0, 0.0]];
    let (got, _, _) = search(&gpu, &cloud(&gpu, &two, 2, false), 0.1, 64);
    assert_eq!(got, vec![vec![1], vec![0]]);
    let (got, _, _) = search(&gpu, &cloud(&gpu, &two, 2, false), 0.01, 64);
    assert_eq!(got, vec![Vec::<u32>::new(), Vec::new()]);
    // A count on the GPU of none.
    let (got, _, _) = search(&gpu, &cloud(&gpu, &two, 0, true), 0.1, 64);
    assert_eq!(got, vec![Vec::<u32>::new(), Vec::new()]);
    assert!(NearGrid::new().build(&gpu, &one, 0.0, 64).is_err(), "no radius");
    assert!(NearGrid::new().build(&gpu, &one, f32::NAN, 64).is_err(), "no number");
}

#[test]
fn many_points_sort_in_three_passes() {
    let Some(gpu) = gpu() else { return };
    let n = 40_000;
    assert!(table_bits(n as u32) >= 16, "three passes of 8 bits");
    let places = random(21, n, [0.0; 3], 1.0);
    let p = cloud(&gpu, &places, n, false);
    let (got, counts, _) = search(&gpu, &p, 0.05, 64);
    // A sample against the CPU: a brute-force search over all of them is slow on the CPU.
    let r2 = 0.05f32 * 0.05;
    for i in (0..n).step_by(397) {
        let mut sure = Vec::new();
        let mut edge = Vec::new();
        for (j, q) in places.iter().enumerate() {
            let d: f32 = (0..3).map(|k| (q[k] - places[i][k]) * (q[k] - places[i][k])).sum();
            if j == i {
                continue;
            }
            if (d - r2).abs() <= 1e-5 * r2 {
                edge.push(j as u32);
            } else if d < r2 {
                sure.push(j as u32);
            }
        }
        let g = &got[i];
        assert!(counts[i] as usize <= LIST);
        assert!(
            sure.iter().all(|j| g.contains(j)) && g.iter().all(|j| sure.contains(j) || edge.contains(j)),
            "{i}"
        );
    }
}

#[test]
fn the_same_points_give_the_same_grid_bit_for_bit() {
    let Some(gpu) = gpu() else { return };
    // A clump over the cap among scattered points: the order inside a bucket matters there.
    let mut places = random(31, 2000, [0.0; 3], 1.0);
    places.extend(random(32, 300, [0.3, 0.3, 0.3], 0.002));
    let p = cloud(&gpu, &places, places.len(), false);
    let mut a = NearGrid::new();
    let mut b = NearGrid::new();
    let first = a.build(&gpu, &p, 0.1, 32).unwrap().unwrap();
    let second = b.build(&gpu, &p, 0.1, 32).unwrap().unwrap();
    for (x, y) in [(first.cells(), second.cells()), (first.points(), second.points())] {
        assert_eq!(x.read::<u32>(&gpu).unwrap(), y.read::<u32>(&gpu).unwrap());
    }
    // Within a bucket the points are in the order of their index.
    let sorted = first.points().read::<[u32; 4]>(&gpu).unwrap();
    let cells = first.cells().read::<u32>(&gpu).unwrap();
    for h in 0..(cells.len() - HEAD as usize) / 2 {
        let (s, e) = (cells[HEAD as usize + 2 * h] as usize, cells[HEAD as usize + 2 * h + 1] as usize);
        assert!(sorted[s..e].windows(2).all(|w| w[0][3] < w[1][3]), "bucket {h}");
    }
    assert_eq!(first.crowded().read::<u32>(&gpu).unwrap()[0], 300 - 32);
}

#[test]
fn a_prefix_sum_over_many_blocks() {
    let Some(gpu) = gpu() else { return };
    for n in [1u32, 1000, 1024, 300_007] {
        let values: Vec<u32> = (0..n).map(|i| (i.wrapping_mul(2654435761) >> 28) & 7).collect();
        let b = Buffer::of::<u32>(&gpu, u64::from(n), "values").unwrap();
        b.write(&gpu, 0, &values).unwrap();
        let mut builder = NearGrid::new();
        builder.scan(&gpu, &b, n).unwrap();
        let got = b.read::<u32>(&gpu).unwrap();
        let mut at = 0u32;
        for (i, v) in values.iter().enumerate() {
            assert_eq!(got[i], at, "{n}: {i}");
            at += v;
        }
        assert_eq!(builder.total().unwrap().read::<u32>(&gpu).unwrap()[0], at, "{n}");
    }
}
