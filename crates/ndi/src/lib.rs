//! NDI®: pictures over the network to and from other programs (OBS, vMix, Resolume,
//! TouchDesigner, NDI Tools, another penumbra). NDI® is a registered trademark of Vizrt NDI AB;
//! see <https://ndi.video/>.
//!
//! penumbra does not ship NDI: the runtime is installed with NDI Tools or the NDI Runtime and
//! loaded when first needed ([`ffi`]). Without it every sender and receiver fails with a
//! message saying where to get it, and nothing else is affected.
//!
//! - [`Sender`]: a source on the network; the conversion runs on the GPU ([`convert::Packer`]),
//!   the sending on a thread of its own.
//! - [`Receiver`]: one source received on a thread of its own, its newest picture and its
//!   audio held for the node; [`convert::Unpacker`] turns the picture into a texture.
//! - [`sources`]: what the network offers, for menus and `ndi.sources`.
//!
//! Colour as with Spout: penumbra's textures are linear light with premultiplied alpha, NDI
//! carries sRGB-encoded Y'CbCr (video range, BT.601 / 709 / 2020 by picture size) and straight
//! alpha; [`penumbra_gpu::Alpha`] says how transparency travels.

pub mod convert;
mod directory;
pub mod ffi;
mod receiver;
mod sender;

pub use convert::{Picture, Unpacker};
pub use directory::{find, settling};
pub use ffi::{Bandwidth, RUNTIME_URL, Source};
pub use receiver::{Audio, MAX_AUDIO_SECONDS, Receiver, Status};
pub use sender::{Sender, Sent};

/// NDI's site, to be linked wherever NDI is chosen (its licence asks for it).
pub const SITE: &str = "https://ndi.video/";

/// The sources on the network now, sorted by name; the first call starts looking, so it may
/// come back empty while [`settling`]. An error when the runtime is missing.
pub fn sources() -> Result<Vec<Source>, String> {
    directory::sources()
}

/// The runtime's version, or why it cannot be used.
pub fn runtime() -> Result<String, String> {
    ffi::runtime().map(|r| r.version().to_string())
}

#[cfg(test)]
mod tests;
