//! Node output textures shown by egui, on the same device the cook thread renders with.
//!
//! A [`Tex`] is registered with egui-wgpu as a native texture: no copy, egui samples the node's
//! Rgba16Float texture directly. Its values are linear and premultiplied, which is what egui
//! expects from a texture (its own are sRGB formats that sample as linear), so colours come out
//! right; values above 1 clip on screen. Each entry holds its `Tex`, so the node's texture ring
//! does not render into a texture egui is still drawing.

use std::collections::HashMap;

use egui::TextureId;
use egui_wgpu::RenderState;
use penumbra_core::NodeId;
use penumbra_gpu::Tex;

/// One node output.
pub type Key = (NodeId, u32);

struct Entry {
    tex: Tex,
    id: TextureId,
    used: bool,
}

#[derive(Default)]
pub struct Textures {
    render: Option<RenderState>,
    entries: HashMap<Key, Entry>,
}

impl Textures {
    pub fn new(render: Option<RenderState>) -> Self {
        Self { render, entries: HashMap::new() }
    }

    /// The egui texture showing `tex` for `key`, registering or updating it as needed. `None`
    /// without a wgpu renderer.
    pub fn show(&mut self, key: Key, tex: &Tex) -> Option<TextureId> {
        let render = self.render.as_ref()?;
        if let Some(e) = self.entries.get_mut(&key) {
            e.used = true;
            if !e.tex.same(tex) {
                render.renderer.write().update_egui_texture_from_wgpu_texture(
                    &render.device,
                    tex.view(),
                    wgpu::FilterMode::Linear,
                    e.id,
                );
                e.tex = tex.clone();
            }
            return Some(e.id);
        }
        let id = render.renderer.write().register_native_texture(
            &render.device,
            tex.view(),
            wgpu::FilterMode::Linear,
        );
        self.entries.insert(key, Entry { tex: tex.clone(), id, used: true });
        Some(id)
    }

    /// Frees what was not shown since the last call. Called once per frame, after drawing.
    pub fn end_frame(&mut self) {
        let Some(render) = self.render.as_ref() else { return };
        let mut stale = Vec::new();
        for (key, e) in &mut self.entries {
            if !e.used {
                stale.push(*key);
            }
            e.used = false;
        }
        if stale.is_empty() {
            return;
        }
        let mut renderer = render.renderer.write();
        for key in stale {
            if let Some(e) = self.entries.remove(&key) {
                // Only the bind group goes; the texture belongs to the node.
                renderer.free_texture(&e.id);
            }
        }
    }
}
