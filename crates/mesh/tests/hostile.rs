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
            ("s.fbx", penumbra_fbx::write::to_bytes(7400, &penumbra_fbx::write::example_scene(), true)),
            ("w.fbx", penumbra_fbx::write::to_bytes(7500, &penumbra_fbx::write::example_scene(), false)),
            ("m.fbx", penumbra_fbx::write::to_bytes(7400, &penumbra_fbx::write::animated_scene(), false)),
            ("k.fbx", penumbra_fbx::write::to_bytes(7400, &penumbra_fbx::write::skinned_scene(), false)),
            (
                "a.fbx",
                penumbra_fbx::write::to_ascii(7400, &penumbra_fbx::write::example_scene()).into_bytes(),
            ),
            (
                "n.fbx",
                penumbra_fbx::write::to_ascii(7700, &penumbra_fbx::write::animated_scene()).into_bytes(),
            ),
            (
                "j.fbx",
                penumbra_fbx::write::to_ascii(7400, &penumbra_fbx::write::skinned_scene()).into_bytes(),
            ),
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
                        // A motion poses at any time without trouble.
                        if let Some(m) = &l.motion {
                            let mut out = Vec::new();
                            for t in [-1.0, 0.7, f64::NAN, 1e30] {
                                m.deform(&l.mesh.vertices, 0, t, &mut out);
                                assert_eq!(out.len(), l.mesh.vertices.len());
                            }
                        }
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

    /// Crafted FBX files: each is refused or read, cheaply.
    #[test]
    fn crafted_fbx_files_are_refused_cheaply() {
        use penumbra_fbx::write::{connection, object, to_bytes};
        use penumbra_fbx::{Array, Node, Value};
        let limits = Limits::default();
        let load = |b: &[u8]| {
            catch_unwind(AssertUnwindSafe(|| parse("x.fbx", b, &sidecar, &limits))).expect("no panic")
        };
        let base = |extra: Vec<Node>, conns: Vec<Node>| {
            let mut nodes = penumbra_fbx::write::example_scene();
            nodes.iter_mut().find(|n| n.name == "Objects").unwrap().children.extend(extra);
            nodes.iter_mut().find(|n| n.name == "Connections").unwrap().children.extend(conns);
            nodes
        };
        // An array claiming 4 Gi doubles behind a few bytes of zlib.
        let mut huge =
            to_bytes(7400, &[Node::new("A", vec![Value::Array(Array::F64(vec![0.0; 8]))], Vec::new())], true);
        let at = 27 + huge[27..].iter().position(|b| *b == b'd').unwrap();
        huge[at + 1..at + 5].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(load(&huge).is_err());
        // Records nested a thousand deep (the writer and the drop recurse, so not more).
        let mut deep = Node::new("x", Vec::new(), Vec::new());
        for _ in 0..1000 {
            deep = Node::new("x", Vec::new(), vec![deep]);
        }
        assert!(load(&to_bytes(7500, &[deep], false)).is_err());
        // Models that are each other's parents: read, with a warning.
        let cyc = base(Vec::new(), vec![connection("OO", 10, 11, None)]);
        let l = load(&to_bytes(7400, &cyc, true)).unwrap();
        assert!(l.warnings.iter().any(|w| w.contains("ancestor")), "{:?}", l.warnings);
        // A geometry whose corners point far outside its points, and one with a billion corners
        // claimed.
        let mut g = penumbra_fbx::write::cube_geometry(22, "bad");
        g.children.iter_mut().find(|c| c.name == "PolygonVertexIndex").unwrap().values[0] =
            Value::Array(Array::I32(vec![0, 1, i32::MAX, -1_000_000_000, 0, 1, -3]));
        let m = object("Model", 12, "bad", "Mesh", Vec::new(), Vec::new());
        let bad = base(vec![m, g], vec![connection("OO", 12, 0, None), connection("OO", 22, 12, None)]);
        let l = load(&to_bytes(7400, &bad, true)).unwrap();
        assert_eq!(l.mesh.parts, ["base", "top", "bad"]);
        assert!(l.warnings.iter().any(|w| w.contains("outside")), "{:?}", l.warnings);
        // Truncated at every byte.
        let full = to_bytes(7400, &bad, true);
        for n in 0..full.len() {
            if let Ok(l) = load(&full[..n]) {
                l.mesh.check().unwrap();
            }
        }
        // ASCII: a claimed length of 2^64 - 1 behind two elements, a length too large for any
        // number, an unterminated string and brace, a thousand open braces, garbage numbers.
        let head = "; FBX 7.4.0 project file\n";
        let ascii = penumbra_fbx::write::to_ascii(7400, &bad);
        let huge = ascii.replacen("*24 {", "*18446744073709551615 {", 1);
        assert_ne!(huge, ascii);
        assert!(load(huge.as_bytes()).is_ok());
        assert!(load(ascii.replacen("*24 {", "*99999999999999999999999 {", 1).as_bytes()).is_err());
        for body in [
            "Objects: {\n Model: 1, \"Model::open",
            "Objects: {\n Model: 1, \"Model::a\", \"Mesh\" {\n",
            &"A: {".repeat(1000),
            "A: *3 { a: 1, 2.5.5, 3 }",
            "A: 1e999999, --4, 0x10",
            "A: \u{0}\u{ff}",
        ] {
            assert!(load(format!("{head}{body}").as_bytes()).is_err(), "{body:.40}");
        }
        // Every truncation of the ASCII file.
        for n in 0..ascii.len() {
            if let Ok(l) = load(&ascii.as_bytes()[..n]) {
                l.mesh.check().unwrap();
            }
        }
        let peak = PEAK.load(Ordering::Relaxed);
        assert!(peak < 256 << 20, "peak {} MiB", peak >> 20);
    }
}
