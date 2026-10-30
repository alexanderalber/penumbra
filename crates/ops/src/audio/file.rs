//! `audio.file`: plays a sound file (WAV, FLAC, MP3, Ogg Vorbis, the AAC sound of MP4, MOV and
//! M4A), block by block on the show clock ([`super::block`]). Decoding runs on a thread of its
//! own ([`penumbra_audio::file`]); the cook only reads what is decoded and never waits.
//!
//! **Time** works as in `tex.video_file`, so a video and its sound set alike stay together: the
//! playhead starts at `start` and moves with the show clock, times `speed`. A node that stops
//! cooking holds its place; after a gap over half a second, or when the show clock goes back,
//! it moves on by one frame from where it was. The sample due at a cook's show time is the
//! file's sample at the playhead, so a 60 s run at speed 1 is exactly 60 s of the file.
//!
//! **Speed** works as on tape: the file's samples are read at `speed` times their rate, so the
//! pitch moves with it. Every output sample is interpolated straight from the file with the
//! resampler's windowed sinc ([`penumbra_audio::resample::Kernel`]), cut off at half the lower
//! of the two rates (the file's times speed and the output's). The same path converts the
//! file's rate to `rate`. A file at the output's rate played at speed 1 is copied unchanged.
//!
//! **Restarts** go to `start`: a rising edge on `restart`, a press through [`Presses`]
//! (`audio.play`, the panel's play button) or, with `restart_on_show`, the node coming back into
//! use. The new pass sounds from the next block on (the sample at this cook's show time), as the
//! video shows `start` at that time; what was playing fades out over 4 ms. With `loop` off a
//! restart plays the file once: a one-shot sample.
//!
//! **Silence:** before the file starts, after it ends (`loop` off), with `play` off or speed 0
//! (after a 4 ms fade), and where the decoder has not caught up (a seek in a long file): such a
//! gap is counted and warned about, and the sound fades in again over 4 ms. A missing or
//! unreadable file warns and plays silence.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_audio::file::{EXTENSIONS, Frame, SoundFile, Status};
use penumbra_audio::resample::{Kernel, dot};
use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, Unit, lock};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator,
};

use super::block::{BlockClock, Span, Step};
use super::instrument::Presses;
use super::oscillator::{MAX_RATE, MIN_RATE};

/// Kind of the file player.
pub const FILE: &str = "audio.file";
/// A pause in cooking longer than this is not played through, as in `tex.video_file`.
const GAP_SECONDS: f64 = 0.5;
/// Fades at starts, stops, restarts and gaps.
const FADE_SECONDS: f64 = 0.004;
/// How long a gap keeps the node's warning up, in show seconds.
const GAP_WARNING_SECONDS: f64 = 2.0;
/// Output samples rendered per read from the file.
const CHUNK: usize = 256;
/// Most file samples per output sample: a higher speed times rate is held here.
const MAX_STEP: f64 = 64.0;
/// Kernels are made for cutoffs in steps of an eighth of an octave.
const KERNEL_STEPS_PER_OCTAVE: f64 = 8.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(FILE, "Plays a sound file: music, an atmosphere or a sample, looped or once")
            .param(ParamSpec::file("file", EXTENSIONS).doc("WAV, FLAC, MP3, Ogg Vorbis, or the AAC sound of an MP4, MOV or M4A"))
            .param(ParamSpec::bool("play", true).doc("Off fades the sound out and holds the place; on plays on from there"))
            .param(
                ParamSpec::float("speed", 1.0)
                    .clamp(0.0, 16.0)
                    .range(0.0, 4.0)
                    .doc("Playback rate, as on tape: 2 plays twice as fast and an octave higher, 0.5 half as fast and an octave lower"),
            )
            .param(ParamSpec::bool("loop", true).doc("Off plays the file once and then stays silent"))
            .param(
                ParamSpec::float("start", 0.0)
                    .clamp(0.0, 1e7)
                    .range(0.0, 60.0)
                    .unit(Unit::Seconds)
                    .more()
                    .doc("Where playback begins when the file is loaded and where a restart goes"),
            )
            .param(ParamSpec::bool("restart", false).doc(
                "Turning on starts the file over from `start`; bind a channel to cue it. The play button does the same",
            ))
            .param(ParamSpec::bool("restart_on_show", false).doc(
                "Starts over whenever the sound comes back into use, such as a switch or sequence picking it",
            ))
            .param(
                ParamSpec::choice("channels", &["file", "mono", "stereo"])
                    .more()
                    .doc("Which channels come out")
                    .choice_doc("file", "as many as the file has: mono stays mono")
                    .choice_doc("mono", "one, the mean of all the file's channels")
                    .choice_doc("stereo", "two: a mono file on both, else the file's first two"),
            )
            .param(ParamSpec::int("rate", 48_000).clamp(f64::from(MIN_RATE), f64::from(MAX_RATE)).more().doc(
                "Samples per second of the sound made, whatever the file's rate. out.audio nodes on one device need the same rate; audio.mix converts between rates",
            ))
            .output("out", DataKind::Audio)
            .output("info", DataKind::Chan)
            .describe_output("out", "The file's sound, channels ch1, ch2, ...")
            .describe_output(
                "info",
                "Playback: position (s), progress (0 to 1), loops, duration (s), rate and channels of the file, gaps, seeks, restarts",
            )
            .sampled()
            .time_dependent(),
        || Box::new(FilePlayer::default()) as Box<dyn Operator>,
    ))
}

/// What a file player shows outside the cook (`audio.file_info`, the panel), by node. Provided
/// by the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct SoundFiles(Arc<Mutex<BTreeMap<NodeId, FileStatus>>>);

/// How one `audio.file` stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileStatus {
    /// The file as resolved (empty: none set).
    pub file: String,
    /// What the decoder says: the file's codec, rate, channels and length, once open.
    pub decoder: Status,
    /// Seconds into the file at the last cook.
    pub position: f64,
    pub playing: bool,
    /// Gaps where the decoder had not caught up, since the file was loaded.
    pub gaps: u64,
    pub restarts: u64,
}

impl SoundFiles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, node: NodeId) -> Option<FileStatus> {
        lock(&self.0).get(&node).cloned()
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    fn set(&self, node: NodeId, status: FileStatus) {
        lock(&self.0).insert(node, status);
    }
}

/// Where the playhead is at each output sample: a straight line through (`anchor_s`,
/// `anchor_sec`) rising `speed` file seconds per output second. Positions are unwrapped
/// (looping goes on past the length); before `floor` it is silent.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    anchor_s: i64,
    anchor_sec: f64,
    speed: f64,
    floor: f64,
    rate: u32,
}

impl Mapping {
    /// The line that reaches `sec` at output sample `s`.
    fn through(s: i64, sec: f64, speed: f64, floor: f64, rate: u32) -> Self {
        Self { anchor_s: s, anchor_sec: sec, speed, floor, rate }
    }

    fn at(&self, s: i64) -> f64 {
        self.anchor_sec + (s.saturating_sub(self.anchor_s)) as f64 * self.speed / f64::from(self.rate.max(1))
    }
}

/// How a render scales what it reads.
enum Gain<'a> {
    /// Fading in after a silence: the gain now (0 to 1), rising by `step` per sample.
    Main { g: &'a mut f64, step: f64 },
    /// Fading out over `len` samples, from the first.
    Out { len: usize },
}

/// Reused buffers of a render.
#[derive(Default)]
struct Scratch {
    frames: Vec<f32>,
    found: Vec<Frame>,
    planar: Vec<Vec<f32>>,
    coeffs: Vec<f32>,
}

#[derive(Default)]
struct FilePlayer {
    file: Option<SoundFile>,
    watch: Option<FileWatch>,
    path: String,
    clock: BlockClock,
    mapping: Option<Mapping>,
    /// The playhead at the end of the last block (file seconds, unwrapped), as `tex.video_file`
    /// keeps it.
    playhead: f64,
    /// The fade-in gain.
    gain: f64,
    /// The cutoff scale the kernel was made for, and the kernel.
    kernel: Option<(f64, Kernel)>,
    scratch: Scratch,
    /// `restart` at the last cook (`None` before the first), for its rising edge.
    restart_was: Option<bool>,
    /// A restart seen during a block rendered again, for the next new block.
    restart_pending: bool,
    pressed: Vec<usize>,
    /// Missing at the end of the last block (a gap ran on).
    missing: bool,
    gaps: u64,
    gap_time: f64,
    restarts: u64,
    names: Vec<Name>,
    has_output: bool,
    status: Status,
}

impl Operator for FilePlayer {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let rate =
            u32::try_from(cx.int("rate")?.clamp(i64::from(MIN_RATE), i64::from(MAX_RATE))).unwrap_or(48_000);
        let play = cx.bool("play")?;
        let speed = cx.float("speed")?.clamp(0.0, 16.0);
        let speed = if speed.is_finite() { speed } else { 0.0 };
        let looping = cx.bool("loop")?;
        let start = cx.float("start")?.clamp(0.0, 1e7);
        let start = if start.is_finite() { start } else { 0.0 };
        let mode = cx.text("channels")?.to_string();
        let time = cx.time();
        let now = time.seconds();
        let span = self.clock.step(time.tick, time.rate.frame_ticks(), rate);
        let advancing = span.step != Step::Again;

        // Every trigger is read at every cook, so none is lost while a block is rendered again.
        let on = cx.bool("restart")?;
        let edge = on && self.restart_was == Some(false);
        self.restart_was = Some(on);
        if advancing {
            self.pressed.clear();
        }
        if let Some(presses) = cx.resource::<Presses>() {
            presses.check_in(cx.node(), advancing, &mut self.pressed);
        }
        let back = cx.bool("restart_on_show")? && cx.since_last_use().is_some_and(|gap| gap > GAP_SECONDS);
        self.restart_pending |= edge || back || !self.pressed.is_empty();

        // The file, opened again when it moves or changes on disk.
        let text = cx.text("file")?.to_string();
        let path = cx.resolve_path(&text).ok();
        let mut reloaded = false;
        match &path {
            Some(p) => {
                let moved = self.watch.as_ref().is_none_or(|w| w.path() != p);
                let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
                if moved || changed || self.file.is_none() {
                    if moved {
                        self.watch = Some(cx.watch_file(p));
                    }
                    let waker = cx.waker();
                    self.file = Some(SoundFile::open(p, Some(Box::new(move || waker.wake()))));
                    self.path = p.display().to_string();
                    self.gaps = 0;
                    self.missing = false;
                    reloaded = true;
                }
            }
            None => {
                self.file = None;
                self.watch = None;
                self.path.clear();
            }
        }
        if let Some(f) = &self.file {
            self.status = f.status();
        } else {
            self.status = Status::default();
        }

        if !advancing && self.has_output {
            return Ok(Cooked::Unchanged);
        }

        // Where the playhead goes, as in `tex.video_file`.
        let moving = if play { speed } else { 0.0 };
        let frame_sec = f64::from(cx.rate().den()) / f64::from(cx.rate().num().max(1));
        let restart = advancing && (std::mem::take(&mut self.restart_pending) || reloaded);
        if restart && !reloaded {
            self.restarts += 1;
        }
        let mut tail = None;
        let mapping = match self.mapping {
            None => Mapping::through(span.end, start, moving, start, rate),
            Some(m) if restart => {
                if m.speed > 0.0 && !reloaded {
                    tail = Some(m);
                }
                Mapping::through(span.end, start, moving, start, rate)
            }
            Some(_) if span.step == Step::Fresh => {
                // A gap or a clock that went back: one show frame on from where it was.
                self.gain = 0.0;
                Mapping::through(span.end, self.playhead + frame_sec * moving, moving, 0.0, rate)
            }
            Some(m) if m.speed != moving || m.rate != rate => {
                let here = m.at(span.start);
                if moving == 0.0 {
                    tail = Some(m);
                }
                if m.speed == 0.0 && here > 1e-9 {
                    self.gain = 0.0;
                }
                Mapping { anchor_s: span.start, anchor_sec: here, speed: moving, floor: m.floor, rate }
            }
            Some(m) => m,
        };
        if self.mapping.is_none() || restart {
            // Starting inside the file fades in; at its very start the file's own attack stays.
            self.gain = if start > 1e-9 { 0.0 } else { 1.0 };
        }
        self.mapping = Some(mapping);
        self.playhead = mapping.at(span.end);

        let Some(file) = self.file.as_mut() else {
            cx.warn("no file set: silent");
            return self.silence(span, rate, cx);
        };
        if let Some(e) = &self.status.error {
            cx.warn(format!("{e}: silent"));
            if !self.status.ready {
                return self.silence(span, rate, cx);
            }
        }
        let Some(info) = self.status.info.clone().filter(|_| self.status.ready) else {
            return Err(CookError::Pending);
        };
        let file_rate = f64::from(info.rate);
        let ch = info.channels.max(1);
        let cue = (start * file_rate).floor() as i64;

        // The kernel for the cutoff this speed needs.
        let step = (moving * file_rate / f64::from(rate)).min(MAX_STEP);
        let scale = if step > 1.0 {
            let octaves = (step.log2() * KERNEL_STEPS_PER_OCTAVE).ceil() / KERNEL_STEPS_PER_OCTAVE;
            0.5f64.powf(octaves)
        } else {
            1.0
        };
        if self.kernel.as_ref().is_none_or(|(s, _)| *s != scale) {
            self.kernel = Some((scale, Kernel::new(1.0, scale)));
        }
        let Some((_, kernel)) = self.kernel.as_ref() else { return Err(CookError::Pending) };

        let n = span.len();
        let mut planes = vec![vec![0.0f64; n]; ch];
        let fade = ((FADE_SECONDS * f64::from(rate)).ceil() as usize).max(1);
        let reader = Reader { file_rate, out_rate: f64::from(rate), channels: ch, looping, cue };
        let mut missing_now = false;
        if let Some(t) = tail {
            let len = fade.min(n);
            let mut gain = Gain::Out { len: fade };
            reader.render(
                file,
                kernel,
                &t,
                span.start,
                len,
                &mut self.scratch,
                &mut planes,
                &mut gain,
                &mut missing_now,
            );
        }
        let mut missing = self.missing;
        let mut gain = Gain::Main { g: &mut self.gain, step: 1.0 / fade as f64 };
        let gaps = reader.render(
            file,
            kernel,
            &mapping,
            span.start,
            n,
            &mut self.scratch,
            &mut planes,
            &mut gain,
            &mut missing,
        );
        self.missing = missing;
        let length = file.length();
        if gaps > 0 {
            self.gaps += gaps;
            self.gap_time = now;
        }
        if self.gaps > 0 && now - self.gap_time < GAP_WARNING_SECONDS && now >= self.gap_time {
            cx.warn(format!("the file was not decoded in time: {} gaps of silence so far", self.gaps));
        }

        let planes = map_channels(planes, &mode);
        self.publish(cx);
        let out = self.chans(planes, rate)?;
        let info_chans = self.info_chans(&info, looping, length, cx.rate().as_f64())?;
        self.has_output = true;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, out), Data::new(DataKind::Chan, info_chans)]))
    }

    /// Running while the file opens (so `node.snapshot` waits for it), idle afterwards.
    fn job_status(&self) -> Option<JobStatus> {
        Some(if self.file.is_some() && !self.status.ready && self.status.error.is_none() {
            JobStatus::Running { progress: None }
        } else {
            JobStatus::Idle
        })
    }
}

impl FilePlayer {
    /// A silent block of one channel, with the info.
    fn silence(&mut self, span: Span, rate: u32, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let out = self.chans(vec![vec![0.0; span.len()]], rate)?;
        let info = penumbra_audio::file::Info {
            codec: String::new(),
            rate: 0,
            channels: 0,
            frames: None,
            exact: false,
            in_memory: false,
        };
        self.publish(cx);
        let info_chans = self.info_chans(&info, false, None, cx.rate().as_f64())?;
        self.has_output = true;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, out), Data::new(DataKind::Chan, info_chans)]))
    }

    fn file_status(&self) -> FileStatus {
        FileStatus {
            file: self.path.clone(),
            decoder: self.status.clone(),
            position: self.playhead,
            playing: self.mapping.is_some_and(|m| m.speed > 0.0),
            gaps: self.gaps,
            restarts: self.restarts,
        }
    }

    /// Tells [`SoundFiles`] how the node stands.
    fn publish(&self, cx: &CookContext<'_>) {
        if let Some(files) = cx.resource::<SoundFiles>() {
            files.set(cx.node(), self.file_status());
        }
    }

    fn chans(&mut self, planes: Vec<Vec<f64>>, rate: u32) -> Result<Chans, CookError> {
        while self.names.len() < planes.len() {
            self.names.push(Name::new(&format!("ch{}", self.names.len() + 1))?);
        }
        let channels = planes
            .into_iter()
            .zip(&self.names)
            .map(|(samples, name)| Channel { name: name.clone(), samples })
            .collect();
        Ok(Chans::new(f64::from(rate), channels)?)
    }

    fn info_chans(
        &self,
        info: &penumbra_audio::file::Info,
        looping: bool,
        length: Option<u64>,
        show_rate: f64,
    ) -> Result<Chans, CookError> {
        let file_rate = f64::from(info.rate.max(1));
        let duration = length.map_or(0.0, |l| l as f64 / file_rate);
        let head = self.playhead.max(0.0);
        let (position, loops) = if duration > 0.0 && looping {
            let pass = (head / duration).floor();
            (head - pass * duration, pass)
        } else {
            (if duration > 0.0 { head.min(duration) } else { head }, 0.0)
        };
        let progress = if duration > 0.0 { (position / duration).clamp(0.0, 1.0) } else { 0.0 };
        let values = [
            ("position", position),
            ("progress", progress),
            ("loops", loops),
            ("duration", duration),
            ("rate", f64::from(info.rate)),
            ("channels", info.channels as f64),
            ("gaps", self.gaps as f64),
            ("seeks", self.status.seeks as f64),
            ("restarts", self.restarts as f64),
        ];
        let mut channels = Vec::with_capacity(values.len());
        for (name, v) in values {
            channels.push(Channel { name: Name::new(name)?, samples: vec![v] });
        }
        Ok(Chans::new(show_rate, channels)?)
    }
}

/// The channels `mode` asks for, from the file's.
fn map_channels(mut planes: Vec<Vec<f64>>, mode: &str) -> Vec<Vec<f64>> {
    match mode {
        "mono" if planes.len() > 1 => {
            let k = 1.0 / planes.len() as f64;
            let n = planes.first().map_or(0, Vec::len);
            let mut mono = vec![0.0; n];
            for p in &planes {
                for (m, s) in mono.iter_mut().zip(p) {
                    *m += s * k;
                }
            }
            vec![mono]
        }
        "stereo" if planes.len() == 1 => {
            let a = planes.pop().unwrap_or_default();
            vec![a.clone(), a]
        }
        "stereo" => {
            planes.truncate(2);
            planes
        }
        _ => planes,
    }
}

/// What a render needs to know about the file and the output.
struct Reader {
    file_rate: f64,
    out_rate: f64,
    channels: usize,
    looping: bool,
    cue: i64,
}

impl Reader {
    /// Adds `n` output samples from sample `s0` on, read along `m`, to `out` (one plane per file
    /// channel). Returns the gaps that began (frames not decoded yet); `missing` says whether
    /// one runs on from before and is left saying whether one runs on after.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        file: &mut SoundFile,
        kernel: &Kernel,
        m: &Mapping,
        s0: i64,
        n: usize,
        sc: &mut Scratch,
        out: &mut [Vec<f64>],
        gain: &mut Gain<'_>,
        missing: &mut bool,
    ) -> u64 {
        let ch = self.channels;
        let taps = kernel.taps();
        let delay = kernel.delay() as i64;
        let to_frames = self.file_rate;
        let exact = m.speed == 1.0 && self.file_rate == self.out_rate;
        sc.coeffs.resize(taps, 0.0);
        if sc.planar.len() != ch {
            sc.planar = vec![Vec::new(); ch];
        }
        let mut gaps = 0;
        let mut done = 0usize;
        while done < n {
            let len = CHUNK.min(n - done);
            let first = s0 + done as i64;
            if m.speed <= 0.0 {
                // Standing still: silent (a stop already faded through the tail).
                done += len;
                continue;
            }
            let xa = m.at(first).max(m.floor) * to_frames;
            let xb = m.at(first + len as i64 - 1).max(m.floor) * to_frames;
            if !(xa.is_finite() && xb.is_finite()) {
                done += len;
                continue;
            }
            let lo = xa.floor() as i64 - delay;
            let hi = xb.floor() as i64 + taps as i64 + 1;
            let count = usize::try_from(hi - lo).unwrap_or(0);
            file.read(lo, count, self.looping, self.cue, &mut sc.frames, &mut sc.found);
            for (c, p) in sc.planar.iter_mut().enumerate() {
                p.clear();
                p.extend(sc.frames.iter().skip(c).step_by(ch));
            }
            for j in 0..len {
                let i_out = done + j;
                let s = first + j as i64;
                let sec = m.at(s);
                if sec < m.floor {
                    continue;
                }
                let x = sec * to_frames;
                let mut base = x.floor();
                let mut frac = x - base;
                if exact && (frac < 1e-6 || frac > 1.0 - 1e-6) {
                    base = x.round();
                    frac = 0.0;
                }
                let k = base as i64 - lo;
                let center = usize::try_from(k).ok().and_then(|k| sc.found.get(k)).copied();
                match center {
                    Some(Frame::Missing) | None => {
                        if !*missing {
                            gaps += 1;
                        }
                        *missing = true;
                        if let Gain::Main { g, .. } = gain {
                            **g = 0.0;
                        }
                        continue;
                    }
                    Some(_) => *missing = false,
                }
                let g = match gain {
                    Gain::Main { g, step } => {
                        let now = **g;
                        **g = (now + *step).min(1.0);
                        now
                    }
                    Gain::Out { len } => 1.0 - ((done + j) as f64 / *len as f64).min(1.0),
                };
                if g <= 0.0 {
                    continue;
                }
                let k = k as usize;
                if exact && frac == 0.0 {
                    for (p, o) in sc.planar.iter().zip(out.iter_mut()) {
                        if let (Some(v), Some(o)) = (p.get(k), o.get_mut(i_out)) {
                            *o += f64::from(*v) * g;
                        }
                    }
                    continue;
                }
                kernel.coefficients(frac, &mut sc.coeffs);
                // The window over frames base - (delay - 1) .. + taps.
                let w0 = k + 1 - delay as usize;
                for (p, o) in sc.planar.iter().zip(out.iter_mut()) {
                    let window = p.get(w0..w0 + taps).unwrap_or(&[]);
                    if let Some(o) = o.get_mut(i_out) {
                        *o += f64::from(dot(&sc.coeffs, window)) * g;
                    }
                }
            }
            done += len;
        }
        gaps
    }
}
