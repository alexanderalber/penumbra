//! `tex.render`: geometry seen through a camera. The camera comes from the `camera` input (a
//! `view.camera` node) or, while that is not connected, from the node's own params, the same as the
//! camera node's (`crate::view`): built from position, target, field of view, roll and lens shift,
//! or a calibrated `calib::Camera` as JSON, so a projector renders exactly as it was solved. Two
//! outputs: the picture, and the data pass (texture coordinates, part id + 1, distance along the
//! view). Lights come in through the `lights` input; without them phong materials are black,
//! constant ones as they are. Lights casting shadows draw them through shadow maps of
//! `shadow_size`. Ambient occlusion (`ao`) dims ambient and environment light in creases and
//! where objects meet.
//!
//! For `res.analyze` a render node that shows its texture input measures, while the analysis asks
//! for it ([`Renders`]), how far the texture coordinates move per pixel of its picture: a small
//! extra pass read back without waiting, repeated when the geometry or the camera changes. From
//! that field the analysis takes what the texture needs ([`Field::demand`]), and the colour pass
//! draws the resolution overlay per pixel.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use penumbra_core::{Error, NodeId, ParamSpec, Unit, Value};
use penumbra_gpu::projection::Overlay;
use penumbra_gpu::render::{
    Ao, Derivatives, LightKind, Look, MAX_LIGHTS, MAX_PROJECTORS, MAX_SHADOWS, Measurement, Renderer,
    Surface, View,
};
use penumbra_gpu::{Geo, Lights, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, Density, FnNodeType, Network, NodeDef, NodeDesc,
    NodeTypes, Operator, Waker,
};

use crate::geo::view_of;
use crate::mapping::{DEMAND_PERCENTILE, quantile};
use crate::view::Viewpoint;

pub const RENDER: &str = "tex.render";

/// The input that, where connected, replaces the node's own camera params.
pub const CAMERA_INPUT: usize = 3;

pub const SURFACES: [&str; 6] = ["auto", "material", "texture", "parts", "uv", "normals"];

/// A measurement that has not arrived after this long is given up (a lost device).
const MEASURE_TIMEOUT: Duration = Duration::from_secs(10);
/// At most one measurement per node this often (a camera moved by a channel would otherwise
/// measure every frame).
const MEASURE_EVERY: Duration = Duration::from_millis(250);

/// How far the texture coordinates move per pixel of a render's picture, measured on the GPU
/// over the pixels that show the texture.
#[derive(Debug)]
pub struct Field {
    /// The measuring pass's size.
    pub size: (u32, u32),
    /// Per covered pixel: the texture coordinates' change per unit of the picture (its whole
    /// width or height), `[du/dx, dv/dx, du/dy, dv/dy]`.
    jacobians: Vec<[f32; 4]>,
}

impl Field {
    /// From a measurement's pixels: those where something was drawn, scaled from the
    /// measuring pass's pixels to the whole picture.
    pub fn new(size: (u32, u32), pixels: &[Derivatives]) -> Self {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let jacobians = pixels
            .iter()
            .filter(|d| d.iter().all(|v| v.is_finite()) && d.iter().any(|v| *v != 0.0))
            .map(|d| [d[0] * w, d[1] * w, d[2] * h, d[3] * h])
            .collect();
        Self { size, jacobians }
    }

    /// Pixels of the measuring pass that show the texture.
    pub fn covered(&self) -> usize {
        self.jacobians.len()
    }

    /// The resolution (across, down) the texture needs so that one texel covers about one
    /// pixel of the picture, when the picture needs `out` pixels (its own size when it is shown
    /// one to one): per pixel as [`Density::demand`] takes a node's Jacobian, then the
    /// [`DEMAND_PERCENTILE`] of each axis over the pixels showing it. `None` when none does.
    pub fn demand(&self, out: [f64; 2]) -> Option<[f64; 2]> {
        let (ox, oy) = (out[0].max(1e-9), out[1].max(1e-9));
        let mut across = Vec::with_capacity(self.jacobians.len());
        let mut down = Vec::with_capacity(self.jacobians.len());
        for j in &self.jacobians {
            let [ux, vx, uy, vy] = j.map(f64::from);
            let axis = |dx: f64, dy: f64| {
                let len = (dx / ox).hypot(dy / oy);
                if len.is_finite() && len > 1e-12 { 1.0 / len } else { 0.0 }
            };
            across.push(axis(ux, uy));
            down.push(axis(vx, vy));
        }
        Some([quantile(across, DEMAND_PERCENTILE)?, quantile(down, DEMAND_PERCENTILE)?])
    }
}

/// What one render node told the analysis.
struct Entry {
    waker: Waker,
    /// What its field depends on as of its last cook (geometry, camera, size); `None` while it
    /// does not show its texture input.
    key: Option<u64>,
    /// The latest field, with the key it was measured for.
    field: Option<(u64, Arc<Field>)>,
    /// A measurement on its way, with its key and when it started.
    pending: Option<(u64, Instant, Measurement)>,
    /// When the last measurement started.
    started: Option<Instant>,
    /// How it drew last, for the viewer's free view.
    scene: Option<RenderScene>,
}

/// How a render node drew its last picture: its camera, size and look. The viewer's free view
/// shows the same scene from elsewhere and draws this camera into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderScene {
    pub viewpoint: Viewpoint,
    pub size: (u32, u32),
    pub look: Look,
}

impl Entry {
    fn stale(&self) -> bool {
        self.key.is_some() && self.field.as_ref().map(|(k, _)| *k) != self.key
    }
}

#[derive(Default)]
struct Inner {
    entries: BTreeMap<NodeId, Entry>,
    overlays: BTreeMap<NodeId, Overlay>,
    /// The analysis wants fields (the overlay is on or the Helpers window is open).
    active: bool,
    /// ...or a command asked until then.
    until: Option<Instant>,
}

impl Inner {
    fn active(&self, now: Instant) -> bool {
        self.active || self.until.is_some_and(|u| now < u)
    }

    /// Wakes the nodes whose field is missing or outdated and not on its way.
    fn wake_stale(&self) {
        for e in self.entries.values().filter(|e| e.stale() && e.pending.is_none()) {
            e.waker.wake();
        }
    }
}

/// The render nodes that cooked, by node: their measured fields for `res.analyze` and the
/// overlay each one draws. Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct Renders(Arc<Mutex<Inner>>);

impl Renders {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// While on, render nodes showing their texture measure their field when it is missing or
    /// outdated; turning it on wakes those that need to.
    pub fn set_active(&self, on: bool) {
        let mut inner = self.lock();
        let was = inner.active(Instant::now());
        inner.active = on;
        if on && !was {
            inner.wake_stale();
        }
    }

    /// As [`Renders::set_active`], for `duration` from now (a command waiting for fields).
    pub fn activate_for(&self, duration: Duration) {
        let mut inner = self.lock();
        let until = Instant::now() + duration;
        inner.until = Some(inner.until.map_or(until, |u| u.max(until)));
        inner.wake_stale();
    }

    /// Takes the measurements the GPU has delivered (after [`penumbra_gpu::Gpu::poll`]) and
    /// wakes the nodes still to measure. Returns whether a field arrived. Called by the engine
    /// on the cook thread with the analysis, so turning pixels into fields stays off the show
    /// path (once a second at most, a few milliseconds).
    pub fn collect(&self) -> bool {
        let mut inner = self.lock();
        let now = Instant::now();
        let mut arrived = false;
        for (id, e) in &mut inner.entries {
            let Some((key, since, m)) = &e.pending else { continue };
            match m.take() {
                None if now.duration_since(*since) < MEASURE_TIMEOUT => continue,
                None => log::warn!("render node {id:?}: its measurement did not arrive"),
                Some(Ok(pixels)) => {
                    e.field = Some((*key, Arc::new(Field::new(m.size(), &pixels))));
                    arrived = true;
                }
                Some(Err(err)) => log::warn!("render node {id:?}: {err}"),
            }
            e.pending = None;
        }
        if inner.active(now) {
            inner.wake_stale();
        }
        arrived
    }

    /// Node `id`'s latest field, and whether it is current (measured for its present geometry
    /// and camera).
    pub fn field(&self, id: NodeId) -> Option<(Arc<Field>, bool)> {
        let inner = self.lock();
        let e = inner.entries.get(&id)?;
        let (_, f) = e.field.as_ref()?;
        Some((f.clone(), !e.stale()))
    }

    /// Shows the resolution overlay on the render nodes in `overlays` and on no other; wakes
    /// those whose overlay changed.
    pub fn set_overlays(&self, overlays: BTreeMap<NodeId, Overlay>) {
        let mut inner = self.lock();
        let changed: Vec<NodeId> = inner
            .overlays
            .keys()
            .chain(overlays.keys())
            .filter(|id| inner.overlays.get(id) != overlays.get(id))
            .copied()
            .collect();
        inner.overlays = overlays;
        for id in changed {
            if let Some(e) = inner.entries.get(&id) {
                e.waker.wake();
            }
        }
    }

    fn overlay(&self, id: NodeId) -> Option<Overlay> {
        self.lock().overlays.get(&id).copied()
    }

    /// Forgets render nodes no longer in `net`. Called by the engine after every pass.
    pub fn prune(&self, net: &Network) {
        self.lock().entries.retain(|id, _| net.node(*id).is_some_and(|n| n.kind == RENDER));
    }

    /// How node `id` drew last ([`RenderScene`]).
    pub fn scene(&self, id: NodeId) -> Option<RenderScene> {
        self.lock().entries.get(&id)?.scene
    }

    /// Records what node `id` shows and how it draws, and returns whether it should measure now.
    fn publish(&self, id: NodeId, waker: Waker, key: Option<u64>, scene: RenderScene) -> bool {
        let mut inner = self.lock();
        let now = Instant::now();
        let active = inner.active(now);
        let e = inner.entries.entry(id).or_insert_with(|| Entry {
            waker: waker.clone(),
            key: None,
            field: None,
            pending: None,
            started: None,
            scene: None,
        });
        e.waker = waker;
        e.key = key;
        e.scene = Some(scene);
        active
            && e.stale()
            && e.pending.is_none()
            && e.started.is_none_or(|t| now.duration_since(t) >= MEASURE_EVERY)
    }

    fn start(&self, id: NodeId, key: u64, m: Measurement) {
        let now = Instant::now();
        if let Some(e) = self.lock().entries.get_mut(&id) {
            e.pending = Some((key, now, m));
            e.started = Some(now);
        }
    }
}

/// Whether a render node shows its texture input: `surface` is `texture`, or `auto` with the
/// input connected.
fn shows_texture(def: &NodeDef, desc: &NodeDesc) -> bool {
    match def.param(desc, "surface").and_then(Value::as_str).unwrap_or("auto") {
        "texture" => true,
        "auto" => def.inputs.get(1).is_some_and(Option::is_some),
        _ => false,
    }
}

/// What a field depends on: the placed models, the camera, culling and the picture's size.
fn field_key(geo: &Geo, view: &View, cull: bool, size: (u32, u32)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for item in &geo.items {
        item.model.id.hash(&mut h);
        item.world.iter().flatten().for_each(|v| v.to_bits().hash(&mut h));
    }
    view.clip_from_world.iter().flatten().for_each(|v| v.to_bits().hash(&mut h));
    (cull, size).hash(&mut h);
    h.finish()
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = super::sized(NodeDesc::new(
        RENDER,
        "Geometry through a camera: the picture, and a data pass (uv, part id + 1, depth)",
    ))
    .output("data", DataKind::Tex)
    .input("geo", DataKind::Geo)
    .input("texture", DataKind::Tex)
    .input("lights", DataKind::Light)
    .input("camera", DataKind::View);
    let desc = crate::view::params(desc).param(ParamSpec::choice("surface", &SURFACES).doc(
        "auto: the texture input where connected, else the file's materials; parts: a colour per part id",
    ));
    let desc = crate::view::more_params(desc, "camera")
        .param(ParamSpec::color("background", [0.0; 4]).more())
        .param(
            ParamSpec::bool("cull", false)
                .more()
                .doc("Leave out faces turned away (counter-clockwise is the front)"),
        )
        .param(ParamSpec::bool("antialias", true).more().doc("Four samples per pixel"))
        .param(
            ParamSpec::int("shadow_size", 2048)
                .clamp(64.0, 8192.0)
                .range(256.0, 4096.0)
                .more()
                .doc("Texels across each light's shadow map: sharper shadows, more memory"),
        )
        .param(ParamSpec::bool("ao", false).group("occlusion").doc(
            "Ambient occlusion: less ambient and environment light in creases and where objects meet; direct light and unlit surfaces stay as they are",
        ))
        .param(
            ParamSpec::float("ao_radius", 0.5)
                .clamp(0.001, 1000.0)
                .range(0.01, 2.0)
                .unit(Unit::Metres)
                .group("occlusion")
                .doc("How far around a point geometry darkens it"),
        )
        .param(
            ParamSpec::float("ao_strength", 1.0)
                .clamp(0.0, 8.0)
                .range(0.0, 3.0)
                .group("occlusion")
                .doc("How much of the hidden ambient light is taken away: 1 as estimated, 0.3 at most 30 %, above 1 darker"),
        );
    types.register(FnNodeType::new(desc, || Box::new(Render::default()) as Box<dyn Operator>))
}

#[derive(Default)]
struct Render {
    renderer: Renderer,
    ring: TexRing,
    data_ring: TexRing,
}

/// Where a render node sees from: its `camera` input where connected, else its own params.
pub fn viewpoint(cx: &CookContext<'_>) -> Result<Viewpoint, CookError> {
    if cx.def().inputs.get(CAMERA_INPUT).is_some_and(Option::is_some) {
        return cx
            .input_as::<Viewpoint>(CAMERA_INPUT)
            .copied()
            .ok_or_else(|| CookError::failed("the camera input has no camera yet"));
    }
    Viewpoint::from_params(cx)
}

impl Operator for Render {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        let (w, h) = (super::side(cx, "width")?, super::side(cx, "height")?);
        let Some(geo) = cx.input_as::<Geo>(0) else {
            return Err(if cx.def().inputs.first().is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect a geo input")
            });
        };
        let texture = cx.input_as::<Tex>(1);
        let surface = match cx.text("surface")? {
            "material" => Surface::Material,
            "texture" => Surface::Texture,
            "parts" => Surface::Parts,
            "uv" => Surface::Uv,
            "normals" => Surface::Normals,
            _ if texture.is_some() => Surface::Texture,
            _ => Surface::Material,
        };
        let viewpoint = viewpoint(cx)?;
        let camera = viewpoint.camera((w, h), |m| cx.warn(m));
        let view = view_of(&camera, viewpoint.near, viewpoint.far)
            .ok_or_else(|| CookError::failed("the camera cannot be drawn"))?;
        let background = match cx.param("background").and_then(|v| v.components()) {
            Some(&[r, g, b, a]) => [r * a, g * a, b * a, a].map(|c| c as f32),
            _ => [0.0; 4],
        };
        // Wired lights not there yet: rather wait than show a frame without them.
        let lights = match cx.input_as::<Lights>(2) {
            Some(l) => l.0.as_slice(),
            None if cx.def().inputs.get(2).is_some_and(Option::is_some) => return Err(CookError::Pending),
            None => &[],
        };
        if lights.len() > MAX_LIGHTS {
            cx.warn(format!("{} lights: only the first {MAX_LIGHTS} are drawn", lights.len()));
        }
        let drawn = lights.get(..MAX_LIGHTS).unwrap_or(lights);
        let shadowed = drawn.iter().filter(|l| l.shadows && l.kind != LightKind::Point).count();
        if shadowed > MAX_SHADOWS {
            cx.warn(format!("{shadowed} lights cast shadows: only the first {MAX_SHADOWS} do"));
        }
        let pictures =
            drawn.iter().filter(|l| l.projector.as_ref().is_some_and(|p| p.texture.is_some())).count();
        if pictures > MAX_PROJECTORS {
            cx.warn(format!(
                "{pictures} projectors with a picture: only the first {MAX_PROJECTORS} are drawn"
            ));
        }
        let around = drawn.iter().filter(|l| l.environment.is_some()).count();
        if around > 1 {
            cx.warn(format!("{around} environment lights: only the first is drawn"));
        }
        let ao = cx.bool("ao")?.then(|| -> Result<Ao, CookError> {
            Ok(Ao { radius: cx.float("ao_radius")? as f32, strength: cx.float("ao_strength")? as f32 })
        });
        let ao = ao.transpose()?;
        if ao.is_some()
            && !drawn.iter().any(|l| matches!(l.kind, LightKind::Ambient | LightKind::Environment))
        {
            cx.warn("ambient occlusion dims only ambient and environment light, and none is wired");
        }
        let shown = surface == Surface::Texture && texture.is_some();
        let renders = cx.resource::<Renders>();
        let look = Look {
            surface,
            shade_all: false,
            background,
            cull: cx.bool("cull")?,
            antialias: cx.bool("antialias")?,
            overlay: renders.and_then(|r| r.overlay(cx.node())).filter(|_| shown),
            shadow_size: u32::try_from(cx.int("shadow_size")?).unwrap_or(2048),
            ao,
        };
        let color = self.ring.next(gpu, w, h, RENDER)?;
        let data = self.data_ring.next(gpu, w, h, "tex.render data")?;
        let key = shown.then(|| field_key(geo, &view, look.cull, (w, h)));
        let scene = RenderScene { viewpoint, size: (w, h), look };
        let measure = renders.is_some_and(|r| r.publish(cx.node(), cx.waker(), key, scene));
        match (renders, key) {
            (Some(renders), Some(key)) if measure => {
                let m = self.renderer.render_and_measure(
                    gpu,
                    geo,
                    lights,
                    &view,
                    &look,
                    texture,
                    &color,
                    Some(&data),
                )?;
                renders.start(cx.node(), key, m);
            }
            _ => self.renderer.render(gpu, geo, lights, &view, &look, texture, &color, Some(&data))?,
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, color), Data::new(DataKind::Tex, data)]))
    }

    /// The texture input is shown through the camera: measured per pixel ([`Renders`]).
    fn density(&self, def: &NodeDef, desc: &NodeDesc, input: usize) -> Density {
        if input == 1 && shows_texture(def, desc) { Density::Varying } else { Density::Unrelated }
    }

    fn sets_size(&self, _: &NodeDef, _: &NodeDesc) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_asks_its_percentile_of_the_texture() {
        // 100 x 50 measuring pixels, the texture across once over the picture's width and
        // twice over its height: 100 and 25 texels.
        let even = [1.0 / 100.0, 0.0, 0.0, 2.0 / 50.0];
        let f = Field::new((100, 50), &vec![even; 5000]);
        let d = f.demand([100.0, 50.0]).unwrap();
        assert!((d[0] - 100.0).abs() < 1e-3 && (d[1] - 25.0).abs() < 1e-3, "{d:?}");
        // Needed at twice the size: twice the texels.
        let d = f.demand([200.0, 100.0]).unwrap();
        assert!((d[0] - 200.0).abs() < 1e-3 && (d[1] - 50.0).abs() < 1e-3, "{d:?}");
        // A quarter of the pixels magnify ten times (near the camera): above the percentile's
        // cut, they decide; 3 % of them do not.
        let near = [0.1 / 100.0, 0.0, 0.0, 0.2 / 50.0];
        let mut px = vec![even; 7500];
        px.extend(vec![near; 2500]);
        let d = Field::new((100, 50), &px).demand([100.0, 50.0]).unwrap();
        assert!((d[0] - 1000.0).abs() < 1e-2, "{d:?}");
        let mut px = vec![even; 9700];
        px.extend(vec![near; 300]);
        let d = Field::new((100, 50), &px).demand([100.0, 50.0]).unwrap();
        assert!((d[0] - 100.0).abs() < 1e-3, "{d:?}");
        // Rotated by 90 degrees on screen: u runs down the picture.
        let turned = [0.0, 2.0 / 100.0, 1.0 / 50.0, 0.0];
        let d = Field::new((100, 50), &[turned]).demand([100.0, 50.0]).unwrap();
        assert!((d[0] - 50.0).abs() < 1e-3 && (d[1] - 50.0).abs() < 1e-3, "{d:?}");
        // Nothing drawn, or not finite: no demand.
        let empty = Field::new((4, 4), &[[0.0; 4], [f32::NAN, 0.0, 0.0, 1.0]]);
        assert_eq!((empty.covered(), empty.demand([4.0, 4.0])), (0, None));
    }
}
