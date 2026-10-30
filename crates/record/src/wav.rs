//! Recording sound alone to a WAV file, on a thread of its own.
//!
//! - **Format:** 24-bit signed PCM or 32-bit float, little-endian, as many channels as the
//!   first block has (at most [`MAX_CHANNELS`]), at the first block's rate. Mono and stereo get
//!   the plain format (tag 1 for PCM, tag 3 for float, which every reader opens); more channels
//!   get `WAVE_FORMAT_EXTENSIBLE` without a speaker assignment. A block at another rate or with
//!   another channel count is left out and counted as refused.
//! - **The file appears with the first block:** before it, rate and channel count are unknown.
//!   A recording that never got a block writes no file.
//! - **Placement by sample number:** every [`AudioBlock`] says at which sample of the file it
//!   starts. The first block that reaches into the file is placed exactly (what lies before the
//!   file's start is cut). After that a block within [`audio::TOLERANCE_SECONDS`] of the end
//!   continues the file, a block up to [`JUMP_SECONDS`] ahead leaves silence for the time
//!   between, and a block further ahead or further back (a seek, a jump of the clock) continues
//!   the file without a gap.
//! - **A crash leaves a readable file:** the header is written with the sizes so far when the
//!   file is created and again at least every [`HEADER_EVERY`] while sound comes, with the
//!   samples handed to the operating system first. A crash loses at most that much of the end.
//!   Stopping writes the final sizes and syncs the file.
//! - **The limit:** a WAV file holds at most 4 GiB of samples (its sizes are 32-bit). A
//!   recording that reaches it is finished there and ends with an error.
//!
//! [`SoundRecording::push`] never waits for the disk: blocks queue for the thread, at most
//! [`QUEUE_SECONDS`] of them; more are dropped and counted.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use penumbra_core::lock;
use serde::Serialize;

use crate::audio::{self, AudioBlock, Placer};

/// Most channels a WAV recording takes.
pub const MAX_CHANNELS: usize = 64;
/// A block at most this far ahead of the end leaves silence for the time between; one further
/// ahead continues the file without a gap. Seconds.
pub const JUMP_SECONDS: f64 = 1.0;
/// How often the header gets the sizes so far while sound comes.
pub const HEADER_EVERY: Duration = Duration::from_secs(1);
/// Most sound waiting for the thread, seconds; more is dropped and counted.
pub const QUEUE_SECONDS: f64 = 30.0;
/// Most samples (all channels together) waiting for the thread, whatever the rate.
const QUEUE_VALUES: usize = 1 << 25;
/// How often a waiting thread looks at the stop flag.
const NAP: Duration = Duration::from_millis(100);
/// Write buffer in front of the file.
const WRITE_BUFFER: usize = 1 << 20;
/// Samples per channel written in one piece of silence.
const SILENCE_CHUNK: usize = 4096;

/// How the samples are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WavFormat {
    /// 24-bit signed integers, clipped to full scale.
    Pcm24,
    /// 32-bit floats, unclipped.
    Float32,
}

impl WavFormat {
    /// The names a param uses, in [`WavFormat::ALL`]'s order.
    pub const NAMES: [&'static str; 2] = ["pcm24", "float32"];
    pub const ALL: [WavFormat; 2] = [WavFormat::Pcm24, WavFormat::Float32];

    pub fn parse(name: &str) -> Option<WavFormat> {
        Self::NAMES.iter().zip(Self::ALL).find(|(n, _)| **n == name).map(|(_, f)| f)
    }

    pub fn name(self) -> &'static str {
        match self {
            WavFormat::Pcm24 => "pcm24",
            WavFormat::Float32 => "float32",
        }
    }

    /// Bytes per sample of one channel.
    pub fn bytes(self) -> usize {
        match self {
            WavFormat::Pcm24 => 3,
            WavFormat::Float32 => 4,
        }
    }

    /// The format tag of the plain header, and the sub-format of the extensible one.
    fn tag(self) -> u16 {
        match self {
            WavFormat::Pcm24 => 1,
            WavFormat::Float32 => 3,
        }
    }
}

/// What to record to.
#[derive(Clone, Debug)]
pub struct WavSettings {
    /// The file wanted. Never overwritten: when it exists, the recording goes to the first free
    /// `<stem>_<n>.<ext>` (n from 2) beside it. Missing folders are created.
    pub path: PathBuf,
    pub format: WavFormat,
}

/// A sound recording as far as it got.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SoundStatus {
    /// The node recording: its name here, its path from the engine's commands.
    pub node: String,
    /// Taking sound.
    pub recording: bool,
    /// Stopped; the queued sound is being written and the file finished.
    pub finishing: bool,
    /// Ended: the file is finished (or failed, see `error`).
    pub done: bool,
    /// The file's name once it is created; until then the name asked for, which gets a number
    /// when it is taken.
    pub file: String,
    /// The file is created and `file` is its final name.
    pub opened: bool,
    /// `pcm24` or `float32`.
    pub format: String,
    /// Samples per second; 0 before the first block.
    pub rate: u32,
    /// 0 before the first block.
    pub channels: usize,
    /// Length of the file so far.
    pub seconds: f64,
    /// Samples per channel in the file, silence put in included.
    pub samples: u64,
    /// Bytes of the file so far, header included.
    pub bytes: u64,
    /// Samples of silence put in where sound was missing (a jump ahead of under a second).
    pub gap_samples: u64,
    /// Samples left out: before the file's start, or already written over.
    pub cut_samples: u64,
    /// Blocks left out: another rate or channel count than the file's.
    pub refused: u64,
    /// Blocks lost before they reached the file (the disk did not keep up, or nobody took them
    /// in time).
    pub dropped: u64,
    /// The `out.audio` nodes heard, when the sound comes from all of them; `None` otherwise.
    pub sources: Option<usize>,
    pub error: Option<String>,
}

/// What the thread and the handle share.
pub(crate) struct Shared {
    status: Mutex<SoundStatus>,
    /// Ends the thread even while a handle lives (the app closing).
    pub(crate) stop: AtomicBool,
    /// Samples (all channels together) waiting for the thread.
    queued: AtomicUsize,
    /// What the caller says about the sound before it reached the recording.
    heard: Mutex<Heard>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Heard {
    sources: Option<usize>,
    lost: u64,
    refused: u64,
    /// Blocks dropped because the queue to the thread was full.
    dropped: u64,
}

impl Shared {
    pub(crate) fn status(&self) -> SoundStatus {
        let mut s = lock(&self.status).clone();
        let h = *lock(&self.heard);
        s.sources = h.sources;
        s.dropped += h.lost + h.dropped;
        s.refused += h.refused;
        s
    }

    fn update(&self, f: impl FnOnce(&mut SoundStatus)) {
        f(&mut lock(&self.status));
    }
}

/// The handle to a running sound recording. Dropping it stops the recording and finishes the
/// file (on the recording's own thread, so dropping never waits).
pub struct SoundRecording {
    tx: Option<Sender<AudioBlock>>,
    shared: Arc<Shared>,
}

impl SoundRecording {
    /// Queues `block` without waiting (the module doc says where it goes). Only when more than
    /// [`QUEUE_SECONDS`] wait already is it dropped and counted.
    pub fn push(&self, block: AudioBlock) {
        let Some(tx) = &self.tx else { return };
        let values = block.len() * block.channels.len().max(1);
        // `as`: seconds to a sample count.
        let most = ((f64::from(block.rate) * QUEUE_SECONDS) as usize)
            .saturating_mul(block.channels.len().max(1))
            .min(QUEUE_VALUES);
        if self.shared.queued.load(Ordering::Acquire) + values > most {
            lock(&self.shared.heard).dropped += 1;
            return;
        }
        self.shared.queued.fetch_add(values, Ordering::AcqRel);
        if tx.send(block).is_err() {
            self.shared.queued.fetch_sub(values, Ordering::AcqRel);
        }
    }

    /// Says the sound comes from `sources` outputs (`None`: from one input), and how many
    /// blocks were `lost` or `refused` before they reached the recording since its start.
    pub fn sound(&self, sources: Option<usize>, lost: u64, refused: u64) {
        let mut h = lock(&self.shared.heard);
        h.sources = sources;
        h.lost = lost;
        h.refused = refused;
    }

    pub fn status(&self) -> SoundStatus {
        self.shared.status()
    }

    /// Whether it still takes sound (it ended when an error occurred).
    pub fn is_running(&self) -> bool {
        lock(&self.shared.status).recording
    }
}

impl Drop for SoundRecording {
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

/// Starts a sound recording: spawns its thread, which creates the file with the first block.
pub(crate) fn start(
    node: &str,
    s: WavSettings,
) -> Result<(SoundRecording, Arc<Shared>, JoinHandle<()>), String> {
    let status = SoundStatus {
        node: node.to_string(),
        recording: true,
        file: s.path.display().to_string(),
        format: s.format.name().to_string(),
        ..SoundStatus::default()
    };
    let shared = Arc::new(Shared {
        status: Mutex::new(status),
        stop: AtomicBool::new(false),
        queued: AtomicUsize::new(0),
        heard: Mutex::new(Heard::default()),
    });
    let (tx, rx) = channel::<AudioBlock>();
    let thread_shared = shared.clone();
    let handle = std::thread::Builder::new()
        .name("record-sound".into())
        .spawn(move || run(&s, &rx, &thread_shared))
        .map_err(|e| format!("cannot start the recording thread: {e}"))?;
    Ok((SoundRecording { tx: Some(tx), shared: shared.clone() }, shared, handle))
}

fn run(s: &WavSettings, rx: &Receiver<AudioBlock>, shared: &Shared) {
    let outcome = catch_unwind(AssertUnwindSafe(|| record(s, rx, shared)));
    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("the recording failed unexpectedly".to_string()),
    };
    if let Some(e) = &error {
        log::warn!("recording {}: {e}", s.path.display());
    }
    shared.update(|st| {
        st.recording = false;
        st.finishing = false;
        st.done = true;
        if error.is_some() {
            st.error = error;
        }
    });
    shared.queued.store(0, Ordering::Release);
}

fn record(s: &WavSettings, rx: &Receiver<AudioBlock>, shared: &Shared) -> Result<(), String> {
    let mut file: Option<Wav> = None;
    let mut joiner = Joiner::default();
    let mut outcome = Ok(());
    let mut take = |block: AudioBlock, file: &mut Option<Wav>| -> Result<(), String> {
        let values = block.len() * block.channels.len().max(1);
        shared.queued.fetch_sub(values.min(shared.queued.load(Ordering::Acquire)), Ordering::AcqRel);
        if block.is_empty() {
            return Ok(());
        }
        let wav = match file {
            Some(w) => w,
            None => {
                let w = Wav::create(s, block.rate, block.channels.len())?;
                shared.update(|st| {
                    st.file = w.path.display().to_string();
                    st.opened = true;
                    st.rate = w.rate;
                    st.channels = w.channels;
                    st.bytes = w.bytes();
                });
                file.insert(w)
            }
        };
        if block.rate != wav.rate || block.channels.len() != wav.channels {
            shared.update(|st| st.refused += 1);
            return Ok(());
        }
        let len = block.len();
        let p = joiner.place(block.at, len, wav.rate);
        let skip = p.skip.min(len);
        let result = if joiner.placer.written == 0 { Ok(()) } else { wav.write(&block, p.pad, skip) };
        shared.update(|st| {
            st.cut_samples += u64::try_from(skip).unwrap_or(0);
            if joiner.placer.written > 0 {
                st.gap_samples += p.pad;
            }
            st.samples = wav.frames;
            // `as`: a sample count to seconds.
            st.seconds = wav.frames as f64 / f64::from(wav.rate.max(1));
            st.bytes = wav.bytes();
        });
        result
    };
    loop {
        let block = match rx.recv_timeout(NAP) {
            Ok(b) => b,
            Err(RecvTimeoutError::Timeout) if !shared.stop.load(Ordering::Acquire) => {
                if let Some(w) = file.as_mut()
                    && let Err(e) = w.patch_if_due()
                {
                    outcome = Err(e);
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        if let Err(e) = take(block, &mut file) {
            outcome = Err(e);
            break;
        }
    }
    shared.update(|st| {
        if st.recording {
            st.recording = false;
            st.finishing = true;
        }
    });
    if outcome.is_ok() {
        // What is queued still: its sender went with the handle, or goes with it soon (the app
        // closing stops the thread while the handle may still live).
        let deadline = Instant::now() + NAP;
        loop {
            match rx.try_recv() {
                Ok(block) => {
                    if let Err(e) = take(block, &mut file) {
                        outcome = Err(e);
                        break;
                    }
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) if Instant::now() >= deadline => break,
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
    let Some(wav) = file else { return outcome };
    let finished = wav.finish();
    if let Ok(bytes) = &finished {
        shared.update(|st| st.bytes = *bytes);
    }
    match (outcome, finished) {
        (Err(e), Ok(_)) => Err(format!("{e} (the file is finished up to there)")),
        (Err(e), Err(_)) | (Ok(()), Err(e)) => Err(e),
        (Ok(()), Ok(_)) => Ok(()),
    }
}

/// Where blocks go: [`Placer`] with the jumps of the module doc taken out.
#[derive(Default)]
pub(crate) struct Joiner {
    /// Added to every block's sample number: the jumps so far.
    offset: i64,
    pub placer: Placer,
}

impl Joiner {
    /// Where a block of `len` samples at file sample `at` goes, at `rate`; counts it as placed.
    pub fn place(&mut self, at: i64, len: usize, rate: u32) -> audio::Placement {
        let at = at.saturating_add(self.offset);
        let written = i64::try_from(self.placer.written).unwrap_or(i64::MAX);
        // `as`: seconds to whole samples.
        let jump = (JUMP_SECONDS * f64::from(rate.max(1))) as i64;
        let tolerance = (audio::TOLERANCE_SECONDS * f64::from(rate.max(1))) as i64;
        let len_i = i64::try_from(len).unwrap_or(i64::MAX);
        let d = at.saturating_sub(written);
        let far = if self.placer.written == 0 {
            // Before anything was written, a block wholly before the start is cut, unless it
            // lies so far back that the clock jumped.
            d > jump || at.saturating_add(len_i) <= -jump
        } else {
            d > jump || d < -tolerance
        };
        let at = if far {
            self.offset = self.offset.saturating_sub(d);
            written
        } else {
            at
        };
        self.placer.place(at, len, rate)
    }
}

/// The WAV file being written.
struct Wav {
    out: BufWriter<File>,
    path: PathBuf,
    format: WavFormat,
    rate: u32,
    channels: usize,
    /// Bytes of the header, up to the samples.
    header: usize,
    /// Samples per channel written.
    frames: u64,
    /// When the header last got the sizes.
    patched: Instant,
    /// Samples came since the header last got the sizes.
    dirty: bool,
}

impl Wav {
    fn create(s: &WavSettings, rate: u32, channels: usize) -> Result<Wav, String> {
        if rate == 0 {
            return Err("the sound has a rate of 0".into());
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(format!("the sound has {channels} channels; a recording takes 1 to {MAX_CHANNELS}"));
        }
        let path = s.path.as_path();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create the folder {}: {e}", dir.display()))?;
        }
        let (file, path) = penumbra_core::files::create_numbered(path)?;
        let mut wav = Wav {
            out: BufWriter::with_capacity(WRITE_BUFFER, file),
            path,
            format: s.format,
            rate,
            channels,
            header: 0,
            frames: 0,
            patched: Instant::now(),
            dirty: false,
        };
        let h = header(wav.format, rate, channels, 0);
        wav.header = h.len();
        wav.out.write_all(&h).map_err(|e| wav.error(&e))?;
        wav.out.flush().map_err(|e| wav.error(&e))?;
        Ok(wav)
    }

    fn error(&self, e: &std::io::Error) -> String {
        format!("cannot write {}: {e}", self.path.display())
    }

    fn frame_bytes(&self) -> u64 {
        (self.format.bytes() * self.channels) as u64
    }

    fn data_bytes(&self) -> u64 {
        self.frames * self.frame_bytes()
    }

    fn bytes(&self) -> u64 {
        self.header as u64 + self.data_bytes()
    }

    /// Frames that still fit under the 4 GiB limit of WAV's sizes.
    fn room(&self) -> u64 {
        let most = (u64::from(u32::MAX) - self.header as u64 - 1) / self.frame_bytes().max(1);
        most.saturating_sub(self.frames)
    }

    /// Writes `pad` samples of silence, then `block` from sample `skip` on.
    fn write(&mut self, block: &AudioBlock, pad: u64, skip: usize) -> Result<(), String> {
        let zeros = vec![0.0f32; SILENCE_CHUNK];
        let mut pad = pad;
        while pad > 0 {
            let k = usize::try_from(pad).unwrap_or(SILENCE_CHUNK).min(SILENCE_CHUNK);
            let part = zeros.get(..k).unwrap_or(&[]);
            let silence: Vec<&[f32]> = (0..self.channels).map(|_| part).collect();
            self.samples(&silence)?;
            pad -= k as u64;
        }
        let len = block.len();
        let channels: Vec<&[f32]> = block.channels.iter().map(|c| c.get(skip..len).unwrap_or(&[])).collect();
        self.samples(&channels)?;
        self.patch_if_due()
    }

    fn samples(&mut self, channels: &[&[f32]]) -> Result<(), String> {
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        let room = usize::try_from(self.room()).unwrap_or(usize::MAX);
        let fits = n.min(room);
        let data = match self.format {
            WavFormat::Pcm24 => audio::pcm24(channels, fits),
            WavFormat::Float32 => float32(channels, fits),
        };
        self.out.write_all(&data).map_err(|e| self.error(&e))?;
        self.frames += fits as u64;
        self.dirty = true;
        if fits < n {
            return Err("the file reached 4 GB, the most a WAV file holds".into());
        }
        Ok(())
    }

    /// Writes the sizes so far into the header, when [`HEADER_EVERY`] has passed.
    fn patch_if_due(&mut self) -> Result<(), String> {
        if self.dirty && self.patched.elapsed() >= HEADER_EVERY {
            self.patch()?;
        }
        Ok(())
    }

    /// Hands what is buffered to the operating system and writes the sizes into the header.
    fn patch(&mut self) -> Result<(), String> {
        let h = header(self.format, self.rate, self.channels, self.frames);
        self.out.flush().map_err(|e| self.error(&e))?;
        let f = self.out.get_mut();
        let done = f
            .seek(SeekFrom::Start(0))
            .and_then(|_| f.write_all(&h))
            .and_then(|()| f.seek(SeekFrom::End(0)).map(|_| ()));
        done.map_err(|e| self.error(&e))?;
        self.patched = Instant::now();
        self.dirty = false;
        Ok(())
    }

    /// Writes the final sizes (and the pad byte an odd length needs) and syncs the file; says
    /// how long the file is.
    fn finish(mut self) -> Result<u64, String> {
        if self.data_bytes() % 2 == 1 {
            self.out.write_all(&[0]).map_err(|e| self.error(&e))?;
        }
        self.patch()?;
        let bytes = self.bytes() + self.data_bytes() % 2;
        let file = self.out.into_inner().map_err(|e| format!("cannot write the file: {}", e.error()))?;
        file.sync_all().map_err(|e| format!("cannot write the file: {e}"))?;
        Ok(bytes)
    }
}

/// The first `n` samples of each channel, interleaved, as 32-bit little-endian floats.
/// Non-numbers are silence.
fn float32(channels: &[&[f32]], n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * channels.len() * 4);
    for k in 0..n {
        for c in channels {
            let v = c.get(k).copied().unwrap_or(0.0);
            let v = if v.is_finite() { v } else { 0.0 };
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// The tail of the extensible format's sub-format GUID, after the format tag.
const GUID_TAIL: [u8; 12] = [0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71];

/// The header of a WAV file in `format` with `frames` samples per channel: `RIFF`, `fmt `,
/// `fact` (every format but plain PCM) and the head of `data`. Its length depends on format and
/// channel count only, so it can be written again over itself with new sizes.
pub(crate) fn header(format: WavFormat, rate: u32, channels: usize, frames: u64) -> Vec<u8> {
    let bytes = format.bytes();
    let ch = u16::try_from(channels).unwrap_or(u16::MAX);
    let align = u16::try_from(bytes * channels).unwrap_or(u16::MAX);
    let bits = u16::try_from(bytes * 8).unwrap_or(32);
    let extensible = channels > 2;
    let mut fmt = Vec::with_capacity(40);
    fmt.extend_from_slice(&(if extensible { 0xFFFE } else { format.tag() }).to_le_bytes());
    fmt.extend_from_slice(&ch.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&rate.saturating_mul(u32::from(align)).to_le_bytes());
    fmt.extend_from_slice(&align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        // No speaker assignment: the channels are what the show made them.
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&u32::from(format.tag()).to_le_bytes());
        fmt.extend_from_slice(&GUID_TAIL);
    } else if format != WavFormat::Pcm24 {
        fmt.extend_from_slice(&0u16.to_le_bytes());
    }
    let fact = extensible || format != WavFormat::Pcm24;
    let data = u32::try_from(frames.saturating_mul(u64::from(align))).unwrap_or(u32::MAX);
    let mut h = Vec::with_capacity(80);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&u32::try_from(fmt.len()).unwrap_or(0).to_le_bytes());
    h.extend_from_slice(&fmt);
    if fact {
        h.extend_from_slice(b"fact");
        h.extend_from_slice(&4u32.to_le_bytes());
        h.extend_from_slice(&u32::try_from(frames).unwrap_or(u32::MAX).to_le_bytes());
    }
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    // RIFF's size counts everything after its own 8 bytes, the pad byte of odd data included.
    let riff = (h.len() as u64 - 8 + u64::from(data) + u64::from(data % 2)).min(u64::from(u32::MAX));
    if let Some(r) = h.get_mut(4..8) {
        r.copy_from_slice(&u32::try_from(riff).unwrap_or(u32::MAX).to_le_bytes());
    }
    h
}

/// A WAV file read back: for tests and checks.
#[derive(Clone, Debug, PartialEq)]
pub struct WavFile {
    pub format: WavFormat,
    pub rate: u32,
    /// One vector per channel, of equal length.
    pub channels: Vec<Vec<f32>>,
    /// The format tag of `fmt `: 1, 3 or 0xFFFE.
    pub tag: u16,
}

/// Reads a WAV file this module wrote (24-bit PCM or 32-bit float, plain or extensible). The
/// sizes in the header count; a `data` size beyond the file's end reads what is there.
pub fn read(path: &Path) -> Result<WavFile, String> {
    let b = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let u16_at = |i: usize| b.get(i..).and_then(|s| s.first_chunk::<2>()).map(|s| u16::from_le_bytes(*s));
    let u32_at = |i: usize| b.get(i..).and_then(|s| s.first_chunk::<4>()).map(|s| u32::from_le_bytes(*s));
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err("not a WAV file".into());
    }
    let mut at = 12;
    let mut fmt = None;
    while let (Some(id), Some(size)) = (b.get(at..at + 4), u32_at(at + 4)) {
        let size = size as usize;
        let body = at + 8;
        if id == b"fmt " {
            fmt = Some(body);
        } else if id == b"data" {
            let f = fmt.ok_or("no fmt chunk before the data")?;
            let tag = u16_at(f).ok_or("short fmt")?;
            let channels = usize::from(u16_at(f + 2).ok_or("short fmt")?);
            let rate = u32_at(f + 4).ok_or("short fmt")?;
            let bits = u16_at(f + 14).ok_or("short fmt")?;
            let sub = if tag == 0xFFFE { u16_at(f + 24).ok_or("short fmt")? } else { tag };
            let format = match (sub, bits) {
                (1, 24) => WavFormat::Pcm24,
                (3, 32) => WavFormat::Float32,
                other => return Err(format!("cannot read format {other:?}")),
            };
            let end = body.saturating_add(size).min(b.len());
            let data = b.get(body..end).unwrap_or(&[]);
            let mut out = vec![Vec::new(); channels];
            for frame in data.chunks_exact(format.bytes() * channels.max(1)) {
                for (c, s) in out.iter_mut().zip(frame.chunks_exact(format.bytes())) {
                    let mut q = [0u8; 4];
                    match format {
                        // Sign-extend the 24-bit sample from its top byte.
                        WavFormat::Pcm24 => q.iter_mut().skip(1).zip(s).for_each(|(d, v)| *d = *v),
                        WavFormat::Float32 => q.iter_mut().zip(s).for_each(|(d, v)| *d = *v),
                    }
                    c.push(match format {
                        // `as`: a sample to f32.
                        WavFormat::Pcm24 => (f64::from(i32::from_le_bytes(q) >> 8) / 8_388_608.0) as f32,
                        WavFormat::Float32 => f32::from_le_bytes(q),
                    });
                }
            }
            return Ok(WavFile { format, rate, channels: out, tag });
        }
        at = body + size + size % 2;
    }
    Err("no data chunk".into())
}
