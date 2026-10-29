//! Sound in a recording: a second track beside the picture.
//!
//! - **Codec by container:** an MP4 gets AAC-LC ([`AAC_BITRATE`], FilmCraft's encoder, the
//!   encoder delay hidden by an edit list), which every player opens. A MOV gets 24-bit PCM, the
//!   format editing programs expect beside ProRes.
//! - **Placement by sample number:** every [`AudioBlock`] says at which sample of the file it
//!   starts. The first block that reaches into the file is placed exactly (what lies before the
//!   file's start is cut). After that a block that starts within [`TOLERANCE_SECONDS`] of where
//!   the track ends simply continues it, so a seek or a source on another clock leaves no gap.
//!   A block further ahead leaves a gap of silence (at most [`MAX_GAP_SECONDS`]); one further
//!   back loses the part already written over.
//! - **Two channels, one rate:** blocks come as [`AUDIO_CHANNELS`] channels; the first block's
//!   rate is the track's, a block at another rate is left out and counted.
//! - **The end:** the track is filled with silence up to the end of the picture, at most
//!   [`END_PAD_SECONDS`].

use filmcraft_isobmff as iso;
use serde::Serialize;

/// Channels of every recorded track.
pub const AUDIO_CHANNELS: usize = 2;
/// AAC bit rate for two channels, bits per second.
pub const AAC_BITRATE: u32 = 320_000;
/// A block this close to the end of the track continues it, seconds.
pub const TOLERANCE_SECONDS: f64 = 0.05;
/// The longest silence put in for a block that starts later, seconds; a block further ahead
/// continues the track instead.
pub const MAX_GAP_SECONDS: f64 = 600.0;
/// The most silence added at the end to reach the end of the picture, seconds.
pub const END_PAD_SECONDS: f64 = 1.0;
/// Samples written in one piece of silence.
const SILENCE_CHUNK: usize = 4096;

/// Sound for a recording: `channels` ([`AUDIO_CHANNELS`] of equal length) at `rate`, starting
/// at sample `at` of the file (negative: before the file's start).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBlock {
    pub at: i64,
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl AudioBlock {
    pub fn len(&self) -> usize {
        self.channels.iter().map(Vec::len).min().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What the sound is encoded as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    /// AAC-LC at [`AAC_BITRATE`]; in MP4.
    Aac,
    /// 24-bit signed little-endian PCM; in MOV.
    Pcm24,
}

impl AudioCodec {
    /// The codec a file of this brand gets.
    pub fn for_brand(mp4: bool) -> AudioCodec {
        if mp4 { AudioCodec::Aac } else { AudioCodec::Pcm24 }
    }

    pub fn name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::Pcm24 => "pcm_s24le",
        }
    }
}

/// The sound of a recording as far as it got.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AudioStatus {
    /// `aac` or `pcm_s24le`; empty before the first block.
    pub codec: String,
    /// Samples per second; 0 before the first block.
    pub rate: u32,
    pub channels: u32,
    /// The `out.audio` nodes that played when last asked; 0: the file gets no sound.
    pub sources: usize,
    /// Length of the track so far.
    pub seconds: f64,
    /// Samples per channel in the track, silence put in included.
    pub samples: u64,
    /// Samples of silence put in where sound was missing (a jump forward, a source that
    /// stopped).
    pub gap_samples: u64,
    /// Samples left out because the track already held their time (a jump back).
    pub cut_samples: u64,
    /// Blocks left out: another rate than the first block's.
    pub refused: u64,
    /// Blocks lost before they reached the file (nobody took them in time).
    pub dropped: u64,
    /// Why the track stopped taking sound; the picture goes on.
    pub error: Option<String>,
}

/// Where a block goes in the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    /// Samples of silence before it.
    pub pad: u64,
    /// Samples cut from its start.
    pub skip: usize,
}

/// Decides where blocks go: see the module doc.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Placer {
    /// Samples per channel placed so far.
    pub written: u64,
}

impl Placer {
    /// Where a block of `len` samples at file sample `at` goes, at `rate`; counts it as placed.
    pub fn place(&mut self, at: i64, len: usize, rate: u32) -> Placement {
        let w = i128::from(self.written);
        let d = i128::from(at) - w;
        let rate = f64::from(rate.max(1));
        // `as`: seconds to whole samples.
        let tolerance = (TOLERANCE_SECONDS * rate) as i128;
        let max_gap = (MAX_GAP_SECONDS * rate) as i128;
        let first = self.written == 0;
        let p = if d < 0 && (first || -d > tolerance) {
            Placement { pad: 0, skip: usize::try_from(-d).unwrap_or(usize::MAX).min(len) }
        } else if d > 0 && (first || d > tolerance) && d <= max_gap {
            Placement { pad: u64::try_from(d).unwrap_or(0), skip: 0 }
        } else {
            Placement { pad: 0, skip: 0 }
        };
        if self.written == 0 && p.skip >= len {
            // Wholly before the file's start: the next block is placed exactly again.
            return p;
        }
        let kept = u64::try_from(len - p.skip.min(len)).unwrap_or(0);
        self.written = self.written.saturating_add(p.pad).saturating_add(kept);
        p
    }
}

/// The sound track of a movie being written.
pub(crate) struct Track {
    pub index: usize,
    pub rate: u32,
    placer: Placer,
    aac: Option<Box<filmcraft_aac::Encoder>>,
    pub status: AudioStatus,
}

/// AAC can take these rates only.
fn aac_rate(rate: u32) -> bool {
    filmcraft_aac::SAMPLE_RATES.contains(&rate)
}

impl Track {
    /// Adds a track at `rate` in `codec` to `mux`.
    pub fn add<W: std::io::Write + std::io::Seek>(
        mux: &mut iso::Mp4Writer<W>,
        codec: AudioCodec,
        rate: u32,
    ) -> Result<Track, String> {
        let channels = AUDIO_CHANNELS as u32;
        let (config, aac) = match codec {
            AudioCodec::Aac => {
                if !aac_rate(rate) {
                    return Err(format!(
                        "sound at {rate} Hz cannot go into an MP4 (AAC takes 8000 to 96000 Hz in fixed steps, \
                         e.g. 44100 or 48000): record ProRes, or play the sound at 48000 Hz"
                    ));
                }
                // AAC carries at most 6144 bits per channel and frame of 1024 samples.
                let most = u64::from(rate) * 6 * u64::from(channels);
                let bps = u32::try_from(u64::from(AAC_BITRATE).min(most)).unwrap_or(AAC_BITRATE);
                let enc = crate::writer::guarded(|| {
                    filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(rate, AUDIO_CHANNELS, bps))
                        .map_err(|e| format!("cannot start the AAC encoder: {e}"))
                })?;
                let entry = iso::SampleEntry::aac(enc.audio_specific_config(), channels, rate);
                let mut config = iso::TrackConfig::new(entry, rate);
                // The encoder's first samples are its delay: the edit list starts after them.
                config.media_start = Some(i64::from(enc.priming_samples()));
                (config, Some(Box::new(enc)))
            }
            AudioCodec::Pcm24 => {
                let pcm = iso::PcmConfig {
                    bits: 24,
                    float: false,
                    big_endian: false,
                    signed: true,
                    channels,
                    sample_rate: f64::from(rate),
                };
                (iso::TrackConfig::new(iso::SampleEntry::pcm(pcm), rate), None)
            }
        };
        let index = mux.add_track(config).map_err(|e| format!("cannot add the sound track: {e}"))?;
        let status =
            AudioStatus { codec: codec.name().to_string(), rate, channels, ..AudioStatus::default() };
        Ok(Track { index, rate, placer: Placer::default(), aac, status })
    }

    /// Puts `block` where it belongs (see the module doc).
    pub fn write<W: std::io::Write + std::io::Seek>(
        &mut self,
        mux: &mut iso::Mp4Writer<W>,
        block: &AudioBlock,
    ) -> Result<(), String> {
        if block.rate != self.rate {
            self.status.refused += 1;
            return Ok(());
        }
        let len = block.len();
        let p = self.placer.place(block.at, len, self.rate);
        let skip = p.skip.min(len);
        self.status.cut_samples += u64::try_from(skip).unwrap_or(0);
        if self.placer.written == 0 {
            return Ok(());
        }
        self.status.gap_samples += p.pad;
        self.silence(mux, p.pad)?;
        let channels: Vec<&[f32]> = (0..AUDIO_CHANNELS)
            .map(|c| block.channels.get(c).and_then(|s| s.get(skip..len)).unwrap_or(&[]))
            .collect();
        self.samples(mux, &channels)?;
        self.count();
        Ok(())
    }

    /// Fills the track with silence up to `end` samples, at most [`END_PAD_SECONDS`] of it,
    /// then writes what the encoder still holds.
    pub fn finish<W: std::io::Write + std::io::Seek>(
        &mut self,
        mux: &mut iso::Mp4Writer<W>,
        end: Option<u64>,
    ) -> Result<(), String> {
        if let Some(end) = end
            && self.placer.written > 0
        {
            // `as`: seconds to whole samples.
            let most = (END_PAD_SECONDS * f64::from(self.rate)) as u64;
            let pad = end.saturating_sub(self.placer.written).min(most);
            self.placer.written += pad;
            self.silence(mux, pad)?;
            self.count();
        }
        if let Some(enc) = self.aac.as_mut() {
            let units = crate::writer::guarded(|| Ok(enc.flush()))?;
            write_units(mux, self.index, &units)?;
        }
        Ok(())
    }

    fn count(&mut self) {
        self.status.samples = self.placer.written;
        // `as`: a sample count to seconds.
        self.status.seconds = self.placer.written as f64 / f64::from(self.rate.max(1));
    }

    fn silence<W: std::io::Write + std::io::Seek>(
        &mut self,
        mux: &mut iso::Mp4Writer<W>,
        mut n: u64,
    ) -> Result<(), String> {
        let zeros = [0.0f32; SILENCE_CHUNK];
        while n > 0 {
            let k = usize::try_from(n).unwrap_or(SILENCE_CHUNK).min(SILENCE_CHUNK);
            let part = zeros.get(..k).unwrap_or(&[]);
            self.samples(mux, &[part; AUDIO_CHANNELS])?;
            n -= k as u64;
        }
        Ok(())
    }

    fn samples<W: std::io::Write + std::io::Seek>(
        &mut self,
        mux: &mut iso::Mp4Writer<W>,
        channels: &[&[f32]],
    ) -> Result<(), String> {
        let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        match self.aac.as_mut() {
            Some(enc) => {
                let units = crate::writer::guarded(|| Ok(enc.encode(channels)))?;
                write_units(mux, self.index, &units)
            }
            None => {
                let data = pcm24(channels, n);
                let sample =
                    iso::WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true };
                mux.write_sample(self.index, sample).map_err(|e| format!("cannot write the sound: {e}"))
            }
        }
    }
}

/// AAC access units, 1024 samples each.
fn write_units<W: std::io::Write + std::io::Seek>(
    mux: &mut iso::Mp4Writer<W>,
    track: usize,
    units: &[Vec<u8>],
) -> Result<(), String> {
    for au in units {
        let sample = iso::WriteSample { data: au, duration: 1024, composition_offset: 0, is_sync: true };
        mux.write_sample(track, sample).map_err(|e| format!("cannot write the sound: {e}"))?;
    }
    Ok(())
}

/// The first `n` samples of each channel, interleaved, as 24-bit signed little-endian PCM.
/// Clipped to full scale; non-numbers are silence.
pub(crate) fn pcm24(channels: &[&[f32]], n: usize) -> Vec<u8> {
    const MAX: f64 = 8_388_607.0;
    let mut out = Vec::with_capacity(n * channels.len() * 3);
    for k in 0..n {
        for c in channels {
            let v = c.get(k).copied().map_or(0.0, f64::from);
            let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
            // `as`: in range after the clamp.
            let q = (v * MAX).round() as i32;
            out.extend_from_slice(&q.to_le_bytes()[..3]);
        }
    }
    out
}

/// The sound track of a movie, decoded: what a player plays, the encoder delay and the end
/// beyond the edit list left out.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub codec: AudioCodec,
    pub rate: u32,
    /// One vector per channel, of equal length.
    pub channels: Vec<Vec<f32>>,
}

impl Sound {
    /// Samples per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Length in seconds.
    pub fn seconds(&self) -> f64 {
        // `as`: a sample count to seconds.
        self.len() as f64 / f64::from(self.rate.max(1))
    }
}

/// Reads the sound of a movie this crate wrote (AAC or 16- or 24-bit PCM); `None` when it has
/// no sound track.
pub fn read(path: &std::path::Path) -> Result<Option<Sound>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let movie = iso::open(file.try_clone().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let Some(index) = movie.track_of_kind(iso::TrackKind::Audio) else { return Ok(None) };
    let Some(track) = movie.tracks.get(index) else { return Ok(None) };
    let rate = track.timescale;
    let edit = track.edits.iter().find(|e| e.media_time >= 0);
    let skip = edit.map_or(0, |e| usize::try_from(e.media_time).unwrap_or(0));
    let keep = edit.map(|e| {
        let n = u128::from(e.segment_duration) * u128::from(rate) / u128::from(movie.timescale.max(1));
        usize::try_from(n).unwrap_or(usize::MAX)
    });
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let codec = match track.codec() {
        Some(iso::CodecConfig::Aac(cfg)) => {
            let mut dec = filmcraft_aac::Decoder::new(&cfg.asc).map_err(|e| format!("AAC: {e}"))?;
            for i in 0..track.samples.len() {
                let au = movie.read_sample(&file, index, i).map_err(|e| e.to_string())?;
                let pcm = dec.decode(&au).map_err(|e| format!("AAC: {e}"))?;
                channels.resize(pcm.len().max(channels.len()), Vec::new());
                for (c, p) in channels.iter_mut().zip(pcm) {
                    c.extend(p);
                }
            }
            AudioCodec::Aac
        }
        Some(iso::CodecConfig::Pcm(p)) if !p.float && !p.big_endian && matches!(p.bits, 16 | 24) => {
            let n = usize::try_from(p.channels).unwrap_or(0).max(1);
            let bytes = usize::from(p.bits / 8);
            channels = vec![Vec::new(); n];
            for i in 0..track.samples.len() {
                let data = movie.read_sample(&file, index, i).map_err(|e| e.to_string())?;
                for frame in data.chunks_exact(bytes * n) {
                    for (c, s) in channels.iter_mut().zip(frame.chunks_exact(bytes)) {
                        // Sign-extend the little-endian sample from its top byte.
                        let mut b = [0u8; 4];
                        for (d, v) in b.iter_mut().skip(4 - bytes).zip(s) {
                            *d = *v;
                        }
                        let v = i32::from_le_bytes(b) >> (8 * (4 - bytes));
                        let full = f64::from(1u32 << (8 * bytes - 1));
                        // `as`: a sample to f32.
                        c.push((f64::from(v) / full) as f32);
                    }
                }
            }
            AudioCodec::Pcm24
        }
        other => return Err(format!("cannot read sound in {}", other.map_or("nothing", |c| c.name()))),
    };
    for c in &mut channels {
        c.drain(..skip.min(c.len()));
        if let Some(keep) = keep {
            c.truncate(keep);
        }
    }
    Ok(Some(Sound { codec, rate, channels }))
}
