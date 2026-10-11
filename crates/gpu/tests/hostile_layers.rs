//! Truncated, flipped and patched PSD files end in errors or pictures, never in a panic, and
//! never allocate past the cap (a capping global allocator for every test binary fed hostile
//! input).

// The allocator below is the only unsafe code: a GlobalAlloc must be an `unsafe impl`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

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

#[cfg(test)]
mod fuzz {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use penumbra_gpu::layers::Source;
    use photocraft_psd::builder::{GroupSpec, LayerSpec, MaskSpec, PixelData, PsdBuilder};
    use photocraft_psd::layer::Rect;

    use super::*;

    fn seed(rle: bool) -> Vec<u8> {
        let solid = |w: u32, h: u32, c: [u8; 4]| PixelData::Rgba8((0..w * h).flat_map(|_| c).collect());
        let mut b = PsdBuilder::new(48, 32);
        if !rle {
            b = b.compression(photocraft_psd::Compression::Raw);
        }
        b.push_layer(LayerSpec::new("bg", 0, 0, 48, 32, solid(48, 32, [1, 2, 3, 255])));
        b.begin_group(GroupSpec::new("g"));
        let mut l = LayerSpec::new("masked", 4, 4, 20, 10, solid(20, 10, [200, 0, 0, 200]));
        l.mask = Some(MaskSpec {
            rect: Rect::from_xywh(4, 4, 20, 10),
            data: vec![128; 200],
            default_color: 0,
            disabled: false,
        });
        b.push_layer(l);
        b.end_group().unwrap();
        b.push_layer(LayerSpec::new("off", -10, 20, 30, 30, solid(30, 30, [9, 9, 9, 255])));
        b.composite(solid(48, 32, [50, 60, 70, 255]));
        b.to_bytes().unwrap()
    }

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

    fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut d = seed.to_vec();
        match rng.below(4) {
            0 => d.truncate(rng.below(d.len())),
            1 => {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(d.len());
                    d[i] ^= 1 << rng.below(8);
                }
            }
            2 => {
                let v: u32 = [u32::MAX, 0x7FFF_FFFF, 300_000, 65_535, 0][rng.below(5)];
                let i = rng.below(d.len().saturating_sub(4));
                d[i..i + 4].copy_from_slice(&v.to_be_bytes());
            }
            _ => {
                let keep = rng.below(d.len().min(512));
                d.truncate(keep);
                for _ in 0..rng.below(2048) {
                    d.push(rng.next() as u8);
                }
            }
        }
        d
    }

    #[test]
    fn mutated_psds_end_in_errors_or_pictures() {
        let cases: usize =
            std::env::var("PENUMBRA_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
        let mut rng = Rng(0xC0FF_EE00_1234_5678);
        let (mut pictures, mut errors) = (0usize, 0usize);
        for rle in [true, false] {
            let seed = seed(rle);
            let s = Source::psd(&seed, 4096).unwrap();
            assert_eq!(s.layers().len(), 4);
            for case in 0..cases {
                let data = mutate(&seed, &mut rng);
                let run = || -> usize {
                    let Ok(s) = Source::psd(&data, 4096) else { return 0 };
                    let mut ok = usize::from(s.merged(4096).is_ok());
                    for l in s.layers().iter().filter(|l| !l.group) {
                        ok += usize::from(s.layer(l, true, 4096).is_ok());
                        ok += usize::from(s.layer(l, false, 4096).is_ok());
                    }
                    ok
                };
                match catch_unwind(AssertUnwindSafe(run)) {
                    Ok(0) => errors += 1,
                    Ok(n) => pictures += n,
                    Err(_) => panic!("case {case} (rle {rle}) panicked"),
                }
            }
        }
        let peak = PEAK.load(Ordering::Relaxed);
        eprintln!("{pictures} pictures, {errors} files refused, peak {} MiB", peak >> 20);
        assert!(errors > 100);
        assert!(peak < 256 << 20, "peak {} MiB", peak >> 20);
    }
}
