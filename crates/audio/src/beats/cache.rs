//! Beat results kept on disk: one small text file per sound file in a cache directory, named
//! by a hash of the sound file's full path.
//!
//! The file's first line is its key: a format name and version, the sound file's size and
//! modification time and the model's id. The second line holds the counts of beats and
//! downbeats, then one time per line, beats first, and a last line `end`. A file that is too
//! large, not text, has another key, wrong counts, times out of order, no `end` or anything
//! after it is ignored, and the result is computed again.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::Beats;
use super::analyze::{MAX_SECONDS, fnv1a};

/// The first word of a cache file.
const MAGIC: &str = "penumbra-beats";
/// Raised whenever the front end, the chunking or the peak picking changes what they give.
const VERSION: u32 = 1;
/// The largest cache file read. A six-hour mix at 200 beats per minute takes under 1 MB.
const MAX_BYTES: u64 = 8 << 20;
/// The most beats a cache file may hold.
const MAX_TIMES: usize = 1 << 20;

/// Where the result for one sound file and model is kept, and the key it must carry.
pub(super) struct Key {
    file: PathBuf,
    header: String,
}

impl Key {
    /// The key of `sound` with `model_id` in `dir`. `None` when the file's size or
    /// modification time cannot be read: then there is no caching.
    pub(super) fn of(sound: &Path, model_id: &str, dir: &Path) -> Option<Key> {
        let meta = fs::metadata(sound).ok()?;
        let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        let full = fs::canonicalize(sound).unwrap_or_else(|_| sound.to_path_buf());
        let name = format!("{:016x}.beats", fnv1a(full.as_os_str().as_encoded_bytes()));
        let model: String = model_id.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
        let header = format!("{MAGIC} {VERSION} {} {mtime} {model}", meta.len());
        Some(Key { file: dir.join(name), header })
    }
}

/// The cached result, `None` when there is none that fits the key.
pub(super) fn load(key: &Key) -> Option<Beats> {
    let file = fs::File::open(&key.file).ok()?;
    let mut text = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > MAX_BYTES {
        return None;
    }
    parse(&text, &key.header)
}

fn parse(text: &str, header: &str) -> Option<Beats> {
    let mut lines = text.lines();
    if lines.next()? != header {
        return None;
    }
    let mut counts = lines.next()?.split(' ');
    let beats: usize = counts.next()?.parse().ok()?;
    let downbeats: usize = counts.next()?.parse().ok()?;
    // Downbeats sit on beats, unless there are no beats at all.
    let too_many = downbeats > if beats == 0 { MAX_TIMES } else { beats };
    if counts.next().is_some() || beats > MAX_TIMES || too_many {
        return None;
    }
    let mut times = |n: usize| -> Option<Vec<f64>> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let t: f64 = lines.next()?.parse().ok()?;
            let ascending = out.last().is_none_or(|p| t > *p);
            if !(t.is_finite() && (0.0..=MAX_SECONDS).contains(&t) && ascending) {
                return None;
            }
            out.push(t);
        }
        Some(out)
    };
    let beats = times(beats)?;
    let downbeats = times(downbeats)?;
    if lines.next()? != "end" || lines.any(|l| !l.is_empty()) {
        return None;
    }
    Some(Beats { beats, downbeats })
}

fn format(beats: &Beats, header: &str) -> String {
    let mut out = format!("{header}\n{} {}\n", beats.beats.len(), beats.downbeats.len());
    for t in beats.beats.iter().chain(&beats.downbeats) {
        // `{}` of an f64 is the shortest text that reads back as the same number.
        out.push_str(&format!("{t}\n"));
    }
    out.push_str("end\n");
    out
}

/// Writes the result, through a temporary file so a reader never sees half of it.
pub(super) fn store(key: &Key, beats: &Beats) -> Result<(), String> {
    let dir = key.file.parent().ok_or("the cache file has no directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = key.file.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, format(beats, &key.header))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &key.file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write {}: {e}", key.file.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "penumbra-beats 1 100 5 m-0123";

    fn sample() -> Beats {
        Beats { beats: vec![0.12, 1.0 / 3.0, 0.5, 7200.98], downbeats: vec![0.12, 7200.98] }
    }

    #[test]
    fn round_trips_exactly() {
        let b = sample();
        assert_eq!(parse(&format(&b, HEADER), HEADER), Some(b));
        let empty = Beats::default();
        assert_eq!(parse(&format(&empty, HEADER), HEADER), Some(empty));
    }

    #[test]
    fn rejects_other_keys_and_corrupt_content() {
        let good = format(&sample(), HEADER);
        assert!(parse(&good, "penumbra-beats 1 100 6 m-0123").is_none());
        // Cut anywhere before the last line ends.
        for cut in 0..good.len() - 1 {
            if let Some(b) = good.get(..cut) {
                assert!(parse(b, HEADER).is_none(), "cut at {cut}");
            }
        }
        let bad = [
            good.replacen("\n4 2\n", "\n5 2\n", 1),
            good.replacen("\n4 2\n", "\n4 5\n", 1),
            good.replacen("\n4 2\n", "\n4 2 1\n", 1),
            good.replacen("\n4 2\n", "\n-4 2\n", 1),
            good.replacen("\n4 2\n", "\n99999999999999999999 2\n", 1),
            good.replacen("0.5\n", "NaN\n", 1),
            good.replacen("0.5\n", "inf\n", 1),
            good.replacen("0.5\n", "-0.5\n", 1),
            good.replacen("0.5\n", "0.1\n", 1),
            good.replacen("0.5\n", "1e9\n", 1),
            good.replacen("0.5\n", "0.5 x\n", 1),
            format!("{good}1.0\n"),
            good.replacen("end\n", "", 1),
            good.replacen("end\n", "end\nend\n", 1),
            format!("{good}\u{0}"),
        ];
        for b in bad {
            assert!(parse(&b, HEADER).is_none(), "{b:?}");
        }
    }

    #[test]
    fn stores_and_loads_beside_a_sound_file() {
        let dir = std::env::temp_dir().join(format!("penumbra-beat-cache-{}", std::process::id()));
        let sound = dir.join("a.wav");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&sound, b"not really sound").unwrap();
        let cache = dir.join("cache");
        let key = Key::of(&sound, "model one", &cache).unwrap();
        assert!(load(&key).is_none());
        store(&key, &sample()).unwrap();
        assert_eq!(load(&key), Some(sample()));
        // Another model or another file content misses.
        assert!(load(&Key::of(&sound, "model two", &cache).unwrap()).is_none());
        fs::write(&sound, b"other sound, longer").unwrap();
        assert!(load(&Key::of(&sound, "model one", &cache).unwrap()).is_none());
        // A corrupt file is ignored.
        let key = Key::of(&sound, "model one", &cache).unwrap();
        fs::write(&key.file, b"\xff\xfe garbage").unwrap();
        assert!(load(&key).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
