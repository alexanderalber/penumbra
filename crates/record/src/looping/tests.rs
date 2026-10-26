use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use penumbra_core::NodeId;
use penumbra_core::scratch::ScratchDir;
use penumbra_video::Limits;

use super::search::window_for_tests;
use super::*;
use crate::{Bytes, Frame, Hub};

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("loop-{name}")).unwrap()
}

/// A tiny pseudo-random sequence (xorshift), the same on every run.
struct Noise(u64);

impl Noise {
    /// -1 to 1.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 2001) as f64 / 1000.0 - 1.0
    }
}

/// A thumbnail of a periodic picture at phase `t / period`: two waves running across it, so
/// forward and backward motion differ.
fn periodic_thumb(t: f64, period: f64, noise: &mut Noise) -> [u8; THUMB] {
    let phase = std::f64::consts::TAU * t / period;
    let mut out = [0u8; THUMB];
    for (s, o) in out.iter_mut().enumerate() {
        let v = 128.0 + 60.0 * (phase + s as f64 * 0.1).sin() + 40.0 * (2.0 * phase + s as f64 * 0.37).cos();
        *o = (v + 4.0 * noise.next()).clamp(0.0, 255.0) as u8;
    }
    out
}

#[test]
fn the_cut_finds_the_period_despite_noise_and_a_dropped_frame() {
    let period = 37.0;
    let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
    let mut thumbs = Thumbs::default();
    // Phase per frame index: frame 50 was dropped while recording, so frame 49 shows twice and
    // everything after is one frame late.
    let phase_of = |i: usize| if i <= 50 { i as f64 } else { i as f64 - 1.0 };
    let n = 160;
    for i in 0..n {
        thumbs.push(&periodic_thumb(phase_of(i), period, &mut noise));
    }
    let cut = find_cut(&thumbs, 30, &|| false).unwrap();
    assert!(cut.end - cut.start >= 30, "{cut:?}");
    let drift = (phase_of(cut.end) - phase_of(cut.start)).rem_euclid(period);
    let drift = drift.min(period - drift);
    assert!(drift <= 1.0, "the cut {cut:?} is {drift} frames off the period");
    assert!(cut.score < 6.0, "{cut:?}");
    // A loop half a period off is far worse.
    let off = window_for_tests(&thumbs, 10, 10 + 55);
    assert!(off > 4.0 * cut.score as f32, "{off} against {cut:?}");
    // Too short a recording for the length asked is an error, and so is stopping.
    assert!(find_cut(&thumbs, 200, &|| false).is_err());
    assert!(find_cut(&thumbs, 30, &|| true).is_err());
}

#[test]
fn a_long_recording_is_searched_coarsely_and_still_found() {
    // 30000 frames take the coarse pass on every second or third frame.
    let period = 611.0;
    let mut noise = Noise(42);
    let mut thumbs = Thumbs::with_capacity(30_000);
    for i in 0..30_000 {
        thumbs.push(&periodic_thumb(i as f64, period, &mut noise));
    }
    let started = Instant::now();
    let cut = find_cut(&thumbs, 3000, &|| false).unwrap();
    let drift = ((cut.end - cut.start) as f64).rem_euclid(period);
    assert!(drift.min(period - drift) <= 1.0, "{cut:?}");
    eprintln!("30000 frames searched in {:?}", started.elapsed());
}

#[test]
fn blends_mix_samples_by_weight() {
    let mut a = vec![0u8, 100, 200, 255];
    blend(&mut a, &[100, 100, 0, 255], 0.5, 1);
    assert_eq!(a, [50, 100, 100, 255]);
    let mut a = vec![10u8, 20];
    blend(&mut a, &[30, 40], 0.0, 1);
    assert_eq!(a, [10, 20]);
    blend(&mut a, &[30, 40], 1.0, 1);
    assert_eq!(a, [30, 40]);
    // 16-bit little-endian samples.
    let le = |v: &[u16]| v.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>();
    let mut a = le(&[0, 1000, 4000]);
    blend(&mut a, &le(&[1000, 1000, 0]), 0.25, 2);
    assert_eq!(a, le(&[250, 1000, 3000]));
}

/// Planes of `layout` whose luma is `luma(x, y)` (8-bit video-range values), chroma neutral.
fn planes(layout: &Layout, luma: impl Fn(usize, usize) -> f64) -> Vec<u8> {
    let mut b = vec![0u8; layout.bytes()];
    let [oy, ou, _, oa] = layout.offsets();
    let scale = f64::from(1u32 << (layout.bits - 8));
    let sb = layout.sample_bytes();
    let put = |b: &mut Vec<u8>, at: usize, v: f64| {
        let v = (v * scale).round() as u16;
        if sb == 1 {
            b[at] = v as u8;
        } else {
            b[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
    };
    let ls = layout.luma_stride() as usize;
    for y in 0..layout.height as usize {
        for x in 0..layout.width as usize {
            put(&mut b, oy + (y * ls + x) * sb, luma(x, y));
        }
    }
    for at in (ou..oa).step_by(sb) {
        put(&mut b, at, 128.0);
    }
    if layout.alpha {
        for at in (oa..layout.bytes()).step_by(sb) {
            put(&mut b, at, 255.0 * ((1 << layout.bits) - 1) as f64 / scale / 255.0);
        }
    }
    b
}

/// Writes a clip of `frames` to `path`.
fn write_clip(path: &Path, codec: Codec, w: u32, h: u32, fps: u32, frames: &[Vec<u8>]) -> PathBuf {
    let settings = Settings {
        path: path.to_path_buf(),
        codec,
        width: w,
        height: h,
        fps: Fps { num: fps, den: 1 },
        threads: 2,
        queue: 1,
        h264: H264Preset::Balanced,
    };
    let mut sink = Sink::create(&settings, |_| {}).unwrap();
    for f in frames {
        sink.push(f).unwrap();
    }
    let path = sink.path().to_path_buf();
    assert_eq!(sink.finish().unwrap(), frames.len() as u64);
    path
}

/// A wave running across the picture, one period every `period` frames, with a little noise.
fn wave_clip(layout: &Layout, n: usize, period: f64) -> Vec<Vec<u8>> {
    let mut noise = Noise(7);
    let w = f64::from(layout.width);
    (0..n)
        .map(|t| {
            let phase = std::f64::consts::TAU * t as f64 / period;
            let grain: Vec<f64> = (0..layout.width * layout.height).map(|_| 2.0 * noise.next()).collect();
            planes(layout, |x, y| {
                let g = grain[y * layout.width as usize + x];
                128.0
                    + 70.0 * (std::f64::consts::TAU * x as f64 / w + phase).sin()
                    + 10.0 * (y as f64 * 0.3).cos()
                    + g
            })
        })
        .collect()
}

fn wait(job: &Job) -> LoopStatus {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let s = job.status();
        if s.done {
            return s;
        }
        assert!(Instant::now() < deadline, "the loop did not finish: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Every frame of a clip, as thumbnails.
fn decode_thumbs(path: &Path) -> Vec<[u8; THUMB]> {
    let mut d = penumbra_video::open(path, &Limits::default()).unwrap();
    let n = d.info().frames;
    (0..n).map(|i| thumbnail(d.frame(i).unwrap().pixels().unwrap()).unwrap()).collect()
}

fn dist(a: &[u8; THUMB], b: &[u8; THUMB]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| f64::from(x.abs_diff(y))).sum::<f64>() / THUMB as f64
}

/// Mean distance between neighbouring frames.
fn step(thumbs: &[[u8; THUMB]]) -> f64 {
    thumbs.windows(2).map(|w| dist(&w[0], &w[1])).sum::<f64>() / (thumbs.len() - 1) as f64
}

#[test]
fn every_method_loops_a_clip_end_to_end() {
    let dir = scratch("e2e");
    let (w, h, fps, n, period) = (64u32, 48u32, 24u32, 90usize, 24.0);
    for codec in [Codec::H264, Codec::Prores422] {
        let layout = codec.layout(w, h);
        let input = write_clip(
            &dir.join(format!("wave.{}", codec.extension())),
            codec,
            w,
            h,
            fps,
            &wave_clip(&layout, n, period),
        );
        let source = decode_thumbs(&input);
        assert_eq!(source.len(), n);
        let typical = step(&source);
        let jobs = Jobs::new();
        for method in Method::ALL {
            let spec = Spec { min_seconds: 1.5, fade_seconds: 0.25, ..Spec::new(method) };
            let job = jobs.start(&input, spec).unwrap();
            let s = wait(&job);
            assert_eq!(s.error, None, "{codec:?} {method:?}");
            assert_eq!(s.codec, codec.name(), "the loop keeps the recording's codec");
            let out = PathBuf::from(&s.out);
            assert!(out.file_name().unwrap().to_string_lossy().starts_with("wave_loop"), "{s:?}");
            let thumbs = decode_thumbs(&out);
            let expected = match method {
                Method::Cut => s.loop_end.unwrap() - s.loop_start.unwrap(),
                Method::Fade => n as u64 - 6,
                Method::Pingpong => 2 * n as u64 - 2,
                Method::CutFade => s.loop_end.unwrap() - s.loop_start.unwrap() - 6,
            };
            assert_eq!(
                (thumbs.len() as u64, s.frames_out),
                (expected, expected),
                "{codec:?} {method:?}: {s:?}"
            );
            assert_eq!(s.frames_in, n as u64);
            if method.cuts() {
                let len = s.loop_end.unwrap() - s.loop_start.unwrap();
                assert!(len >= 36, "{s:?}");
                let drift = (len as f64).rem_euclid(period);
                assert!(drift.min(period - drift) <= 1.0, "{codec:?} {method:?}: {s:?}");
                assert!(s.score.unwrap() <= s.motion.unwrap(), "a seam smoother than the motion: {s:?}");
            }
            if method != Method::Pingpong {
                // The seam: from the last frame to the first is no bigger a step than any other.
                let seam = dist(thumbs.last().unwrap(), &thumbs[0]);
                assert!(seam < 1.6 * typical, "{codec:?} {method:?}: seam {seam}, steps {typical}");
            }
            assert!((s.progress - 1.0).abs() < 1e-9);
        }
        // A pure fade of the whole clip: the second loop of the same file gets a number.
        let job = jobs.start(&input, Spec { fade_seconds: 0.25, ..Spec::new(Method::Fade) }).unwrap();
        let s = wait(&job);
        assert!(s.out.contains("wave_loop_"), "{s:?}");
        assert_eq!(jobs.all().len(), 5);
    }
}

#[test]
fn pingpong_plays_forward_then_backward_in_stretches() {
    let dir = scratch("pingpong");
    let codec = Codec::Prores422;
    let layout = codec.layout(64, 48);
    let levels: Vec<f64> = (0..9).map(|k| 40.0 + 20.0 * f64::from(k)).collect();
    let frames: Vec<Vec<u8>> = levels.iter().map(|&l| planes(&layout, |_, _| l)).collect();
    let input = write_clip(&dir.join("steps.mov"), codec, 64, 48, 30, &frames);
    let mut plan = prepare(&input, &Spec::new(Method::Pingpong)).unwrap();
    // Three frames per stretch backward.
    plan.reverse_bytes = 3 * layout.bytes();
    let out = plan.out.clone();
    let job = Job::new(LoopStatus::default());
    run(plan, &job).unwrap();
    let mut d = penumbra_video::open(&out, &Limits::default()).unwrap();
    let order: Vec<usize> = (0..d.info().frames)
        .map(|i| {
            let t = thumbnail(d.frame(i).unwrap().pixels().unwrap()).unwrap();
            let mean = t.iter().map(|&v| f64::from(v)).sum::<f64>() / THUMB as f64;
            ((mean - 40.0) / 20.0).round() as usize
        })
        .collect();
    assert_eq!(order, [0, 1, 2, 3, 4, 5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1]);
}

#[test]
fn bad_inputs_are_refused_at_once() {
    let dir = scratch("bad");
    let jobs = Jobs::new();
    assert!(jobs.start(&dir.join("missing.mp4"), Spec::new(Method::Cut)).unwrap_err().contains("no file"));
    let junk = dir.join("junk.mp4");
    std::fs::write(&junk, b"not a movie").unwrap();
    assert!(jobs.start(&junk, Spec::new(Method::Fade)).is_err());
    // Two seconds are too short for a cut of at least five.
    let layout = Codec::H264.layout(32, 32);
    let frames: Vec<Vec<u8>> = (0..20).map(|k| planes(&layout, |_, _| 50.0 + f64::from(k))).collect();
    let input = write_clip(&dir.join("short.mp4"), Codec::H264, 32, 32, 10, &frames);
    assert!(jobs.start(&input, Spec::new(Method::Cut)).unwrap_err().contains("min_seconds"));
    // ProRes does not go into MP4; nothing is created for a refused job.
    let spec =
        Spec { codec: Some(Codec::Prores422), out: Some(dir.join("x.mp4")), ..Spec::new(Method::Fade) };
    assert!(jobs.start(&input, spec).is_err());
    assert!(!dir.join("x.mp4").exists() && !dir.join("short_loop.mp4").exists());
    assert!(jobs.all().is_empty());
    // A loop never replaces its recording.
    let spec = Spec { out: Some(input.clone()), ..Spec::new(Method::Fade) };
    let job = jobs.start(&input, spec).unwrap();
    let s = wait(&job);
    assert!(s.out.ends_with("short_2.mp4"), "{s:?}");
    // Closed, no new jobs.
    assert_eq!(jobs.shutdown(Duration::from_secs(10)), 0);
    assert!(jobs.start(&input, Spec::new(Method::Fade)).is_err());
}

#[test]
fn a_recording_asked_to_loop_starts_its_loop_when_finished() {
    let dir = scratch("hub");
    let hub = Hub::new();
    let node = NodeId(3);
    hub.attach(node, "/rec");
    let settings = Settings {
        path: dir.join("take.mov"),
        codec: Codec::Prores422,
        width: 32,
        height: 32,
        fps: Fps { num: 20, den: 1 },
        threads: 2,
        queue: 64,
        h264: H264Preset::Balanced,
    };
    let rec = hub.start(node, settings).unwrap();
    let layout = rec.layout();
    rec.then_loop(Some(Spec { fade_seconds: 0.5, ..Spec::new(Method::Fade) }));
    for k in 0..40 {
        let f = Frame { index: k, planes: Box::new(Bytes(planes(&layout, |_, _| 60.0 + 3.0 * k as f64))) };
        assert!(rec.push(f).is_ok());
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(rec);
    let deadline = Instant::now() + Duration::from_secs(60);
    let s = loop {
        let s = hub.status(node).unwrap();
        if s.looping.as_ref().is_some_and(|l| l.done) {
            break s;
        }
        assert!(Instant::now() < deadline, "{s:?}");
        std::thread::sleep(Duration::from_millis(5));
    };
    let l = s.looping.clone().unwrap();
    assert_eq!((l.error.clone(), l.frames_out, l.fade_frames), (None, 30, 10), "{l:?}");
    assert!(l.out.ends_with("take_loop.mov"), "{l:?}");
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["loop"]["method"], "fade", "{json}");
    assert_eq!(hub.loops().all().len(), 1);
}

#[test]
fn recordings_read_back_frame_for_frame() {
    // A recording's timescale is its frame rate; reading frame i must not land on i + 1.
    let dir = scratch("index");
    for codec in [Codec::Prores422, Codec::H264] {
        let layout = codec.layout(64, 48);
        let frames: Vec<Vec<u8>> =
            (0..6).map(|k| planes(&layout, |_, _| 40.0 + 20.0 * f64::from(k))).collect();
        let input = write_clip(&dir.join(format!("steps.{}", codec.extension())), codec, 64, 48, 30, &frames);
        let mut d = penumbra_video::open(&input, &Limits::default()).unwrap();
        for i in [0, 1, 2, 3, 4, 5, 3, 0, 5, 1] {
            let t = thumbnail(d.frame(i).unwrap().pixels().unwrap()).unwrap();
            let want = 40.0 + 20.0 * i as f64;
            assert!((f64::from(t[0]) - want).abs() <= 2.0, "{codec:?} frame {i}: {} for {want}", t[0]);
        }
    }
}

/// How long each method takes on a 1080p60 H.264 clip of 10 s. Run with
/// `cargo test --release -p penumbra-record loop_speed -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, about a minute"]
fn loop_speed_at_1080p() {
    let dir = scratch("speed");
    let (w, h, fps, n) = (1920u32, 1080u32, 60u32, 600usize);
    let layout = Codec::H264.layout(w, h);
    let settings = Settings {
        path: dir.join("wave.mp4"),
        codec: Codec::H264,
        width: w,
        height: h,
        fps: Fps { num: fps, den: 1 },
        threads: crate::writer::default_threads(),
        queue: 1,
        h264: H264Preset::Fast,
    };
    let mut sink = Sink::create(&settings, |_| {}).unwrap();
    let rows: Vec<f64> = (0..h).map(|y| 10.0 * (f64::from(y) * 0.01).cos()).collect();
    for t in 0..n {
        let phase = std::f64::consts::TAU * t as f64 / 173.0;
        let wave: Vec<f64> = (0..w)
            .map(|x| 70.0 * (std::f64::consts::TAU * f64::from(x) / f64::from(w) * 3.0 + phase).sin())
            .collect();
        sink.push(&planes(&layout, |x, y| 128.0 + wave[x] + rows[y])).unwrap();
    }
    let input = sink.path().to_path_buf();
    sink.finish().unwrap();
    let jobs = Jobs::new();
    for method in Method::ALL {
        let started = Instant::now();
        let job = jobs.start(&input, Spec { min_seconds: 3.0, ..Spec::new(method) }).unwrap();
        let s = wait(&job);
        assert_eq!(s.error, None);
        let secs = started.elapsed().as_secs_f64();
        eprintln!(
            "{method:?}: {:.1} s for {} frames in, {} out: {:.0} frames per second out; cut {:?}..{:?} score {:?}",
            secs,
            s.frames_in,
            s.frames_out,
            s.frames_out as f64 / secs,
            s.loop_start,
            s.loop_end,
            s.score
        );
    }
}
