//! Mutated, truncated and random sound files end in an error, silence or a shorter sound,
//! never in a panic or a hang, and never allocate more than the cap (every test binary fed
//! hostile input gets a capping global allocator).

// The allocator below is the only unsafe code: a GlobalAlloc must be an `unsafe impl`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live bytes allowed in this test process.
const CAP: usize = 256 << 20;
/// Most bytes one small hostile file may make the decoder take (the whole-file limit is 32 MiB).
const CASE_CAP: usize = 64 << 20;

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

#[cfg(test)]
mod fuzz {
    use super::*;

    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use penumbra_audio::SoundFile;
    use penumbra_audio::file::{Frame, WavFormat, wav_bytes};

    /// xorshift64*, deterministic.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut b = seed.to_vec();
        match rng.below(5) {
            // Flip some bytes, more often near the start where the headers are.
            0 | 1 => {
                for _ in 0..1 + rng.below(8) {
                    let at = if rng.below(2) == 0 { rng.below(b.len().min(256)) } else { rng.below(b.len()) };
                    if let Some(x) = b.get_mut(at) {
                        *x = rng.next() as u8;
                    }
                }
            }
            // Cut it short.
            2 => b.truncate(rng.below(b.len())),
            // A big-endian 32-bit number set to an extreme.
            3 => {
                let at = rng.below(b.len().min(512).saturating_sub(4));
                let v: u32 = [0, 1, 0x7FFF_FFFF, 0xFFFF_FFFF, 0x8000_0000][rng.below(5)];
                if let Some(s) = b.get_mut(at..at + 4) {
                    s.copy_from_slice(&v.to_be_bytes());
                }
            }
            // Random bytes over a stretch.
            _ => {
                let at = rng.below(b.len());
                let n = rng.below(64).min(b.len() - at);
                for x in &mut b[at..at + n] {
                    *x = rng.next() as u8;
                }
            }
        }
        b
    }

    /// Opens `path` whole and streamed, waits for it and reads some of it, looping and not.
    fn play(path: &Path) {
        for memory in [32 << 20, 0] {
            let mut f = SoundFile::open_with(path, memory, None);
            let t = Instant::now();
            loop {
                let s = f.status();
                if s.ready || s.error.is_some() {
                    break;
                }
                assert!(t.elapsed() < Duration::from_secs(20), "{}: hangs opening", path.display());
                std::thread::sleep(Duration::from_millis(1));
            }
            let (mut out, mut found) = (Vec::new(), Vec::new());
            for (from, looping) in [(0, false), (1000, true), (-50, true), (1 << 40, true), (30_000, false)] {
                f.read(from, 2000, looping, 0, &mut out, &mut found);
                assert!(out.iter().all(|v| v.is_finite()));
                assert_eq!(found.len(), 2000);
            }
            std::thread::sleep(Duration::from_millis(2));
            f.read(0, 2000, true, 0, &mut out, &mut found);
            assert!(
                !found.is_empty() || f.status().error.is_some() || found.iter().all(|x| *x != Frame::Data)
            );
        }
    }

    /// The Ogg page checksum: CRC-32, polynomial 0x04C11DB7, not reflected, starting at 0.
    fn ogg_crc(data: &[u8]) -> u32 {
        let mut crc = 0u32;
        for &b in data {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            }
        }
        crc
    }

    /// Sets the checksum of every page that can still be found.
    fn fix_ogg_checksums(b: &mut [u8]) {
        let mut at = 0usize;
        while at + 27 <= b.len() {
            if &b[at..at + 4] != b"OggS" {
                at += 1;
                continue;
            }
            let n = b[at + 26] as usize;
            if at + 27 + n > b.len() {
                break;
            }
            let len: usize = b[at + 27..at + 27 + n].iter().map(|&l| l as usize).sum();
            let end = (at + 27 + n + len).min(b.len());
            b[at + 22..at + 26].fill(0);
            let crc = ogg_crc(&b[at..end]);
            b[at + 22..at + 26].copy_from_slice(&crc.to_le_bytes());
            at = end;
        }
    }

    #[test]
    fn the_checksum_matches_a_real_file() {
        let mut ogg = fixture("tone1k_44k.ogg");
        let before = ogg.clone();
        fix_ogg_checksums(&mut ogg);
        assert_eq!(ogg, before);
    }

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("penumbra-hostile-sound-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mutated_sound_files_fail_softly_within_the_cap() {
        let dir = dir();
        let tone: Vec<f64> = (0..9600).map(|i| (i as f64 * 0.05).sin() * 0.5).collect();
        let seeds: Vec<(&str, Vec<u8>)> = vec![
            ("wav", wav_bytes(48_000, 2, WavFormat::Pcm16, &tone)),
            ("wav", wav_bytes(44_100, 1, WavFormat::Float32, &tone)),
            ("flac", fixture("tone1k_44k.flac")),
            ("mp3", fixture("tone1k_48k.mp3")),
            ("ogg", fixture("tone1k_44k.ogg")),
            ("m4a", fixture("tone1k_48k.m4a")),
            ("mp4", fixture("bars_tone.mp4")),
            ("aiff", fixture("short.aiff")),
            // Ogg again, its pages' checksums made right after the mutation, so symphonia reads
            // the broken headers instead of dropping the pages.
            ("ogg+crc", fixture("tone1k_44k.ogg")),
            // A file under another format's name: the content decides.
            ("mp3", fixture("tone1k_44k.ogg")),
            ("wav", fixture("tone1k_48k.m4a")),
        ];
        // `PN_FUZZ_SEED` and `PN_FUZZ_ROUNDS` explore further by hand.
        let seed = std::env::var("PN_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5EED_50D5);
        let rounds = std::env::var("PN_FUZZ_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
        let mut rng = Rng(seed);
        let mut worst = 0usize;
        for (i, (ext, seed)) in seeds.iter().enumerate() {
            for r in 0..rounds {
                let mut bytes = mutate(seed, &mut rng);
                let ext = if *ext == "ogg+crc" {
                    fix_ogg_checksums(&mut bytes);
                    "ogg"
                } else {
                    ext
                };
                let path = dir.join(format!("m{i}_{r}.{ext}"));
                std::fs::write(&path, &bytes).unwrap();
                let before = LIVE.load(Ordering::Relaxed);
                PEAK.store(before, Ordering::Relaxed);
                play(&path);
                let grew = PEAK.load(Ordering::Relaxed).saturating_sub(before);
                worst = worst.max(grew);
                assert!(grew < CASE_CAP, "{}: {} MiB", path.display(), grew >> 20);
            }
        }
        // Random bytes under every extension.
        for (r, ext) in ["wav", "flac", "mp3", "ogg", "m4a", "mp4", "mov", "aac"].iter().enumerate() {
            let n = 16 + rng.below(4096);
            let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            let path = dir.join(format!("random{r}.{ext}"));
            std::fs::write(&path, &bytes).unwrap();
            play(&path);
        }
        eprintln!("most memory one file took: {} MiB", worst >> 20);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
