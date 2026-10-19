//! Sending a texture under a name.
//!
//! Per frame: the input is converted into a staging texture of the Spout format on wgpu's queue
//! and left in the copy-source state; then, under the sender's access mutex, one D3D11 copy on
//! the same queue moves it into the shared texture, and the frame counter rises. Nothing waits
//! for the GPU. A new size or format makes a new shared texture, announced to receivers by its
//! new handle in the sender's description.

use penumbra_gpu::{Gpu, Tex};

use crate::convert::{Alpha, Converter, Direction};
use crate::d3d::{Interop, Shared, Side, Wrapped};
use crate::directory::Registration;
use crate::protocol::Info;
use crate::{Format, Sent};

/// How long a send waits for a receiver to finish copying the previous frame before it skips
/// this one. Receivers hold the lock for one copy command, well under this.
const ACCESS_WAIT_MS: u32 = 4;

struct Staging {
    // Dropped before the texture it wraps (field order).
    wrapped: Wrapped,
    texture: wgpu::Texture,
    shared: Shared,
    format: Format,
}

pub struct Sender {
    interop: Interop,
    registration: Registration,
    staging: Option<Staging>,
    converter: Converter,
    frames: u64,
    busy: u64,
}

fn description(shared: Option<&Shared>) -> Info {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let info = Info::default().with_description(&exe);
    match shared {
        Some(s) => Info { handle: s.handle, width: s.width, height: s.height, format: s.format, ..info },
        None => info,
    }
}

impl Sender {
    /// A sender called `name` (or `name_1`, ... if another program has it), listed at once,
    /// without a texture until the first [`Sender::send`].
    pub fn new(gpu: &Gpu, name: &str) -> Result<Self, String> {
        let interop = Interop::new(gpu.device(), gpu.queue())?;
        let registration = Registration::new(name, &description(None))?;
        Ok(Self { interop, registration, staging: None, converter: Converter::new(), frames: 0, busy: 0 })
    }

    /// The name receivers see.
    pub fn name(&self) -> &str {
        self.registration.name()
    }

    /// Frames sent and frames left out because a receiver held the texture.
    pub fn counts(&self) -> (u64, u64) {
        (self.frames, self.busy)
    }

    /// The size and format being shared, once something was sent.
    pub fn sharing(&self) -> Option<(u32, u32, Format)> {
        self.staging.as_ref().map(|s| (s.shared.width, s.shared.height, s.format))
    }

    /// Shares `tex` as the next frame.
    pub fn send(&mut self, gpu: &Gpu, tex: &Tex, format: Format, alpha: Alpha) -> Result<Sent, String> {
        let (w, h) = tex.size();
        if self.staging.as_ref().is_none_or(|s| (s.shared.width, s.shared.height, s.format) != (w, h, format))
        {
            // Released before the new one exists, so a size change does not hold two.
            self.staging = None;
            let staging = self.make_staging(gpu, w, h, format)?;
            self.registration.update(&description(Some(&staging.shared)))?;
            self.staging = Some(staging);
        }
        let Some(staging) = &self.staging else { return Err("no staging texture".into()) };
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("spout send") });
        self.converter.encode(gpu, &mut encoder, tex.view(), &staging.texture, Direction::ToSpout, alpha)?;
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &staging.texture,
                selector: None,
                state: wgpu::TextureUses::COPY_SRC,
            }),
        );
        let ((), error) = gpu.scoped(|| {
            gpu.submit([encoder.finish()]);
        });
        if let Some(e) = error {
            return Err(format!("cannot convert the picture for Spout: {e}"));
        }
        let Some(_guard) = self.registration.access().lock(ACCESS_WAIT_MS) else {
            self.busy += 1;
            return Ok(Sent::Busy);
        };
        self.interop.copy(staging.shared.resource(), staging.wrapped.resource(), &staging.wrapped);
        self.registration.frame_sent();
        self.frames += 1;
        Ok(Sent::Frame)
    }

    fn make_staging(&self, gpu: &Gpu, w: u32, h: u32, format: Format) -> Result<Staging, String> {
        let shared = self.interop.create_shared(w, h, format.dxgi())?;
        let (texture, error) = gpu.scoped(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("spout send"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.wgpu(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        if let Some(e) = error {
            return Err(format!("cannot create a {w}x{h} texture to send: {e}"));
        }
        let wrapped = self.interop.wrap(&texture, Side::Source)?;
        Ok(Staging { wrapped, texture, shared, format })
    }
}
