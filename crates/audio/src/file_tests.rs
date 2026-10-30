//! Sound files: every format decodes to the right tone, level and length; streamed files read
//! the same as whole ones, wrap at the loop, seek and keep their cue.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::file::{Frame, SoundFile, Status, WavFormat, wav_bytes};

pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("penumbra-audio-file-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait(f: &SoundFile) -> Status {
    let t = Instant::now();
    loop {
        let s = f.status();
        if s.ready || s.error.is_some() || t.elapsed() > Duration::from_secs(10) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Reads `count` frames from `from`, waiting for a streamed file to decode them.
fn read_all(f: &mut SoundFile, from: i64, count: usize, looping: bool) -> (Vec<f32>, Vec<Frame>) {
    read_cued(f, from, count, looping, 0)
}

fn read_cued(f: &mut SoundFile, from: i64, count: usize, looping: bool, cue: i64) -> (Vec<f32>, Vec<Frame>) {
    let (mut out, mut found) = (Vec::new(), Vec::new());
    let t = Instant::now();
    loop {
        f.read(from, count, looping, cue, &mut out, &mut found);
        if !found.contains(&Frame::Missing) || t.elapsed() > Duration::from_secs(10) {
            return (out, found);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn sine(rate: u32, channels: u16, seconds: f64, hz: f64, amp: f64) -> Vec<f64> {
    let n = (seconds * f64::from(rate)) as usize;
    let mut v = Vec::with_capacity(n * channels as usize);
    for i in 0..n {
        for c in 0..channels {
            let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(rate) + f64::from(c);
            v.push(amp * phase.sin());
        }
    }
    v
}

/// Zero crossings upward per second over `x` (one channel).
fn frequency(x: &[f32], rate: f64) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] <= 0.0 && w[1] > 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    if ups.len() < 2 {
        return 0.0;
    }
    (ups.len() - 1) as f64 / ((ups[ups.len() - 1] - ups[0]) / rate)
}

/// Every WAV sample format, mono and stereo, decodes to what was written, within its
/// quantisation, and the info says so.
#[test]
fn wav_formats_decode_to_their_samples() {
    let dir = temp_dir("formats");
    for (format, tol) in [
        (WavFormat::Pcm16, 2.0 / 32767.0),
        (WavFormat::Pcm24, 2.0 / 8_388_607.0),
        (WavFormat::Pcm32, 1e-7),
        (WavFormat::Float32, 1e-7),
    ] {
        for channels in [1u16, 2] {
            let samples = sine(44_100, channels, 0.3, 440.0, 0.8);
            let path = dir.join(format!("{format:?}_{channels}.wav"));
            std::fs::write(&path, wav_bytes(44_100, channels, format, &samples)).unwrap();
            let mut f = SoundFile::open(&path, None);
            let s = wait(&f);
            let info = s.info.clone().unwrap();
            assert!(s.ready && s.error.is_none(), "{s:?}");
            assert_eq!(
                (info.rate, info.channels, info.frames),
                (44_100, usize::from(channels), Some(13_230))
            );
            assert!(info.in_memory && info.exact);
            let frames = samples.len() / usize::from(channels);
            let (out, found) = read_all(&mut f, -5, frames + 10, false);
            assert!(found[..5].iter().all(|f| *f == Frame::Outside));
            assert!(found[5..5 + frames].iter().all(|f| *f == Frame::Data));
            assert!(found[5 + frames..].iter().all(|f| *f == Frame::Outside));
            let ch = usize::from(channels);
            let worst = out[5 * ch..(5 + frames) * ch]
                .iter()
                .zip(&samples)
                .map(|(a, b)| (f64::from(*a) - b).abs())
                .fold(0.0, f64::max);
            assert!(worst <= tol * 1.01, "{format:?} {channels}: off by {worst}");
        }
    }
}

/// FLAC, MP3, Ogg Vorbis, AAC in M4A and in an MP4 with a video track: a 1 kHz tone at half
/// scale, 0.5 s long.
#[test]
fn compressed_formats_decode_to_the_tone() {
    for (name, codec, rate) in [
        ("tone1k_44k.flac", "flac", 44_100),
        ("tone1k_48k.mp3", "mp3", 48_000),
        ("tone1k_44k.ogg", "vorbis", 44_100),
        ("tone1k_48k.m4a", "aac", 48_000),
        ("bars_tone.mp4", "aac", 48_000),
    ] {
        let mut f = SoundFile::open(&fixture(name), None);
        let s = wait(&f);
        assert!(s.ready && s.error.is_none(), "{name}: {s:?}");
        let info = s.info.unwrap();
        assert_eq!((info.codec.as_str(), info.rate, info.channels), (codec, rate, 1), "{name}");
        let frames = info.frames.unwrap() as usize;
        let seconds = frames as f64 / f64::from(rate);
        eprintln!("{name}: {frames} frames, {seconds:.4} s");
        assert!((0.48..0.56).contains(&seconds), "{name}: {seconds} s");
        let (out, _) = read_all(&mut f, 0, frames, false);
        let mid = &out[frames / 4..frames * 3 / 4];
        let hz = frequency(mid, f64::from(rate));
        let peak = mid.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        eprintln!("{name}: {hz:.2} Hz, peak {peak:.4}");
        assert!((hz - 1000.0).abs() < 0.5, "{name}: {hz} Hz");
        assert!((peak - 0.5).abs() < 0.03, "{name}: peak {peak}");
        // The tone starts at frame 0 rising, so an upward crossing falls on whole periods: an
        // encoder's priming left in (1024 frames for AAC) would show as a third of a period.
        let period = f64::from(rate) / 1000.0;
        let at = (2000..frames - 1).find(|&i| out[i] <= 0.0 && out[i + 1] > 0.0).unwrap();
        let x = at as f64 + f64::from(-out[at] / (out[at + 1] - out[at]));
        let off = (x / period - (x / period).round()) * period;
        eprintln!("{name}: crossing {off:+.3} frames off the period");
        assert!(off.abs() < 0.5, "{name}: the tone is {off} frames late");
    }
}

/// A streamed file reads the same as the whole one, frame for frame, block after block.
#[test]
fn a_streamed_file_reads_as_the_whole() {
    let dir = temp_dir("stream");
    let samples = sine(48_000, 2, 6.0, 330.0, 0.5);
    let path = dir.join("long.wav");
    std::fs::write(&path, wav_bytes(48_000, 2, WavFormat::Float32, &samples)).unwrap();
    let mut whole = SoundFile::open(&path, None);
    assert!(wait(&whole).info.unwrap().in_memory);
    let mut streamed = SoundFile::open_with(&path, 0, None);
    let s = wait(&streamed);
    assert!(!s.info.as_ref().unwrap().in_memory, "{s:?}");
    let (mut a, mut fa) = (Vec::new(), Vec::new());
    let mut missing = 0;
    for block in 0..(6 * 60) {
        let from = block * 800;
        whole.read(from, 800, false, 0, &mut a, &mut fa);
        let (b, fb) = read_all(&mut streamed, from, 800, false);
        missing += usize::from(fb.contains(&Frame::Missing));
        assert_eq!(a, b, "block {block}");
        assert_eq!(fa, fb, "block {block}");
        // At about real time: 800 frames are 16.7 ms; the decoder stays ahead.
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(missing, 0);
    // The cue copy was ready before the first read, so the start never missed.
    assert_eq!(streamed.status().seeks, 0, "{:?}", streamed.status());
}

/// A looping stream decodes on through the end: frame `length + k` is frame `k`, and a seek
/// far ahead lands in the right pass.
#[test]
fn a_streamed_loop_wraps_and_seeks() {
    let dir = temp_dir("wrap");
    // A ramp, so every frame is known by its value.
    let n = 48_000usize;
    let samples: Vec<f64> = (0..n).map(|i| i as f64 / n as f64).collect();
    let path = dir.join("ramp.wav");
    std::fs::write(&path, wav_bytes(48_000, 1, WavFormat::Float32, &samples)).unwrap();
    let mut f = SoundFile::open_with(&path, 0, None);
    wait(&f);
    // Read up to the end and on across it in blocks, as a player does.
    let mut from = 40_000i64;
    let mut seen = Vec::new();
    while from < 60_000 {
        let (b, found) = read_all(&mut f, from, 1000, true);
        assert!(found.iter().all(|x| *x == Frame::Data));
        seen.extend(b);
        from += 1000;
    }
    for (i, v) in seen.iter().enumerate() {
        let frame = (40_000 + i) % n;
        assert_eq!(*v, samples[frame] as f32, "frame {}", 40_000 + i);
    }
    // Pass 50, frame 12345.
    let (b, found) = read_all(&mut f, 50 * n as i64 + 12_345, 10, true);
    assert!(found.iter().all(|x| *x == Frame::Data));
    assert_eq!(b[0], samples[12_345] as f32);
    // Without loop the end is the end.
    let (_, found) = read_all(&mut f, n as i64 - 5, 10, false);
    assert_eq!(found[..5], [Frame::Data; 5]);
    assert_eq!(found[5..], [Frame::Outside; 5]);
    assert!(f.status().seeks >= 1);
}

/// A seek in a long streamed file misses for a moment, then reads right; the cue point reads at
/// once.
#[test]
fn a_seek_in_a_stream_misses_briefly_and_the_cue_never() {
    let dir = temp_dir("seek");
    let n = 20 * 48_000usize;
    let samples: Vec<f64> = (0..n).map(|i| (i % 48_000) as f64 / 48_000.0 - 0.5).collect();
    let path = dir.join("seek.wav");
    std::fs::write(&path, wav_bytes(48_000, 1, WavFormat::Float32, &samples)).unwrap();
    let mut f = SoundFile::open_with(&path, 0, None);
    wait(&f);
    let cue = 3 * 48_000 + 100;
    // Tell it the cue, then let the cue copy settle.
    let (mut out, mut found) = (Vec::new(), Vec::new());
    f.read(0, 10, false, cue, &mut out, &mut found);
    std::thread::sleep(Duration::from_millis(300));
    let _ = read_cued(&mut f, 0, 10, false, cue);
    // Far ahead: right after a moment.
    let (b, _) = read_cued(&mut f, 15 * 48_000, 10, false, cue);
    assert_eq!(b[0], samples[15 * 48_000] as f32);
    // The cue copy answers at once, wherever the window is.
    f.read(cue, 480, false, cue, &mut out, &mut found);
    assert!(found.iter().all(|x| *x == Frame::Data), "the cue missed");
    assert_eq!(out[0], samples[cue as usize] as f32);
}

/// A missing file, an empty one, text, a WAV cut short and a header with absurd numbers: an
/// error or a shorter sound, quickly, no crash.
#[test]
fn bad_files_fail_softly() {
    let dir = temp_dir("bad");
    let good = wav_bytes(48_000, 1, WavFormat::Pcm16, &sine(48_000, 1, 0.2, 440.0, 0.5));
    let mut zero_channels = good.clone();
    zero_channels[22..24].copy_from_slice(&0u16.to_le_bytes());
    let mut huge_rate = good.clone();
    huge_rate[24..28].copy_from_slice(&4_000_000_000u32.to_le_bytes());
    let mut tiny_rate = good.clone();
    tiny_rate[24..28].copy_from_slice(&10u32.to_le_bytes());
    let mut many_channels = good.clone();
    many_channels[22..24].copy_from_slice(&1000u16.to_le_bytes());
    let mut huge_data = good.clone();
    huge_data[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty.wav", Vec::new()),
        ("text.wav", b"this is not a sound file at all".to_vec()),
        ("cut.wav", good[..good.len() / 2 + 1].to_vec()),
        ("header_only.wav", good[..44].to_vec()),
        ("zero_channels.wav", zero_channels),
        ("huge_rate.wav", huge_rate),
        ("tiny_rate.wav", tiny_rate),
        ("many_channels.wav", many_channels),
        ("huge_data.wav", huge_data),
        ("wrong_extension.mp3", good.clone()),
    ];
    for (name, bytes) in cases {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let mut f = SoundFile::open(&path, None);
        let s = wait(&f);
        eprintln!("{name}: {:?} {:?}", s.error, s.info);
        assert!(s.ready || s.error.is_some(), "{name}: neither ready nor failed");
        let (mut out, mut found) = (Vec::new(), Vec::new());
        f.read(0, 4800, true, 0, &mut out, &mut found);
        assert!(out.iter().all(|v| v.is_finite()));
    }
    let f = SoundFile::open(&dir.join("not_there.wav"), None);
    let s = wait(&f);
    assert!(s.error.as_deref().unwrap_or_default().contains("cannot open"), "{s:?}");
    // A WAV under another extension still plays: the content decides.
    let f = SoundFile::open(&dir.join("wrong_extension.mp3"), None);
    assert!(wait(&f).ready);
    // A WAV cut in half plays the half that is there.
    let f = SoundFile::open(&dir.join("cut.wav"), None);
    let s = wait(&f);
    if let Some(i) = s.info {
        assert!(i.frames.unwrap_or(0) <= 4801, "{i:?}");
    }
}

/// Bits packed from the least significant on, as Vorbis writes them.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit: u32,
}

impl BitWriter {
    fn put(&mut self, value: u64, n: u32) {
        for i in 0..n {
            if self.bit == 0 {
                self.bytes.push(0);
            }
            let last = self.bytes.last_mut().unwrap();
            *last |= (((value >> i) & 1) as u8) << self.bit;
            self.bit = (self.bit + 1) % 8;
        }
    }
}

/// One codebook, length ordered (a few bits for any number of entries), with a lookup table
/// of `lookup` type.
fn codebook(w: &mut BitWriter, dimensions: u64, entries: u64, lookup: u64) {
    w.put(0x56_4342, 24);
    w.put(dimensions, 16);
    w.put(entries, 24);
    w.put(1, 1);
    w.put(0, 5);
    let bits = 64 - entries.leading_zeros();
    w.put(entries, bits);
    w.put(lookup, 4);
    if lookup != 0 {
        w.put(0, 64);
        w.put(0, 4);
        w.put(0, 1);
        let values = if lookup == 1 {
            super::file::lookup1_values(entries as u32, dimensions as u16)
        } else {
            entries * dimensions
        };
        for _ in 0..values.min(100_000) {
            w.put(0, 1);
        }
    }
}

/// The codebook walk refuses what would make symphonia reserve terabytes (2^24 entries of
/// 65535 dimensions take 33 bits to declare), and passes what real encoders write.
#[test]
fn vorbis_codebooks_are_held_to_a_size() {
    use super::file::vorbis_codebooks_fit;
    let mut w = BitWriter::default();
    w.put(0, 8);
    codebook(&mut w, 65_535, 0xFF_FFFF, 1);
    let e = vorbis_codebooks_fit(&w.bytes).unwrap_err();
    assert!(e.contains("entries"), "{e}");
    let mut w = BitWriter::default();
    w.put(0, 8);
    codebook(&mut w, 4096, 4096, 1);
    let e = vorbis_codebooks_fit(&w.bytes).unwrap_err();
    assert!(e.contains("lookup"), "{e}");
    // Many middling books add up.
    let mut w = BitWriter::default();
    w.put(255, 8);
    for _ in 0..256 {
        codebook(&mut w, 1, 60_000, 0);
    }
    assert!(vorbis_codebooks_fit(&w.bytes).is_err());
    // Small books pass; a cut header fails.
    let mut w = BitWriter::default();
    w.put(1, 8);
    codebook(&mut w, 2, 81, 1);
    codebook(&mut w, 4, 256, 2);
    assert_eq!(vorbis_codebooks_fit(&w.bytes), Ok(()));
    assert!(vorbis_codebooks_fit(&w.bytes[..w.bytes.len() / 2]).is_err());
}

#[test]
fn lookup1_values_is_the_exact_root() {
    use super::file::lookup1_values;
    for (entries, dims) in
        [(81u32, 4u16), (80, 4), (82, 4), (1, 7), (0xFF_FFFF, 1), (0xFF_FFFF, 65_535), (625, 4), (0, 3)]
    {
        let v = lookup1_values(entries, dims);
        let pow = |x: u64| (0..dims).try_fold(1u64, |a, _| a.checked_mul(x));
        assert!(pow(v).is_some_and(|p| p <= u64::from(entries)) || entries == 0, "{entries} {dims}: {v}");
        assert!(pow(v + 1).is_none_or(|p| p > u64::from(entries)), "{entries} {dims}: {v}");
    }
}

/// An Ogg Vorbis file whose setup declares a huge codebook, and one whose comment claims 4 GB,
/// are refused before symphonia reads them; Ogg Opus is refused.
#[test]
fn hostile_ogg_headers_are_refused() {
    let dir = temp_dir("ogg");
    let ogg = std::fs::read(fixture("tone1k_44k.ogg")).unwrap();
    // The fixture's three header packets, by its lacing (first page: ident; second: comment
    // and setup).
    let page = |at: usize| {
        let n = ogg[at + 26] as usize;
        let lacing = ogg[at + 27..at + 27 + n].to_vec();
        let body = at + 27 + n;
        let len: usize = lacing.iter().map(|&l| l as usize).sum();
        (lacing, body, body + len)
    };
    let (_, b0, e0) = page(0);
    let ident = ogg[b0..e0].to_vec();
    let make = |packets: &[Vec<u8>]| {
        // One page per packet, no CRC (the check runs before symphonia, which would verify it).
        let mut out = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            let mut lacing = vec![255u8; p.len() / 255];
            lacing.push((p.len() % 255) as u8);
            out.extend_from_slice(b"OggS\0");
            out.push(if i == 0 { 2 } else { 0 });
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&7u32.to_le_bytes());
            out.extend_from_slice(&(i as u32).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.push(lacing.len() as u8);
            out.extend_from_slice(&lacing);
            out.extend_from_slice(p);
        }
        out
    };
    let comment = |vendor: u32| {
        let mut c = b"\x03vorbis".to_vec();
        c.extend_from_slice(&vendor.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.push(1);
        c
    };
    let mut w = BitWriter::default();
    w.put(0, 8);
    codebook(&mut w, 65_535, 0xFF_FFFF, 1);
    let mut setup = b"\x05vorbis".to_vec();
    setup.extend_from_slice(&w.bytes);
    let cases = [
        ("codebook.ogg", make(&[ident.clone(), comment(0), setup]), "codebooks"),
        ("comment.ogg", make(&[ident.clone(), comment(0xFFFF_FFF0)]), "comment"),
        ("opus.ogg", make(&[b"OpusHead\x01\x02\0\0\x80\xbb\0\0\0\0\0".to_vec()]), "Opus"),
    ];
    for (name, bytes, says) in cases {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let f = SoundFile::open(&path, None);
        let e = wait(&f).error.unwrap_or_default();
        assert!(e.contains(says), "{name}: {e}");
    }
    // The fixture (libvorbis at its lowest quality) passes.
    assert!(wait(&SoundFile::open(&fixture("tone1k_44k.ogg"), None)).ready);
}

/// MPEG-4 descriptors: lengths inside their parents pass, a length past the end fails.
#[test]
fn mp4_descriptors_are_held_to_their_box() {
    use super::file::descriptors_fit;
    // ES (3) holding a decoder config (4) holding specific info (5) of 2 bytes.
    let ok = [3u8, 22, 0, 1, 0, 4, 17, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 2, 0x12, 0x10];
    assert!(descriptors_fit(&ok, 0));
    assert!(!descriptors_fit(&ok[..23], 0), "cut short");
    let mut bad = ok;
    bad[21] = 0x7F;
    assert!(!descriptors_fit(&bad, 0));
    assert!(!descriptors_fit(&[5, 0xFF, 0xFF, 0xFF, 0x7F], 0));
}
