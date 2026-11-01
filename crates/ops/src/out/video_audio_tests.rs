//! `out.video` with sound, on the software adapter: what `out.audio` plays lands in the file
//! sample for sample, offline as live; pause, seeks and a tone that starts with a white frame;
//! the same from the `audio` input, with no `out.audio` at all.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use penumbra_audio::Outputs;
use penumbra_core::scratch::ScratchDir;
use penumbra_core::{LatePolicy, NodeId, NodePath, ParamPath, Rate, ShowClock, Value};
use penumbra_gpu::{AdapterChoice, Gpu};
use penumbra_graph::{Cooker, JobPool, Network, NodeTypes, Op};
use penumbra_record::{AudioCodec, Hub, Status, audio};

use crate::builtin_types;

/// Samples per show frame at 48 kHz and 60 fps.
const PER_FRAME: usize = 800;

fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match Gpu::headless(AdapterChoice::Software) {
        Ok(g) => Some(g),
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(e) => {
            eprintln!("skipping GPU tests: {e}");
            None
        }
    })
    .clone()
}

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
    connect_to(from, to, 0)
}

fn connect_to(from: &str, to: &str, input: u32) -> Op {
    Op::Connect { from: NodePath::parse(from).unwrap(), output: 0, to: NodePath::parse(to).unwrap(), input }
}

fn delete(path: &str) -> Op {
    Op::Delete { path: NodePath::parse(path).unwrap() }
}

struct Rig {
    types: Arc<NodeTypes>,
    net: Arc<Network>,
    cooker: Cooker,
    clock: ShowClock,
    record: Hub,
    outputs: Outputs,
    gpu: Gpu,
    /// Passes with the devices on (a live show) or off (an offline render).
    realtime: bool,
    _dir: ScratchDir,
}

impl Rig {
    /// An oscillator into `/speakers` and a picture into `/rec`, recording to `file` (in a
    /// scratch folder); `extra` adds to or changes it.
    fn new(gpu: Gpu, name: &str, file: &str, extra: &[Op]) -> Self {
        let dir = ScratchDir::new(&format!("ops-video-audio-{name}")).unwrap();
        let mut ops = vec![
            create(
                "/osc",
                "audio.oscillator",
                &[("frequency", Value::Float(440.0)), ("amplitude", Value::Float(0.25))],
            ),
            create("/speakers", "out.audio", &[("reverb", Value::Float(0.0))]),
            connect("/osc", "/speakers"),
            create("/picture", "tex.constant", &[("width", Value::Int(32)), ("height", Value::Int(32))]),
            create("/rec", "out.video", &[("file", file.into()), ("codec", "prores_422".into())]),
            connect("/picture", "/rec"),
        ];
        ops.extend_from_slice(extra);
        let types = Arc::new(builtin_types().unwrap());
        let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(2).unwrap()));
        cooker.provide(gpu.clone());
        cooker.set_base_dir(Some(dir.to_path_buf()));
        let record = Hub::with_planes_timeout(Duration::from_secs(60));
        cooker.provide(record.clone());
        // Never a speaker in tests.
        let outputs = Outputs::offline(48_000, 2);
        cooker.provide(outputs.clone());
        outputs.set_wanted(&super::audio::wants(&net, &types));
        let clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);
        Self { types, net, cooker, clock, record, outputs, gpu, realtime: false, _dir: dir }
    }

    fn id(&self, path: &str) -> NodeId {
        self.net.resolve(&path.parse().unwrap()).unwrap()
    }

    fn edit(&mut self, ops: &[Op]) {
        self.net = Arc::new(self.net.apply(&self.types, ops).unwrap().network);
    }

    /// One pass as the engine runs it, waiting for the encoder first so no frame is dropped.
    fn pass(&mut self) {
        let rec = self.id("/rec");
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.record.status(rec).is_some_and(|s| s.queue > 0 && !s.done) {
            assert!(Instant::now() < deadline, "the encoder did not catch up");
            std::thread::sleep(Duration::from_millis(2));
        }
        let sinks: Vec<NodeId> = ["/rec", "/speakers"]
            .iter()
            .filter_map(|p| self.net.resolve(&NodePath::parse(p).unwrap()))
            .collect();
        self.cooker.cook(&self.net, self.clock.now(), &sinks, &[]);
        for &id in &sinks {
            assert_eq!(self.cooker.status(id).and_then(|s| s.error), None);
        }
        self.outputs.after_pass(self.clock.is_playing(), self.realtime);
        self.gpu.poll();
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.clock.advance(1);
            self.pass();
        }
    }

    /// Cooks the first frame until the picture is there; the sound is running by then.
    fn warm_up(&mut self) {
        let picture = self.id("/picture");
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.cooker.outputs(picture).is_none_or(<[_]>::is_empty) {
            assert!(Instant::now() < deadline, "no picture");
            self.pass();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn start(&mut self) {
        self.edit(&[set("/rec.record", true)]);
        self.pass();
    }

    /// Stops the recording and waits for the file (one pass more: the sound of the pass that
    /// stopped it comes with the next).
    fn stop(&mut self) -> Status {
        self.edit(&[set("/rec.record", false)]);
        self.clock.advance(1);
        self.pass();
        self.clock.advance(1);
        self.pass();
        let rec = self.id("/rec");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let s = self.record.status(rec).unwrap();
            if s.done {
                assert_eq!(s.error, None);
                return s;
            }
            assert!(Instant::now() < deadline, "the recording did not finish: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The oscillator's samples `from..from + n` of the show (steady, so its phase is exact).
fn sine(from: i64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|k| {
            let t = (from + k as i64) as f64 / 48_000.0;
            (0.25 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
        })
        .collect()
}

fn worst(got: &[f32], want: &[f32]) -> f32 {
    got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0, f32::max)
}

#[test]
fn out_video_records_what_out_audio_plays_offline_and_live() {
    let Some(gpu) = gpu() else { return };
    for realtime in [false, true] {
        let mut rig = Rig::new(gpu.clone(), &format!("tone-{realtime}"), "tone.mov", &[]);
        rig.realtime = realtime;
        rig.clock.seek(30);
        rig.warm_up();
        rig.start();
        assert_eq!(rig.record.status(rig.id("/rec")).unwrap().audio.map(|a| a.sources), Some(1));
        rig.frames(59);
        let s = rig.stop();
        assert_eq!((s.frames, s.dropped, s.seconds), (60, 0, 1.0), "{s:?}");
        let a = s.audio.clone().unwrap();
        assert_eq!(
            (a.codec.as_str(), a.rate, a.channels, a.samples),
            ("pcm_s24le", 48_000, 2, 48_000),
            "{a:?}"
        );
        assert_eq!((a.gap_samples, a.refused, a.dropped), (0, 0, 0));
        let sound = audio::read(Path::new(&s.file)).unwrap().unwrap();
        assert_eq!((sound.codec, sound.len()), (AudioCodec::Pcm24, 48_000));
        // File sample 0 is the show's sample at frame 30. The last 144 samples (the limiter's
        // delay) would have come after the stop: silence.
        let n = 60 * PER_FRAME - 144;
        let want = sine(30 * PER_FRAME as i64, n);
        for c in &sound.channels {
            assert!(worst(&c[..n], &want) < 1e-6, "realtime {realtime}: {}", worst(&c[..n], &want));
            assert!(c[n..].iter().all(|v| *v == 0.0));
        }
    }
}

#[test]
fn audio_off_or_no_out_audio_records_no_sound() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(gpu.clone(), "off", "off.mov", &[set("/rec.audio", "off")]);
    rig.warm_up();
    rig.start();
    rig.frames(10);
    let s = rig.stop();
    assert_eq!(s.audio, None);
    assert_eq!(audio::read(Path::new(&s.file)).unwrap(), None);
    let mut rig = Rig::new(gpu, "none", "none.mov", &[delete("/speakers")]);
    rig.warm_up();
    rig.start();
    rig.frames(10);
    let s = rig.stop();
    assert_eq!(s.audio.map(|a| (a.sources, a.samples)), Some((0, 0)));
    assert_eq!(audio::read(Path::new(&s.file)).unwrap(), None);
}

/// A pause records nothing; a seek back or far ahead continues the sound without a gap as the
/// picture continues; a jump of under a second leaves the same time in both.
#[test]
fn pause_and_seeks_keep_picture_and_sound_the_same_length() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(gpu, "seeks", "seeks.mov", &[]);
    rig.clock.seek(100);
    rig.warm_up();
    rig.start();
    rig.frames(29);
    // Paused: passes without the clock moving.
    rig.clock.pause();
    for _ in 0..10 {
        rig.pass();
    }
    rig.clock.play();
    rig.frames(30);
    let before = rig.record.status(rig.id("/rec")).unwrap();
    // Back by a second: the picture continues with the next frame, the sound with it.
    let now = rig.clock.now().frame;
    rig.clock.seek(now - 60);
    rig.pass();
    rig.frames(29);
    // Ahead by 0.3 s: both catch up (the sound is made for the skipped time).
    rig.clock.seek(rig.clock.now().frame + 18);
    rig.pass();
    rig.frames(9);
    // Ahead by 0.8 s: the picture holds its frame, the sound has a gap of the same time.
    rig.clock.seek(rig.clock.now().frame + 48);
    rig.pass();
    rig.frames(9);
    // Ahead by 5 s: both continue without a gap.
    rig.clock.seek(rig.clock.now().frame + 300);
    rig.pass();
    rig.frames(9);
    let s = rig.stop();
    assert!(before.audio.is_some());
    let a = s.audio.unwrap();
    // `as`: whole samples.
    let picture = (s.seconds * 48_000.0).round() as u64;
    assert_eq!(a.samples, picture, "{a:?}, {} s of picture", s.seconds);
    // 30 + 30 + 30 + 10 + 10 + 10 frames, plus the 17 the 0.3 s jump skipped and the 47 of 0.8 s.
    assert_eq!(picture, (120 + 17 + 47) * PER_FRAME as u64);
    assert_eq!(s.skipped, 17 + 47);
    // The 0.8 s jump leaves 47 frames of silence (the block after it is one frame long); the
    // seek back and the 5 s jump fit without a gap or a cut. Cut: the limiter's delay at the
    // start (the pass that started the recording played nothing new, as the warm-up had).
    assert_eq!((a.gap_samples, a.cut_samples), (47 * PER_FRAME as u64, 144), "{a:?}");
}

/// A square tone at 1 Hz and a picture that turns white with it, every second, recorded for
/// 150 frames from show frame 30 on, with `extra` on top: the recording's status, the frames
/// where the picture turns white and the file's sound.
fn flash_and_tone(gpu: Gpu, name: &str, extra: &[Op]) -> (Status, Vec<usize>, Vec<Vec<f32>>) {
    let mut ops = vec![
        set("/osc.shape", "square"),
        set("/osc.frequency", 1.0),
        create(
            "/flash",
            "chan.lfo",
            &[
                ("shape", "square".into()),
                ("rate", Value::Float(1.0)),
                ("offset", Value::Float(0.5)),
                ("amplitude", Value::Float(0.5)),
            ],
        ),
    ];
    for c in 0..3 {
        ops.push(Op::Bind {
            param: ParamPath::parse("/picture.color").unwrap(),
            component: Some(c),
            from: NodePath::parse("/flash").unwrap(),
            channel: None,
            cell: None,
            format: None,
        });
    }
    ops.extend_from_slice(extra);
    let mut rig = Rig::new(gpu, name, &format!("{name}.mov"), &ops);
    // Start half a second in, so the file starts in the dark and quiet half.
    rig.clock.seek(30);
    rig.warm_up();
    rig.start();
    rig.frames(149);
    let s = rig.stop();
    let file = PathBuf::from(&s.file);
    let mut d = penumbra_video::open(&file, &penumbra_video::Limits::default()).unwrap();
    let mut white = Vec::new();
    let mut was = false;
    for f in 0..150 {
        let px = d.frame(f).unwrap();
        let luma = match &px.pixels().unwrap().data {
            penumbra_video::PixelData::Yuv16 { planes, bits, .. } => {
                f64::from(planes[0][0]) / f64::from(1u32 << (bits - 8))
            }
            other => panic!("{other:?}"),
        };
        let is = luma > 128.0;
        if is && !was {
            white.push(f);
        }
        was = is;
    }
    let sound = audio::read(&file).unwrap().unwrap();
    (s, white, sound.channels)
}

/// The samples where a square tone turns from below 0 to above.
fn rising(c: &[f32]) -> Vec<usize> {
    (1..c.len()).filter(|&k| c[k - 1] < 0.0 && c[k] > 0.0).collect()
}

/// The A/V check: a square tone and a picture that turns white with it, every second. In the
/// file the tone's rising edge and the first white frame are at the same time.
#[test]
fn a_tone_and_a_white_frame_at_the_same_show_time_meet_in_the_file() {
    let Some(gpu) = gpu() else { return };
    let (_, white, channels) = flash_and_tone(gpu, "sync", &[]);
    let rising = rising(&channels[0]);
    // White from frames 30 and 90 (show times 1 s and 2 s); the tone rises on the same samples.
    assert_eq!(white, [30, 90]);
    let at: Vec<usize> = white.iter().map(|f| f * PER_FRAME).collect();
    assert_eq!(
        rising,
        at,
        "A/V offset {:?} samples",
        rising.iter().zip(&at).map(|(r, a)| *r as i64 - *a as i64).collect::<Vec<_>>()
    );
}

/// The same check with the tone wired to the `audio` input and no `out.audio` at all: `auto`
/// takes the input as `input` does, the mono tone is on both channels, and click and flash
/// share a frame.
#[test]
fn the_audio_input_records_a_tone_no_speaker_plays_in_step_with_the_picture() {
    let Some(gpu) = gpu() else { return };
    for choice in ["auto", "input"] {
        let extra = [delete("/speakers"), connect_to("/osc", "/rec", 1), set("/rec.audio", choice)];
        let (s, white, channels) = flash_and_tone(gpu.clone(), &format!("input-{choice}"), &extra);
        let a = s.audio.clone().unwrap();
        assert_eq!(
            (a.sources, a.rate, a.channels, a.samples),
            (1, 48_000, 2, 150 * PER_FRAME as u64),
            "{a:?}"
        );
        // Nothing cut or filled in: the input's blocks join from the file's first sample on.
        assert_eq!((a.gap_samples, a.cut_samples, a.refused, a.dropped), (0, 0, 0, 0), "{a:?}");
        assert_eq!(white, [30, 90]);
        let at: Vec<usize> = white.iter().map(|f| f * PER_FRAME).collect();
        assert_eq!(rising(&channels[0]), at, "{choice}");
        assert_eq!(channels[0], channels[1], "a mono input is on both channels");
        // Straight from the oscillator: no limiter, the full amplitude.
        assert!(channels[0].iter().all(|v| (v.abs() - 0.25).abs() < 1e-6), "{choice}");
    }
}

/// With the input wired, `all` still records what `out.audio` plays and `off` records nothing;
/// neither cooks the chain on the input.
#[test]
fn all_and_off_ignore_a_wired_audio_input() {
    let Some(gpu) = gpu() else { return };
    let other = [
        create(
            "/other",
            "audio.oscillator",
            &[("frequency", Value::Float(1000.0)), ("amplitude", Value::Float(0.5))],
        ),
        connect_to("/other", "/rec", 1),
    ];
    let mut extra = other.to_vec();
    extra.push(set("/rec.audio", "all"));
    let mut rig = Rig::new(gpu.clone(), "all-wired", "all-wired.mov", &extra);
    rig.clock.seek(30);
    rig.warm_up();
    rig.start();
    rig.frames(59);
    let s = rig.stop();
    assert!(rig.cooker.outputs(rig.id("/other")).is_none_or(<[_]>::is_empty), "the input was cooked");
    let sound = audio::read(Path::new(&s.file)).unwrap().unwrap();
    let n = 60 * PER_FRAME - 144;
    let want = sine(30 * PER_FRAME as i64, n);
    assert!(worst(&sound.channels[0][..n], &want) < 1e-6, "not the out.audio's tone");
    let mut extra = other.to_vec();
    extra.push(set("/rec.audio", "off"));
    let mut rig = Rig::new(gpu, "off-wired", "off-wired.mov", &extra);
    rig.warm_up();
    rig.start();
    rig.frames(10);
    let s = rig.stop();
    assert!(rig.cooker.outputs(rig.id("/other")).is_none_or(<[_]>::is_empty), "the input was cooked");
    assert_eq!(s.audio, None);
    assert_eq!(audio::read(Path::new(&s.file)).unwrap(), None);
}

/// `input` with nothing wired records the picture without sound and says why; a sound wired in
/// while it records starts at its show time, after silence.
#[test]
fn input_with_nothing_wired_records_silence_and_warns() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(gpu, "unwired", "unwired.mov", &[set("/rec.audio", "input")]);
    rig.clock.seek(30);
    rig.warm_up();
    rig.start();
    rig.frames(10);
    let rec = rig.id("/rec");
    let warning = rig.cooker.status(rec).and_then(|s| s.warning).unwrap_or_default();
    assert!(warning.contains("nothing is connected to the audio input"), "{warning}");
    assert_eq!(rig.record.status(rec).unwrap().audio.map(|a| (a.sources, a.samples)), Some((0, 0)));
    // Wired at file frame 11: the first block of a newly wired input is one frame long and ends
    // at that cook, so the sound starts at frame 10.
    rig.edit(&[connect_to("/osc", "/rec", 1)]);
    rig.frames(20);
    let s = rig.stop();
    assert_eq!(s.frames, 31, "{s:?}");
    let a = s.audio.unwrap();
    assert_eq!(
        (a.sources, a.samples, a.gap_samples),
        (1, 31 * PER_FRAME as u64, 10 * PER_FRAME as u64),
        "{a:?}"
    );
    let sound = audio::read(Path::new(&s.file)).unwrap().unwrap();
    let c = &sound.channels[0];
    let from = 10 * PER_FRAME;
    assert!(c[..from].iter().all(|v| *v == 0.0), "silence before the wire");
    let want = sine(40 * PER_FRAME as i64, 21 * PER_FRAME);
    assert!(worst(&c[from..], &want) < 1e-6, "{}", worst(&c[from..], &want));
}
