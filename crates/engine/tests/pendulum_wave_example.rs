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

/// Renders a project with an `out.video` offline, picture and sound: every frame cooked in
/// turn at 60 fps, each waiting for the encoder, so nothing is dropped; the buses play into
/// nothing, as in a render slower than real time. The project is `PENUMBRA_RENDER_PROJECT` (a
/// folder with `project.ron` and at least one `out.video`), the length `PENUMBRA_RENDER_SECONDS`
/// (64 when not set). The file goes where the node's `file` says. Uses the GPU when there is
/// one, else the software adapter.
///
/// `PENUMBRA_RENDER_PROJECT=<dir> cargo test --release -p penumbra-engine --test
/// pendulum_wave_example render_a_project_offline -- --ignored --nocapture`
#[test]
#[ignore]
fn render_a_project_offline() {
    use std::time::{Duration, Instant};

    use penumbra_core::{ParamPath, Value};
    use penumbra_gpu::{AdapterChoice, Gpu};
    use penumbra_graph::Op;

    let Some(dir) = std::env::var_os("PENUMBRA_RENDER_PROJECT").map(PathBuf::from) else {
        eprintln!("PENUMBRA_RENDER_PROJECT is not set: nothing to render");
        return;
    };
    let seconds: i64 =
        std::env::var("PENUMBRA_RENDER_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(64);
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    let content = penumbra_project::parse(&text, &types).unwrap();
    let ops = penumbra_project::diff(&Content::default(), &content);
    let mut net = Arc::new(Network::new().apply_with(&types, &ops, Params::Keep).unwrap().network);
    let videos: Vec<_> = net.nodes().filter(|n| n.kind == "out.video").map(|n| n.id).collect();
    assert!(!videos.is_empty(), "the project has no out.video");
    let sinks: Vec<_> =
        net.nodes().filter(|n| n.kind == "out.video" || n.kind == "out.audio").map(|n| n.id).collect();
    let gpu =
        Gpu::headless(AdapterChoice::Hardware).or_else(|_| Gpu::headless(AdapterChoice::Software)).unwrap();
    let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(4).unwrap()));
    cooker.provide(gpu.clone());
    cooker.set_base_dir(Some(dir.clone()));
    let record = penumbra_record::Hub::with_planes_timeout(Duration::from_secs(60));
    cooker.provide(record.clone());
    let outputs = Outputs::offline(48_000, 2);
    cooker.provide(outputs.clone());
    cooker.provide(penumbra_ops::audio::instrument::Presses::new());
    outputs.set_wanted(&penumbra_ops::out::audio::wants(&net, &types));
    let switch = |net: &Arc<Network>, on: bool| {
        let ops: Vec<Op> = videos
            .iter()
            .map(|id| Op::Set {
                param: ParamPath::new(net.path_of(*id).unwrap(), "record".parse().unwrap()).unwrap(),
                value: Value::Bool(on),
            })
            .collect();
        Arc::new(net.apply(&types, &ops).unwrap().network)
    };
    let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::EveryFrame);
    // Shaders and fonts load in the background: cook the first frame until every picture to
    // record is there, then start.
    let pictures: Vec<_> = videos
        .iter()
        .filter_map(|id| net.node(*id).and_then(|n| n.inputs.first().copied().flatten()))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !pictures.iter().all(|p| cooker.output(*p).is_some()) {
        assert!(Instant::now() < deadline, "the pictures to record did not come");
        cooker.cook(&net, clock.now(), &sinks, &[]);
        gpu.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    net = switch(&net, true);
    let started = Instant::now();
    let frames = seconds * 60;
    // Frame `frames` stops the recording; the one after hands it the sound of that pass.
    for frame in 0..=frames + 1 {
        if frame == frames {
            net = switch(&net, false);
        }
        // Waits for the encoder, so no frame is dropped.
        for id in &videos {
            let deadline = Instant::now() + Duration::from_secs(60);
            while record.status(*id).is_some_and(|s| s.queue > 0) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        cooker.cook(&net, clock.now(), &sinks, &[]);
        for id in &sinks {
            assert_eq!(cooker.status(*id).and_then(|s| s.error), None, "frame {frame}");
        }
        outputs.after_pass(true, false);
        gpu.poll();
        clock.advance(1);
        if frame % 600 == 0 {
            eprintln!("frame {frame} of {frames}, {:.0} s", started.elapsed().as_secs_f64());
        }
    }
    for id in &videos {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !record.status(*id).is_some_and(|s| s.done) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = record.status(*id).unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&s).unwrap());
        assert!(s.done && s.error.is_none(), "{s:?}");
        assert_eq!(s.dropped, 0);
    }
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
