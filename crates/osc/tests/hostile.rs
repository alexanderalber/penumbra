//! Mutated, truncated and random OSC packets and patterns end in an error or a result, never in
//! a panic, and never allocate more than the cap (every test binary fed hostile input gets a
//! capping global allocator).

// The allocator below is the only unsafe code: a GlobalAlloc must be an `unsafe impl`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Live bytes allowed in this test process.
const CAP: usize = 256 << 20;

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

    use penumbra_osc::codec::{self, Arg, Limits, Message, message_packet, packets};
    use penumbra_osc::pattern::{Budget, Pattern};

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

    fn seeds() -> Vec<Vec<u8>> {
        let m = Message::new(
            "/scene/speed",
            vec![
                Arg::Float(0.5),
                Arg::Int(3),
                Arg::Str("text".into()),
                Arg::Blob(vec![1, 2, 3]),
                Arg::Long(7),
                Arg::Double(0.25),
                Arg::True,
                Arg::Midi([1, 2, 3, 4]),
            ],
        );
        let many: Vec<Message> =
            (0..20).map(|i| Message::new(format!("/ch/{i}"), vec![Arg::Float(i as f32)])).collect();
        let mut nested = packets(&many[..3], 1400, true).remove(0);
        for _ in 0..3 {
            let mut b = b"#bundle\0".to_vec();
            b.extend_from_slice(&codec::IMMEDIATE.to_be_bytes());
            b.extend_from_slice(&(nested.len() as i32).to_be_bytes());
            b.extend_from_slice(&nested);
            nested = b;
        }
        let mut arrays = b"/arr\0\0\0\0,[i[f]s]\0\0\0\0".to_vec();
        arrays.extend_from_slice(&1i32.to_be_bytes());
        arrays.extend_from_slice(&1f32.to_bits().to_be_bytes());
        arrays.extend_from_slice(b"ab\0\0");
        vec![message_packet(&m), packets(&many, 1400, true).remove(0), nested, arrays]
    }

    fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
        let mut d = seed.to_vec();
        match rng.below(6) {
            0 => d.truncate(rng.below(d.len() + 1)),
            1 => {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(d.len());
                    if let Some(b) = d.get_mut(i) {
                        *b ^= 1 << rng.below(8);
                    }
                }
            }
            2 => {
                // A size or count field replaced by a large or negative number.
                let v: u32 = [u32::MAX, 0x7FFF_FFFF, 0x8000_0000, 65_536, 0, 3][rng.below(6)];
                let i = rng.below(d.len().saturating_sub(4)) & !3;
                if let Some(s) = d.get_mut(i..i + 4) {
                    s.copy_from_slice(&v.to_be_bytes());
                }
            }
            3 => {
                // Type tags replaced by random tag characters.
                if let Some(comma) = d.iter().position(|b| *b == b',') {
                    let tags = b"ifsSbhtdcrmTFNI[]?";
                    for k in 1..1 + rng.below(12) {
                        if let Some(b) = d.get_mut(comma + k)
                            && *b != 0
                        {
                            *b = tags[rng.below(tags.len())];
                        }
                    }
                }
            }
            4 => {
                let keep = rng.below(d.len().min(64));
                d.truncate(keep);
                for _ in 0..rng.below(2048) {
                    d.push(rng.next() as u8);
                }
            }
            _ => {
                let a = rng.below(d.len());
                let b = (a + rng.below(256)).min(d.len());
                let copy = d[a..b].to_vec();
                let at = rng.below(d.len());
                d.splice(at..at, copy);
            }
        }
        d
    }

    #[test]
    fn mutated_packets_end_in_errors_or_messages() {
        let seeds = seeds();
        let limits = Limits::default();
        for s in &seeds {
            assert!(codec::parse(s, &limits).is_ok(), "seed parses");
        }
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut ok, mut err) = (0u32, 0u32);
        for round in 0..300_000 {
            let seed = &seeds[round % seeds.len()];
            let data = if round % 50 == 0 {
                (0..rng.below(4096)).map(|_| rng.next() as u8).collect()
            } else {
                mutate(seed, &mut rng)
            };
            match codec::parse(&data, &limits) {
                Ok(p) => {
                    ok += 1;
                    assert!(p.messages.len() <= limits.max_messages);
                    // Whatever parsed encodes and parses back the same.
                    for (_, m) in &p.messages {
                        if m.args.iter().all(|a| !matches!(a, Arg::Float(f) if f.is_nan()))
                            && m.args.iter().all(|a| !matches!(a, Arg::Double(f) if f.is_nan()))
                        {
                            let again = codec::parse(&message_packet(m), &limits).unwrap();
                            assert_eq!(&again.messages[0].1, m);
                        }
                    }
                }
                Err(_) => err += 1,
            }
        }
        assert!(ok > 1000 && err > 1000, "ok {ok}, err {err}");
        assert!(PEAK.load(Ordering::Relaxed) < 64 << 20, "peak {}", PEAK.load(Ordering::Relaxed));
    }

    #[test]
    fn random_patterns_compile_or_refuse_and_stay_in_budget() {
        let mut rng = Rng(42);
        let alphabet = b"ab/*?[]{},!-xyz01";
        for _ in 0..50_000 {
            let len = rng.below(80);
            let p: String = std::iter::once('/')
                .chain((0..len).map(|_| char::from(alphabet[rng.below(alphabet.len())])))
                .collect();
            let name: String = std::iter::once('/')
                .chain((0..rng.below(80)).map(|_| char::from(b"ab/xyz01"[rng.below(8)])))
                .collect();
            if let Ok(pat) = Pattern::compile(&p) {
                let mut budget = Budget(10_000);
                let _ = pat.matches(&name, &mut budget);
            }
        }
    }
}
