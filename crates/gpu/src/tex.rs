//! Textures as they flow between nodes.

use std::sync::Arc;

use penumbra_core::Error;

use crate::{FORMAT, Gpu};

struct Inner {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// A texture in [`FORMAT`]: linear light, premultiplied alpha, origin top left. Cloning is an
/// `Arc` clone. This is the payload of `tex` data in the graph.
#[derive(Clone)]
pub struct Tex(Arc<Inner>);

impl Tex {
    /// A new texture, contents undefined until rendered to. Sizes are checked against the
    /// device, so a hostile resolution is an error, not a wgpu panic.
    pub fn new(gpu: &Gpu, width: u32, height: u32, label: &str) -> Result<Self, Error> {
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
                format: FORMAT,
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
        Ok(Self(Arc::new(Inner { texture, view, width, height })))
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

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        u64::from(self.0.width) * u64::from(self.0.height) * 8
    }

    /// Whether both are the same texture.
    pub fn same(&self, other: &Tex) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Held by someone besides the one asking (downstream data, the UI, a snapshot).
    fn shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
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
    fn hostile_sizes_are_errors() {
        let Some(gpu) = gpu() else { return };
        assert!(Tex::new(&gpu, 0, 4, "zero").is_err());
        assert!(Tex::new(&gpu, 1 << 20, 4, "huge").is_err());
    }
}
