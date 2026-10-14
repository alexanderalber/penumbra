//! Block-compressed video frames (HAP) to textures: the blocks are uploaded as they are, as
//! BC1, BC3, BC4 or BC7 textures the GPU decompresses when sampling, and a shader converts them
//! to [`crate::FORMAT`] (sRGB to linear, Hap Q's YCoCg to RGB, premultiplied alpha). The upload is
//! a quarter (BC3) or an eighth (BC1) of an RGBA8 frame. Needs the device feature
//! `TEXTURE_COMPRESSION_BC`, which [`crate::device_descriptor`] requests where the adapter has
//! it (every DX12 GPU and desktop Vulkan GPU does).

use std::sync::Arc;

use penumbra_core::{Error, lock};

use crate::{FORMAT, Gpu, Tex};

const SOURCE: &str = include_str!("bc.wgsl");

/// A block-compressed texture format. Blocks are 4 x 4 pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcFormat {
    /// RGB, 8 bytes per block.
    Bc1,
    /// RGBA, 16 bytes per block.
    Bc3,
    /// One channel, 8 bytes per block.
    Bc4,
    /// RGBA, 16 bytes per block.
    Bc7,
}

impl BcFormat {
    fn block_bytes(self) -> u32 {
        match self {
            Self::Bc1 | Self::Bc4 => 8,
            Self::Bc3 | Self::Bc7 => 16,
        }
    }

    fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            Self::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
            Self::Bc4 => wgpu::TextureFormat::Bc4RUnorm,
            Self::Bc7 => wgpu::TextureFormat::Bc7RgbaUnorm,
        }
    }
}

/// What the texels mean. Colour is sRGB-encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcColor {
    /// Opaque RGB.
    Rgb,
    /// RGB with straight alpha.
    Rgba,
    /// Scaled YCoCg (Hap Q): Co, Cg, scale, Y in R, G, B, A.
    YCoCg,
    /// Alpha only, in the red channel.
    Alpha,
}

/// One frame to convert. Texture data is whole blocks, rows of blocks top first.
pub struct BcFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub color: BcColor,
    pub format: BcFormat,
    pub data: &'a [u8],
    /// A BC4 alpha texture (Hap Q Alpha).
    pub alpha: Option<&'a [u8]>,
}

/// The conversion pipeline, compiled once per device ([`Gpu::bc_pipeline`]).
pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl Pipeline {
    fn compile(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let ((pipeline, layout), error) = gpu.scoped(|| {
            let texture = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bc textures"),
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
                    texture(1),
                    texture(2),
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bc"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("bc.wgsl"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("bc"),
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
            Some(e) => Err(Error::new(format!("bc.wgsl: {e}"))),
            None => Ok(Self { pipeline, layout }),
        }
    }
}

/// An uploaded texture, kept while frames keep its size and format.
struct Slot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    format: wgpu::TextureFormat,
}

/// Converts frames of one HAP stream, reusing its textures from frame to frame. Owned by the
/// node that plays the stream.
#[derive(Default)]
pub struct BcConverter {
    /// Colour, alpha.
    slots: [Option<Slot>; 2],
}

impl BcConverter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Uploads `frame` and renders it into `target`, which must have the frame's size.
    pub fn convert(&mut self, gpu: &Gpu, frame: &BcFrame<'_>, target: &Tex) -> Result<(), Error> {
        let (w, h) = (frame.width, frame.height);
        if target.size() != (w, h) {
            return Err(Error::new(format!("target is {:?}, the frame {w}x{h}", target.size())));
        }
        if !gpu.device().features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC) {
            return Err(Error::new("this GPU cannot sample BC textures, which HAP needs"));
        }
        let pipeline = gpu.bc_pipeline()?;
        self.write(gpu, 0, frame.format, w, h, frame.data)?;
        match frame.alpha {
            Some(a) => self.write(gpu, 1, BcFormat::Bc4, w, h, a)?,
            // An unused binding still needs a texture; a 4x4 BC4 block is the smallest.
            None if matches!(self.slots, [_, None]) => self.write(gpu, 1, BcFormat::Bc4, 4, 4, &[0; 8])?,
            None => {}
        }
        let mode: u32 = match frame.color {
            BcColor::Rgb => 0,
            BcColor::Rgba => 1,
            BcColor::YCoCg => 2,
            BcColor::Alpha => 3,
        };
        let words = [mode, u32::from(frame.alpha.is_some()), 0, 0];
        let uniform: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
        let device = gpu.device();
        let ((), error) = gpu.scoped(|| {
            let buffer = gpu.uniform_buffer("bc conv", &uniform);
            let [Some(color), Some(alpha)] = &self.slots else {
                return;
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bc textures"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&color.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&alpha.view),
                    },
                ],
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("bc") });
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("bc"),
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
            Some(e) => Err(Error::new(format!("HAP conversion failed: {e}"))),
            None => Ok(()),
        }
    }

    /// Writes one texture of `width` x `height` pixels (padded to whole blocks), (re)creating it
    /// when size or format changed.
    fn write(
        &mut self,
        gpu: &Gpu,
        index: usize,
        format: BcFormat,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let max = gpu.max_texture_size();
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::new(format!("a {width}x{height} video does not fit the GPU (1 to {max})")));
        }
        let (bw, bh) = (width.div_ceil(4), height.div_ceil(4));
        let row = bw * format.block_bytes();
        let need = u64::from(row) * u64::from(bh);
        if bytes.len() as u64 != need {
            return Err(Error::new(format!(
                "a {width}x{height} {format:?} texture needs {need} bytes, got {}",
                bytes.len()
            )));
        }
        let size = (bw * 4, bh * 4);
        let slot = self.slots.get_mut(index).ok_or_else(|| Error::new("no such texture"))?;
        let fits = slot.as_ref().is_some_and(|s| (s.size, s.format) == (size, format.wgpu()));
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        if !fits {
            let (texture, error) = gpu.scoped(|| {
                gpu.device().create_texture(&wgpu::TextureDescriptor {
                    label: Some("hap texture"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: format.wgpu(),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            });
            if let Some(e) = error {
                return Err(Error::new(format!("cannot create a HAP texture: {e}")));
            }
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            *slot = Some(Slot { texture, view, size, format: format.wgpu() });
        }
        let Some(slot) = slot.as_ref() else {
            return Err(Error::new("no HAP texture"));
        };
        let ((), error) = gpu.scoped(|| {
            gpu.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &slot.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(bh) },
                extent,
            );
        });
        match error {
            Some(e) => Err(Error::new(format!("HAP upload failed: {e}"))),
            None => Ok(()),
        }
    }
}

impl Gpu {
    /// The BC conversion pipeline, compiled on first use and shared.
    pub fn bc_pipeline(&self) -> Result<Arc<Pipeline>, Error> {
        let mut slot = lock(&self.inner.bc);
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

    /// A BC1 block of one colour (565): both endpoints the colour, every index 0.
    fn bc1_solid(c: u16) -> [u8; 8] {
        let [a, b] = c.to_le_bytes();
        [a, b, a, b, 0, 0, 0, 0]
    }

    /// A BC3 block: alpha `a` everywhere (both endpoints, indices 0), colour as [`bc1_solid`].
    fn bc3_solid(a: u8, c: u16) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = a;
        b[1] = a;
        b[8..].copy_from_slice(&bc1_solid(c));
        b
    }

    fn bc4_solid(v: u8) -> [u8; 8] {
        [v, v, 0, 0, 0, 0, 0, 0]
    }

    fn srgb(v: f32) -> f32 {
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }

    fn convert(gpu: &Gpu, frame: &BcFrame<'_>) -> crate::Pixels {
        let target = Tex::new(gpu, frame.width, frame.height, "t").unwrap();
        BcConverter::new().convert(gpu, frame, &target).unwrap();
        gpu.read(&target).unwrap()
    }

    fn close(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tolerance)
    }

    #[test]
    fn bc1_padded_frame_keeps_block_order_and_is_linear() {
        let Some(gpu) = gpu() else { return };
        assert!(gpu.device().features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC));
        // 6x5 pixels: 2x2 blocks. Red, green / blue, white.
        let mut data = Vec::new();
        for c in [0xF800u16, 0x07E0, 0x001F, 0xFFFF] {
            data.extend(bc1_solid(c));
        }
        let px = convert(
            &gpu,
            &BcFrame {
                width: 6,
                height: 5,
                color: BcColor::Rgb,
                format: BcFormat::Bc1,
                data: &data,
                alpha: None,
            },
        );
        assert_eq!(px.get(0, 0).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px.get(5, 0).unwrap(), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(px.get(0, 4).unwrap(), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(px.get(5, 4).unwrap(), [1.0, 1.0, 1.0, 1.0]);

        // Mid grey (565: 16, 32, 16) decodes through the sRGB curve.
        let grey = bc1_solid((16 << 11) | (32 << 5) | 16);
        let px = convert(
            &gpu,
            &BcFrame {
                width: 4,
                height: 4,
                color: BcColor::Rgb,
                format: BcFormat::Bc1,
                data: &grey,
                alpha: None,
            },
        );
        let r = srgb(132.0 / 255.0);
        assert!(close(px.get(2, 2).unwrap(), [r, srgb(130.0 / 255.0), r, 1.0], 2e-3), "{:?}", px.get(2, 2));
    }

    #[test]
    fn bc3_alpha_is_premultiplied_and_ycocg_with_alpha_decodes() {
        let Some(gpu) = gpu() else { return };
        let data = bc3_solid(128, 0xFFFF);
        let px = convert(
            &gpu,
            &BcFrame {
                width: 4,
                height: 4,
                color: BcColor::Rgba,
                format: BcFormat::Bc3,
                data: &data,
                alpha: None,
            },
        );
        let a = 128.0 / 255.0;
        assert!(close(px.get(0, 0).unwrap(), [a, a, a, a], 2e-3), "{:?}", px.get(0, 0));

        // Scaled YCoCg: Co = Cg = 0 (128 in 565: R 16, G 32), scale 0 (B 0), Y in alpha.
        let y = 200u8;
        let data = bc3_solid(y, (16 << 11) | (32 << 5));
        let alpha = bc4_solid(255);
        let px = convert(
            &gpu,
            &BcFrame {
                width: 4,
                height: 4,
                color: BcColor::YCoCg,
                format: BcFormat::Bc3,
                data: &data,
                alpha: Some(&alpha),
            },
        );
        // 565 cannot hold 128: R 16 and G 32 decode as 132 and 130, so Co and Cg are a little
        // off zero, and the expected colour follows them.
        let (y, co, cg) = (f32::from(y) / 255.0, 132.0 / 255.0 - 0.50196, 130.0 / 255.0 - 0.50196);
        let expect = [srgb(y + co - cg), srgb(y + cg), srgb(y - co - cg), 1.0];
        assert!(close(px.get(1, 1).unwrap(), expect, 5e-3), "{:?} vs {expect:?}", px.get(1, 1));

        // A wrong length is refused, not read past; so is a target of another size.
        let target = Tex::new(&gpu, 8, 4, "t").unwrap();
        let short = BcFrame {
            width: 8,
            height: 4,
            color: BcColor::Rgba,
            format: BcFormat::Bc3,
            data: &data,
            alpha: None,
        };
        let e = BcConverter::new().convert(&gpu, &short, &target).unwrap_err();
        assert!(e.0.contains("needs 32 bytes"), "{e}");
        let small = BcFrame { width: 4, height: 4, ..short };
        assert!(BcConverter::new().convert(&gpu, &small, &target).is_err());
        assert_eq!(gpu.errors().count, 0);
    }
}
