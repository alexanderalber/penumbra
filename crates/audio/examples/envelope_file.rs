//! Measures the envelopes of a sound file ([`beats::analyze_envelopes`]): time, hops, memory
//! and the size of the cache file.
//!
//! `cargo run -p penumbra-audio --release --example envelope_file -- SOUND [CACHE_DIR]`
//!
//! The cache goes to `CACHE_DIR`, by default a fresh directory in the system's temp directory
//! (removed afterwards). Live heap bytes are capped at 1 GiB, so a runaway cannot take the
//! machine down; the peak is printed.

// The allocator below is the only unsafe code: a GlobalAlloc must be an `unsafe impl`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use penumbra_audio::beats;

/// Live bytes allowed in this process.
const CAP: usize = 1 << 30;

struct Capped;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let now = LIVE.fetch_add(size, Ordering::Relaxed) + size;
        if now > CAP {
            LIVE.fetch_sub(size, Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Capped = Capped;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [sound] => run(Path::new(sound), None),
        [sound, cache] => run(Path::new(sound), Some(PathBuf::from(cache))),
        _ => Err("usage: envelope_file SOUND [CACHE_DIR]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(sound: &Path, cache: Option<PathBuf>) -> Result<(), String> {
    let temp = cache.is_none();
    let cache = cache
        .unwrap_or_else(|| std::env::temp_dir().join(format!("penumbra-envelopes-{}", std::process::id())));
    let start = Instant::now();
    let mut partials = 0usize;
    let env = beats::analyze_envelopes(sound, Some(&cache), None, &mut |_| {}, &mut |_| partials += 1)
        .map_err(|e| e.to_string())?;
    let took = start.elapsed();
    let seconds = env.len() as f64 * env.hop_seconds();
    println!(
        "{:.1} s of sound in {:.2?} ({:.0}x real time), {} hops of {} samples, {} partial results",
        seconds,
        took,
        seconds / took.as_secs_f64().max(1e-9),
        env.len(),
        env.hop(),
        partials
    );
    println!(
        "envelopes {:.1} MB in memory, heap peak {:.1} MB",
        env.bytes() as f64 / 1e6,
        PEAK.load(Ordering::Relaxed) as f64 / 1e6
    );
    let size: u64 = std::fs::read_dir(&cache)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.len())
        .sum();
    println!("cache {:.1} MB in {}", size as f64 / 1e6, cache.display());
    let start = Instant::now();
    let again = beats::analyze_envelopes(sound, Some(&cache), None, &mut |_| {}, &mut |_| {})
        .map_err(|e| e.to_string())?;
    println!("read from the cache in {:.2?}, same: {}", start.elapsed(), again == env);
    if temp {
        let _ = std::fs::remove_dir_all(&cache);
    }
    Ok(())
}
