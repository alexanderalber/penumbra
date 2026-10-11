//! Decoded video frames to textures: the planes are uploaded as they come from the decoder
//! (Y'CbCr 4:2:0 to 4:4:4, 8 to 16 bits, or sRGB RGBA) and converted to [`crate::FORMAT`] by a
//! shader. Converting on the CPU halved decode throughput when measured; the upload of the planes
//! is about a third of an RGBA16F frame for 4:2:0.

use std::sync::Arc;

use penumbra_core::Error;

use crate::{FORMAT, Gpu, Tex};

const SOURCE: &str = include_str!("video.wgsl");

/// Samples as the decoder delivered them. Plane sizes follow from the frame size and the
/// chroma shifts; slices that are too short are refused, never read past.
pub enum Planes<'a> {
    /// 8-bit Y'CbCr, optionally with a full-resolution alpha plane.
    Yuv8 { planes: [&'a [u8]; 3], alpha: Option<&'a [u8]> },
    /// 9- to 16-bit Y'CbCr in `u16`, `bits` significant bits, low aligned.
    Yuv16 { planes: [&'a [u16]; 3], alpha: Option<&'a [u16]>, bits: u32 },
    /// sRGB-encoded RGBA, straight alpha, 8 bits per channel.
    Rgba8(&'a [u8]),
}

/// How Y'CbCr values become linear RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoColor {
    /// Luma coefficients (Kr, Kb) of the matrix: BT.601, BT.709 or BT.2020.
    pub kr_kb: (f32, f32),
    pub full_range: bool,
    pub transfer: Transfer,
}

/// Transfer curves the shader decodes. Primaries are not converted (BT.2020 content shows with
/// BT.709 primaries, slightly desaturated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// sRGB curve, also for BT.709 (as FilmCraft and most players do).
    Srgb,
    Linear,
    /// SMPTE ST 2084, scaled so 100 nits is 1.0.
    Pq,
    Hlg,
}

/// One frame to convert.
pub struct VideoPlanes<'a> {
    pub width: u32,
    pub height: u32,
    /// Chroma subsampling as (horizontal, vertical) shifts: (1, 1) is 4:2:0.
    pub shift: (u32, u32),
    pub planes: Planes<'a>,
    pub color: VideoColor,
}

/// The conversion pipeline, compiled once per device ([`Gpu::video_pipeline`]).
pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl Pipeline {
    pub(crate) fn compile(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let ((pipeline, layout), error) = gpu.scoped(|| {
            let plane = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("video planes"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    plane(1),
                    plane(2),
                    plane(3),
                    plane(4),
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("video"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("video.wgsl"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("video"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            });
            (pipeline, layout)
        });
        match error {
            Some(e) => Err(Error::new(format!("video.wgsl: {e}"))),
            None => Ok(Self { pipeline, layout }),
        }
    }
}

/// A plane texture, kept while frames keep its size and format.
struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// Converts frames of one video stream, reusing its plane textures from frame to frame. Owned
/// by the node that plays the stream.
#[derive(Default)]
pub struct Converter {
    planes: [Option<Plane>; 4],
}

impl Converter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Uploads `frame` and renders it into `target`, which must have the frame's size.
    pub fn convert(&mut self, gpu: &Gpu, frame: &VideoPlanes<'_>, target: &Tex) -> Result<(), Error> {
        let (w, h) = (frame.width, frame.height);
        if target.size() != (w, h) {
            return Err(Error::new(format!("target is {:?}, the frame {w}x{h}", target.size())));
        }
        let (sx, sy) = frame.shift;
        if sx > 1 || sy > 1 {
            return Err(Error::new(format!("unsupported chroma subsampling ({sx}, {sy})")));
        }
        let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        let pipeline = gpu.video_pipeline()?;
        let mut bits = 8;
        let mut has_alpha = false;
        let mode = match &frame.planes {
            Planes::Yuv8 { planes: [y, cb, cr], alpha } => {
                self.write(gpu, 0, wgpu::TextureFormat::R8Uint, w, h, y, 1)?;
                self.write(gpu, 1, wgpu::TextureFormat::R8Uint, cw, ch, cb, 1)?;
                self.write(gpu, 2, wgpu::TextureFormat::R8Uint, cw, ch, cr, 1)?;
                if let Some(a) = alpha {
                    self.write(gpu, 3, wgpu::TextureFormat::R8Uint, w, h, a, 1)?;
                    has_alpha = true;
                }
                0
            }
            Planes::Yuv16 { planes: [y, cb, cr], alpha, bits: b } => {
                if !(9..=16).contains(b) {
                    return Err(Error::new(format!("unsupported bit depth {b}")));
                }
                bits = *b;
                let f = wgpu::TextureFormat::R16Uint;
                self.write(gpu, 0, f, w, h, bytemuck::cast_slice(y), 2)?;
                self.write(gpu, 1, f, cw, ch, bytemuck::cast_slice(cb), 2)?;
                self.write(gpu, 2, f, cw, ch, bytemuck::cast_slice(cr), 2)?;
                if let Some(a) = alpha {
                    self.write(gpu, 3, f, w, h, bytemuck::cast_slice(a), 2)?;
                    has_alpha = true;
                }
                0
            }
            Planes::Rgba8(data) => {
                self.write(gpu, 0, wgpu::TextureFormat::Rgba8Uint, w, h, data, 4)?;
                1
            }
        };
        // Planes a format does not use are bound to whatever 1x1 stand-in exists.
        for i in 1..4 {
            let unused = match i {
                3 => !has_alpha,
                _ => mode == 1,
            };
            if unused && self.planes.get(i).is_none_or(Option::is_none) {
                self.write(gpu, i, wgpu::TextureFormat::R8Uint, 1, 1, &[0], 1)?;
            }
        }
        let transfer = match frame.color.transfer {
            Transfer::Srgb => 0u32,
            Transfer::Linear => 1,
            Transfer::Pq => 2,
            Transfer::Hlg => 3,
        };
        let (kr, kb) = frame.color.kr_kb;
        let words: [u32; 12] = [
            w,
            h,
            sx,
            sy,
            mode,
            bits,
            u32::from(frame.color.full_range),
            transfer,
            kr.to_bits(),
            kb.to_bits(),
            u32::from(has_alpha),
            0,
        ];
        let uniform: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
        let device = gpu.device();
        let ((), error) = gpu.scoped(|| {
            let buffer = gpu.uniform_buffer("video conv", &uniform);
            let view = |i: usize| self.planes.get(i).and_then(Option::as_ref).map(|p| &p.view);
            let (Some(v0), Some(v1), Some(v2), Some(v3)) = (view(0), view(1), view(2), view(3)) else {
                return;
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video planes"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(v0) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(v1) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(v2) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(v3) },
                ],
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("video") });
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("video"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(&pipeline.pipeline);
                rp.set_bind_group(0, &group, &[]);
                rp.draw(0..3, 0..1);
            }
            gpu.submit([encoder.finish()]);
        });
        match error {
            Some(e) => Err(Error::new(format!("video conversion failed: {e}"))),
            None => Ok(()),
        }
    }

    /// Writes one plane, (re)creating its texture when size or format changed.
    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        gpu: &Gpu,
        index: usize,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        bytes: &[u8],
        texel: u32,
    ) -> Result<(), Error> {
        let need = u64::from(width) * u64::from(height) * u64::from(texel);
        if (bytes.len() as u64) < need {
            return Err(Error::new(format!(
                "plane {index} has {} bytes, {width}x{height} needs {need}",
                bytes.len()
            )));
        }
        let max = gpu.max_texture_size();
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::new(format!("a {width}x{height} video does not fit the GPU (1 to {max})")));
        }
        let slot = self.planes.get_mut(index).ok_or_else(|| Error::new("no such plane"))?;
        let fits = slot.as_ref().is_some_and(|p| (p.width, p.height, p.format) == (width, height, format));
        if !fits {
            let (texture, error) = gpu.scoped(|| {
                gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("video plane"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            });
            if let Some(e) = error {
                return Err(Error::new(format!("cannot create a video plane: {e}")));
            }
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            *slot = Some(Plane { texture, view, width, height, format });
        }
        let Some(plane) = slot.as_ref() else {
            return Err(Error::new("no video plane"));
        };
        let len = usize::try_from(need).map_err(|_| Error::new("plane too large"))?;
        let data = bytes.get(..len).ok_or_else(|| Error::new("plane too short"))?;
        let ((), error) = gpu.scoped(|| {
            gpu.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &plane.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * texel),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        });
        match error {
            Some(e) => Err(Error::new(format!("video upload failed: {e}"))),
            None => Ok(()),
        }
    }
}

impl Gpu {
    /// The video conversion pipeline, compiled on first use and shared.
    pub fn video_pipeline(&self) -> Result<Arc<Pipeline>, Error> {
        let mut slot = self.inner.video.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = Arc::new(Pipeline::compile(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    const BT709: VideoColor =
        VideoColor { kr_kb: (0.2126, 0.0722), full_range: false, transfer: Transfer::Srgb };

    fn convert(gpu: &Gpu, frame: &VideoPlanes<'_>) -> crate::Pixels {
        let target = Tex::new(gpu, frame.width, frame.height, "t").unwrap();
        Converter::new().convert(gpu, frame, &target).unwrap();
        gpu.read(&target).unwrap()
    }

    fn srgb(v: f32) -> f32 {
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }

    #[test]
    fn limited_range_420_white_black_and_red() {
        let Some(gpu) = gpu() else { return };
        // 4x2 luma: left half white (235), right half black (16); chroma neutral (128).
        let y = [235u8, 235, 16, 16, 235, 235, 16, 16];
        let cb = [128u8, 128];
        let cr = [128u8, 128];
        let px = convert(
            &gpu,
            &VideoPlanes {
                width: 4,
                height: 2,
                shift: (1, 1),
                planes: Planes::Yuv8 { planes: [&y, &cb, &cr], alpha: None },
                color: BT709,
            },
        );
        let white = px.get(0, 0).unwrap();
        let black = px.get(3, 1).unwrap();
        assert!(white.iter().all(|c| (c - 1.0).abs() < 2e-3), "{white:?}");
        assert!(black[..3].iter().all(|c| c.abs() < 1e-3) && black[3] == 1.0, "{black:?}");

        // BT.709 red (R' = 1): Y' 63, Cb 102, Cr 240 in 8-bit limited range.
        let one = |v: u8| [v];
        let (y, cb, cr) = (one(63), one(102), one(240));
        let px = convert(
            &gpu,
            &VideoPlanes {
                width: 1,
                height: 1,
                shift: (0, 0),
                planes: Planes::Yuv8 { planes: [&y, &cb, &cr], alpha: None },
                color: BT709,
            },
        );
        let [r, g, b, _] = px.get(0, 0).unwrap();
        assert!((r - 1.0).abs() < 0.02 && g < 0.01 && b < 0.01, "{r} {g} {b}");
    }

    #[test]
    fn ten_bit_full_range_with_alpha_is_premultiplied() {
        let Some(gpu) = gpu() else { return };
        // Mid grey at half alpha, 10-bit full range.
        let y = [512u16];
        let c = [512u16];
        let a = [512u16];
        let px = convert(
            &gpu,
            &VideoPlanes {
                width: 1,
                height: 1,
                shift: (0, 0),
                planes: Planes::Yuv16 { planes: [&y, &c, &c], alpha: Some(&a), bits: 10 },
                color: VideoColor { full_range: true, ..BT709 },
            },
        );
        let [r, g, b, alpha] = px.get(0, 0).unwrap();
        let expect = srgb(512.0 / 1023.0) * (512.0 / 1023.0);
        assert!((alpha - 512.0 / 1023.0).abs() < 1e-3, "{alpha}");
        for v in [r, g, b] {
            assert!((v - expect).abs() < 2e-3, "{v} vs {expect}");
        }
    }

    #[test]
    fn rgba8_is_linearised_and_premultiplied_and_short_planes_refused() {
        let Some(gpu) = gpu() else { return };
        let data = [255u8, 128, 0, 128];
        let px = convert(
            &gpu,
            &VideoPlanes { width: 1, height: 1, shift: (0, 0), planes: Planes::Rgba8(&data), color: BT709 },
        );
        let [r, g, b, a] = px.get(0, 0).unwrap();
        let alpha = 128.0 / 255.0;
        assert!((a - alpha).abs() < 1e-3);
        assert!((r - alpha).abs() < 2e-3 && (g - srgb(128.0 / 255.0) * alpha).abs() < 2e-3 && b == 0.0);

        let target = Tex::new(&gpu, 2, 2, "t").unwrap();
        let short = [0u8; 3];
        let e = Converter::new()
            .convert(
                &gpu,
                &VideoPlanes {
                    width: 2,
                    height: 2,
                    shift: (0, 0),
                    planes: Planes::Rgba8(&short),
                    color: BT709,
                },
                &target,
            )
            .unwrap_err();
        assert!(e.0.contains("needs"), "{e}");
        assert_eq!(gpu.errors().count, 0);
    }
}
