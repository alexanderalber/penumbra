//! `tex.mapper`: maps a region of its input (the surface) onto one projector's image, as
//! TouchDesigner's Stoner does: fit, corner offsets, mesh warp, soft edges where mappers of the
//! same surface overlap, an alignment grid. Its output is the projector's picture at the
//! projector's resolution; an `out.projector` shows it, possibly after a colour correction of that
//! projector alone.
//!
//! Mappers fed by the same output are one surface. Each tells the others where its region lies
//! through [`Mappers`], a resource the engine provides and prunes; a mapper whose region changes
//! wakes the others on its surface, so their soft edges follow the next frame.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_core::{Error, NodeId, ParamSpec, Value, lock};
use penumbra_gpu::projection::Overlay;
use penumbra_gpu::{Projection, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, Density, FnNodeType, Network, NodeDef, NodeDesc,
    NodeTypes, Operator, Port, Waker,
};

use super::{gpu, sized, waiting};
use crate::mapping::{Mapping, Mesh, Setup};
use crate::out::{Fit, item_for};

pub const MAPPER: &str = "tex.mapper";
/// The four corner params, in the order of [`Setup::corners`].
pub const CORNERS: [&str; 4] = ["corner_tl", "corner_tr", "corner_br", "corner_bl"];
/// Pattern colours of the mappers on one surface, in node order: neighbours are complementary,
/// so grid lines that meet add up to white.
pub(crate) const TINTS: [[f32; 3]; 6] = [
    [1.0, 0.15, 0.15],
    [0.15, 1.0, 1.0],
    [1.0, 1.0, 0.15],
    [0.25, 0.25, 1.0],
    [0.15, 1.0, 0.15],
    [1.0, 0.15, 1.0],
];

fn desc() -> NodeDesc {
    let desc = sized(NodeDesc::new(
        MAPPER,
        "Maps a part of its input onto a projector: corners, mesh warp, soft edges with other mappers of the same input",
    ))
    .param(ParamSpec::vec4("region", [0.0, 0.0, 1.0, 1.0]).clamp(0.0, 1.0).doc(
        "The part of the input (the surface) shown: x, y, width, height in input sizes",
    ))
    .param(
        ParamSpec::choice("fit", &["fit", "fill", "stretch"])
            .doc("How the region sits in the picture before the corners move")
            .choice_doc("fit", "whole, aspect kept, with bars")
            .choice_doc("fill", "covering the picture, aspect kept, cropped")
            .choice_doc("stretch", "covering the picture, distorted to its aspect"),
    )
    .param(ParamSpec::bool("blend", true).doc(
        "Soft edges where other mappers of the same input overlap this one (off for stacked projectors)",
    ))
    .param(ParamSpec::bool("pattern", false).doc("An alignment grid of the input instead of the picture"));
    let mut desc = desc;
    for (name, doc) in CORNERS.iter().zip([
        "Top left corner, moved from its fitted place, in picture sizes (drag it in the viewer)",
        "Top right corner, moved from its fitted place, in picture sizes",
        "Bottom right corner, moved from its fitted place, in picture sizes",
        "Bottom left corner, moved from its fitted place, in picture sizes",
    ]) {
        desc = desc.param(ParamSpec::vec2(name, [0.0, 0.0]).range(-0.5, 0.5).group("warp").doc(doc));
    }
    desc.param(ParamSpec::text("warp", "").multiline().group("warp").doc(
        "Mesh warp: one line per grid row, `dx,dy` offsets in picture sizes; empty is none (edit it in the viewer)",
    ))
    .param(
        ParamSpec::float("blend_gamma", 1.0)
            .range(0.2, 3.0)
            .more()
            .doc("Exponent on the soft edge; above 1 darkens the overlap, below 1 brightens it"),
    )
    .input("in", DataKind::Tex)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(Mapper::default()) as Box<dyn Operator>))
}

/// The mapping params, read through `param` (a node's stored values or the values it cooks
/// with).
pub fn setup<'a>(param: impl Fn(&str) -> Option<&'a Value>) -> Result<Setup, String> {
    let vec = |name: &str| param(name).and_then(Value::components).map(<[f64]>::to_vec).unwrap_or_default();
    let defaults = Setup::default();
    let region = match vec("region").as_slice() {
        &[x, y, w, h] => [x, y, w, h],
        _ => defaults.region,
    };
    let mut corners = defaults.corners;
    for (c, name) in corners.iter_mut().zip(CORNERS) {
        if let &[x, y] = vec(name).as_slice() {
            *c = [x, y];
        }
    }
    let text = |name: &str| param(name).and_then(Value::as_str).unwrap_or_default();
    Ok(Setup {
        fit: Fit::parse(text("fit")),
        region,
        corners,
        mesh: Mesh::parse(text("warp"))?,
        blend: param("blend").and_then(Value::as_bool).unwrap_or(true),
        blend_gamma: param("blend_gamma").and_then(Value::as_f64).unwrap_or(1.0),
        pattern: param("pattern").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// What one mapper tells the others (and the editor).
struct Entry {
    surface: Option<Port>,
    /// Its region after `fill` cropped it; `None` when it does not blend.
    region: Option<[f64; 4]>,
    mapping: Arc<Mapping>,
    waker: Waker,
}

/// The mappers that cooked, by node: who shares a surface with whom, and each one's mapping for
/// the editor's handles. Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct Mappers(Arc<Mutex<BTreeMap<NodeId, Entry>>>, Arc<Mutex<BTreeMap<NodeId, Overlay>>>);

impl Mappers {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<NodeId, Entry>> {
        lock(&self.0)
    }

    /// Shows the resolution overlay (`res.overlay`) on the mappers in `overlays` and on no
    /// other; wakes those whose overlay changed.
    pub fn set_overlays(&self, overlays: BTreeMap<NodeId, Overlay>) {
        let changed: Vec<NodeId> = {
            let mut current = lock(&self.1);
            let changed = current
                .keys()
                .chain(overlays.keys())
                .filter(|id| current.get(id) != overlays.get(id))
                .copied()
                .collect();
            *current = overlays;
            changed
        };
        let entries = self.lock();
        for id in changed {
            if let Some(e) = entries.get(&id) {
                e.waker.wake();
            }
        }
    }

    fn overlay(&self, id: NodeId) -> Option<Overlay> {
        lock(&self.1).get(&id).copied()
    }

    /// The mapping node `id` drew last.
    pub fn mapping(&self, id: NodeId) -> Option<Arc<Mapping>> {
        self.lock().get(&id).map(|e| e.mapping.clone())
    }

    /// Forgets mappers no longer in `net` (deleted, or another kind now) and wakes those that
    /// shared a surface with them. Called by the engine after every pass.
    pub fn prune(&self, net: &Network) {
        let mut entries = self.lock();
        let gone: Vec<(NodeId, Option<Port>)> = entries
            .iter()
            .filter(|(id, _)| net.node(**id).is_none_or(|n| n.kind != MAPPER))
            .map(|(id, e)| (*id, e.surface))
            .collect();
        for (id, surface) in gone {
            entries.remove(&id);
            wake_surface(&entries, surface, id);
        }
    }

    /// Records `id`'s entry and returns its peers' regions and its rank on the surface. When
    /// what others see of it changed, they are woken to blend anew.
    fn publish(&self, id: NodeId, entry: Entry) -> (Vec<[f64; 4]>, usize) {
        let mut entries = self.lock();
        let changed =
            entries.get(&id).is_none_or(|old| old.surface != entry.surface || old.region != entry.region);
        let old_surface = entries.get(&id).and_then(|e| e.surface);
        let surface = entry.surface;
        entries.insert(id, entry);
        if changed {
            wake_surface(&entries, surface, id);
            if old_surface != surface {
                wake_surface(&entries, old_surface, id);
            }
        }
        let same = |e: &Entry| surface.is_some() && e.surface == surface;
        let peers =
            entries.iter().filter(|(o, e)| **o != id && same(e)).filter_map(|(_, e)| e.region).collect();
        let rank = entries.range(..id).filter(|(_, e)| same(e)).count();
        (peers, rank)
    }
}

fn wake_surface(entries: &BTreeMap<NodeId, Entry>, surface: Option<Port>, except: NodeId) {
    if surface.is_none() {
        return;
    }
    for (id, e) in entries {
        if *id != except && e.surface == surface {
            e.waker.wake();
        }
    }
}

/// What a mapping depends on: the params, the picture size, the input size.
type MappingKey = (Setup, (u32, u32), (u32, u32));

#[derive(Default)]
struct Mapper {
    ring: TexRing,
    projection: Option<Projection>,
    /// The mapping for the last params, picture size and input size.
    cached: Option<(MappingKey, Arc<Mapping>)>,
}

impl Operator for Mapper {
    fn density(&self, _def: &NodeDef, _desc: &NodeDesc, _input: usize) -> Density {
        Density::Varying
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let setup = setup(|n| cx.param(n)).map_err(CookError::Failed)?;
        let src = cx.input_as::<penumbra_gpu::Tex>(0).ok_or_else(|| waiting(cx))?;
        let side = |name: &str| cx.int(name).map(|v| v.clamp(1, super::MAX_SIDE as i64) as u32);
        let size = (side("width")?, side("height")?);
        let tex_size = src.size();
        let key = (setup, size, tex_size);
        let mapping = match &self.cached {
            Some((k, m)) if *k == key => m.clone(),
            _ => {
                let m = Arc::new(Mapping::new(&key.0, size, tex_size).map_err(CookError::Failed)?);
                self.cached = Some((key, m.clone()));
                m
            }
        };
        let surface = cx.def().inputs.first().copied().flatten();
        let (peers, rank) = match cx.resource::<Mappers>() {
            Some(mappers) => mappers.publish(
                cx.node(),
                Entry {
                    surface,
                    region: mapping.blend.then_some(mapping.region),
                    mapping: mapping.clone(),
                    waker: cx.waker(),
                },
            ),
            None => (Vec::new(), 0),
        };
        let tint = TINTS.get(rank % TINTS.len()).copied().unwrap_or([1.0; 3]);
        let target = self.ring.next(gpu, size.0, size.1, MAPPER)?;
        let projection = self.projection.get_or_insert_with(|| Projection::new(gpu));
        let mut item = item_for(src, &mapping, &peers, tint, [0.0, 0.0, size.0 as f32, size.1 as f32]);
        item.look.overlay = cx.resource::<Mappers>().and_then(|m| m.overlay(cx.node()));
        projection.render(gpu, &target, &[item])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}
