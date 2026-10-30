//! WAV recordings: the header in each format, placement of blocks, and whole recordings through
//! the hub, read back.

use std::time::{Duration, Instant};

use penumbra_core::NodeId;
use penumbra_core::scratch::ScratchDir;

use crate::Hub;
use crate::audio::AudioBlock;
use crate::wav::{self, Joiner, SoundStatus, WavFormat, WavSettings};

fn block(at: i64, channels: usize, n: usize, rate: u32) -> AudioBlock {
    let channels = (0..channels)
        .map(|c| (0..n).map(|k| ((at + k as i64) as f32 * 0.001 + c as f32 * 0.1).sin() * 0.5).collect())
        .collect();
    AudioBlock { at, rate, channels }
}

fn wait_done(hub: &Hub, node: NodeId) -> SoundStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = hub.sound_status(node).unwrap();
        if s.done {
            return s;
        }
        assert!(Instant::now() < deadline, "not finished: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn headers_have_the_sizes_and_the_format() {
    let h = wav::header(WavFormat::Pcm24, 48_000, 2, 10);
    assert_eq!(h.len(), 44);
    assert_eq!(&h[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 60);
    assert_eq!(u16::from_le_bytes([h[20], h[21]]), 1, "plain PCM");
    assert_eq!(u16::from_le_bytes([h[32], h[33]]), 6, "block align");
    assert_eq!(u16::from_le_bytes([h[34], h[35]]), 24);
    assert_eq!(&h[36..40], b"data");
    assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 60);
    // Mono 24-bit with an odd length: RIFF counts the pad byte.
    let h = wav::header(WavFormat::Pcm24, 44_100, 1, 3);
    assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 9 + 1);
    let h = wav::header(WavFormat::Float32, 48_000, 1, 5);
    assert_eq!(h.len(), 58);
    assert_eq!(u16::from_le_bytes([h[20], h[21]]), 3, "float");
    assert_eq!(&h[38..42], b"fact");
    assert_eq!(u32::from_le_bytes(h[46..50].try_into().unwrap()), 5);
    let h = wav::header(WavFormat::Pcm24, 96_000, 6, 0);
    assert_eq!(h.len(), 80);
    assert_eq!(u16::from_le_bytes([h[20], h[21]]), 0xFFFE, "extensible");
    assert_eq!(u16::from_le_bytes([h[44], h[45]]), 1, "PCM sub-format");
    assert_eq!(&h[48..60], &[0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
}

#[test]
fn blocks_join_and_jumps_leave_no_gap() {
    let rate = 48_000;
    let mut j = Joiner::default();
    // Wholly before the start: cut, nothing placed.
    let p = j.place(-800, 800, rate);
    assert_eq!((p.skip, j.placer.written), (800, 0));
    // Straddling: the part before the start cut.
    let p = j.place(-100, 800, rate);
    assert_eq!((p.pad, p.skip, j.placer.written), (0, 100, 700));
    j.place(700, 800, rate);
    assert_eq!(j.placer.written, 1500);
    // Ahead by 0.5 s: silence for the time between.
    let p = j.place(1500 + 24_000, 800, rate);
    assert_eq!((p.pad, j.placer.written), (24_000, 26_300));
    // Ahead by 5 s, or back by 2 s: continues without a gap.
    let p = j.place(26_300 + 240_000, 800, rate);
    assert_eq!((p.pad, p.skip, j.placer.written), (0, 0, 27_100));
    let p = j.place(27_100 + 240_000 - 96_000, 800, rate);
    assert_eq!((p.pad, p.skip, j.placer.written), (0, 0, 27_900));
    // The jumps count: the next block in show order follows on.
    let p = j.place(27_100 + 240_000 - 96_000 + 800, 800, rate);
    assert_eq!((p.pad, p.skip, j.placer.written), (0, 0, 28_700));
    // A first block far ahead starts the file at once.
    let mut j = Joiner::default();
    let p = j.place(10 * 48_000, 800, rate);
    assert_eq!((p.pad, j.placer.written), (0, 800));
}

#[test]
fn a_recording_reads_back_sample_for_sample() {
    let dir = ScratchDir::new("record-wav").unwrap();
    let hub = Hub::new();
    let node = NodeId(7);
    hub.attach_sound(node, "rec");
    for (format, channels, tag, tolerance) in [
        (WavFormat::Pcm24, 2, 1, 1.0 / 8_388_607.0),
        (WavFormat::Float32, 1, 3, 0.0),
        (WavFormat::Pcm24, 4, 0xFFFE, 1e-6),
    ] {
        let settings = WavSettings { path: dir.join("take.wav"), format };
        let rec = hub.start_sound(node, settings).unwrap();
        let mut want: Vec<AudioBlock> = Vec::new();
        for k in 0..60 {
            let b = block(k * 800, channels, 800, 48_000);
            want.push(b.clone());
            rec.push(b);
        }
        drop(rec);
        let s = wait_done(&hub, node);
        assert_eq!(s.error, None);
        assert_eq!(
            (s.samples, s.rate, s.channels, s.format.as_str()),
            (48_000, 48_000, channels, format.name())
        );
        assert_eq!(s.seconds, 1.0);
        let w = wav::read(std::path::Path::new(&s.file)).unwrap();
        assert_eq!((w.format, w.rate, w.tag, w.channels.len()), (format, 48_000, tag, channels));
        assert_eq!(std::fs::metadata(&s.file).unwrap().len(), s.bytes);
        for (c, got) in w.channels.iter().enumerate() {
            let expect: Vec<f32> = want.iter().flat_map(|b| b.channels[c].clone()).collect();
            assert_eq!(got.len(), expect.len());
            let worst = got.iter().zip(&expect).map(|(g, e)| (g - e).abs()).fold(0.0, f32::max);
            assert!(worst <= tolerance as f32 + 1e-7, "{format:?} {channels}: {worst}");
        }
    }
    // Never overwritten: the three takes are take, take_2, take_3.
    for name in ["take.wav", "take_2.wav", "take_3.wav"] {
        assert!(dir.join(name).exists(), "{name}");
    }
}

#[test]
fn another_format_is_refused_and_no_sound_writes_no_file() {
    let dir = ScratchDir::new("record-wav-refused").unwrap();
    let hub = Hub::new();
    let node = NodeId(3);
    let rec =
        hub.start_sound(node, WavSettings { path: dir.join("a.wav"), format: WavFormat::Pcm24 }).unwrap();
    rec.push(block(0, 2, 480, 48_000));
    rec.push(block(480, 1, 480, 48_000));
    rec.push(block(480, 2, 441, 44_100));
    rec.push(block(480, 2, 480, 48_000));
    drop(rec);
    let s = wait_done(&hub, node);
    assert_eq!((s.samples, s.refused, s.error), (960, 2, None));
    // Mono 24-bit of odd length: the pad byte is there, the reader takes the samples only.
    let rec =
        hub.start_sound(node, WavSettings { path: dir.join("odd.wav"), format: WavFormat::Pcm24 }).unwrap();
    rec.push(block(0, 1, 101, 48_000));
    drop(rec);
    let s = wait_done(&hub, node);
    assert_eq!(std::fs::metadata(&s.file).unwrap().len(), 44 + 303 + 1);
    assert_eq!(wav::read(std::path::Path::new(&s.file)).unwrap().channels[0].len(), 101);
    // No sound came: no file.
    let rec =
        hub.start_sound(node, WavSettings { path: dir.join("none.wav"), format: WavFormat::Pcm24 }).unwrap();
    drop(rec);
    let s = wait_done(&hub, node);
    assert!(!s.opened && s.error.is_none(), "{s:?}");
    assert!(!dir.join("none.wav").exists());
}

/// While it records, the header already has the sizes of what is written: a crash leaves a
/// file that reads.
#[test]
fn the_header_follows_the_recording() {
    let dir = ScratchDir::new("record-wav-crash").unwrap();
    let hub = Hub::new();
    let node = NodeId(4);
    let rec = hub
        .start_sound(node, WavSettings { path: dir.join("live.wav"), format: WavFormat::Float32 })
        .unwrap();
    rec.push(block(0, 2, 4800, 48_000));
    std::thread::sleep(wav::HEADER_EVERY + Duration::from_millis(50));
    rec.push(block(4800, 2, 4800, 48_000));
    let deadline = Instant::now() + Duration::from_secs(10);
    let file = loop {
        let s = rec.status();
        if s.samples == 9600 {
            break s.file;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    // Without a stop, the header has the second block within the interval.
    let read = || wav::read(std::path::Path::new(&file)).unwrap().channels[0].len();
    assert!(read() >= 4800);
    let deadline = Instant::now() + 2 * wav::HEADER_EVERY + Duration::from_millis(500);
    while read() < 9600 {
        assert!(Instant::now() < deadline, "the header lags: {}", read());
        std::thread::sleep(Duration::from_millis(20));
    }
    hub.shutdown(Duration::from_secs(5));
    let s = rec.status();
    assert!(s.done && s.error.is_none(), "{s:?}");
    drop(rec);
}

#[test]
fn a_folder_that_cannot_be_made_is_an_error() {
    let dir = ScratchDir::new("record-wav-blocked").unwrap();
    std::fs::write(dir.join("blocker"), b"a file").unwrap();
    let hub = Hub::new();
    let node = NodeId(5);
    let rec = hub
        .start_sound(node, WavSettings { path: dir.join("blocker/x.wav"), format: WavFormat::Pcm24 })
        .unwrap();
    rec.push(block(0, 2, 480, 48_000));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rec.status().done {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(rec.status().error.unwrap().contains("cannot create the folder"), "{:?}", rec.status());
    assert!(!rec.is_running());
}
