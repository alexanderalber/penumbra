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

/// A node that cooks every frame on a GPU that lags more frames behind than the readbacks it
/// waits for still hears back: the oldest readback, the next to arrive, is kept, and its words
/// count once it is there.
#[test]
fn readbacks_that_lag_many_frames_still_arrive() {
    let mut reports = Reports::<u32>::new();
    let started: Vec<Seen> = (0..3 * WAITING_MOST).map(|_| Seen::default()).collect();
    for seen in &started {
        reports.wait_for(seen.clone(), 1);
        assert_eq!(reports.words(&1), None, "nothing arrived yet");
    }
    // The GPU catches up with the first of them.
    started[0].arrive([4, 3, 2, 1]);
    assert_eq!(reports.words(&1), Some([4, 3, 2, 1]));
    // The newest is kept as well.
    started[started.len() - 1].arrive([9, 0, 0, 0]);
    assert_eq!(reports.words(&1), Some([9, 0, 0, 0]));
}

/// Readbacks that failed never arrive and leave the queue: seven of them do not take the places
/// of the readbacks that do arrive, two cooks late, on a GPU that lags behind.
#[test]
fn failed_readbacks_do_not_make_reports_deaf() {
    let mut reports = Reports::<u32>::new();
    for _ in 0..WAITING_MOST - 1 {
        let dead = Seen::default();
        dead.fail();
        reports.wait_for(dead, 1);
    }
    let started: Vec<Seen> = (0..40).map(|_| Seen::default()).collect();
    let mut heard = None;
    for (k, seen) in started.iter().enumerate() {
        reports.wait_for(seen.clone(), 1);
        if k >= 2 {
            started[k - 2].arrive([k as u32, 0, 0, 0]);
        }
        heard = heard.or(reports.words(&1).map(|_| k));
    }
    assert_eq!(heard, Some(2), "the first readback to arrive is heard at once");
}

/// The half floats a picture is packed into by hand round as `half` rounds them: at the edges
/// (no number, infinities, minus zero, the smallest and the largest).
#[test]
fn half_floats_round_at_their_edges_as_half_does() {
    let Some(gpu) = gpu() else { return };
    let values = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.0,
        0.0,
        2f32.powi(-25),
        2f32.powi(-25) * 1.5,
        2f32.powi(-24),
        2f32.powi(-14) * 0.75,
        2f32.powi(-14),
        1.0 + 2f32.powi(-11),
        1.0 + 3.0 * 2f32.powi(-11),
        65504.0,
        65519.0,
        65520.0,
        -65520.0,
        1e9,
    ];
    let b = Buffer::of::<f32>(&gpu, values.len() as u64, "values").unwrap();
    b.write(&gpu, 0, &values).unwrap();
    let n = values.len() as u32;
    let p = Points::new(n, n, vec![Attr::new("v", AttrType::F32, b).unwrap()]).unwrap();
    let tex =
        p.picture(&gpu, "v", (n, 1), false, &mut TexRing::default(), &mut BufferRing::default()).unwrap();
    let px = gpu.read(&tex).unwrap();
    for (i, v) in values.iter().enumerate() {
        let want = half::f16::from_f32(*v).to_f32();
        let got = px.get(i as u32, 0).unwrap()[0];
        let same = (want.is_nan() && got.is_nan())
            || (want == got && want.is_sign_negative() == got.is_sign_negative());
        assert!(same, "{v:e}: {got:e}, half gives {want:e}");
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_probe_reads_the_first_values_and_sums_up_all() {
    use super::probe::{Ask, Probe, Probed};
    let Some(gpu) = gpu() else { return };
    // 3000 points: more than one workgroup of the summary, x = i, weight = 2 at every point.
    let n = 3000u32;
    let pos: Vec<[f32; 4]> = (0..n).map(|i| [i as f32, -(i as f32), 1.0, 1.0]).collect();
    let b = Buffer::of::<[f32; 4]>(&gpu, u64::from(n), "p").unwrap();
    b.write(&gpu, 0, &pos).unwrap();
    let p = Points::new(n, n, vec![Attr::new("position", AttrType::Vec3, b).unwrap()]).unwrap();
    let mut probe = Probe::new();
    let read = |probe: &mut Probe, p: &Points, name: &str| {
        let out = probe.fill(&gpu, p, Ask { name, first: 3, summary: true }).unwrap();
        Probed::of(&out.read::<[f32; 4]>(&gpu).unwrap()).unwrap()
    };
    let got = read(&mut probe, &p, "P");
    assert_eq!(got.count, n);
    assert_eq!(got.first, [[0.0, -0.0, 1.0, 1.0], [1.0, -1.0, 1.0, 1.0], [2.0, -2.0, 1.0, 1.0]]);
    assert!((got.mean[0] - 1499.5).abs() < 0.01 && (got.mean[1] + 1499.5).abs() < 0.01, "{got:?}");
    assert_eq!((got.min[0], got.max[0], got.min[1], got.max[1]), (0.0, 2999.0, -2999.0, 0.0));
    // A standard attribute the points lack: its default everywhere.
    let got = read(&mut probe, &p, "PointScale");
    assert_eq!((got.first[0][0], got.mean[0], got.max[0]), (1.0, 1.0, 1.0));
    // A count on the GPU of 10: the summary goes over those only.
    let header = Buffer::new(&gpu, HEADER_BYTES, "h").unwrap();
    header.write(&gpu, 0, &[10u32, 0, 0, 0]).unwrap();
    let live = p.clone().with_count(Count::gpu(header.clone(), n, Seen::default()).unwrap()).unwrap();
    let got = read(&mut probe, &live, "position");
    assert_eq!((got.count, got.mean[0], got.max[0]), (10, 4.5, 9.0));
    // None left: zeros.
    header.write(&gpu, 0, &[0u32, 0, 0, 0]).unwrap();
    let got = read(&mut probe, &live, "position");
    assert_eq!((got.count, got.mean, got.first[0]), (0, [0.0; 4], [0.0; 4]));
    assert!(probe.fill(&gpu, &p, Ask { name: "nothing", first: 1, summary: false }).is_err());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// Particle ids that already lie in their input's band stay as they are: points that come round
/// a feedback loop into a later input of a merge keep their ids frame after frame, where adding
/// the band again moved them every frame. Ids from 0, as a particle system counts, still move up
/// into the band.
#[test]
fn merged_ids_already_in_their_band_stay() {
    let Some(gpu) = gpu() else { return };
    let ids = |values: &[u32]| {
        let b = Buffer::of::<u32>(&gpu, values.len() as u64, "ids").unwrap();
        b.write(&gpu, 0, values).unwrap();
        let n = values.len() as u32;
        Points::new(n, n, vec![Attr::new("id", AttrType::U32, b).unwrap()]).unwrap()
    };
    let half = 1u32 << 31;
    let new = ids(&[0, 1]);
    let mut merger = merge::Merger::new();
    // The first frame: two systems that count from 0.
    let first = merger.merge(&gpu, &[(0, &new), (1, &ids(&[0, 1]))]).unwrap();
    let read = |p: &Points| p.attr("id").unwrap().buffer().read::<u32>(&gpu).unwrap()[..4].to_vec();
    assert_eq!(read(&first), [0, 1, half, half + 1]);
    // What came round goes into the second input again: its banded ids do not move.
    let fed = ids(&[half, half + 1, half + 7]);
    let again = merger.merge(&gpu, &[(0, &ids(&[2, 3])), (1, &fed)]).unwrap();
    let got = again.attr("id").unwrap().buffer().read::<u32>(&gpu).unwrap()[..5].to_vec();
    assert_eq!(got, [2, 3, half, half + 1, half + 7]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// Four sets of points with six attributes between them, some lacking some: a set's own count
/// known, or on the GPU below its capacity.
fn four_sets(gpu: &Gpu, scale: u32) -> Vec<Points> {
    let names: [(&str, AttrType); 6] = [
        ("position", AttrType::Vec3),
        ("color", AttrType::Vec4),
        ("scale", AttrType::F32),
        ("uv", AttrType::Vec2),
        ("velocity", AttrType::Vec3),
        ("tag", AttrType::U32),
    ];
    (0..4u32)
        .map(|k| {
            let capacity = (k + 1) * 10 * scale;
            let attrs: Vec<Attr> = names
                .iter()
                .enumerate()
                .filter(|(a, _)| (a + k as usize) % 5 != 4)
                .map(|(a, (name, ty))| {
                    let words = (ty.stride() / 4) as u32;
                    let values: Vec<u32> = (0..capacity * words)
                        .map(|w| {
                            if *ty == AttrType::U32 {
                                k * 1000 + w
                            } else {
                                (k as f32 * 100.0 + a as f32 + w as f32 * 0.5).to_bits()
                            }
                        })
                        .collect();
                    let b = Buffer::of::<u32>(gpu, values.len() as u64, name).unwrap();
                    b.write(gpu, 0, &values).unwrap();
                    Attr::new(name, *ty, b).unwrap()
                })
                .collect();
            let p = Points::new(capacity, capacity, attrs).unwrap();
            if k % 2 == 0 {
                return p;
            }
            let header = Buffer::new(gpu, HEADER_BYTES, "count").unwrap();
            header.write(gpu, 0, &[capacity - 3, 0, 0, 0]).unwrap();
            p.with_count(Count::gpu(header, capacity, Seen::default()).unwrap()).unwrap()
        })
        .collect()
}

/// A merge of four sets and six attributes: each attribute of each set in its place, in order,
/// the default where a set lacks it, the count on the GPU the sum of the live points.
#[test]
fn a_merge_of_four_sets_puts_every_attribute_in_place() {
    let Some(gpu) = gpu() else { return };
    let sets = four_sets(&gpu, 1);
    let refs: Vec<(usize, &Points)> = sets.iter().enumerate().collect();
    let mut merger = merge::Merger::new();
    for _ in 0..2 {
        let out = merger.merge(&gpu, &refs).unwrap();
        let header = out.count().header().unwrap().read::<u32>(&gpu).unwrap();
        let live: Vec<u32> = sets.iter().map(|p| p.count().known().unwrap_or(p.capacity() - 3)).collect();
        assert_eq!(header[0], live.iter().sum::<u32>());
        for a in out.attrs() {
            let got = a.buffer().read::<u32>(&gpu).unwrap();
            let words = (a.ty().stride() / 4) as usize;
            let mut at = 0usize;
            for (k, p) in sets.iter().enumerate() {
                let n = live[k] as usize * words;
                let want: Vec<u32> = match p.attr(a.name()) {
                    Some(src) => src.buffer().read::<u32>(&gpu).unwrap()[..n].to_vec(),
                    None => {
                        let fill = merge::fill_of(a.name(), a.ty());
                        (0..n).map(|w| fill[w % words].to_bits()).collect()
                    }
                };
                assert_eq!(got[at..at + n], want[..], "`{}` of set {k}", a.name());
                at += n;
            }
        }
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// CPU time of a merge of four sets and six attributes, as a cook pass records it (deferred).
#[test]
#[ignore = "times a merge on the hardware GPU: cargo test -p penumbra-gpu merge_timing -- --ignored --nocapture"]
fn merge_timing() {
    let gpu = Gpu::headless(crate::AdapterChoice::Hardware).unwrap();
    eprintln!("{}", gpu.name());
    let sets = four_sets(&gpu, 1000);
    let refs: Vec<(usize, &Points)> = sets.iter().enumerate().collect();
    let mut merger = merge::Merger::new();
    for _ in 0..50 {
        let _d = gpu.defer();
        merger.merge(&gpu, &refs).unwrap();
    }
    gpu.wait(std::time::Duration::from_secs(10)).unwrap();
    let runs = 2000;
    let mut cpu = std::time::Duration::ZERO;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        {
            let _d = gpu.defer();
            let out = merger.merge(&gpu, &refs).unwrap();
            drop(out);
        }
        cpu += t.elapsed();
        gpu.poll();
    }
    eprintln!(
        "merge of 4 sets x 6 attributes: {:.1} us CPU per merge",
        cpu.as_secs_f64() * 1e6 / f64::from(runs)
    );
}

/// A probe and the first rows read again and again take their buffers from a ring: a thousand
/// read-backs keep using the same few buffers, and each holds what was asked this time (the
/// header of a count the CPU knows reads as zeros again after one on the GPU).
#[test]
fn probes_and_first_rows_use_their_buffers_again() {
    use super::probe::{self, Ask, Probe, Probed};
    let Some(gpu) = gpu() else { return };
    let n = 50u32;
    let b = Buffer::of::<[f32; 4]>(&gpu, u64::from(n), "position").unwrap();
    let known = Points::new(n, n, vec![Attr::new("position", AttrType::Vec3, b.clone()).unwrap()]).unwrap();
    let header = Buffer::new(&gpu, HEADER_BYTES, "h").unwrap();
    header.write(&gpu, 0, &[7u32, 0, 0, 0]).unwrap();
    let on_gpu = known.clone().with_count(Count::gpu(header, n, Seen::default()).unwrap()).unwrap();
    let mut probe = Probe::new();
    let mut ring = crate::BufferRing::new();
    let (mut probed, mut rows): (Vec<crate::BufferKey>, Vec<crate::BufferKey>) = (Vec::new(), Vec::new());
    let keep = |keys: &mut Vec<crate::BufferKey>, out: &Buffer| {
        if !keys.iter().any(|k| k.is(out)) {
            keys.push(out.key());
        }
    };
    for i in 0..1000u32 {
        b.write(&gpu, 0, &[[i as f32, 1.0, 2.0, 1.0]]).unwrap();
        let points = if i % 2 == 0 { &known } else { &on_gpu };
        let out = probe.fill(&gpu, points, Ask { name: "position", first: 2, summary: false }).unwrap();
        keep(&mut probed, &out);
        let first = probe::first_rows(&gpu, points, &["position"], 2, &mut ring).unwrap();
        keep(&mut rows, &first);
        if i % 100 == 0 || i % 100 == 1 {
            let got = Probed::of(&out.read::<[f32; 4]>(&gpu).unwrap()).unwrap();
            assert_eq!((got.first[0][0], got.count), (i as f32, if i % 2 == 0 { n } else { 7 }), "probe {i}");
            let words = first.read::<u32>(&gpu).unwrap();
            let count = if i % 2 == 0 { 0 } else { 7 };
            assert_eq!((words[0], f32::from_bits(words[4])), (count, i as f32), "first rows {i}");
        }
    }
    assert!(probed.len() <= 2 && rows.len() <= 2, "{} and {} buffers", probed.len(), rows.len());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
