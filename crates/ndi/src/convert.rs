//! The GPU halves of sending and receiving: received bytes to a texture ([`Unpacker`],
//! [`unpack.wgsl`](../unpack.wgsl)), and a texture to the bytes to send, read back without
//! waiting ([`Packer`], [`pack.wgsl`](../pack.wgsl)).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use penumbra_core::lock;
use penumbra_gpu::{Alpha, Gpu, Tex};

use crate::ffi::fourcc;

const UNPACK: &str = include_str!("unpack.wgsl");
const PACK: &str = include_str!("pack.wgsl");

/// The luma coefficients (Kr, Kb) NDI uses for Y'CbCr at a picture size: BT.601 for SD, BT.709
/// for HD, BT.2020 above 1920x1080.
pub fn matrix(width: u32, height: u32) -> (f32, f32) {
    if width > 1920 || height > 1080 {
        (0.2627, 0.0593)
    } else if width > 720 || height > 576 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    }
}

/// A received picture, as the runtime delivered it.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: usize,
    /// Rows of `stride` bytes (for UYVA followed by `width` alpha bytes per row).
    pub data: Vec<u8>,
    /// One field of an interlaced picture: shown with each line twice.
    pub field: bool,
}

impl Picture {
    /// The texture's size this picture makes.
    pub fn size(&self) -> (u32, u32) {
        (self.width, if self.field { self.height * 2 } else { self.height })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct UnpackKey {
    mode: u32,
    alpha: Alpha,
    /// BT.601, BT.709, BT.2020 as 0, 1, 2.
    matrix: u32,
    field: bool,
}

fn matrix_index(width: u32, height: u32) -> u32 {
    match matrix(width, height) {
        (kr, _) if kr > 0.29 => 0,
        (kr, _) if kr < 0.22 => 1,
        _ => 2,
    }
}

const MATRICES: [(f32, f32); 3] = [(0.299, 0.114), (0.2126, 0.0722), (0.2627, 0.0593)];

/// Uploads received pictures and converts them into penumbra's textures. One per receiving node.
#[derive(Default)]
pub struct Unpacker {
    layout: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout, wgpu::ShaderModule)>,
    pipelines: HashMap<UnpackKey, wgpu::RenderPipeline>,
    /// The bytes as a texture, kept while the size and format stay.
    color: Option<wgpu::Texture>,
    alpha: Option<wgpu::Texture>,
}

fn upload_texture(gpu: &Gpu, slot: &mut Option<wgpu::Texture>, w: u32, h: u32, format: wgpu::TextureFormat) {
    let fits = slot.as_ref().is_some_and(|t| (t.width(), t.height(), t.format()) == (w, h, format));
    if !fits {
        *slot = Some(gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("ndi received"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
    }
}

fn write(gpu: &Gpu, texture: &wgpu::Texture, data: &[u8], stride: usize) -> Result<(), String> {
    let row = u32::try_from(stride).map_err(|_| "a row too long".to_string())?;
    gpu.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(texture.height()),
        },
        texture.size(),
    );
    Ok(())
}

impl Unpacker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Uploads `picture` and converts it into `target` (of [`Picture::size`]).
    pub fn convert(
        &mut self,
        gpu: &Gpu,
        picture: &Picture,
        alpha: Alpha,
        target: &Tex,
    ) -> Result<(), String> {
        let (w, h) = (picture.width, picture.height);
        if target.size() != picture.size() {
            return Err("the target does not fit the picture".into());
        }
        let mode = match picture.fourcc {
            fourcc::UYVY => 0,
            fourcc::UYVA => 1,
            fourcc::BGRA => 2,
            fourcc::BGRX => 3,
            fourcc::RGBA => 4,
            fourcc::RGBX => 5,
            other => {
                return Err(format!("NDI delivered {}, which penumbra does not take", fourcc::name(other)));
            }
        };
        if mode <= 1 && !w.is_multiple_of(2) {
            return Err(format!("a {w}x{h} UYVY picture: the width must be even"));
        }
        let rows = picture.stride * h as usize;
        let need = rows + if mode == 1 { (w * h) as usize } else { 0 };
        if picture.data.len() < need {
            return Err("a received picture shorter than its size".into());
        }
        let ((), error) = gpu.scoped(|| {
            let color_width = if mode <= 1 { w / 2 } else { w };
            upload_texture(gpu, &mut self.color, color_width, h, wgpu::TextureFormat::Rgba8Unorm);
            let (aw, ah) = if mode == 1 { (w, h) } else { (1, 1) };
            upload_texture(gpu, &mut self.alpha, aw, ah, wgpu::TextureFormat::R8Unorm);
        });
        if let Some(e) = error {
            return Err(format!("cannot make the upload textures: {e}"));
        }
        let (Some(color), Some(alpha_tex)) = (&self.color, &self.alpha) else {
            return Err("the upload textures are missing".into());
        };
        let key = UnpackKey { mode, alpha, matrix: matrix_index(w, h), field: picture.field };
        let device = gpu.device();
        if self.layout.is_none() {
            let (layout, error) = gpu.scoped(|| {
                let entry = |binding| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                };
                let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("ndi unpack"),
                    entries: &[entry(0), entry(1)],
                });
                let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("ndi unpack"),
                    bind_group_layouts: &[Some(&group)],
                    immediate_size: 0,
                });
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("ndi unpack.wgsl"),
                    source: wgpu::ShaderSource::Wgsl(UNPACK.into()),
                });
                (group, pipeline_layout, module)
            });
            if let Some(e) = error {
                return Err(format!("cannot build the NDI conversion: {e}"));
            }
            self.layout = Some(layout);
        }
        let Some((group_layout, pipeline_layout, module)) = &self.layout else {
            return Err("the NDI conversion is missing".into());
        };
        if let Entry::Vacant(slot) = self.pipelines.entry(key) {
            let (kr, kb) = MATRICES.get(key.matrix as usize).copied().unwrap_or(MATRICES[1]);
            let constants = [
                ("MODE", f64::from(key.mode)),
                ("ALPHA", alpha.shader_constant()),
                ("KR", f64::from(kr)),
                ("KB", f64::from(kb)),
                ("FIELD", if key.field { 1.0 } else { 0.0 }),
            ];
            let options = wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() };
            let (pipeline, error) = gpu.scoped(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("ndi unpack"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some("vs"),
                        compilation_options: options.clone(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs"),
                        compilation_options: options.clone(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: target.texture().format(),
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            });
            if let Some(e) = error {
                return Err(format!("cannot build the NDI conversion: {e}"));
            }
            slot.insert(pipeline);
        }
        let Some(pipeline) = self.pipelines.get(&key) else {
            return Err("the NDI conversion is missing".into());
        };
        let (result, error) = gpu.scoped(|| -> Result<(), String> {
            let data = picture.data.get(..rows).ok_or("a received picture shorter than its size")?;
            write(gpu, color, data, picture.stride)?;
            if mode == 1 {
                let plane = picture.data.get(rows..need).ok_or("a received picture shorter than its size")?;
                write(gpu, alpha_tex, plane, w as usize)?;
            }
            let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
            let alpha_view = alpha_tex.create_view(&wgpu::TextureViewDescriptor::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ndi unpack"),
                layout: group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&alpha_view),
                    },
                ],
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ndi unpack") });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ndi unpack"),
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
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            }
            gpu.submit([encoder.finish()]);
            Ok(())
        });
        if let Some(e) = error {
            return Err(format!("converting the NDI picture failed: {e}"));
        }
        result
    }
}

/// The send pipeline, compiled once per alpha mode and matrix.
#[derive(Default)]
struct PackPipelines {
    layout: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout, wgpu::ShaderModule)>,
    pipelines: HashMap<(Alpha, u32), wgpu::ComputePipeline>,
}

impl PackPipelines {
    fn get(
        &mut self,
        gpu: &Gpu,
        alpha: Alpha,
        matrix: u32,
    ) -> Result<(&wgpu::ComputePipeline, &wgpu::BindGroupLayout), String> {
        let device = gpu.device();
        if self.layout.is_none() {
            let (layout, error) = gpu.scoped(|| {
                let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty,
                    count: None,
                };
                let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("ndi pack"),
                    entries: &[
                        entry(
                            0,
                            wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                        ),
                        entry(
                            1,
                            wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                        ),
                        entry(
                            2,
                            wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: false },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                        ),
                    ],
                });
                let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("ndi pack"),
                    bind_group_layouts: &[Some(&group)],
                    immediate_size: 0,
                });
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("ndi pack.wgsl"),
                    source: wgpu::ShaderSource::Wgsl(PACK.into()),
                });
                (group, pipeline_layout, module)
            });
            if let Some(e) = error {
                return Err(format!("cannot build the NDI conversion: {e}"));
            }
            self.layout = Some(layout);
        }
        let Some((group, pipeline_layout, module)) = &self.layout else {
            return Err("the NDI conversion is missing".into());
        };
        if let Entry::Vacant(slot) = self.pipelines.entry((alpha, matrix)) {
            let (kr, kb) = MATRICES.get(matrix as usize).copied().unwrap_or(MATRICES[1]);
            let constants =
                [("ALPHA", alpha.shader_constant()), ("KR", f64::from(kr)), ("KB", f64::from(kb))];
            let (pipeline, error) = gpu.scoped(|| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("ndi pack"),
                    layout: Some(pipeline_layout),
                    module,
                    entry_point: Some("main"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    cache: None,
                })
            });
            if let Some(e) = error {
                return Err(format!("cannot build the NDI conversion: {e}"));
            }
            slot.insert(pipeline);
        }
        let pipeline = self.pipelines.get(&(alpha, matrix)).ok_or("the NDI conversion is missing")?;
        Ok((pipeline, group))
    }
}

const WAITING: u8 = 0;
const MAPPED: u8 = 1;
const FAILED: u8 = 2;
/// Workgroup size of `pack.wgsl`.
const GROUP: u32 = 64;
/// Most workgroups per row of the dispatch grid.
const GRID_ROW: u32 = 32768;

/// Converts textures to the bytes NDI sends and reads them back through a ring of at most
/// `max` buffers, never waiting for the GPU. One per sending node.
pub struct Packer {
    max: usize,
    in_flight: Arc<AtomicUsize>,
    free: Vec<wgpu::Buffer>,
    back: Sender<wgpu::Buffer>,
    returned: Receiver<wgpu::Buffer>,
    storage: Option<wgpu::Buffer>,
    pipelines: PackPipelines,
}

impl Packer {
    pub fn new(max: usize) -> Self {
        let (back, returned) = channel();
        Self {
            max: max.max(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
            free: Vec::new(),
            back,
            returned,
            storage: None,
            pipelines: PackPipelines::default(),
        }
    }

    /// Frames read back and not yet dropped.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Converts `tex` to UYVY (UYVA unless `alpha` is opaque) and starts reading it back.
    /// `Ok(None)` when every buffer of the ring is still on its way: the frame is to be left out.
    pub fn grab(&mut self, gpu: &Gpu, tex: &Tex, alpha: Alpha) -> Result<Option<Packed>, String> {
        let (tw, th) = tex.size();
        let width = tw & !1;
        let height = th;
        if width < 2 || height < 1 {
            return Err(format!("a {tw}x{th} picture is too small to send"));
        }
        let with_alpha = alpha != Alpha::Opaque;
        let color_words = width / 2 * height;
        let alpha_words = if with_alpha { (width * height).div_ceil(4) } else { 0 };
        let total = color_words + alpha_words;
        let size = u64::from(total) * 4;
        while let Ok(b) = self.returned.try_recv() {
            self.free.push(b);
        }
        self.free.retain(|b| b.size() == size);
        if self.in_flight() >= self.max {
            return Ok(None);
        }
        let groups = total.div_ceil(GROUP);
        let (gx, gy) = (groups.min(GRID_ROW), groups.div_ceil(GRID_ROW));
        let words: [u32; 8] = [width, height, color_words, total, gx * GROUP, 0, 0, 0];
        let uniform: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let (pipeline, group_layout) = self.pipelines.get(gpu, alpha, matrix_index(width, height))?;
        let device = gpu.device();
        let reuse = self.storage.as_ref().is_some_and(|s| s.size() == size);
        let free = self.free.pop();
        let previous = self.storage.take().filter(|_| reuse);
        let ((storage, staging), error) = gpu.scoped(|| {
            let storage = previous.unwrap_or_else(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ndi packed"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                })
            });
            let staging = free.unwrap_or_else(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ndi readback"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            });
            let params = gpu.uniform_buffer("ndi pack params", &uniform);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ndi pack"),
                layout: group_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(tex.view()),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: storage.as_entire_binding() },
                ],
            });
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ndi pack") });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ndi pack"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
            }
            encoder.copy_buffer_to_buffer(&storage, 0, &staging, 0, size);
            gpu.submit([encoder.finish()]);
            (storage, staging)
        });
        self.storage = Some(storage);
        if let Some(e) = error {
            return Err(format!("converting for NDI failed: {e}"));
        }
        let state = Arc::new(AtomicU8::new(WAITING));
        let mapped = state.clone();
        staging.map_async(wgpu::MapMode::Read, .., move |r| {
            mapped.store(if r.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
        });
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        Ok(Some(Packed {
            width,
            height,
            fourcc: if with_alpha { fourcc::UYVA } else { fourcc::UYVY },
            stride: width as usize * 2,
            buffer: Some(staging),
            state,
            device: device.clone(),
            back: Mutex::new(self.back.clone()),
            in_flight: self.in_flight.clone(),
        }))
    }
}

/// One frame's bytes on their way back from the GPU. Wait for them on any thread but the cook
/// thread; dropping it returns the buffer to its [`Packer`].
pub struct Packed {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    /// Bytes per UYVY row.
    pub stride: usize,
    buffer: Option<wgpu::Buffer>,
    state: Arc<AtomicU8>,
    device: wgpu::Device,
    /// In a mutex only to make the whole `Sync` (a sender is `Send` but not `Sync`).
    back: Mutex<Sender<wgpu::Buffer>>,
    in_flight: Arc<AtomicUsize>,
}

impl Packed {
    /// Waits up to `timeout` for the bytes, polling the device without blocking it for others.
    pub fn wait(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.state.load(Ordering::Acquire) {
                MAPPED => return Ok(()),
                FAILED => return Err("the GPU could not hand the frame back".into()),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err("the GPU did not hand the frame back in time".into());
            }
            self.device.poll(wgpu::PollType::Poll).map_err(|e| format!("waiting for the GPU: {e}"))?;
            if self.state.load(Ordering::Acquire) == WAITING {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Hands the bytes to `f` once mapped ([`Packed::wait`] first).
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Result<R, String> {
        if self.state.load(Ordering::Acquire) != MAPPED {
            return Err("the frame is not back yet".into());
        }
        let buffer = self.buffer.as_ref().ok_or("the frame is gone")?;
        let view = buffer.get_mapped_range(..).map_err(|e| format!("reading the frame: {e}"))?;
        Ok(f(&view))
    }
}

impl Drop for Packed {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        if let Some(buffer) = self.buffer.take()
            && self.state.load(Ordering::Acquire) == MAPPED
        {
            buffer.unmap();
            // A closed channel: the packer is gone, the buffer goes with this frame.
            lock(&self.back).send(buffer).ok();
        }
    }
}
