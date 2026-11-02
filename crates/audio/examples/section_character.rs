//! Prints the character of every section of a sound file, as the music bus finds it: the count
//! of each role, a histogram of `change`, and `change` at track starts against other section
//! starts. For tuning the weights of `change`.
//!
//! `cargo run -p penumbra-audio --release --example section_character -- [SOUND] [CACHE_DIR] [--list]`
//!
//! `SOUND` is the beat-grid example's track by default. `CACHE_DIR` is where beats and
//! envelopes are cached; by default the app's own cache (`%LOCALAPPDATA%/penumbra/cache/beats`),
//! so a file the app has analysed loads at once. `--list` prints one line per section.

use std::path::{Path, PathBuf};
use std::time::Instant;

use penumbra_audio::beats::{self, BeatGrid, BeatModel, Bus, Role, Sections, TRAVEL_CAP, TRAVEL_FLOOR};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let list = args.iter().any(|a| a == "--list");
    args.retain(|a| a != "--list");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    let sound = args.first().map_or(example, PathBuf::from);
    let cache = args.get(1).map(PathBuf::from).or_else(app_cache);
    if let Err(e) = run(&sound, cache, list) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn app_cache() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("penumbra").join("cache").join("beats"))
}

fn run(sound: &Path, cache: Option<PathBuf>, list: bool) -> Result<(), String> {
    let model = BeatModel::shipped().map_err(|e| e.to_string())?;
    let t = Instant::now();
    let found =
        beats::analyze_file(sound, &model, cache.as_deref(), None, &mut |_| {}).map_err(|e| e.to_string())?;
    let env = beats::analyze_envelopes(sound, cache.as_deref(), None, &mut |_| {}, &mut |_| {})
        .map_err(|e| e.to_string())?;
    println!("{}: beats and envelopes in {:.2} s", sound.display(), t.elapsed().as_secs_f64());
    let grid = BeatGrid::from_beats(&found);
    let t = Instant::now();
    let sections = Sections::of(&grid, &env)?;
    println!("sections in {:.3} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let bus = Bus::new(&grid, &env, Some(&sections), TRAVEL_FLOOR, TRAVEL_CAP);
    println!("bus in {:.3} s, {:.2} MB", t.elapsed().as_secs_f64(), bus.bytes() as f64 / 1e6);
    let chars = bus.sections();
    println!("{} sections in {} tracks", chars.len(), sections.tracks());

    let mut counts = [0usize; 6];
    for c in chars {
        if let Some(n) = counts.get_mut(c.role.index()) {
            *n += 1;
        }
    }
    let roles: Vec<String> = Role::ALL.iter().zip(counts).map(|(r, n)| format!("{} {n}", r.name())).collect();
    println!("roles: {}", roles.join(", "));

    let mut bins = [0usize; 10];
    for c in chars.iter().skip(1) {
        // `as`: clamped to the bins.
        let b = ((c.change * 10.0).floor() as usize).min(9);
        if let Some(n) = bins.get_mut(b) {
            *n += 1;
        }
    }
    println!("change (sections after the first):");
    for (i, n) in bins.iter().enumerate() {
        println!("  {:.1}-{:.1} {n:4} {}", i as f64 / 10.0, (i + 1) as f64 / 10.0, "#".repeat(*n));
    }
    let list_of = sections.list();
    let mut starts = Vec::new();
    let mut others = Vec::new();
    let mut by_role: Vec<Vec<f64>> = vec![Vec::new(); 6];
    for (i, (c, s)) in chars.iter().zip(list_of).enumerate().skip(1) {
        if s.track_start {
            starts.push(c.change);
        } else {
            others.push(c.change);
            if let Some(v) = by_role.get_mut(c.role.index()) {
                v.push(c.change);
            }
        }
        if list {
            println!(
                "{i:3} {:>8} track {:2}{} {:6} strength {:.2} energy {:.2} jump {:+.2} bass {:.2} density {:.2} kick {:.2} change {:.2}",
                clock(c.start),
                s.track,
                if s.track_start { "*" } else { " " },
                c.role.name(),
                s.strength,
                c.energy,
                c.jump,
                c.bass,
                c.density,
                c.kick,
                c.change
            );
        }
    }
    starts.sort_by(f64::total_cmp);
    others.sort_by(f64::total_cmp);
    println!(
        "track starts ({}): min {:.2}, median {:.2}",
        starts.len(),
        starts.first().copied().unwrap_or(0.0),
        quantile(&starts, 0.5)
    );
    println!(
        "other starts ({}): median {:.2}, 90th percentile {:.2}",
        others.len(),
        quantile(&others, 0.5),
        quantile(&others, 0.9)
    );
    for (r, v) in Role::ALL.iter().zip(by_role.iter_mut()) {
        v.sort_by(f64::total_cmp);
        if !v.is_empty() {
            println!("  inside tracks, {}: {} , median {:.2}", r.name(), v.len(), quantile(v, 0.5));
        }
    }
    Ok(())
}

/// The `q` quantile of sorted `v` by the nearest rank, 0 when empty.
fn quantile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
    v.get(i).copied().unwrap_or(0.0)
}

fn clock(t: f64) -> String {
    let s = t.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}
