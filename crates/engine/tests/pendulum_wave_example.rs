//! The pendulum-wave example's sound, rendered offline: an ignored test that cooks the
//! example's `/mix` for 20 s and writes it as a WAV to listen to.
//!
//! Run with `cargo test -p penumbra-engine --test pendulum_wave_example -- --ignored`. The WAV
//! goes to `PENUMBRA_LISTEN_DIR`, else to the system's temp directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use penumbra_core::{Chans, LatePolicy, NodePath, Rate, ShowClock};
use penumbra_graph::{Cooker, JobPool, Network, Params, Port};
use penumbra_project::{Content, FILE_NAME};

const SECONDS: usize = 20;

#[test]
#[ignore]
fn write_the_pendulum_wave() {
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/pendulum-wave");
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    let content = penumbra_project::parse(&text, &types).unwrap();
    let ops = penumbra_project::diff(&Content::default(), &content);
    let net = Arc::new(Network::new().apply_with(&types, &ops, Params::Keep).unwrap().network);
    let mix = net.resolve(&NodePath::parse("/mix").unwrap()).unwrap();
    let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
    let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
    let mut all = Vec::new();
    for _ in 0..SECONDS * 60 {
        cooker.cook(&net, clock.now(), &[mix], &[]);
        assert_eq!(cooker.status(mix).and_then(|s| s.error), None);
        let block = cooker.output(Port { node: mix, output: 0 }).unwrap().get::<Chans>().unwrap();
        all.extend_from_slice(&block.channels()[0].samples);
        clock.advance(1);
    }
    let peak = all.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
    eprintln!("peak {:.1} dBFS over {SECONDS} s", 20.0 * peak.max(1e-12).log10());
    assert!(peak > 0.05 && peak < 0.9, "peak {peak}");
    let out = std::env::var_os("PENUMBRA_LISTEN_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let file = out.join("pendulum_example.wav");
    write_wav(&file, &all).unwrap();
    eprintln!("wrote {}", file.display());
}

/// A mono 16-bit WAV at 48 kHz.
fn write_wav(path: &Path, x: &[f64]) -> std::io::Result<()> {
    let data_len = (x.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + x.len() * 2);
    b.extend(b"RIFF");
    b.extend((36 + data_len).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(48_000u32.to_le_bytes());
    b.extend((48_000u32 * 2).to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data_len.to_le_bytes());
    for s in x {
        b.extend(((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, b)
}
