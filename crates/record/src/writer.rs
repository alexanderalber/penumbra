//! One recording: a thread that encodes the frames it is handed and writes them into a movie
//! file, and the [`Recording`] handle that hands them over.
//!
//! The queue between them holds [`Settings::queue`] frames. [`Recording::push`] never waits: a
//! frame that does not fit comes back and the caller counts it as dropped. Frames carry their
//! position in the file ([`Frame::index`]); a frame lasts until the next one, so dropped or
//! missing frames make the one before longer and the file keeps time with the show.
//!
//! Dropping the [`Recording`] stops it: the thread encodes what is queued, then finishes the
//! file (writes its index, `moov`) and ends. A failure (the disk full, the folder not
//! writable, the encoder failing) ends the recording early; the file is still finished as far
//! as it got, where that is possible, and the error stays in the [`Status`].

use std::fs::File;
use std::io::BufWriter;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use filmcraft_isobmff as iso;
use penumbra_core::lock;

use crate::hub::Status;
use crate::looping::{Job, Jobs, Spec};
use crate::{Codec, Fps, Frame, H264Preset, Layout, Planes};

/// How long the thread waits for one frame's planes (a GPU readback) before giving up on it,
/// unless its [`crate::Hub`] says otherwise.
pub const PLANES_TIMEOUT: Duration = Duration::from_secs(5);
/// Write buffer in front of the file.
const WRITE_BUFFER: usize = 8 << 20;
/// How often a waiting thread looks at the stop flag.
const NAP: Duration = Duration::from_millis(100);

/// What to record and how.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The file wanted. Never overwritten: when it exists, the recording goes to the first free
    /// `<stem>_<n>.<ext>` (n from 2) beside it. Missing folders are created.
    pub path: PathBuf,
    pub codec: Codec,
    /// Even, see [`Layout::check`].
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    /// Encoder threads (at least 1).
    pub threads: usize,
    /// Frames the queue holds before [`Recording::push`] drops (at least 1).
    pub queue: usize,
    /// How hard the H.264 encoder works; ignored by ProRes.
    pub h264: H264Preset,
}

impl Settings {
    pub fn layout(&self) -> Layout {
        self.codec.layout(self.width, self.height)
    }
}

/// Encoder threads by default: all logical cores but two (the cook thread and the outputs keep
/// those), at least 2. Measured on an 8-core, 16-thread CPU recording a busy 1080p60 picture in
/// H.264: 8 threads managed 37 frames per second inside the running show, 12 dropped a few, 14
/// and 16 kept up.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(2)).max(2)
}

/// What the recording thread and its handle share.
pub(crate) struct Shared {
    status: Mutex<Status>,
    /// Ends the thread even while a handle lives (the app closing).
    pub(crate) stop: AtomicBool,
    queued: AtomicUsize,
    dropped: AtomicU64,
    skipped: AtomicU64,
    /// The highest frame index that came due, -1 before the first.
    due: AtomicI64,
    /// The loop to make of the file once it is finished.
    then: Mutex<Option<Spec>>,
    /// The loop being made of the file, or made.
    looping: Mutex<Option<Arc<Job>>>,
}

impl Shared {
    fn new(status: Status) -> Self {
        Self {
            status: Mutex::new(status),
            stop: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            due: AtomicI64::new(-1),
            then: Mutex::new(None),
            looping: Mutex::new(None),
        }
    }

    pub(crate) fn status(&self) -> Status {
        let mut s = lock(&self.status).clone();
        s.queue = self.queued.load(Ordering::Acquire);
        s.dropped = self.dropped.load(Ordering::Acquire);
        s.skipped = self.skipped.load(Ordering::Acquire);
        s.looping = lock(&self.looping).as_ref().map(|j| j.status());
        s
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        f(&mut lock(&self.status));
    }
}

/// The handle to a running recording. Dropping it stops the recording and finishes the file
/// (on the recording's own thread, so dropping never waits).
pub struct Recording {
    tx: Option<SyncSender<Frame>>,
    shared: Arc<Shared>,
    layout: Layout,
    fps: Fps,
}

impl Recording {
    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn fps(&self) -> Fps {
        self.fps
    }

    /// Queues `frame` without waiting. When the queue is full (the encoder is behind) or the
    /// recording has ended (an error), the frame comes back; the caller drops it and says so
    /// with [`Recording::dropped`].
    pub fn push(&self, frame: Frame) -> Result<(), Frame> {
        let Some(tx) = &self.tx else { return Err(frame) };
        self.shared.queued.fetch_add(1, Ordering::AcqRel);
        match tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(f) | TrySendError::Disconnected(f)) => {
                self.shared.queued.fetch_sub(1, Ordering::AcqRel);
                Err(f)
            }
        }
    }

    /// Whether a frame would fit into the queue now (so the caller can skip the work of making
    /// one). Racy by nature: [`Recording::push`] may still refuse it.
    pub fn has_room(&self, queue: usize) -> bool {
        self.shared.queued.load(Ordering::Acquire) < queue
    }

    /// Counts frames that were due but could not be taken (queue full, no room for a readback).
    pub fn dropped(&self, n: u64) {
        self.shared.dropped.fetch_add(n, Ordering::AcqRel);
    }

    /// Counts frames of the recording's rate the show did not cook (the show slower than the
    /// recording's rate); the frame before lasts longer instead.
    pub fn skipped(&self, n: u64) {
        self.shared.skipped.fetch_add(n, Ordering::AcqRel);
    }

    /// Says frame `index` came due, whether it was queued, dropped or skipped: the file lasts at
    /// least until that frame's end, so frames dropped at the very end make the last frame
    /// longer as they do anywhere else.
    pub fn due(&self, index: i64) {
        self.shared.due.fetch_max(index, Ordering::AcqRel);
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }

    /// Makes a loop of the file once it is finished, or none. The last call before the
    /// recording ends counts; a recording that fails makes no loop.
    pub fn then_loop(&self, spec: Option<Spec>) {
        *lock(&self.shared.then) = spec;
    }

    /// Whether it still takes frames (it ended when an error occurred).
    pub fn is_running(&self) -> bool {
        self.shared.status().recording
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.shared.update(|s| {
            if s.recording {
                s.recording = false;
                s.finishing = true;
            }
        });
        // Closing the channel tells the thread to finish.
        self.tx = None;
    }
}

/// Starts a recording: checks the settings and spawns its thread, which creates the file. A
/// file that cannot be created ends the recording at once with the error in its status.
/// `planes_timeout` is how long it waits for one frame's planes.
pub(crate) fn start(
    node: &str,
    s: Settings,
    planes_timeout: Duration,
    loops: Jobs,
) -> Result<(Recording, Arc<Shared>, JoinHandle<()>), String> {
    let layout = s.layout();
    layout.check()?;
    if s.fps.num == 0 || s.fps.den == 0 {
        return Err("the frame rate is 0".into());
    }
    let status = Status {
        node: node.to_string(),
        recording: true,
        finishing: false,
        file: s.path.display().to_string(),
        codec: s.codec.name().to_string(),
        width: s.width,
        height: s.height,
        fps: s.fps.as_f64(),
        ..Status::default()
    };
    let shared = Arc::new(Shared::new(status));
    let (tx, rx) = sync_channel::<Frame>(s.queue.max(1));
    let fps = s.fps;
    let thread_shared = shared.clone();
    let handle = std::thread::Builder::new()
        .name("record".into())
        .spawn(move || run(&s, &rx, &thread_shared, planes_timeout, &loops))
        .map_err(|e| format!("cannot start the recording thread: {e}"))?;
    Ok((Recording { tx: Some(tx), shared: shared.clone(), layout, fps }, shared, handle))
}

fn run(s: &Settings, rx: &Receiver<Frame>, shared: &Shared, planes_timeout: Duration, loops: &Jobs) {
    let outcome = catch_unwind(AssertUnwindSafe(|| record(s, rx, shared, planes_timeout)));
    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("the recording failed unexpectedly".to_string()),
    };
    if let Some(e) = &error {
        log::warn!("recording {}: {e}", s.path.display());
    }
    // The loop starts before the recording says it is done, so whoever waits for done sees it.
    let then = lock(&shared.then).take();
    if let (Some(spec), None) = (then, &error) {
        let file = PathBuf::from(shared.status().file);
        let job = loops.start(&file, spec.clone()).unwrap_or_else(|e| {
            log::warn!("no loop of {}: {e}", file.display());
            Job::failed(&file, &spec, e)
        });
        *lock(&shared.looping) = Some(job);
    }
    shared.update(|st| {
        st.recording = false;
        st.finishing = false;
        st.done = true;
        if error.is_some() {
            st.error = error;
        }
    });
    // Frames still queued are dropped with the receiver; they never reached the file.
    let lost = shared.queued.swap(0, Ordering::AcqRel);
    shared.dropped.fetch_add(lost as u64, Ordering::AcqRel);
}

/// One coded picture on its way into the file.
struct Packet {
    data: Vec<u8>,
    index: i64,
    sync: bool,
}

fn record(
    s: &Settings,
    rx: &Receiver<Frame>,
    shared: &Shared,
    planes_timeout: Duration,
) -> Result<(), String> {
    let mut sink = Sink::create(s, |path| {
        shared.update(|st| {
            st.file = path.display().to_string();
            st.opened = true;
        });
    })?;
    let mut last_index = -1i64;
    let mut busy: Option<f64> = None;
    let mut failure: Option<String> = None;
    loop {
        let mut frame = match rx.recv_timeout(NAP) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) if !shared.stop.load(Ordering::Acquire) => continue,
            Err(_) => break,
        };
        shared.queued.fetch_sub(1, Ordering::AcqRel);
        if frame.index <= last_index {
            continue;
        }
        last_index = frame.index;
        let took = match sink.encode(frame.index, frame.planes.as_mut(), planes_timeout) {
            Ok(t) => t,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        drop(frame);
        busy = Some(busy.map_or(took, |b| 0.9 * b + 0.1 * took));
        let seconds = (last_index + 1) as f64 / s.fps.as_f64();
        let (frames, bytes) = (sink.frames(), sink.bytes());
        shared.update(|st| {
            st.frames = frames;
            st.bytes = bytes;
            st.seconds = seconds;
            st.encoder_fps = busy.filter(|b| *b > 0.0).map_or(0.0, |b| 1.0 / b);
        });
    }
    shared.update(|st| {
        st.recording = false;
        st.finishing = true;
    });
    let closed = sink.close(failure, shared.due.load(Ordering::Acquire));
    shared.update(|st| {
        st.frames = closed.frames;
        st.bytes = closed.bytes;
        if let Some(seconds) = closed.seconds {
            st.seconds = seconds;
        }
    });
    closed.result
}

/// A movie file written frame after frame on the caller's thread. The recording thread uses
/// one, and so does anything that makes a movie from frames it has in memory (a loop of a
/// recording). [`Sink::push`] encodes the frame before it returns; nothing is dropped.
pub struct Sink {
    encoder: Encoder,
    mux: iso::Mp4Writer<BufWriter<File>>,
    track: usize,
    layout: Layout,
    fps: Fps,
    /// The last packet, held until the next says how long it lasts.
    held: Option<Packet>,
    written: Written,
    path: PathBuf,
    /// Index of the frame [`Sink::push`] writes next.
    next: i64,
}

/// What [`Sink::close`] reports.
struct Closed {
    frames: u64,
    bytes: u64,
    /// Length of the file, when a last frame was written.
    seconds: Option<f64>,
    result: Result<(), String>,
}

impl Sink {
    /// Creates the file `s.path` names and starts the encoder. An existing file is never
    /// overwritten: the sink takes the first free `<stem>_<n>.<ext>` beside it. Missing folders
    /// are created. `on_file` hears the name actually created, before the encoder starts.
    pub fn create(s: &Settings, on_file: impl FnOnce(&Path)) -> Result<Sink, String> {
        check(s)?;
        let path = s.path.as_path();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create the folder {}: {e}", dir.display()))?;
        }
        let (file, path) = penumbra_core::files::create_numbered(path)?;
        on_file(&path);
        Sink::with_file(s, file, path)
    }

    /// Writes into `file`, created empty as `path` (`s.path` is not looked at).
    pub fn with_file(s: &Settings, file: File, path: PathBuf) -> Result<Sink, String> {
        check(s)?;
        let layout = s.layout();
        let encoder = Encoder::new(s)?;
        let mp4 = s.codec == Codec::H264
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("m4v"));
        let mut opts = iso::WriterOptions::new(if mp4 { iso::Brand::Mp4 } else { iso::Brand::Mov });
        opts.metadata.push(("\u{a9}too".into(), "penumbra".into()));
        let mut mux =
            iso::Mp4Writer::new(BufWriter::with_capacity(WRITE_BUFFER, file), opts).map_err(write_error)?;
        let track = mux.add_track(encoder.track(&layout, s.fps, mp4)?).map_err(write_error)?;
        Ok(Sink {
            encoder,
            mux,
            track,
            layout,
            fps: s.fps,
            held: None,
            written: Written::default(),
            path,
            next: 0,
        })
    }

    /// The file written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The planes [`Sink::push`] takes.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Frames encoded so far.
    pub fn frames(&self) -> u64 {
        self.written.frames + u64::from(self.held.is_some())
    }

    /// Bytes of coded pictures so far.
    pub fn bytes(&self) -> u64 {
        self.written.bytes + self.held.as_ref().map_or(0, |h| h.data.len() as u64)
    }

    /// Encodes frame `index`, waiting up to `timeout` for its planes; says how long the work
    /// took in seconds.
    fn encode(&mut self, index: i64, planes: &mut dyn Planes, timeout: Duration) -> Result<f64, String> {
        let (packets, took) = self.encoder.encode(&self.layout, index, planes, timeout)?;
        self.written.pass(&mut self.mux, self.track, self.fps, &mut self.held, packets)?;
        Ok(took)
    }

    /// Encodes `planes` (as [`Sink::layout`] says) as the next frame, one frame long.
    pub fn push(&mut self, planes: &[u8]) -> Result<(), String> {
        let index = self.next;
        self.next += 1;
        self.encode(index, &mut Slice(planes), Duration::ZERO).map(|_| ())
    }

    /// Writes what the encoder still holds and finishes the file; returns the frames written.
    pub fn finish(self) -> Result<u64, String> {
        let closed = self.close(None, -1);
        closed.result.map(|()| closed.frames)
    }

    /// Ends the file. Without a `failure`, first encodes what the encoder still holds. The last
    /// frame lasts until the end of frame `due`, at least one frame. The file is finished as far
    /// as it got.
    fn close(mut self, mut failure: Option<String>, due: i64) -> Closed {
        if failure.is_none() {
            let rest = self.encoder.flush();
            let passed = rest.and_then(|rest| {
                self.written.pass(&mut self.mux, self.track, self.fps, &mut self.held, rest)
            });
            if let Err(e) = passed {
                failure = Some(e);
            }
        }
        let mut seconds = None;
        if let Some(h) = self.held.take() {
            let next = (h.index + 1).max(due.saturating_add(1));
            seconds = Some(next as f64 / self.fps.as_f64());
            if let Err(e) = self.written.put(&mut self.mux, self.track, self.fps, h, next)
                && failure.is_none()
            {
                failure = Some(e);
            }
        }
        let finished = self.mux.finish().map_err(write_error).and_then(|w| {
            let file = w.into_inner().map_err(|e| format!("cannot write the file: {}", e.error()))?;
            file.sync_all().map_err(|e| format!("cannot write the file: {e}"))
        });
        let result = match (failure, finished) {
            (Some(e), Ok(())) => Err(format!("{e} (the file is finished up to there)")),
            (Some(e), Err(_)) => Err(e),
            (None, Err(e)) => Err(e),
            (None, Ok(())) => Ok(()),
        };
        Closed { frames: self.written.frames, bytes: self.written.bytes, seconds, result }
    }
}

/// Whether the settings can be encoded at all.
fn check(s: &Settings) -> Result<(), String> {
    s.layout().check()?;
    if s.fps.num == 0 || s.fps.den == 0 {
        return Err("the frame rate is 0".into());
    }
    Ok(())
}

/// Planes borrowed for one [`Sink::push`].
struct Slice<'a>(&'a [u8]);

impl Planes for Slice<'_> {
    fn read(
        &mut self,
        _timeout: Duration,
        f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        f(self.0)
    }
}

/// Samples written so far. A packet is held until the next arrives, which says how long it lasts.
#[derive(Default)]
struct Written {
    frames: u64,
    bytes: u64,
}

impl Written {
    /// Writes the held packet and each of `packets` but the last, which is held instead.
    fn pass(
        &mut self,
        mux: &mut iso::Mp4Writer<BufWriter<File>>,
        track: usize,
        fps: Fps,
        held: &mut Option<Packet>,
        packets: Vec<Packet>,
    ) -> Result<(), String> {
        for p in packets {
            if let Some(h) = held.take() {
                self.put(mux, track, fps, h, p.index)?;
            }
            *held = Some(p);
        }
        Ok(())
    }

    /// Writes `p`, lasting until frame `next`.
    fn put(
        &mut self,
        mux: &mut iso::Mp4Writer<BufWriter<File>>,
        track: usize,
        fps: Fps,
        p: Packet,
        next: i64,
    ) -> Result<(), String> {
        let frames = u32::try_from((next - p.index).clamp(1, i64::from(u32::MAX))).unwrap_or(1);
        let duration = frames.saturating_mul(fps.den);
        let sample = iso::WriteSample { data: &p.data, duration, composition_offset: 0, is_sync: p.sync };
        mux.write_sample(track, sample).map_err(write_error)?;
        self.frames += 1;
        self.bytes += p.data.len() as u64;
        Ok(())
    }
}

fn write_error(e: iso::Error) -> String {
    format!("cannot write the file: {e}")
}

/// The encoders behind one interface: planes in, packets in file order out.
enum Encoder {
    H264(Box<filmcraft_h264enc::Encoder>),
    ProRes {
        encoder: filmcraft_prores::Encoder,
        frame: Box<filmcraft_prores::Frame>,
        pool: Option<rayon::ThreadPool>,
    },
}

impl Encoder {
    fn new(s: &Settings) -> Result<Self, String> {
        use filmcraft_prores::Profile;
        let threads = s.threads.max(1);
        let profile = match s.codec {
            Codec::H264 => {
                use filmcraft_h264enc as h;
                let mut cfg = h::EncoderConfig::new(s.width, s.height, s.fps.num, s.fps.den);
                s.h264.apply(&mut cfg);
                // A keyframe every 2 s, so a player can seek; no B-frames, so packets come in
                // the order they are shown and a frame can last any number of frames.
                cfg.keyint = (s.fps.as_f64() * 2.0).round().clamp(1.0, 600.0) as u32;
                cfg.bframes = 0;
                cfg.threads = threads;
                cfg.format = h::PacketFormat::LengthPrefixed;
                let enc = guarded(|| {
                    h::Encoder::new(cfg).map_err(|e| format!("cannot start the H.264 encoder: {e}"))
                })?;
                return Ok(Encoder::H264(Box::new(enc)));
            }
            Codec::Prores422 => Profile::Standard,
            Codec::Prores422Hq => Profile::Hq,
            Codec::Prores4444 => Profile::P4444,
        };
        let layout = s.layout();
        let mut cfg = filmcraft_prores::EncoderConfig::new(profile, s.width, s.height);
        cfg.encode_alpha = layout.alpha;
        cfg.threads = true;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("record-{i}"))
            .build()
            .map_err(|e| log::warn!("no own thread pool for the encoder, using the global one: {e}"))
            .ok();
        let frame = filmcraft_prores::Frame::new(
            s.width,
            s.height,
            profile.chroma(),
            layout.bits as u8,
            layout.alpha,
        );
        Ok(Encoder::ProRes {
            encoder: filmcraft_prores::Encoder::with_config(cfg),
            frame: Box::new(frame),
            pool,
        })
    }

    /// The track's sample entry: codec, size, colour (BT.709, video range).
    fn track(&self, layout: &Layout, fps: Fps, mp4: bool) -> Result<iso::TrackConfig, String> {
        let (w, h) = (
            u16::try_from(layout.width).map_err(|_| "too wide")?,
            u16::try_from(layout.height).map_err(|_| "too high")?,
        );
        let mut entry = match self {
            Encoder::H264(enc) => {
                let avcc = iso::AvcConfig::parse(&enc.avcc())
                    .map_err(|e| format!("the H.264 encoder's setup: {e}"))?;
                iso::SampleEntry::avc(avcc, w, h)
            }
            Encoder::ProRes { encoder, .. } => {
                iso::SampleEntry::prores(iso::FourCc(encoder.config().profile.fourcc()), w, h)
            }
        };
        if let Some(v) = entry.video.as_mut() {
            v.color = Some(if mp4 {
                iso::ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false }
            } else {
                iso::ColorInfo::Nclc { primaries: 1, transfer: 1, matrix: 1 }
            });
            v.pixel_aspect = Some((1, 1));
        }
        Ok(iso::TrackConfig::new(entry, fps.num))
    }

    /// Encodes frame `index` from `planes`, waiting up to `timeout` for them; also says how long the work took
    /// in seconds, from when the planes were there (not counting the wait for them).
    fn encode(
        &mut self,
        layout: &Layout,
        index: i64,
        planes: &mut dyn Planes,
        timeout: Duration,
    ) -> Result<(Vec<Packet>, f64), String> {
        let [oy, ou, ov, oa] = layout.offsets();
        let mut out = Vec::new();
        let mut started = None;
        match self {
            Encoder::H264(enc) => {
                let luma = layout.luma_stride() as usize;
                let chroma = layout.chroma_stride() as usize;
                planes.read(timeout, &mut |b| {
                    started = Some(Instant::now());
                    let plane = |from: usize, to: usize| b.get(from..to).ok_or("the frame is too short");
                    let yuv = filmcraft_h264enc::YuvFrame {
                        y: plane(oy, ou)?,
                        u: plane(ou, ov)?,
                        v: plane(ov, oa)?,
                        y_stride: luma,
                        uv_stride: chroma,
                    };
                    let packets = guarded(|| enc.encode(&yuv, index).map_err(|e| format!("H.264: {e}")))?;
                    out.extend(packets.into_iter().map(|p| Packet {
                        data: p.data,
                        index: p.pts,
                        sync: p.keyframe,
                    }));
                    Ok(())
                })?;
            }
            Encoder::ProRes { encoder, frame: pf, pool } => {
                let (w, h) = (layout.width as usize, layout.height as usize);
                let (cw, ch) = layout.chroma_size();
                let (cw, ch) = (cw as usize, ch as usize);
                let luma = layout.luma_stride() as usize * 2;
                let chroma = layout.chroma_stride() as usize * 2;
                planes.read(timeout, &mut |b| {
                    started = Some(Instant::now());
                    unpack(b, oy, luma, w, h, &mut pf.y)?;
                    unpack(b, ou, chroma, cw, ch, &mut pf.cb)?;
                    unpack(b, ov, chroma, cw, ch, &mut pf.cr)?;
                    if let Some(a) = pf.alpha.as_mut() {
                        unpack(b, oa, luma, w, h, a)?;
                    }
                    Ok(())
                })?;
                let pf: &filmcraft_prores::Frame = pf;
                let run = || encoder.encode(pf).map_err(|e| format!("ProRes: {e}"));
                let data = match pool {
                    Some(p) => guarded(|| p.install(run))?,
                    None => guarded(run)?,
                };
                out.push(Packet { data, index, sync: true });
            }
        }
        Ok((out, started.map_or(0.0, |t| t.elapsed().as_secs_f64())))
    }

    /// What the encoder still holds at the end.
    fn flush(&mut self) -> Result<Vec<Packet>, String> {
        match self {
            Encoder::H264(enc) => {
                let packets = guarded(|| Ok(enc.flush()))?;
                Ok(packets
                    .into_iter()
                    .map(|p| Packet { data: p.data, index: p.pts, sync: p.keyframe })
                    .collect())
            }
            Encoder::ProRes { .. } => Ok(Vec::new()),
        }
    }
}

/// Little-endian 16-bit samples of one plane (rows `stride` bytes apart, starting at `offset`)
/// into `dst`, `width` samples per row without padding.
fn unpack(
    b: &[u8],
    offset: usize,
    stride: usize,
    width: usize,
    height: usize,
    dst: &mut [u16],
) -> Result<(), String> {
    let short = || "the frame is too short".to_string();
    if width == 0 {
        return Ok(());
    }
    for (row, out) in dst.chunks_exact_mut(width).take(height).enumerate() {
        let start = offset + row * stride;
        let line = b.get(start..start + width * 2).ok_or_else(short)?;
        for (d, s) in out.iter_mut().zip(line.as_chunks::<2>().0) {
            *d = u16::from_le_bytes(*s);
        }
    }
    Ok(())
}

/// Runs foreign encoder code; a panic in it is an error, not the end of the app.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err("the encoder failed unexpectedly".into()))
}
