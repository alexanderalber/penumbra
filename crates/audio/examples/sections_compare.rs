//! Finds the sections of a sound file twice, with blends of two tracks taken into the track
//! change's transition zone (as the app does) and with each blend left as a section of its own,
//! and writes both for a listening comparison.
//!
//! `cargo run -p penumbra-audio --release --example sections_compare -- SOUND OUT_DIR [CACHE_DIR]`
//!
//! `CACHE_DIR` is where beats and envelopes are cached; by default the app's own cache
//! (`%LOCALAPPDATA%/penumbra/cache/beats`), so a file the app has analysed loads at once. The
//! beat network is the shipped one (`assets/models`). Writes `OUT_DIR/sections_absorbed.json`,
//! `OUT_DIR/sections_separate.json` and `OUT_DIR/compare_data.js`, which sets
//! `window.COMPARE` to both for a page opened over `file://`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use penumbra_audio::beats::{self, BLEND_BARS, BeatGrid, BeatModel, Sections};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [sound, out] => run(Path::new(sound), Path::new(out), None),
        [sound, out, cache] => run(Path::new(sound), Path::new(out), Some(PathBuf::from(cache))),
        _ => Err("usage: sections_compare SOUND OUT_DIR [CACHE_DIR]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn app_cache() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("penumbra").join("cache").join("beats"))
}

fn run(sound: &Path, out: &Path, cache: Option<PathBuf>) -> Result<(), String> {
    let cache = cache.or_else(app_cache).ok_or("no cache directory: pass CACHE_DIR")?;
    let model = BeatModel::shipped().map_err(|e| e.to_string())?;
    let t = Instant::now();
    let found =
        beats::analyze_file(sound, &model, Some(&cache), None, &mut |_| {}).map_err(|e| e.to_string())?;
    let env = beats::analyze_envelopes(sound, Some(&cache), None, &mut |_| {}, &mut |_| {})
        .map_err(|e| e.to_string())?;
    println!("beats and envelopes in {:.1} s", t.elapsed().as_secs_f64());
    let grid = BeatGrid::from_beats(&found);
    let end = env.len() as f64 * env.hop_seconds();
    let bars: Vec<f64> = grid.downbeats().into_iter().filter(|t| *t < end).collect();
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut js = String::from("window.COMPARE = {");
    let _ = write!(js, "\"duration\": {end:.3}, \"bars\": [{}], ", join(&bars));
    for (name, blend) in [("absorbed", BLEND_BARS), ("separate", 0)] {
        let s = Sections::find_blending(&bars, end, &env, blend)?;
        let json = to_json(&s, blend);
        println!(
            "{name} (blend bars {blend}): {} tracks, {} sections, {} kinds, {} zones",
            s.tracks(),
            s.len(),
            s.kinds(),
            s.list().iter().filter(|x| x.zone.1 > x.zone.0).count()
        );
        let path = out.join(format!("sections_{name}.json"));
        std::fs::write(&path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
        let _ = write!(js, "\"{name}\": {json}, ");
    }
    js.push_str("};\n");
    let path = out.join("compare_data.js");
    std::fs::write(&path, js).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("written to {}", out.display());
    Ok(())
}

fn join(v: &[f64]) -> String {
    v.iter().map(|t| format!("{t:.3}")).collect::<Vec<_>>().join(",")
}

fn to_json(s: &Sections, blend: usize) -> String {
    let mut j = format!(
        "{{\"blend_bars\": {blend}, \"tracks\": {}, \"kinds\": {}, \"sections\": [",
        s.tracks(),
        s.kinds()
    );
    for (i, x) in s.list().iter().enumerate() {
        if i > 0 {
            j.push(',');
        }
        let _ = write!(
            j,
            "\n{{\"start\": {:.3}, \"end\": {:.3}, \"first_bar\": {}, \"bars\": {}, \"label\": {}, \"letter\": {}, \
             \"track\": {}, \"track_start\": {}, \"strength\": {:.3}, \"zone\": [{:.3}, {:.3}]}}",
            x.start,
            x.end,
            x.first_bar,
            x.bars,
            x.label,
            x.letter,
            x.track,
            x.track_start,
            x.strength,
            x.zone.0,
            x.zone.1
        );
    }
    j.push_str("]}");
    j
}
