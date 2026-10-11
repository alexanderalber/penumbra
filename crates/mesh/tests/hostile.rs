//! Truncated, flipped and patched mesh files end in an error or a mesh, never in a panic, and
//! never allocate more than the cap (every test binary fed hostile input gets a capping global
//! allocator, after a spike once took the PC down).

// The allocator below is the only unsafe code: a GlobalAlloc must be an `unsafe impl`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live bytes allowed in this test process. A file that tries more aborts the test (the
/// allocation fails) instead of exhausting the machine.
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

    use penumbra_mesh::write::{combine, to_glb, to_obj, to_stl};
    use penumbra_mesh::{Error, Limits, Material, Mesh, parse, prim};

    use super::*;

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

    fn scene() -> Mesh {
        let mut b = prim::cuboid([1.0, 1.0, 1.0]).unwrap();
        b.materials = vec![Material::plain("red", [0.8, 0.1, 0.1, 1.0])];
        combine(&[b, prim::moved(prim::sphere(0.5, 8).unwrap(), [1.5, 0.0, 0.0])])
    }

    fn sidecar(r: &str) -> Result<Vec<u8>, Error> {
        match r {
            "m.mtl" => Ok(b"newmtl red\nKd 1 0 0\nmap_Kd t.png\n".to_vec()),
            "t.png" => Ok(b"\x89PNG not really".to_vec()),
            _ => Err(Error::new("no such file")),
        }
    }

    fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut d = seed.to_vec();
        match rng.below(6) {
            0 => d.truncate(rng.below(d.len())),
            1 => {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(d.len());
                    if let Some(b) = d.get_mut(i) {
                        *b ^= 1 << rng.below(8);
                    }
                }
            }
            2 => {
                // Four bytes of a large or small number, little endian, anywhere.
                let v: u32 =
                    [u32::MAX, 0x7FFF_FFFF, 1_000_000_000, 0, 0x7F80_0000, 0xFFC0_0000][rng.below(6)];
                let i = rng.below(d.len().saturating_sub(4));
                if let Some(s) = d.get_mut(i..i + 4) {
                    s.copy_from_slice(&v.to_le_bytes());
                }
            }
            3 => {
                // A digit run in text replaced by a huge or negative number.
                let starts: Vec<usize> =
                    (1..d.len()).filter(|&i| d[i].is_ascii_digit() && !d[i - 1].is_ascii_digit()).collect();
                if !starts.is_empty() {
                    let at = starts[rng.below(starts.len())];
                    let end = (at..d.len()).find(|&i| !d[i].is_ascii_digit()).unwrap_or(d.len());
                    let with: &[u8] =
                        [&b"18446744073709551615"[..], b"4294967295", b"-1", b"1e308", b"999999999"]
                            [rng.below(5)];
                    d.splice(at..end, with.iter().copied());
                }
            }
            4 => {
                // The head kept, random bytes after it.
                let keep = rng.below(d.len().min(256));
                d.truncate(keep);
                for _ in 0..rng.below(4096) {
                    d.push(rng.next() as u8);
                }
            }
            _ => {
                // A slice duplicated.
                let a = rng.below(d.len());
                let b = (a + rng.below(512)).min(d.len());
                let copy = d[a..b].to_vec();
                let at = rng.below(d.len());
                d.splice(at..at, copy);
            }
        }
        d
    }

    #[test]
    fn mutated_files_end_in_errors_or_meshes() {
        let s = scene();
        let gltf_json = {
            // The .glb's JSON with the buffer inline as a data URI.
            let glb = to_glb(&s);
            let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
            let json = std::str::from_utf8(&glb[20..20 + json_len]).unwrap().trim_end().to_string();
            let bin = &glb[20 + json_len + 8..];
            let b64: String = {
                const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut out = String::new();
                for c in bin.chunks(3) {
                    let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
                    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                    for k in 0..4 {
                        out.push(if k <= c.len() {
                            A[((n >> (18 - 6 * k)) & 63) as usize] as char
                        } else {
                            '='
                        });
                    }
                }
                out
            };
            json.replace(
                &format!(r#""buffers":[{{"byteLength":{}}}]"#, bin.len()),
                &format!(r#""buffers":[{{"byteLength":{},"uri":"data:;base64,{b64}"}}]"#, bin.len()),
            )
            .into_bytes()
        };
        let ascii_stl = {
            let mut t = String::from("solid s\n");
            for tri in s.indices.chunks(3).take(40) {
                t.push_str("facet normal 0 0 1\nouter loop\n");
                for i in tri {
                    let p = s.vertices[*i as usize].position;
                    t.push_str(&format!("vertex {} {} {}\n", p[0], p[1], p[2]));
                }
                t.push_str("endloop\nendfacet\n");
            }
            t.push_str("endsolid s\n");
            t.into_bytes()
        };
        let seeds: Vec<(&str, Vec<u8>)> = vec![
            ("s.obj", to_obj(&s, Some("m.mtl")).into_bytes()),
            ("s.stl", to_stl(&s)),
            ("a.stl", ascii_stl),
            ("s.glb", to_glb(&s)),
            ("s.gltf", gltf_json),
        ];
        let limits = Limits::default();
        let mut rng = Rng(0x5EED_1234_ABCD_0001);
        // More cases for a one-off search: PENUMBRA_FUZZ_CASES=50000.
        let cases: usize =
            std::env::var("PENUMBRA_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
        let (mut ok, mut errors) = (0usize, 0usize);
        for (name, seed) in &seeds {
            // The seed itself loads.
            parse(name, seed, &sidecar, &limits).unwrap_or_else(|e| panic!("{name}: {e}"));
            for case in 0..cases {
                let data = mutate(seed, &mut rng);
                match catch_unwind(AssertUnwindSafe(|| parse(name, &data, &sidecar, &limits))) {
                    Ok(Ok(l)) => {
                        l.mesh
                            .check()
                            .unwrap_or_else(|e| panic!("{name} case {case}: inconsistent mesh: {e}"));
                        ok += 1;
                    }
                    Ok(Err(_)) => errors += 1,
                    Err(_) => panic!("{name} case {case} panicked"),
                }
            }
        }
        let peak = PEAK.load(Ordering::Relaxed);
        eprintln!("{ok} loaded, {errors} errors, peak {} MiB", peak >> 20);
        assert!(errors > 1000, "the mutations should break many files");
        assert!(peak < 256 << 20, "peak {} MiB", peak >> 20);
    }
}
