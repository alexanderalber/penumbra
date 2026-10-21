//! Image files into textures, through `photocraft-codecs`. PNG, JPEG, TIFF, WebP, EXR, HDR, BMP,
//! TGA, GIF, QOI, PNM; 8-bit, 16-bit and float.
//!
//! Integer samples are taken as sRGB-encoded and linearised; float samples (EXR, HDR, PFM) as
//! linear already. Alpha is premultiplied on the way in. ICC profiles are ignored
//! (no colour management).
//!
//! Decoding runs off the cook thread (the image node's job). Memory is the real risk with
//! hostile files, not panics: [`limits`] rejects anything too large from the header before a
//! pixel buffer is allocated (the codecs' default lets a 66-byte PNG allocate 2 GB).

use half::f16;
use penumbra_core::Error;
use photocraft_codecs::{ChannelLayout, DecodeOptions, Image, Limits, SampleType};

use crate::readback::srgb_to_linear;
use crate::{Gpu, Tex};

/// Largest image file read at all.
pub const MAX_FILE_BYTES: u64 = 1 << 30;
/// Most pixels of a decoded image (8192 x 8192).
pub const MAX_PIXELS: u64 = 1 << 26;
/// Largest pixel buffer a decoder may allocate.
pub const MAX_DECODE_BYTES: u64 = 512 << 20;

/// Decode limits for textures at most `max_side` pixels wide and high.
pub fn limits(max_side: u32) -> Limits {
    Limits { max_width: max_side, max_height: max_side, max_pixels: MAX_PIXELS, max_alloc: MAX_DECODE_BYTES }
}

/// An image ready for upload: linear, premultiplied RGBA as half floats.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f16>,
    /// What the file held, e.g. `rgba 16-bit`.
    pub source: String,
}

impl Decoded {
    /// The pixels as 32-bit floats, for CPU work on a picture (here, in an optimised crate).
    pub fn to_rgba32f(&self) -> Vec<[f32; 4]> {
        self.rgba.as_chunks::<4>().0.iter().map(|c| c.map(f16::to_f32)).collect()
    }
}

/// Reads and decodes `path` for a texture at most `max_side` pixels on each side.
pub fn decode_file(path: &std::path::Path, max_side: u32) -> Result<Decoded, Error> {
    let len = std::fs::metadata(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?.len();
    if len > MAX_FILE_BYTES {
        return Err(Error::new(format!("{}: {len} bytes is too large", path.display())));
    }
    let bytes = std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    decode(&bytes, max_side).map_err(|e| e.context(path.display()))
}

pub fn decode(bytes: &[u8], max_side: u32) -> Result<Decoded, Error> {
    let opts = DecodeOptions { limits: limits(max_side), keep_orientation: false };
    let image = photocraft_codecs::decode_with(bytes, &opts).map_err(|e| Error::new(e.to_string()))?;
    to_linear(&image)
}

fn to_linear(image: &Image) -> Result<Decoded, Error> {
    let (width, height) = image.dimensions();
    let sample = image.sample_type();
    let source = format!(
        "{} {}",
        format!("{:?}", image.layout()).to_lowercase(),
        match sample {
            SampleType::U8 => "8-bit",
            SampleType::U16 => "16-bit",
            SampleType::F16 => "half float",
            SampleType::F32 => "float",
        }
    );
    // Same sample type, so the conversion only adds channels (or converts CMYK).
    let target = if sample == SampleType::F32 { SampleType::F16 } else { sample };
    let rgba = image.converted(ChannelLayout::Rgba, target);
    let data = rgba.data();
    let n = width as usize * height as usize * 4;
    let mut out = Vec::with_capacity(n);
    // Premultiplies straight alpha; `c` is linear already.
    let mut push = |rgb: [f32; 3], a: f32| {
        out.extend(rgb.map(|c| f16::from_f32(c * a)));
        out.push(f16::from_f32(a));
    };
    match target {
        SampleType::U8 => {
            let lut: Vec<f32> = (0..256).map(|v| srgb_to_linear(v as f32 / 255.0)).collect();
            let lin = |c: u8| lut.get(usize::from(c)).copied().unwrap_or(0.0);
            for [r, g, b, a] in data.as_chunks::<4>().0 {
                push([lin(*r), lin(*g), lin(*b)], f32::from(*a) / 255.0);
            }
        }
        SampleType::U16 => {
            let lut: Vec<f32> = (0..65536).map(|v| srgb_to_linear(v as f32 / 65535.0)).collect();
            let samples: Vec<u16> = data.as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes(*b)).collect();
            let lin = |c: u16| lut.get(usize::from(c)).copied().unwrap_or(0.0);
            for [r, g, b, a] in samples.as_chunks::<4>().0 {
                push([lin(*r), lin(*g), lin(*b)], f32::from(*a) / 65535.0);
            }
        }
        SampleType::F16 | SampleType::F32 => {
            let finite = |c: f32| if c.is_finite() { c } else { 0.0 };
            let samples: Vec<f32> =
                data.as_chunks::<2>().0.iter().map(|b| finite(f16::from_ne_bytes(*b).to_f32())).collect();
            for [r, g, b, a] in samples.as_chunks::<4>().0 {
                push([*r, *g, *b], a.clamp(0.0, 1.0));
            }
        }
    }
    if out.len() != n {
        return Err(Error::new(format!("decoded {} samples, expected {n}", out.len())));
    }
    Ok(Decoded { width, height, rgba: out, source })
}

/// An image as data rather than colour: samples as stored (integers scaled to 0..1, no sRGB
/// decode, alpha not premultiplied), RGBA as 32-bit floats. For maps and label pictures, whose
/// values must come back exactly as written.
pub struct Raw {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[f32; 4]>,
    /// The largest integer sample of the file's type (255, 65535), 1 for float files: a sample
    /// times this is the integer written.
    pub scale: f32,
}

/// Reads `path` as data (see [`Raw`]), at most `max_side` pixels on each side and `max_pixels`
/// in all.
pub fn decode_file_raw(path: &std::path::Path, max_side: u32, max_pixels: u64) -> Result<Raw, Error> {
    let len = std::fs::metadata(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?.len();
    if len > MAX_FILE_BYTES {
        return Err(Error::new(format!("{}: {len} bytes is too large", path.display())));
    }
    let bytes = std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let mut limits = limits(max_side);
    limits.max_pixels = limits.max_pixels.min(max_pixels);
    let opts = DecodeOptions { limits, keep_orientation: false };
    let image = photocraft_codecs::decode_with(&bytes, &opts)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let (width, height) = image.dimensions();
    let scale = match image.sample_type() {
        SampleType::U8 => 255.0,
        SampleType::U16 => 65535.0,
        SampleType::F16 | SampleType::F32 => 1.0,
    };
    let flat = image.to_rgba_f32();
    let rgba: Vec<[f32; 4]> = flat.as_chunks::<4>().0.to_vec();
    if rgba.len() as u64 != u64::from(width) * u64::from(height) {
        return Err(Error::new(format!(
            "{}: decoded {} pixels, expected {width} x {height}",
            path.display(),
            rgba.len()
        )));
    }
    Ok(Raw { width, height, rgba, scale })
}

/// A one-channel 8-bit PNG of `values` (one per pixel, as written): label pictures.
pub fn encode_png_gray8(width: u32, height: u32, values: Vec<u8>) -> Result<Vec<u8>, Error> {
    let image = Image::from_u8(width, height, ChannelLayout::Gray, values)
        .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
    photocraft_codecs::encode(
        &image,
        photocraft_codecs::Format::Png,
        &photocraft_codecs::EncodeOptions::default(),
    )
    .map_err(|e| Error::new(format!("cannot encode the PNG: {e}")))
}

/// A one-channel 16-bit PNG of `values` (one per pixel, as written): weights and other data
/// finer than 8 bits.
pub fn encode_png_gray16(width: u32, height: u32, values: &[u16]) -> Result<Vec<u8>, Error> {
    let image = Image::from_u16(width, height, ChannelLayout::Gray, values)
        .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
    photocraft_codecs::encode(
        &image,
        photocraft_codecs::Format::Png,
        &photocraft_codecs::EncodeOptions::default(),
    )
    .map_err(|e| Error::new(format!("cannot encode the PNG: {e}")))
}

/// A three-channel 16-bit PNG of `values` (red, green, blue per pixel, as written): colour data
/// finer than 8 bits, such as a projector's gain map.
pub fn encode_png_rgb16(width: u32, height: u32, values: &[u16]) -> Result<Vec<u8>, Error> {
    let image = Image::from_u16(width, height, ChannelLayout::Rgb, values)
        .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
    photocraft_codecs::encode(
        &image,
        photocraft_codecs::Format::Png,
        &photocraft_codecs::EncodeOptions::default(),
    )
    .map_err(|e| Error::new(format!("cannot encode the PNG: {e}")))
}

/// An OpenEXR file of 32-bit float RGBA rows (linear, as stored; no alpha conversion): data such
/// as a `tex.remap` map, which 8- and 16-bit files would take as sRGB and too coarse.
pub fn encode_exr_rgba32f(width: u32, height: u32, rgba: &[[f32; 4]]) -> Result<Vec<u8>, Error> {
    let flat: Vec<f32> = rgba.iter().flatten().copied().collect();
    let image = Image::from_f32(width, height, ChannelLayout::Rgba, &flat)
        .map_err(|e| Error::new(format!("cannot build the image: {e}")))?;
    photocraft_codecs::encode(
        &image,
        photocraft_codecs::Format::OpenExr,
        &photocraft_codecs::EncodeOptions::default(),
    )
    .map_err(|e| Error::new(format!("cannot encode the EXR: {e}")))
}

impl Gpu {
    /// A texture holding `image`. Safe to call off the cook thread.
    pub fn upload(&self, image: &Decoded, label: &str) -> Result<Tex, Error> {
        let tex = Tex::new(self, image.width, image.height, label)?;
        let bytes: Vec<u8> = image.rgba.iter().flat_map(|h| h.to_le_bytes()).collect();
        let ((), error) = self.scoped(|| {
            self.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 8),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 },
            );
        });
        match error {
            Some(e) => Err(Error::new(format!("upload failed: {e}"))),
            None => Ok(tex),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;
    use photocraft_codecs::{EncodeOptions, Format};

    fn png16(w: u32, h: u32, samples: &[u16]) -> Vec<u8> {
        let img = Image::from_u16(w, h, ChannelLayout::Rgba, samples).unwrap();
        photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap()
    }

    #[test]
    fn sixteen_bit_is_linearised_and_premultiplied() {
        // sRGB 0.5 is linear 0.214; at alpha 0.5 premultiplied 0.107.
        let half = 32768u16;
        let d = decode(&png16(1, 1, &[half, 0, 65535, half]), 64).unwrap();
        let v: Vec<f32> = d.rgba.iter().map(|h| h.to_f32()).collect();
        assert!((v[0] - 0.107).abs() < 1e-3, "{v:?}");
        assert_eq!(v[1], 0.0);
        assert!((v[2] - 0.5).abs() < 1e-3);
        assert!((v[3] - 0.5).abs() < 1e-3);
        assert_eq!(d.source, "rgba 16-bit");
    }

    #[test]
    fn float_exr_keeps_values_outside_0_to_1() {
        let px = [[0.25, -1.0, 0.75, 1.0], [2.5, 0.5, 0.0, 1.0]];
        let d = decode(&encode_exr_rgba32f(2, 1, &px).unwrap(), 64).unwrap();
        let v: Vec<f32> = d.rgba.iter().map(|h| h.to_f32()).collect();
        assert_eq!(v[..4], [0.25, -1.0, 0.75, 1.0]);
        assert_eq!(v[4..], [2.5, 0.5, 0.0, 1.0]);
        assert_eq!(d.source, "rgba float");
    }

    #[test]
    fn bombs_and_garbage_are_errors() {
        // A header claiming 100000 x 100000 pixels.
        let mut png = png16(1, 1, &[0, 0, 0, 0]);
        png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(decode(&png, 8192).is_err());
        assert!(decode(b"not an image", 8192).is_err());
        // Larger than the texture limit.
        assert!(decode(&png16(4, 1, &[0; 16]), 2).is_err());
    }

    #[test]
    fn upload_and_read_back() {
        let Some(gpu) = gpu() else { return };
        let d = decode(&png16(2, 1, &[65535, 0, 0, 65535, 0, 0, 0, 0]), 64).unwrap();
        let tex = gpu.upload(&d, "img").unwrap();
        let px = gpu.read(&tex).unwrap();
        assert_eq!(px.data, [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 0.0]]);
    }
}
