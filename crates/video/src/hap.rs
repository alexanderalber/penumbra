//! HAP movies (Vidvox): every frame is a block-compressed GPU texture (BC1, BC3, BC4 or BC7),
//! usually Snappy-compressed on top, stored in a QuickTime or MP4 file. Decoding only undoes the
//! Snappy stage; the GPU samples the blocks directly (`penumbra_gpu::bc`). Every frame is a
//! keyframe, so seeking costs nothing. FilmCraft does not read HAP; its MP4/MOV demuxer
//! (`filmcraft-isobmff`) gives us the sample table.
//!
//! Frame layout, after the HAP specification (github.com/Vidvox/hap, `documentation/HapVideoDRAFT.md`):
//! a frame is one section, a 3-byte little-endian length and a type byte (length 0 means a
//! 4-byte length follows). The type's high nibble is the second-stage compressor (none, Snappy,
//! or "complex": a table of independently compressed chunks), the low nibble the texture
//! format. Hap Q Alpha wraps two such sections (colour and alpha) in a "multiple images"
//! section.
//!
//! Hap HDR (BC6H) is refused: no file to check its colour handling against.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use filmcraft_isobmff::{Mp4File, TrackKind};

use crate::{ClipInfo, Decoder, Frame, Timing};

/// A block-compressed texture format. Blocks are 4 x 4 pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcFormat {
    /// RGB, 8 bytes per block (DXT1).
    Bc1,
    /// RGBA, 16 bytes per block (DXT5).
    Bc3,
    /// One channel, 8 bytes per block (RGTC1).
    Bc4,
    /// RGBA, 16 bytes per block (BPTC).
    Bc7,
}

impl BcFormat {
    pub fn block_bytes(self) -> usize {
        match self {
            Self::Bc1 | Self::Bc4 => 8,
            Self::Bc3 | Self::Bc7 => 16,
        }
    }

    /// Bytes of a `width` x `height` texture: whole blocks, rows top first.
    pub fn texture_bytes(self, width: u32, height: u32) -> usize {
        width.div_ceil(4) as usize * height.div_ceil(4) as usize * self.block_bytes()
    }
}

/// What the texels of a HAP frame mean. Colour is sRGB-encoded throughout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HapColor {
    /// Opaque RGB (Hap, BC1).
    Rgb,
    /// RGB with straight alpha (Hap Alpha in BC3, Hap R in BC7).
    Rgba,
    /// Scaled YCoCg in BC3 (Hap Q): Co, Cg, scale, Y in R, G, B, A.
    YCoCg,
    /// Only alpha, in BC4's single channel (Hap Alpha-Only).
    Alpha,
}

/// One block-compressed texture of a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BcTexture {
    pub format: BcFormat,
    /// [`BcFormat::texture_bytes`] long.
    pub data: Vec<u8>,
}

/// A HAP frame, ready for upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HapFrame {
    pub width: u32,
    pub height: u32,
    pub color: HapColor,
    pub texture: BcTexture,
    /// The BC4 alpha texture of Hap Q Alpha.
    pub alpha: Option<BcTexture>,
}

impl HapFrame {
    pub fn byte_size(&self) -> usize {
        self.texture.data.len() + self.alpha.as_ref().map_or(0, |a| a.data.len())
    }
}

/// Sample entry formats that are HAP, with their names.
const FOURCCS: &[(&[u8; 4], &str)] = &[
    (b"Hap1", "Hap"),
    (b"Hap5", "Hap Alpha"),
    (b"HapY", "Hap Q"),
    (b"HapM", "Hap Q Alpha"),
    (b"HapA", "Hap Alpha-Only"),
    (b"Hap7", "Hap R"),
    (b"HapH", "Hap HDR"),
];

/// Top-level box types an MP4 or MOV file may start with.
const FIRST_BOXES: &[&[u8; 4]] = &[b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot"];

pub struct HapDecoder {
    file: File,
    mp4: Mp4File,
    track: usize,
    info: ClipInfo,
}

impl HapDecoder {
    /// Opens `path` if it is a movie whose first video track is HAP; `None` for anything else
    /// (including files the demuxer cannot read: FilmCraft gets to say what is wrong with them).
    pub fn open(path: &Path) -> Result<Option<Self>, String> {
        let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let mut head = [0u8; 8];
        if file.read_exact(&mut head).is_err() {
            return Ok(None);
        }
        let [_, _, _, _, a, b, c, d] = head;
        if !FIRST_BOXES.contains(&&[a, b, c, d]) {
            return Ok(None);
        }
        let Ok(mp4) = filmcraft_isobmff::open(&file) else {
            return Ok(None);
        };
        let Some(track) = mp4.track_of_kind(TrackKind::Video) else {
            return Ok(None);
        };
        let Some(t) = mp4.tracks.get(track) else {
            return Ok(None);
        };
        let Some(entry) = t.entries.first() else {
            return Ok(None);
        };
        let Some(&(fourcc, codec)) = FOURCCS.iter().find(|(f, _)| **f == entry.format.0) else {
            return Ok(None);
        };
        if fourcc == b"HapH" {
            return Err("Hap HDR is not supported".into());
        }
        let (width, height) = match &entry.video {
            Some(v) => (u32::from(v.width), u32::from(v.height)),
            None => (t.width, t.height),
        };
        let info = ClipInfo {
            width,
            height,
            frames: t.samples.len(),
            timing: timing(t.timescale, t.samples.iter().map(|s| s.duration))?,
            codec: codec.into(),
            container: if mp4.is_quicktime { "mov" } else { "mp4" }.into(),
            seeks: true,
        };
        Ok(Some(Self { file, mp4, track, info }))
    }
}

/// Frame timing from sample durations: a rate when they are all equal (a last sample of
/// duration 0 is common and taken as the others), end times otherwise.
fn timing(timescale: u32, durations: impl Iterator<Item = u32>) -> Result<Timing, String> {
    if timescale == 0 {
        return Err("the video track has no timescale".into());
    }
    let mut durations: Vec<u32> = durations.collect();
    if let [.., before, last] = durations.as_mut_slice()
        && *last == 0
    {
        *last = *before;
    }
    let Some(&first) = durations.first() else {
        return Err("the clip has no frames".into());
    };
    if durations.contains(&0) {
        return Err("a frame has no duration".into());
    }
    if durations.iter().all(|&d| d == first) {
        return Ok(Timing::Rate { num: i64::from(timescale), den: i64::from(first) });
    }
    let mut t = 0u64;
    let ends: Vec<f64> = durations
        .iter()
        .map(|&d| {
            t += u64::from(d);
            t as f64 / f64::from(timescale)
        })
        .collect();
    Ok(Timing::Ends(ends.into()))
}

impl Decoder for HapDecoder {
    fn info(&self) -> &ClipInfo {
        &self.info
    }

    fn frame(&mut self, index: usize) -> Result<Frame, String> {
        let sample = self
            .mp4
            .tracks
            .get(self.track)
            .and_then(|t| t.samples.get(index))
            .ok_or_else(|| format!("frame {index} is past the end"))?;
        // The largest frame HAP can hold, uncompressed, plus headers: anything bigger is damage.
        let most = 2 * BcFormat::Bc3.texture_bytes(self.info.width, self.info.height) + (1 << 20);
        if sample.size as usize > most {
            return Err(format!("frame {index} has {} bytes, more than a HAP frame can need", sample.size));
        }
        let data =
            self.mp4.read_sample(&self.file, self.track, index).map_err(|e| format!("frame {index}: {e}"))?;
        let frame =
            decode(&data, self.info.width, self.info.height).map_err(|e| format!("frame {index}: {e}"))?;
        Ok(Frame::Hap(Arc::new(frame)))
    }
}

/// Section types and nibbles from the specification.
const MULTIPLE_IMAGES: u8 = 0x0D;
const DECODE_INSTRUCTIONS: u8 = 0x01;
const CHUNK_COMPRESSORS: u8 = 0x02;
const CHUNK_SIZES: u8 = 0x03;
const CHUNK_OFFSETS: u8 = 0x04;
const COMPRESSOR_NONE: u8 = 0xA;
const COMPRESSOR_SNAPPY: u8 = 0xB;
const COMPRESSOR_COMPLEX: u8 = 0xC;

/// Most chunks a frame may declare (encoders use up to 64).
const MAX_CHUNKS: usize = 4096;

/// Splits off the section at the start of `data`: (type, payload, rest).
fn section(data: &[u8]) -> Result<(u8, &[u8], &[u8]), String> {
    let Some(&[a, b, c, kind]) = data.first_chunk::<4>() else {
        return Err("truncated section header".into());
    };
    let short = u32::from_le_bytes([a, b, c, 0]) as usize;
    let (len, header) = if short != 0 {
        (short, 4)
    } else {
        let Some(&[_, _, _, _, a, b, c, d]) = data.first_chunk::<8>() else {
            return Err("truncated section header".into());
        };
        (u32::from_le_bytes([a, b, c, d]) as usize, 8)
    };
    let end = header + len;
    let payload = data.get(header..end).ok_or("a section runs past the frame")?;
    let rest = data.get(end..).unwrap_or_default();
    Ok((kind, payload, rest))
}

/// Decodes one HAP frame (a sample) of a `width` x `height` clip.
pub fn decode(data: &[u8], width: u32, height: u32) -> Result<HapFrame, String> {
    let (kind, payload, _) = section(data)?;
    if kind != MULTIPLE_IMAGES {
        let (color, texture) = texture(kind, payload, width, height)?;
        return Ok(HapFrame { width, height, color, texture, alpha: None });
    }
    // Hap Q Alpha: a colour texture and an alpha texture, in either order.
    let (mut color, mut alpha) = (None, None);
    let mut rest = payload;
    while !rest.is_empty() {
        let (kind, payload, tail) = section(rest)?;
        rest = tail;
        match texture(kind, payload, width, height)? {
            (HapColor::Alpha, t) if alpha.is_none() => alpha = Some(t),
            (HapColor::YCoCg, t) if color.is_none() => color = Some(t),
            _ => return Err("unexpected texture in a multi-image frame".into()),
        }
    }
    match (color, alpha) {
        (Some(texture), Some(alpha)) => {
            Ok(HapFrame { width, height, color: HapColor::YCoCg, texture, alpha: Some(alpha) })
        }
        _ => Err("a multi-image frame needs a Hap Q texture and an alpha texture".into()),
    }
}

/// One texture section: its meaning and its blocks.
fn texture(kind: u8, payload: &[u8], width: u32, height: u32) -> Result<(HapColor, BcTexture), String> {
    let (color, format) = match kind & 0x0F {
        0xB => (HapColor::Rgb, BcFormat::Bc1),
        0xE => (HapColor::Rgba, BcFormat::Bc3),
        0xF => (HapColor::YCoCg, BcFormat::Bc3),
        0x1 => (HapColor::Alpha, BcFormat::Bc4),
        0xC => (HapColor::Rgba, BcFormat::Bc7),
        0x2 | 0x3 => return Err("Hap HDR (BC6H) is not supported".into()),
        other => return Err(format!("unknown texture format 0x{other:X}")),
    };
    let want = format.texture_bytes(width, height);
    let data = match kind >> 4 {
        COMPRESSOR_COMPLEX => chunked(payload, want)?,
        compressor => {
            let mut out = Vec::new();
            expand(compressor, payload, want, &mut out)?;
            out
        }
    };
    if data.len() != want {
        return Err(format!("the texture has {} bytes, {width}x{height} needs {want}", data.len()));
    }
    Ok((color, BcTexture { format, data }))
}

/// Appends `src`, decompressed by `compressor`, to `out`, refusing to grow `out` past `most`.
fn expand(compressor: u8, src: &[u8], most: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let room = most.saturating_sub(out.len());
    match compressor {
        COMPRESSOR_NONE => {
            if src.len() > room {
                return Err("the texture is larger than the frame".into());
            }
            out.extend_from_slice(src);
        }
        COMPRESSOR_SNAPPY => {
            let n = snap::raw::decompress_len(src).map_err(|e| format!("Snappy: {e}"))?;
            if n > room {
                return Err("the texture is larger than the frame".into());
            }
            let at = out.len();
            out.resize(at + n, 0);
            let dst = out.get_mut(at..).unwrap_or_default();
            snap::raw::Decoder::new().decompress(src, dst).map_err(|e| format!("Snappy: {e}"))?;
        }
        other => return Err(format!("unknown compressor 0x{other:X}")),
    }
    Ok(())
}

/// A "complex" texture: decode instructions (per-chunk compressor, size and optionally offset),
/// then the chunks, each decompressed on its own and concatenated.
fn chunked(payload: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let (kind, instructions, chunks) = section(payload)?;
    if kind != DECODE_INSTRUCTIONS {
        return Err("a chunked texture without decode instructions".into());
    }
    let (mut compressors, mut sizes, mut offsets) = (None, None, None);
    let mut rest = instructions;
    while !rest.is_empty() {
        let (kind, payload, tail) = section(rest)?;
        rest = tail;
        match kind {
            CHUNK_COMPRESSORS => compressors = Some(payload),
            CHUNK_SIZES => sizes = Some(words(payload)?),
            CHUNK_OFFSETS => offsets = Some(words(payload)?),
            // Later additions to the specification are skipped, as it asks.
            _ => {}
        }
    }
    let (Some(compressors), Some(sizes)) = (compressors, sizes) else {
        return Err("decode instructions without chunk compressors or sizes".into());
    };
    let n = compressors.len();
    if n == 0 || n > MAX_CHUNKS || sizes.len() != n || offsets.as_ref().is_some_and(|o| o.len() != n) {
        return Err("inconsistent chunk tables".into());
    }
    let mut out = Vec::with_capacity(want);
    let mut running = 0usize;
    for (i, (&compressor, &size)) in compressors.iter().zip(&sizes).enumerate() {
        let start = match &offsets {
            Some(o) => o.get(i).copied().unwrap_or_default() as usize,
            None => running,
        };
        let end = start.checked_add(size as usize).ok_or("chunk out of range")?;
        let src = chunks.get(start..end).ok_or("a chunk runs past the frame")?;
        expand(compressor, src, want, &mut out)?;
        running = end;
    }
    Ok(out)
}

/// Little-endian `u32`s.
fn words(data: &[u8]) -> Result<Vec<u32>, String> {
    let (words, rest) = data.as_chunks::<4>();
    if !rest.is_empty() {
        return Err("a chunk table is not whole words".into());
    }
    Ok(words.iter().map(|w| u32::from_le_bytes(*w)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A section header plus payload, short or long form.
    fn sect(kind: u8, payload: &[u8], long: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if long {
            v.extend_from_slice(&[0, 0, 0, kind]);
            v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        } else {
            let n = (payload.len() as u32).to_le_bytes();
            v.extend_from_slice(&[n[0], n[1], n[2], kind]);
        }
        v.extend_from_slice(payload);
        v
    }

    fn snappy(data: &[u8]) -> Vec<u8> {
        snap::raw::Encoder::new().compress_vec(data).unwrap()
    }

    fn blocks(n: usize, bytes: usize) -> Vec<u8> {
        (0..n * bytes).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn plain_and_long_header_and_padding_to_whole_blocks() {
        // 6x5 pixels are 2x2 blocks.
        let data = blocks(4, 8);
        let f = decode(&sect(0xAB, &data, true), 6, 5).unwrap();
        assert_eq!((f.color, f.texture.format, f.alpha.is_none()), (HapColor::Rgb, BcFormat::Bc1, true));
        assert_eq!(f.texture.data, data);
        // One byte short or long is refused.
        assert!(decode(&sect(0xAB, &data[1..], false), 6, 5).is_err());
        let mut more = data.clone();
        more.push(0);
        assert!(decode(&sect(0xAB, &more, false), 6, 5).is_err());
    }

    #[test]
    fn snappy_and_q_alpha_as_two_images() {
        let color = blocks(4, 16);
        let alpha = blocks(4, 8);
        let mut inner = sect(0xBF, &snappy(&color), false);
        inner.extend(sect(0xB1, &snappy(&alpha), false));
        let f = decode(&sect(MULTIPLE_IMAGES, &inner, false), 8, 8).unwrap();
        assert_eq!(f.color, HapColor::YCoCg);
        assert_eq!(f.texture, BcTexture { format: BcFormat::Bc3, data: color.clone() });
        assert_eq!(f.alpha, Some(BcTexture { format: BcFormat::Bc4, data: alpha }));
        // A multi-image frame without its alpha is refused.
        let lone = sect(MULTIPLE_IMAGES, &sect(0xBF, &snappy(&color), false), false);
        assert!(decode(&lone, 8, 8).is_err());
    }

    #[test]
    fn chunks_with_and_without_offsets() {
        let data = blocks(16, 16);
        let (a, b) = data.split_at(100);
        // Chunk 0 Snappy, chunk 1 stored; in the file back to front with an offset table.
        let (ca, chunk1) = (snappy(a), b.to_vec());
        let mut tables = sect(CHUNK_COMPRESSORS, &[COMPRESSOR_SNAPPY, COMPRESSOR_NONE], false);
        let sizes: Vec<u8> =
            [ca.len() as u32, chunk1.len() as u32].iter().flat_map(|s| s.to_le_bytes()).collect();
        tables.extend(sect(CHUNK_SIZES, &sizes, false));
        let offsets: Vec<u8> = [chunk1.len() as u32, 0].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut with_offsets = tables.clone();
        with_offsets.extend(sect(CHUNK_OFFSETS, &offsets, false));
        // An unknown section in the instructions is skipped.
        with_offsets.extend(sect(0x7E, &[1, 2, 3], false));

        let mut payload = sect(DECODE_INSTRUCTIONS, &with_offsets, false);
        payload.extend_from_slice(&chunk1);
        payload.extend_from_slice(&ca);
        let f = decode(&sect(0xCE, &payload, false), 16, 16).unwrap();
        assert_eq!((f.color, f.texture.data.as_slice()), (HapColor::Rgba, data.as_slice()));

        let mut payload = sect(DECODE_INSTRUCTIONS, &tables, false);
        payload.extend_from_slice(&ca);
        payload.extend_from_slice(&chunk1);
        let f = decode(&sect(0xCC, &payload, false), 16, 16).unwrap();
        assert_eq!((f.texture.format, f.texture.data.as_slice()), (BcFormat::Bc7, data.as_slice()));

        // A chunk that runs past the frame.
        let mut payload = sect(DECODE_INSTRUCTIONS, &tables, false);
        payload.extend_from_slice(&ca);
        assert!(decode(&sect(0xCC, &payload, false), 16, 16).is_err());
    }

    #[test]
    fn hostile_frames_are_errors() {
        let big = snappy(&vec![0u8; 1 << 20]);
        for frame in [
            Vec::new(),
            vec![1, 0, 0],
            vec![200, 0, 0, 0xAB, 1, 2],
            vec![0, 0, 0, 0xAB, 255, 255, 255, 255],
            // Decompresses to far more than an 8x8 texture.
            sect(0xBB, &big, false),
            // Not Snappy at all.
            sect(0xBB, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 1], false),
            sect(0x92, &[0; 32], false),
            sect(0xA7, &[0; 32], false),
            sect(0xCB, &sect(DECODE_INSTRUCTIONS, &sect(CHUNK_COMPRESSORS, &[0xA], false), false), false),
            sect(MULTIPLE_IMAGES, &[], false),
        ] {
            assert!(decode(&frame, 8, 8).is_err(), "{frame:?}");
        }
    }

    #[test]
    fn timing_from_sample_durations() {
        assert_eq!(timing(10240, [1024, 1024, 0].into_iter()), Ok(Timing::Rate { num: 10240, den: 1024 }));
        assert_eq!(timing(10, [1, 2, 1].into_iter()), Ok(Timing::Ends(vec![0.1, 0.3, 0.4].into())));
        assert!(timing(10, [1, 0, 1].into_iter()).is_err());
        assert!(timing(0, [1].into_iter()).is_err());
        assert!(timing(10, std::iter::empty()).is_err());
    }
}
