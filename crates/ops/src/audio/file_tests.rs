//! `audio.file`: plays WAV sample for sample, converts rates, loops without a seam, restarts,
//! changes speed as tape does, follows the show clock as `tex.video_file` does, and stays
//! silent with a warning on bad files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use penumbra_audio::file::{WavFormat, wav_bytes};
use penumbra_core::{Chans, Rate};
use penumbra_graph::{Data, DataKind, Op, Port};

use super::file::FILE;
use super::instrument::Presses;
use super::tests::{F, create, set};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("penumbra-audio-file-node-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_wav(dir: &Path, name: &str, rate: u32, channels: u16, samples: &[f64]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, wav_bytes(rate, channels, WavFormat::Float32, samples)).unwrap();
    path.to_string_lossy().into_owned()
}

fn sine(rate: u32, seconds: f64, hz: f64, amp: f64) -> Vec<f64> {
    let n = (seconds * f64::from(rate)).round() as usize;
    (0..n).map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin()).collect()
}

/// Exactly representable as f32, different at every sample: `i` back from the samples.
fn ramp(n: usize) -> Vec<f64> {
    (0..n).map(|i| ((i % 65_536) as f64 - 32_768.0) / 32_768.0).collect()
}

fn player(file: &str) -> Vec<Op> {
    vec![create("/p", FILE), set("/p.file", file)]
}

/// Cooks until the file is open; the block of that cook.
fn first(f: &mut F) -> Chans {
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    let t = Instant::now();
    loop {
        f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
        if let Some(d) = f.cooker.output(Port { node: id, output: 0 }) {
            return d.get::<Chans>().unwrap().clone();
        }
        assert!(t.elapsed() < Duration::from_secs(10), "the file never opened");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The next frame's block (one channel per entry).
fn next(f: &mut F, frames: u64) -> Vec<Vec<f64>> {
    f.clock.advance(frames);
    f.cook("/p").unwrap().channels().iter().map(|c| c.samples.clone()).collect()
}

fn info(f: &F, name: &str) -> f64 {
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    let d: &Data = f.cooker.output(Port { node: id, output: 1 }).unwrap();
    assert_eq!(d.kind(), DataKind::Chan);
    let c = d.get::<Chans>().unwrap();
    c.channels().iter().find(|c| c.name.as_str() == name).unwrap().samples[0]
}

/// Cooks the next frame, whatever comes of it.
fn cook_any(f: &mut F) {
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    f.clock.advance(1);
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
}

fn warning(f: &F) -> Option<String> {
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    f.cooker.status(id).and_then(|s| s.warning)
}

/// Upward zero crossings per second.
fn frequency(x: &[f64], rate: f64) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] <= 0.0 && w[1] > 0.0)
        .map(|(i, w)| i as f64 + -w[0] / (w[1] - w[0]))
        .collect();
    (ups.len() - 1) as f64 / ((ups[ups.len() - 1] - ups[0]) / rate)
}

fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// A 48 kHz file at 48 kHz: the samples come out unchanged, from the first frame after the
/// first cook on, in two channels for a stereo file.
#[test]
fn a_wav_plays_sample_for_sample() {
    let dir = temp_dir("exact");
    let n = 48_000;
    let left = ramp(n);
    let mut both = Vec::with_capacity(2 * n);
    for (i, l) in left.iter().enumerate() {
        both.push(*l);
        both.push(-(i as f64) / n as f64);
    }
    let file = write_wav(&dir, "stereo.wav", 48_000, 2, &both);
    let mut f = F::new(&player(&file));
    let c = first(&mut f);
    // The first cook's block lies before the playhead's start: silent.
    assert_eq!(c.channels().len(), 2);
    assert_eq!(c.channels()[1].name.as_str(), "ch2");
    assert!(c.channels().iter().all(|ch| ch.samples.iter().all(|s| *s == 0.0)));
    let mut got: Vec<Vec<f64>> = vec![Vec::new(), Vec::new()];
    for _ in 0..30 {
        let b = next(&mut f, 1);
        got[0].extend(&b[0]);
        got[1].extend(&b[1]);
    }
    assert_eq!(got[0].len(), 24_000);
    for i in 0..24_000 {
        assert_eq!(got[0][i], f64::from(left[i] as f32), "left {i}");
        assert_eq!(got[1][i], f64::from(both[2 * i + 1] as f32), "right {i}");
    }
    assert!(warning(&f).is_none(), "{:?}", warning(&f));
    assert_eq!(info(&f, "rate"), 48_000.0);
    assert_eq!(info(&f, "channels"), 2.0);
    assert_eq!(info(&f, "duration"), 1.0);
    assert!((info(&f, "position") - 0.5).abs() < 1e-9);
}

/// A 44.1 kHz tone at 48 kHz: the same pitch and level, the same length in time.
#[test]
fn a_44k_file_plays_at_48k() {
    let dir = temp_dir("rate");
    let file = write_wav(&dir, "tone44.wav", 44_100, 1, &sine(44_100, 1.0, 1000.0, 0.5));
    let mut ops = player(&file);
    ops.push(set("/p.loop", false));
    let mut f = F::new(&ops);
    first(&mut f);
    let mut all = Vec::new();
    for _ in 0..80 {
        all.extend(next(&mut f, 1).remove(0));
    }
    let hz = frequency(&all[4800..43_200], 48_000.0);
    let p = peak(&all[4800..43_200]);
    assert!((hz - 1000.0).abs() < 0.01, "{hz} Hz");
    assert!((p - 0.5).abs() < 0.001, "peak {p}");
    // The interpolation rings on for half its kernel past the last sample.
    let sounding = all.iter().rposition(|s| s.abs() > 1e-3).unwrap() + 1;
    assert!((sounding as i64 - 48_000).abs() <= 36, "{sounding} samples of sound");
    assert!(all[48_100..].iter().all(|s| s.abs() < 1e-3), "silent after the end");
    assert_eq!(info(&f, "progress"), 1.0);
}

/// Loops join without a step: a whole number of waves, at the file's rate and converted.
#[test]
fn a_loop_has_no_seam() {
    let dir = temp_dir("seam");
    for (rate, name) in [(48_000u32, "loop48.wav"), (44_100, "loop44.wav")] {
        // 250 waves of 1 kHz in 0.25 s.
        let file = write_wav(&dir, name, rate, 1, &sine(rate, 0.25, 1000.0, 0.9));
        let mut f = F::new(&player(&file));
        first(&mut f);
        let mut all = Vec::new();
        for _ in 0..90 {
            all.extend(next(&mut f, 1).remove(0));
        }
        // A sine's largest step from one sample to the next.
        let most = 0.9 * (std::f64::consts::TAU * 1000.0 / 48_000.0) * 1.001;
        // From 10 ms on: a sine switched on at the file's start rings a little once converted.
        let worst = all[480..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        assert!(worst <= most, "{name}: a step of {worst} (a sine's is {most})");
        assert!(peak(&all) > 0.89 && peak(&all) < 0.905, "{name}: {}", peak(&all));
        assert_eq!(info(&f, "loops"), 6.0, "{name}: 1.5 s of a 0.25 s loop");
    }
}

/// A restart (the param's edge or a press) plays from `start` at the next block, with the old
/// sound faded out over the first 4 ms of this one.
#[test]
fn a_restart_goes_to_start() {
    let dir = temp_dir("restart");
    let n = 96_000;
    let file = write_wav(&dir, "ramp.wav", 48_000, 1, &ramp(n));
    let mut ops = player(&file);
    ops.push(set("/p.start", 0.25));
    let mut f = F::new(&ops);
    let presses = Presses::new();
    f.cooker.provide(presses.clone());
    first(&mut f);
    // Loaded: playing from start, faded in over 4 ms (inside the file).
    let b = next(&mut f, 1).remove(0);
    assert_eq!(b[400], f64::from(ramp(n)[12_000 + 400] as f32));
    for _ in 0..10 {
        next(&mut f, 1);
    }
    f.edit(&[set("/p.restart", true)]);
    let b = next(&mut f, 1).remove(0);
    // The old pass fades out; the new one has not begun yet (it starts at this cook's time).
    assert!(b[0] != 0.0 && b[300..].iter().all(|s| *s == 0.0), "fade out, then wait");
    let b = next(&mut f, 1).remove(0);
    let want: Vec<f64> = ramp(n)[12_000..12_800].iter().map(|v| f64::from(*v as f32)).collect();
    assert_eq!(b[192..], want[192..], "from start, after the 4 ms fade in");
    assert_eq!(info(&f, "restarts"), 1.0);
    // A press does the same.
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    for _ in 0..5 {
        next(&mut f, 1);
    }
    assert!(presses.request(id, 0));
    next(&mut f, 1);
    let b = next(&mut f, 1).remove(0);
    assert_eq!(b[192..], want[192..]);
    assert_eq!(info(&f, "restarts"), 2.0);
}

/// Speed 2 plays twice as fast and an octave up, as tape does: a 1 s tone lasts 0.5 s.
#[test]
fn speed_two_is_an_octave_up_and_half_as_long() {
    let dir = temp_dir("speed");
    let file = write_wav(&dir, "tone48.wav", 48_000, 1, &sine(48_000, 1.0, 1000.0, 0.5));
    let mut ops = player(&file);
    ops.push(set("/p.loop", false));
    ops.push(set("/p.speed", 2.0));
    let mut f = F::new(&ops);
    first(&mut f);
    let mut all = Vec::new();
    for _ in 0..60 {
        all.extend(next(&mut f, 1).remove(0));
    }
    let hz = frequency(&all[2400..21_600], 48_000.0);
    assert!((hz - 2000.0).abs() < 0.02, "{hz} Hz");
    // Halving the rate cuts at half the output's band: 2 kHz passes at full level.
    assert!((peak(&all[2400..21_600]) - 0.5).abs() < 0.002, "{}", peak(&all[2400..21_600]));
    let sounding = all.iter().rposition(|s| s.abs() > 1e-3).unwrap() + 1;
    assert!((sounding as i64 - 24_000).abs() <= 2, "{sounding}");
}

/// Over 60 s of show at 60 fps with skipped frames, a pause, a speed change, play off and on
/// and a gap, the playhead stays where `tex.video_file`'s would be (its formula, run alongside),
/// within a sample; and at speed 1 the sound at the end is the file's sound at the playhead.
#[test]
fn the_playhead_follows_the_show_clock_as_a_video_does() {
    let dir = temp_dir("sync");
    let n = 70 * 48_000;
    let file = write_wav(&dir, "long.wav", 48_000, 1, &ramp(n));
    let mut ops = player(&file);
    ops.push(set("/p.loop", false));
    let mut f = F::at(Rate::FPS_60, &ops);
    first(&mut f);
    // tex.video_file's playhead: start, plus each cook's show seconds times speed, one frame
    // after a gap.
    let frame = 1.0 / 60.0;
    let mut video = 0.0f64;
    let mut last = f.clock.now().seconds();
    let mut speed = 1.0;
    let mut play = true;
    let mut worst = 0.0f64;
    let mut frames = 0u64;
    let mut cooks = 0;
    while frames < 60 * 60 {
        let mut step = match cooks % 97 {
            13 => 3,
            50 => 2,
            _ => 1,
        };
        match cooks {
            600 => {
                speed = 1.5;
                f.edit(&[set("/p.speed", 1.5)]);
            }
            1200 => {
                speed = 1.0;
                f.edit(&[set("/p.speed", 1.0)]);
            }
            1800 => {
                play = false;
                f.edit(&[set("/p.play", false)]);
            }
            1900 => {
                play = true;
                f.edit(&[set("/p.play", true)]);
            }
            // A gap of a second (the node not cooked): one frame on, as the video does.
            2500 => step = 60,
            _ => {}
        }
        f.clock.advance(step);
        frames += step;
        let now = f.clock.now().seconds();
        let dt = if (0.0..=0.5).contains(&(now - last)) { now - last } else { frame };
        last = now;
        if play {
            video += dt * speed;
        }
        f.cook("/p").unwrap();
        // Paused: cooked again at the same time.
        if cooks == 1000 {
            f.cook("/p").unwrap();
        }
        cooks += 1;
        worst = worst.max((info(&f, "position") - video).abs());
    }
    eprintln!("playhead off the video's by {:.3e} s at most, at {video:.3} s", worst);
    assert!(worst < 1.0 / 48_000.0, "{worst} s");
    // At speed 1 now: the block is the file from the playhead of the previous cook.
    let before = info(&f, "position");
    f.clock.advance(1);
    let b = f.cook("/p").unwrap().channels()[0].samples.clone();
    let at = (before * 48_000.0).round() as usize;
    let want: Vec<f64> = ramp(n)[at..at + b.len()].iter().map(|v| f64::from(*v as f32)).collect();
    assert_eq!(b, want);
}

/// No file, a missing file and a broken one: a warning and silence, never an error; absurd
/// params are held in range.
#[test]
fn bad_files_and_params_play_silence_with_a_warning() {
    let dir = temp_dir("bad");
    let mut f = F::new(&[create("/p", FILE)]);
    let b = f.block("/p");
    assert!(b.iter().all(|s| *s == 0.0) && !b.is_empty());
    assert!(warning(&f).unwrap().contains("no file set"));

    let missing = dir.join("nope.wav").to_string_lossy().into_owned();
    let mut f = F::new(&player(&missing));
    let t = Instant::now();
    while warning(&f).is_none_or(|w| !w.contains("cannot open")) {
        assert!(t.elapsed() < Duration::from_secs(5));
        cook_any(&mut f);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(f.block("/p").iter().all(|s| *s == 0.0));

    let junk = dir.join("junk.wav");
    std::fs::write(&junk, b"RIFF\x10\0\0\0WAVEjunkjunk").unwrap();
    let mut f = F::new(&player(&junk.to_string_lossy()));
    let t = Instant::now();
    while warning(&f).is_none() {
        assert!(t.elapsed() < Duration::from_secs(5));
        cook_any(&mut f);
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!("junk: {:?}", warning(&f));

    let file = write_wav(&dir, "ok.wav", 48_000, 1, &sine(48_000, 0.5, 440.0, 0.5));
    let mut ops = player(&file);
    ops.extend([
        set("/p.speed", 1e300),
        set("/p.rate", -5_i64),
        set("/p.start", -1.0),
        set("/p.channels", "stereo"),
    ]);
    let mut f = F::new(&ops);
    first(&mut f);
    for _ in 0..10 {
        let b = next(&mut f, 1);
        assert_eq!(b.len(), 2, "stereo from mono");
        assert!(b.iter().flatten().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}

/// `position` bound to the channel `pos` of a `chan.constant` at `/k`.
fn scrubbed(file: &str, at: f64) -> Vec<Op> {
    let mut ops = player(file);
    ops.extend([
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", at),
        Op::Bind {
            param: penumbra_core::ParamPath::parse("/p.position").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(penumbra_core::Name::new("pos").unwrap()),
            cell: None,
            format: None,
            from_param: None,
        },
    ]);
    ops
}

/// The ramp's samples from `from` on, as the file holds them (f32).
fn ramp_from(all: &[f64], from: usize, n: usize) -> Vec<f64> {
    all[from..from + n].iter().map(|v| f64::from(*v as f32)).collect()
}

fn max_error(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |m, (x, y)| m.max((x - y).abs()))
}

/// A bound position moves the file like a tape moved by hand: standing is silent, real time
/// forwards plays the file, real time backwards plays it reversed, a throw faster than 4 times
/// real time is silent after a fade, and unbound the file plays on from there.
#[test]
fn a_bound_position_scrubs_like_tape() {
    let dir = temp_dir("scrub");
    let n = 96_000;
    let all = ramp(n);
    let file = write_wav(&dir, "ramp.wav", 48_000, 1, &all);
    let mut f = F::new(&scrubbed(&file, 0.5));
    first(&mut f);
    // Standing: silence, though the show clock runs and play is on.
    for _ in 0..3 {
        assert_eq!(peak(&next(&mut f, 1)[0]), 0.0, "standing");
    }
    assert_eq!(info(&f, "position"), 0.5);
    // Forwards at real time (800 samples per 60 fps frame): the file, after a 4 ms fade in.
    let mut pos = 24_000usize;
    for k in 0..10 {
        pos += 800;
        f.edit(&[set("/k.value0", pos as f64 / 48_000.0)]);
        let b = next(&mut f, 1).remove(0);
        let want = ramp_from(&all, pos - 800, 800);
        let skip = if k == 0 { 192 } else { 0 };
        assert!(max_error(&b[skip..], &want[skip..]) < 1e-4, "forwards, frame {k}");
    }
    // Backwards at real time: the file reversed.
    for k in 0..10 {
        pos -= 800;
        f.edit(&[set("/k.value0", pos as f64 / 48_000.0)]);
        let b = next(&mut f, 1).remove(0);
        let want: Vec<f64> = (0..800).map(|j| f64::from(all[pos + 800 - j] as f32)).collect();
        assert!(max_error(&b[2..], &want[2..]) < 1e-3, "backwards, frame {k}");
    }
    // A throw: the sound fades out over 4 ms and stays silent; no step larger than the ramp's.
    pos += 30_000;
    f.edit(&[set("/k.value0", pos as f64 / 48_000.0)]);
    let b = next(&mut f, 1).remove(0);
    assert!(b[0] != 0.0 && peak(&b[192..]) == 0.0, "fade out, then silence");
    assert!(b.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01), "no click");
    assert_eq!(info(&f, "position"), pos as f64 / 48_000.0);
    // Unbound: plays on from there at speed 1, faded in.
    f.edit(&[Op::Unbind { param: penumbra_core::ParamPath::parse("/p.position").unwrap(), component: None }]);
    next(&mut f, 1);
    let b = next(&mut f, 1).remove(0);
    assert_eq!(b, ramp_from(&all, pos + 800, 800), "on from the position");
}

/// Changing `start`, setting `position` by hand and a seek request jump at once, with the fades
/// of a restart, and play on from there; restarts are not counted.
#[test]
fn start_position_and_seeks_jump_at_once() {
    let dir = temp_dir("jumps");
    let n = 96_000;
    let all = ramp(n);
    let file = write_wav(&dir, "ramp.wav", 48_000, 1, &all);
    let mut f = F::new(&player(&file));
    let seeks = crate::seek::Seeks::new();
    f.cooker.provide(seeks.clone());
    first(&mut f);
    for _ in 0..5 {
        next(&mut f, 1);
    }
    let check = |f: &mut F, at: usize, why: &str| {
        let b = next(f, 1).remove(0);
        assert!(b[0] != 0.0 && b[300..].iter().all(|s| *s == 0.0), "{why}: fade out, then wait");
        let b = next(f, 1).remove(0);
        assert_eq!(b[192..], ramp_from(&all, at, 800)[192..], "{why}: from there after the fade in");
        let b = next(f, 1).remove(0);
        assert_eq!(b, ramp_from(&all, at + 800, 800), "{why}: plays on");
    };
    f.edit(&[set("/p.start", 0.5)]);
    check(&mut f, 24_000, "start");
    f.edit(&[set("/p.position", 1.25)]);
    check(&mut f, 60_000, "position");
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    assert!(seeks.request(id, 0.25));
    check(&mut f, 12_000, "seek");
    assert_eq!(info(&f, "restarts"), 0.0);
}

/// In show mode the playhead is kept in whole seconds; after a crash restart (a new cooker and
/// clock, the place given back before the first cook) the file plays on from there instead of
/// from `start`. Another file's place, a bound `position` and hostile places are not taken.
#[test]
fn the_playhead_comes_back_after_a_crash_restart() {
    let dir = temp_dir("place");
    let rate = 48_000u32;
    let file = write_wav(&dir, "ramp.wav", rate, 1, &ramp(10 * rate as usize));
    let mut f = F::new(&player(&file));
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    first(&mut f);
    // 3.5 s in.
    for _ in 0..210 {
        next(&mut f, 1);
    }
    let at = info(&f, "position");
    assert!((at - 3.5).abs() < 0.05, "{at}");
    let places = f.cooker.show_places();
    let (_, place) = places.iter().find(|(n, _)| *n == id).cloned().unwrap();
    let kept: serde_json::Value = serde_json::from_str(&place).unwrap();
    assert_eq!(kept["seconds"], 3.0, "whole seconds: {place}");

    let restarted = |place: &str, ops: &[Op]| {
        let mut g = F::new(ops);
        g.cooker.restore_show_place(id, place.to_string()).unwrap();
        first(&mut g);
        // A frame on, the block is the file from the kept second on.
        let block = next(&mut g, 1);
        (g, block)
    };
    let (g, block) = restarted(&place, &player(&file));
    let p = info(&g, "position");
    assert!((p - (3.0 + 2.0 / 60.0)).abs() < 0.02, "{p}");
    // Faded in over 4 ms, then the ramp from 3 s on.
    let i = 400;
    let expected = |k: usize| f64::from(ramp(10 * rate as usize)[k] as f32);
    // The ramp repeats every 65 536 samples: sample 3 s + 400 of the file.
    assert_eq!(block[0][i], expected(3 * rate as usize + i));
    // Taken, the place is the playhead again.
    assert!(g.cooker.show_places().iter().any(|(n, _)| *n == id));

    // Another file: from the start.
    let other = write_wav(&dir, "other.wav", rate, 1, &ramp(10 * rate as usize));
    let (g, _) = restarted(&place, &player(&other));
    assert!(info(&g, "position") < 0.1, "{}", info(&g, "position"));
    // Bound position: the channel decides, and no place is kept.
    let (g, _) = restarted(&place, &scrubbed(&file, 0.0));
    assert!(info(&g, "position") < 0.1, "{}", info(&g, "position"));
    assert!(!g.cooker.show_places().iter().any(|(n, _)| *n == id));
    // Hostile places.
    let mut h = F::new(&player(&file));
    first(&mut h);
    for bad in [
        r#"{"file": "x", "seconds": -1}"#.to_string(),
        r#"{"file": "x", "seconds": 1e300}"#.to_string(),
        "[nan".to_string(),
        r#"{"file": "x", "seconds": 1, "more": 2}"#.to_string(),
        format!(r#"{{"file": "{}", "seconds": 1}}"#, "x".repeat(10_000)),
    ] {
        assert!(h.cooker.restore_show_place(id, bad.clone()).is_err(), "{bad}");
    }
    next(&mut h, 1);
    assert!(info(&h, "position") < 0.1);
    // A place given back while the file plays is a jump there.
    h.cooker.restore_show_place(id, place.clone()).unwrap();
    next(&mut h, 1);
    next(&mut h, 1);
    assert!((info(&h, "position") - 3.0).abs() < 0.05, "{}", info(&h, "position"));
    let _ = std::fs::remove_dir_all(&dir);
}
