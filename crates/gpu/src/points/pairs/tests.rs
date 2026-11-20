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
    cut(lines, set.max, |i| i).0
}

/// PCG3D, as `pn_hash` in the shaders.
fn pcg3d(p: [u32; 3]) -> [u32; 3] {
    let mut v = p.map(|x| x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223));
    for round in 0..2 {
        v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
        v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
        v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
        if round == 0 {
            v = v.map(|x| x ^ (x >> 16));
        }
    }
    v
}

/// The key of the cut of a line between the points with keys `a` and `b`: 24 bits, the bin on
/// top.
fn cut_key(a: u32, b: u32) -> u32 {
    pcg3d([a.min(b), a.max(b), 0x6a09_e667])[1] >> 8
}

/// The lines a new cut at `max` keeps, in order, and the most lines that share one key. Where
/// they do not all fit: the lines whose key is below the largest bound that keeps at most 63/64
/// of `max`. `key` gives a point's key from its index.
fn cut(lines: Vec<(u32, u32)>, max: u32, key: impl Fn(u32) -> u32) -> (Vec<(u32, u32)>, u32) {
    let mut per_key = std::collections::BTreeMap::<u32, u32>::new();
    for (i, j) in &lines {
        *per_key.entry(cut_key(key(*i), key(*j))).or_default() += 1;
    }
    let mut bound = 1 << 24;
    if lines.len() as u32 > max {
        let limit = max - max / 64;
        let mut below = 0;
        bound = 0;
        for (k, c) in &per_key {
            if below + c > limit {
                bound = *k;
                break;
            }
            below += c;
        }
    }
    let largest = per_key.values().copied().max().unwrap_or(0);
    (lines.into_iter().filter(|(i, j)| cut_key(key(*i), key(*j)) < bound).collect(), largest)
}

/// Far more lines wanted than `max`: the cut stops inside a bin, so it keeps all but 1/64 of
/// `max`, also where one bin of the 4096 holds more lines than `max`.
#[test]
fn a_cut_far_below_the_lines_wanted_keeps_nearly_max() {
    for (total, max) in [(300_000u32, 1000u32), (5_000_000, 1000), (50_000, 1), (2_000_000, 100_000)] {
        let lines: Vec<(u32, u32)> = (0..total).map(|k| (k, k + 1)).collect();
        let (kept, largest) = cut(lines, max, |i| i);
        let limit = max - max / 64;
        let kept = kept.len() as u32;
        assert!(kept <= limit && kept + largest > limit, "{total} lines, max {max}: {kept} kept");
        assert!(kept * 100 >= max * 97, "{total} lines, max {max}: {kept} kept");
    }
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
            let out = pairer.pairs(&gpu, &input, &set, None).unwrap();
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
    let out = Pairer::new().pairs(&gpu, &input, &set, None).unwrap();
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
    let out = Pairer::new().pairs(&gpu, &input, &set, None).unwrap();
    assert_eq!(out.capacity(), 200);
    let kept = lines(&gpu, &out);
    let (expected, largest) = cut(all.clone(), 100, |i| i);
    assert_eq!(kept, expected);
    assert!(kept.len() <= 99 && kept.len() + largest as usize > 99, "{} lines kept", kept.len());
    assert!(kept.len() >= 97, "{} lines kept", kept.len());
    let words = out.count().header().unwrap().read::<u32>(&gpu).unwrap();
    assert_eq!((words[1], words[2], words[3]), (kept.len() as u32, 0, all.len() as u32));
}

/// Points at `places` with an `id` each, and `tag` the same as the id.
fn cloud_with_ids(gpu: &Gpu, places: &[[f32; 3]], ids: &[u32]) -> Points {
    let n = places.len() as u32;
    let pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(n), "position").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let mut attrs = vec![Attr::new("position", AttrType::Vec3, p).unwrap()];
    for name in ["id", "tag"] {
        let b = Buffer::of::<u32>(gpu, u64::from(n), name).unwrap();
        b.write(gpu, 0, ids).unwrap();
        attrs.push(Attr::new(name, AttrType::U32, b).unwrap());
    }
    Points::new(n, n, attrs).unwrap()
}

/// The lines as pairs of ids, the smaller first.
fn id_pairs(lines: &[(u32, u32)]) -> std::collections::BTreeSet<(u32, u32)> {
    lines.iter().map(|(a, b)| (*a.min(b), *a.max(b))).collect()
}

/// The cut at `max` keeps the same pairs while the points stand, and while they move a little
/// and some die (particles): which lines are kept does not follow the points' order, so it does
/// not jump when a point before them goes. Fewer lines wanted only add lines.
#[test]
fn the_cut_at_max_keeps_the_same_pairs_while_points_move_and_die() {
    let Some(gpu) = gpu() else { return };
    let places = random(11, 3000);
    let ids: Vec<u32> = (0..3000u32).map(|i| i * 7 + 3).collect();
    let all = Settings { attrs: vec!["tag".into()], ..settings(0.2, 3) };
    let mut pairer = Pairer::new();
    let a = cloud_with_ids(&gpu, &places, &ids);
    let wanted_a = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &a, &all, None).unwrap()));
    let set = Settings { max: wanted_a.len() as u32 / 2, ..all.clone() };
    let kept_a = lines(&gpu, &pairer.pairs(&gpu, &a, &set, None).unwrap());
    assert!(kept_a.len() as u32 <= set.max && kept_a.len() as u32 > set.max * 97 / 100, "{}", kept_a.len());
    for _ in 0..3 {
        assert_eq!(lines(&gpu, &pairer.pairs(&gpu, &a, &set, None).unwrap()), kept_a, "the same points");
    }
    let kept_a = id_pairs(&kept_a);
    // One point in twenty dies, the others move a hair and close up behind it, as a compaction
    // leaves them.
    let (places_b, ids_b): (Vec<[f32; 3]>, Vec<u32>) = places
        .iter()
        .zip(&ids)
        .enumerate()
        .filter(|(k, _)| k % 20 != 7)
        .map(|(_, (p, id))| ([p[0] + 2e-4, p[1] - 1e-4, p[2] + 1e-4], *id))
        .unzip();
    let b = cloud_with_ids(&gpu, &places_b, &ids_b);
    let wanted_b = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &b, &all, None).unwrap()));
    let kept_b = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &b, &set, None).unwrap()));
    let both: Vec<&(u32, u32)> = wanted_a.intersection(&wanted_b).collect();
    assert!(both.len() * 10 > wanted_a.len() * 8, "most pairs stay: {} of {}", both.len(), wanted_a.len());
    let lost = both.iter().filter(|p| kept_a.contains(p) && !kept_b.contains(p)).count();
    assert_eq!(lost, 0, "lines kept before that are still wanted stay ({} pairs in both)", both.len());
    assert!(kept_b.len() as u32 <= set.max);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// A few points more or less change no line between the others while the cut at `max` binds:
/// three points born at the front of the list (where the oldest particles sit after a merge),
/// one gone at the back, the rest moved a hair.
#[test]
fn a_small_change_moves_no_other_line_past_the_cut() {
    let Some(gpu) = gpu() else { return };
    let places = random(12, 3000);
    let ids: Vec<u32> = (0..3000u32).map(|i| i * 5 + 1).collect();
    let all = Settings { attrs: vec!["tag".into()], ..settings(0.2, 3) };
    let mut pairer = Pairer::new();
    let a = cloud_with_ids(&gpu, &places, &ids);
    let wanted_a = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &a, &all, None).unwrap()));
    let set = Settings { max: wanted_a.len() as u32 / 3, ..all.clone() };
    let kept_a = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &a, &set, None).unwrap()));
    let mut places_b: Vec<[f32; 3]> = places[..3].iter().map(|p| [p[0] + 0.01, p[1], p[2]]).collect();
    let mut ids_b: Vec<u32> = vec![90_001, 90_002, 90_003];
    for (p, id) in places.iter().zip(&ids).take(2999) {
        places_b.push([p[0] + 1e-5, p[1], p[2] - 1e-5]);
        ids_b.push(*id);
    }
    let b = cloud_with_ids(&gpu, &places_b, &ids_b);
    let wanted_b = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &b, &all, None).unwrap()));
    let kept_b = id_pairs(&lines(&gpu, &pairer.pairs(&gpu, &b, &set, None).unwrap()));
    let both: Vec<&(u32, u32)> = wanted_a.intersection(&wanted_b).collect();
    assert!(
        wanted_b.len() > wanted_a.len(),
        "the new points bring lines: {} {}",
        wanted_a.len(),
        wanted_b.len()
    );
    let switched = both.iter().filter(|p| kept_a.contains(p) != kept_b.contains(p)).count();
    assert_eq!(switched, 0, "of {} pairs in both, {switched} switched", both.len());
    assert!(kept_b.len() as u32 <= set.max);
}

#[test]
fn a_count_on_the_gpu_and_none_one_two_points() {
    let Some(gpu) = gpu() else { return };
    let places = random(5, 500);
    let set = settings(0.2, 3);
    let out = Pairer::new().pairs(&gpu, &cloud(&gpu, &places, 320, true), &set, None).unwrap();
    assert_eq!(lines(&gpu, &out), model(&places, 320, &set));
    let mut pairer = Pairer::new();
    let none = pairer.pairs(&gpu, &cloud(&gpu, &places, 0, false), &set, None).unwrap();
    assert_eq!((none.count().known(), none.strips()), (Some(0), Strips::Pairs));
    assert!(none.attr(SPAN).is_some() && none.attr("tag").is_some(), "the attributes are there all the same");
    let one = pairer.pairs(&gpu, &cloud(&gpu, &places[..1], 1, false), &set, None).unwrap();
    assert!(lines(&gpu, &one).is_empty());
    let two = [[0.0, 0.0, 0.0], [0.1, 0.0, 0.0]];
    let out = pairer.pairs(&gpu, &cloud(&gpu, &two, 2, false), &set, None).unwrap();
    assert_eq!(lines(&gpu, &out), vec![(0, 1)]);
    let out = pairer
        .pairs(&gpu, &cloud(&gpu, &two, 2, false), &Settings { both: true, ..set.clone() }, None)
        .unwrap();
    assert_eq!(lines(&gpu, &out), vec![(0, 1), (1, 0)]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_random_choice_stays_with_the_pair() {
    let Some(gpu) = gpu() else { return };
    let places = random(6, 300);
    let set = Settings { choose: Choose::Random { seed: 3 }, ..settings(0.3, 2) };
    let mut pairer = Pairer::new();
    let a = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &places, 300, false), &set, None).unwrap());
    // Every point moved a hair: the nearest would change here and there, the random choice not.
    let moved: Vec<[f32; 3]> = places.iter().map(|p| [p[0] + 1e-4, p[1] - 1e-4, p[2]]).collect();
    let b = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &moved, 300, false), &set, None).unwrap());
    assert_eq!(a, b);
    let other = Settings { choose: Choose::Random { seed: 4 }, ..set };
    let c = lines(&gpu, &pairer.pairs(&gpu, &cloud(&gpu, &places, 300, false), &other, None).unwrap());
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
    let a = Pairer::new().pairs(&gpu, &input, &set, None).unwrap();
    let b = Pairer::new().pairs(&gpu, &input, &set, None).unwrap();
    for name in ["position", "tag", "color", SPAN, SPAN01] {
        let x = a.attr(name).unwrap().buffer().read::<u32>(&gpu).unwrap();
        let y = b.attr(name).unwrap().buffer().read::<u32>(&gpu).unwrap();
        assert!(x == y, "{name}");
    }
    let words = a.count().header().unwrap().read::<u32>(&gpu).unwrap();
    assert_eq!(words, b.count().header().unwrap().read::<u32>(&gpu).unwrap());
    assert!(words[2] > 0, "the clump is over the cap: {words:?}");
}

/// `p` with its count on the GPU bounded by `at_most` instead of its capacity, as particles'
/// count is.
fn bounded(p: &Points, at_most: u32) -> Points {
    let header = p.count().header().unwrap().clone();
    p.clone().with_count(Count::gpu(header, at_most, Seen::default()).unwrap()).unwrap()
}

/// A count whose bound moves from step to step, as particles' does, keeps the buffers of the
/// lines: the next lines take the same buffers once the last ones are let go.
#[test]
fn a_moving_bound_keeps_the_buffers_of_the_lines() {
    let Some(gpu) = gpu() else { return };
    let places = random(9, 600);
    let input = cloud(&gpu, &places, 500, true);
    let set = settings(0.2, 3);
    let mut pairer = Pairer::new();
    let a = pairer.pairs(&gpu, &bounded(&input, 500), &set, None).unwrap();
    let keys: Vec<(String, crate::BufferKey)> =
        a.attrs().iter().map(|x| (x.name().to_string(), x.buffer().key())).collect();
    drop(a);
    let b = pairer.pairs(&gpu, &bounded(&input, 520), &set, None).unwrap();
    for (name, key) in &keys {
        assert!(key.is(b.attr(name).unwrap().buffer()), "{name} was made anew");
    }
    assert_eq!(lines(&gpu, &b), model(&places, 500, &set));
}

/// A `max` far below the lines wanted, where one bin of the cut holds more lines than `max`: the
/// cut keeps nearly `max` lines, the same ones frame after frame.
#[test]
fn a_small_max_among_many_lines_keeps_nearly_max_and_holds_still() {
    let Some(gpu) = gpu() else { return };
    let places = random(13, 10_000);
    let input = cloud(&gpu, &places, places.len(), false);
    let all = Settings { attrs: vec!["tag".into()], ..settings(0.25, 16) };
    let mut pairer = Pairer::new();
    let wanted = lines(&gpu, &pairer.pairs(&gpu, &input, &all, None).unwrap()).len() as u32;
    assert!(wanted > 20 * CUT_BINS, "{wanted} lines wanted");
    for max in [20u32, 1] {
        let set = Settings { max, ..all.clone() };
        let first = lines(&gpu, &pairer.pairs(&gpu, &input, &set, None).unwrap());
        let kept = first.len() as u32;
        assert!(kept <= max && kept + 1 >= max - max / 64, "max {max}: {kept} kept of {wanted}");
        for _ in 0..3 {
            assert_eq!(lines(&gpu, &pairer.pairs(&gpu, &input, &set, None).unwrap()), first, "max {max}");
        }
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
