use std::path::Path;
use std::time::{Duration, Instant};

use penumbra_core::NodeId;
use penumbra_core::scratch::ScratchDir;
use penumbra_video::{Limits, PixelData};

use crate::writer::default_threads;
use crate::{Bytes, Codec, Fps, Frame, H264Preset, Hub, Layout, Planes, Settings};

/// A scratch directory per test, removed when the returned guard drops.
fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("record-{name}")).unwrap()
}

/// Planes of one flat colour, given as 8-bit video-range values (scaled up for more bits).
fn flat(layout: &Layout, y: u8, cb: u8, cr: u8) -> Vec<u8> {
    let mut b = vec![0u8; layout.bytes()];
    let [oy, ou, ov, oa] = layout.offsets();
    let scale = 1u16 << (layout.bits - 8);
    let top = (1u16 << layout.bits) - 1;
    let mut fill = |from: usize, to: usize, v: u16| {
        if layout.sample_bytes() == 1 {
            b[from..to].fill(v as u8);
        } else {
            for s in b[from..to].as_chunks_mut::<2>().0 {
                s.copy_from_slice(&v.to_le_bytes());
            }
        }
    };
    fill(oy, ou, u16::from(y) * scale);
    fill(ou, ov, u16::from(cb) * scale);
    fill(ov, oa, u16::from(cr) * scale);
    if layout.alpha {
        fill(oa, layout.bytes(), top);
    }
    b
}

fn settings(path: &Path, codec: Codec, w: u32, h: u32) -> Settings {
    Settings {
        path: path.to_path_buf(),
        codec,
        width: w,
        height: h,
        fps: Fps { num: 30, den: 1 },
        threads: 2,
        queue: 4,
        h264: H264Preset::Balanced,
    }
}

fn wait_done(hub: &Hub, node: NodeId) -> crate::Status {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = hub.status(node).unwrap();
        if s.done {
            return s;
        }
        assert!(Instant::now() < deadline, "the recording did not finish: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Mean of the first plane read back, as 8-bit.
fn mean_luma(frame: &penumbra_video::Frame) -> f64 {
    let px = frame.pixels().unwrap();
    match &px.data {
        PixelData::Yuv8 { planes, .. } => {
            planes[0].iter().map(|&v| f64::from(v)).sum::<f64>() / planes[0].len() as f64
        }
        PixelData::Yuv16 { planes, bits, .. } => {
            let s = f64::from(1u32 << (bits - 8));
            planes[0].iter().map(|&v| f64::from(v) / s).sum::<f64>() / planes[0].len() as f64
        }
        other => panic!("unexpected pixels {other:?}"),
    }
}

#[test]
fn every_codec_records_and_reads_back() {
    let dir = scratch("codecs");
    let hub = Hub::new();
    // Every codec, and H.264 with each preset.
    let takes = Codec::ALL
        .into_iter()
        .map(|c| (c, H264Preset::Balanced))
        .chain([(Codec::H264, H264Preset::Fast), (Codec::H264, H264Preset::Best)]);
    for (i, (codec, preset)) in takes.enumerate() {
        let node = NodeId(i as u64 + 1);
        hub.attach(node, &format!("/rec{i}"));
        let path = dir.join(format!("{}_{i}.{}", codec.name(), codec.extension()));
        let (w, h) = (64, 48);
        let rec = hub.start(node, Settings { h264: preset, ..settings(&path, codec, w, h) }).unwrap();
        let layout = rec.layout();
        // Dark then light, five frames each (pushed slowly enough for the queue).
        for k in 0..10 {
            let y = if k < 5 { 40 } else { 200 };
            let mut frame = Frame { index: k, planes: Box::new(Bytes(flat(&layout, y, 128, 128))) };
            let deadline = Instant::now() + Duration::from_secs(30);
            while let Err(f) = rec.push(frame) {
                assert!(Instant::now() < deadline, "the queue never had room: {:?}", rec.status());
                frame = f;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        drop(rec);
        let s = wait_done(&hub, node);
        assert_eq!(s.error, None, "{codec:?} {preset:?}");
        assert_eq!((s.frames, s.dropped), (10, 0), "{codec:?} {preset:?}");
        assert_eq!(s.node, format!("/rec{i}"));
        let mut d = penumbra_video::open(&path, &Limits::default()).unwrap();
        let info = d.info().clone();
        assert_eq!((info.width, info.height, info.frames), (w, h, 10), "{codec:?} {preset:?}: {info:?}");
        assert!((info.rate() - 30.0).abs() < 1e-6, "{codec:?} {preset:?}: {}", info.rate());
        let dark = mean_luma(&d.frame(2).unwrap());
        let light = mean_luma(&d.frame(7).unwrap());
        assert!((dark - 40.0).abs() < 1.5, "{codec:?} {preset:?}: dark {dark}");
        assert!((light - 200.0).abs() < 1.5, "{codec:?} {preset:?}: light {light}");
    }
}

#[test]
fn a_gap_makes_the_frame_before_last_longer() {
    let dir = scratch("gap");
    let hub = Hub::new();
    let node = NodeId(1);
    let path = dir.join("gap.mov");
    let rec = hub.start(node, settings(&path, Codec::Prores422, 32, 32)).unwrap();
    let layout = rec.layout();
    for k in [0, 1, 4] {
        let f = Frame { index: k, planes: Box::new(Bytes(flat(&layout, 100, 128, 128))) };
        assert!(rec.push(f).is_ok());
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(rec);
    let s = wait_done(&hub, node);
    assert_eq!(s.error, None);
    assert_eq!(s.frames, 3);
    assert!((s.seconds - 5.0 / 30.0).abs() < 1e-9, "{}", s.seconds);
    let d = penumbra_video::open(&path, &Limits::default()).unwrap();
    assert!((d.info().duration() - 5.0 / 30.0).abs() < 1e-3, "{:?}", d.info());
}

#[test]
fn files_are_never_overwritten_and_bad_paths_are_errors() {
    let dir = scratch("paths");
    let hub = Hub::new();
    let existing = dir.join("taken.mp4");
    std::fs::write(&existing, b"keep me").unwrap();
    for (k, want) in ["taken_2.mp4", "taken_3.mp4"].iter().enumerate() {
        let node = NodeId(10 + k as u64);
        let rec = hub.start(node, settings(&existing, Codec::H264, 32, 32)).unwrap();
        let layout = rec.layout();
        assert!(rec.push(Frame { index: 0, planes: Box::new(Bytes(flat(&layout, 100, 128, 128))) }).is_ok());
        drop(rec);
        let s = wait_done(&hub, node);
        assert_eq!(s.error, None);
        assert_eq!(Path::new(&s.file), dir.join(want), "an existing file gets a number");
    }
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    // A folder below a file cannot be created.
    let below_a_file = existing.join("x.mp4");
    let _rec = hub.start(NodeId(2), settings(&below_a_file, Codec::H264, 32, 32)).unwrap();
    let s = wait_done(&hub, NodeId(2));
    assert!(s.error.is_some(), "{s:?}");
    // Odd and tiny sizes are refused before anything starts.
    assert!(hub.start(NodeId(3), settings(&dir.join("odd.mp4"), Codec::H264, 33, 32)).is_err());
    assert!(hub.start(NodeId(3), settings(&dir.join("tiny.mp4"), Codec::H264, 8, 8)).is_err());
}

/// Planes that take their time, like a readback the GPU is slow to deliver: the queue fills.
struct Slow(Vec<u8>);

impl Planes for Slow {
    fn read(
        &mut self,
        _timeout: Duration,
        f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        std::thread::sleep(Duration::from_millis(100));
        f(&self.0)
    }
}

#[test]
fn a_full_queue_drops_frames_instead_of_waiting() {
    let dir = scratch("queue");
    let hub = Hub::new();
    let node = NodeId(1);
    let rec = hub.start(node, settings(&dir.join("slow.mov"), Codec::Prores422, 32, 32)).unwrap();
    let layout = rec.layout();
    let started = Instant::now();
    let mut refused = 0;
    for k in 0..20 {
        rec.due(k);
        let f = Frame { index: k, planes: Box::new(Slow(flat(&layout, 100, 128, 128))) };
        if rec.push(f).is_err() {
            refused += 1;
            rec.dropped(1);
        }
    }
    assert!(started.elapsed() < Duration::from_millis(100), "push waited: {:?}", started.elapsed());
    assert!(refused >= 14, "only {refused} of 20 refused with a queue of 4");
    assert!(!rec.has_room(4));
    drop(rec);
    let s = wait_done(&hub, node);
    assert_eq!(s.error, None);
    assert_eq!(s.frames + s.dropped, 20, "{s:?}");
    // The frames dropped at the end make the last frame written longer: the file lasts 20.
    assert!((s.seconds - 20.0 / 30.0).abs() < 1e-9, "{s:?}");
    let d = penumbra_video::open(&dir.join("slow.mov"), &Limits::default()).unwrap();
    assert_eq!(d.info().frames, 20, "{:?}", d.info());
}

#[test]
fn shutdown_finishes_files() {
    let dir = scratch("hub");
    let hub = Hub::new();
    let node = NodeId(7);
    hub.attach(node, "/out/video1");
    assert_eq!(hub.status(node), None, "nothing recorded yet");
    // A recording whose handle is still alive is finished by the shutdown.
    let path = dir.join("open.mov");
    let rec = hub.start(node, settings(&path, Codec::Prores422, 32, 32)).unwrap();
    let layout = rec.layout();
    assert!(rec.push(Frame { index: 0, planes: Box::new(Bytes(flat(&layout, 100, 128, 128))) }).is_ok());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(hub.shutdown(Duration::from_secs(10)), 0);
    let s = hub.status(node).unwrap();
    assert!(s.done && s.error.is_none() && s.frames == 1, "{s:?}");
    assert_eq!(hub.all().len(), 1);
    assert!(hub.start(node, settings(&dir.join("late.mov"), Codec::Prores422, 32, 32)).is_err());
    let d = penumbra_video::open(&path, &Limits::default()).unwrap();
    assert_eq!(d.info().frames, 1);
    drop(rec);
}

#[test]
fn layouts_and_rates() {
    let l = Codec::H264.layout(1918, 1080);
    assert_eq!((l.luma_stride(), l.chroma_stride(), l.chroma_size()), (1920, 960, (959, 540)));
    assert_eq!(l.offsets(), [0, 1920 * 1080, 1920 * 1080 + 960 * 540, 1920 * 1080 + 2 * 960 * 540]);
    assert_eq!(l.bytes(), 1920 * 1080 * 3 / 2);
    let p = Codec::Prores4444.layout(1920, 1080);
    assert_eq!(p.bytes(), 1920 * 1080 * 2 * 4);
    assert!(Codec::Prores4444.layout(8192, 8192).check().is_err(), "more than a storage buffer");
    assert!(Codec::H264.layout(8192, 4320).check().is_ok());
    assert_eq!(Fps::from_f64(60.0), Some(Fps { num: 60, den: 1 }));
    assert_eq!(Fps::from_f64(29.97), Some(Fps { num: 29970, den: 1000 }));
    assert_eq!(Fps::from_f64(f64::NAN), None);
    assert_eq!(Fps::from_f64(0.0), None);
    for (name, codec) in Codec::NAMES.iter().zip(Codec::ALL) {
        assert_eq!(Codec::parse(name), Some(codec));
        assert_eq!(codec.name(), *name);
    }
    assert!(default_threads() >= 2);
    for (name, preset) in H264Preset::NAMES.iter().zip(H264Preset::ALL) {
        assert_eq!(H264Preset::parse(name), Some(preset));
        assert_eq!(preset.name(), *name);
    }
    assert_eq!(H264Preset::default(), H264Preset::Balanced);
}
