//! Receiving another program's sender.
//!
//! Per frame: the sender's description says which shared texture is current; a new handle,
//! size or format opens it again. If the sender counts frames and the count has not moved,
//! nothing is copied. Otherwise one D3D11 copy, under the sender's lock, moves the shared
//! texture into a wgpu texture of the same format on wgpu's queue, and a pass converts that into
//! a `tex` texture.

use penumbra_gpu::{Gpu, TexRing};

use crate::convert::{Alpha, Converter, Direction};
use crate::d3d::{Interop, NamedMutex, Semaphore, Shared, Side, Wrapped};
use crate::directory;
use crate::protocol::{Info, format_name, receive_format};
use crate::{Connected, Received};

/// How long a receive waits for the sender to finish writing a frame.
const ACCESS_WAIT_MS: u32 = 4;

struct Connection {
    name: String,
    info: Info,
    // Dropped before the texture it wraps (field order).
    wrapped: Wrapped,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    shared: Shared,
    no_alpha: bool,
    access: NamedMutex,
    frames: Option<Semaphore>,
    /// The sender's frame count at the last copy.
    copied: Option<i32>,
}

pub struct Receiver {
    interop: Interop,
    connection: Option<Connection>,
    converter: Converter,
    ring: TexRing,
    frames: u64,
}

impl Receiver {
    pub fn new(gpu: &Gpu) -> Result<Self, String> {
        let interop = Interop::new(gpu.device(), gpu.queue())?;
        Ok(Self { interop, connection: None, converter: Converter::new(), ring: TexRing::new(), frames: 0 })
    }

    pub fn connected(&self) -> Option<Connected> {
        self.connection.as_ref().map(|c| Connected {
            name: c.name.clone(),
            width: c.shared.width,
            height: c.shared.height,
            format: format_name(c.info.format),
            program: c.info.program(),
            counts_frames: c.frames.is_some(),
        })
    }

    /// Frames received.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn disconnect(&mut self) {
        self.connection = None;
    }

    /// The next frame of sender `name` (empty: the active sender). `again` copies even when the
    /// sender's frame count has not moved (a changed param wants a new conversion).
    pub fn receive(&mut self, gpu: &Gpu, name: &str, alpha: Alpha, again: bool) -> Result<Received, String> {
        let name = if name.is_empty() {
            directory::default_sender().ok_or("no Spout sender is running")?
        } else {
            name.to_string()
        };
        let Some(info) = directory::info(&name) else {
            self.connection = None;
            return Err(format!("no Spout sender named `{name}`"));
        };
        if self.connection.as_ref().is_none_or(|c| c.name != name || c.info != info) {
            self.connection = None;
            self.connection = Some(self.connect(gpu, name, info)?);
        }
        let Some(c) = self.connection.as_mut() else { return Err("not connected".into()) };
        let count = c.frames.as_ref().and_then(Semaphore::count);
        if !again && count.is_some() && count == c.copied {
            return Ok(Received::Unchanged);
        }
        // The target into the copy-destination state on wgpu's side before D3D11 copies.
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("spout receive") });
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &c.target,
                selector: None,
                state: wgpu::TextureUses::COPY_DST,
            }),
        );
        gpu.submit([encoder.finish()]);
        {
            let keyed = c.shared.lock_keyed(ACCESS_WAIT_MS);
            let named = if keyed.is_none() { c.access.lock(ACCESS_WAIT_MS) } else { None };
            let held = match &keyed {
                Some(k) => k.is_some(),
                None => named.is_some(),
            };
            if !held {
                return Ok(Received::Unchanged);
            }
            self.interop.copy(c.wrapped.resource(), c.shared.resource(), &c.wrapped);
        }
        c.copied = count;
        let (w, h) = (c.shared.width, c.shared.height);
        let out = self.ring.next(gpu, w, h, "spout receive").map_err(|e| e.0)?;
        let alpha = if c.no_alpha { Alpha::Opaque } else { alpha };
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("spout receive") });
        self.converter.encode(gpu, &mut encoder, &c.view, out.texture(), Direction::FromSpout, alpha)?;
        let ((), error) = gpu.scoped(|| {
            gpu.submit([encoder.finish()]);
        });
        if let Some(e) = error {
            return Err(format!("cannot convert the Spout picture: {e}"));
        }
        self.frames += 1;
        Ok(Received::Frame(out))
    }

    fn connect(&self, gpu: &Gpu, name: String, info: Info) -> Result<Connection, String> {
        let shared = self.interop.open_shared(info.handle)?;
        let (format, no_alpha) = receive_format(shared.format).ok_or_else(|| {
            format!("`{name}` sends {}, which penumbra does not take", format_name(shared.format))
        })?;
        let (w, h) = (shared.width, shared.height);
        let max = gpu.max_texture_size();
        if w == 0 || h == 0 || w > max || h > max {
            return Err(format!("`{name}` sends {w}x{h}, outside 1 to {max} on each side"));
        }
        let (target, error) = gpu.scoped(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("spout receive"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        });
        if let Some(e) = error {
            return Err(format!("cannot create a {w}x{h} texture to receive into: {e}"));
        }
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        // wgpu clears a texture it never wrote before its first read, which would wipe the
        // first copy D3D11 makes into it; cleared once here, it counts as written.
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("spout receive") });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("spout receive clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        gpu.submit([encoder.finish()]);
        let wrapped = self.interop.wrap(&target, Side::Dest)?;
        let access = NamedMutex::open_or_create(&format!("{name}_SpoutAccessMutex"))?;
        let frames = Semaphore::open(&format!("{name}_Count_Semaphore"));
        Ok(Connection { name, info, wrapped, target, view, shared, no_alpha, access, frames, copied: None })
    }
}
