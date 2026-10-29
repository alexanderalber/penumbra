//! `out.audio` against buses without a device: what it pushes, when it pushes nothing, the
//! limiter, several nodes on one device, pause, and the buses following the network.

use std::sync::Arc;
use std::time::Duration;

use penumbra_audio::limiter::{CEILING_DB, db_to_gain};
use penumbra_audio::{Hub, Outputs};
use penumbra_core::{Chans, LatePolicy, NodeId, NodePath, ParamPath, Rate, ShowClock, Value};
use penumbra_graph::{Cooker, JobPool, Network, NodeTypes, Op, Port};

use super::audio::{AUDIO_OUT, wants};
use crate::builtin_types;

/// Samples the node limiter delays the sound by at 48 kHz.
const LA: usize = 144;

struct F {
    types: Arc<NodeTypes>,
    net: Arc<Network>,
    cooker: Cooker,
    clock: ShowClock,
    outputs: Outputs,
    /// The oscillators' blocks as they cooked, per path.
    seen: Vec<(String, Vec<f64>)>,
}

impl F {
    fn new(ops: &[Op]) -> Self {
        let types = Arc::new(builtin_types().unwrap());
        let net = Arc::new(Network::new().apply(&types, ops).unwrap().network);
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let outputs = Outputs::offline(48_000, 2);
        cooker.provide(outputs.clone());
        cooker.provide(Hub::new());
        let f = Self {
            types,
            net,
            cooker,
            clock: ShowClock::new(Rate::FPS_60, LatePolicy::Realtime),
            outputs,
            seen: Vec::new(),
        };
        f.outputs.set_wanted(&wants(&f.net, &f.types));
        f
    }

    fn edit(&mut self, ops: &[Op]) {
        self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        self.outputs.set_wanted(&wants(&self.net, &self.types));
    }

    fn id(&self, path: &str) -> NodeId {
        self.net.resolve(&path.parse().unwrap()).unwrap()
    }

    /// One pass as the engine runs it: the `out.audio` nodes cooked, then the buses flushed.
    fn pass(&mut self) {
        let sinks: Vec<NodeId> = self.net.nodes().filter(|n| n.kind == AUDIO_OUT).map(|n| n.id).collect();
        self.cooker.cook(&self.net, self.clock.now(), &sinks, &[]);
        for n in self.net.nodes().filter(|n| n.kind == "audio.oscillator") {
            let path = self.net.path_of(n.id).unwrap().to_string();
            if let Some(d) = self.cooker.output(Port { node: n.id, output: 0 }) {
                let c = d.get::<Chans>().unwrap();
                let samples = c.channels()[0].samples.clone();
                match self.seen.iter_mut().find(|(p, _)| *p == path) {
                    Some((_, s)) => s.extend(samples),
                    None => self.seen.push((path, samples)),
                }
            }
        }
        self.outputs.after_pass(self.clock.is_playing(), true);
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.clock.advance(1);
            self.pass();
        }
    }

    fn error(&self, path: &str) -> Option<String> {
        self.cooker.status(self.id(path)).and_then(|s| s.error)
    }

    fn warning(&self, path: &str) -> Option<String> {
        self.cooker.status(self.id(path)).and_then(|s| s.warning)
    }

    fn pushed(&self, key: &str) -> Vec<Vec<f32>> {
        self.outputs.pushed(key)
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

fn tone(path: &str, freq: f64, amp: f64) -> Vec<Op> {
    vec![
        create(path, "audio.oscillator"),
        set(&format!("{path}.frequency"), freq),
        set(&format!("{path}.amplitude"), amp),
    ]
}

/// An oscillator into `out.audio`, the reverb off (`extra` may turn it on).
fn played(osc: &str, out: &str, extra: &[Op]) -> F {
    let mut ops = tone(osc, 440.0, 0.25);
    ops.push(create(out, AUDIO_OUT));
    ops.push(set(&format!("{out}.reverb"), 0.0));
    ops.push(connect(osc, out, 0));
    ops.extend_from_slice(extra);
    F::new(&ops)
}

fn reference<'a>(f: &'a F, path: &str) -> &'a [f64] {
    f.seen.iter().find(|(p, _)| p == path).map(|(_, s)| s.as_slice()).unwrap()
}

/// The jump fade the node puts on the first block.
fn fade(k: usize) -> f64 {
    let n = 144.0;
    let x = ((k as f64 + 0.5) / n).min(1.0);
    x * x * (3.0 - 2.0 * x)
}

#[test]
fn the_oscillator_plays_gaplessly_also_over_a_skipped_frame() {
    let mut f = played("/osc", "/out", &[]);
    f.pass();
    f.frames(30);
    // A frame skipped: its samples come with the next block.
    f.clock.advance(2);
    f.pass();
    f.frames(60);
    assert_eq!(f.error("/out"), None);
    let pushed = f.pushed("");
    // Mono stays mono with the reverb off; the player puts it on every speaker.
    assert_eq!(pushed.len(), 1);
    let reference = reference(&f, "/osc");
    // 92 frames of 800 samples, one of them two frames long.
    assert_eq!(reference.len(), 93 * 800);
    assert_eq!(pushed[0].len(), reference.len());
    // The reference itself is one steady sine: no gap, no overlap, across every block.
    let start = -800.0;
    for (k, v) in reference.iter().enumerate() {
        let n = start + k as f64;
        let want = 0.25 * (std::f64::consts::TAU * 440.0 * n / 48_000.0).sin();
        assert!((v - want).abs() < 1e-6, "sample {k}: {v} vs {want}");
    }
    // What was pushed is the reference, delayed by the limiter's look-ahead, faded in at the
    // first block, unchanged by the limiter (well below the ceiling).
    for c in &pushed {
        assert!(c[..LA].iter().all(|v| *v == 0.0));
        for (k, want) in reference.iter().take(reference.len() - LA).enumerate() {
            let want = want * if k < 144 { fade(k) } else { 1.0 };
            let got = f64::from(c[k + LA]);
            assert!((got - want).abs() < 1e-6, "sample {k}: {got} vs {want}");
        }
    }
}

#[test]
fn a_recook_at_the_same_tick_pushes_nothing() {
    let mut f = played("/osc", "/out", &[]);
    f.frames(10);
    let before = f.pushed("")[0].len();
    // Paused: params change, the oscillator recooks the same frame, nothing is pushed twice.
    f.clock.pause();
    for i in 0..5 {
        f.edit(&[set("/osc.amplitude", 0.1 + 0.01 * f64::from(i)), set("/out.level", -f64::from(i))]);
        f.pass();
    }
    assert_eq!(f.pushed("")[0].len(), before);
    let bus = f.outputs.bus_status("").unwrap();
    assert!(bus.paused, "a show that stands still fades out");
    f.clock.play();
    f.frames(1);
    assert_eq!(f.pushed("")[0].len(), before + 800);
}

#[test]
fn a_device_source_handing_on_the_same_block_is_not_played_twice() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/out", AUDIO_OUT),
        connect("/in", "/out", 0),
    ]);
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    hub.feed("test", 48_000, 1, &[0.0; 16]).unwrap();
    f.frames(1);
    hub.feed("test", 48_000, 1, &vec![0.1f32; 1000]).unwrap();
    f.frames(1);
    let first = f.pushed("")[0].len();
    assert_eq!(first, 1000);
    // No new samples, but the node cooks: its level is changed every frame (as a binding would).
    for i in 0..4 {
        f.edit(&[set("/out.level", -1.0 - f64::from(i))]);
        f.frames(1);
    }
    assert_eq!(f.pushed("")[0].len(), first, "the same block was pushed again");
    hub.feed("test", 48_000, 1, &vec![0.1f32; 500]).unwrap();
    f.frames(1);
    assert_eq!(f.pushed("")[0].len(), first + 500);
}

#[test]
fn a_seek_fades_the_new_block_in() {
    let mut f = played("/osc", "/out", &[set("/osc.shape", "square")]);
    f.frames(20);
    let before = f.pushed("")[0].len();
    f.clock.seek(3);
    f.pass();
    f.frames(2);
    let pushed = &f.pushed("")[0];
    // The square wave is 0.25 or -0.25 everywhere; right after the jump (and the look-ahead)
    // it rises from silence over the fade.
    let at = before + LA;
    let first = f64::from(pushed[at]).abs();
    let later = f64::from(pushed[at + 200]).abs();
    assert!(first < 0.01, "{first}");
    assert!((later - 0.25).abs() < 1e-6, "{later}");
    for k in 0..143 {
        assert!(pushed[at + k].abs() <= pushed[at + k + 1].abs() + 1e-6, "not rising at {k}");
    }
}

#[test]
fn a_loud_burst_never_exceeds_the_ceiling_and_a_quiet_tone_passes() {
    // +12 dB: four times full scale.
    let mut f = played("/osc", "/out", &[set("/osc.amplitude", 4.0)]);
    f.frames(60);
    let ceiling = db_to_gain(CEILING_DB);
    let pushed = f.pushed("");
    let peak = pushed.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(f64::from(peak) <= ceiling + 1e-6, "{peak}");
    assert!(f64::from(peak) > ceiling - 0.01, "{peak}");
    assert!(pushed.iter().flatten().all(|v| v.is_finite()));
    let held = f.outputs.lane_status(f.id("/out")).unwrap().1.reduction_held_db;
    assert!(held > 12.0, "{held}");
    assert!(f.warning("/out").unwrap().contains("limiter"), "{:?}", f.warning("/out"));
    // -20 dBFS passes unchanged within 0.1 dB.
    let mut f = played("/osc", "/out", &[set("/osc.amplitude", 0.1)]);
    f.frames(60);
    let pushed = &f.pushed("")[0];
    let rms = |x: &[f32]| (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    let reference = reference(&f, "/osc");
    let want = (reference[800..].iter().map(|v| v * v).sum::<f64>() / (reference.len() - 800) as f64).sqrt();
    let got = rms(&pushed[800 + LA..]);
    let db = 20.0 * (got / want).log10();
    assert!(db.abs() < 0.1, "{db} dB");
    assert_eq!(f.outputs.lane_status(f.id("/out")).unwrap().1.reduction_held_db, 0.0);
}

#[test]
fn level_and_mute_ramp_and_the_panic_button_silences_every_output() {
    let mut f = played("/osc", "/out", &[set("/osc.shape", "square"), set("/out.level", -6.0)]);
    f.frames(10);
    let v = f64::from(f.pushed("")[0].last().copied().unwrap()).abs();
    assert!((v - 0.25 * db_to_gain(-6.0)).abs() < 1e-6, "{v}");
    f.edit(&[set("/out.mute", true)]);
    f.frames(2);
    let tail = &f.pushed("")[0];
    assert!(tail[tail.len() - 400..].iter().all(|v| *v == 0.0));
    f.edit(&[set("/out.mute", false)]);
    f.frames(2);
    f.outputs.set_muted(true);
    f.frames(2);
    let tail = &f.pushed("")[0];
    assert!(tail[tail.len() - 800..].iter().all(|v| *v == 0.0), "audio.mute");
    assert!(f.warning("/out").unwrap().contains("muted"));
    f.outputs.set_muted(false);
    f.frames(2);
    let tail = &f.pushed("")[0];
    assert!(tail[tail.len() - 100..].iter().all(|v| v.abs() > 0.1));
}

#[test]
fn two_nodes_on_one_device_add_up_and_their_sum_is_limited() {
    let mut ops = tone("/a", 440.0, 0.2);
    ops.extend(tone("/b", 660.0, 0.1));
    ops.extend([
        create("/oa", AUDIO_OUT),
        create("/ob", AUDIO_OUT),
        set("/oa.reverb", 0.0),
        set("/ob.reverb", 0.0),
        connect("/a", "/oa", 0),
        connect("/b", "/ob", 0),
    ]);
    let mut f = F::new(&ops);
    f.frames(40);
    let pushed = &f.pushed("")[0];
    let (a, b) = (reference(&f, "/a"), reference(&f, "/b"));
    // Each node's limiter and the bus limiter delay by one look-ahead each.
    let delay = 2 * LA;
    for k in 2000..a.len() - delay {
        let want = a[k] + b[k];
        let got = f64::from(pushed[k + delay]);
        assert!((got - want).abs() < 1e-6, "sample {k}: {got} vs {want}");
    }
    assert_eq!(f.outputs.keys(), vec![String::new()]);
    assert_eq!(f.outputs.bus_status("").unwrap().lanes.len(), 2);
    // Two loud ones: each is limited, and so is their sum.
    f.edit(&[set("/a.amplitude", 0.8), set("/b.amplitude", 0.8)]);
    f.frames(30);
    let pushed = &f.pushed("")[0];
    let peak = pushed.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(f64::from(peak) <= db_to_gain(CEILING_DB) + 1e-6, "{peak}");
    assert!(f.outputs.bus_status("").unwrap().reduction_db > 1.0);
}

#[test]
fn nodes_on_two_devices_each_play() {
    let mut ops = tone("/a", 440.0, 0.2);
    ops.extend(tone("/b", 660.0, 0.1));
    ops.extend([
        create("/oa", AUDIO_OUT),
        create("/ob", AUDIO_OUT),
        set("/ob.device", "Headphones"),
        connect("/a", "/oa", 0),
        connect("/b", "/ob", 0),
    ]);
    let mut f = F::new(&ops);
    f.frames(10);
    assert_eq!(f.outputs.keys(), vec![String::new(), "Headphones".to_string()]);
    assert_eq!(f.pushed("")[0].len(), 10 * 800);
    assert_eq!(f.pushed("Headphones")[0].len(), 10 * 800);
}

#[test]
fn a_pause_fades_out_without_an_underrun() {
    let mut f = played("/osc", "/out", &[]);
    let mut out = vec![0.0f32; 480 * 2];
    // The device pulls 10 ms a hundred times a second, the show cooks sixty frames: `wall` counts
    // the show's frame periods, paused or not.
    let mut pulled: i64 = 0;
    let mut wall: i64 = 0;
    let mut step = |f: &mut F, out: &mut Vec<f32>, playing: bool| -> f32 {
        wall += 1;
        if playing {
            f.clock.advance(1);
        }
        f.pass();
        let mut loudest = 0.0f32;
        while pulled * 60 < wall * 100 {
            f.outputs.pull("", out);
            loudest = out.iter().fold(loudest, |m, v| m.max(v.abs()));
            pulled += 1;
        }
        loudest
    };
    for _ in 0..60 {
        step(&mut f, &mut out, true);
    }
    let st = f.outputs.bus_status("").unwrap().offline.unwrap();
    assert_eq!(st.state, penumbra_audio::playback::State::Playing);
    assert_eq!(st.underruns, 0);
    f.clock.pause();
    let mut loudest_late = 0.0f32;
    for i in 0..50 {
        let loudest = step(&mut f, &mut out, false);
        if i > 1 {
            loudest_late = loudest_late.max(loudest);
        }
    }
    let st = f.outputs.bus_status("").unwrap().offline.unwrap();
    assert_eq!(st.underruns, 0);
    assert!(st.paused);
    assert_eq!(loudest_late, 0.0);
    // Playing again starts over without a gap counted.
    f.clock.play();
    for _ in 0..60 {
        step(&mut f, &mut out, true);
    }
    let st = f.outputs.bus_status("").unwrap().offline.unwrap();
    assert!(!st.paused);
    assert_eq!(st.state, penumbra_audio::playback::State::Playing);
    assert_eq!(st.underruns, 0);
}

#[test]
fn a_node_that_stops_feeding_fades_out_and_a_deleted_one_closes_its_device() {
    let mut f = played("/osc", "/out", &[]);
    f.frames(10);
    // The source goes: the node cooks once more (nothing connected), then the bus gets nothing.
    f.edit(&[Op::Delete { path: NodePath::parse("/osc").unwrap() }]);
    f.frames(3);
    assert!(f.outputs.bus_status("").unwrap().paused);
    assert!(f.warning("/out").unwrap().contains("nothing connected"));
    f.edit(&[set("/out.active", false)]);
    assert!(f.outputs.keys().is_empty(), "an inactive node does not keep its device");
    assert_eq!(f.outputs.closing(), 1);
    std::thread::sleep(Duration::from_millis(200));
    f.frames(1);
    assert_eq!(f.outputs.closing(), 0);
    f.edit(&[set("/out.active", true), set("/out.device", "x")]);
    assert_eq!(f.outputs.keys(), vec!["x".to_string()]);
    f.edit(&[Op::Delete { path: NodePath::parse("/out").unwrap() }]);
    assert!(f.outputs.keys().is_empty());
}

#[test]
fn hostile_params_and_blocks_do_not_break_it() {
    let mut f = played(
        "/osc",
        "/out",
        &[set("/out.level", 1e9), set("/out.latency", -5.0), set("/out.device", "  \u{0}\u{7f} weird  ")],
    );
    f.frames(5);
    assert_eq!(f.error("/out"), None);
    let pushed = f.pushed("\u{0}\u{7f} weird");
    assert!(!pushed.is_empty());
    assert!(
        pushed.iter().flatten().all(|v| v.is_finite() && f64::from(v.abs()) <= db_to_gain(CEILING_DB) + 1e-6)
    );
    f.edit(&[set("/out.level", -1e9)]);
    f.frames(3);
    let tail = &f.outputs.pushed("\u{0}\u{7f} weird")[0];
    assert!(tail[tail.len() - 400..].iter().all(|v| *v == 0.0));
    // The hub refuses what cannot be played, and keeps going.
    let out = &f.outputs;
    let node = NodeId(999);
    let s = penumbra_audio::playback::Settings::default();
    assert!(out.submit(node, "z", s, 0, &[vec![0.0; 10]], 0.0).is_err());
    assert!(out.submit(node, "z", s, 48_000, &[], 0.0).is_err());
    assert!(out.submit(node, "z", s, 48_000, &vec![vec![0.0; 4]; 65], 0.0).is_err());
    out.submit(node, "z", s, 48_000, &[vec![f64::NAN, f64::INFINITY, 1e300], vec![0.5]], 0.0).unwrap();
    // Another node at another rate on the same device is refused while the first plays.
    assert!(out.submit(NodeId(1000), "z", s, 44_100, &[vec![0.0; 10]], 0.0).is_err());
    out.after_pass(true, true);
    let z = out.pushed("z");
    assert!(z.iter().flatten().all(|v| v.is_finite()));
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| f64::from(*p) * f64::from(*q)).sum::<f64>();
    dot(a, b) / (dot(a, a) * dot(b, b)).sqrt()
}

#[test]
fn the_default_reverb_gives_a_mono_tone_a_stereo_room_and_off_it_stays_wide_until_a_jump() {
    // The node as created: the reverb at its default.
    let mut ops = tone("/osc", 440.0, 0.25);
    ops.extend([set("/osc.shape", "saw"), create("/out", AUDIO_OUT), connect("/osc", "/out", 0)]);
    let mut f = F::new(&ops);
    f.frames(30);
    // A short burst, then silence: the room rings on, differently left and right.
    f.edit(&[set("/osc.amplitude", 0.0)]);
    let before = f.pushed("")[0].len();
    f.frames(30);
    let pushed = f.pushed("");
    assert_eq!(pushed.len(), 2, "the reverb makes mono stereo");
    let (l, r) = (&pushed[0][before + 2000..], &pushed[1][before + 2000..]);
    let tail = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(tail > 1e-3, "a tail after the tone: {tail}");
    let corr = correlation(l, r);
    assert!(corr.abs() < 0.5, "left and right differ: {corr}");
    assert_eq!(f.outputs.lane_status(f.id("/out")).unwrap().1.channels, 2);
    // Off: the tail fades within a frame, the node stays stereo while it plays.
    f.edit(&[set("/out.reverb", 0.0), set("/osc.amplitude", 0.25)]);
    f.frames(5);
    assert_eq!(f.outputs.lane_status(f.id("/out")).unwrap().1.channels, 2);
    let pushed = f.pushed("");
    let n = pushed[0].len();
    assert_eq!(pushed[0][n - 800..], pushed[1][n - 800..], "dry: the same on both sides");
    // A jump with the reverb off: mono again.
    f.clock.seek(2);
    f.pass();
    f.frames(2);
    assert_eq!(f.outputs.lane_status(f.id("/out")).unwrap().1.channels, 1);
}

#[test]
fn a_seek_leaves_no_tail_and_mute_silences_the_tail_at_once() {
    let big = [set("/out.reverb", 1.0), set("/out.room", 1.0), set("/osc.shape", "square")];
    // Without a jump the large room rings on for seconds after the tone stops.
    let mut f = played("/osc", "/out", &big);
    f.frames(30);
    f.edit(&[set("/osc.amplitude", 0.0)]);
    f.frames(30);
    let p = &f.pushed("")[0];
    assert!(p[p.len() - 800..].iter().any(|v| v.abs() > 1e-3), "a tail");
    // A seek empties the room: after the look-ahead nothing is left.
    let mut f = played("/osc", "/out", &big);
    f.frames(30);
    f.edit(&[set("/osc.amplitude", 0.0)]);
    let before = f.pushed("")[0].len();
    f.clock.seek(2);
    f.pass();
    f.frames(5);
    for c in f.pushed("") {
        assert!(c[before + LA..].iter().all(|v| *v == 0.0), "a tail after the seek");
    }
    // Mute: the tail is gone after the frame it fades across.
    let mut f = played("/osc", "/out", &big);
    f.frames(30);
    f.edit(&[set("/out.mute", true)]);
    f.frames(2);
    for c in f.pushed("") {
        assert!(c[c.len() - 400..].iter().all(|v| *v == 0.0), "the tail is muted");
    }
}

#[test]
fn the_limiter_holds_the_ceiling_with_the_largest_room_on_a_loud_input() {
    let mut f = played(
        "/osc",
        "/out",
        &[
            set("/osc.amplitude", 4.0),
            set("/osc.shape", "saw"),
            set("/out.reverb", 1.0),
            set("/out.room", 1.0),
        ],
    );
    f.frames(120);
    f.edit(&[set("/out.reverb", 0.6), set("/out.room", 0.0), set("/out.damping", 1.0)]);
    f.frames(60);
    let ceiling = db_to_gain(CEILING_DB);
    let pushed = f.pushed("");
    assert_eq!(pushed.len(), 2);
    let peak = pushed.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(pushed.iter().flatten().all(|v| v.is_finite()));
    assert!(f64::from(peak) <= ceiling + 1e-6, "{peak}");
    assert!(f.warning("/out").unwrap().contains("limiter"), "{:?}", f.warning("/out"));
}

#[test]
fn a_mono_and_a_stereo_node_on_one_device_add_up_on_both_sides() {
    let mut ops = tone("/a", 440.0, 0.2);
    ops.extend(tone("/b", 660.0, 0.0));
    // `/a` dry and mono, `/b` with the room (stereo) but silent.
    ops.extend([
        create("/oa", AUDIO_OUT),
        create("/ob", AUDIO_OUT),
        set("/oa.reverb", 0.0),
        connect("/a", "/oa", 0),
        connect("/b", "/ob", 0),
    ]);
    let mut f = F::new(&ops);
    f.frames(30);
    let bus = f.outputs.bus_status("").unwrap();
    assert_eq!(bus.channels, 2);
    let pushed = f.pushed("");
    let a = reference(&f, "/a");
    let delay = 2 * LA;
    for c in &pushed {
        for k in 2000..a.len() - delay {
            assert!((f64::from(c[k + delay]) - a[k]).abs() < 1e-6, "sample {k}");
        }
    }
}

/// A 16-bit WAV at 48 kHz, one vector per channel.
fn write_wav(path: &std::path::Path, channels: &[Vec<f32>]) {
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let ch = channels.len() as u16;
    let data_len = (frames * channels.len() * 2) as u32;
    let mut b = Vec::new();
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
    std::fs::write(path, b).unwrap();
}

/// Writes `reverb_pluck.wav`: plucked notes dry, then with reverb 0.2 room 0.4, then reverb 0.5
/// room 0.9, 6 s each. Run with `cargo test -p penumbra-ops reverb_pluck -- --ignored`; the
/// file goes to `PENUMBRA_LISTEN_DIR`, else to the system's temp directory.
#[test]
#[ignore]
fn write_reverb_pluck() {
    let mut all: Vec<Vec<f32>> = vec![Vec::new(), Vec::new()];
    for (mix, room) in [(0.0, 0.4), (0.2, 0.4), (0.5, 0.9)] {
        let mut f = F::new(&[
            create("/a", "chan.lfo"),
            set("/a.rate", 0.75),
            create("/b", "chan.lfo"),
            set("/b.rate", 1.1),
            create("/rings", "chan.merge"),
            connect("/a", "/rings", 0),
            connect("/b", "/rings", 1),
            create("/pluck", "audio.instrument"),
            set("/pluck.preset", "pluck"),
            set("/pluck.trigger", "both"),
            set("/pluck.key", "F"),
            set("/pluck.octave", 3),
            set("/pluck.spread", 3.0),
            connect("/rings", "/pluck", 0),
            create("/out", AUDIO_OUT),
            set("/out.reverb", mix),
            set("/out.room", room),
            connect("/pluck", "/out", 0),
        ]);
        f.frames(6 * 60);
        assert_eq!(f.error("/out"), None);
        let pushed = f.pushed("");
        for (k, a) in all.iter_mut().enumerate() {
            // The dry part is mono: the same on both sides.
            a.extend_from_slice(&pushed[k.min(pushed.len() - 1)]);
            a.extend(std::iter::repeat_n(0.0, 24_000));
        }
    }
    let dir =
        std::env::var_os("PENUMBRA_LISTEN_DIR").map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let file = dir.join("reverb_pluck.wav");
    write_wav(&file, &all);
    eprintln!("wrote {}", file.display());
}
