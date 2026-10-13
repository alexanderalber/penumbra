//! Pictures of geometry for the editor's thumbnails and the viewer. A geo node used to render
//! one on every cook; with geometry that moves every frame (instances from a script, an animated
//! transform) that was several renders per frame that nobody looked at. Now the cook thread makes
//! them only for the geo outputs it publishes, and only when the geometry changed: every pass for
//! a viewer, at most every few passes for a thumbnail. In between a thumbnail keeps the last
//! picture; the next pass after the interval catches up, so a picture is never left behind for
//! good when the geometry stops changing.

use std::collections::HashMap;

use penumbra_core::NodeId;
use penumbra_gpu::render::Renderer;
use penumbra_gpu::{Geo, Gpu, Tex};
use penumbra_graph::{Data, DataKind};
use penumbra_ops::geo::{Preview, preview};

struct Made {
    /// [`Data::address`] of the geometry the picture shows.
    data: usize,
    tex: Tex,
    pass: u64,
}

/// One per shown geometry output: a renderer's buffers are rewritten by its next render before
/// the GPU has drawn the last, so pictures made in one pass cannot share one.
#[derive(Default)]
struct Shown {
    renderer: Renderer,
    made: Option<Made>,
}

#[derive(Default)]
pub struct GeoPictures {
    shown: HashMap<(NodeId, usize), Shown>,
}

impl GeoPictures {
    /// Puts a picture on every geometry among `outputs` (of node `id`) that has none. A new one
    /// is made when the geometry changed and the last is at least `every` passes old (1: always).
    pub fn attach(&mut self, gpu: &Gpu, id: NodeId, outputs: &mut [Data], pass: u64, every: u64) {
        for (k, data) in outputs.iter_mut().enumerate() {
            let Some(geo) = data.get::<Geo>() else { continue };
            if geo.preview.is_some() {
                continue;
            }
            let address = data.address();
            let shown = self.shown.entry((id, k)).or_default();
            let made = shown.made.as_ref();
            let fresh = made.is_some_and(|m| m.data == address);
            let wait = made.is_some_and(|m| pass.saturating_sub(m.pass) < every.max(1));
            let tex = match made {
                Some(m) if fresh || wait => m.tex.clone(),
                _ => match preview(gpu, &mut shown.renderer, geo, Preview::Geometry) {
                    Ok(tex) => {
                        shown.made = Some(Made { data: address, tex: tex.clone(), pass });
                        tex
                    }
                    Err(e) => {
                        log::warn!("geo picture: {e}");
                        continue;
                    }
                },
            };
            let mut shown = geo.clone();
            shown.preview = Some(tex);
            *data = Data::new(DataKind::Geo, shown);
        }
    }

    /// Forgets the pictures of nodes `keep` says are gone.
    pub fn prune(&mut self, keep: impl Fn(NodeId) -> bool) {
        self.shown.retain(|(id, _), _| keep(*id));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use penumbra_gpu::geo::Model;

    use super::*;

    #[test]
    fn pictures_follow_the_geometry_at_most_every_few_passes() {
        let gpu = match Gpu::headless(penumbra_gpu::AdapterChoice::Software) {
            Ok(g) => g,
            Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
            Err(_) => return,
        };
        let cube = penumbra_mesh::prim::cuboid([1.0; 3]).unwrap();
        let model = Arc::new(Model::upload(&gpu, cube, "cube").unwrap().0);
        let geo = |at: f64| {
            let mut g = Geo::single(model.clone());
            if let Some(item) = g.items.first_mut() {
                item.world = penumbra_ops::geo::trs([at, 0.0, 0.0], [0.0; 3], [1.0; 3]);
            }
            Data::new(DataKind::Geo, g)
        };
        let picture = |out: &[Data]| out.first().and_then(|d| d.get::<Geo>()?.preview.clone()).unwrap();
        let mut pictures = GeoPictures::default();
        let id = NodeId(1);
        let first = geo(0.0);
        let mut out = vec![first.clone()];
        pictures.attach(&gpu, id, &mut out, 10, 6);
        let a = picture(&out);
        // The same geometry again: the same picture, whenever.
        let mut out = vec![first.clone()];
        pictures.attach(&gpu, id, &mut out, 12, 6);
        assert!(picture(&out).same(&a));
        // Moved within the interval of the last picture (made at 10): that one still; after it,
        // a new one, though nothing moved in between.
        let moved = geo(1.0);
        let mut out = vec![moved.clone()];
        pictures.attach(&gpu, id, &mut out, 14, 6);
        assert!(picture(&out).same(&a));
        let mut out = vec![moved.clone()];
        pictures.attach(&gpu, id, &mut out, 16, 6);
        let b = picture(&out);
        assert!(!b.same(&a));
        // A viewer (every pass) follows at once.
        let mut out = vec![geo(2.0)];
        pictures.attach(&gpu, id, &mut out, 27, 1);
        assert!(!picture(&out).same(&b));
        pictures.prune(|_| false);
        assert!(pictures.shown.is_empty());
    }
}
