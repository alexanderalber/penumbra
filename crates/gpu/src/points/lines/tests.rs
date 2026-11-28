//! Resampling and smoothing lines on WARP against what they must give: even spacing, mixed
//! attributes, closed lines that stay closed, lines that stay apart, counts on the GPU, the cap,
//! and settings and positions that are no use.

use super::*;
use crate::points::MOST_ON_LINE;
use crate::points::Seen;
use crate::testing::gpu;

/// Points at `places` with a colour and an id per point, in buffers of `capacity`, with `strips`;
/// their count on the GPU when `on_gpu`.
fn set(gpu: &Gpu, places: &[[f32; 3]], strips: Strips, capacity: u32, on_gpu: bool) -> Points {
    let n = places.len() as u32;
    let mut pos: Vec<[f32; 4]> = places.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
    pos.resize(capacity as usize, [9e9; 4]);
    // Colour: red runs with the index, so a mixed colour shows where a point came from.
    let mut color: Vec<[f32; 4]> = (0..n).map(|i| [i as f32, 1.0, 0.0, 1.0]).collect();
    color.resize(capacity as usize, [0.0; 4]);
    let mut ids: Vec<u32> = (0..n).map(|i| 100 + i).collect();
    ids.resize(capacity as usize, 0xdead);
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "position").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let c = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "color").unwrap();
    c.write(gpu, 0, &color).unwrap();
    let i = Buffer::of::<u32>(gpu, u64::from(capacity), "id").unwrap();
    i.write(gpu, 0, &ids).unwrap();
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, p).unwrap(),
        Attr::new("color", AttrType::Vec4, c).unwrap(),
        Attr::new("id", AttrType::U32, i).unwrap(),
    ];
    let points = if on_gpu {
        let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
        header.write(gpu, 0, &[n, 0, 0, 0]).unwrap();
        Points::new(capacity, 0, attrs)
            .unwrap()
            .with_count(Count::gpu(header, capacity, Seen::default()).unwrap())
            .unwrap()
    } else {
        Points::new(capacity, n, attrs).unwrap()
    };
    points.with_strips(strips)
}

fn live(gpu: &Gpu, p: &Points) -> usize {
    match p.count().header() {
        Some(h) => h.read::<u32>(gpu).unwrap()[0] as usize,
        None => p.count().known().unwrap() as usize,
    }
}

fn header(gpu: &Gpu, p: &Points) -> [u32; 4] {
    let h = p.count().header().unwrap().read::<u32>(gpu).unwrap();
    [h[0], h[1], h[2], h[3]]
}

fn places(gpu: &Gpu, p: &Points) -> Vec<[f32; 3]> {
    let n = live(gpu, p);
    let pos = p.attr("position").unwrap().buffer().read::<[f32; 4]>(gpu).unwrap();
    pos[..n].iter().map(|v| [v[0], v[1], v[2]]).collect()
}

fn colors(gpu: &Gpu, p: &Points) -> Vec<[f32; 4]> {
    let n = live(gpu, p);
    p.attr("color").unwrap().buffer().read::<[f32; 4]>(gpu).unwrap()[..n].to_vec()
}

fn runs(gpu: &Gpu, p: &Points) -> Vec<[f32; 2]> {
    let n = live(gpu, p);
    p.attr(LINE).unwrap().buffer().read::<[f32; 2]>(gpu).unwrap()[..n].to_vec()
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    dist(a, b) < 1e-4
}

fn by_count(c: u32) -> Resample {
    Resample { by: By::Count(c), max: 1_000_000 }
}

fn by_length(l: f32) -> Resample {
    Resample { by: By::Length(l), max: 1_000_000 }
}

/// An open line with uneven steps: x 0, 1, 1.5, 4 (4 m long), on the x axis.
fn uneven() -> Vec<[f32; 3]> {
    vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.5, 0.0, 0.0], [4.0, 0.0, 0.0]]
}

#[test]
fn a_line_by_count_gets_even_steps_and_mixed_attributes() {
    let Some(gpu) = gpu() else { return };
    let mut r = Resampler::new();
    let input = set(&gpu, &uneven(), Strips::None, 4, false);
    let out = r.resample(&gpu, &input, &by_count(9), None).unwrap();
    assert_eq!(out.count().known(), Some(9));
    assert_eq!(out.strips(), Strips::Regular { len: 9, closed: false });
    let p = places(&gpu, &out);
    for (k, q) in p.iter().enumerate() {
        assert!(near(*q, [0.5 * k as f32, 0.0, 0.0]), "point {k} at {q:?}");
    }
    // Red is the old index, mixed by where the point lies: x 0.5 is halfway from point 0 to 1,
    // x 2.0 a fifth of the way from point 2 (x 1.5) to 3 (x 4).
    let c = colors(&gpu, &out);
    assert!((c[1][0] - 0.5).abs() < 1e-5, "{:?}", c[1]);
    assert!((c[4][0] - 2.2).abs() < 1e-5, "{:?}", c[4]);
    assert_eq!(c[8][0], 3.0);
    // Whole numbers come from the nearer old point.
    let ids = out.attr("id").unwrap().buffer().read::<u32>(&gpu).unwrap();
    assert_eq!(ids[..9], [100, 101, 101, 102, 102, 102, 103, 103, 103]);
    assert_eq!(out.bounds(), input.bounds());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_closed_ring_stays_closed_with_even_steps_round_it() {
    let Some(gpu) = gpu() else { return };
    // A square of side 1 with an extra corner point on one side: uneven steps.
    let square = vec![[0.0, 0.0, 0.0], [0.25, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]];
    let input = set(&gpu, &square, Strips::Regular { len: 5, closed: true }, 5, false);
    let mut r = Resampler::new();
    let out = r.resample(&gpu, &input, &by_count(8), None).unwrap();
    assert_eq!(out.strips(), Strips::Regular { len: 8, closed: true });
    let p = places(&gpu, &out);
    // 4 m round, 8 points: half a metre apart, the step from the last back to the first too.
    for k in 0..8 {
        let d = dist(p[k], p[(k + 1) % 8]);
        assert!((d - 0.5).abs() < 1e-4, "step {k}: {d}");
    }
    assert!(near(p[0], square[0]) && near(p[7], [0.0, 0.0, 0.5]), "{p:?}");
    // By length: 4 m in steps of about 0.3 m is 13 points, 4/13 m apart.
    let out = r.resample(&gpu, &input, &by_length(0.3), None).unwrap();
    assert_eq!(out.strips(), Strips::Runs { closed: true });
    let p = places(&gpu, &out);
    assert_eq!(p.len(), 13);
    // Evenly spaced along the outline: a step across a corner is shorter as the crow flies.
    let on_square = |s: f32| match s {
        s if s < 1.0 => [s, 0.0, 0.0],
        s if s < 2.0 => [1.0, 0.0, s - 1.0],
        s if s < 3.0 => [3.0 - s, 0.0, 1.0],
        s => [0.0, 0.0, 4.0 - s],
    };
    for (k, q) in p.iter().enumerate() {
        let want = on_square(4.0 * k as f32 / 13.0);
        assert!(near(*q, want), "point {k}: {q:?} for {want:?}");
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn lines_stay_apart_and_by_length_each_gets_its_own_count() {
    let Some(gpu) = gpu() else { return };
    // Three lines of 3 points: 2 m along x at y 0, 1 m at y 5, a single place at y 10.
    let mut pts = Vec::new();
    for (y, len) in [(0.0, 2.0), (5.0, 1.0), (10.0, 0.0)] {
        for k in 0..3 {
            pts.push([len * k as f32 / 2.0, y, 0.0]);
        }
    }
    let input = set(&gpu, &pts, Strips::regular(3), 9, false);
    let mut r = Resampler::new();
    let out = r.resample(&gpu, &input, &by_length(0.25), None).unwrap();
    assert_eq!(out.strips(), Strips::Runs { closed: false });
    assert!(out.count().known().is_none());
    // 2 m in quarters is 9 points, 1 m is 5, a line without length 2.
    let p = places(&gpu, &out);
    assert_eq!(p.len(), 16);
    assert_eq!(header(&gpu, &out), [16, 0, 16, 0]);
    let rs = runs(&gpu, &out);
    let mut at = 0;
    for (y, n, len) in [(0.0, 9, 2.0), (5.0, 5, 1.0), (10.0, 2, 0.0)] {
        for k in 0..n {
            assert_eq!(rs[at + k], [k as f32, n as f32], "point {} of the line at y {y}", at + k);
            let want = [len * k as f32 / (n - 1) as f32, y, 0.0];
            assert!(near(p[at + k], want), "{:?} for {want:?}", p[at + k]);
        }
        at += n;
    }
    // By count every line gets the same number: the third, without length, all at its place.
    let out = r.resample(&gpu, &input, &by_count(4), None).unwrap();
    assert_eq!((out.count().known(), out.strips()), (Some(12), Strips::regular(4)));
    let p = places(&gpu, &out);
    assert!(near(p[5], [1.0 / 3.0, 5.0, 0.0]) && p[8..].iter().all(|q| near(*q, [0.0, 10.0, 0.0])), "{p:?}");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_count_on_the_gpu_gives_one_on_the_gpu() {
    let Some(gpu) = gpu() else { return };
    // Lines of 2 (trails), two of them alive in room for 8.
    let pts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 2.0]];
    let input = set(&gpu, &pts, Strips::regular(2), 16, true);
    let mut r = Resampler::new();
    let out = r.resample(&gpu, &input, &by_count(3), None).unwrap();
    assert!(out.count().known().is_none());
    assert_eq!(out.strips(), Strips::regular(3));
    let p = places(&gpu, &out);
    assert_eq!(p.len(), 6);
    assert!(near(p[1], [0.5, 0.0, 0.0]) && near(p[4], [0.0, 1.0, 1.0]), "{p:?}");
    // Pairs, as proximity gives them, are lines of two.
    let input = input.with_strips(Strips::Pairs);
    let out = r.resample(&gpu, &input, &by_length(0.5), None).unwrap();
    assert_eq!(places(&gpu, &out).len(), 3 + 5);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn lines_past_max_are_left_out_whole_and_the_capacity_grows_to_what_is_wanted() {
    let Some(gpu) = gpu() else { return };
    let mut pts = Vec::new();
    for y in 0..4 {
        pts.extend([[0.0, y as f32, 0.0], [1.0, y as f32, 0.0]]);
    }
    let input = set(&gpu, &pts, Strips::regular(2), 8, false);
    let mut r = Resampler::new();
    // 11 points a line; 30 keep two lines whole.
    let out = r.resample(&gpu, &input, &Resample { by: By::Count(11), max: 30 }, None).unwrap();
    assert_eq!(out.count().known(), Some(22));
    assert_eq!(r.cut(&Resample { by: By::Count(11), max: 30 }), Some((22, 44)));
    assert_eq!(r.resample(&gpu, &input, &by_count(11), None).unwrap().count().known(), Some(44));
    assert_eq!(r.cut(&by_count(11)), None);
    let set_len = Resample { by: By::Length(0.1), max: 30 };
    let out = r.resample(&gpu, &input, &set_len, None).unwrap();
    assert_eq!(header(&gpu, &out), [22, 0, 44, 0]);
    // By length the capacity starts at the input's count (rounded up to 1024) and grows once
    // the wanted number is read back; within `max` every line then fits. 1001 points a line.
    let mut r = Resampler::new();
    let set_len = Resample { by: By::Length(0.001), max: 1_000_000 };
    let first = r.resample(&gpu, &input, &set_len, None).unwrap();
    assert_eq!(header(&gpu, &first), [1001, 0, 4004, 0]);
    let mut after = None;
    for _ in 0..100 {
        gpu.poll();
        let out = r.resample(&gpu, &input, &set_len, None).unwrap();
        if header(&gpu, &out)[0] == 4004 {
            after = Some(out);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let after = after.expect("the capacity grows to the 4004 points wanted");
    let p = places(&gpu, &after);
    assert_eq!(p.len(), 4004);
    assert!(near(p[1001 + 500], [0.5, 1.0, 0.0]), "{:?}", p[1501]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn settings_and_places_that_are_no_use_do_not_break_it() {
    let Some(gpu) = gpu() else { return };
    let mut pts = uneven();
    pts.push([f32::NAN, 0.0, 0.0]);
    pts.push([f32::INFINITY, 1e38, -1e38]);
    let input = set(&gpu, &pts, Strips::None, 6, false);
    let mut r = Resampler::new();
    for by in [
        By::Count(0),
        By::Count(u32::MAX),
        By::Length(0.0),
        By::Length(-1.0),
        By::Length(f32::NAN),
        By::Length(f32::INFINITY),
        By::Length(1e-30),
        By::Length(1e30),
    ] {
        for max in [0, 1, 7, u32::MAX] {
            let out = r.resample(&gpu, &input, &Resample { by, max }, None).unwrap();
            assert!(live(&gpu, &out) <= out.count().at_most() as usize, "{by:?} {max}");
            assert!(out.count().at_most() <= out.capacity());
        }
    }
    // No points, and points without position.
    let empty = set(&gpu, &[], Strips::None, 4, false);
    assert_eq!(r.resample(&gpu, &empty, &by_count(4), None).unwrap().count().known(), Some(0));
    assert!(r.resample(&gpu, &input.without("position"), &by_count(4), None).is_err());
    let mut s = Smoother::new();
    for (iterations, strength) in [(0, 0.5), (3, f32::NAN), (3, -1.0), (3, 7.0), (u32::MAX, 0.5)] {
        let out = s.smooth(&gpu, &input, &Smooth { iterations, strength, pin_ends: false }).unwrap();
        assert_eq!(out.count(), input.count());
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// One smoothing step on the CPU over lines of `len` (closed or not), as the GPU does it.
fn smooth_cpu(p: &[[f32; 3]], len: usize, closed: bool, strength: f32, pin: bool) -> Vec<[f32; 3]> {
    (0..p.len())
        .map(|i| {
            let first = i - i % len;
            let points = len.min(p.len() - first);
            let along = i - first;
            let closed = closed && points > 2;
            let before = if along > 0 {
                Some(i - 1)
            } else if closed {
                Some(first + points - 1)
            } else {
                None
            };
            let after = if along + 1 < points {
                Some(i + 1)
            } else if closed {
                Some(first)
            } else {
                None
            };
            let end = before.is_none() || after.is_none();
            let near: Vec<[f32; 3]> = [before, after].into_iter().flatten().map(|j| p[j]).collect();
            if near.is_empty() || (end && pin) {
                return p[i];
            }
            let k = near.len() as f32;
            let mean = [0, 1, 2].map(|c| near.iter().map(|q| q[c]).sum::<f32>() / k);
            [0, 1, 2].map(|c| p[i][c] + (mean[c] - p[i][c]) * strength)
        })
        .collect()
}

#[test]
fn smoothing_follows_each_line_and_keeps_pinned_ends() {
    let Some(gpu) = gpu() else { return };
    // Two zigzags of 6 points, side by side, and a ring.
    let mut pts = Vec::new();
    for y in [0.0, 3.0] {
        for k in 0..6 {
            pts.push([k as f32, y, if k % 2 == 0 { 0.0 } else { 1.0 }]);
        }
    }
    let input = set(&gpu, &pts, Strips::regular(6), 12, false);
    let mut s = Smoother::new();
    for (iterations, pin) in [(1, true), (2, true), (3, false), (8, true)] {
        let out = s.smooth(&gpu, &input, &Smooth { iterations, strength: 0.5, pin_ends: pin }).unwrap();
        let mut want = pts.clone();
        for _ in 0..iterations {
            want = smooth_cpu(&want, 6, false, 0.5, pin);
        }
        let got = places(&gpu, &out);
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(near(*g, *w), "{iterations} steps, point {k}: {g:?} for {w:?}");
        }
        // Lines stay apart: nothing moves across y.
        assert!(got[..6].iter().all(|q| q[1] == 0.0) && got[6..].iter().all(|q| q[1] == 3.0));
        if pin {
            assert_eq!([got[0], got[5], got[6], got[11]], [pts[0], pts[5], pts[6], pts[11]]);
        }
        assert_eq!((out.strips(), out.count()), (input.strips(), input.count()));
    }
    // A ring has no ends: every point moves, also the first and the last.
    let ring: Vec<[f32; 3]> = (0..8)
        .map(|k| {
            let a = k as f32 * std::f32::consts::TAU / 8.0;
            let r = if k % 2 == 0 { 1.0 } else { 1.3 };
            [r * a.cos(), 0.0, r * a.sin()]
        })
        .collect();
    let input = set(&gpu, &ring, Strips::Regular { len: 8, closed: true }, 8, true);
    let out = s.smooth(&gpu, &input, &Smooth { iterations: 2, strength: 0.5, pin_ends: true }).unwrap();
    let want = smooth_cpu(&smooth_cpu(&ring, 8, true, 0.5, true), 8, true, 0.5, true);
    let got = places(&gpu, &out);
    assert!(got.iter().zip(&want).all(|(g, w)| near(*g, *w)), "{got:?} for {want:?}");
    assert!(!near(got[0], ring[0]) && !near(got[7], ring[7]));
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn smoothing_a_grid_both_ways_and_lines_of_any_length() {
    let Some(gpu) = gpu() else { return };
    // A 3 x 3 grid, flat but for a bump in the middle.
    let mut pts = Vec::new();
    for r in 0..3 {
        for c in 0..3 {
            pts.push([c as f32, 0.0, if (r, c) == (1, 1) { 4.0 } else { r as f32 }]);
        }
    }
    let grid = Strips::Grid { cols: 3, rows: true, columns: true };
    let input = set(&gpu, &pts, grid, 9, false);
    let mut s = Smoother::new();
    let out = s.smooth(&gpu, &input, &Smooth { iterations: 1, strength: 1.0, pin_ends: true }).unwrap();
    let got = places(&gpu, &out);
    // The middle takes the mean of its four neighbours; the border stays.
    assert!(near(got[4], [1.0, 0.0, 1.0]), "{:?}", got[4]);
    assert!(got.iter().zip(&pts).enumerate().all(|(k, (g, p))| k == 4 || g == p));
    // Lines of any length from a resampling: each smooths on its own.
    let mut pts = Vec::new();
    for (y, len) in [(0.0, 2.0), (5.0, 1.0)] {
        for k in 0..3 {
            pts.push([len * k as f32 / 2.0, y, if k == 1 { 1.0 } else { 0.0 }]);
        }
    }
    let input = set(&gpu, &pts, Strips::regular(3), 6, false);
    let mut r = Resampler::new();
    let lines = r.resample(&gpu, &input, &by_length(0.5), None).unwrap();
    let before = places(&gpu, &lines);
    let out = s.smooth(&gpu, &lines, &Smooth { iterations: 4, strength: 0.5, pin_ends: true }).unwrap();
    let got = places(&gpu, &out);
    assert_eq!(got.len(), before.len());
    let rs = runs(&gpu, &out);
    for (k, (g, b)) in got.iter().zip(&before).enumerate() {
        assert_eq!(g[1], b[1], "point {k} stays on its line");
        if rs[k][0] == 0.0 || rs[k][0] + 1.0 == rs[k][1] {
            assert_eq!(g, b, "the ends of each line stay");
        }
    }
    // A resampling of lines of any length again.
    let again = r.resample(&gpu, &lines, &by_count(3), None).unwrap();
    let p = places(&gpu, &again);
    assert_eq!(p.len(), 6);
    assert!(
        near(p[0], [0.0, 0.0, 0.0]) && near(p[2], [2.0, 0.0, 0.0]) && near(p[5], [1.0, 5.0, 0.0]),
        "{p:?}"
    );
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn the_programs_count_points_on_a_line_as_the_attribute_does() {
    assert!(LAYOUT.contains(&format!("LN_MOST: u32 = {MOST_ON_LINE}u")));
    const { assert!(MOST_BY_COUNT <= MOST_ON_LINE) };
}

#[test]
fn long_lines_across_many_blocks_and_the_columns_of_a_grid_measure_right() {
    let Some(gpu) = gpu() else { return };
    // Ten lines of 700 points each, every line 0.7 m along x with even steps but for a jump
    // in its middle: they cross the blocks of 1024 points of the prefix sum.
    let mut pts = Vec::new();
    for l in 0..10 {
        for k in 0..700 {
            let x = k as f32 * 0.001 + if k >= 350 { 0.3 } else { 0.0 };
            pts.push([x, l as f32, 0.0]);
        }
    }
    let input = set(&gpu, &pts, Strips::regular(700), 7000, false);
    let mut r = Resampler::new();
    let out = r.resample(&gpu, &input, &by_length(0.01), None).unwrap();
    // Each line is 0.999 m long: 101 points, 0.00999 m apart.
    let p = places(&gpu, &out);
    assert_eq!(p.len(), 1010);
    for l in 0..10 {
        for k in 0..101 {
            let want = [k as f32 * 0.999 / 100.0, l as f32, 0.0];
            assert!(near(p[l * 101 + k], want), "line {l} point {k}: {:?} for {want:?}", p[l * 101 + k]);
        }
    }
    // One ring of 5000 points round a circle of radius 1.
    let ring: Vec<[f32; 3]> = (0..5000)
        .map(|k| {
            let a = k as f32 * std::f32::consts::TAU / 5000.0;
            [a.cos(), 0.0, a.sin()]
        })
        .collect();
    let input = set(&gpu, &ring, Strips::Regular { len: 5000, closed: true }, 5000, false);
    let out = r.resample(&gpu, &input, &by_count(100), None).unwrap();
    let p = places(&gpu, &out);
    for k in 0..100 {
        let d = dist(p[k], p[(k + 1) % 100]);
        assert!((d - 0.0628).abs() < 2e-4, "step {k}: {d}");
    }
    // The columns of a grid: 3 wide, 4 high, the columns 0, 1 and 2 m tall.
    let mut pts = Vec::new();
    for r in 0..4 {
        for c in 0..3 {
            pts.push([c as f32, 0.0, r as f32 * c as f32 / 3.0]);
        }
    }
    let grid = Strips::Grid { cols: 3, rows: false, columns: true };
    let input = set(&gpu, &pts, grid, 12, false);
    let out = r.resample(&gpu, &input, &by_length(0.5), None).unwrap();
    let p = places(&gpu, &out);
    // Column 0 has no length: 2 points; column 1, 1 m: 3; column 2, 2 m: 5.
    assert_eq!(p.len(), 10);
    assert!(near(p[3], [1.0, 0.0, 0.5]) && near(p[9], [2.0, 0.0, 2.0]), "{p:?}");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
