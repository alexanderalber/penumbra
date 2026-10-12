//! `res.analyze` and `res.optimize` for 2D chains.
//!
//! Resolutions stay fixed; this measures them on demand. Each projector needs its viewport's
//! pixels. Walking upstream from it, every node passes the need on through its
//! [`Density`]: the same frame for a filter, scaled for a transform, transposed for a flop, and
//! for a mapper measured over its homography and mesh ([`Mapping::surface_demand`]). A node
//! fed by several consumers needs the most any of them asks for. The need of a node is a
//! resolution (across, down): the size its output would have to have so that one of its
//! pixels lands on about one projector pixel.
//!
//! Comparing that with the node's actual size gives its verdict. Nodes whose `width` and
//! `height` set their size ([`Cooker::sets_size`]) get a suggestion, which `res.optimize`
//! writes unless `keep_size` pins them; the others follow their input, so the suggestion goes
//! to the node they follow.
//!
//! A `tex.render` passes its need on to its texture input through the field its camera sees:
//! measured on the GPU per pixel ([`Renders`]), the same percentile as a mapper's. Until a render
//! node's field has arrived, the report lists it as `pending`.

use std::collections::{BTreeMap, BTreeSet};

use penumbra_core::{Name, NodeId, Unit, Value};
use penumbra_gpu::Tex;
use penumbra_gpu::projection::Overlay;
use penumbra_graph::{Cooker, DataKind, Density, Network, NodeDef, NodeTypes, Op};
use serde::Serialize;

use crate::out::place::place;
use crate::out::{BoardState, DEFAULT_RASTER, Fit, PROJECTOR, raster};
use crate::tex::KEEP_SIZE;
use crate::tex::mapper::{MAPPER, Mappers};
use crate::tex::render::{RENDER, Renders};

/// Below this many actual pixels per needed one a node is too coarse...
pub const COARSE: f64 = 0.9;
/// ...and above this many it is finer than anything shown (projection.wgsl and render.wgsl
/// colour the overlay with the same limits; the user's band, 2026-10-11).
pub const WASTED: f64 = 1.3;

/// Sizes `res.optimize` prefers for the longer side: numbers people recognise. Where the next
/// one would land above [`WASTED`] (they are up to a factor 2 apart), [`suggest`] takes a
/// multiple of 16 instead.
const STEPS: [u32; 26] = [
    16, 32, 64, 128, 256, 384, 512, 640, 768, 1024, 1280, 1536, 1920, 2048, 2560, 3072, 3840, 4096, 5120,
    6144, 7680, 8192, 10240, 12288, 15360, 16384,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Between [`COARSE`] and [`WASTED`].
    Ok,
    Coarse,
    Wasted,
    /// Not cooked yet, or no size.
    Unknown,
}

/// One node a projector shows, directly or through others.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeRes {
    #[serde(skip)]
    pub id: NodeId,
    pub path: String,
    pub kind: String,
    /// Its output's size; `None` before it cooked.
    pub size: Option<[u32; 2]>,
    /// What the projectors need of it, rounded up.
    pub needed: [u32; 2],
    /// Actual pixels per needed pixel along the worse axis.
    pub ratio: Option<f64>,
    pub verdict: Verdict,
    /// Its `width` and `height` set its size (otherwise it follows its input or a file).
    pub sets_size: bool,
    /// Pinned by `keep_size`: `res.optimize` leaves it alone.
    pub keep: bool,
    /// The size `res.optimize` would give it; only for nodes that set their size and are not
    /// about right.
    pub suggested: Option<[u32; 2]>,
}

/// One projector as the analysis saw it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectorRes {
    pub path: String,
    /// Its viewport in pixels.
    pub raster: [u32; 2],
    /// The display is unknown (headless, not connected): [`DEFAULT_RASTER`] was assumed.
    pub assumed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    /// Every node a projector shows, from the projectors upstream.
    pub nodes: Vec<NodeRes>,
    pub projectors: Vec<ProjectorRes>,
    /// Render nodes whose field (what their camera sees of the texture) has not arrived yet or
    /// is outdated: what they ask of their texture is missing or from before the last change.
    pub pending: Vec<String>,
    /// Per mapper and render node, what its overlay needs (`res.overlay`).
    #[serde(skip)]
    pub overlays: BTreeMap<NodeId, Overlay>,
}

impl Report {
    /// The edits `res.optimize` makes: the suggested sizes of the nodes in `only` (all when
    /// `None`) that set their size and are not pinned. Returns `(node, from, to)` per change.
    pub fn changes(&self, only: Option<&BTreeSet<NodeId>>) -> Vec<(&NodeRes, [u32; 2], [u32; 2])> {
        self.nodes
            .iter()
            .filter(|n| only.is_none_or(|o| o.contains(&n.id)))
            .filter(|n| n.sets_size && !n.keep)
            .filter_map(|n| Some((n, n.size?, n.suggested?)))
            .filter(|(_, from, to)| from != to)
            .collect()
    }

    /// [`Report::changes`] as one batch of edits.
    pub fn optimize_ops(&self, net: &Network, only: Option<&BTreeSet<NodeId>>) -> Vec<Op> {
        let mut ops = Vec::new();
        for (node, _, to) in self.changes(only) {
            let Some(path) = net.path_of(node.id) else { continue };
            for (name, v) in [("width", to[0]), ("height", to[1])] {
                if let Ok(param) = Name::new(name).and_then(|n| path.param(n)) {
                    ops.push(Op::Set { param, value: Value::Int(i64::from(v)) });
                }
            }
        }
        ops
    }
}

/// How a node's need reaches a node upstream.
#[derive(Clone, Copy)]
enum Edge {
    /// Through input `i`.
    Input(usize),
    /// Through a node reference param (`tex.select`): shown as it is.
    Reference,
}

/// What the analysis reads.
pub struct Scene<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    pub cooker: &'a Cooker,
    /// What the output side knows of the displays; `None` assumes [`DEFAULT_RASTER`].
    pub board: Option<&'a BoardState>,
    /// The device's largest texture side, the cap for suggestions.
    pub max_side: u32,
}

impl Scene<'_> {
    fn mappers(&self) -> Option<&Mappers> {
        self.cooker.resource::<Mappers>()
    }

    fn renders(&self) -> Option<&Renders> {
        self.cooker.resource::<Renders>()
    }

    fn kind(&self, id: NodeId) -> &str {
        self.net.node(id).map_or("", |n| n.kind.as_str())
    }

    /// The first texture output's size.
    fn size(&self, id: NodeId) -> Option<[u32; 2]> {
        let outputs = self.cooker.outputs(id)?;
        let (w, h) = outputs.iter().find_map(|d| d.get::<Tex>())?.size();
        Some([w, h])
    }

    /// The nodes `node` shows pixels of.
    fn upstream(&self, node: &NodeDef) -> Vec<(NodeId, Edge)> {
        let Some(desc) = self.types.desc(&node.kind) else { return Vec::new() };
        let mut out = Vec::new();
        for (i, port) in node.inputs.iter().enumerate() {
            let tex = desc.inputs.get(i).is_some_and(|s| s.kind == DataKind::Tex);
            if let (Some(port), true, false) = (port, tex, node.is_previous(i)) {
                out.push((port.node, Edge::Input(i)));
            }
        }
        for spec in desc.params.iter().filter(|s| s.node_ref) {
            let text = node.param(desc, &spec.name).and_then(Value::as_str).unwrap_or_default();
            if let Ok(id) = self.net.resolve_from(node.id, text)
                && !text.trim().is_empty()
            {
                out.push((id, Edge::Reference));
            }
        }
        out
    }

    /// What `node`, needing `need`, needs of the node behind `edge`.
    fn pass_on(&self, node: NodeId, edge: Edge, need: [f64; 2]) -> Option<[f64; 2]> {
        let Edge::Input(i) = edge else { return Some(need) };
        match self.cooker.density(node, i).unwrap_or(Density::SAME) {
            Density::Varying if self.kind(node) == RENDER => {
                // Not measured yet: no demand until it is (the report says `pending`).
                let (field, _) = self.renders()?.field(node)?;
                field.demand(need)
            }
            Density::Varying => match self.mappers().and_then(|m| m.mapping(node)) {
                Some(m) => m.surface_demand(need),
                None => Some(need),
            },
            d => d.demand(need),
        }
    }

    /// Needs of every node upstream of `seeds`, consumers before what they consume, so a
    /// node's need is complete (the most any consumer asks) before it is passed on.
    fn propagate(&self, seeds: &[(NodeId, [f64; 2])]) -> Needs {
        // Everything upstream of the seeds, with how many consumers within it each one has.
        let mut pending: BTreeMap<NodeId, usize> = BTreeMap::new();
        let mut edges: BTreeMap<NodeId, Vec<(NodeId, Edge)>> = BTreeMap::new();
        let mut stack: Vec<NodeId> = seeds.iter().map(|(id, _)| *id).collect();
        let mut seen: BTreeSet<NodeId> = stack.iter().copied().collect();
        while let Some(id) = stack.pop() {
            let Some(node) = self.net.node(id) else { continue };
            let up = self.upstream(node);
            for (u, _) in &up {
                *pending.entry(*u).or_default() += 1;
                if seen.insert(*u) {
                    stack.push(*u);
                }
            }
            edges.insert(id, up);
        }
        let mut need: BTreeMap<NodeId, [f64; 2]> = BTreeMap::new();
        let mut surfaces = BTreeMap::new();
        let mut order = Vec::new();
        for (id, n) in seeds {
            merge(&mut need, *id, *n);
        }
        // Depth first, so the report lists one chain after the other.
        let mut ready: Vec<NodeId> =
            seen.iter().copied().filter(|id| pending.get(id).copied().unwrap_or(0) == 0).collect();
        ready.reverse();
        while let Some(id) = ready.pop() {
            order.push(id);
            for &(u, edge) in edges.get(&id).map(Vec::as_slice).unwrap_or_default() {
                if let Some(n) = need.get(&id).copied()
                    && let Some(up) = self.pass_on(id, edge, n)
                {
                    merge(&mut need, u, up);
                    if let Edge::Input(i) = edge
                        && self.cooker.density(id, i) == Some(Density::Varying)
                    {
                        surfaces.insert(id, (i, up));
                    }
                }
                if let Some(p) = pending.get_mut(&u) {
                    *p = p.saturating_sub(1);
                    if *p == 0 {
                        ready.push(u);
                    }
                }
            }
        }
        order.retain(|id| need.contains_key(id));
        Needs { nodes: need, surfaces, order }
    }

    /// Actual pixels per needed pixel along the worse axis.
    fn ratio(&self, id: NodeId, need: [f64; 2]) -> Option<f64> {
        let [w, h] = self.size(id)?;
        let r = (f64::from(w) / need[0].max(1e-9)).min(f64::from(h) / need[1].max(1e-9));
        r.is_finite().then_some(r)
    }
}

/// What [`Scene::propagate`] found.
struct Needs {
    /// Per node, the most any consumer needs of it.
    nodes: BTreeMap<NodeId, [f64; 2]>,
    /// Per mapper and render node, the input whose need varies across its picture (the
    /// mapper's surface, the render's texture) and what it needs of it (measured once; the
    /// overlay reuses it).
    surfaces: BTreeMap<NodeId, (usize, [f64; 2])>,
    /// The nodes with a need, consumers before what they consume.
    order: Vec<NodeId>,
}

fn merge(need: &mut BTreeMap<NodeId, [f64; 2]>, id: NodeId, n: [f64; 2]) {
    if !(n[0].is_finite() && n[1].is_finite()) {
        return;
    }
    let e = need.entry(id).or_insert([0.0, 0.0]);
    *e = [e[0].max(n[0]), e[1].max(n[1])];
}

fn verdict(ratio: Option<f64>) -> Verdict {
    match ratio {
        None => Verdict::Unknown,
        Some(r) if r < COARSE => Verdict::Coarse,
        Some(r) if r > WASTED => Verdict::Wasted,
        Some(_) => Verdict::Ok,
    }
}

/// The size for a node of `size` that needs `need`: scaled evenly (the aspect stays, so
/// nothing in it stretches) until neither axis is short, the longer side rounded up to the next
/// of [`STEPS`] (or, where that would be wasted, to a multiple of 16; for tiny sizes the exact
/// one), capped at `max_side`.
pub fn suggest(size: [u32; 2], need: [f64; 2], max_side: u32) -> [u32; 2] {
    let (w, h) = (f64::from(size[0].max(1)), f64::from(size[1].max(1)));
    let f = (need[0] / w).max(need[1] / h);
    let (long, short) = (w.max(h), w.min(h));
    let max_side = max_side.max(1);
    let wanted = long * f;
    let fits = |s: f64| s >= wanted - 0.5 && s <= wanted * WASTED;
    let step = STEPS.iter().copied().map(f64::from).find(|s| *s >= wanted - 0.5).filter(|s| fits(*s));
    let sixteen = ((wanted - 0.5) / 16.0).ceil().max(1.0) * 16.0;
    let exact = (wanted - 0.5).ceil().max(1.0);
    let target = step.or_else(|| fits(sixteen).then_some(sixteen)).unwrap_or(exact);
    // `as` saturates; the cap keeps it in range anyway.
    let target = (target.min(f64::from(max_side)) as u32).max(1);
    let other = (f64::from(target) * short / long).round().max(1.0) as u32;
    if w >= h { [target, other] } else { [other, target] }
}

/// What the projectors of `scene` need of every node they show.
pub fn analyze(scene: &Scene<'_>) -> Report {
    let mut report = Report::default();
    let Some(projector) = scene.types.desc(PROJECTOR) else { return report };
    let mut seeds = Vec::new();
    for node in scene.net.nodes().filter(|n| n.kind == PROJECTOR) {
        let param = |name: &str| node.param(projector, name);
        let viewport = match param("viewport").and_then(Value::components) {
            Some(&[x, y, w, h]) => [x, y, w, h],
            _ => [0.0, 0.0, 1.0, 1.0],
        };
        let r = raster(scene.board, node.id, viewport);
        let known = scene.board.and_then(|b| b.projectors.get(&node.id)?.display.as_ref()).is_some();
        report.projectors.push(ProjectorRes {
            path: path(scene.net, node.id),
            raster: [r.0, r.1],
            assumed: !known && r == DEFAULT_RASTER,
        });
        let off = param("active").and_then(Value::as_bool) == Some(false)
            || param("test").and_then(Value::as_bool) == Some(true);
        let Some(port) = node.inputs.first().copied().flatten() else { continue };
        let Some(size) = scene.size(port.node) else { continue };
        if off {
            continue;
        }
        let fit = Fit::parse(param("fit").and_then(Value::as_str).unwrap_or_default());
        let Some(placed) = place(fit, [0.0, 0.0, 1.0, 1.0], r, (size[0], size[1])) else { continue };
        // The input's frame spans the placed rectangle, divided by the part of it shown.
        let need = [
            f64::from(placed.dest.w) / f64::from(placed.uv_scale[0]).max(1e-9),
            f64::from(placed.dest.h) / f64::from(placed.uv_scale[1]).max(1e-9),
        ];
        seeds.push((port.node, need));
    }
    let needs = scene.propagate(&seeds);
    for &id in &needs.order {
        let measured = scene.renders().and_then(|r| r.field(id)).is_some_and(|(_, current)| current);
        if scene.kind(id) == RENDER && scene.cooker.density(id, 1) == Some(Density::Varying) && !measured {
            report.pending.push(path(scene.net, id));
        }
    }
    for &id in &needs.order {
        let Some(&n) = needs.nodes.get(&id) else { continue };
        let Some(node) = scene.net.node(id) else { continue };
        let size = scene.size(id);
        let ratio = scene.ratio(id, n);
        let verdict = verdict(ratio);
        let sets_size = scene.cooker.sets_size(id);
        let keep =
            scene.types.desc(&node.kind).and_then(|d| node.param(d, KEEP_SIZE)).and_then(Value::as_bool)
                == Some(true);
        let suggested = match (size, verdict) {
            (Some(s), Verdict::Coarse | Verdict::Wasted) if sets_size => Some(suggest(s, n, scene.max_side)),
            _ => None,
        };
        report.nodes.push(NodeRes {
            id,
            path: path(scene.net, id),
            kind: node.kind.clone(),
            size,
            // A hair above a whole number is float noise, not a pixel more.
            needed: n.map(|v| (v - 1e-6).ceil().clamp(0.0, f64::from(u32::MAX)) as u32),
            ratio,
            verdict,
            sets_size,
            keep,
            suggested,
        });
    }
    report.overlays = overlays(scene, &needs);
    report
}

/// Per mapper and render node: its picture's pixels per needed projector pixel, how much
/// coarser than its surface (a render's: its texture) the weakest node behind the surface is,
/// and its own ratio (the overlay shows the weakest link). Where one such node is behind
/// another, only the one nearer the content draws: its colours pass through the other, whose
/// own tint would hide them.
fn overlays(scene: &Scene<'_>, needs: &Needs) -> BTreeMap<NodeId, Overlay> {
    let mut out = BTreeMap::new();
    let mut behind_of: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for node in scene.net.nodes().filter(|n| n.kind == MAPPER || n.kind == RENDER) {
        let (Some(n), Some(size), Some(&(input, s_need))) =
            (needs.nodes.get(&node.id), scene.size(node.id), needs.surfaces.get(&node.id))
        else {
            continue;
        };
        let Some(surface) = node.inputs.get(input).copied().flatten().map(|p| p.node) else { continue };
        let Some(s_ratio) = scene.ratio(surface, s_need) else { continue };
        let behind = scene.propagate(&[(surface, s_need)]).nodes;
        let weakest = behind.iter().filter_map(|(id, n)| scene.ratio(*id, *n)).fold(s_ratio, f64::min);
        behind_of.insert(node.id, behind.keys().copied().collect());
        out.insert(
            node.id,
            Overlay {
                factor: (weakest / s_ratio.max(1e-9)).clamp(0.0, 1.0) as f32,
                scale: [f64::from(size[0]) / n[0].max(1e-9), f64::from(size[1]) / n[1].max(1e-9)]
                    .map(|v| v as f32),
                // A mapper or render too coarse for its projector makes the whole picture
                // coarse; otherwise the surface decides (a surface finer than needed stays blue).
                cap: scene.ratio(node.id, *n).filter(|r| *r < COARSE).map_or(f32::MAX, |r| r as f32),
            },
        );
    }
    let drawn: BTreeSet<NodeId> = out.keys().copied().collect();
    out.retain(|id, _| behind_of.get(id).is_none_or(|b| b.is_disjoint(&drawn)));
    out
}

/// Pixel-sized params whose look changes when the nodes in `changed` change size: theirs, and
/// those of the nodes downstream that follow their size. As `/blur1.size`.
pub fn pixel_params(net: &Network, types: &NodeTypes, cooker: &Cooker, changed: &[NodeId]) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut seen: BTreeSet<NodeId> = BTreeSet::new();
    let mut stack: Vec<NodeId> = changed.to_vec();
    let first: BTreeSet<NodeId> = changed.iter().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = net.node(id) else { continue };
        let Some(desc) = types.desc(&node.kind) else { continue };
        if !first.contains(&id) && cooker.sets_size(id) {
            // Its own size stays; what it shows is rescaled into it.
            continue;
        }
        for spec in desc.params.iter().filter(|s| s.unit == Unit::Pixels) {
            if spec.name != "width" && spec.name != "height" {
                out.insert(format!("{}.{}", path(net, id), spec.name));
            }
        }
        for c in net.consumers(id) {
            let tex = c.inputs.iter().enumerate().any(|(i, p)| {
                p.is_some_and(|p| p.node == id)
                    && types
                        .desc(&c.kind)
                        .and_then(|d| d.inputs.get(i))
                        .is_some_and(|s| s.kind == DataKind::Tex)
            });
            if tex {
                stack.push(c.id);
            }
        }
    }
    out.into_iter().collect()
}

fn path(net: &Network, id: NodeId) -> String {
    net.path_of(id).map(|p| p.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_keep_the_aspect_and_round_to_steps() {
        // Needs 1900 x 1000 of a 1280 x 720: 1920 across, the height from the aspect.
        assert_eq!(suggest([1280, 720], [1900.0, 1000.0], 16384), [1920, 1080]);
        // Too fine: down to what is needed, rounded up.
        assert_eq!(suggest([3840, 2160], [1000.0, 560.0], 16384), [1024, 576]);
        // The worse axis decides; portrait stays portrait.
        assert_eq!(suggest([1080, 1920], [500.0, 1500.0], 16384), [864, 1536]);
        // Capped by the device.
        assert_eq!(suggest([1920, 1080], [30000.0, 1000.0], 8192), [8192, 4608]);
        // Already a step: unchanged.
        assert_eq!(suggest([1920, 1080], [1920.0, 1080.0], 16384), [1920, 1080]);
        // The next step (1024) would be wasted for 770: a multiple of 16 instead.
        assert_eq!(suggest([1024, 1024], [770.0, 770.0], 16384), [784, 784]);
        // Tiny: exact.
        assert_eq!(suggest([64, 64], [9.0, 9.0], 16384), [9, 9]);
        // Whatever comes out lies in the band.
        for need in (10..5000).step_by(7) {
            let need = f64::from(need);
            let [w, _] = suggest([1000, 500], [need, need / 2.0], 16384);
            assert_eq!(verdict(Some(f64::from(w) / need)), Verdict::Ok, "{need} -> {w}");
        }
    }

    #[test]
    fn verdicts_have_a_band_around_one() {
        assert_eq!(verdict(Some(0.85)), Verdict::Coarse);
        assert_eq!(verdict(Some(0.9)), Verdict::Ok);
        assert_eq!(verdict(Some(1.3)), Verdict::Ok);
        assert_eq!(verdict(Some(1.4)), Verdict::Wasted);
        assert_eq!(verdict(None), Verdict::Unknown);
    }
}
