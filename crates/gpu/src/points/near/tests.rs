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
/// it found and the first `list` of their indices.
fn walker(list: usize) -> String {
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
        if c < {list}u {{
            list[i * {list}u + c] = n.index;
        }}
        c += 1u;
    }}
    found[i] = c;
}}
",
        query_wgsl(4)
    )
}

/// What the GPU found per point: the count and the first [`LIST`] indices, sorted.
fn search(gpu: &Gpu, p: &Points, radius: f32, cap: u32) -> (Vec<Vec<u32>>, Vec<u32>, u32) {
    search_listing(gpu, p, radius, cap, LIST)
}

/// [`search`] listing `list` neighbours per point: fewer keep the list of many points within the
/// largest buffer a GPU binds.
fn search_listing(
    gpu: &Gpu,
    p: &Points,
    radius: f32,
    cap: u32,
    list: usize,
) -> (Vec<Vec<u32>>, Vec<u32>, u32) {
    let mut builder = NearGrid::new();
    let n = p.count().at_most() as usize;
    let Some(near) = builder.build(gpu, p, radius, cap).unwrap() else {
        return (vec![Vec::new(); n], vec![0; n], 0);
    };
    let program = gpu.cached_compute("near test", &walker(list), &[ParamSpec::int("n", 0)]).unwrap();
    let listed = list;
    let found = Buffer::of::<u32>(gpu, n as u64, "found").unwrap();
    let list = Buffer::of::<u32>(gpu, (n * listed) as u64, "list").unwrap();
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
            let mut l: Vec<u32> = all[i * listed..i * listed + (counts[i] as usize).min(listed)].to_vec();
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

/// The CPU's answer as [`reference`] gives it, through a grid of cubes of the radius: fast enough
/// for a hundred thousand points in a debug build.
fn reference_grid(places: &[[f32; 3]], radius: f32) -> Vec<(Vec<u32>, Vec<u32>)> {
    use std::collections::HashMap;
    let r2 = radius * radius;
    let cube = |p: &[f32; 3]| p.map(|v| (v / radius).floor() as i64);
    let mut cubes: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, p) in places.iter().enumerate() {
        cubes.entry(cube(p)).or_default().push(i as u32);
    }
    places
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut sure = Vec::new();
            let mut edge = Vec::new();
            let c = cube(p);
            for d in 0..27i64 {
                let at = [c[0] + d % 3 - 1, c[1] + (d / 3) % 3 - 1, c[2] + d / 9 - 1];
                for &j in cubes.get(&at).map(Vec::as_slice).unwrap_or(&[]) {
                    if j as usize == i {
                        continue;
                    }
                    let q = places[j as usize];
                    let d: f32 = (0..3).map(|k| (q[k] - p[k]) * (q[k] - p[k])).sum();
                    if (d - r2).abs() <= 1e-5 * r2 {
                        edge.push(j);
                    } else if d < r2 {
                        sure.push(j);
                    }
                }
            }
            (sure, edge)
        })
        .collect()
}

/// `places` turned by `angle` about the axis `(1, 2, 3)` (Rodrigues).
fn tilted(places: &[[f32; 3]], angle: f32) -> Vec<[f32; 3]> {
    let l = 14f32.sqrt();
    let k = [1.0 / l, 2.0 / l, 3.0 / l];
    let (s, c) = angle.sin_cos();
    places
        .iter()
        .map(|v| {
            let cross = [k[1] * v[2] - k[2] * v[1], k[2] * v[0] - k[0] * v[2], k[0] * v[1] - k[1] * v[0]];
            let dot = k[0] * v[0] + k[1] * v[1] + k[2] * v[2];
            std::array::from_fn(|i| v[i] * c + cross[i] * s + k[i] * dot * (1.0 - c))
        })
        .collect()
}

/// Points on a line, on a flat sheet, on a sheet turned out of the axes, on a sphere's shell and
/// in a random cloud: every one finds all its neighbours, and no cube is over the cap, since none
/// holds more than a few points.
#[test]
fn lines_sheets_and_shells_find_every_neighbour() {
    let Some(gpu) = gpu() else { return };
    let line: Vec<[f32; 3]> = (0..20_000).map(|i| [i as f32 * 0.01, 0.0, 0.0]).collect();
    let sheet: Vec<[f32; 3]> =
        (0..300 * 300).map(|i| [(i % 300) as f32 * 0.01, (i / 300) as f32 * 0.01, 0.0]).collect();
    let turned = tilted(&sheet, 0.7);
    // A Fibonacci sphere: about 2 cm between points, about a dozen within 4 cm.
    let shell: Vec<[f32; 3]> = (0..30_000)
        .map(|i| {
            let z = 1.0 - 2.0 * (i as f32 + 0.5) / 30_000.0;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let a = i as f32 * 2.399_963;
            [r * a.cos(), r * a.sin(), z]
        })
        .collect();
    let cloud_points = random(41, 50_000, [0.0; 3], 1.0);
    let cases = [
        (&line, 0.015, "line"),
        (&sheet, 0.015, "sheet"),
        (&turned, 0.015, "turned sheet"),
        (&shell, 0.04, "shell"),
        (&cloud_points, 0.06, "cloud"),
    ];
    // Points that miss a neighbour and points over the cap, per case.
    let mut wrong = Vec::new();
    for (places, radius, what) in cases {
        let p = cloud(&gpu, places, places.len(), false);
        let (got, _, crowded) = search(&gpu, &p, radius, 64);
        let want = reference_grid(places, radius);
        let missed =
            want.iter().zip(&got).filter(|((sure, _), g)| !sure.iter().all(|j| g.contains(j))).count();
        if (missed, crowded) != (0, 0) {
            wrong.push((what, missed, crowded));
        }
    }
    assert!(wrong.is_empty(), "(case, points that miss a neighbour, points over the cap): {wrong:?}");
    for (places, radius, what) in cases {
        let (got, counts, _) = search(&gpu, &cloud(&gpu, places, places.len(), false), radius, 64);
        agrees(&got, &counts, &reference_grid(places, radius), what);
    }
}

/// A picture of a million pixels turned into points on the floor. Each point has 8 neighbours at
/// most, so the test lists 12: a list of 48 for a million points would be 192 MB, more than many
/// GPUs bind at once (128 MiB is common).
#[test]
fn a_million_points_on_a_sheet_find_every_neighbour() {
    let Some(gpu) = gpu() else { return };
    let sheet: Vec<[f32; 3]> =
        (0..1000 * 1000).map(|i| [(i % 1000) as f32 * 0.01, (i / 1000) as f32 * 0.01, 0.0]).collect();
    let p = cloud(&gpu, &sheet, sheet.len(), false);
    let (got, counts, crowded) = search_listing(&gpu, &p, 0.015, 64, 12);
    assert_eq!(crowded, 0);
    assert!(counts.iter().all(|c| *c <= 8), "a point of the sheet has more than 8 neighbours");
    agrees(&got, &counts, &reference_grid(&sheet, 0.015), "a million");
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
    // The cubes are counted from the middle of the first, middle and last point: with all three
    // at the clump's low corner, the clump is in one cube.
    for i in [0, 150, 299] {
        places[i] = [0.2 - 0.001, -0.1 - 0.001, 0.05 - 0.001];
    }
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

/// A first point parked far away (as a hidden particle may be) does not throw the others into
/// one cube.
#[test]
fn a_far_first_point_does_not_crowd_the_others() {
    let Some(gpu) = gpu() else { return };
    let mut places = random(12, 2000, [0.5, 0.0, 0.2], 1.0);
    places[0] = [-1.0e6, -1.0e6, -1.0e6];
    check(&gpu, &places, places.len(), false, 0.12, "far first point");
    // The same on the GPU's count, where the last live point is not the last of the buffers.
    check(&gpu, &places, 1500, true, 0.12, "far first point, count on the GPU");
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
    // A radius of nothing finds nothing, also for points on top of each other.
    let same = [[0.5, 0.5, 0.5]; 3];
    for radius in [0.0, -1.0, f32::NAN, f32::INFINITY, 1e-45] {
        let (got, _, _) = search(&gpu, &cloud(&gpu, &same, 3, false), radius, 64);
        assert!(got.iter().all(Vec::is_empty), "radius {radius}: {got:?}");
    }
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
    let mut over = 0;
    for h in 0..(cells.len() - HEAD as usize) / 2 {
        let (s, e) = (cells[HEAD as usize + 2 * h] as usize, cells[HEAD as usize + 2 * h + 1] as usize);
        assert!(sorted[s..e].windows(2).all(|w| w[0][3] < w[1][3]), "bucket {h}");
        over += e.saturating_sub(s).saturating_sub(32);
    }
    // Every point past the cap of its bucket is counted: the clump's, and a scattered point's
    // that shares the clump's bucket.
    let crowded = first.crowded().read::<u32>(&gpu).unwrap()[0] as usize;
    assert_eq!(crowded, over);
    assert!((300 - 32..300 - 32 + 4).contains(&crowded), "{crowded}");
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

/// Points in buffers of `capacity`, at most `at_most` alive on a count on the GPU, all alive.
fn bounded(gpu: &Gpu, places: &[[f32; 3]], capacity: u32, at_most: u32) -> Points {
    let mut pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    pos.resize(capacity as usize, [0.0; 4]);
    let b = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "position").unwrap();
    b.write(gpu, 0, &pos).unwrap();
    let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
    header.write(gpu, 0, &[at_most, 0, 0, 0]).unwrap();
    Points::new(capacity, 0, vec![Attr::new("position", AttrType::Vec3, b).unwrap()])
        .unwrap()
        .with_count(Count::gpu(header, at_most, Seen::default()).unwrap())
        .unwrap()
}

/// A count on the GPU whose bound moves from build to build, as particles' does, keeps the
/// grid's buffers.
#[test]
fn a_moving_bound_keeps_the_buffers() {
    let Some(gpu) = gpu() else { return };
    let places = random(51, 1200, [0.0; 3], 1.0);
    let mut grid = NearGrid::new();
    let a = grid.build(&gpu, &bounded(&gpu, &places, 2048, 1000), 0.1, 64).unwrap().unwrap();
    let (cells, sorted) = (a.cells().clone(), a.points().clone());
    drop(a);
    let b = grid.build(&gpu, &bounded(&gpu, &places, 2048, 1100), 0.1, 64).unwrap().unwrap();
    assert!(b.points().same(&sorted) && b.cells().same(&cells), "the same buffers");
    // Still right: the points past the old bound are found.
    let p = bounded(&gpu, &places, 2048, 1100);
    let (got, counts, _) = search(&gpu, &p, 0.1, 64);
    agrees(&got, &counts, &reference(&places, 1100, 0.1), "moved bound");
}

/// A search looks at no more than 27 cubes of `cap` points per point: the cap shrinks with the
/// number of points, so a frame of a dense crowd stays short (2^31 looks at most).
#[test]
fn the_cap_shrinks_so_a_search_stays_short() {
    let Some(gpu) = gpu() else { return };
    let places = random(61, 200_000, [0.0; 3], 1.0);
    let p = cloud(&gpu, &places, places.len(), false);
    let near = NearGrid::new().build(&gpu, &p, 0.05, MAX_PER_CELL).unwrap().unwrap();
    let looks = 27 * u64::from(near.cap()) * 200_000;
    assert!(looks <= 1 << 31, "cap {}: {looks} looks", near.cap());
    assert!(near.cap() >= 350, "no lower than it must: {}", near.cap());
    assert_eq!(cloud(&gpu, &places[..1000], 1000, false).capacity(), 1000);
    let few = NearGrid::new().build(&gpu, &cloud(&gpu, &places[..1000], 1000, false), 0.05, MAX_PER_CELL);
    assert_eq!(few.unwrap().unwrap().cap(), MAX_PER_CELL, "few points keep the whole cap");
}
