//! Spout: sharing textures with other programs on the same machine (TouchDesigner, Resolume,
//! OBS, Unity, ...) without copying them through the CPU. Windows only; elsewhere every sender
//! and receiver fails with a message and no senders are listed.
//!
//! Spout is a protocol, not a library here: penumbra speaks Spout 2's (the Spout2 SDK, BSD
//! licence) directly. A sender is a name in a shared list plus a D3D11 texture with a shared
//! handle; receivers open that handle on their own device and copy from it. penumbra renders
//! with wgpu on DirectX 12 and reaches D3D11 through D3D11On12 on the same queue
//! ([`d3d`]), so a frame costs one GPU copy and a conversion pass, and never a wait.
//!
//! - [`protocol`]: the bytes in shared memory (pure, tested everywhere).
//! - [`Sender`], [`Receiver`]: one per node; a [`Watch`] wakes a receiving node.
//! - [`senders`]: what is being sent now, for menus and `spout.senders`.
//!
//! Colour: penumbra's textures are linear light with premultiplied alpha; Spout programs
//! exchange display-ready, sRGB-encoded colour, so both directions convert, and [`Alpha`] says
//! how transparency travels.

pub mod convert;
pub mod protocol;

#[cfg(windows)]
mod d3d;
#[cfg(windows)]
mod directory;
#[cfg(not(windows))]
#[path = "none.rs"]
mod none;
#[cfg(windows)]
mod receiver;
#[cfg(windows)]
mod sender;
#[cfg(windows)]
mod watch;

pub use convert::Alpha;
#[cfg(not(windows))]
pub use none::{Receiver, Sender, Watch};
#[cfg(windows)]
pub use receiver::Receiver;
#[cfg(windows)]
pub use sender::Sender;
#[cfg(windows)]
pub use watch::Watch;

/// Whether Spout works on this system at all.
pub const SUPPORTED: bool = cfg!(windows);

/// The pixel format a sender shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    /// BGRA, 8 bits per channel: what every Spout program takes.
    #[default]
    Bgra8,
    /// RGBA, 16-bit float: values above 1 and finer steps, for receivers that take it.
    Rgba16Float,
}

impl Format {
    pub const NAMES: [&'static str; 2] = ["8bit", "16bit_float"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "8bit" => Self::Bgra8,
            "16bit_float" => Self::Rgba16Float,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bgra8 => "8bit",
            Self::Rgba16Float => "16bit_float",
        }
    }

    pub fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        }
    }

    /// The `DXGI_FORMAT` of the shared texture.
    pub fn dxgi(self) -> u32 {
        match self {
            Self::Bgra8 => protocol::dxgi::B8G8R8A8_UNORM,
            Self::Rgba16Float => protocol::dxgi::R16G16B16A16_FLOAT,
        }
    }
}

/// What one send did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Frame,
    /// A receiver held the texture: this frame was left out.
    Busy,
}

/// What a receive got.
pub enum Received {
    /// A new frame.
    Frame(penumbra_gpu::Tex),
    /// Nothing new since the last frame (the sender counts frames), or the sender held its
    /// texture: the last frame stands.
    Unchanged,
}

/// The sender a receiver is connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connected {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// The sender's pixel format, readable.
    pub format: String,
    /// The sending program's file name.
    pub program: String,
    /// Whether the sender counts frames (only then are repeated frames skipped).
    pub counts_frames: bool,
}

/// A sender running now, in this process or another.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Listing {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub program: String,
}

/// The senders running now, sorted by name.
pub fn senders() -> Vec<Listing> {
    #[cfg(windows)]
    {
        directory::senders()
            .into_iter()
            .map(|l| Listing {
                width: l.info.width,
                height: l.info.height,
                format: protocol::format_name(l.info.format),
                program: l.info.program(),
                name: l.name,
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// The sender a receiver with no name connects to, if any is running.
pub fn default_sender() -> Option<String> {
    #[cfg(windows)]
    {
        directory::default_sender()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
#[cfg(windows)]
mod tests;
