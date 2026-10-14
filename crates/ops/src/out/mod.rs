//! `out` nodes: sinks that leave the graph.
//!
//! `out.projector` is one projector: a viewport inside the fullscreen output window on one
//! display. Usually a projector fills its display; a 4K signal into a hardware splitter is one
//! display with four projector nodes, each with a quarter as viewport. Projectors naming the
//! same display share its window.
//!
//! The node itself does no GPU work. After every cook pass the engine collects the projectors
//! and the textures feeding them ([`collect`]); the output crate opens a window per display and
//! draws them from its own threads. What the output side found out (display not connected,
//! window open) comes back through the [`OutputBoard`], which the cooker provides as a
//! resource, so the node shows it as its error or warning like any other node.
//!
//! Mapping a surface onto a projector (corners, mesh, soft edges) is `tex.mapper`'s job; a
//! projector only places its input into its viewport by `fit`. The outputs draw that with the
//! same drawer the mapper uses ([`penumbra_gpu::Projection`]), from a plain [`Mapping`].

pub mod place;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use penumbra_core::{Error, NodeId, ParamSpec, Value};
use penumbra_gpu::Tex;
use penumbra_graph::{
    CookContext, CookError, Cooked, Cooker, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes,
    Operator,
};
use serde::Serialize;

use crate::mapping::{Mapping, Setup};

/// The projector image size assumed while its display is unknown (headless, not connected).
pub const DEFAULT_RASTER: (u32, u32) = (1920, 1080);

pub const PROJECTOR: &str = "out.projector";

/// The projector that shows nothing, only paces.
pub const PACER: &str = "out.pacer";

/// Refresh rates `out.pacer` accepts, in Hz.
pub const PACER_RANGE: (f64, f64) = (10.0, 500.0);

/// How a projector's texture fills its viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Whole texture visible, aspect kept, black bars where the shapes differ.
    #[default]
    Fit,
    /// Viewport covered, aspect kept, the texture cropped.
    Fill,
    /// Viewport covered, aspect ignored.
    Stretch,
}

impl Fit {
    pub fn parse(s: &str) -> Fit {
        match s {
            "fill" => Fit::Fill,
            "stretch" => Fit::Stretch,
            _ => Fit::Fit,
        }
    }
}

/// One projector as the outputs draw it, collected after a cook pass.
#[derive(Clone, Debug)]
pub struct ProjectorFrame {
    pub node: NodeId,
    pub path: String,
    /// The display id as set (`EPSON PJ @ port 4`); empty when none is chosen.
    pub display: String,
    pub active: bool,
    /// x, y, width, height in display sizes (0..1).
    pub viewport: [f64; 4],
    pub fit: Fit,
    /// What its input carries (a `penumbra_gpu::Tex`); `None` when unconnected or not cooked.
    pub source: Option<Data>,
    /// Its input placed by `fit`; `None` without an input. With `test` on, the whole viewport.
    pub mapping: Option<Arc<Mapping>>,
    /// Shows its test pattern instead of the input.
    pub test: bool,
    /// Its colour in the test pattern (by its rank among the projectors).
    pub tint: [f32; 3],
}

/// A display as the output side sees it (named by the system crate, sized by winit).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DisplayInfo {
    pub id: String,
    pub monitor: String,
    pub port: String,
    /// The connector type the OS reports (informative, not always right).
    pub connector: String,
    pub os_name: String,
    pub size: [u32; 2],
    pub position: [i32; 2],
    pub refresh_hz: Option<f64>,
    /// The editor window is (mostly) on this display; projectors are not shown here.
    pub editor: bool,
}

/// An open output window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowInfo {
    pub display: String,
    pub size: [u32; 2],
    /// Presented frames per second over the last report period.
    pub fps: f64,
    /// Frames that took longer than one and a half refresh periods since the window opened.
    pub missed: u64,
    /// Longest frame interval of the last report period, in milliseconds.
    pub max_ms: f64,
    /// This window's vsync drives the show clock.
    pub paces: bool,
    pub error: Option<String>,
}

/// What the output side reports about one projector node.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProjectorState {
    /// The display it is shown on (resolved id).
    pub display: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
}

/// Shared between the output side, the engine's commands and the projector nodes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BoardState {
    /// Whether this process opens output windows at all (not in headless runs).
    pub managed: bool,
    pub displays: Vec<DisplayInfo>,
    pub windows: Vec<WindowInfo>,
    pub projectors: BTreeMap<NodeId, ProjectorState>,
    /// Where the editor window is (`x, y, w, h` in physical desktop pixels), set by the UI;
    /// projectors are not opened on the display that shows it.
    pub editor: Option<[i32; 4]>,
}

/// The [`BoardState`] behind a lock; cheap to clone.
#[derive(Clone, Default)]
pub struct OutputBoard(Arc<Mutex<BoardState>>);

impl OutputBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> BoardState {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut BoardState)) {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn projector(&self, id: NodeId) -> (bool, Option<ProjectorState>) {
        let s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        (s.managed, s.projectors.get(&id).cloned())
    }
}

/// Registers the `out` node types.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(PROJECTOR, "A projector: shows its input fullscreen on a display, or in a part of it")
        .param(
            ParamSpec::text("display", "")
                .doc("Display id from output.list, e.g. `EPSON PJ @ port 4`; the monitor name alone works when unique"),
        )
        .param(ParamSpec::bool("active", true).doc("Open the output window"))
        .param(ParamSpec::choice("fit", &["fit", "fill", "stretch"]).doc(
            "fit: all visible, black bars; fill: covered, cropped; stretch: covered, distorted",
        ))
        .param(
            ParamSpec::vec4("viewport", [0.0, 0.0, 1.0, 1.0])
                .clamp(0.0, 1.0)
                .more()
                .doc("x, y, width, height inside the display, in display sizes (a quarter of a 4K splitter: 0, 0, 0.5, 0.5)"),
        )
        .param(ParamSpec::bool("test", false).doc(
            "A test pattern in the projector's own pixels instead of the input: square cells, centre, \
             circle, and the image's edge in this projector's colour",
        ))
        .input("in", DataKind::Tex)
        // Not reading the clock; declared so the node picks up what the output side reports
        // (a display plugged in or out) every frame. Cooking it costs nothing.
        .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Projector) as Box<dyn Operator>))?;
    let desc = NodeDesc::new(
        PACER,
        "A projector that shows nothing: ticks like a projector's vsync and paces the show clock, for timing as at the show without one",
    )
    .param(ParamSpec::bool("active", true).doc("Tick; off, the show clock runs on its timer again"))
    .param(
        ParamSpec::float("refresh", 60.0)
            .clamp(PACER_RANGE.0, PACER_RANGE.1)
            .range(23.0, 240.0)
            .unit(penumbra_core::Unit::Hertz)
            .doc("The projector's refresh rate; as with a real one, the show clock counts one frame per tick"),
    )
    .input("in", DataKind::Tex)
    // As a projector: cooked every frame, so its input costs what it would at the show, and
    // the note about a real output pacing instead stays current.
    .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(SimulatedProjector) as Box<dyn Operator>))
}

/// The refresh rate of the first active `out.pacer` in `net`, if any.
pub fn pacer_refresh(net: &Network, types: &NodeTypes) -> Option<f64> {
    let desc = types.desc(PACER)?;
    net.nodes().filter(|n| n.kind == PACER).find_map(|n| {
        let active = n.param(desc, "active").and_then(|v| v.as_bool()).unwrap_or(true);
        let hz = n.param(desc, "refresh").and_then(|v| v.as_f64())?;
        (active && hz.is_finite()).then(|| hz.clamp(PACER_RANGE.0, PACER_RANGE.1))
    })
}

struct SimulatedProjector;

impl Operator for SimulatedProjector {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        if cx.bool("active")?
            && let Some(board) = cx.resource::<OutputBoard>()
            && let Some(w) = board.state().windows.iter().find(|w| w.paces)
        {
            cx.warn(format!("the output on {} paces the show clock; this one waits", w.display));
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

/// The size of a projector's image: its viewport of the display it is shown on, as the
/// output side reported it; [`DEFAULT_RASTER`] while unknown.
pub fn raster(board: Option<&BoardState>, node: NodeId, viewport: [f64; 4]) -> (u32, u32) {
    let display = board.and_then(|b| {
        let id = b.projectors.get(&node)?.display.as_ref()?;
        b.displays.iter().find(|d| &d.id == id).map(|d| (d.size[0], d.size[1]))
    });
    let (w, h) = display.unwrap_or(DEFAULT_RASTER);
    let [_, _, vw, vh] = viewport.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 });
    (((f64::from(w) * vw).round() as u32).max(1), ((f64::from(h) * vh).round() as u32).max(1))
}

fn viewport_of(value: Option<&Value>) -> [f64; 4] {
    match value.and_then(Value::components) {
        Some(&[x, y, w, h]) => [x, y, w, h],
        _ => [0.0, 0.0, 1.0, 1.0],
    }
}

struct Projector;

impl Operator for Projector {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let active = cx.bool("active")?;
        if !active {
            return Ok(Cooked::Outputs(Vec::new()));
        }
        if cx.text("display")?.trim().is_empty() {
            cx.warn("no display chosen (see output.list)");
            return Ok(Cooked::Outputs(Vec::new()));
        }
        let Some(board) = cx.resource::<OutputBoard>() else {
            return Ok(Cooked::Outputs(Vec::new()));
        };
        match board.projector(cx.node()) {
            (false, _) => cx.warn("this process opens no output windows (headless)"),
            (true, Some(state)) => {
                if let Some(e) = state.error {
                    return Err(CookError::Failed(e));
                }
                if let Some(w) = state.warning {
                    cx.warn(w);
                }
            }
            // The output side has not looked at this node yet.
            (true, None) => {}
        }
        if cx.def().inputs.first().is_none_or(Option::is_none) && !cx.bool("test")? {
            cx.warn("connect an input; the projector shows black");
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

/// The plain mappings computed so far, kept across cook passes so unchanged projectors keep
/// their geometry (and the outputs need not upload it again).
#[derive(Default)]
pub struct Mappings(HashMap<NodeId, (PlainKey, Arc<Mapping>)>);

/// What a plain mapping depends on: fit, projector size, input size.
type PlainKey = (Fit, (u32, u32), (u32, u32));

/// The projectors of `net` with what feeds them, after `cooker` cooked them.
pub fn collect(
    net: &Network,
    types: &NodeTypes,
    cooker: &Cooker,
    cache: &mut Mappings,
) -> Vec<ProjectorFrame> {
    let Some(desc) = types.desc(PROJECTOR) else { return Vec::new() };
    let board = cooker.resource::<OutputBoard>().map(OutputBoard::state);
    let mut out = Vec::new();
    for (rank, node) in net.nodes().filter(|n| n.kind == PROJECTOR).enumerate() {
        let param = |name: &str| node.param(desc, name);
        let text = |name: &str| match param(name) {
            Some(Value::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let viewport = viewport_of(param("viewport"));
        let fit = Fit::parse(&text("fit"));
        let test = matches!(param("test"), Some(Value::Bool(true)));
        let source = node.inputs.first().copied().flatten().and_then(|port| cooker.output(port).cloned());
        let size = source.as_ref().and_then(|d| d.get::<Tex>()).map(Tex::size);
        // The test pattern covers the whole viewport: a stretched placeholder.
        let placed = if test { Some((Fit::Stretch, (1, 1))) } else { size.map(|s| (fit, s)) };
        let mapping = placed.and_then(|(fit, size)| {
            let key = (fit, raster(board.as_ref(), node.id, viewport), size);
            if let Some((k, m)) = cache.0.get(&node.id)
                && *k == key
            {
                return Some(m.clone());
            }
            let m = Arc::new(Mapping::new(&Setup { fit, ..Setup::default() }, key.1, size).ok()?);
            cache.0.insert(node.id, (key, m.clone()));
            Some(m)
        });
        out.push(ProjectorFrame {
            node: node.id,
            path: net.path_of(node.id).map(|p| p.to_string()).unwrap_or_default(),
            display: text("display").trim().to_string(),
            active: !matches!(param("active"), Some(Value::Bool(false))),
            viewport,
            fit,
            source,
            mapping,
            test,
            tint: tint(rank),
        });
    }
    let alive: Vec<NodeId> = out.iter().map(|p| p.node).collect();
    cache.0.retain(|id, _| alive.contains(id));
    out
}

/// The colour of the projector of `rank` among the projector nodes (in node order) in its test
/// pattern; the Helpers window shows it next to the projector.
pub fn tint(rank: usize) -> [f32; 3] {
    let tints = crate::tex::mapper::TINTS;
    tints.get(rank % tints.len()).copied().unwrap_or([1.0; 3])
}

/// The rank of projector `id` among the projector nodes of `net`.
pub fn rank(net: &Network, id: NodeId) -> Option<usize> {
    net.nodes().filter(|n| n.kind == PROJECTOR).position(|n| n.id == id)
}

/// How `tex` drawn through `mapping` looks: geometry, viewport in target pixels, soft edges
/// against `peers`, pattern colour.
pub fn item_for<'a>(
    tex: &'a Tex,
    mapping: &'a Mapping,
    peers: &[[f64; 4]],
    tint: [f32; 3],
    viewport: [f32; 4],
) -> penumbra_gpu::projection::Item<'a> {
    let f = |r: [f64; 4]| r.map(|v| v as f32);
    penumbra_gpu::projection::Item {
        key: mapping.id,
        tex,
        vertices: &mapping.vertices,
        indices: &mapping.indices,
        viewport,
        look: penumbra_gpu::projection::Look {
            region: f(mapping.region),
            peers: peers.iter().copied().map(f).collect(),
            blend: mapping.blend,
            gamma: mapping.blend_gamma as f32,
            pattern: mapping.pattern,
            test: false,
            overlay: None,
            tint,
        },
    }
}

/// How a projector draws: its input placed by `fit` into `viewport` (target pixels), or its
/// test pattern (`blank` stands in for the texture, which the pattern does not read). `None`
/// without an input.
pub fn item<'a>(
    frame: &'a ProjectorFrame,
    blank: &'a Tex,
    viewport: [f32; 4],
) -> Option<penumbra_gpu::projection::Item<'a>> {
    let mapping = frame.mapping.as_deref()?;
    if frame.test {
        let mut item = item_for(blank, mapping, &[], frame.tint, viewport);
        item.look.test = true;
        return Some(item);
    }
    let tex = frame.source.as_ref()?.get::<Tex>()?;
    Some(item_for(tex, mapping, &[], [1.0; 3], viewport))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_types;
    use penumbra_core::{LatePolicy, NodePath, ParamPath, Rate, ShowClock};
    use penumbra_graph::{JobPool, Op};

    #[test]
    fn projectors_are_collected_and_report_the_board() {
        let types = Arc::new(builtin_types().unwrap());
        let set = |p: &str, v: Value| Op::Set { param: ParamPath::parse(p).unwrap(), value: v };
        let ops = [
            Op::Create {
                path: NodePath::parse("/p").unwrap(),
                kind: PROJECTOR.into(),
                params: Default::default(),
            },
            set("/p.display", Value::Text("EPSON PJ @ port 4".into())),
            set("/p.fit", Value::Text("fill".into())),
        ];
        let net = Arc::new(Network::new().apply(&types, &ops).unwrap().network);
        let p = net.resolve(&"/p".parse().unwrap()).unwrap();
        let mut cooker = Cooker::new(types.clone(), Arc::new(JobPool::new(1).unwrap()));
        let board = OutputBoard::new();
        cooker.provide(board.clone());
        let mut clock = ShowClock::new(Rate::FPS_60, LatePolicy::Realtime);

        cooker.cook(&net, clock.now(), &[p], &[]);
        let status = cooker.status(p).unwrap();
        assert!(status.warning.as_deref().unwrap_or("").contains("headless"), "{status:?}");

        board.update(|s| {
            s.managed = true;
            s.projectors
                .insert(p, ProjectorState { error: Some("not connected".into()), ..Default::default() });
        });
        clock.advance(1);
        cooker.cook(&net, clock.now(), &[p], &[]);
        assert_eq!(cooker.status(p).unwrap().error.as_deref(), Some("not connected"));
        // The node picks up the next report by itself.
        board.update(|s| {
            s.projectors.insert(
                p,
                ProjectorState { display: Some("EPSON PJ @ port 4".into()), ..Default::default() },
            );
        });
        clock.advance(1);
        cooker.cook(&net, clock.now(), &[p], &[]);
        let status = cooker.status(p).unwrap();
        assert_eq!(status.error, None);
        assert!(status.warning.as_deref().unwrap_or("").contains("connect an input"), "{status:?}");

        let frames = collect(&net, &types, &cooker, &mut Mappings::default());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].display, "EPSON PJ @ port 4");
        assert_eq!(frames[0].fit, Fit::Fill);
        assert_eq!(frames[0].viewport, [0.0, 0.0, 1.0, 1.0]);
        assert!(frames[0].active && frames[0].source.is_none());
    }
}
