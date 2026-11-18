//! Lines between near points on WARP against a model on the CPU: the nearest few per point, a
//! pair once or from both ends, the shortest line, the cap on all lines, the length per line, a
//! count on the GPU, a random choice that stays with the pair, and the same lines from the same
//! points, bit for bit.

use super::*;
use crate::points::{HEADER_BYTES, Seen};
use crate::testing::gpu;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn random(seed: u64, n: usize) -> Vec<[f32; 3]> {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    (0..n).map(|_| std::array::from_fn(|_| r.next() * 2.0 - 1.0)).collect()
}

/// Points at `places` with `tag` (their index) and `color`, the first `live` alive; their count
/// on the GPU when `on_gpu`.
fn cloud(gpu: &Gpu, places: &[[f32; 3]], live: usize, on_gpu: bool) -> Points {
    let capacity = places.len() as u32;
    let pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "position").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let tags: Vec<u32> = (0..capacity).collect();
    let t = Buffer::of::<u32>(gpu, u64::from(capacity), "tag").unwrap();
    t.write(gpu, 0, &tags).unwrap();
    let colors: Vec<[f32; 4]> = (0..capacity).map(|i| [i as f32, 0.5, 0.25, 1.0]).collect();
    let c = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "color").unwrap();
    c.write(gpu, 0, &colors).unwrap();
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, p).unwrap(),
        Attr::new("tag", AttrType::U32, t).unwrap(),
        Attr::new("color", AttrType::Vec4, c).unwrap(),
    ];
    if !on_gpu {
        return Points::new(capacity, live as u32, attrs).unwrap();
    }
    let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
    header.write(gpu, 0, &[live as u32, 0, 0, 0]).unwrap();
    Points::new(capacity, 0, attrs)
        .unwrap()
        .with_count(Count::gpu(header, capacity, Seen::default()).unwrap())
        .unwrap()
}

fn settings(distance: f32, most: u32) -> Settings {
    Settings {
        distance,
        min_distance: 0.0,
        most,
        choose: Choose::Nearest,
        both: false,
        max: 1_000_000,
        cap: 64,
        attrs: vec!["tag".into(), "color".into()],
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]) * (a[k] - b[k])).sum::<f32>().sqrt()
}

/// The lines the CPU expects, in order: per point its choices, the nearest first.
fn model(places: &[[f32; 3]], live: usize, set: &Settings) -> Vec<(u32, u32)> {
    let chosen: Vec<Vec<u32>> = (0..live)
        .map(|i| {
            let mut c: Vec<(f32, u32)> = (0..live)
                .filter(|j| *j != i)
                .map(|j| (dist(places[i], places[j]), j as u32))
                .filter(|(d, _)| *d <= set.distance && *d >= set.min_distance)
                .collect();
            for (d, j) in &c {
                for edge in [set.distance, set.min_distance] {
                    assert!(
                        (d - edge).abs() > 1e-6 * set.distance,
                        "pair {i} {j} at the edge: choose another seed"
                    );
                }
            }
            c.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            c.truncate(set.most as usize);
            c.into_iter().map(|(_, j)| j).collect()
        })
        .collect();
    let mut lines = Vec::new();
    for (i, mine) in chosen.iter().enumerate() {
        for j in mine {
            if set.both || (i as u32) < *j || !chosen[*j as usize].contains(&(i as u32)) {
                lines.push((i as u32, *j));
            }
        }
    }
    lines.truncate(set.max as usize);
    lines
}

/// The lines as made: pairs of tags, read through the ends.
fn lines(gpu: &Gpu, out: &Points) -> Vec<(u32, u32)> {
    let words = out.count().header().unwrap().read::<u32>(gpu).unwrap();
    assert_eq!(words[0], 2 * words[1], "two ends per line");
    let tags = out.attr("tag").unwrap().buffer().read::<u32>(gpu).unwrap();
    (0..words[1] as usize).map(|k| (tags[2 * k], tags[2 * k + 1])).collect()
}

#[test]
fn each_point_joins_its_nearest_and_a_pair_is_one_line() {
    let Some(gpu) = gpu() else { return };
    for seed in [1u64, 2, 3] {
        let places = random(seed, 400);
        let input = cloud(&gpu, &places, places.len(), false);
        let mut pairer = Pairer::new();
        for (most, both) in [(3u32, false), (3, true), (1, false), (8, false)] {
            let set = Settings { both, ..settings(0.2, most) };
            let out = pairer.pairs(&gpu, &input, &set).unwrap();
            assert_eq!(out.strips(), Strips::Pairs);
            assert_eq!(
                lines(&gpu, &out),
                model(&places, places.len(), &set),
                "seed {seed}, {most} lines, both {both}"
            );
        }
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn each_end_carries_its_point_and_the_length_of_its_line() {
    let Some(gpu) = gpu() else { return };
    let places = random(7, 300);
    let input = cloud(&gpu, &places, places.len(), false);
    let set = Settings { min_distance: 0.08, ..settings(0.25, 4) };
    let out = Pairer::new().pairs(&gpu, &input, &set).unwrap();
    let got = lines(&gpu, &out);
    assert_eq!(got, model(&places, places.len(), &set));
    let pos = out.attr("position").unwrap().buffer().read::<[f32; 4]>(&gpu).unwrap();
    let color = out.attr("color").unwrap().buffer().read::<[f32; 4]>(&gpu).unwrap();
    let span = out.attr(SPAN).unwrap().buffer().read::<f32>(&gpu).unwrap();
    let span01 = out.attr(SPAN01).unwrap().buffer().read::<f32>(&gpu).unwrap();
    for (k, (a, b)) in got.iter().enumerate() {
        for (end, i) in [(2 * k, *a), (2 * k + 1, *b)] {
            let p = places[i as usize];
            assert_eq!([pos[end][0], pos[end][1], pos[end][2]], p, "line {k}");
            assert_eq!(color[end][0], i as f32, "line {k}: the colour of its point");
            let d = dist(places[*a as usize], places[*b as usize]);
            assert!((span[end] - d).abs() < 1e-5, "line {k}: {} for {d}", span[end]);
            assert!((span01[end] - d / 0.25).abs() < 1e-5);
            assert!(span[end] >= 0.08 - 1e-6, "no line shorter than the least");
        }
    }
    assert_eq!(out.bounds(), input.bounds());
}

#[test]
fn the_most_lines_cut_the_rest_and_say_how_many_were_wanted() {
    let Some(gpu) = gpu() else { return };
    let places = random(4, 300);
    let input = cloud(&gpu, &places, places.len(), false);
    let all = model(&places, places.len(), &settings(0.2, 3));
    assert!(all.len() > 100);
    let set = Settings { max: 100, ..settings(0.2, 3) };
    let out = Pairer::new().pairs(&gpu, &input, &set).unwrap();
    assert_eq!(out.capacity(), 200);
    assert_eq!(lines(&gpu, &out), all[..100]);
    let words = out.count().header().unwrap().read::<u32>(&gpu).unwrap();
    assert_eq!((words[1], words[2], words[3]), (100, 0, all.len() as u32));
}

#[test]
fn a_count_on_the_gpu_and_none_one_two_points() {
    let Some(gpu) = gpu() else { return };
    let places = random(5, 500);
    let set = settings(0.2, 3);
    let out = Pairer::new().pairs(&gpu, &cloud(&gpu, &places, 320, true), &set).unwrap();
    assert_eq!(lines(&gpu, &out), model(&places, 320, &set));
    let mut pairer = Pairer::new();
    let none = pairer.pairs(&gpu, &cloud(&gpu, &places, 0, false), &set).unwrap();
    assert_eq!((none.count().known(), none.strips()), (Some(0), Strips::Pairs));
    assert!(none.attr(SPAN).is_some() && none.attr("tag").is_some(), "the attributes are there all the same");
    let one = pairer.pairs(&gpu, &cloud(&gpu, &places[..1], 1, false), &set).unwrap();
    assert!(lines(&gpu, &one).is_empty());
    let two = [[0.0, 0.0, 0.0], [0.1, 0.0, 0.0]];
    let out = pairer.pairs(&gpu, &cloud(&gpu, &two, 2, false), &set).unwrap();
    assert_eq!(lines(&gpu, &out), vec![(0, 1)]);
    let out =
        pairer.pairs(&gpu, &cloud(&gpu, &two, 2, false), &Settings { both: true, ..set.clone() }).unwrap();
    assert_eq!(lines(&gpu, &out), vec![(0, 1), (1, 0)]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_random_choice_stays_with_the_pair() {
    let Some(gpu) = gpu() else { return };
    let places = random(6, 300);
    let set = Settings { choose: Choose::Random { seed: 3 }, ..settings(0.3, 2) };
    let mut pairer = Pairer::new();
    let a = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &places, 300, false), &set).unwrap());
    // Every point moved a hair: the nearest would change here and there, the random choice not.
    let moved: Vec<[f32; 3]> = places.iter().map(|p| [p[0] + 1e-4, p[1] - 1e-4, p[2]]).collect();
    let b = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &moved, 300, false), &set).unwrap());
    assert_eq!(a, b);
    let other = Settings { choose: Choose::Random { seed: 4 }, ..set };
    let c = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &places, 300, false), &other).unwrap());
    assert_ne!(a, c, "another seed chooses others");
}

#[test]
fn the_same_points_give_the_same_lines_bit_for_bit() {
    let Some(gpu) = gpu() else { return };
    let mut places = random(8, 1500);
    // A clump over the cap of a cube.
    for p in places.iter_mut().take(200) {
        *p = [0.5 + p[0] * 0.001, 0.5 + p[1] * 0.001, 0.5 + p[2] * 0.001];
    }
    let input = cloud(&gpu, &places, places.len(), false);
    let set = Settings { cap: 32, ..settings(0.15, 5) };
    let a = Pairer::new().pairs(&gpu, &input, &set).unwrap();
    let b = Pairer::new().pairs(&gpu, &input, &set).unwrap();
    for name in ["position", "tag", "color", SPAN, SPAN01] {
        let x = a.attr(name).unwrap().buffer().read::<u32>(&gpu).unwrap();
        let y = b.attr(name).unwrap().buffer().read::<u32>(&gpu).unwrap();
        assert!(x == y, "{name}");
    }
    let words = a.count().header().unwrap().read::<u32>(&gpu).unwrap();
    assert_eq!(words, b.count().header().unwrap().read::<u32>(&gpu).unwrap());
    assert!(words[2] > 0, "the clump is over the cap: {words:?}");
}
