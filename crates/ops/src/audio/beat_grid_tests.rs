//! `chan.beat_grid`: the beats of the file an `audio.file` plays, found in the background, as
//! channels at its playhead; ticks exactly `offset` early; the grid as a table; errors for
//! wiring that gives no file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use penumbra_audio::file::{WavFormat, wav_bytes};
use penumbra_core::{Chans, Table};
use penumbra_graph::{Op, Port};

use super::beat_grid::{BEAT_GRID, BeatFiles};
use super::file::{FILE, SoundFiles};
use super::tests::{F, connect, create, set};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("penumbra-beat-grid-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 12 s of clicks at 120 beats per minute, every fourth one a louder, lower thump, mono.
fn clicks(dir: &Path) -> String {
    let rate = 44_100usize;
    let n = 12 * rate;
    let mut samples = vec![0.0f64; n];
    for beat in 0..24 {
        let at = beat * rate / 2;
        // The downbeat a low thump, the others a short high click.
        let (gain, decay, step, len) =
            if beat % 4 == 0 { (0.9, 800.0, 0.02, 4000) } else { (0.3, 60.0, 0.35, 400) };
        for i in 0..len.min(n - at) {
            samples[at + i] = gain * (-(i as f64) / decay).exp() * (i as f64 * step).sin();
        }
    }
    let path = dir.join("clicks.wav");
    std::fs::write(&path, wav_bytes(44_100, 1, WavFormat::Float32, &samples)).unwrap();
    path.to_string_lossy().into_owned()
}

fn fixture(ops: &[Op], cache: &Path) -> F {
    let mut f = F::new(ops);
    f.cooker.provide(SoundFiles::new());
    f.cooker.provide(BeatFiles::new(Some(cache.to_path_buf())));
    f
}

fn grid_ops(file: &str) -> Vec<Op> {
    vec![create("/p", FILE), set("/p.file", file), create("/g", BEAT_GRID), connect("/p", "/g", 0)]
}

/// Cooks `/g` and its `grid` output; the channels and the error.
fn cook(f: &mut F) -> (Option<Chans>, Option<String>) {
    let id = f.net.resolve(&"/g".parse().unwrap()).unwrap();
    let grid = Port { node: id, output: 1 };
    f.cooker.cook_showing(&f.net, f.clock.now(), &[id], &[], &[grid]);
    let out = f.cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Chans>()).cloned();
    (out, f.cooker.status(id).and_then(|s| s.error))
}

fn value(c: &Chans, name: &str) -> f64 {
    c.get(name).unwrap_or_else(|| panic!("no channel {name}")).samples[0]
}

/// Cooks the paused show until the analysis has ended.
fn analysed(f: &mut F) -> Chans {
    let t = Instant::now();
    loop {
        if let (Some(c), None) = cook(f)
            && value(&c, "analyzing") == 0.0
        {
            return c;
        }
        assert!(t.elapsed() < Duration::from_secs(120), "the analysis never ended: {:?}", cook(f));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The frames in which `tick` and `downbeat` were 1 over `frames` frames.
fn ticks(f: &mut F, frames: u64) -> (Vec<u64>, Vec<u64>) {
    let (mut beats, mut bars) = (Vec::new(), Vec::new());
    for i in 0..frames {
        f.clock.advance(1);
        let c = cook(f).0.unwrap();
        if value(&c, "tick") == 1.0 {
            beats.push(i);
        }
        if value(&c, "downbeat") == 1.0 {
            bars.push(i);
        }
    }
    (beats, bars)
}

#[test]
fn the_grid_follows_the_playhead_and_ticks_ahead() {
    let dir = temp_dir("follow");
    let file = clicks(&dir);
    let cache = dir.join("cache");
    let mut f = fixture(&grid_ops(&file), &cache);
    let c = analysed(&mut f);
    assert_eq!(value(&c, "progress"), 1.0);
    assert_eq!(value(&c, "ready"), 1.0);
    // How many beats a bar has is the network's call on these clicks; it is at least one.
    assert!(value(&c, "beats_per_bar") >= 1.0);
    assert!((value(&c, "bpm") - 120.0).abs() < 2.0, "{}", value(&c, "bpm"));

    // The table: a header and one row per click.
    let id = f.net.resolve(&"/g".parse().unwrap()).unwrap();
    let table =
        f.cooker.output(Port { node: id, output: 1 }).and_then(|d| d.get::<Table>()).cloned().unwrap();
    assert_eq!(table.rows()[0], ["time", "beat", "bar", "beat_in_bar", "downbeat"]);
    assert!((22..=25).contains(&(table.num_rows() - 1)), "{} rows", table.num_rows());
    let downs: Vec<f64> =
        table.rows().iter().skip(1).filter(|r| r[4] == "1").map(|r| r[0].parse().unwrap()).collect();
    assert!(!downs.is_empty());

    // 4 s at 60 fps: a tick every half second, a downbeat every two.
    let (beats, bars) = ticks(&mut f, 240);
    assert!((7..=9).contains(&beats.len()), "{beats:?}");
    for w in beats.windows(2) {
        assert!((w[1] - w[0]).abs_diff(30) <= 1, "{beats:?}");
    }
    // A downbeat in the frame that passes each downbeat of the table: frame i shows (i + 1) / 60 s.
    let passed: Vec<u64> = downs
        .iter()
        .filter(|t| **t > 1.0 / 60.0 && **t <= 4.0)
        .map(|t| (t * 60.0).ceil() as u64 - 1)
        .collect();
    assert_eq!(bars.len(), passed.len(), "{bars:?} vs {downs:?}");
    assert!(bars.iter().zip(&passed).all(|(b, p)| b.abs_diff(*p) <= 1), "{bars:?} vs {passed:?}");
    assert!(bars.iter().all(|b| beats.contains(b)), "{bars:?} not on {beats:?}");

    // 100 ms ahead: the same ticks, six frames earlier. The second node reads the cache.
    let mut ops = grid_ops(&file);
    ops.push(set("/g.offset", 100.0));
    let mut g = fixture(&ops, &cache);
    let started = Instant::now();
    analysed(&mut g);
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    let (early, _) = ticks(&mut g, 240);
    let shifted: Vec<u64> = beats.iter().filter(|b| **b >= 6).map(|b| b - 6).collect();
    let near = |a: u64, b: u64| a.abs_diff(b) <= 1;
    assert!(shifted.iter().all(|s| early.iter().any(|e| near(*e, *s))), "{early:?} vs {beats:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_looped_file_ticks_on_across_its_end() {
    // The clicks loop (audio.file's default): 12 s, the last click at 11.5 s, the first at 0 s.
    let dir = temp_dir("loop");
    let file = clicks(&dir);
    let mut f = fixture(&grid_ops(&file), &dir.join("cache"));
    analysed(&mut f);
    // 11 to 13 s: a tick every half second, at 12 s too, and the beats count from 0 again.
    let (mut beats, mut index, mut onsets) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..780u64 {
        f.clock.advance(1);
        let c = cook(&mut f).0.unwrap();
        if i >= 660 && value(&c, "tick") == 1.0 {
            beats.push(i);
            index.push(value(&c, "beat_index"));
        }
        if i >= 660 && value(&c, "onset") == 1.0 {
            onsets.push(i);
        }
    }
    // The envelopes run on across the end too: the click at 0 s of the next pass is an onset.
    assert!(onsets.iter().any(|o| o.abs_diff(719) <= 1), "{onsets:?}");
    assert!((3..=5).contains(&onsets.len()), "{onsets:?}");
    assert_eq!(beats.len(), 4, "{beats:?}");
    for w in beats.windows(2) {
        assert!((w[1] - w[0]).abs_diff(30) <= 1, "{beats:?}");
    }
    // Frame i shows (i + 1) / 60 s: 12 s is frame 719.
    assert!(beats.iter().any(|b| b.abs_diff(719) <= 1), "{beats:?}");
    assert_eq!(index.iter().filter(|i| **i == 0.0).count(), 1, "{index:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wiring_without_a_file_is_an_error() {
    let dir = temp_dir("wiring");
    let cache = dir.join("cache");
    // Nothing wired.
    let mut f = fixture(&[create("/g", BEAT_GRID)], &cache);
    assert!(cook(&mut f).1.unwrap().contains("wire the out of an audio.file"));
    // Not an audio.file.
    let ops = [create("/o", "audio.oscillator"), create("/g", BEAT_GRID), connect("/o", "/g", 0)];
    let mut f = fixture(&ops, &cache);
    let t = Instant::now();
    let e = loop {
        if let Some(e) = cook(&mut f).1 {
            break e;
        }
        assert!(t.elapsed() < Duration::from_secs(10));
    };
    assert!(e.contains("straight from an audio.file"), "{e}");
    // An audio.file without a file, and one whose file is no sound.
    let mut f = fixture(&grid_ops(""), &cache);
    let t = Instant::now();
    let e = loop {
        if let Some(e) = cook(&mut f).1 {
            break e;
        }
        assert!(t.elapsed() < Duration::from_secs(10));
    };
    assert!(e.contains("no file"), "{e}");
    let junk = dir.join("junk.wav");
    std::fs::write(&junk, vec![0x55u8; 5000]).unwrap();
    let mut f = fixture(&grid_ops(&junk.to_string_lossy()), &cache);
    let t = Instant::now();
    let e = loop {
        if let Some(e) = cook(&mut f).1.filter(|e| e.contains("no beats")) {
            break e;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", cook(&mut f));
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(e.contains("no beats"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn analyses_are_shared_and_stop_when_unwanted() {
    let dir = temp_dir("shared");
    let file = PathBuf::from(clicks(&dir));
    let files = BeatFiles::new(None);
    let a = files.want(&file);
    let b = files.want(&file);
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    drop((a, b));
    // Nobody holds it: the next one is new, and cancelling it stops it.
    let c = files.want(&file);
    assert!(files.cancel(&file));
    let t = Instant::now();
    while c.state().running {
        assert!(t.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(c.state().cancelled);
    // A cancelled analysis stays until it is started again.
    assert!(std::sync::Arc::ptr_eq(&files.want(&file), &c));
    files.restart(&file);
    assert!(c.replaced());
    let d = files.want(&file);
    assert!(!std::sync::Arc::ptr_eq(&d, &c) && d.state().running);
    files.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The value of channel `name` of node `path`'s first output.
fn read(f: &F, path: &str, name: &str) -> f64 {
    let id = f.net.resolve(&path.parse().unwrap()).unwrap();
    let c = f.cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Chans>()).cloned().unwrap();
    value(&c, name)
}

#[test]
fn the_envelopes_are_the_live_analysis_without_its_lag() {
    // The example track through an audio.file at 48 kHz into a chan.beat_analysis, and into
    // grids whose offset puts back the live analysis' lag: each grid then reads what the live
    // node reads in the same frame.
    use penumbra_audio::beats::{ENVELOPE_RATE, KICK_LAG_HOPS, LEVEL_LAG_HOPS, ONSET_LAG_HOPS};
    let dir = temp_dir("envelopes");
    let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let hop_ms = 512.0 / ENVELOPE_RATE * 1000.0;
    let mut ops = grid_ops(&music.to_string_lossy());
    ops.push(set("/g.offset", -LEVEL_LAG_HOPS * hop_ms));
    for (path, lag) in [("/h", ONSET_LAG_HOPS), ("/k", KICK_LAG_HOPS)] {
        ops.push(create(path, BEAT_GRID));
        ops.push(connect("/p", path, 0));
        ops.push(set(&format!("{path}.offset"), -lag * hop_ms));
    }
    ops.push(create("/a", "chan.beat_analysis"));
    ops.push(connect("/p", "/a", 0));
    let mut f = fixture(&ops, &dir.join("cache"));
    analysed(&mut f);
    let ids: Vec<_> =
        ["/g", "/h", "/k", "/a"].iter().map(|p| f.net.resolve(&p.parse().unwrap()).unwrap()).collect();
    let (mut diff, mut n) = ([0.0f64; 4], 0usize);
    let (mut hits, mut matched) = ([0usize; 2], [0usize; 2]);
    let mut series: Vec<[bool; 4]> = Vec::new();
    // 30 s at 60 frames per second, from the file's start; the first 5 s let the live
    // analysis' ranges fill.
    for i in 0..1800 {
        f.clock.advance(1);
        f.cooker.cook_showing(&f.net, f.clock.now(), &ids, &[], &[]);
        let onsets = [
            read(&f, "/h", "onset"),
            read(&f, "/a", "onset"),
            read(&f, "/k", "kick"),
            read(&f, "/a", "kick"),
        ];
        series.push(onsets.map(|v| v == 1.0));
        if i < 300 {
            continue;
        }
        for (d, name) in diff.iter_mut().zip(["level", "low", "mid", "high"]) {
            *d += (read(&f, "/g", name) - read(&f, "/a", name)).abs();
        }
        n += 1;
    }
    let mean = diff.map(|d| d / n as f64);
    // The live analysis cuts its hops where its first block began; the file's are cut from its
    // first sample. Hops 6 ms apart read alike within a hundredth on the mean (measured: 0.003
    // to 0.007).
    assert!(mean.iter().all(|m| *m < 0.015), "mean differences level, low, mid, high: {mean:?}");
    // Onsets and kicks: each live one has a grid one within a frame.
    for (k, (grid, live)) in [(0usize, 1usize), (2, 3)].into_iter().enumerate() {
        for (i, s) in series.iter().enumerate().skip(300) {
            if s[live] {
                hits[k] += 1;
                let near = series.get(i.saturating_sub(1)..(i + 2).min(series.len())).unwrap();
                if near.iter().any(|x| x[grid]) {
                    matched[k] += 1;
                }
            }
        }
        assert!(hits[k] > 30, "{hits:?}");
        assert!(matched[k] * 100 >= hits[k] * 97, "matched {matched:?} of {hits:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
