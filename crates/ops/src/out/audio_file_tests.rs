//! `out.audio_file`: what its input or every `out.audio` plays lands in the WAV file sample for
//! sample; pause and seeks; formats, channels, names and errors.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_audio::Outputs;
use penumbra_core::scratch::ScratchDir;
use penumbra_core::{Channel, Chans, LatePolicy, Name, NodeId, NodePath, ParamPath, Rate, ShowClock, Value};
use penumbra_graph::{
    CookContext, CookError, Cooked, Cooker, Data, DataKind, FnNodeType, JobPool, Network, NodeDesc,
    NodeTypes, Op, Operator,
};
use penumbra_record::wav::{self, WavFile};
use penumbra_record::{Hub, SoundStatus, WavFormat};

use crate::audio::block::BlockClock;
use crate::builtin_types;

/// Samples per show frame at 48 kHz and 60 fps.
const PER_FRAME: usize = 800;

fn create(path: &str, kind: &str, params: &[(&str, Value)]) -> Op {
    Op::Create {
        path: NodePath::parse(path).unwrap(),
        kind: kind.into(),
        params: params.iter().map(|(k, v)| (k.parse().unwrap(), v.clone())).collect(),
    }
}

fn set(p: &str, v: impl Into<Value>) -> Op {
    Op::Set { param: ParamPath::parse(p).unwrap(), value: v.into() }
}

fn connect(from: &str, to: &str) -> Op {
    Op::Connect {
        from: NodePath::parse(from).unwrap(),
        output: 0,
        to: NodePath::parse(to).unwrap(),
        input: 0,
    }
}

/// A stereo tone for the tests: 440 Hz on the left at 0.25, 660 Hz on the right at 0.5, steady
/// on the show clock.
#[derive(Default)]
struct Stereo {
    clock: BlockClock,
}

impl Operator for Stereo {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let time = cx.time();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), 48_000);
        let side = |f: f64, a: f64| -> Vec<f64> {
            (span.start..span.end)
                .map(|n| a * (std::f64::consts::TAU * f * n as f64 / 48_000.0).sin())
                .collect()
        };
        let chans = Chans::new(
            48_000.0,
            vec![
                Channel { name: Name::new("l").unwrap(), samples: side(440.0, 0.25) },
                Channel { name: Name::new("r").unwrap(), samples: side(660.0, 0.5) },
            ],
        )?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}

fn types() -> Arc<NodeTypes> {
    let mut types = builtin_types().unwrap();
    types
        .register(FnNodeType::new(
            NodeDesc::new("test.stereo", "A stereo tone").output("out", DataKind::Audio).time_dependent(),
            || Box::new(Stereo::default()) as Box<dyn Operator>,
        ))
        .unwrap();
    Arc::new(types)
}

struct Rig {
    types: Arc<NodeTypes>,
    net: Arc<Network>,
    cooker: Cooker,
    clock: ShowClock,
    record: Hub,
    outputs: Outputs,
    dir: ScratchDir,
}

impl Rig {
    /// An oscillator into `/rec` (source input) and into `/speakers`, recording to `file` in
    /// a scratch folder; `extra` adds to or changes it.
    fn new(name: &str, file: &str, extra: &[Op]) -> Self {
        let dir = ScratchDir::new(&format!("ops-audio-file-{name}")).unwrap();
        let mut ops = vec![
            create(
                "/osc",
                "audio.oscillator",
                &[("frequency", Value::Float(440.0)), ("amplitude", Value::Float(0.25))],
            ),
            create("/speakers", "out.audio", &[("reverb", Value::Float(0.0))]),
            connect("/osc", "/speakers"),
            create("/rec", "out.audio_file", &[("file", file.into())]),
            connect("/osc", "/rec"),
        ];
        ops.extend_from_slice(extra);
        let types = types();
        let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(2).unwrap()));
        cooker.set_base_dir(Some(dir.to_path_buf()));
        let record = Hub::new();
        cooker.provide(record.clone());
        // Never a speaker in tests.
        let outputs = Outputs::offline(48_000, 2);
        cooker.provide(outputs.clone());
        outputs.set_wanted(&super::audio::wants(&net, &types));
        let clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        Self { types, net, cooker, clock, record, outputs, dir }
    }

    fn id(&self, path: &str) -> NodeId {
        self.net.resolve(&path.parse().unwrap()).unwrap()
    }

    fn edit(&mut self, ops: &[Op]) {
        self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
        self.outputs.set_wanted(&super::audio::wants(&self.net, &self.types));
    }

    /// One pass as the engine runs it; returns `/rec`'s error and warning.
    fn pass(&mut self) -> (Option<String>, Option<String>) {
        let sinks: Vec<NodeId> = ["/rec", "/speakers"]
            .iter()
            .filter_map(|p| self.net.resolve(&NodePath::parse(p).unwrap()))
            .collect();
        self.cooker.cook(&self.net, self.clock.now(), &sinks, &[]);
        self.outputs.after_pass(self.clock.is_playing(), true);
        let s = self.cooker.status(self.id("/rec")).unwrap();
        (s.error, s.warning)
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.clock.advance(1);
            let (error, _) = self.pass();
            assert_eq!(error, None);
        }
    }

    fn start(&mut self) {
        self.edit(&[set("/rec.record", true)]);
        let (error, _) = self.pass();
        assert_eq!(error, None);
    }

    /// Stops the recording at the next frame and waits for the file (one pass more: the sound
    /// from the tap of the pass that stopped it comes with the next).
    fn stop(&mut self) -> SoundStatus {
        self.edit(&[set("/rec.record", false)]);
        self.clock.advance(1);
        self.pass();
        self.clock.advance(1);
        self.pass();
        let rec = self.id("/rec");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let s = self.record.sound_status(rec).unwrap();
            if s.done {
                return s;
            }
            assert!(Instant::now() < deadline, "the recording did not finish: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// A steady sine's samples `from..from + n` of the show, at 48 kHz.
fn sine(f: f64, a: f64, from: i64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|k| (a * (std::f64::consts::TAU * f * (from + k as i64) as f64 / 48_000.0).sin()) as f32)
        .collect()
}

fn worst(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len());
    got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0, f32::max)
}

fn read(s: &SoundStatus) -> WavFile {
    assert_eq!(s.error, None, "{s:?}");
    wav::read(Path::new(&s.file)).unwrap()
}

/// The frequency of a tone, from its rising zero crossings.
fn frequency(c: &[f32]) -> f64 {
    let rising: Vec<usize> = (1..c.len()).filter(|&k| c[k - 1] < 0.0 && c[k] >= 0.0).collect();
    let (first, last) = (rising[0], rising[rising.len() - 1]);
    (rising.len() - 1) as f64 * 48_000.0 / (last - first) as f64
}

#[test]
fn the_input_lands_in_the_file_sample_for_sample() {
    for (format, tolerance) in [(WavFormat::Pcm24, 1.5 / 8_388_607.0), (WavFormat::Float32, 1e-7)] {
        let mut rig = Rig::new(format.name(), "tone", &[set("/rec.format", format.name())]);
        rig.clock.seek(30);
        rig.pass();
        rig.start();
        rig.frames(59);
        let s = rig.stop();
        assert_eq!((s.samples, s.seconds, s.rate, s.channels), (48_000, 1.0, 48_000, 1), "{s:?}");
        assert_eq!((s.gap_samples, s.refused, s.dropped, s.sources), (0, 0, 0, None), "{s:?}");
        assert!(s.file.ends_with("tone.wav"), "{s:?}");
        let w = read(&s);
        assert_eq!((w.format, w.rate, w.channels.len()), (format, 48_000, 1));
        // File sample 0 is the show's sample at frame 30.
        let want = sine(440.0, 0.25, 30 * PER_FRAME as i64, 48_000);
        let worst = worst(&w.channels[0], &want);
        assert!(worst <= tolerance as f32, "{format:?}: {worst}");
        assert!((frequency(&w.channels[0]) - 440.0).abs() < 0.01);
    }
}

#[test]
fn stereo_stays_stereo() {
    let extra = [
        create("/stereo", "test.stereo", &[]),
        Op::Disconnect { to: NodePath::parse("/rec").unwrap(), input: 0 },
        connect("/stereo", "/rec"),
    ];
    let mut rig = Rig::new("stereo", "", &extra);
    rig.pass();
    rig.start();
    rig.frames(120);
    let s = rig.stop();
    // Empty `file`: recordings/<node name>.wav.
    assert_eq!(PathBuf::from(&s.file), rig.dir.join("recordings").join("rec.wav"));
    let w = read(&s);
    // The block of the cook that stops it is the 121st.
    assert_eq!((w.channels.len(), w.channels[0].len(), w.tag), (2, 121 * PER_FRAME, 1));
    assert!((frequency(&w.channels[0]) - 440.0).abs() < 0.01);
    assert!((frequency(&w.channels[1]) - 660.0).abs() < 0.01);
    let peak = |c: &[f32]| c.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((peak(&w.channels[0]) - 0.25).abs() < 1e-3 && (peak(&w.channels[1]) - 0.5).abs() < 1e-3);
}

/// `source: all` records what the out.audio nodes hand their device, sample for sample.
#[test]
fn all_records_what_out_audio_plays() {
    let mut rig = Rig::new("all", "all.wav", &[set("/rec.source", "all")]);
    rig.clock.seek(30);
    rig.pass();
    rig.start();
    assert_eq!(rig.record.sound_status(rig.id("/rec")).unwrap().sources, Some(1));
    rig.frames(59);
    let s = rig.stop();
    let w = read(&s);
    assert_eq!((w.channels.len(), s.sources, s.dropped, s.refused), (2, Some(1), 0, 0), "{s:?}");
    // The limiter delays the sound by 144 samples: the sound of the last 144 samples of show
    // time comes after the stop.
    let n = w.channels[0].len();
    assert_eq!(n, 60 * PER_FRAME - 144, "{s:?}");
    // What the bus pushed to its (offline) device holds the same samples, in one stretch.
    let pushed = rig.outputs.pushed("");
    let first = &w.channels[0];
    let probe = 1000;
    let at = (0..=pushed[0].len() - n)
        .min_by(|&a, &b| {
            let e = |k: usize| worst(&pushed[0][k..k + probe], &first[..probe]);
            e(a).total_cmp(&e(b))
        })
        .unwrap();
    // A mono bus: the tap gives it on both sides.
    assert_eq!(pushed.len(), 1);
    for (c, got) in w.channels.iter().enumerate() {
        let e = worst(got, &pushed[0][at..at + n]);
        assert!(e < 1e-6, "channel {c}: {e}");
    }
    // And that is the tone from the show's sample at frame 30 on, in step with the show.
    assert!(worst(first, &sine(440.0, 0.25, 30 * PER_FRAME as i64, n)) < 1e-6);
    assert!((frequency(first) - 440.0).abs() < 0.01);
}

/// A pause records nothing; a seek back or far ahead continues the file; a jump ahead of under
/// a second is caught up by the oscillator (up to half a second) or left as silence.
#[test]
fn pause_and_seeks() {
    let mut rig = Rig::new("seeks", "seeks", &[]);
    rig.clock.seek(100);
    rig.pass();
    rig.start();
    rig.frames(29);
    rig.clock.pause();
    for _ in 0..10 {
        rig.pass();
    }
    rig.clock.play();
    rig.frames(30);
    // Back by a second.
    let now = rig.clock.now().frame;
    rig.clock.seek(now - 60);
    rig.pass();
    rig.frames(29);
    // Ahead by 0.3 s: the oscillator makes the skipped time.
    rig.clock.seek(rig.clock.now().frame + 18);
    rig.pass();
    rig.frames(9);
    // Ahead by 0.8 s: silence for the time skipped.
    rig.clock.seek(rig.clock.now().frame + 48);
    rig.pass();
    rig.frames(9);
    // Ahead by 5 s: continues without a gap.
    rig.clock.seek(rig.clock.now().frame + 300);
    rig.pass();
    rig.frames(9);
    let s = rig.stop();
    // Blocks: 29 + 30 played, 30 after the seek back, 18 + 9 after the short jump, 1 + 9 after
    // the 0.8 s jump with 47 frames of silence before, 1 + 9 after the long jump, 1 at the stop.
    let frames = 29 + 30 + 30 + 18 + 9 + 47 + 10 + 10 + 1;
    assert_eq!(s.samples, frames * PER_FRAME as u64, "{s:?}");
    assert_eq!(s.gap_samples, 47 * PER_FRAME as u64, "{s:?}");
    // Only the block of the cook that started it, which lies before the start.
    assert_eq!(s.cut_samples, PER_FRAME as u64, "{s:?}");
    assert_eq!(read(&s).channels[0].len() as u64, s.samples);
}

#[test]
fn record_toggles_and_never_overwrites() {
    let mut rig = Rig::new("names", "takes/take", &[]);
    rig.pass();
    let rec = rig.id("/rec");
    assert_eq!(rig.record.sound_status(rec), None);
    let mut files = Vec::new();
    for _ in 0..3 {
        rig.start();
        rig.frames(10);
        let s = rig.stop();
        assert_eq!(s.samples, 11 * PER_FRAME as u64, "{s:?}");
        files.push(PathBuf::from(s.file));
        // Off, nothing more is recorded.
        rig.frames(5);
        assert_eq!(rig.record.sound_status(rec).unwrap().samples, 11 * PER_FRAME as u64);
    }
    let names: Vec<String> = files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into()).collect();
    assert_eq!(names, ["take.wav", "take_2.wav", "take_3.wav"]);
    assert!(files.iter().all(|f| f.parent() == Some(&rig.dir.join("takes"))));
    // A name with its own extension keeps it.
    rig.edit(&[set("/rec.file", "raw.sound")]);
    rig.start();
    rig.frames(2);
    assert!(rig.stop().file.ends_with("raw.sound"));
}

#[test]
fn problems_say_what_to_do() {
    // Nothing wired with source input: not recording, and why.
    let mut rig =
        Rig::new("unwired", "x", &[Op::Disconnect { to: NodePath::parse("/rec").unwrap(), input: 0 }]);
    rig.pass();
    rig.edit(&[set("/rec.record", true)]);
    let (error, _) = rig.pass();
    assert!(error.unwrap().contains("nothing to record: connect a sound"));
    // Wired later: still not, until record turns off and on.
    rig.edit(&[connect("/osc", "/rec")]);
    rig.clock.advance(1);
    assert!(rig.pass().0.is_some());
    rig.edit(&[set("/rec.record", false)]);
    rig.pass();
    rig.start();
    // Unwired while recording: a warning, the file goes on.
    rig.edit(&[Op::Disconnect { to: NodePath::parse("/rec").unwrap(), input: 0 }]);
    rig.clock.advance(1);
    let (error, warning) = rig.pass();
    assert_eq!(error, None);
    assert!(warning.unwrap().contains("nothing connected"));
    rig.edit(&[connect("/osc", "/rec")]);
    rig.frames(3);
    // The sound changes its rate: left out and said.
    rig.edit(&[set("/osc.rate", 44_100)]);
    // The thread counts it; the node says so at a cook after.
    let deadline = Instant::now() + Duration::from_secs(10);
    let warning = loop {
        rig.clock.advance(1);
        let (error, warning) = rig.pass();
        assert_eq!(error, None);
        if let Some(w) = warning.filter(|w| w.contains("left out")) {
            break w;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(warning.contains("the sound is no longer the file's 48000 Hz, 1 channel;"), "{warning}");
    let s = rig.stop();
    assert!(s.refused >= 1 && s.error.is_none(), "{s:?}");

    // A folder that cannot be made: the node's error.
    let mut rig = Rig::new("blocked", "blocker/x", &[]);
    std::fs::write(rig.dir.join("blocker"), b"a file").unwrap();
    rig.pass();
    rig.edit(&[set("/rec.record", true)]);
    rig.pass();
    let deadline = Instant::now() + Duration::from_secs(10);
    let error = loop {
        rig.clock.advance(1);
        if let (Some(e), _) = rig.pass() {
            break e;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(error.contains("cannot create the folder"), "{error}");

    // source all without an out.audio: a warning, no file.
    let mut rig = Rig::new(
        "silent",
        "silent",
        &[set("/rec.source", "all"), Op::Delete { path: NodePath::parse("/speakers").unwrap() }],
    );
    rig.pass();
    rig.start();
    rig.clock.advance(1);
    let (error, warning) = rig.pass();
    assert_eq!(error, None);
    assert!(warning.unwrap().contains("no out.audio plays"));
    let s = rig.stop();
    assert!(!s.opened && s.error.is_none(), "{s:?}");
    assert!(!rig.dir.join("silent.wav").exists());
}
