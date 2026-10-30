//! Sound files: WAV, FLAC, MP3, Ogg Vorbis and the AAC track of MP4, MOV and M4A, decoded on a
//! thread of their own (symphonia) so that a reader never waits for the disk or a decoder.
//!
//! A [`SoundFile`] opens its file on its thread and then takes one of two ways:
//!
//! - **In memory:** a file whose decoded samples fit in [`MEMORY_BYTES`] (about 87 s of 48 kHz
//!   stereo) is decoded whole, once. Loops and restarts are then instant and reads never miss.
//! - **Streamed:** a longer file, or one whose length the header does not give, is decoded into
//!   a window that runs [`AHEAD_SECONDS`] ahead of where the reader last read and keeps
//!   [`BEHIND_SECONDS`] behind it. Reading far outside the window makes the thread seek there.
//!   While looping, the thread decodes on through the end into the start again, so the seam
//!   is in the window like any other frame. A copy of the first second after the cue point
//!   ([`SoundFile::read`]'s `cue`) is kept as well, so a restart sounds at once.
//!
//! Frames are counted **unwrapped**: when looping, frame `n * length + f` is frame `f` of the
//! file in pass `n`. A read reports every frame as data, outside the file (before 0, past the
//! end without loop: silence by nature) or missing (not decoded yet: an underrun).
//!
//! Hostile files end in an error or a shorter sound, never in a crash: rates outside
//! [`MIN_RATE`]..[`MAX_RATE`] and more than [`MAX_CHANNELS`] channels are refused, a bad packet is
//! skipped (after [`MAX_BAD_PACKETS`] in a row the rest counts as the end), the window is capped
//! at [`WINDOW_BYTES`], tags and pictures at [`TAG_BYTES`], and a panic inside the decoder is
//! caught and reported as an error. symphonia reserves memory on the word of header fields, so
//! before it reads a file the headers it trusts are checked against the file: MP4 boxes, sample
//! tables and descriptors, ID3 frames, FLAC metadata, WAV chunks, and Ogg Vorbis comments and
//! codebooks. Only codecs whose headers are checked this way are decoded: PCM, FLAC, MP3, AAC
//! and Vorbis; ALAC, MP1, MP2, Ogg FLAC and Ogg Opus are refused.

use std::collections::VecDeque;
use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use penumbra_core::lock;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Limit, MetadataOptions};
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

/// File name extensions offered for sound files.
pub const EXTENSIONS: &[&str] = &["wav", "wave", "flac", "mp3", "ogg", "oga", "m4a", "mp4", "mov", "aac"];
/// Files whose decoded samples (as 32-bit floats) fit in this many bytes are decoded whole.
pub const MEMORY_BYTES: usize = 32 << 20;
/// How far a streamed file is decoded ahead of the reader.
pub const AHEAD_SECONDS: f64 = 4.0;
/// How much of a streamed file stays decoded behind the reader.
pub const BEHIND_SECONDS: f64 = 0.5;
/// Most bytes of a streamed file's window (a very high rate or many channels shorten it).
pub const WINDOW_BYTES: usize = 32 << 20;
/// Seconds after the cue point a streamed file keeps decoded for restarts.
pub const CUE_SECONDS: f64 = 1.0;
/// Rates accepted, samples per second.
pub const MIN_RATE: u32 = 1_000;
pub const MAX_RATE: u32 = 768_000;
/// Most channels of a file.
pub const MAX_CHANNELS: usize = 64;
/// Bad packets in a row after which the rest of the file counts as its end.
pub const MAX_BAD_PACKETS: u32 = 64;
/// Most bytes of a tag or embedded picture read along.
pub const TAG_BYTES: usize = 1 << 20;
/// Most samples one decoded packet may hold (all channels).
const MAX_PACKET_SAMPLES: usize = 1 << 22;
/// A read this far past the decoded end makes the thread seek instead of decoding on to it.
const SEEK_SLACK_SECONDS: f64 = 0.25;
/// How long the thread sleeps when there is nothing to decode, at most.
const IDLE: Duration = Duration::from_millis(20);

/// What the file holds, known once it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// The codec's short name: `pcm_s16le`, `flac`, `mp3`, `vorbis`, `aac`, ...
    pub codec: String,
    pub rate: u32,
    pub channels: usize,
    /// The length in frames: exact once the whole file was decoded, else the header's word
    /// (`None` when it has none).
    pub frames: Option<u64>,
    /// Whether `frames` comes from decoding to the end.
    pub exact: bool,
    /// Decoded whole into memory (else streamed).
    pub in_memory: bool,
}

impl Info {
    /// The length in seconds, when known.
    pub fn seconds(&self) -> Option<f64> {
        self.frames.map(|f| f as f64 / f64::from(self.rate.max(1)))
    }
}

/// How a [`SoundFile`] stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub info: Option<Info>,
    /// Why the file cannot be played; a sound that was read so far stays readable.
    pub error: Option<String>,
    /// Samples can be read: decoded whole, or streaming has begun.
    pub ready: bool,
    /// Seeks the thread made (a streamed file only).
    pub seeks: u64,
    /// Packets skipped as undecodable.
    pub bad_packets: u64,
}

/// What [`SoundFile::read`] found for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Decoded samples.
    Data,
    /// Before the start, or past the end: silence by nature.
    Outside,
    /// Not decoded yet: silence for now (an underrun).
    Missing,
}

/// The decoded samples of a streamed file around the reader.
#[derive(Default)]
struct Window {
    /// Unwrapped frame of the first sample in `samples`.
    start: i64,
    /// Interleaved.
    samples: VecDeque<f32>,
}

#[derive(Default)]
struct Shared {
    status: Status,
    /// The whole file, interleaved (in memory).
    memory: Option<Arc<[f32]>>,
    window: Window,
    /// The first [`CUE_SECONDS`] from `cue.0` (a file frame), interleaved.
    cue: Option<(i64, Vec<f32>)>,
    /// The file's length in frames, once its end was reached.
    length: Option<u64>,
    /// The unwrapped frame the decoding stopped at (the end, not looping).
    end: Option<i64>,
    /// Where the reader last read, whether it loops and where its cue point is.
    wanted: i64,
    looping: bool,
    cue_wanted: i64,
    stop: bool,
}

type Pair = (Mutex<Shared>, Condvar);

/// A sound file being decoded on its own thread. Dropping it stops the thread (without waiting
/// for it).
pub struct SoundFile {
    shared: Arc<Pair>,
    /// The whole file and its channels, once the reader saw it (in memory: no lock per read).
    memory: Option<(Arc<[f32]>, usize)>,
}

impl SoundFile {
    /// Starts opening `path` on a thread of its own. Never fails: errors show in [`Self::status`].
    /// `ready` is called once the file is open (or failed), from that thread.
    pub fn open(path: &Path, ready: Option<Box<dyn Fn() + Send>>) -> Self {
        Self::open_with(path, MEMORY_BYTES, ready)
    }

    /// [`Self::open`] with another limit for decoding whole (0 streams every file).
    pub fn open_with(path: &Path, memory_bytes: usize, ready: Option<Box<dyn Fn() + Send>>) -> Self {
        let shared: Arc<Pair> = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let theirs = shared.clone();
        let path = path.to_path_buf();
        let spawned = std::thread::Builder::new().name("audio file".into()).spawn(move || {
            let result =
                catch_unwind(AssertUnwindSafe(|| run(&path, &theirs, memory_bytes, ready.as_deref())));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("the decoder failed on this file".to_string()),
            };
            if let Some(e) = error {
                lock(&theirs.0).status.error = Some(e);
                if let Some(f) = ready.as_deref() {
                    f();
                }
            }
        });
        if let Err(e) = spawned {
            lock(&shared.0).status.error = Some(format!("cannot start decoding: {e}"));
        }
        Self { shared, memory: None }
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.0).status.clone()
    }

    /// The file's length in frames: exact once known, else the header's.
    pub fn length(&self) -> Option<u64> {
        let s = lock(&self.shared.0);
        s.length.or(s.status.info.as_ref().and_then(|i| i.frames))
    }

    /// Reads `count` frames from unwrapped frame `from` into `out` (interleaved, `channels` per
    /// frame; resized to fit) and what was found per frame into `found`. `looping` wraps frames
    /// past the end to the start. `cue` is the frame a restart goes to: a streamed file keeps
    /// the second after it decoded. The decoding of a streamed file follows `from`. Nothing
    /// before the file is open: every frame is [`Frame::Missing`].
    pub fn read(
        &mut self,
        from: i64,
        count: usize,
        looping: bool,
        cue: i64,
        out: &mut Vec<f32>,
        found: &mut Vec<Frame>,
    ) {
        found.clear();
        found.resize(count, Frame::Missing);
        if self.memory.is_none() {
            let mut s = lock(&self.shared.0);
            if let Some(m) = &s.memory {
                let channels = s.status.info.as_ref().map_or(1, |i| i.channels.max(1));
                self.memory = Some((m.clone(), channels));
            } else {
                let channels = s.status.info.as_ref().map_or(1, |i| i.channels.max(1));
                out.clear();
                out.resize(count.saturating_mul(channels), 0.0);
                if s.status.ready {
                    read_window(&s, from, looping, channels, out, found);
                }
                let moved = s.wanted != from || s.looping != looping || s.cue_wanted != cue;
                s.wanted = from;
                s.looping = looping;
                s.cue_wanted = cue;
                drop(s);
                if moved {
                    self.shared.1.notify_all();
                }
                return;
            }
        }
        let Some((m, channels)) = &self.memory else { return };
        let channels = *channels;
        out.clear();
        out.resize(count.saturating_mul(channels), 0.0);
        let len = (m.len() / channels) as i64;
        for (i, (frame, f)) in out.chunks_exact_mut(channels).zip(found.iter_mut()).enumerate() {
            let u = from.saturating_add(i as i64);
            let at = if u < 0 || len == 0 {
                None
            } else if looping {
                Some(u.rem_euclid(len))
            } else {
                (u < len).then_some(u)
            };
            match at.and_then(|a| usize::try_from(a).ok()) {
                Some(a) => {
                    if let Some(src) = m.get(a * channels..(a + 1) * channels) {
                        frame.copy_from_slice(src);
                    }
                    *f = Frame::Data;
                }
                None => *f = Frame::Outside,
            }
        }
    }
}

impl Drop for SoundFile {
    fn drop(&mut self) {
        lock(&self.shared.0).stop = true;
        self.shared.1.notify_all();
    }
}

/// [`SoundFile::read`] of a streamed file, under the lock.
fn read_window(s: &Shared, from: i64, looping: bool, channels: usize, out: &mut [f32], found: &mut [Frame]) {
    let w = &s.window;
    let have = (w.samples.len() / channels) as i64;
    let length = s.length.map(|l| l as i64);
    let cue = s.cue.as_ref().map(|(at, c)| (*at, c, (c.len() / channels) as i64));
    for (i, (frame, f)) in out.chunks_exact_mut(channels).zip(found.iter_mut()).enumerate() {
        let u = from.saturating_add(i as i64);
        let past_end = !looping && (length.is_some_and(|l| u >= l) || s.end.is_some_and(|e| u >= e));
        if u < 0 || past_end {
            *f = Frame::Outside;
            continue;
        }
        let k = u - w.start;
        if (0..have).contains(&k) {
            let at = (k as usize) * channels;
            for (c, o) in frame.iter_mut().enumerate() {
                *o = w.samples.get(at + c).copied().unwrap_or(0.0);
            }
            *f = Frame::Data;
            continue;
        }
        // The cue copy holds file frames of the first pass only.
        if let Some((at, c, n)) = cue {
            let k = u - at;
            if (0..n).contains(&k)
                && let Some(src) = c.get(k as usize * channels..(k as usize + 1) * channels)
            {
                frame.copy_from_slice(src);
                *f = Frame::Data;
            }
        }
    }
}

/// An open file and its decoder.
struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    channels: usize,
    buffer: Option<SampleBuffer<f32>>,
    /// After a seek: the decoder frame decoding should start at (earlier frames are dropped).
    seek_to: Option<u64>,
    /// Decoder frames before the file's frame 0 (an MP4 edit list skipping the encoder's
    /// priming), and the file's length its edit list gives.
    lead: u64,
    limit: Option<u64>,
    /// Frames decoded but not handed out yet (learning the format from the first packet).
    carry: Vec<f32>,
    bad_in_row: u32,
    bad: u64,
}

enum Next {
    /// Frames appended.
    Frames(usize),
    End,
}

impl Source {
    fn open(path: &Path) -> Result<(Self, Info), String> {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if mp4_magic(path) {
            mp4_sane(path)?;
        }
        let tag_end = id3_sane(path)?;
        flac_sane(path, tag_end)?;
        riff_sane(path)?;
        ogg_sane(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format_options = FormatOptions { enable_gapless: true, ..FormatOptions::default() };
        let meta = MetadataOptions {
            limit_metadata_bytes: Limit::Maximum(TAG_BYTES),
            limit_visual_bytes: Limit::Maximum(TAG_BYTES),
        };
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_options, &meta)
            .map_err(|e| format!("not a sound file penumbra can read ({e})"))?;
        let format = probed.format;
        let codecs = symphonia::default::get_codecs();
        let name = |t: &symphonia::core::formats::Track| {
            codecs.get_codec(t.codec_params.codec).map(|d| d.short_name)
        };
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && name(t).is_some_and(supported_codec))
            .ok_or_else(|| match format.tracks().iter().find_map(name) {
                Some(other) => {
                    format!("the {other} sound in this file is not supported: convert it to WAV, FLAC or MP3")
                }
                None => "the file has no sound track penumbra can decode".to_string(),
            })?;
        let params = track.codec_params.clone();
        let track_id = track.id;
        let codec = codecs.get_codec(params.codec).map_or("?", |d| d.short_name).to_string();
        let decoder = codecs
            .make(&params, &DecoderOptions::default())
            .map_err(|e| format!("cannot decode the {codec} sound ({e})"))?;
        let mut source = Source {
            format,
            decoder,
            track: track_id,
            time_base: params.time_base,
            rate: params.sample_rate.unwrap_or(0),
            channels: params.channels.map_or(0, |c| c.count()),
            buffer: None,
            seek_to: None,
            lead: 0,
            limit: None,
            carry: Vec::new(),
            bad_in_row: 0,
            bad: 0,
        };
        if source.rate == 0 || source.channels == 0 {
            // The header does not say: the first packet does.
            source.learn()?;
        }
        if !(MIN_RATE..=MAX_RATE).contains(&source.rate) {
            return Err(format!(
                "a sample rate of {} Hz is not supported (from {MIN_RATE} to {MAX_RATE})",
                source.rate
            ));
        }
        if source.channels == 0 || source.channels > MAX_CHANNELS {
            return Err(format!("{} channels are not supported (from 1 to {MAX_CHANNELS})", source.channels));
        }
        let mut frames = params.n_frames.map(|n| source.ts_to_frames(n)).filter(|n| *n > 0);
        if let Some(edit) = mp4_edit(path, track_id) {
            source.lead = source.ts_to_frames(edit.media_time);
            if source.lead > 0 {
                source.carry.clear();
                source.seek_to = Some(source.lead);
            }
            frames = frames.map(|f| f.saturating_sub(source.lead));
            if let Some((duration, scale)) = edit.duration.filter(|(d, s)| *d > 0 && *s > 0) {
                let f = u128::from(duration) * u128::from(source.rate) / u128::from(scale);
                let f = u64::try_from(f).unwrap_or(u64::MAX);
                source.limit = Some(f);
                frames = Some(frames.map_or(f, |x| x.min(f)));
            }
        }
        let info = Info {
            codec,
            rate: source.rate,
            channels: source.channels,
            frames,
            exact: false,
            in_memory: false,
        };
        Ok((source, info))
    }

    /// Decodes until the first sound to learn the rate and channels; keeps what it decoded.
    fn learn(&mut self) -> Result<(), String> {
        let mut scratch = Vec::new();
        for _ in 0..MAX_BAD_PACKETS * 4 {
            match self.packet(&mut scratch, true)? {
                Next::Frames(n) if n > 0 => {
                    self.carry = scratch;
                    return Ok(());
                }
                Next::Frames(_) => {}
                Next::End => break,
            }
        }
        Err("the file holds no sound".into())
    }

    fn ts_to_frames(&self, ts: u64) -> u64 {
        match self.time_base {
            Some(tb) if tb.denom > 0 => {
                let f = u128::from(ts) * u128::from(tb.numer) * u128::from(self.rate) / u128::from(tb.denom);
                u64::try_from(f).unwrap_or(u64::MAX)
            }
            _ => ts,
        }
    }

    fn frames_to_ts(&self, frames: u64) -> u64 {
        match self.time_base {
            Some(tb) if tb.numer > 0 && self.rate > 0 => {
                let t = u128::from(frames) * u128::from(tb.denom)
                    / (u128::from(tb.numer) * u128::from(self.rate));
                u64::try_from(t).unwrap_or(u64::MAX)
            }
            _ => frames,
        }
    }

    /// Appends the next frames to `out` (interleaved, `self.channels` per frame).
    fn next(&mut self, out: &mut Vec<f32>) -> Result<Next, String> {
        if !self.carry.is_empty() {
            let n = self.carry.len() / self.channels.max(1);
            out.append(&mut self.carry);
            return Ok(Next::Frames(n));
        }
        self.packet(out, false)
    }

    /// Decodes one packet into `out`. With `learning`, takes the rate and channels from it.
    fn packet(&mut self, out: &mut Vec<f32>, learning: bool) -> Result<Next, String> {
        let packet = match self.format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(Next::End);
            }
            Err(SymError::ResetRequired) => return Ok(Next::End),
            Err(SymError::DecodeError(_)) => return self.bad_packet(),
            Err(e) => {
                log::warn!("sound file: reading stopped early: {e}");
                return Ok(Next::End);
            }
        };
        if packet.track_id() != self.track {
            return Ok(Next::Frames(0));
        }
        let at = self.ts_to_frames(packet.ts());
        let decoded = match self.decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_) | SymError::IoError(_)) => return self.bad_packet(),
            Err(SymError::ResetRequired) => {
                self.decoder.reset();
                return Ok(Next::Frames(0));
            }
            Err(e) => {
                log::warn!("sound file: decoding stopped early: {e}");
                return Ok(Next::End);
            }
        };
        self.bad_in_row = 0;
        let spec = *decoded.spec();
        let have = spec.channels.count();
        if learning {
            self.rate = spec.rate;
            self.channels = have;
        }
        let frames = decoded.frames();
        if have == 0 || frames == 0 {
            return Ok(Next::Frames(0));
        }
        let capacity = decoded.capacity().max(frames);
        if capacity.saturating_mul(have) > MAX_PACKET_SAMPLES {
            return self.bad_packet();
        }
        let fits = self.buffer.as_ref().is_some_and(|b| b.capacity() >= capacity * have);
        if !fits {
            self.buffer = Some(SampleBuffer::new(capacity as u64, spec));
        }
        let Some(buffer) = self.buffer.as_mut() else { return Ok(Next::Frames(0)) };
        buffer.copy_interleaved_ref(decoded);
        let samples = buffer.samples();
        // After a seek, frames before the target are dropped.
        let mut skip = 0usize;
        if let Some(target) = self.seek_to {
            skip = usize::try_from(target.saturating_sub(at)).unwrap_or(usize::MAX).min(frames);
            if skip < frames {
                self.seek_to = None;
            }
        }
        // The edit list's end.
        let mut kept = frames - skip;
        if let Some(limit) = self.limit {
            let first = (at + skip as u64).saturating_sub(self.lead);
            let allowed = usize::try_from(limit.saturating_sub(first)).unwrap_or(usize::MAX);
            if allowed == 0 {
                return Ok(Next::End);
            }
            kept = kept.min(allowed);
        }
        let want = self.channels.max(1);
        out.reserve(kept * want);
        for frame in samples.chunks_exact(have).skip(skip).take(kept) {
            for c in 0..want {
                out.push(frame.get(c).copied().filter(|v| v.is_finite()).unwrap_or(0.0));
            }
        }
        Ok(Next::Frames(kept))
    }

    fn bad_packet(&mut self) -> Result<Next, String> {
        self.bad += 1;
        self.bad_in_row += 1;
        if self.bad_in_row >= MAX_BAD_PACKETS {
            log::warn!("sound file: {MAX_BAD_PACKETS} bad packets in a row; the rest counts as the end");
            return Ok(Next::End);
        }
        Ok(Next::Frames(0))
    }

    /// Seeks to file frame `frame`: the next frames decoded start there (or at the end).
    fn seek(&mut self, frame: u64) -> Result<(), String> {
        self.carry.clear();
        let frame = frame.saturating_add(self.lead);
        let ts = self.frames_to_ts(frame);
        match self.format.seek(SeekMode::Accurate, SeekTo::TimeStamp { ts, track_id: self.track }) {
            Ok(_) => {
                self.decoder.reset();
                self.seek_to = Some(frame);
                Ok(())
            }
            Err(e) => Err(format!("cannot seek in the file ({e})")),
        }
    }
}

/// The decoding thread.
fn run(
    path: &Path,
    shared: &Arc<Pair>,
    memory_bytes: usize,
    ready: Option<&(dyn Fn() + Send)>,
) -> Result<(), String> {
    let (mut source, mut info) = Source::open(path)?;
    let bytes_per_frame = 4 * info.channels;
    let fits = info
        .frames
        .is_some_and(|f| usize::try_from(f).is_ok_and(|f| f.saturating_mul(bytes_per_frame) <= memory_bytes));
    if fits {
        if let Some(all) = decode_whole(&mut source, shared, info.frames.unwrap_or(0), memory_bytes)? {
            let frames = (all.len() / info.channels) as u64;
            info.frames = Some(frames);
            info.exact = true;
            info.in_memory = true;
            let mut s = lock(&shared.0);
            s.length = Some(frames);
            s.memory = Some(all.into());
            s.status.info = Some(info);
            s.status.ready = true;
            s.status.bad_packets = source.bad;
            drop(s);
            if let Some(f) = ready {
                f();
            }
            return Ok(());
        }
        // Longer than the header said: streamed after all, from the start.
        let (again, again_info) = Source::open(path)?;
        source = again;
        info = again_info;
        info.frames = None;
    }
    lock(&shared.0).status.info = Some(info.clone());
    stream(&mut source, &info, path, shared, ready)
}

/// Decodes the whole file. `None` when it turns out longer than `memory_bytes`.
fn decode_whole(
    source: &mut Source,
    shared: &Arc<Pair>,
    frames: u64,
    memory_bytes: usize,
) -> Result<Option<Vec<f32>>, String> {
    let budget = memory_bytes / 4 / source.channels.max(1) * source.channels.max(1);
    let mut all: Vec<f32> =
        Vec::with_capacity(usize::try_from(frames).unwrap_or(0).saturating_mul(source.channels).min(budget));
    let mut packet = Vec::new();
    loop {
        if lock(&shared.0).stop {
            return Ok(Some(all));
        }
        packet.clear();
        match source.next(&mut packet)? {
            Next::Frames(_) => {}
            Next::End => break,
        }
        let need = all.len() + packet.len();
        if need > budget {
            return Ok(None);
        }
        // Grown by hand, so the buffer never passes the budget.
        if need > all.capacity() {
            let want = all.capacity().saturating_mul(2).max(need).min(budget);
            all.reserve_exact(want - all.len());
        }
        all.extend_from_slice(&packet);
    }
    all.shrink_to_fit();
    Ok(Some(all))
}

/// Streams a file into the window around the reader until told to stop.
/// Ready (and `ready` called) once the cue point is decoded, so a start there has no gap.
fn stream(
    source: &mut Source,
    info: &Info,
    path: &Path,
    shared: &Arc<Pair>,
    mut ready: Option<&(dyn Fn() + Send)>,
) -> Result<(), String> {
    let ch = info.channels;
    let rate = f64::from(info.rate);
    let cap_frames = (WINDOW_BYTES / 4 / ch).max(1024) as i64;
    let ahead = ((AHEAD_SECONDS * rate) as i64).min(cap_frames * 8 / 10).max(1);
    let behind = ((BEHIND_SECONDS * rate) as i64).min(cap_frames / 10);
    let slack = (SEEK_SLACK_SECONDS * rate) as i64;
    let cue_frames = ((CUE_SECONDS * rate) as i64).min(cap_frames / 8).max(1);
    // The unwrapped frame of file frame 0 in this pass, and the next file frame decoded.
    let mut base: i64 = 0;
    let mut file_pos: u64 = 0;
    let mut ended = false;
    let mut cue_at: Option<i64> = None;
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let (wanted, looping, cue_wanted, window_start, length) = {
            let s = lock(&shared.0);
            if s.stop {
                return Ok(());
            }
            (s.wanted, s.looping, s.cue_wanted.max(0), s.window.start, s.length.or(info.frames))
        };
        let next_label = base.saturating_add(file_pos as i64);
        let length_i = length.map(|l| l as i64).filter(|l| *l > 0);

        // The copy of the cue point for restarts.
        if cue_at != Some(cue_wanted) {
            cue_at = Some(cue_wanted);
            let cue = decode_cue(source, cue_wanted, cue_frames, ch)?;
            let copied = cue.as_ref().map_or(0, |c| c.len() / ch) as i64;
            // A reader playing elsewhere keeps its window: the decoder goes back to where the
            // window ends. Else the window goes on from the cue copy.
            let elsewhere =
                was_ready(shared) && !(cue_wanted..cue_wanted + copied.max(1)).contains(&wanted.max(0));
            let resumed = elsewhere && file_pos > 0 && source.seek(file_pos).is_ok();
            let mut s = lock(&shared.0);
            if !resumed {
                s.window = Window { start: cue_wanted, samples: cue.iter().flatten().copied().collect() };
                s.end = None;
                base = 0;
                file_pos = cue_wanted as u64 + copied as u64;
                ended = false;
            }
            s.cue = cue.map(|mut c| {
                c.truncate(usize::try_from(cue_frames).unwrap_or(0) * ch);
                (cue_wanted, c)
            });
            s.status.bad_packets = source.bad;
            s.status.ready = true;
            drop(s);
            if let Some(f) = ready.take() {
                f();
            }
            continue;
        }

        // Before the file is the start of it, for the decoder.
        let wanted = wanted.max(0);
        let outside = wanted < window_start || wanted > next_label.saturating_add(slack);
        if outside {
            let target = wanted.max(0);
            let (file_frame, new_base) = match (looping, length_i) {
                (true, Some(l)) => (target.rem_euclid(l), target - target.rem_euclid(l)),
                _ => (target, 0),
            };
            let past_end = !looping && length_i.is_some_and(|l| file_frame >= l);
            let mut s = lock(&shared.0);
            s.window = Window { start: target, samples: VecDeque::new() };
            s.end = past_end.then_some(target);
            s.status.seeks += 1;
            drop(s);
            base = new_base;
            file_pos = file_frame as u64;
            ended = past_end;
            if !past_end && let Err(e) = source.seek(file_pos) {
                // A file that cannot seek is read again from its start, dropping what
                // lies before the target.
                log::warn!("sound file {}: {e}", path.display());
                let (again, _) = Source::open(path)?;
                *source = again;
                source.carry.clear();
                source.seek_to = Some(file_pos + source.lead);
            }
            continue;
        }

        // Forget what lies behind the reader.
        {
            let mut s = lock(&shared.0);
            let drop_frames = (wanted - behind - s.window.start).max(0);
            let have = (s.window.samples.len() / ch) as i64;
            let n = drop_frames.min(have);
            if n > 0 {
                s.window.samples.drain(..(n as usize) * ch);
                s.window.start += n;
            }
        }

        if ended && looping {
            // Loop turned on after the end: on into the start.
            if source.seek(0).is_ok() {
                base = next_label;
                file_pos = 0;
                ended = false;
                lock(&shared.0).end = None;
                continue;
            }
        }
        if ended || next_label - wanted >= ahead {
            let s = lock(&shared.0);
            if !s.stop && s.wanted == wanted && s.looping == looping && s.cue_wanted.max(0) == cue_wanted {
                let _ = shared.1.wait_timeout(s, IDLE);
            }
            continue;
        }

        buf.clear();
        match source.next(&mut buf)? {
            Next::Frames(n) => {
                file_pos += n as u64;
                let mut s = lock(&shared.0);
                s.window.samples.extend(buf.iter().copied());
                // Over the cap (a reader stuck far behind): the oldest go.
                let over = s.window.samples.len().saturating_sub(cap_frames as usize * ch);
                let over = over.div_ceil(ch) * ch;
                if over > 0 {
                    let over = over.min(s.window.samples.len());
                    s.window.samples.drain(..over);
                    s.window.start += (over / ch) as i64;
                }
                s.status.bad_packets = source.bad;
            }
            Next::End => {
                let mut s = lock(&shared.0);
                if file_pos > 0 && s.length.is_none_or(|l| l != file_pos) {
                    s.length = Some(file_pos);
                    if let Some(i) = s.status.info.as_mut() {
                        i.frames = Some(file_pos);
                        i.exact = true;
                    }
                }
                let looping = s.looping;
                drop(s);
                if looping && file_pos > 0 && source.seek(0).is_ok() {
                    base = next_label;
                    file_pos = 0;
                } else {
                    ended = true;
                    lock(&shared.0).end = Some(next_label);
                }
            }
        }
    }
}

/// Whether the file was ready before (the reader has been reading).
fn was_ready(shared: &Arc<Pair>) -> bool {
    lock(&shared.0).status.ready
}

/// Decodes at least `frames` from file frame `at` (or less at the end), in whole packets.
fn decode_cue(source: &mut Source, at: i64, frames: i64, ch: usize) -> Result<Option<Vec<f32>>, String> {
    if source.seek(at.max(0) as u64).is_err() {
        return Ok(None);
    }
    let want = usize::try_from(frames).unwrap_or(0) * ch;
    let mut out = Vec::with_capacity(want);
    for _ in 0..100_000 {
        if out.len() >= want {
            break;
        }
        match source.next(&mut out)? {
            Next::Frames(_) => {}
            Next::End => break,
        }
    }
    Ok(Some(out))
}

/// Sample formats [`wav_bytes`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    Pcm32,
    Float32,
}

/// A WAV file in memory, `samples` interleaved in -1..1: for tests and fixtures.
pub fn wav_bytes(rate: u32, channels: u16, format: WavFormat, samples: &[f64]) -> Vec<u8> {
    let (bits, tag): (u16, u16) = match format {
        WavFormat::Pcm16 => (16, 1),
        WavFormat::Pcm24 => (24, 1),
        WavFormat::Pcm32 => (32, 1),
        WavFormat::Float32 => (32, 3),
    };
    let block = channels * bits / 8;
    let data_len = samples.len() as u32 * u32::from(bits / 8);
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&tag.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    b.extend_from_slice(&block.to_le_bytes());
    b.extend_from_slice(&bits.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for &v in samples {
        let v = v.clamp(-1.0, 1.0);
        // `as`: scaled into the integer range first.
        match format {
            WavFormat::Pcm16 => b.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
            WavFormat::Pcm24 => {
                let x = (v * 8_388_607.0).round() as i32;
                b.extend_from_slice(x.to_le_bytes().get(..3).unwrap_or(&[0, 0, 0]));
            }
            WavFormat::Pcm32 => b.extend_from_slice(&((v * 2_147_483_647.0).round() as i32).to_le_bytes()),
            WavFormat::Float32 => b.extend_from_slice(&(v as f32).to_le_bytes()),
        }
    }
    b
}

/// The first edit of an MP4 or MOV track: where in the media it starts (track time base) and
/// how long it lasts (movie time base, and that base).
struct Edit {
    media_time: u64,
    duration: Option<(u64, u32)>,
}

/// Most boxes [`mp4_edit`] looks at.
const MAX_BOXES: usize = 4096;

/// A box inside an MP4: its type, where its body starts and where it ends.
type Mp4Box = ([u8; 4], u64, u64);

/// The children of the box whose body is `start..end`, reading headers only. Stops at the first
/// that does not fit.
fn mp4_children(f: &mut File, start: u64, end: u64, seen: &mut usize) -> Vec<Mp4Box> {
    mp4_walk(f, start, end, seen).0
}

/// [`mp4_children`], and whether every child fitted.
fn mp4_walk(f: &mut File, start: u64, end: u64, seen: &mut usize) -> (Vec<Mp4Box>, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let mut out = Vec::new();
    let mut at = start;
    while at.saturating_add(8) <= end && *seen < MAX_BOXES {
        *seen += 1;
        let mut h = [0u8; 8];
        if f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(&mut h).is_err() {
            break;
        }
        let short = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let kind = [h[4], h[5], h[6], h[7]];
        let (len, body) = match short {
            0 => (end - at, at + 8),
            1 => {
                let mut l = [0u8; 8];
                if f.read_exact(&mut l).is_err() {
                    break;
                }
                (u64::from_be_bytes(l), at + 16)
            }
            n => (n, at + 8),
        };
        if len < body - at || at.saturating_add(len) > end {
            return (out, false);
        }
        out.push((kind, body, at + len));
        at += len;
    }
    (out, true)
}

/// Boxes that hold boxes, as far as the sound's tables lie in them.
const MP4_CONTAINERS: &[&[u8; 4]] =
    &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"udta", b"mvex", b"moof", b"traf"];

/// Checks what symphonia sizes its memory by before it reads an MP4 or MOV: every box inside
/// its parent, every table's entry count within its box, every sample no larger than the file.
/// A file failing this would make the demuxer reserve gigabytes on the word of a header.
fn mp4_sane(path: &Path) -> Result<(), String> {
    let bad = || "the MP4's tables do not fit the file".to_string();
    let Ok(mut f) = File::open(path) else { return Ok(()) };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut seen = 0usize;
    // (body start, end, whether the box is a tag list, whose items all hold boxes)
    let mut stack = vec![(0u64, size, false)];
    while let Some((start, end, tags)) = stack.pop() {
        let (boxes, fits) = mp4_walk(&mut f, start, end, &mut seen);
        // Trailing junk at the top level is common; inside a box it is not.
        if !fits && start != 0 {
            return Err(bad());
        }
        for (kind, body, end) in boxes {
            if MP4_CONTAINERS.contains(&&kind) || tags {
                stack.push((body, end, &kind == b"ilst"));
                continue;
            }
            if &kind == b"meta" {
                // A full box (version and flags first) in MP4, a plain one in QuickTime.
                let head = mp4_read(&mut f, body, 4).unwrap_or_default();
                let skip = if head == [0, 0, 0, 0] { 4 } else { 0 };
                stack.push((body + skip, end, false));
                continue;
            }
            if &kind == b"ilst" {
                stack.push((body, end, true));
                continue;
            }
            if &kind == b"stsd" {
                stsd_sane(&mut f, body, end, &mut seen).ok_or_else(bad)?;
                continue;
            }
            let entry: u64 = match &kind {
                b"stco" | b"stss" => 4,
                b"co64" | b"stts" | b"ctts" => 8,
                b"stsc" => 12,
                b"stsz" => 4,
                _ => continue,
            };
            let head =
                mp4_read(&mut f, body, usize::try_from((end - body).min(12)).unwrap_or(0)).ok_or_else(bad)?;
            if &kind == b"stsz" {
                let constant = be32(&head, 4).ok_or_else(bad)?;
                let count = u64::from(be32(&head, 8).ok_or_else(bad)?);
                if constant > 0 {
                    if u64::from(constant) > size {
                        return Err(bad());
                    }
                    continue;
                }
                if body + 12 + count * entry > end {
                    return Err(bad());
                }
                // Every sample's size, read in pieces.
                let mut at = body + 12;
                let mut left = count;
                while left > 0 {
                    let n = left.min(16_384);
                    let chunk = mp4_read(&mut f, at, (n * 4) as usize).ok_or_else(bad)?;
                    if chunk.as_chunks::<4>().0.iter().any(|c| u64::from(u32::from_be_bytes(*c)) > size) {
                        return Err(bad());
                    }
                    at += n * 4;
                    left -= n;
                }
                continue;
            }
            let count = u64::from(be32(&head, 4).ok_or_else(bad)?);
            if body + 8 + count * entry > end {
                return Err(bad());
            }
        }
    }
    Ok(())
}

/// `n` bytes at `at`.
fn mp4_read(f: &mut File, at: u64, n: usize) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut b = vec![0u8; n];
    f.seek(SeekFrom::Start(at)).ok()?;
    f.read_exact(&mut b).ok()?;
    Some(b)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The edit list of the `track`-th track (counted from 0, as symphonia numbers them) of an MP4
/// or MOV file: AAC sound starts with the encoder's priming, which the edit list skips. `None`
/// for other files and tracks without one. Reads box headers and the few bytes it needs, so a
/// large file costs a few seeks.
fn mp4_edit(path: &Path, track: u32) -> Option<Edit> {
    if !mp4_magic(path) {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut seen = 0usize;
    let top = mp4_children(&mut f, 0, size, &mut seen);
    let &(_, ms, me) = top.iter().find(|(k, ..)| k == b"moov")?;
    let moov = mp4_children(&mut f, ms, me, &mut seen);
    // The movie's time scale, for edit durations.
    let movie_scale = moov.iter().find(|(k, ..)| k == b"mvhd").and_then(|&(_, b, e)| {
        let v = mp4_read(&mut f, b, usize::try_from((e - b).min(24)).ok()?)?;
        be32(&v, if v.first() == Some(&1) { 20 } else { 12 })
    });
    let &(_, ts, te) = moov.iter().filter(|(k, ..)| k == b"trak").nth(usize::try_from(track).ok()?)?;
    let trak = mp4_children(&mut f, ts, te, &mut seen);
    let &(_, es, ee) = trak.iter().find(|(k, ..)| k == b"edts")?;
    let edts = mp4_children(&mut f, es, ee, &mut seen);
    let &(_, ls, le) = edts.iter().find(|(k, ..)| k == b"elst")?;
    let body = mp4_read(&mut f, ls, usize::try_from((le - ls).min(8 + 20 * 4)).ok()?)?;
    let version = *body.first()?;
    let count = be32(&body, 4)?;
    let entry = if version == 1 { 20 } else { 12 };
    // Leading empty edits (media time -1) would delay the track; they are passed over.
    for i in 0..(count as usize).min(4) {
        let e = body.get(8 + i * entry..8 + (i + 1) * entry)?;
        let (duration, media) = if version == 1 {
            (
                u64::from_be_bytes(e.get(0..8)?.try_into().ok()?),
                i64::from_be_bytes(e.get(8..16)?.try_into().ok()?),
            )
        } else {
            (u64::from(be32(e, 0)?), i64::from(i32::from_be_bytes(e.get(4..8)?.try_into().ok()?)))
        };
        if media >= 0 {
            return Some(Edit {
                media_time: u64::try_from(media).ok()?,
                duration: movie_scale.map(|s| (duration, s)),
            });
        }
    }
    None
}

/// Whether the file starts as an MP4 or MOV does.
fn mp4_magic(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && matches!(&head[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip")
}

/// Checks an ID3v2 tag at the start of a file (MP3, also some others): the tag within the file
/// and every frame within the tag. symphonia reserves a frame's memory on the word of its
/// header, up to 256 MB.
fn id3_sane(path: &Path) -> Result<u64, String> {
    use std::io::{Read, Seek, SeekFrom};
    let bad = || "the file's ID3 tag does not fit the file".to_string();
    let Ok(mut f) = File::open(path) else { return Ok(0) };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut h = [0u8; 10];
    if f.read_exact(&mut h).is_err() || &h[0..3] != b"ID3" {
        return Ok(0);
    }
    let syncsafe = |b: &[u8]| b.iter().fold(0u64, |a, &x| (a << 7) | u64::from(x & 0x7F));
    let version = h[3];
    let tag_end = 10 + syncsafe(&h[6..10]);
    if tag_end > size {
        return Err(bad());
    }
    // Frames are only walked where their layout is known (versions 2 to 4, no extended
    // header, not unsynchronised as a whole).
    if !(2..=4).contains(&version) || h[5] & 0xC0 != 0 {
        return Ok(tag_end);
    }
    let header = if version == 2 { 6u64 } else { 10 };
    let mut at = 10u64;
    for _ in 0..100_000 {
        if at + header > tag_end {
            break;
        }
        let mut fh = [0u8; 10];
        let n = usize::try_from(header).unwrap_or(10);
        let Some(part) = fh.get_mut(..n) else { return Err(bad()) };
        if f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(part).is_err() {
            return Err(bad());
        }
        if fh[0] == 0 {
            // Padding.
            break;
        }
        let len = match version {
            2 => (u64::from(fh[3]) << 16) | (u64::from(fh[4]) << 8) | u64::from(fh[5]),
            3 => u64::from(u32::from_be_bytes([fh[4], fh[5], fh[6], fh[7]])),
            _ => syncsafe(&fh[4..8]),
        };
        if at + header + len > tag_end {
            return Err(bad());
        }
        at += header + len;
    }
    Ok(tag_end)
}

/// Checks the metadata blocks of a FLAC file (after an ID3 tag ending at `from`): every block
/// within the file, and in comments and pictures every length within its block. symphonia
/// reserves memory for them on the word of the lengths.
fn flac_sane(path: &Path, from: u64) -> Result<(), String> {
    let bad = || "the FLAC file's metadata does not fit the file".to_string();
    let Ok(mut f) = File::open(path) else { return Ok(()) };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    if mp4_read(&mut f, from, 4).as_deref() != Some(b"fLaC") {
        return Ok(());
    }
    let mut at = from + 4;
    for _ in 0..1024 {
        let h = mp4_read(&mut f, at, 4).ok_or_else(bad)?;
        let last = h.first().is_some_and(|b| b & 0x80 != 0);
        let kind = h.first().map_or(0, |b| b & 0x7F);
        let len = h.get(1..4).map_or(0, |b| b.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)));
        let body = at + 4;
        if body + len > size {
            return Err(bad());
        }
        if kind == 4 || kind == 6 {
            let b = mp4_read(&mut f, body, usize::try_from(len).unwrap_or(0)).ok_or_else(bad)?;
            let fits = if kind == 4 { vorbis_comment_fits(&b) } else { picture_fits(&b) };
            if !fits {
                return Err(bad());
            }
        }
        at = body + len;
        if last {
            break;
        }
    }
    Ok(())
}

/// Whether every length in a Vorbis comment (little endian, no framing bit) stays inside it.
fn vorbis_comment_fits(b: &[u8]) -> bool {
    let le = |at: usize| {
        b.get(at..at + 4).and_then(|x| x.try_into().ok()).map(|x| u64::from(u32::from_le_bytes(x)))
    };
    let Some(vendor) = le(0) else { return false };
    let mut at = 4 + vendor;
    let Some(count) = usize::try_from(at).ok().and_then(le) else { return false };
    at += 4;
    for _ in 0..count {
        let Some(len) = usize::try_from(at).ok().and_then(le) else { return false };
        at += 4 + len;
        if at > b.len() as u64 {
            return false;
        }
    }
    at <= b.len() as u64
}

/// Whether the lengths in a FLAC picture block (big endian) stay inside it.
fn picture_fits(b: &[u8]) -> bool {
    let mut at = 4u64;
    for step in [0u64, 0, 16] {
        at += step;
        let Some(len) = usize::try_from(at).ok().and_then(|a| be32(b, a)) else { return false };
        at += 4 + u64::from(len);
        if at > b.len() as u64 {
            return false;
        }
    }
    true
}

/// Checks the chunks of a WAV file: every chunk but the sound itself within the file (a sound
/// chunk cut short by a crashed recorder still plays), and in a LIST every entry within it.
fn riff_sane(path: &Path) -> Result<(), String> {
    let bad = || "the WAV file's chunks do not fit the file".to_string();
    let Ok(mut f) = File::open(path) else { return Ok(()) };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let head = mp4_read(&mut f, 0, 12);
    if head.as_ref().and_then(|h| h.get(0..4)) != Some(b"RIFF") {
        return Ok(());
    }
    let le = |b: &[u8], at: usize| {
        b.get(at..at + 4).and_then(|x| x.try_into().ok()).map(|x| u64::from(u32::from_le_bytes(x)))
    };
    let mut at = 12u64;
    for _ in 0..4096 {
        let Some(h) = mp4_read(&mut f, at, 8) else { break };
        let len = le(&h, 4).ok_or_else(bad)?;
        let body = at + 8;
        if h.get(0..4) == Some(b"data") {
            break;
        }
        if body + len > size {
            return Err(bad());
        }
        if h.get(0..4) == Some(b"LIST") {
            let b = mp4_read(&mut f, body, usize::try_from(len).unwrap_or(0)).ok_or_else(bad)?;
            let mut k = 4u64;
            while k + 8 <= len {
                let n = usize::try_from(k + 4).ok().and_then(|a| le(&b, a)).ok_or_else(bad)?;
                k += 8 + n + (n & 1);
                if k > len + 1 {
                    return Err(bad());
                }
            }
        }
        at = body + len + (len & 1);
    }
    Ok(())
}

/// The codecs penumbra decodes: the ones whose headers it checks before symphonia reads them
/// (others that symphonia knows, ALAC, MP1 and MP2, are refused).
fn supported_codec(short_name: &str) -> bool {
    short_name.starts_with("pcm_") || matches!(short_name, "flac" | "mp3" | "aac" | "vorbis")
}

/// Sample entries whose child boxes symphonia reads, with the size of their fixed part by
/// sound sample entry version (0, 1 as QuickTime has it, 2).
const SOUND_ENTRIES: &[&[u8; 4]] = &[b"mp4a", b"alac", b"fLaC", b"Opus", b"lpcm", b"sowt", b"twos"];

/// The sample descriptions in an `stsd` box: every entry within the box, and in sound entries
/// every child box (and an `esds`'s descriptors) within its parent. `None` when one does not
/// fit.
fn stsd_sane(f: &mut File, body: u64, end: u64, seen: &mut usize) -> Option<()> {
    let (entries, fits) = mp4_walk(f, body.checked_add(8)?, end, seen);
    if !fits {
        return None;
    }
    for (kind, b, e) in entries {
        if !SOUND_ENTRIES.contains(&&kind) {
            continue;
        }
        let head = mp4_read(f, b, usize::try_from((e - b).min(10)).ok()?)?;
        let version = head.get(8..10).and_then(|v| v.try_into().ok()).map_or(0, u16::from_be_bytes);
        let fixed = match version {
            1 => 44,
            2 => 64,
            _ => 28,
        };
        if b + fixed > e {
            return None;
        }
        let mut stack = vec![(b + fixed, e)];
        while let Some((start, stop)) = stack.pop() {
            let (children, fits) = mp4_walk(f, start, stop, seen);
            if !fits {
                return None;
            }
            for (k, cb, ce) in children {
                match &k {
                    b"wave" => stack.push((cb, ce)),
                    b"esds" => {
                        let d = mp4_read(f, cb, usize::try_from(ce - cb).ok()?)?;
                        if !descriptors_fit(d.get(4..)?, 0) {
                            return None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Some(())
}

/// Whether every MPEG-4 descriptor in `b` (tag, length in 7-bit groups, body) stays inside
/// it, down into the ES and decoder config descriptors.
pub(crate) fn descriptors_fit(b: &[u8], depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    let mut at = 0usize;
    while at < b.len() {
        let Some(&tag) = b.get(at) else { return false };
        at += 1;
        let mut len = 0usize;
        for i in 0..4 {
            let Some(&x) = b.get(at) else { return false };
            at += 1;
            len = (len << 7) | usize::from(x & 0x7F);
            if x & 0x80 == 0 {
                break;
            }
            if i == 3 {
                return false;
            }
        }
        let Some(body) = b.get(at..at.saturating_add(len)) else { return false };
        let inner = match tag {
            // ES: id, flags, then what the flags announce.
            3 => {
                let flags = body.get(2).copied().unwrap_or(0);
                let mut skip = 3usize;
                if flags & 0x80 != 0 {
                    skip += 2;
                }
                if flags & 0x40 != 0 {
                    skip += 1 + usize::from(body.get(skip).copied().unwrap_or(0));
                }
                if flags & 0x20 != 0 {
                    skip += 2;
                }
                body.get(skip..)
            }
            // Decoder config: 13 bytes, then its specific info.
            4 => body.get(13..),
            _ => Some(&[][..]),
        };
        let Some(inner) = inner else { return false };
        if !inner.is_empty() && !descriptors_fit(inner, depth + 1) {
            return false;
        }
        at += len;
    }
    true
}

/// Most bytes of one Ogg header packet [`ogg_sane`] reassembles (a comment with pictures).
const OGG_PACKET_BYTES: usize = 64 << 20;
/// Most codebook entries of one Vorbis stream, all codebooks together. libvorbis uses some
/// thousands; symphonia keeps a few bytes per entry.
pub const VORBIS_ENTRIES: u64 = 1 << 20;
/// Most floats of the vector lookup tables of one Vorbis stream, all codebooks together.
pub const VORBIS_LOOKUP_FLOATS: u64 = 1 << 22;

/// What an Ogg logical stream is, by its first packet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OggKind {
    Vorbis,
    /// Something symphonia does not read (video, a skeleton): passed over.
    Other,
}

#[derive(Default)]
struct OggStream {
    kind: Option<OggKind>,
    packets: u8,
    channels: u8,
    partial: Vec<u8>,
}

/// Checks the header packets of every logical stream of an Ogg file before symphonia reads
/// them, since it sizes memory on their word: a Vorbis comment's lengths within its packet,
/// and in the setup the codebooks' entries and lookup tables under [`VORBIS_ENTRIES`] and
/// [`VORBIS_LOOKUP_FLOATS`]. Ogg FLAC and Opus are refused (symphonia reads their headers
/// unchecked too). Pages are found as symphonia finds them, by their capture pattern.
fn ogg_sane(path: &Path) -> Result<(), String> {
    let Ok(mut f) = File::open(path) else { return Ok(()) };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    if mp4_read(&mut f, 0, 4).as_deref() != Some(b"OggS") {
        return Ok(());
    }
    let broken = |what: &str| format!("the Ogg file's {what} does not fit: not played");
    let mut streams: std::collections::BTreeMap<u32, OggStream> = std::collections::BTreeMap::new();
    let mut at = 0u64;
    while at + 27 <= size {
        let Some(h) = mp4_read(&mut f, at, 27) else { break };
        if h.get(0..4) != Some(b"OggS") {
            at = ogg_capture(&mut f, at + 1, size);
            continue;
        }
        let flags = h.get(5).copied().unwrap_or(0);
        let serial = h.get(14..18).and_then(|b| b.try_into().ok()).map_or(0, u32::from_le_bytes);
        let segments = usize::from(h.get(26).copied().unwrap_or(0));
        let Some(lacing) = mp4_read(&mut f, at + 27, segments) else { break };
        let body_at = at + 27 + segments as u64;
        let body_len: u64 = lacing.iter().map(|&l| u64::from(l)).sum();
        if body_at + body_len > size {
            break;
        }
        if flags & 2 != 0 {
            if streams.len() >= 4096 {
                return Err(broken("stream list"));
            }
            streams.insert(serial, OggStream::default());
        }
        let wanted = streams.get(&serial).is_some_and(|s| s.packets < 3 && s.kind != Some(OggKind::Other));
        if wanted {
            let body = mp4_read(&mut f, body_at, usize::try_from(body_len).unwrap_or(0)).unwrap_or_default();
            let Some(st) = streams.get_mut(&serial) else { break };
            if flags & 1 == 0 {
                st.partial.clear();
            }
            let mut off = 0usize;
            for &lace in &lacing {
                let n = usize::from(lace);
                st.partial.extend_from_slice(body.get(off..off + n).unwrap_or(&[]));
                off += n;
                if st.partial.len() > OGG_PACKET_BYTES {
                    return Err(broken("header packet"));
                }
                if lace < 255 {
                    let packet = std::mem::take(&mut st.partial);
                    ogg_header(st, &packet)?;
                    st.packets += 1;
                    if st.packets >= 3 || st.kind == Some(OggKind::Other) {
                        break;
                    }
                }
            }
        }
        at = body_at + body_len;
    }
    Ok(())
}

/// The next capture pattern at or after `from` (`size` when there is none).
fn ogg_capture(f: &mut File, from: u64, size: u64) -> u64 {
    let mut at = from;
    while at + 4 <= size {
        let n = usize::try_from((size - at).min(1 << 16)).unwrap_or(1 << 16);
        let Some(chunk) = mp4_read(f, at, n) else { return size };
        if let Some(i) = chunk.windows(4).position(|w| w == b"OggS") {
            return at + i as u64;
        }
        // Overlap by three bytes so a pattern across the boundary is found.
        at += (n as u64).saturating_sub(3).max(1);
    }
    size
}

/// Checks one header packet of a stream (`st.packets` says which).
fn ogg_header(st: &mut OggStream, p: &[u8]) -> Result<(), String> {
    let refuse = |what: &str| format!("the Ogg Vorbis {what} is broken: not played");
    match (st.packets, st.kind) {
        (0, _) => {
            if p.starts_with(b"\x01vorbis") {
                // Version, channels, rate, three bitrates, block sizes.
                let channels = p.get(11).copied().unwrap_or(0);
                let blocks = p.get(28).copied().unwrap_or(0);
                let (b0, b1) = (blocks & 0x0F, blocks >> 4);
                if channels == 0
                    || usize::from(channels) > MAX_CHANNELS
                    || !(6..=13).contains(&b0)
                    || !(b0..=13).contains(&b1)
                {
                    return Err(refuse("identification header"));
                }
                st.channels = channels;
                st.kind = Some(OggKind::Vorbis);
            } else if p.starts_with(b"\x7FFLAC") || p.starts_with(b"OpusHead") {
                return Err(
                    "Ogg FLAC and Ogg Opus are not supported: convert the file to FLAC or MP3".to_string()
                );
            } else {
                st.kind = Some(OggKind::Other);
            }
        }
        (1, Some(OggKind::Vorbis)) => {
            if !(p.starts_with(b"\x03vorbis") && p.get(7..).is_some_and(vorbis_comment_fits)) {
                return Err(refuse("comment header"));
            }
        }
        (2, Some(OggKind::Vorbis)) => {
            if !p.starts_with(b"\x05vorbis") {
                return Err(refuse("setup header"));
            }
            vorbis_codebooks_fit(p.get(7..).unwrap_or(&[])).map_err(|e| refuse(&e))?;
        }
        _ => {}
    }
    Ok(())
}

/// Bits read from the least significant on, as Vorbis packs them.
struct Bits<'a> {
    b: &'a [u8],
    pos: u64,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n.min(32) {
            let byte = *self.b.get(usize::try_from(self.pos / 8).ok()?)?;
            v |= u32::from((byte >> (self.pos % 8)) & 1) << i;
            self.pos += 1;
        }
        Some(v)
    }

    fn skip(&mut self, n: u64) -> Option<()> {
        self.pos = self.pos.checked_add(n)?;
        (self.pos <= self.b.len() as u64 * 8).then_some(())
    }
}

/// `floor(entries ^ (1 / dimensions))`, exactly: the largest `v` with `v ^ dimensions <=
/// entries`.
pub(crate) fn lookup1_values(entries: u32, dimensions: u16) -> u64 {
    if dimensions == 0 {
        return 0;
    }
    let fits = |v: u64| {
        let mut x: u64 = 1;
        for _ in 0..dimensions {
            x = match x.checked_mul(v) {
                Some(x) if x <= u64::from(entries) => x,
                _ => return false,
            };
        }
        true
    };
    // `as`: a first guess, corrected below.
    let mut v = (f64::from(entries).powf(1.0 / f64::from(dimensions)).floor() as u64).max(1);
    while v > 0 && !fits(v) {
        v -= 1;
    }
    while fits(v + 1) {
        v += 1;
    }
    v
}

/// Walks the codebooks at the start of a Vorbis setup header (as symphonia does, without
/// keeping anything) and says what is too large.
pub(crate) fn vorbis_codebooks_fit(b: &[u8]) -> Result<(), String> {
    let short = || "setup header".to_string();
    let mut bits = Bits { b, pos: 0 };
    let count = bits.read(8).ok_or_else(short)? + 1;
    let (mut entries_total, mut floats_total) = (0u64, 0u64);
    for _ in 0..count {
        if bits.read(24).ok_or_else(short)? != 0x0056_4342 {
            return Err(short());
        }
        let dimensions = bits.read(16).ok_or_else(short)?;
        let entries = bits.read(24).ok_or_else(short)?;
        entries_total += u64::from(entries);
        if entries_total > VORBIS_ENTRIES {
            return Err(format!("codebooks ({entries_total} entries)"));
        }
        let ordered = bits.read(1).ok_or_else(short)? == 1;
        if !ordered {
            if bits.read(1).ok_or_else(short)? == 1 {
                for _ in 0..entries {
                    if bits.read(1).ok_or_else(short)? == 1 {
                        bits.skip(5).ok_or_else(short)?;
                    }
                }
            } else {
                bits.skip(u64::from(entries) * 5).ok_or_else(short)?;
            }
        } else {
            bits.skip(5).ok_or_else(short)?;
            let mut cur = 0u32;
            while cur < entries {
                let n = bits.read(32 - (entries - cur).leading_zeros()).ok_or_else(short)?;
                cur = cur.checked_add(n).filter(|c| *c <= entries).ok_or_else(short)?;
            }
        }
        let lookup = bits.read(4).ok_or_else(short)?;
        match lookup {
            0 => {}
            1 | 2 => {
                let floats = u64::from(entries) * u64::from(dimensions);
                floats_total += floats;
                if floats_total > VORBIS_LOOKUP_FLOATS {
                    return Err(format!("codebooks ({floats_total} lookup values)"));
                }
                bits.skip(64).ok_or_else(short)?;
                let value_bits = u64::from(bits.read(4).ok_or_else(short)?) + 1;
                bits.skip(1).ok_or_else(short)?;
                let values = if lookup == 1 { lookup1_values(entries, dimensions as u16) } else { floats };
                bits.skip(values * value_bits).ok_or_else(short)?;
            }
            _ => return Err(short()),
        }
    }
    Ok(())
}
