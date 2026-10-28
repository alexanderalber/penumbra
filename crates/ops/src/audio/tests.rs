//! The audio family: the oscillator's waves and blocks, the mixer.

use std::sync::Arc;

use penumbra_core::{Chans, LatePolicy, NodePath, ParamPath, Rate, ShowClock, Value};
use penumbra_graph::{Cooker, DataKind, JobPool, Network, Op, Port};

use crate::builtin_types;

struct F {
    types: Arc<penumbra_graph::NodeTypes>,
    net: Arc<Network>,
    cooker: Cooker,
    clock: ShowClock,
}

impl F {
    fn new(ops: &[Op]) -> Self {
        Self::at(Rate::FPS_60, ops)
    }

    fn at(rate: Rate, ops: &[Op]) -> Self {
        let types = Arc::new(builtin_types().unwrap());
        let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
        let cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        Self { types, net, cooker, clock: ShowClock::new(rate, LatePolicy::Realtime) }
    }

    fn edit(&mut self, ops: &[Op]) {
        self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
    }

    fn cook(&mut self, path: &str) -> Result<Chans, String> {
        let id = self.net.resolve(&path.parse().unwrap()).unwrap();
        self.cooker.cook(&self.net, self.clock.now(), &[id], &[]);
        if let Some(e) = self.cooker.status(id).and_then(|s| s.error) {
            return Err(e);
        }
        let data = self.cooker.output(Port { node: id, output: 0 }).unwrap();
        assert_eq!(data.kind(), DataKind::Audio);
        Ok(data.get::<Chans>().unwrap().clone())
    }

    fn block(&mut self, path: &str) -> Vec<f64> {
        let c = self.cook(path).unwrap();
        c.channels()[0].samples.clone()
    }
}

fn create(path: &str, kind: &str) -> Op {
    Op::Create { path: NodePath::parse(path).unwrap(), kind: kind.into(), params: Default::default() }
}

fn set(p: &str, v: impl Into<Value>) -> Op {
    Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
}

fn connect(from: &str, to: &str, input: u32) -> Op {
    Op::Connect { from: NodePath::parse(from).unwrap(), output: 0, to: NodePath::parse(to).unwrap(), input }
}

fn osc(path: &str, shape: &str, frequency: f64) -> Vec<Op> {
    vec![
        create(path, "audio.oscillator"),
        set(&format!("{path}.shape"), shape),
        set(&format!("{path}.frequency"), frequency),
        set(&format!("{path}.amplitude"), 1.0),
    ]
}

/// The blocks of a sine joined: no jump beyond one sample's step, and the lengths add up to
/// the elapsed time exactly.
#[test]
fn a_sine_runs_on_across_blocks() {
    for (rate, fps) in [(48_000_u32, 60_u32), (44_100, 60), (48_000, 50)] {
        let show = Rate::fps(fps).unwrap();
        let mut ops = osc("/o", "sine", 1000.0);
        ops.push(set("/o.rate", i64::from(rate)));
        let mut f = F::at(show, &ops);
        let mut all = f.block("/o");
        let first = all.len();
        assert_eq!(first, (rate / fps) as usize, "the first block is one frame's worth");
        let frames = 600;
        for _ in 0..frames {
            f.clock.advance(1);
            all.extend(f.block("/o"));
        }
        assert_eq!(all.len() - first, frames as usize * rate as usize / fps as usize, "{rate} at {fps}");
        let step = std::f64::consts::TAU * 1000.0 / f64::from(rate);
        let worst = all.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        assert!(worst <= step * 1.0001, "{rate} at {fps}: a jump of {worst}, a step is {step}");
        let peak = all.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
        assert!(peak > 0.999 && peak <= 1.0, "{peak}");
    }
}

/// The waves at a frequency whose period is a whole number of samples.
#[test]
fn the_waves_have_their_shapes() {
    // 48000 / 600 = 80 samples per wave, 10 waves per frame.
    for shape in ["sine", "saw", "square", "triangle"] {
        let mut f = F::new(&osc("/o", shape, 600.0));
        f.clock.seek(1);
        let b = f.block("/o");
        assert_eq!(b.len(), 800);
        // The first block starts over at sample 0 here, so phase 0 is at index 0.
        let expect = |k: usize| crate::audio::oscillator::wave(shape, (k % 80) as f64 / 80.0);
        // Not at the jumps of saw and square, where the summed phase may sit a rounding
        // error either side.
        for (k, s) in b.iter().enumerate().filter(|(k, _)| k % 40 != 0) {
            assert!((s - expect(k)).abs() < 1e-9, "{shape} at {k}: {s} vs {}", expect(k));
        }
        // Periodic, and the mean of a symmetric wave is about zero.
        assert!((b[3] - b[83]).abs() < 1e-9);
        let mean = b.iter().sum::<f64>() / b.len() as f64;
        assert!(mean.abs() < 0.03, "{shape}: mean {mean}");
    }
}

#[test]
fn noise_is_the_same_every_time_and_differs_by_seed() {
    let mut ops = osc("/o", "noise", 0.0);
    ops.push(create("/p", "audio.oscillator"));
    ops.extend([set("/p.shape", "noise"), set("/p.amplitude", 1.0), set("/p.seed", 7)]);
    let a = F::new(&ops).block("/o");
    let b = F::new(&ops).block("/o");
    let c = F::new(&ops).block("/p");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert!(a.iter().all(|s| (-1.0..=1.0).contains(s)));
    let mean = a.iter().sum::<f64>() / a.len() as f64;
    let rms = (a.iter().map(|s| s * s).sum::<f64>() / a.len() as f64).sqrt();
    assert!(mean.abs() < 0.1 && (rms - 1.0 / 3.0_f64.sqrt()).abs() < 0.05, "mean {mean}, rms {rms}");
}

/// A new amplitude is reached across one block, not in a step.
#[test]
fn a_new_amplitude_glides_across_the_block() {
    let mut f = F::new(&osc("/o", "square", 600.0));
    f.block("/o");
    f.edit(&[set("/o.amplitude", 0.0)]);
    f.clock.advance(1);
    let b = f.block("/o");
    assert!((b[0].abs() - 1.0).abs() < 1e-9, "starts at the old amplitude");
    assert!(b[799].abs() < 0.01, "ends near the new one");
    let mid = b[400].abs();
    assert!((mid - 0.5).abs() < 0.01, "{mid}");
    f.clock.advance(1);
    assert!(f.block("/o").iter().all(|s| *s == 0.0));
}

/// A recook at the same tick computes the same block again; skipped frames come as one
/// longer block that continues the wave.
#[test]
fn the_same_tick_repeats_and_skipped_frames_catch_up() {
    let mut f = F::new(&osc("/o", "sine", 440.0));
    f.clock.advance(1);
    let a = f.block("/o");
    f.edit(&[set("/o.amplitude", 1.0)]);
    let again = f.block("/o");
    assert_eq!(a, again);
    f.clock.advance(3);
    let skipped = f.block("/o");
    assert_eq!(skipped.len(), 2400);
    let step = std::f64::consts::TAU * 440.0 / 48_000.0;
    assert!((skipped[0] - a[799]).abs() <= step * 1.0001);
}

#[test]
fn mix_adds_by_gain_and_matches_channels_by_name() {
    let mut ops = osc("/a", "square", 600.0);
    ops.extend(osc("/b", "square", 600.0));
    ops.extend([
        set("/a.amplitude", 0.5),
        set("/b.amplitude", 0.25),
        set("/b.name", "other"),
        create("/m", "audio.mix"),
        set("/m.gain1", 2.0),
        set("/m.gain", 0.5),
        connect("/a", "/m", 0),
        connect("/b", "/m", 1),
    ]);
    let mut f = F::new(&ops);
    let c = f.cook("/m").unwrap();
    // The first input's channel; the second's has another name, so its first channel adds.
    assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["ch1"]);
    // (0.5 + 2 * 0.25) * 0.5
    assert!((c.channels()[0].samples[0] - 0.5).abs() < 1e-9, "{}", c.channels()[0].samples[0]);
    // A gain of 0 mutes, after gliding there across one block.
    f.edit(&[set("/m.gain0", 0.0)]);
    f.clock.advance(1);
    f.cook("/m").unwrap();
    f.clock.advance(1);
    let c = f.cook("/m").unwrap();
    // Away from the square's edges, where the phase may sit a rounding error either side.
    assert!((c.channels()[0].samples[10] - 0.25).abs() < 1e-9);
}

#[test]
fn mix_refuses_two_rates() {
    let mut ops = osc("/a", "sine", 440.0);
    ops.extend(osc("/b", "sine", 440.0));
    ops.extend([
        set("/b.rate", 44_100),
        create("/m", "audio.mix"),
        connect("/a", "/m", 0),
        connect("/b", "/m", 1),
    ]);
    let e = F::new(&ops).cook("/m").unwrap_err();
    assert!(e.contains("44100") && e.contains("48000") && e.contains("resample"), "{e}");
}

/// A sound cannot be wired into a chan input, nor a channel into an audio input.
#[test]
fn audio_and_chan_do_not_connect() {
    let types = builtin_types().unwrap();
    let ops = [create("/o", "audio.oscillator"), create("/m", "chan.math"), connect("/o", "/m", 0)];
    assert!(Network::new().apply(&types, &ops).is_err());
    let ops = [create("/c", "chan.constant"), create("/m", "audio.mix"), connect("/c", "/m", 0)];
    assert!(Network::new().apply(&types, &ops).is_err());
    let ops = [create("/o", "audio.oscillator"), create("/a", "chan.audio_analysis"), connect("/o", "/a", 0)];
    assert!(Network::new().apply(&types, &ops).is_ok());
}
