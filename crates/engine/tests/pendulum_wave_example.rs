//! The pendulum-wave example's sound, rendered offline: ignored tests that cook the example
//! for 20 s and write WAVs to listen to. `pendulum_example.wav` is the dry `/mix`,
//! `pendulum_reverb.wav` what `/speakers` (`out.audio`, with its default reverb and limiter)
//! hands its device, through a bus without a device.
//!
//! Run with `cargo test -p penumbra-engine --test pendulum_wave_example -- --ignored`. The WAVs
//! go to `PENUMBRA_LISTEN_DIR`, else to the system's temp directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use penumbra_audio::Outputs;
use penumbra_core::{Chans, LatePolicy, NodePath, Rate, ShowClock};
use penumbra_graph::{Cooker, JobPool, Network, Params, Port};
use penumbra_project::{Content, FILE_NAME};

const SECONDS: usize = 20;

#[cfg(test)]
fn example() -> (Arc<penumbra_graph::NodeTypes>, Arc<Network>) {
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/pendulum-wave");
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    let content = penumbra_project::parse(&text, &types).unwrap();
    let ops = penumbra_project::diff(&Content::default(), &content);
    let net = Arc::new(Network::new().apply_with(&types, &ops, Params::Keep).unwrap().network);
    (types, net)
}

fn listen_dir() -> PathBuf {
    std::env::var_os("PENUMBRA_LISTEN_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

#[test]
#[ignore]
fn write_the_pendulum_wave() {
    let (types, net) = example();
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
    let file = listen_dir().join("pendulum_example.wav");
    write_wav(&file, &[all]).unwrap();
    eprintln!("wrote {}", file.display());
}

#[test]
#[ignore]
fn write_the_pendulum_wave_through_its_output() {
    let (types, net) = example();
    let speakers = net.resolve(&NodePath::parse("/speakers").unwrap()).unwrap();
    let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
    let outputs = Outputs::offline(48_000, 2);
    cooker.provide(outputs.clone());
    outputs.set_wanted(&penumbra_ops::out::audio::wants(&net, &types));
    let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
    for _ in 0..SECONDS * 60 {
        clock.advance(1);
        cooker.cook(&net, clock.now(), &[speakers], &[]);
        assert_eq!(cooker.status(speakers).and_then(|s| s.error), None);
        outputs.after_pass(true, true);
    }
    let pushed: Vec<Vec<f64>> =
        outputs.pushed("").iter().map(|c| c.iter().map(|v| f64::from(*v)).collect()).collect();
    assert_eq!(pushed.len(), 2, "the reverb makes the mono mix stereo");
    let peak = pushed.iter().flatten().fold(0.0_f64, |m, s| m.max(s.abs()));
    eprintln!("peak {:.1} dBFS over {SECONDS} s", 20.0 * peak.max(1e-12).log10());
    assert!(peak > 0.05 && peak < 0.9, "peak {peak}");
    let file = listen_dir().join("pendulum_reverb.wav");
    write_wav(&file, &pushed).unwrap();
    eprintln!("wrote {}", file.display());
}

/// A 16-bit WAV at 48 kHz, one vector per channel.
#[cfg(test)]
fn write_wav(path: &Path, channels: &[Vec<f64>]) -> std::io::Result<()> {
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let ch = channels.len() as u16;
    let data_len = (frames * channels.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend(b"RIFF");
    b.extend((36 + data_len).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(ch.to_le_bytes());
    b.extend(48_000u32.to_le_bytes());
    b.extend((48_000u32 * 2 * u32::from(ch)).to_le_bytes());
    b.extend((2 * ch).to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data_len.to_le_bytes());
    for k in 0..frames {
        for c in channels {
            b.extend(((c[k].clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
    }
    std::fs::write(path, b)
}
