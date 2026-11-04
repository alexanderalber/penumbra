//! Textures as they flow between nodes.

use std::sync::{Arc, Weak};

use penumbra_core::Error;

use crate::{FLOAT_FORMAT, FORMAT, Gpu};

struct Inner {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// A texture in [`FORMAT`] (or [`FLOAT_FORMAT`], loaded from a file of 32-bit floats): linear
/// light, premultiplied alpha, origin top left. Cloning is an `Arc` clone. This is the payload of
/// `tex` data in the graph.
#[derive(Clone)]
pub struct Tex(Arc<Inner>);

impl Tex {
    /// A new texture, contents undefined until rendered to. Sizes are checked against the
    /// device, so a hostile resolution is an error, not a wgpu panic.
    pub fn new(gpu: &Gpu, width: u32, height: u32, label: &str) -> Result<Self, Error> {
        Self::with_format(gpu, width, height, FORMAT, label)
    }

    /// A new texture of [`FLOAT_FORMAT`] for pictures uploaded from the CPU (not rendered to).
    pub fn new_float(gpu: &Gpu, width: u32, height: u32, label: &str) -> Result<Self, Error> {
        Self::with_format(gpu, width, height, FLOAT_FORMAT, label)
    }

    fn with_format(
        gpu: &Gpu,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> Result<Self, Error> {
        let max = gpu.max_texture_size();
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Error::new(format!("resolution {width}x{height} must be 1 to {max} on each side")));
        }
        let (texture, error) = gpu.scoped(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot create a {width}x{height} texture: {e}")));
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self(Arc::new(Inner { texture, view, width, height, format })))
    }

    pub fn width(&self) -> u32 {
        self.0.width
    }

    pub fn height(&self) -> u32 {
        self.0.height
    }

    pub fn size(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.0.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.0.view
    }

    /// [`FORMAT`], or [`FLOAT_FORMAT`] for a picture of 32-bit floats.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.0.format
    }

    /// Bytes per pixel: 8, or 16 for [`FLOAT_FORMAT`].
    pub fn pixel_bytes(&self) -> u32 {
        if self.0.format == FLOAT_FORMAT { 16 } else { 8 }
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        u64::from(self.0.width) * u64::from(self.0.height) * u64::from(self.pixel_bytes())
    }

    /// Whether both are the same texture.
    pub fn same(&self, other: &Tex) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Names this texture without holding it.
    pub(crate) fn key(&self) -> TexKey {
        TexKey(Arc::downgrade(&self.0))
    }

    /// Held by someone besides the one asking (downstream data, the UI, a snapshot).
    fn shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
}

/// A texture named without holding it, for caches of things made from it.
pub(crate) struct TexKey(Weak<Inner>);

impl TexKey {
    /// Whether this names `tex`. The weak reference keeps the allocation, so no other texture
    /// can come to live at the same address.
    pub(crate) fn is(&self, tex: &Tex) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&tex.0))
    }

    /// Whether the texture still exists.
    pub(crate) fn alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// Bytes a texture takes on the GPU by its size, format, layers, samples and mip levels. The
/// driver's padding and alignment come on top.
pub fn texture_bytes(t: &wgpu::Texture) -> u64 {
    let format = t.format();
    let (bw, bh) = format.block_dimensions();
    let block = format.block_copy_size(None).map_or_else(
        || match format {
            wgpu::TextureFormat::Depth16Unorm => 2,
            wgpu::TextureFormat::Stencil8 => 1,
            wgpu::TextureFormat::Depth32FloatStencil8 => 8,
            // The other depth formats, and anything unknown, as four bytes a texel.
            _ => 4,
        },
        u64::from,
    );
    let (w, h) = (t.width().max(1), t.height().max(1));
    let levels: u64 = (0..t.mip_level_count().min(32))
        .map(|l| {
            u64::from((w >> l).max(1).div_ceil(bw.max(1))) * u64::from((h >> l).max(1).div_ceil(bh.max(1)))
        })
        .sum();
    let bytes = levels * block * u64::from(t.depth_or_array_layers()) * u64::from(t.sample_count());
    // Direct3D 12 places a multisampled texture on 4 MiB boundaries, so a small one takes 4 MiB:
    // the previews of geometry and materials draw with 4x antialiasing.
    if t.sample_count() > 1 { bytes.next_multiple_of(MSAA_ALIGNMENT) } else { bytes }
}

/// Placement alignment of multisampled textures on Direct3D 12.
const MSAA_ALIGNMENT: u64 = 4 << 20;

/// Bytes on the GPU of `textures`, each counted once however often it is named (a ring's
/// texture that is also kept as the last result).
pub fn unique_bytes<'a>(textures: impl IntoIterator<Item = &'a Tex>) -> u64 {
    let mut seen: Vec<&Tex> = Vec::new();
    for t in textures {
        if !seen.iter().any(|s| s.same(t)) {
            seen.push(t);
        }
    }
    seen.iter().map(|t| t.bytes()).sum()
}

/// [`texture_bytes`] of the texture behind a view.
pub fn view_bytes(v: &wgpu::TextureView) -> u64 {
    texture_bytes(v.texture())
}

/// Most textures a [`TexRing`] keeps.
const RING: usize = 3;

/// The output textures of one node, reused across cooks: a texture nobody else holds any more
/// is rendered into again instead of allocating a new one every frame. The previous result
/// usually is still held downstream until the new one replaces it, so two or three textures
/// circulate.
#[derive(Default)]
pub struct TexRing {
    textures: Vec<Tex>,
}

impl TexRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// A texture of this size to render into that nobody else is reading.
    pub fn next(&mut self, gpu: &Gpu, width: u32, height: u32, label: &str) -> Result<Tex, Error> {
        self.textures.retain(|t| t.size() == (width, height));
        if let Some(t) = self.textures.iter().find(|t| !t.shared()) {
            return Ok(t.clone());
        }
        let t = Tex::new(gpu, width, height, label)?;
        if self.textures.len() >= RING {
            // All held elsewhere: forget the oldest, its holders keep it alive.
            self.textures.remove(0);
        }
        self.textures.push(t.clone());
        Ok(t)
    }

    /// Forgets the textures nobody else holds, keeping the ones still read (usually the node's
    /// current output), so an idle node holds no spares; [`TexRing::next`] makes new ones when
    /// the node cooks again.
    pub fn trim(&mut self) {
        self.textures.retain(Tex::shared);
    }

    /// Bytes on the GPU of the textures the ring keeps, its current output among them.
    pub fn bytes(&self) -> u64 {
        self.textures.iter().map(Tex::bytes).sum()
    }

    /// The textures the ring keeps.
    pub fn iter(&self) -> impl Iterator<Item = &Tex> {
        self.textures.iter()
    }

    /// Textures the ring keeps now.
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    #[test]
    fn ring_reuses_what_nobody_holds() {
        let Some(gpu) = gpu() else { return };
        let mut ring = TexRing::new();
        let a = ring.next(&gpu, 8, 4, "a").unwrap();
        let b = ring.next(&gpu, 8, 4, "b").unwrap();
        assert!(!a.same(&b), "a is still held");
        drop(a);
        let c = ring.next(&gpu, 8, 4, "c").unwrap();
        assert!(!c.same(&b));
        let held: Vec<Tex> = (0..5).map(|_| ring.next(&gpu, 8, 4, "x").unwrap()).collect();
        assert!(ring.textures.len() <= RING);
        drop(held);
        let other = ring.next(&gpu, 4, 4, "resized").unwrap();
        assert_eq!(other.size(), (4, 4));
        assert_eq!(ring.textures.len(), 1);
    }

    #[test]
    fn trim_keeps_only_what_is_held() {
        let Some(gpu) = gpu() else { return };
        let mut ring = TexRing::new();
        let a = ring.next(&gpu, 8, 4, "a").unwrap();
        let b = ring.next(&gpu, 8, 4, "b").unwrap();
        drop(a);
        assert_eq!(ring.len(), 2);
        ring.trim();
        assert_eq!(ring.len(), 1, "the spare goes, the held one stays");
        let c = ring.next(&gpu, 8, 4, "c").unwrap();
        assert!(!c.same(&b));
        drop((b, c));
        ring.trim();
        assert!(ring.is_empty());
    }

    #[test]
    fn bytes_count_each_texture_once() {
        let Some(gpu) = gpu() else { return };
        let mut ring = TexRing::new();
        let a = ring.next(&gpu, 8, 4, "a").unwrap();
        let b = ring.next(&gpu, 8, 4, "b").unwrap();
        assert_eq!(a.bytes(), 8 * 4 * 8);
        assert_eq!(texture_bytes(a.texture()), a.bytes());
        assert_eq!(view_bytes(a.view()), a.bytes());
        assert_eq!(ring.bytes(), 2 * a.bytes());
        // The last result kept beside the ring is in it already.
        assert_eq!(unique_bytes(ring.iter().chain([&a, &a])), ring.bytes());
        drop((a, b));
        ring.trim();
        assert_eq!(ring.bytes(), 0);
        let mips = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("mips"),
            size: wgpu::Extent3d { width: 8, height: 4, depth_or_array_layers: 2 },
            mip_level_count: 4,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        // 8x4 + 4x2 + 2x1 + 1x1 texels of 4 bytes, two layers.
        assert_eq!(texture_bytes(&mips), (32 + 8 + 2 + 1) * 4 * 2);
    }

    #[test]
    fn hostile_sizes_are_errors() {
        let Some(gpu) = gpu() else { return };
        assert!(Tex::new(&gpu, 0, 4, "zero").is_err());
        assert!(Tex::new(&gpu, 1 << 20, 4, "huge").is_err());
    }
}
