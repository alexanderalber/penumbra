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
    let mut countdown = Vec::new();
    for i in 0..780u64 {
        f.clock.advance(1);
        let c = cook(&mut f).0.unwrap();
        if (700..740).contains(&i) {
            assert_eq!(value(&c, "sections_ready"), 1.0);
            countdown.push((
                value(&c, "seconds_to_boundary"),
                value(&c, "build"),
                value(&c, "section_index"),
            ));
        }
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
    // The sections go by the time within the pass: the end of the file is a boundary, counted
    // down to and built up to, then the first section begins again.
    let (before, after) = countdown.split_at(19);
    let (end, build, _) = before.last().unwrap();
    assert!(*end < 0.05 && *build > 0.95, "{countdown:?}");
    assert!(after.iter().skip(2).all(|(s, b, k)| *s > 1.0 && *b < 0.5 && *k == 0.0), "{countdown:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wiring_without_a_file_is_an_error_and_a_file_without_beats_a_warning() {
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
    // A file with no beats to find is a warning: the node still says where the playhead is,
    // so a show whose clock follows `seconds` keeps following the music.
    let mut f = fixture(&grid_ops(&junk.to_string_lossy()), &cache);
    let id = f.net.resolve(&"/g".parse().unwrap()).unwrap();
    let t = Instant::now();
    let w = loop {
        let (_, error) = cook(&mut f);
        assert_eq!(error, None);
        if let Some(w) = f.cooker.status(id).and_then(|s| s.warning).filter(|w| w.contains("no beats")) {
            break w;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", cook(&mut f));
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(w.contains("no beats"), "{w}");
    let before = value(&cook(&mut f).0.unwrap(), "seconds");
    for _ in 0..60 {
        f.clock.advance(1);
        cook(&mut f);
    }
    let (c, error) = cook(&mut f);
    assert_eq!(error, None);
    let c = c.unwrap();
    assert!((value(&c, "seconds") - before - 1.0).abs() < 0.05, "{} after {before}", value(&c, "seconds"));
    assert_eq!(value(&c, "ready"), 0.0);
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

/// A scrubbed `audio.file` (its `position` bound) moves the grid with it: the beat is the one at
/// the position, a jump or a move back does not tick, moving forward across a beat does.
#[test]
fn the_grid_follows_a_scrubbed_position() {
    let dir = temp_dir("scrub");
    let file = clicks(&dir);
    let mut ops = grid_ops(&file);
    ops.extend([
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", 3.2),
        Op::Bind {
            param: penumbra_core::ParamPath::parse("/p.position").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(penumbra_core::Name::new("pos").unwrap()),
            cell: None,
            format: None,
        },
    ]);
    let mut f = fixture(&ops, &dir.join("cache"));
    analysed(&mut f);
    let at = |f: &mut F, pos: f64| {
        f.edit(&[set("/k.value0", pos)]);
        f.clock.advance(1);
        cook(f).0.unwrap()
    };
    let c = at(&mut f, 3.2);
    assert_eq!(value(&c, "beat_index"), 6.0, "the beat at 3.0 s");
    // Back and in a jump: the beat follows, no tick.
    let c = at(&mut f, 1.2);
    assert_eq!((value(&c, "beat_index"), value(&c, "tick")), (2.0, 0.0));
    let c = at(&mut f, 9.2);
    assert_eq!((value(&c, "beat_index"), value(&c, "tick")), (18.0, 0.0));
    let c = at(&mut f, 1.2);
    assert_eq!(value(&c, "beat_index"), 2.0);
    // Forward slowly across 1.5 s: one tick, at the crossing.
    let mut ticks = Vec::new();
    for k in 1..=60u32 {
        let pos = 1.2 + f64::from(k) * 0.01;
        let c = at(&mut f, pos);
        if value(&c, "tick") == 1.0 {
            ticks.push(k);
        }
    }
    assert_eq!(ticks.len(), 1, "{ticks:?}");
    assert!(ticks[0].abs_diff(30) <= 2, "{ticks:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The section channels of `c`, by name.
fn sections_of(c: &Chans) -> Vec<(&'static str, f64)> {
    let names = &super::beat_grid::CHANNELS;
    let first = names.iter().position(|n| *n == "section_index").unwrap();
    let last = names.iter().position(|n| *n == "sections_ready").unwrap();
    names[first..=last].iter().map(|n| (*n, value(c, n))).collect()
}

/// Cooks `/g` and `/o` with the `sections` output of `/g` read; the channels of both, once
/// both have them.
fn try_cook_two(f: &mut F) -> Option<(Chans, Chans)> {
    let ids: Vec<_> = ["/g", "/o"].iter().map(|p| f.net.resolve(&p.parse().unwrap()).unwrap()).collect();
    let table = Port { node: ids[0], output: 3 };
    f.cooker.cook_showing(&f.net, f.clock.now(), &ids, &[], &[table]);
    let get =
        |i: usize| f.cooker.output(Port { node: ids[i], output: 0 }).and_then(|d| d.get::<Chans>()).cloned();
    Some((get(0)?, get(1)?))
}

fn cook_two(f: &mut F) -> (Chans, Chans) {
    try_cook_two(f).unwrap()
}

/// The example track, scrubbed: the section channels read 0 until the sections are found, the
/// `sections` table lists them end to end, the countdowns reach the boundary, `offset` moves the
/// lookup ahead.
#[test]
fn the_sections_of_the_example_track() {
    let dir = temp_dir("sections");
    let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let mut ops = grid_ops(&music.to_string_lossy());
    ops.extend([
        create("/o", BEAT_GRID),
        connect("/p", "/o", 0),
        set("/o.offset", 1000.0),
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", 10.0),
        Op::Bind {
            param: penumbra_core::ParamPath::parse("/p.position").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(penumbra_core::Name::new("pos").unwrap()),
            cell: None,
            format: None,
        },
    ]);
    let mut f = fixture(&ops, &dir.join("cache"));
    let t = Instant::now();
    let mut waited = 0;
    loop {
        assert!(t.elapsed() < Duration::from_secs(120), "the sections never came");
        std::thread::sleep(Duration::from_millis(20));
        f.clock.advance(1);
        let Some((g, _)) = try_cook_two(&mut f) else { continue };
        if value(&g, "sections_ready") == 0.0 {
            waited += 1;
            assert!(sections_of(&g).iter().all(|(_, v)| *v == 0.0), "{:?}", sections_of(&g));
        } else if value(&g, "analyzing") == 0.0 {
            break;
        }
    }
    assert!(waited > 0, "the sections were there before the analysis");

    let id = f.net.resolve(&"/g".parse().unwrap()).unwrap();
    let table =
        f.cooker.output(Port { node: id, output: 3 }).and_then(|d| d.get::<Table>()).cloned().unwrap();
    assert_eq!(
        table.rows()[0],
        [
            "start",
            "end",
            "bars",
            "label",
            "letter",
            "track",
            "strength",
            "zone_start",
            "zone_end",
            "energy",
            "bass",
            "density",
            "kick",
            "jump",
            "role",
            "change"
        ]
    );
    let rows: Vec<Vec<f64>> = table
        .rows()
        .iter()
        .skip(1)
        .map(|r| r.iter().map(|x| x.parse().unwrap_or(f64::NAN)).collect())
        .collect();
    assert!(!rows.is_empty());
    for w in rows.windows(2) {
        assert_eq!(w[0][1], w[1][0], "end to end: {rows:?}");
    }
    let last = rows.last().unwrap();
    assert!((last[1] - 62.0).abs() < 2.0 && rows[0][0] < 3.0, "{rows:?}");
    assert!(
        rows.iter().all(|r| (0.0..=1.0).contains(&r[6])
            && r[7] <= r[0] + 1e-3
            && r[8] >= r[0] - 1e-3
            && (0.0..=1.0).contains(&r[9])),
        "{rows:?}"
    );

    let at = |f: &mut F, pos: f64| {
        f.edit(&[set("/k.value0", pos)]);
        f.clock.advance(1);
        cook_two(f)
    };
    // The boundary into the second section, or the end of the file when there is one section.
    let b = rows.get(1).map_or(last[1], |r| r[0]);
    let (g, _) = at(&mut f, b - 0.02);
    assert_eq!(value(&g, "sections_ready"), 1.0);
    assert_eq!(value(&g, "section_index"), 0.0);
    assert!((value(&g, "seconds_to_boundary") - 0.02).abs() < 2e-3, "{:?}", sections_of(&g));
    assert!(value(&g, "bars_to_boundary") < 0.02 && value(&g, "build") > 0.98, "{:?}", sections_of(&g));
    assert!((0.0..1.0).contains(&value(&g, "phrase")) && value(&g, "phrase_bar") < 8.0);
    if let Some(next) = rows.get(1) {
        let (g, _) = at(&mut f, b + 0.02);
        assert_eq!(value(&g, "section_index"), 1.0);
        assert_eq!(value(&g, "section_label"), next[3]);
        assert!((value(&g, "seconds_to_boundary") - (next[1] - b - 0.02)).abs() < 2e-3);
        assert!(value(&g, "build") < 1.0);
    }
    // One second ahead: /o at a position reads what /g reads a second later.
    let (_, o) = at(&mut f, b - 1.5);
    let (g, _) = at(&mut f, b - 0.5);
    for (name, v) in sections_of(&g) {
        assert!((value(&o, name) - v).abs() < 2e-3, "{name}: {} vs {v}", value(&o, name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Synthetic grid and envelopes: two kinds of sound, 16 bars each. The sections come only from
/// a complete grid with complete envelopes; the lookup counts down to the boundary between them.
#[test]
fn sections_come_from_complete_grids_and_envelopes() {
    use std::sync::Arc;

    use penumbra_audio::beats::{Beats, ENVELOPE_RATE, EnvelopeBuilder, envelopes_of};

    use super::beat_grid::{Grid, State, find_sections, section_values};
    // 120 beats per minute, a bar every 2 s: a low hum with a kick, then a high tone with hiss.
    let rate = ENVELOPE_RATE;
    let n = (64.0 * rate) as usize;
    let mut seed = 3u32;
    let x: Vec<f32> = (0..n)
        .map(|i| {
            use std::f64::consts::TAU;
            let t = i as f64 / rate;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = f64::from(seed >> 8) / f64::from(1u32 << 23) - 1.0;
            let beat = t % 0.5;
            let v = if t < 32.0 {
                0.6 * (-beat / 0.07).exp() * (TAU * 55.0 * beat).sin() + 0.2 * (TAU * 82.0 * t).sin()
            } else {
                0.15 * (TAU * 3000.0 * t).sin() + 0.2 * noise * (-beat / 0.05).exp()
            };
            v as f32
        })
        .collect();
    let env = Arc::new(envelopes_of(&x, 1, rate).unwrap());
    let beats = Beats {
        beats: (0..128).map(|i| f64::from(i) * 0.5).collect(),
        downbeats: (0..32).map(|i| f64::from(i) * 2.0).collect(),
        downbeat_logits: Vec::new(),
    };
    let grid = Arc::new(Grid::new(beats.clone(), f64::INFINITY));
    let s = find_sections(&grid, &env).unwrap();
    let list = s.sections.list();
    let starts: Vec<f64> = list.iter().map(|c| c.start).collect();
    let k = starts.iter().position(|t| (t - 32.0).abs() <= 2.0).unwrap_or_else(|| panic!("{starts:?}"));
    let b = starts[k];
    assert_ne!(list[k].label, list[k - 1].label);
    assert_eq!(s.strip.len(), s.sections.len());
    assert_eq!(s.strip[0].text, "A");
    // `as`: small indices.
    let v = section_values(Some(&s), b - 0.05);
    assert_eq!(v[0], (k - 1) as f64);
    assert!((v[7] - 0.05).abs() < 1e-3 && v[8] > 0.97, "{v:?}");
    let v = section_values(Some(&s), b + 0.05);
    assert_eq!(v[0], k as f64);
    assert_eq!(v[13], 1.0);
    // Before the sections, and at a time that is no number.
    assert!(section_values(None, b).iter().all(|v| *v == 0.0));
    let v = section_values(Some(&s), f64::NAN);
    assert!(v[..13].iter().all(|v| *v == 0.0) && v[13] == 1.0, "{v:?}");

    // Claimed only once, and only with both complete.
    let complete = State { grid: Some(grid.clone()), envelopes: Some(env.clone()), ..State::default() };
    let mut st = complete.clone();
    assert!(st.claim_sections().is_some() && st.sections_running && st.busy());
    assert!(st.claim_sections().is_none());
    let partial = Arc::new(Grid::new(beats, 10.0));
    let snapshot = {
        let mut b = EnvelopeBuilder::new(rate, 1).unwrap();
        b.push_interleaved(&x[..rate as usize * 4]);
        Arc::new(b.snapshot())
    };
    let hostile = [
        State { grid: None, ..complete.clone() },
        State { envelopes: None, envelope_error: Some("broken".into()), ..complete.clone() },
        State { grid: Some(partial), ..complete.clone() },
        State { envelopes: Some(snapshot), ..complete.clone() },
        State { running: true, ..complete.clone() },
        State { error: Some("no sound".into()), grid: None, ..complete.clone() },
        State { sections_error: Some("before".into()), ..complete.clone() },
    ];
    for mut h in hostile {
        assert!(h.claim_sections().is_none(), "{h:?}");
    }
    // No beats at all, and envelopes of silence too short for a bar: errors, no crash.
    let empty = Grid::new(Beats::default(), f64::INFINITY);
    assert!(find_sections(&empty, &env).is_err());
    let none = envelopes_of(&[], 1, rate).unwrap();
    assert!(find_sections(&grid, &none).is_err());
}

/// The cue list of the example track: a `chan.sequence` whose clock is bound to the grid's
/// `seconds` stands at the step of the section under the playhead, wherever the playhead
/// jumps, after a loop and with `offset`, and fades through each blend zone.
#[test]
fn a_cue_list_follows_the_sections_of_the_example_track() {
    use super::cues::{Blend, Level, Pieces, cues};
    let dir = temp_dir("cues");
    let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let file = music.to_string_lossy().into_owned();
    let mut ops = grid_ops(&file);
    let bind = |param: &str, from: &str, channel: &str| Op::Bind {
        param: penumbra_core::ParamPath::parse(param).unwrap(),
        component: None,
        from: from.parse().unwrap(),
        channel: Some(penumbra_core::Name::new(channel).unwrap()),
        cell: None,
        format: None,
    };
    ops.extend([
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", 1.0),
        bind("/p.position", "/k", "pos"),
        create("/s", crate::sequence::SEQUENCE),
        bind("/s.clock", "/g", "seconds"),
    ]);
    let mut f = fixture(&ops, &dir.join("cache"));
    let ids: Vec<_> = ["/g", "/s"].iter().map(|p| f.net.resolve(&p.parse().unwrap()).unwrap()).collect();
    let cook_both = |f: &mut F| {
        f.cooker.cook_showing(&f.net, f.clock.now(), &ids, &[], &[]);
        let get = |i: usize| {
            f.cooker.output(Port { node: ids[i], output: 0 }).and_then(|d| d.get::<Chans>()).cloned()
        };
        (get(0), get(1))
    };
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < Duration::from_secs(120), "the sections never came");
        std::thread::sleep(Duration::from_millis(20));
        f.clock.advance(1);
        if let (Some(g), _) = cook_both(&mut f)
            && value(&g, "sections_ready") == 1.0
            && value(&g, "analyzing") == 0.0
        {
            break;
        }
    }
    let state = f.cooker.resource::<BeatFiles>().unwrap().get(Path::new(&file)).unwrap().state();
    let structure = state.sections.clone().unwrap();
    let env = state.envelopes.clone().unwrap();
    let duration = env.len() as f64 * env.hop_seconds();
    let c =
        cues(&structure.sections, duration, "music.wav", Level::Section, Pieces::Label, Blend::Zone).unwrap();
    let list = structure.sections.list().to_vec();
    assert_eq!(c.starts.len(), list.len(), "{}", c.steps);
    f.edit(&[set("/s.steps", c.steps.as_str()), set("/s.names", c.names.as_str()), set("/s.lead_in", 0.0)]);

    let at = |f: &mut F, pos: f64| {
        f.edit(&[set("/k.value0", pos)]);
        f.clock.advance(1);
        let (g, s) = cook_both(f);
        let s = s.unwrap_or_else(|| panic!("no show: {:?}", f.cooker.status(ids[1])));
        (g.unwrap(), s)
    };
    // Inside section i, away from both blends.
    let inside = |i: usize| {
        let a = list[i].zone.1.max(list[i].start);
        let b = list.get(i + 1).map_or(list[i].end, |n| n.zone.0.min(n.start));
        (a + b) / 2.0
    };
    // Jumps all over the file: last, first, then every other one backwards.
    let n = list.len();
    let mut order: Vec<usize> = vec![n - 1, 0];
    order.extend((0..n).rev());
    for i in order {
        let (g, s) = at(&mut f, inside(i));
        assert_eq!(value(&g, "section_index"), i as f64);
        assert_eq!(value(&s, "step"), i as f64, "at {}: {}", inside(i), c.steps);
        assert_eq!(value(&s, "piece"), list[i].label as f64);
        assert_eq!(value(&s, "mix"), 0.0);
    }
    // The file loops: a pass later the show is at the same step.
    for i in [1.min(n - 1), 0] {
        let (g, s) = at(&mut f, duration + inside(i));
        assert_eq!(value(&g, "section_index"), i as f64);
        assert_eq!(value(&s, "step"), i as f64);
    }
    assert_eq!(value(&at(&mut f, duration - 0.05).1, "step"), (n - 1) as f64);
    // Offset: a second ahead, half a second before step 1 begins is step 1 already.
    if let Some(&(b, _)) = c.starts.get(1) {
        assert_eq!(value(&at(&mut f, b - 0.5).1, "step"), 0.0);
        f.edit(&[set("/g.offset", 1000.0)]);
        let (g, s) = at(&mut f, b - 0.5);
        assert_eq!(value(&s, "step"), 1.0);
        assert!((value(&g, "seconds") - (b + 0.5)).abs() < 1e-3);
        f.edit(&[set("/g.offset", 0.0)]);
    }
    // Through a blend zone the show fades from one section's piece into the next one's.
    for (i, s) in list.iter().enumerate().skip(1) {
        if s.zone.1 - s.zone.0 < 0.1 {
            continue;
        }
        let (_, show) = at(&mut f, (s.zone.0 + s.zone.1) / 2.0);
        let mix = value(&show, "mix");
        assert!(mix > 0.2 && mix < 0.8, "section {i}: mix {mix}");
        assert_eq!(value(&show, "from"), list[i - 1].label as f64);
        assert_eq!(value(&show, "to"), s.label as f64);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bus channels of `c`, by name.
fn bus_of(c: &Chans) -> Vec<(&'static str, f64)> {
    let names = &super::beat_grid::CHANNELS;
    let first = names.iter().position(|n| *n == "travelled").unwrap();
    names[first..].iter().map(|n| (*n, value(c, n))).collect()
}

/// The music bus of the example track (32 bars at 124 BPM, a breakdown in bars 17 to 24),
/// scrubbed: the beats travelled match the grid's, the tempo holds at 124 or at the floor,
/// drift stays small, the breakdown has less bass, the kick's return is a drop. Seeks back and
/// a second pass give the same values.
#[test]
fn the_music_bus_of_the_example_track() {
    let dir = temp_dir("bus");
    let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let mut ops = grid_ops(&music.to_string_lossy());
    ops.extend([
        create("/o", BEAT_GRID),
        connect("/p", "/o", 0),
        set("/o.travel_floor", 200.0),
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", 10.0),
        Op::Bind {
            param: penumbra_core::ParamPath::parse("/p.position").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(penumbra_core::Name::new("pos").unwrap()),
            cell: None,
            format: None,
        },
    ]);
    let mut f = fixture(&ops, &dir.join("cache"));
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < Duration::from_secs(120), "the bus never came");
        std::thread::sleep(Duration::from_millis(20));
        f.clock.advance(1);
        let Some((g, _)) = try_cook_two(&mut f) else { continue };
        if value(&g, "sections_ready") == 0.0 {
            assert!(bus_of(&g).iter().all(|(_, v)| *v == 0.0), "{:?}", bus_of(&g));
        } else if value(&g, "analyzing") == 0.0 {
            break;
        }
    }
    let at = |f: &mut F, pos: f64| {
        f.edit(&[set("/k.value0", pos)]);
        f.clock.advance(1);
        cook_two(f)
    };
    let duration = {
        let file = music.to_string_lossy().into_owned();
        let state = f.cooker.resource::<BeatFiles>().unwrap().get(Path::new(&file)).unwrap().state();
        assert!(state.bus.is_some());
        let env = state.envelopes.unwrap();
        env.len() as f64 * env.hop_seconds()
    };
    assert!((duration - 61.9).abs() < 0.2, "{duration}");

    let mut before = f64::NEG_INFINITY;
    let mut seen = Vec::new();
    let (mut low_break, mut low_around) = (Vec::new(), Vec::new());
    for k in 0..240 {
        let s = 0.1 + f64::from(k) * 0.25;
        if s >= duration - 0.1 {
            break;
        }
        let (g, o) = at(&mut f, s);
        let travelled = value(&g, "travelled");
        let beats = value(&g, "beat_index") + value(&g, "beat");
        assert!(travelled >= before, "{s}: {before} then {travelled}");
        before = travelled;
        if s >= 3.0 {
            assert!((travelled - beats).abs() <= 0.01 * beats, "{s}: {travelled} vs {beats}");
        }
        let bpm = value(&g, "travel_bpm");
        assert!((bpm - 124.0).abs() <= 2.0, "{s}: {bpm}");
        assert!((value(&g, "beat_length") - 1.0).abs() < 0.03, "{s}: {:?}", bus_of(&g));
        assert!(value(&g, "drift").abs() <= 0.1, "{s}: {:?}", bus_of(&g));
        assert!((value(&o, "travel_bpm") - 200.0).abs() < 0.01, "{s}: {:?}", bus_of(&o));
        assert!((value(&o, "travelled") - s * 200.0 / 60.0).abs() < 0.01, "{s}: {:?}", bus_of(&o));
        for e in ["energy", "energy_low", "energy_mid", "energy_high", "section_energy", "drop"] {
            assert!((0.0..=1.0).contains(&value(&g, e)), "{s}: {:?}", bus_of(&g));
        }
        let low = value(&g, "energy_low");
        if (33.0..45.0).contains(&s) {
            low_break.push(low);
        } else if (17.0..29.0).contains(&s) || (48.0..60.0).contains(&s) {
            low_around.push(low);
        }
        seen.push((s, bus_of(&g)));
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (lb, la) = (mean(&low_break), mean(&low_around));
    assert!(lb + 0.2 < la, "bass in the breakdown {lb}, around it {la}");

    // The kick returns with bar 25: a drop. None at 10 s or in the breakdown.
    let bar = 4.0 * 10_670.0 / 22_050.0;
    let (g, _) = at(&mut f, 24.0 * bar + 0.05);
    assert!(value(&g, "drop") > 0.8, "{:?}", bus_of(&g));
    assert!(value(&g, "section_energy") > 0.0);
    for s in [10.0, 38.0] {
        let (g, _) = at(&mut f, s);
        assert_eq!(value(&g, "drop"), 0.0, "{s}: {:?}", bus_of(&g));
    }

    // Back in time and on the second pass: the same values.
    for (s, want) in seen.iter().rev().step_by(7) {
        let (g, _) = at(&mut f, *s);
        assert_eq!(&bus_of(&g), want, "back at {s}");
        // The loop's length is the file's: 128 beats of 10 670 samples.
        let (g, _) = at(&mut f, 128.0 * 10_670.0 / 22_050.0 + *s);
        for ((name, v), (_, w)) in bus_of(&g).iter().zip(want) {
            assert!((v - w).abs() < 1e-6, "second pass at {s}, {name}: {v} vs {w}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The character of the example track's sections (intro, main, the breakdown from bar 17, the
/// main part again): roles, kicks only outside the breakdown, a stronger change at the
/// breakdown and at the kick's return than from intro to main, and the section channels equal
/// to the `sections` table at a seek into each section.
#[test]
fn the_character_of_the_example_track() {
    let dir = temp_dir("character");
    let music = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let mut ops = grid_ops(&music.to_string_lossy());
    ops.extend([
        create("/o", BEAT_GRID),
        connect("/p", "/o", 0),
        create("/k", "chan.constant"),
        set("/k.name0", "pos"),
        set("/k.value0", 10.0),
        Op::Bind {
            param: penumbra_core::ParamPath::parse("/p.position").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(penumbra_core::Name::new("pos").unwrap()),
            cell: None,
            format: None,
        },
    ]);
    let mut f = fixture(&ops, &dir.join("cache"));
    let names = ["section_role", "section_change", "section_bass", "section_density", "section_kick"];
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < Duration::from_secs(120), "the sections never came");
        std::thread::sleep(Duration::from_millis(20));
        f.clock.advance(1);
        let Some((g, _)) = try_cook_two(&mut f) else { continue };
        if value(&g, "sections_ready") == 0.0 {
            assert!(names.iter().all(|n| value(&g, n) == 0.0), "{:?}", bus_of(&g));
        } else if value(&g, "analyzing") == 0.0 {
            break;
        }
    }
    let id = f.net.resolve(&"/g".parse().unwrap()).unwrap();
    let table =
        f.cooker.output(Port { node: id, output: 3 }).and_then(|d| d.get::<Table>()).cloned().unwrap();
    let header = &table.rows()[0];
    assert_eq!(header[9..], ["energy", "bass", "density", "kick", "jump", "role", "change"]);
    let col = |name: &str| header.iter().position(|h| h == name).unwrap();
    let rows = &table.rows()[1..];
    let num = |r: &Vec<String>, name: &str| -> f64 { r[col(name)].parse().unwrap() };
    let roles: Vec<&str> = rows.iter().map(|r| r[col("role")].as_str()).collect();
    assert_eq!(roles, ["intro", "main", "break", "drop"], "{rows:?}");

    // The breakdown: bars 17 to 24, without kick.
    let bar = 4.0 * 10_670.0 / 22_050.0;
    assert!((num(&rows[2], "start") - 16.0 * bar).abs() < 0.1, "{rows:?}");
    assert!(num(&rows[2], "kick") < 0.1, "{rows:?}");
    for r in [&rows[1], &rows[3]] {
        assert!(num(r, "kick") > 0.8, "{rows:?}");
        assert!(num(r, "bass") > num(&rows[2], "bass") + 0.2, "{rows:?}");
        assert!(num(r, "density") > 0.5, "{rows:?}");
    }
    let change: Vec<f64> = rows.iter().map(|r| num(r, "change")).collect();
    assert_eq!(change[0], 0.0);
    assert!(change[2] > change[1] + 0.05 && change[3] > change[1] + 0.05, "{change:?}");
    assert!(num(&rows[3], "jump") >= 0.2 && num(&rows[2], "jump") < -0.2, "{rows:?}");

    // A seek into each section: the channels are its row.
    for (i, r) in rows.iter().enumerate() {
        let mid = (num(r, "start") + num(r, "end")) / 2.0;
        f.edit(&[set("/k.value0", mid)]);
        f.clock.advance(1);
        let (g, _) = cook_two(&mut f);
        assert_eq!(value(&g, "section_index"), i as f64);
        let role =
            ["intro", "break", "build", "drop", "outro", "main"].iter().position(|x| *x == roles[i]).unwrap();
        assert_eq!(value(&g, "section_role"), role as f64);
        for (channel, column) in [
            ("section_change", "change"),
            ("section_bass", "bass"),
            ("section_density", "density"),
            ("section_kick", "kick"),
        ] {
            assert!(
                (value(&g, channel) - num(r, column)).abs() < 1e-3,
                "{channel} in section {i}: {:?}",
                bus_of(&g)
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
