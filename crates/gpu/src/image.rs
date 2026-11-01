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

/// An image of 32-bit floats ready for upload: linear, premultiplied RGBA. A scan's maps are
/// such files; half floats would round their positions (see [`crate::FLOAT_FORMAT`]).
pub struct FloatImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[f32; 4]>,
    pub source: String,
}

impl FloatImage {
    /// The same picture rounded to half floats.
    pub fn to_half(&self) -> Decoded {
        let rgba = self.rgba.iter().flat_map(|c| c.map(f16::from_f32)).collect();
        Decoded { width: self.width, height: self.height, rgba, source: self.source.clone() }
    }
}

/// A decoded file as it goes onto the GPU: half floats, or 32-bit floats where the file holds
/// them.
pub enum Picture {
    Half(Decoded),
    Float(FloatImage),
}

impl Picture {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Picture::Half(d) => (d.width, d.height),
            Picture::Float(f) => (f.width, f.height),
        }
    }
}

fn read_file(path: &std::path::Path) -> Result<Vec<u8>, Error> {
    let len = std::fs::metadata(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))?.len();
    if len > MAX_FILE_BYTES {
        return Err(Error::new(format!("{}: {len} bytes is too large", path.display())));
    }
    std::fs::read(path).map_err(|e| Error::new(format!("{}: {e}", path.display())))
}

/// Reads and decodes `path` for a texture at most `max_side` pixels on each side.
pub fn decode_file(path: &std::path::Path, max_side: u32) -> Result<Decoded, Error> {
    let bytes = read_file(path)?;
    decode(&bytes, max_side).map_err(|e| e.context(path.display()))
}

/// [`decode_file`], keeping 32-bit float samples as they are ([`Picture::Float`]).
pub fn decode_file_picture(path: &std::path::Path, max_side: u32) -> Result<Picture, Error> {
    let bytes = read_file(path)?;
    decode_picture(&bytes, max_side).map_err(|e| e.context(path.display()))
}

/// [`decode`], keeping 32-bit float samples as they are ([`Picture::Float`]).
pub fn decode_picture(bytes: &[u8], max_side: u32) -> Result<Picture, Error> {
    let opts = DecodeOptions { limits: limits(max_side), keep_orientation: false };
    let image = photocraft_codecs::decode_with(bytes, &opts).map_err(|e| Error::new(e.to_string()))?;
    if image.sample_type() != SampleType::F32 {
        return Ok(Picture::Half(to_linear(&image)?));
    }
    let (width, height) = image.dimensions();
    let source = format!("{} float", format!("{:?}", image.layout()).to_lowercase());
    let rgba = image.converted(ChannelLayout::Rgba, SampleType::F32);
    let finite = |c: f32| if c.is_finite() { c } else { 0.0 };
    let samples: Vec<f32> =
        rgba.data().as_chunks::<4>().0.iter().map(|b| finite(f32::from_ne_bytes(*b))).collect();
    let pixels: Vec<[f32; 4]> = samples
        .as_chunks::<4>()
        .0
        .iter()
        .map(|[r, g, b, a]| {
            let a = a.clamp(0.0, 1.0);
            [r * a, g * a, b * a, a]
        })
        .collect();
    if pixels.len() as u64 != u64::from(width) * u64::from(height) {
        return Err(Error::new(format!("decoded {} pixels, expected {width} x {height}", pixels.len())));
    }
    Ok(Picture::Float(FloatImage { width, height, rgba: pixels, source }))
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

/// `png` made at most `max_side` pixels on its longer side, as an 8-bit RGBA PNG: pictures sent
/// where size costs, such as a screenshot of a 4K window to a language model. Each new pixel
/// averages the pixels it covers, weighted by alpha. Returns the bytes unchanged when the
/// picture already fits or `max_side` is 0.
pub fn shrink_png(png: &[u8], max_side: u32) -> Result<Vec<u8>, Error> {
    let opts = DecodeOptions { limits: limits(16_384), keep_orientation: false };
    let image = photocraft_codecs::decode_with(png, &opts).map_err(|e| Error::new(e.to_string()))?;
    let (w, h) = image.dimensions();
    if max_side == 0 || w.max(h) <= max_side {
        return Ok(png.to_vec());
    }
    let scale = f64::from(max_side) / f64::from(w.max(h));
    let nw = ((f64::from(w) * scale).round() as u32).max(1);
    let nh = ((f64::from(h) * scale).round() as u32).max(1);
    let src = image.to_rgba8();
    let mut out = Vec::with_capacity(nw as usize * nh as usize * 4);
    let span = |i: u32, n: u32, total: u32| {
        let a = (u64::from(i) * u64::from(total) / u64::from(n)) as usize;
        let b = (u64::from(i + 1) * u64::from(total) / u64::from(n)) as usize;
        (a, b.max(a + 1))
    };
    for y in 0..nh {
        let (y0, y1) = span(y, nh, h);
        for x in 0..nw {
            let (x0, x1) = span(x, nw, w);
            let (mut rgb, mut alpha) = ([0.0f64; 3], 0.0f64);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * w as usize + sx) * 4;
                    let Some(&[r, g, b, a]) = src.get(i..i + 4) else { continue };
                    let a = f64::from(a);
                    for (sum, c) in rgb.iter_mut().zip([r, g, b]) {
                        *sum += f64::from(c) * a;
                    }
                    alpha += a;
                }
            }
            for sum in rgb {
                out.push(if alpha > 0.0 { (sum / alpha).round() as u8 } else { 0 });
            }
            out.push((alpha / ((y1 - y0) * (x1 - x0)) as f64).round() as u8);
        }
    }
    crate::encode_png_rgba8(nw, nh, out)
}

impl Gpu {
    /// A texture holding `picture`: [`crate::FLOAT_FORMAT`] for 32-bit floats where the device
    /// filters it ([`Gpu::float_textures`]), else [`crate::FORMAT`]. Safe to call off the cook
    /// thread.
    pub fn upload_picture(&self, picture: &Picture, label: &str) -> Result<Tex, Error> {
        match picture {
            Picture::Half(d) => self.upload(d, label),
            Picture::Float(f) if self.float_textures() => self.upload_float(f, label),
            Picture::Float(f) => self.upload(&f.to_half(), label),
        }
    }

    /// A texture of [`crate::FLOAT_FORMAT`] holding `image`.
    pub fn upload_float(&self, image: &FloatImage, label: &str) -> Result<Tex, Error> {
        let tex = Tex::new_float(self, image.width, image.height, label)?;
        let bytes: Vec<u8> = image.rgba.iter().flatten().flat_map(|f| f.to_le_bytes()).collect();
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
                    bytes_per_row: Some(image.width * 16),
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
    fn shrinking_keeps_the_shape_and_averages_by_alpha() {
        // 4 x 2: left half opaque red over transparent green, right half opaque blue.
        let mut rgba = Vec::new();
        for _row in 0..2 {
            rgba.extend_from_slice(&[255, 0, 0, 255, 0, 255, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255]);
        }
        let png = crate::encode_png_rgba8(4, 2, rgba).unwrap();
        assert_eq!(shrink_png(&png, 4).unwrap(), png, "fits already");
        assert_eq!(shrink_png(&png, 0).unwrap(), png, "0 means no limit");
        let small = photocraft_codecs::decode(&shrink_png(&png, 2).unwrap()).unwrap();
        assert_eq!(small.dimensions(), (2, 1));
        // The transparent green does not tint the red; alpha is the plain average.
        assert_eq!(small.to_rgba8(), [255, 0, 0, 128, 0, 0, 255, 255]);
        assert!(shrink_png(b"not a picture", 2).is_err());
    }

    /// The error of a map's red, as a pass sampling it sees it, against x + 0.5 + `offset` at each
    /// pixel of a 1920-pixel row: largest and mean, in pixels.
    fn map_error(gpu: &Gpu, map: &Tex, offset: f32) -> (f32, f32) {
        let (w, h) = map.size();
        let pass = crate::Pass::compile(
            gpu,
            "map error",
            "@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let m = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    return vec4f(m.r * pn.resolution.x - v.pos.x, 0.0, 0.0, 1.0);
}
",
            &[],
        )
        .unwrap();
        let out = Tex::new(gpu, w, h, "error").unwrap();
        let input = crate::PassInput { inputs: &[Some(map)], seconds: 0.0, frame: 0, param: &|_: &str| None };
        pass.render(gpu, &out, &input).unwrap();
        let px = gpu.read(&out).unwrap();
        let errors: Vec<f32> = px.data.iter().map(|c| (c[0] - offset).abs()).collect();
        let worst = errors.iter().copied().fold(0.0, f32::max);
        (worst, errors.iter().sum::<f32>() / errors.len() as f32)
    }

    #[test]
    fn a_float_map_keeps_a_fraction_of_a_pixel_where_half_floats_round_it() {
        let Some(gpu) = gpu() else { return };
        assert!(gpu.float_textures(), "the adapter filters 32-bit floats");
        // Positions a third of a pixel off each pixel's centre, as a decode with sines gives them.
        let (w, h, offset) = (1920u32, 2u32, 0.37f32);
        let rgba: Vec<[f32; 4]> =
            (0..w * h).map(|i| [((i % w) as f32 + 0.5 + offset) / w as f32, 0.5, 1.0, 1.0]).collect();
        let map = FloatImage { width: w, height: h, rgba, source: String::new() };
        let half = gpu.upload(&map.to_half(), "map").unwrap();
        let float = gpu.upload_picture(&Picture::Float(map), "map").unwrap();
        assert_eq!((float.format(), float.bytes()), (crate::FLOAT_FORMAT, u64::from(w * h) * 16));
        assert_eq!(half.bytes(), u64::from(w * h) * 8);
        let (f_worst, f_mean) = map_error(&gpu, &float, offset);
        let (h_worst, h_mean) = map_error(&gpu, &half, offset);
        eprintln!("map error px: float worst {f_worst} mean {f_mean}; half worst {h_worst} mean {h_mean}");
        assert!(f_worst < 0.01, "{f_worst}");
        assert!(h_worst > 0.4, "{h_worst}");
        // Read back whole, converted for copies, readback rows and mip chains.
        let back = gpu.read(&float).unwrap();
        assert_eq!(back.data[1919][0], (1919.0 + 0.5 + offset) / 1920.0);
        let converted = gpu.to_half(&float).unwrap();
        assert_eq!(converted.format(), crate::FORMAT);
        let target = Tex::new(&gpu, w, h, "copy").unwrap();
        gpu.copy(&float, &target).unwrap();
        let copied = gpu.read(&target).unwrap();
        assert!((copied.data[1919][0] - back.data[1919][0]).abs() < 1e-3);
        let pending = gpu.read_async(&[&float]).unwrap();
        assert_eq!(pending.wait(&gpu, std::time::Duration::from_secs(10)), crate::ReadState::Ready);
        let mut rows = 0;
        pending
            .rows(0, |r| -> Result<(), ()> {
                assert_eq!(r.len(), w as usize * 8);
                rows += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(rows, h);
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
