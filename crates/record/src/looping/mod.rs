//! Loops of recordings: a movie made from a recording that a player can repeat without a
//! visible jump from the last frame to the first.
//!
//! - [`Method::Cut`] finds the stretch of the recording whose end looks most like its start
//!   ([`search`], on luma thumbnails of every frame) and keeps only that stretch.
//! - [`Method::Fade`] cross-fades the end of the recording into its start: the last frames are
//!   blended with the first ones, which the loop leaves out.
//! - [`Method::Pingpong`] plays the recording forward, then backward.
//! - [`Method::CutFade`] cuts, then fades over the seam.
//!
//! A loop runs as a [`Job`] on a thread of its own ([`Jobs::start`]). It decodes the recording
//! once for the thumbnails (cut only), then again frame by frame for the output, encoding as it
//! goes. Memory stays bounded: the thumbnails (576 bytes per frame), the start frames a fade
//! blends in, and for pingpong a stretch of at most [`REVERSE_BYTES`] decoded backward.

mod convert;
mod search;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_video::{ClipInfo, Decoder, Frame as Decoded, Limits, Timing, VideoFrame};
use serde::Serialize;

pub use convert::{THUMB, THUMB_H, THUMB_W, blend, thumbnail, to_planes};
pub use search::{Cut, Thumbs, find_cut, motion};

use crate::writer::{Settings, Sink, default_threads};
use crate::{Codec, Fps, H264Preset, Layout};

/// Bytes of decoded frames a pingpong holds while it plays a stretch backward.
pub const REVERSE_BYTES: usize = 256 << 20;
/// Shortest loop a cut looks for unless asked otherwise, in seconds.
pub const DEFAULT_MIN_SECONDS: f64 = 5.0;
/// Length of a fade unless asked otherwise, in seconds.
pub const DEFAULT_FADE_SECONDS: f64 = 1.0;
/// Finished jobs whose status stays readable.
const KEEP: usize = 32;

/// How a recording becomes a loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Cut,
    Fade,
    Pingpong,
    CutFade,
}

impl Method {
    /// The names a param or command uses, in [`Method::ALL`]'s order.
    pub const NAMES: [&'static str; 4] = ["cut", "fade", "pingpong", "cut_fade"];
    pub const ALL: [Method; 4] = [Method::Cut, Method::Fade, Method::Pingpong, Method::CutFade];

    pub fn parse(name: &str) -> Option<Method> {
        Self::NAMES.iter().zip(Self::ALL).find(|(n, _)| **n == name).map(|(_, m)| m)
    }

    pub fn name(self) -> &'static str {
        match self {
            Method::Cut => "cut",
            Method::Fade => "fade",
            Method::Pingpong => "pingpong",
            Method::CutFade => "cut_fade",
        }
    }

    /// Whether it searches for the best stretch.
    pub fn cuts(self) -> bool {
        matches!(self, Method::Cut | Method::CutFade)
    }

    /// Whether it cross-fades over the seam.
    pub fn fades(self) -> bool {
        matches!(self, Method::Fade | Method::CutFade)
    }
}

/// What loop to make.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub method: Method,
    /// Shortest loop a cut may find.
    pub min_seconds: f64,
    /// Length of the cross-fade.
    pub fade_seconds: f64,
    /// The file to write; `None` is `<stem>_loop.<ext>` beside the recording. Never
    /// overwritten: a name taken gets the first free `_<n>`.
    pub out: Option<PathBuf>,
    /// `None` keeps the recording's codec family: H.264 for H.264, ProRes for ProRes.
    pub codec: Option<Codec>,
}

impl Spec {
    pub fn new(method: Method) -> Spec {
        Spec {
            method,
            min_seconds: DEFAULT_MIN_SECONDS,
            fade_seconds: DEFAULT_FADE_SECONDS,
            out: None,
            codec: None,
        }
    }
}

/// A loop as far as it got.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LoopStatus {
    /// The recording.
    pub file: String,
    /// The loop written.
    pub out: String,
    pub method: String,
    pub codec: String,
    pub frames_in: u64,
    pub frames_out: u64,
    /// First frame of the recording in the loop (a cut).
    pub loop_start: Option<u64>,
    /// The frame after the loop's last (a cut): it looks most like `loop_start`.
    pub loop_end: Option<u64>,
    /// Frames cross-faded (a fade).
    pub fade_frames: u64,
    /// How well the cut's seam matches: the mean luma difference around it in 8-bit steps,
    /// 0 a perfect match.
    pub score: Option<f64>,
    /// How much the recording changes from one frame to the next, on the scale of `score`
    /// (a cut). A `score` no larger than this makes a seam as smooth as the rest of the movie.
    pub motion: Option<f64>,
    /// 0 to 1.
    pub progress: f64,
    pub done: bool,
    pub error: Option<String>,
    /// Seconds the job has taken.
    pub seconds: f64,
}

/// One loop being made, or made.
pub struct Job {
    status: Mutex<LoopStatus>,
    stop: AtomicBool,
    started: Instant,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.status().fmt(f)
    }
}

impl Job {
    fn new(status: LoopStatus) -> Job {
        Job { status: Mutex::new(status), stop: AtomicBool::new(false), started: Instant::now() }
    }

    pub fn status(&self) -> LoopStatus {
        lock(&self.status).clone()
    }

    /// Ends the job early; its file is removed.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn update(&self, f: impl FnOnce(&mut LoopStatus)) {
        let mut s = lock(&self.status);
        f(&mut s);
        if !s.done {
            s.seconds = self.started.elapsed().as_secs_f64();
        }
    }

    /// A job that failed before it started.
    pub fn failed(file: &Path, spec: &Spec, error: String) -> Arc<Job> {
        Arc::new(Job::new(LoopStatus {
            file: file.display().to_string(),
            method: spec.method.name().to_string(),
            done: true,
            error: Some(error),
            ..LoopStatus::default()
        }))
    }
}

#[derive(Default)]
struct Inner {
    jobs: Vec<Arc<Job>>,
    threads: Vec<JoinHandle<()>>,
    closed: bool,
}

/// The loop jobs of a process. Cheap to clone; every clone is the same list.
#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<Inner>>,
}

impl Jobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a loop of the recording `input`. What can be checked at once is checked here
    /// (the file opens as a movie, the loop fits into it, the output file can be created);
    /// the rest runs on a thread of its own. Refused while a loop of the same file runs.
    pub fn start(&self, input: &Path, spec: Spec) -> Result<Arc<Job>, String> {
        {
            let inner = lock(&self.inner);
            if inner.closed {
                return Err("the app is closing".into());
            }
            let busy = inner.jobs.iter().any(|j| {
                let s = j.status();
                !s.done && Path::new(&s.file) == input
            });
            if busy {
                return Err(format!("a loop of {} is being made already", input.display()));
            }
        }
        let plan = prepare(input, &spec)?;
        let job = Arc::new(Job::new(LoopStatus {
            file: input.display().to_string(),
            out: plan.out.display().to_string(),
            method: spec.method.name().to_string(),
            codec: plan.settings.codec.name().to_string(),
            frames_in: plan.frames as u64,
            ..LoopStatus::default()
        }));
        let mut inner = lock(&self.inner);
        if inner.closed {
            drop(plan.file);
            remove(&plan.out);
            return Err("the app is closing".into());
        }
        let worker = job.clone();
        let handle = std::thread::Builder::new()
            .name("loop".into())
            .spawn(move || work(plan, &worker))
            .map_err(|e| format!("cannot start the loop thread: {e}"))?;
        let (finished, running): (Vec<_>, Vec<_>) =
            std::mem::take(&mut inner.threads).into_iter().partition(JoinHandle::is_finished);
        inner.threads = running;
        for t in finished {
            if t.join().is_err() {
                log::error!("a loop thread panicked");
            }
        }
        inner.threads.push(handle);
        inner.jobs.push(job.clone());
        // Old finished jobs go, the newest stay.
        while inner.jobs.len() > KEEP {
            match inner.jobs.iter().position(|j| j.status().done) {
                Some(i) => {
                    inner.jobs.remove(i);
                }
                None => break,
            }
        }
        Ok(job)
    }

    /// Every job kept, oldest first.
    pub fn all(&self) -> Vec<LoopStatus> {
        lock(&self.inner).jobs.iter().map(|j| j.status()).collect()
    }

    /// No new jobs from now on.
    pub fn close(&self) {
        lock(&self.inner).closed = true;
    }

    /// Stops every job (their files are removed) and waits up to `timeout` for the threads;
    /// returns how many were still busy then.
    pub fn shutdown(&self, timeout: Duration) -> usize {
        let threads = {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            for j in &inner.jobs {
                j.stop();
            }
            std::mem::take(&mut inner.threads)
        };
        let deadline = Instant::now() + timeout;
        while threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut busy = 0;
        for t in threads {
            if t.is_finished() {
                if t.join().is_err() {
                    log::error!("a loop thread panicked");
                }
            } else {
                busy += 1;
            }
        }
        busy
    }
}

/// A loop checked and ready to run.
struct Plan {
    input: PathBuf,
    spec: Spec,
    frames: usize,
    settings: Settings,
    file: File,
    out: PathBuf,
    min_len: usize,
    fade: usize,
    /// Bytes of planes a pingpong holds going backward.
    reverse_bytes: usize,
}

/// The codec a loop of a clip gets unless asked: the clip's family.
pub fn codec_for(info: &ClipInfo, input: &Path) -> Codec {
    let c = info.codec.to_ascii_lowercase();
    if c.contains("prores") {
        if c.contains("4444") {
            Codec::Prores4444
        } else if c.contains("hq") {
            Codec::Prores422Hq
        } else {
            Codec::Prores422
        }
    } else if c.contains("264") || c.contains("avc") {
        Codec::H264
    } else if extension(input).as_deref() == Some("mov") {
        Codec::Prores422Hq
    } else {
        Codec::H264
    }
}

fn extension(p: &Path) -> Option<String> {
    p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// The file a loop of `input` goes to unless named: `<stem>_loop.<ext>` beside it. The
/// extension stays where the codec fits it (MOV takes any codec, MP4 only H.264).
pub fn default_out(input: &Path, codec: Codec) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = match extension(input).as_deref() {
        Some("mov") => "mov".to_string(),
        Some(e @ ("mp4" | "m4v")) if codec == Codec::H264 => e.to_string(),
        _ => codec.extension().to_string(),
    };
    input.with_file_name(format!("{stem}_loop.{ext}"))
}

/// Frames of `seconds` at `fps`, at least `min`.
fn frames_of(seconds: f64, fps: Fps, min: usize) -> usize {
    let f = (seconds.max(0.0) * fps.as_f64()).round();
    if f.is_finite() { (f as usize).max(min) } else { min }
}

fn fps_of(info: &ClipInfo) -> Result<Fps, String> {
    let fps = match info.timing {
        Timing::Rate { num, den } => match (u32::try_from(num), u32::try_from(den)) {
            (Ok(num), Ok(den)) if num > 0 && den > 0 => Some(Fps { num, den }),
            _ => None,
        },
        Timing::Ends(_) => Fps::from_f64(info.rate()),
    };
    fps.ok_or_else(|| format!("{:.3} frames per second cannot be encoded", info.rate()))
}

fn prepare(input: &Path, spec: &Spec) -> Result<Plan, String> {
    if !input.is_file() {
        return Err(format!("no file {}", input.display()));
    }
    let info = penumbra_video::open(input, &Limits::default())?.info().clone();
    let fps = fps_of(&info)?;
    let codec = spec.codec.unwrap_or_else(|| codec_for(&info, input));
    let (w, h) = (info.width & !1, info.height & !1);
    codec.layout(w, h).check()?;
    let n = info.frames;
    let min_len = frames_of(spec.min_seconds, fps, 2);
    let fade = if spec.method.fades() { frames_of(spec.fade_seconds, fps, 1) } else { 0 };
    if spec.method.cuts() && n < min_len + 1 {
        return Err(format!(
            "the recording has {n} frames ({:.1} s); a cut needs more than min_seconds ({:.1} s)",
            info.duration(),
            spec.min_seconds
        ));
    }
    if n < 2 {
        return Err("the recording has a single frame".into());
    }
    let mut out = spec.out.clone().unwrap_or_else(|| default_out(input, codec));
    if out.extension().is_none() {
        out.set_extension(codec.extension());
    }
    let ext = extension(&out);
    if codec != Codec::H264 && matches!(ext.as_deref(), Some("mp4" | "m4v")) {
        return Err(format!("{} goes into a .mov file, not .{}", codec.name(), ext.unwrap_or_default()));
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create the folder {}: {e}", dir.display()))?;
    }
    let (file, out) = penumbra_core::files::create_numbered(&out)?;
    let settings = Settings {
        path: out.clone(),
        codec,
        width: w,
        height: h,
        fps,
        threads: default_threads(),
        queue: 1,
        h264: H264Preset::Balanced,
    };
    Ok(Plan {
        input: input.to_path_buf(),
        spec: spec.clone(),
        frames: n,
        settings,
        file,
        out,
        min_len,
        fade,
        reverse_bytes: REVERSE_BYTES,
    })
}

fn remove(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        log::warn!("cannot remove the unfinished loop {}: {e}", path.display());
    }
}

/// Runs a plan on the job's thread and reports the end.
fn work(plan: Plan, job: &Job) {
    let out = plan.out.clone();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(plan, job)));
    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("the loop failed unexpectedly".to_string()),
    };
    if let Some(e) = &error {
        log::warn!("loop {}: {e}", out.display());
        remove(&out);
    }
    job.update(|s| {
        if error.is_none() {
            s.progress = 1.0;
        }
        s.error = error;
        s.done = true;
    });
}

/// Progress over one stretch of the work: `base` to `base + span` of the whole.
struct Progress<'a> {
    job: &'a Job,
    base: f64,
    span: f64,
    total: usize,
}

impl Progress<'_> {
    /// `done` of `total` frames; an error once the job is stopped.
    fn at(&self, done: usize) -> Result<(), String> {
        if self.job.stopped() {
            return Err("stopped".into());
        }
        if done.is_multiple_of(8) {
            let p = self.base + self.span * done as f64 / self.total.max(1) as f64;
            self.job.update(|s| s.progress = p.min(1.0));
        }
        Ok(())
    }
}

fn pixels(dec: &mut dyn Decoder, index: usize) -> Result<Arc<VideoFrame>, String> {
    match dec.frame(index).map_err(|e| format!("frame {index}: {e}"))? {
        Decoded::Pixels(f) => Ok(f),
        Decoded::Hap(_) => Err("HAP movies cannot be looped".into()),
    }
}

fn run(plan: Plan, job: &Job) -> Result<(), String> {
    let Plan { input, spec, frames: n, settings, file, out, min_len, fade, reverse_bytes } = plan;
    let mut dec = penumbra_video::open(&input, &Limits::default())?;
    let mut seg = (0, n);
    let cut_share = if spec.method.cuts() { 0.4 } else { 0.0 };
    if spec.method.cuts() {
        let progress = Progress { job, base: 0.0, span: cut_share, total: n };
        let mut thumbs = Thumbs::with_capacity(n);
        for i in 0..n {
            progress.at(i)?;
            match pixels(dec.as_mut(), i) {
                Ok(f) => thumbs.push(&thumbnail(&f)?),
                // The frame count comes from the file's duration and may be one too many.
                Err(_) if i + 1 == n && i > 0 => break,
                Err(e) => return Err(e),
            }
        }
        let stop = || job.stopped();
        let cut = find_cut(&thumbs, min_len, &stop)?;
        let motion = motion(&thumbs);
        seg = (cut.start, cut.end);
        job.update(|s| {
            s.motion = Some((motion * 100.0).round() / 100.0);
            s.loop_start = Some(cut.start as u64);
            s.loop_end = Some(cut.end as u64);
            s.score = Some((cut.score * 100.0).round() / 100.0);
        });
    }
    let mut sink = Sink::with_file(&settings, file, out)?;
    let layout = sink.layout();
    let progress = Progress { job, base: cut_share, span: 1.0 - cut_share, total: 0 };
    let written = if spec.method == Method::Pingpong {
        pingpong(dec.as_mut(), &mut sink, &layout, seg, reverse_bytes, progress)?
    } else {
        let len = seg.1 - seg.0;
        let fade = fade.min(len / 2);
        job.update(|s| s.fade_frames = fade as u64);
        linear(dec.as_mut(), &mut sink, &layout, seg, fade, progress)?
    };
    let frames = sink.finish()?;
    job.update(|s| s.frames_out = frames.max(written));
    Ok(())
}

/// Frames `seg.0..seg.1`, the last `fade` blended with the first `fade`, which are left out.
fn linear(
    dec: &mut dyn Decoder,
    sink: &mut Sink,
    layout: &Layout,
    seg: (usize, usize),
    fade: usize,
    progress: Progress<'_>,
) -> Result<u64, String> {
    let (a, b) = seg;
    let len = b - a;
    let progress = Progress { total: len, ..progress };
    let mut held: Vec<Vec<u8>> = Vec::with_capacity(fade);
    let mut buf = Vec::new();
    let mut written = 0u64;
    for k in 0..len {
        progress.at(k)?;
        let f = pixels(dec, a + k)?;
        to_planes(&f, layout, &mut buf)?;
        if k < fade {
            held.push(std::mem::take(&mut buf));
            continue;
        }
        if let Some(m) = k.checked_sub(len - fade) {
            // The start fades in over the end: weight 1/(F+1) at the first blended frame,
            // F/(F+1) at the last; the frame after that is the start's frame F.
            let w = (m + 1) as f64 / (fade + 1) as f64;
            if let Some(start) = held.get(m) {
                blend(&mut buf, start, w, layout.sample_bytes());
            }
        }
        sink.push(&buf)?;
        written += 1;
        progress.job.update(|s| s.frames_out = written);
    }
    Ok(written)
}

/// Frames `seg.0..seg.1` forward, then backward without the two ends (each plays once per
/// loop). Backward goes in stretches of at most `reverse_bytes`, each decoded forward.
fn pingpong(
    dec: &mut dyn Decoder,
    sink: &mut Sink,
    layout: &Layout,
    seg: (usize, usize),
    reverse_bytes: usize,
    progress: Progress<'_>,
) -> Result<u64, String> {
    let (a, b) = seg;
    let len = b - a;
    let back = len.saturating_sub(2);
    let progress = Progress { total: len + back, ..progress };
    let mut buf = Vec::new();
    let mut written = 0u64;
    for k in 0..len {
        progress.at(k)?;
        to_planes(pixels(dec, a + k)?.as_ref(), layout, &mut buf)?;
        sink.push(&buf)?;
        written += 1;
    }
    let chunk = (reverse_bytes / layout.bytes().max(1)).max(2);
    // Frames b - 2 down to a + 1.
    let mut hi = b.saturating_sub(1);
    let lo_end = a + 1;
    while hi > lo_end {
        let lo = hi.saturating_sub(chunk).max(lo_end);
        let mut stretch: Vec<Vec<u8>> = Vec::with_capacity(hi - lo);
        for i in lo..hi {
            progress.at(written as usize)?;
            let mut planes = Vec::new();
            to_planes(pixels(dec, i)?.as_ref(), layout, &mut planes)?;
            stretch.push(planes);
        }
        for planes in stretch.iter().rev() {
            sink.push(planes)?;
            written += 1;
            progress.job.update(|s| s.frames_out = written);
        }
        hi = lo;
    }
    Ok(written)
}
