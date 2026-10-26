use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use penumbra_core::scratch::ScratchDir;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// `ramp30.mp4`: 64x32 H.264 with B-frames, 30 frames at 30 fps, GOP 10; frame n is grey
/// 20 + 6 n (full range) everywhere, stored in limited range.
fn ramp() -> PathBuf {
    fixture("ramp30.mp4")
}

/// The frame number a ramp frame shows.
fn ramp_index(f: &Frame) -> usize {
    let f = f.pixels().expect("pixels");
    let PixelData::Yuv8 { planes, .. } = &f.data else {
        panic!("expected 8-bit Y'CbCr, got {}", f.format_label())
    };
    let y = &planes[0];
    let mean = y.iter().map(|&v| f64::from(v)).sum::<f64>() / y.len() as f64;
    let full = (mean - 16.0) * 255.0 / 219.0;
    ((full - 20.0) / 6.0).round() as usize
}

/// A GIF with four frames of different delays (10, 20, 30, 40 hundredths), each one grey level.
fn gif_file(dir: &std::path::Path) -> PathBuf {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame as ImageFrame, RgbaImage};
    let path = dir.join("steps.gif");
    let file = std::fs::File::create(&path).unwrap();
    let mut enc = GifEncoder::new(file);
    for (i, cs) in [10u32, 20, 30, 40].into_iter().enumerate() {
        let v = 40 * (i as u8 + 1);
        let img = RgbaImage::from_pixel(8, 4, image::Rgba([v, v, v, 255]));
        enc.encode_frame(ImageFrame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(cs * 10, 1))).unwrap();
    }
    drop(enc);
    path
}

/// A scratch directory per test, removed when the returned guard drops.
fn temp_dir(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("video-{name}")).unwrap()
}

#[test]
fn opens_h264_and_decodes_every_frame_in_order_and_after_a_seek() {
    let mut d = open(&ramp(), &Limits::default()).unwrap();
    let info = d.info().clone();
    assert_eq!((info.width, info.height, info.frames), (64, 32, 30));
    assert_eq!(info.timing, Timing::Rate { num: 30, den: 1 });
    assert!((info.duration() - 1.0).abs() < 1e-9);
    for i in 0..30 {
        assert_eq!(ramp_index(&d.frame(i).unwrap()), i);
    }
    // Backwards and into the middle of a GOP.
    assert_eq!(ramp_index(&d.frame(13).unwrap()), 13);
    assert_eq!(ramp_index(&d.frame(2).unwrap()), 2);
}

#[test]
fn timing_maps_times_to_frames() {
    let info = ClipInfo {
        width: 1,
        height: 1,
        frames: 4,
        timing: Timing::Ends(vec![0.1, 0.3, 0.6, 1.0].into()),
        codec: String::new(),
        container: String::new(),
        seeks: true,
    };
    assert_eq!(info.index_at(-1.0), 0);
    assert_eq!(info.index_at(0.0), 0);
    assert_eq!(info.index_at(0.1), 1);
    assert_eq!(info.index_at(0.59), 2);
    assert_eq!(info.index_at(5.0), 3);
    assert!((info.start_of(2) - 0.3).abs() < 1e-12);
    let rate = ClipInfo { timing: Timing::Rate { num: 30000, den: 1001 }, frames: 100, ..info };
    // Frame 3 starts at 3 * 1001 / 30000 s; exactly there it must be frame 3, not 2.
    assert_eq!(rate.index_at(rate.start_of(3)), 3);
}

#[test]
fn gif_frames_and_delays() {
    let dir = temp_dir("gif");
    let mut d = open(&gif_file(&dir), &Limits::default()).unwrap();
    let info = d.info().clone();
    assert_eq!((info.width, info.height, info.frames), (8, 4, 4));
    assert_eq!(info.timing, Timing::Ends(vec![0.1, 0.3, 0.6, 1.0].into()));
    let grey = |f: Frame| match &f.pixels().unwrap().data {
        PixelData::Rgba8(d) => d[0],
        _ => panic!("expected RGBA8"),
    };
    assert_eq!(grey(d.frame(0).unwrap()), 40);
    assert_eq!(grey(d.frame(3).unwrap()), 160);
    // Backwards rewinds.
    assert_eq!(grey(d.frame(1).unwrap()), 80);
    assert!(d.frame(4).is_err());
}

#[test]
fn gif_disposal_keeps_clears_and_restores() {
    // 4x2 canvas. Frame 0: all red, kept. Frame 1: green 2x1 at (1, 0), restored to what was
    // there afterwards. Frame 2: blue 1x1 at (3, 1), cleared to transparent afterwards. Frame
    // 3: a 1x1 frame at (0, 1) whose only pixel is transparent.
    let dir = temp_dir("dispose");
    let path = dir.join("dispose.gif");
    {
        let file = std::fs::File::create(&path).unwrap();
        let palette = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let mut enc = ::gif::Encoder::new(file, 4, 2, &palette).unwrap();
        let mut frame = |left, top, width, height, index: u8, dispose, transparent: Option<u8>| {
            let f = ::gif::Frame {
                left,
                top,
                width,
                height,
                delay: 5,
                dispose,
                transparent,
                buffer: vec![index; usize::from(width) * usize::from(height)].into(),
                ..::gif::Frame::default()
            };
            enc.write_frame(&f).unwrap();
        };
        use ::gif::DisposalMethod::{Background, Keep, Previous};
        frame(0, 0, 4, 2, 0, Keep, None);
        frame(1, 0, 2, 1, 1, Previous, None);
        frame(3, 1, 1, 1, 2, Background, None);
        frame(0, 1, 1, 1, 3, Keep, Some(3));
    }
    let mut d = open(&path, &Limits::default()).unwrap();
    let px = |f: &Frame, x: usize, y: usize| match &f.pixels().unwrap().data {
        PixelData::Rgba8(d) => {
            let i = (y * 4 + x) * 4;
            [d[i], d[i + 1], d[i + 2], d[i + 3]]
        }
        _ => panic!("expected RGBA8"),
    };
    let (red, green, blue, clear) = ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0, 0, 0, 0]);
    let f1 = d.frame(1).unwrap();
    assert_eq!([px(&f1, 0, 0), px(&f1, 1, 0), px(&f1, 2, 0), px(&f1, 3, 0)], [red, green, green, red]);
    let f2 = d.frame(2).unwrap();
    assert_eq!([px(&f2, 1, 0), px(&f2, 3, 1)], [red, blue]);
    let f3 = d.frame(3).unwrap();
    assert_eq!([px(&f3, 3, 1), px(&f3, 0, 1), px(&f3, 1, 0)], [clear, red, red]);
    // Rewinding gives the same pictures.
    assert_eq!(px(&d.frame(1).unwrap(), 1, 0), green);
}

#[test]
fn refuses_what_is_not_a_video() {
    let dir = temp_dir("refuse");
    let junk = dir.join("junk.mp4");
    std::fs::write(&junk, b"this is not a video at all").unwrap();
    assert!(open(&junk, &Limits::default()).is_err());
    assert!(open(&dir.join("missing.mp4"), &Limits::default()).is_err());
    let small = Limits { max_side: 32, ..Limits::default() };
    let e = open(&ramp(), &small).err().unwrap();
    assert!(e.contains("64x32"), "{e}");
    // A truncated file opens or fails, but never panics.
    let bytes = std::fs::read(ramp()).unwrap();
    let cut = dir.join("cut.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    if let Ok(mut d) = open(&cut, &Limits::default()) {
        for i in 0..d.info().frames {
            let _ = d.frame(i);
        }
    }
}

/// HAP fixtures written by ffmpeg 8 (`-c:v hap`): 40x24, 4 frames at 10 fps. The top 12 rows of
/// frame n are grey 20 + 40 n, the rest blue; alpha is 255 left of x = 20 and 128 right of it.
/// `hap1.mov` is Hap (BC1, Snappy), `hap5_chunks.mov` Hap Alpha in 2 chunks, `hapy_raw.mov` Hap Q
/// without second-stage compression.
fn hap_frames(name: &str) -> (ClipInfo, Vec<Frame>) {
    let mut d = open(&fixture(name), &Limits::default()).unwrap();
    let info = d.info().clone();
    let frames = (0..info.frames).map(|i| d.frame(i).unwrap()).collect();
    (info, frames)
}

fn hap(f: &Frame) -> &HapFrame {
    match f {
        Frame::Hap(h) => h,
        Frame::Pixels(_) => panic!("expected a HAP frame"),
    }
}

/// The 16 bytes of the 4x4 block at block column `bx`, row `by` of a BC3 (or 8 of a BC1)
/// texture 40 pixels (10 blocks) wide.
fn block(t: &BcTexture, bx: usize, by: usize) -> &[u8] {
    let n = t.format.block_bytes();
    let at = (by * 10 + bx) * n;
    &t.data[at..at + n]
}

/// The top-left pixel of a BC1 colour block (also the colour half of BC3), as 8-bit RGB.
fn bc1_pixel0(b: &[u8]) -> [i32; 3] {
    let rgb = |c: u16| {
        let c = i32::from(c);
        let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
        [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
    };
    let (c0, c1) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
    let (e0, e1) = (rgb(c0), rgb(c1));
    let mix = |w0: i32, w1: i32, d: i32| [0, 1, 2].map(|k| (w0 * e0[k] + w1 * e1[k]) / d);
    match b[4] & 3 {
        0 => e0,
        1 => e1,
        2 if c0 > c1 => mix(2, 1, 3),
        3 if c0 > c1 => mix(1, 2, 3),
        2 => mix(1, 1, 2),
        _ => [0, 0, 0],
    }
}

fn near(a: i32, b: i32, tolerance: i32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn hap_files_from_ffmpeg() {
    let (info, frames) = hap_frames("hap1.mov");
    assert_eq!((info.width, info.height, info.frames, info.codec.as_str()), (40, 24, 4, "Hap"));
    assert_eq!(info.rate(), 10.0);
    assert!(info.seeks);
    for (n, f) in frames.iter().enumerate() {
        let h = hap(f);
        assert_eq!(
            (h.color, h.texture.format, h.texture.data.len()),
            (HapColor::Rgb, BcFormat::Bc1, 10 * 6 * 8)
        );
        let grey = 20 + 40 * n as i32;
        let top = bc1_pixel0(block(&h.texture, 0, 0));
        assert!(top.iter().all(|&c| near(c, grey, 8)), "frame {n}: {top:?}");
        let bottom = bc1_pixel0(block(&h.texture, 0, 5));
        assert!(near(bottom[0], 0, 8) && near(bottom[2], 255, 8), "frame {n}: {bottom:?}");
    }

    let (info, frames) = hap_frames("hap5_chunks.mov");
    assert_eq!(info.codec, "Hap Alpha");
    for f in &frames {
        let h = hap(f);
        assert_eq!((h.color, h.texture.format), (HapColor::Rgba, BcFormat::Bc3));
        // BC3: the first byte is the first alpha endpoint.
        assert_eq!(block(&h.texture, 0, 0)[0], 255);
        assert!(near(i32::from(block(&h.texture, 9, 5)[0]), 128, 1));
    }

    let (info, frames) = hap_frames("hapy_raw.mov");
    assert_eq!(info.codec, "Hap Q");
    for (n, f) in frames.iter().enumerate() {
        let h = hap(f);
        assert_eq!((h.color, h.texture.format), (HapColor::YCoCg, BcFormat::Bc3));
        // Y is in alpha, so the first alpha endpoint is the grey's luma.
        let y = i32::from(block(&h.texture, 0, 0)[0]);
        assert!(near(y, 20 + 40 * n as i32, 4), "frame {n}: Y {y}");
    }
}

#[test]
fn hap_player_loops_without_seeking() {
    let mut p = Player::new(&fixture("hap1.mov"), Limits::default(), None).unwrap();
    let Shown::New(first) = wait_for(&mut p, 0.0, true) else { panic!("no first frame") };
    assert_eq!(hap(&first).width, 40);
    // Three passes of the 0.4 s clip in real time: every frame due is decoded already.
    let start = Instant::now();
    for step in 1..=12 {
        // Mid-frame, so float rounding at the loop point cannot pick the frame before.
        let t = (f64::from(step) + 0.5) / 10.0;
        if let Some(wait) = (start + Duration::from_secs_f64(t)).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        assert!(matches!(p.show(t, true).unwrap(), Shown::New(_)), "step {step}: {:?}", p.stats());
    }
    let stats = p.stats();
    assert_eq!((stats.loops, stats.seeks, stats.late), (3, 0, 0), "{stats:?}");
}

#[test]
fn damaged_hap_files_fail_without_panicking() {
    let dir = temp_dir("hap");
    let bytes = std::fs::read(fixture("hap5_chunks.mov")).unwrap();
    // Every 7th byte flipped in turn, and every truncation length in steps of 13.
    for i in (0..bytes.len()).step_by(7) {
        let mut b = bytes.clone();
        b[i] ^= 0xA5;
        let path = dir.join("flip.mov");
        std::fs::write(&path, &b).unwrap();
        if let Ok(mut d) = open(&path, &Limits::default()) {
            for k in 0..d.info().frames.min(8) {
                let _ = d.frame(k);
            }
        }
    }
    for n in (0..bytes.len()).step_by(13) {
        let path = dir.join("cut.mov");
        std::fs::write(&path, &bytes[..n]).unwrap();
        if let Ok(mut d) = open(&path, &Limits::default()) {
            for k in 0..d.info().frames.min(8) {
                let _ = d.frame(k);
            }
        }
    }
}

/// Waits (polling, as the cook thread would every frame) until the player has the frame due.
fn wait_for(p: &mut Player, t: f64, looping: bool) -> Shown {
    let start = Instant::now();
    loop {
        match p.show(t, looping).unwrap() {
            Shown::Waiting | Shown::Late if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            s => return s,
        }
    }
}

#[test]
fn player_loops_onto_a_decoded_start_without_seeking() {
    let mut p = Player::new(&ramp(), Limits::default(), None).unwrap();
    let Shown::New(first) = wait_for(&mut p, 0.0, true) else { panic!("no first frame") };
    assert_eq!(ramp_index(&first), 0);
    // Three passes through the 1 s clip at a 60 Hz show rate, in real time: each loop point is
    // served by the thread that decoded the start ahead, never by a seek. A machine too busy
    // to decode in real time makes frames late; they are waited for (the show keeps the last
    // frame meanwhile), since being late is the machine's doing and a seek the player's.
    let start = Instant::now();
    let mut seen = Vec::new();
    for step in 1..=180 {
        let t = f64::from(step) / 60.0;
        let due = start + Duration::from_secs_f64(t);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        match wait_for(&mut p, t, true) {
            Shown::New(f) => seen.push(ramp_index(&f)),
            Shown::Same => {}
            other => panic!("step {step}: {other:?}, stats {:?}", p.stats()),
        }
    }
    let stats = p.stats();
    assert_eq!(stats.loops, 3, "{stats:?}");
    assert_eq!(stats.seeks, 0, "{stats:?}");
    // 1, 2, ..., 29, 0, 1, ... : every frame once per pass, in order.
    let expect: Vec<usize> = (1..=90).map(|i| i % 30).collect();
    assert_eq!(seen, expect);
}

#[test]
fn player_holds_the_last_frame_and_seeks_backwards() {
    let mut p = Player::new(&ramp(), Limits::default(), None).unwrap();
    let Shown::New(f) = wait_for(&mut p, 5.0, false) else { panic!("no frame") };
    assert_eq!(ramp_index(&f), 29);
    assert!(matches!(p.show(7.0, false).unwrap(), Shown::Same));
    let Shown::New(f) = wait_for(&mut p, 0.5, false) else { panic!("no frame after the seek") };
    assert_eq!(ramp_index(&f), 15);
    assert!(p.stats().seeks >= 1);
}

/// Calls `show` at `t` for a while, as a cook thread would every frame, so the second thread
/// gets its pre-roll decoded.
fn hold(p: &mut Player, t: f64, looping: bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        p.show(t, looping).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn player_restarts_at_the_cue_without_seeking() {
    // Not looping: at the end the second thread holds the cue.
    let mut p = Player::new(&ramp(), Limits::default(), None).unwrap();
    p.set_cue(0.5);
    let Shown::New(f) = wait_for(&mut p, 5.0, false) else { panic!("no frame") };
    assert_eq!(ramp_index(&f), 29);
    hold(&mut p, 5.0, false);
    p.restart();
    let Shown::New(f) = p.show(0.5, false).unwrap() else { panic!("the cue was not ready: {:?}", p.stats()) };
    assert_eq!(ramp_index(&f), 15);
    let Shown::New(f) = wait_for(&mut p, 0.55, false) else { panic!("no frame after the cue") };
    assert_eq!(ramp_index(&f), 16);
    let stats = p.stats();
    assert_eq!((stats.seeks, stats.restarts, stats.loops), (0, 1, 0), "{stats:?}");
    assert_eq!(p.position(), Some((0, 16)));

    // Looping with the cue at the start: the loop's pre-roll serves the restart.
    let mut p = Player::new(&ramp(), Limits::default(), None).unwrap();
    let Shown::New(_) = wait_for(&mut p, 0.4, true) else { panic!("no frame") };
    hold(&mut p, 0.4, true);
    p.restart();
    let Shown::New(f) = p.show(0.0, true).unwrap() else {
        panic!("the start was not ready: {:?}", p.stats())
    };
    assert_eq!(ramp_index(&f), 0);
    let stats = p.stats();
    assert_eq!((stats.seeks, stats.restarts, stats.loops), (0, 1, 0), "{stats:?}");
    // Playback goes on from the new pass, loops included.
    let Shown::New(f) = wait_for(&mut p, 1.1, true) else { panic!("no frame after the loop") };
    assert_eq!(ramp_index(&f), 3);
    assert_eq!(p.position(), Some((1, 3)));
}

#[test]
fn player_reports_open_errors() {
    let mut p = Player::new(&fixture("missing.mp4"), Limits::default(), None).unwrap();
    let start = Instant::now();
    loop {
        match p.show(0.0, true) {
            Err(e) => {
                assert!(e.contains("missing.mp4"), "{e}");
                break;
            }
            Ok(_) if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Ok(s) => panic!("no error: {s:?}"),
        }
    }
}

/// Decode speed of a file named by `PENUMBRA_BENCH_FILE` (manual: `cargo test -p
/// penumbra-video bench -- --ignored --nocapture`).
#[test]
#[ignore]
fn bench() {
    let Ok(path) = std::env::var("PENUMBRA_BENCH_FILE") else { return };
    let start = Instant::now();
    let mut d = open(std::path::Path::new(&path), &Limits::default()).unwrap();
    let info = d.info().clone();
    println!("open {:?}: {info:?}", start.elapsed());
    let n = info.frames.min(120);
    let start = Instant::now();
    let mut worst = Duration::ZERO;
    for i in 0..n {
        let t = Instant::now();
        d.frame(i).unwrap();
        worst = worst.max(t.elapsed());
    }
    let per = start.elapsed() / n as u32;
    println!("{n} frames: {per:?} each, worst {worst:?}; clip rate {:.2} fps", info.rate());
}
