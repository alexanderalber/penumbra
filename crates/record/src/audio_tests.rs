//! Sound in recordings: where blocks go, the PCM bytes, both codecs read back, a threaded
//! recording with dropped frames, and a loop of a recording with sound.

use std::path::Path;
use std::time::{Duration, Instant};

use filmcraft_isobmff as iso;
use penumbra_core::NodeId;
use penumbra_core::scratch::ScratchDir;

use crate::audio::{self, AudioBlock, AudioCodec, Placement, Placer};
use crate::looping::{Jobs, Method, Spec};
use crate::{Bytes, Codec, Fps, Frame, H264Preset, Hub, Layout, Settings, Sink};

const RATE: u32 = 48_000;
/// Samples per frame at 30 fps.
const PER_FRAME: usize = 1600;

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("record-audio-{name}")).unwrap()
}

fn settings(path: &Path, codec: Codec) -> Settings {
    Settings {
        path: path.to_path_buf(),
        codec,
        width: 32,
        height: 32,
        fps: Fps { num: 30, den: 1 },
        threads: 2,
        queue: 4,
        h264: H264Preset::Fast,
    }
}

fn grey(layout: &Layout, y: u8) -> Vec<u8> {
    let mut b = vec![0u8; layout.bytes()];
    let [oy, ou, ..] = layout.offsets();
    if layout.sample_bytes() == 1 {
        b[oy..ou].fill(y);
        b[ou..].fill(128);
    } else {
        for (i, s) in b.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let v = if i * 2 < ou { u16::from(y) << 2 } else { 512 };
            s.copy_from_slice(&v.to_le_bytes());
        }
    }
    b
}

/// A 440 Hz tone at -12 dBFS on the left and 660 Hz on the right, samples `from..from + n` of
/// the show.
fn tone(from: i64, n: usize) -> Vec<Vec<f32>> {
    let a = 0.25;
    let at = |f: f64| {
        (0..n)
            .map(|k| {
                let t = (from + k as i64) as f64 / f64::from(RATE);
                (a * (std::f64::consts::TAU * f * t).sin()) as f32
            })
            .collect::<Vec<f32>>()
    };
    vec![at(440.0), at(660.0)]
}

fn block(at: i64, n: usize) -> AudioBlock {
    AudioBlock { at, rate: RATE, channels: tone(at, n) }
}

#[test]
fn blocks_are_placed_by_their_sample_continued_within_the_tolerance_and_jumps_leave_gaps() {
    let tol = (audio::TOLERANCE_SECONDS * f64::from(RATE)) as i64;
    let mut p = Placer::default();
    // Wholly before the file's start: nothing placed, and the next block is placed exactly.
    assert_eq!(p.place(-2000, 800, RATE), Placement { pad: 0, skip: 800 });
    assert_eq!(p.written, 0);
    // The first block reaching in is cut where the file starts.
    assert_eq!(p.place(-144, 800, RATE), Placement { pad: 0, skip: 144 });
    assert_eq!(p.written, 656);
    // Continuing blocks go on, also a little early or late (another clock, a seek).
    assert_eq!(p.place(656, 800, RATE), Placement { pad: 0, skip: 0 });
    assert_eq!(p.place(1456 + tol, 800, RATE), Placement { pad: 0, skip: 0 });
    assert_eq!(p.place(2256 - tol, 800, RATE), Placement { pad: 0, skip: 0 });
    assert_eq!(p.written, 656 + 3 * 800);
    // Further ahead: a gap of silence, so the sound stays at its time.
    let w = p.written as i64;
    assert_eq!(p.place(w + 48_000, 800, RATE), Placement { pad: 48_000, skip: 0 });
    // Further back: the part already written over is cut, a block wholly behind is dropped.
    let w = p.written as i64;
    assert_eq!(p.place(w - 3000, 4000, RATE), Placement { pad: 0, skip: 3000 });
    let w = p.written;
    assert_eq!(p.place(w as i64 - 5000, 800, RATE), Placement { pad: 0, skip: 800 });
    assert_eq!(p.written, w);
    // Absurdly far ahead: continues instead of writing hours of silence.
    let w = p.written;
    let far = (audio::MAX_GAP_SECONDS * f64::from(RATE)) as i64 * 2;
    assert_eq!(p.place(w as i64 + far, 800, RATE), Placement { pad: 0, skip: 0 });
    assert_eq!(p.written, w + 800);
    // A first block that starts late is placed late: the file starts with silence.
    let mut p = Placer::default();
    assert_eq!(p.place(100, 800, RATE), Placement { pad: 100, skip: 0 });
}

#[test]
fn pcm24_interleaves_clips_and_silences_non_numbers() {
    let left = [0.0, 1.0, -1.0, 2.0];
    let right = [0.5, -0.5, f32::NAN, f32::NEG_INFINITY];
    let b = audio::pcm24(&[&left, &right], 4);
    assert_eq!(b.len(), 4 * 2 * 3);
    let s = |i: usize| i32::from_le_bytes([0, b[i * 3], b[i * 3 + 1], b[i * 3 + 2]]) >> 8;
    assert_eq!([s(0), s(1)], [0, 4_194_304]);
    assert_eq!([s(2), s(3)], [8_388_607, -4_194_304]);
    assert_eq!([s(4), s(5)], [-8_388_607, 0]);
    assert_eq!([s(6), s(7)], [8_388_607, 0]);
    // A channel shorter than asked plays silence there.
    let b = audio::pcm24(&[&left[..1], &right], 2);
    assert_eq!(&b[6..9], &[0, 0, 0]);
}

/// Writes `frames` frames with one block of sound per frame through a [`Sink`].
fn sink_with_sound(path: &Path, codec: Codec, frames: i64) -> std::path::PathBuf {
    let s = settings(path, codec);
    let mut sink = Sink::create(&s, |_| {}).unwrap();
    let layout = sink.layout();
    for k in 0..frames {
        sink.push(&grey(&layout, 100)).unwrap();
        sink.write_audio(&block(k * PER_FRAME as i64, PER_FRAME));
    }
    let path = sink.path().to_path_buf();
    let status = sink.audio_status().unwrap();
    assert_eq!(status.error, None);
    assert_eq!(status.samples, frames as u64 * PER_FRAME as u64);
    sink.finish().unwrap();
    path
}

#[test]
fn a_mov_gets_24_bit_pcm_sample_for_sample_beside_the_frames() {
    let dir = scratch("pcm");
    let path = sink_with_sound(&dir.join("tone.mov"), Codec::Prores422, 30);
    let bytes = std::fs::read(&path).unwrap();
    let movie = iso::open(bytes.as_slice()).unwrap();
    let a = movie.track_of_kind(iso::TrackKind::Audio).unwrap();
    let track = &movie.tracks[a];
    let Some(iso::CodecConfig::Pcm(p)) = track.codec() else { panic!("{:?}", track.codec()) };
    assert_eq!((p.bits, p.channels, p.big_endian, p.float), (24, 2, false, false));
    assert_eq!(track.timescale, RATE);
    // One chunk per frame's block, between the pictures.
    assert!(track.samples.len() >= 30, "{} chunks", track.samples.len());
    let v = movie.track_of_kind(iso::TrackKind::Video).unwrap();
    assert!(movie.tracks[v].samples[1].offset > track.samples[0].offset);
    let sound = audio::read(&path).unwrap().unwrap();
    assert_eq!((sound.codec, sound.rate, sound.channels.len()), (AudioCodec::Pcm24, RATE, 2));
    assert_eq!(sound.len(), 30 * PER_FRAME, "one second of sound for one second of picture");
    let want = tone(0, 30 * PER_FRAME);
    for (got, want) in sound.channels.iter().zip(&want) {
        let worst = got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2.0 / 8_388_607.0, "{worst}");
    }
}

/// Signal to noise of `got` against `want`, dB.
fn snr(got: &[f32], want: &[f32]) -> f64 {
    let (mut s, mut n) = (0.0, 0.0);
    for (g, w) in got.iter().zip(want) {
        s += f64::from(*w).powi(2);
        n += f64::from(g - w).powi(2);
    }
    10.0 * (s / n.max(1e-30)).log10()
}

#[test]
fn an_mp4_gets_aac_starting_on_the_first_sample_and_lasting_as_the_picture() {
    let dir = scratch("aac");
    let path = sink_with_sound(&dir.join("tone.mp4"), Codec::H264, 30);
    let bytes = std::fs::read(&path).unwrap();
    let movie = iso::open(bytes.as_slice()).unwrap();
    let a = movie.track_of_kind(iso::TrackKind::Audio).unwrap();
    let track = &movie.tracks[a];
    assert!(matches!(track.codec(), Some(iso::CodecConfig::Aac(_))), "{:?}", track.codec());
    assert_eq!(track.edits.first().map(|e| e.media_time), Some(1024), "the encoder delay is skipped");
    let sound = audio::read(&path).unwrap().unwrap();
    assert_eq!((sound.codec, sound.rate, sound.channels.len()), (AudioCodec::Aac, RATE, 2));
    // AAC comes in frames of 1024: the track ends within one frame after the picture.
    let n = 30 * PER_FRAME;
    assert!((n..n + 1024).contains(&sound.len()), "{} samples", sound.len());
    let want = tone(0, n);
    for (c, (got, want)) in sound.channels.iter().zip(&want).enumerate() {
        // Skip the first and last 1024: the encoder's start and the silence after the end.
        let q = snr(&got[1024..n - 1024], &want[1024..n - 1024]);
        assert!(q > 30.0, "channel {c}: {q:.1} dB");
    }
}

#[test]
fn sound_at_a_rate_aac_cannot_take_says_why_and_the_picture_goes_on() {
    let dir = scratch("rate");
    let s = settings(&dir.join("odd.mp4"), Codec::H264);
    let mut sink = Sink::create(&s, |_| {}).unwrap();
    let layout = sink.layout();
    for _ in 0..3 {
        sink.push(&grey(&layout, 100)).unwrap();
        sink.write_audio(&AudioBlock { at: 0, rate: 30_000, channels: vec![vec![0.1; 1000]; 2] });
    }
    let status = sink.audio_status().unwrap();
    assert!(status.error.as_deref().is_some_and(|e| e.contains("30000 Hz")), "{status:?}");
    let path = sink.path().to_path_buf();
    assert_eq!(sink.finish().unwrap(), 3);
    assert_eq!(audio::read(&path).unwrap(), None);
    // In a MOV any rate goes; a block at another rate than the first is left out.
    let s = settings(&dir.join("odd.mov"), Codec::Prores422);
    let mut sink = Sink::create(&s, |_| {}).unwrap();
    sink.push(&grey(&sink.layout(), 100)).unwrap();
    sink.write_audio(&AudioBlock { at: 0, rate: 30_000, channels: vec![vec![0.1; 1000]; 2] });
    sink.write_audio(&AudioBlock { at: 1000, rate: 48_000, channels: vec![vec![0.1; 1000]; 2] });
    let status = sink.audio_status().unwrap();
    assert_eq!((status.rate, status.samples, status.refused), (30_000, 1000, 1));
    let path = sink.path().to_path_buf();
    sink.finish().unwrap();
    assert_eq!(audio::read(&path).unwrap().unwrap().rate, 30_000);
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

/// Frames the queue had no room for are dropped, the sound is not: it lasts as long as the
/// picture and stays in place.
#[test]
fn a_recording_keeps_every_sample_when_frames_are_dropped() {
    let dir = scratch("dropped");
    let hub = Hub::new();
    let node = NodeId(1);
    hub.attach(node, "/rec");
    let rec =
        hub.start(node, Settings { queue: 1, ..settings(&dir.join("drop.mov"), Codec::Prores4444) }).unwrap();
    rec.sound(1, 0, 0);
    let layout = rec.layout();
    let frames = 60;
    let mut dropped = 0;
    for k in 0..frames {
        rec.due(k);
        // Every third frame is not even tried; the others only when the queue has room.
        let frame = Frame { index: k, planes: Box::new(Bytes(grey(&layout, 120))) };
        if k % 3 == 1 || rec.push(frame).is_err() {
            rec.dropped(1);
            dropped += 1;
        }
        rec.push_audio(block(k * PER_FRAME as i64, PER_FRAME));
    }
    drop(rec);
    let s = wait_done(&hub, node);
    assert_eq!(s.error, None);
    assert!(dropped >= 20 && s.dropped == dropped, "{s:?}");
    let a = s.audio.unwrap();
    assert_eq!((a.samples, a.gap_samples, a.sources), (frames as u64 * PER_FRAME as u64, 0, 1));
    let sound = audio::read(Path::new(&s.file)).unwrap().unwrap();
    assert_eq!(sound.len(), frames as usize * PER_FRAME);
    let want = tone(0, sound.len());
    assert!(snr(&sound.channels[0], &want[0]) > 100.0);
}

/// A recording with sound is read and looped as before; the loop has no sound.
#[test]
fn a_loop_of_a_recording_with_sound_is_silent() {
    let dir = scratch("loop");
    let path = sink_with_sound(&dir.join("take.mp4"), Codec::H264, 20);
    let d = penumbra_video::open(&path, &penumbra_video::Limits::default()).unwrap();
    assert_eq!(d.info().frames, 20);
    let jobs = Jobs::new();
    let job = jobs.start(&path, Spec::new(Method::Pingpong)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        let s = job.status();
        if s.done {
            break s;
        }
        assert!(Instant::now() < deadline, "the loop did not finish: {s:?}");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status.error, None);
    assert!(status.frames_out > 0);
    assert_eq!(audio::read(Path::new(&status.out)).unwrap(), None);
}

/// ffprobe and ffmpeg (on PATH, for checking only) read both files: two streams of the same
/// length, and nothing to warn about when decoding them.
#[test]
#[ignore]
fn ffmpeg_reads_both_streams_of_mp4_and_mov() {
    let dir = scratch("ffprobe");
    for (file, codec, audio) in [("t.mp4", Codec::H264, "aac"), ("t.mov", Codec::Prores422Hq, "pcm_s24le")] {
        let path = sink_with_sound(&dir.join(file), codec, 90);
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,sample_rate,channels,duration",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with("3.000000"), "{text}");
        assert!(lines[1].starts_with(&format!("{audio},48000,2,")), "{text}");
        // AAC comes in frames of 1024 samples: up to one frame beyond the picture.
        let seconds: f64 = lines[1].rsplit(',').next().unwrap().parse().unwrap();
        assert!((3.0..=3.0 + 1024.0 / 48_000.0).contains(&seconds), "{text}");
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "warning", "-i"])
            .arg(&path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        // FilmCraft's writer gives a stereo MOV no channel layout box (`chan`), so ffmpeg says it
        // guessed stereo; anything else is a warning.
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let warnings: Vec<&str> =
            stderr.lines().filter(|l| !l.contains("Guessed Channel Layout: stereo")).collect();
        assert!(warnings.is_empty(), "{file}: {stderr}");
    }
}
