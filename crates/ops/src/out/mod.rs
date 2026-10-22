//! `out` nodes: sinks that leave the graph.
//!
//! `out.video` records its input to a movie file ([`video`]); `out.spout` shares it with other
//! programs on this machine ([`spout`]), `out.ndi` with programs on the network ([`ndi`]).
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
//!
//! A projector's colour correction (params in group *colour*, [`correct`]) is the last step of
//! that draw, so projectors of different models can be matched to each other; `projector.match`
//! measures it with a camera. While it measures, the [`OutputBoard`] holds a flat signal per
//! projector ([`BoardState::flat`]) that replaces the picture without touching the network;
//! `scan.capture` shows its structured light patterns the same way ([`BoardState::stripes`]).
//! Neither goes through the colour correction.
//!
//! Two more inputs belong to the correction, both in the projector's own pixels: `gain`, a
//! factor per pixel and channel that evens out a hot spot and a colour cast, and `black`, a lift
//! of the black where another projector does not reach; `projector.flatfield` measures them.

pub mod correct;
pub mod ndi;
pub mod place;
pub mod spout;
pub mod video;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use penumbra_core::{Error, NodeId, ParamSpec, Value, lock};
use penumbra_gpu::Tex;
use penumbra_gpu::projection::{Correction, Stripes};
use penumbra_graph::{
    CookContext, CookError, Cooked, Cooker, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes,
    Operator,
};
use serde::Serialize;

use crate::mapping::{Mapping, Setup};

/// The projector image size assumed while its display is unknown (headless, not connected).
pub const DEFAULT_RASTER: (u32, u32) = (1920, 1080);

pub const PROJECTOR: &str = "out.projector";
/// The param group of a projector's colour correction.
pub const COLOUR: &str = "colour";

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
    /// The display id as set (`EPSON PJ [SECA519] @ target 168194`); empty when none is chosen.
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
    /// Its colour correction; `None` when off or the identity.
    pub correction: Option<Arc<Correction>>,
    /// What its `gain` and `black` inputs carry (per projector pixel: a factor on the drive, a
    /// lift of the black); `None` when unconnected or not cooked.
    pub gain: Option<Data>,
    pub black: Option<Data>,
    /// How much of them applies (its `flatten` and `lift` params; 0 with `correct` off).
    pub flatten: f32,
    pub lift: f32,
    /// A flat signal shown instead of anything, uncorrected (a measurement running).
    pub flat: Option<[f32; 3]>,
    /// A structured light pattern shown instead of anything but `flat`, uncorrected (a scan
    /// running).
    pub stripes: Option<Stripes>,
}

/// A display as the output side sees it (named by the system crate, sized by winit).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DisplayInfo {
    pub id: String,
    pub monitor: String,
    /// Manufacturer and product from the EDID as a PnP id (`SECA519`).
    pub edid: Option<String>,
    /// The EDID serial number, when the display has a usable one.
    pub serial: Option<String>,
    /// The connector it is on (`target 168194`); empty where the OS reports none.
    pub target: String,
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
    /// The OS is told the displays are required (no sleep) because output windows are open.
    pub keeps_display_awake: bool,
    pub projectors: BTreeMap<NodeId, ProjectorState>,
    /// Where the editor window is (`x, y, w, h` in physical desktop pixels), set by the UI;
    /// projectors are not opened on the display that shows it.
    pub editor: Option<[i32; 4]>,
    /// Flat signals (sRGB-encoded) shown instead of these projectors' pictures, uncorrected:
    /// set by a measurement (`projector.match`) and cleared when it ends.
    #[serde(skip)]
    pub flat: BTreeMap<NodeId, [f32; 3]>,
    /// Structured light patterns shown instead of these projectors' pictures, uncorrected: set
    /// by a scan (`scan.capture`) and cleared when it ends. A flat signal wins over them.
    #[serde(skip)]
    pub stripes: BTreeMap<NodeId, Stripes>,
    /// The newest output pass each projector's window presented.
    #[serde(skip)]
    pub presented: BTreeMap<NodeId, u64>,
}

/// The [`BoardState`] behind a lock; cheap to clone.
#[derive(Clone, Default)]
pub struct OutputBoard(Arc<Mutex<BoardState>>);

impl OutputBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> BoardState {
        lock(&self.0).clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut BoardState)) {
        f(&mut lock(&self.0));
    }

    fn projector(&self, id: NodeId) -> (bool, Option<ProjectorState>) {
        let s = lock(&self.0);
        (s.managed, s.projectors.get(&id).cloned())
    }
}

/// Registers the `out` node types.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let desc = NodeDesc::new(PROJECTOR, "A projector: shows its input fullscreen on a display, or in a part of it")
        .param(
            ParamSpec::text("display", "")
                .doc("Display id from output.list, e.g. `EPSON PJ [SECA519] @ target 168194` (model and connector) or `AW3225QF [DELA246 #BY4BYZ3]` (model and serial number). The monitor name alone works when unique"),
        )
        .param(
            ParamSpec::bool("active", true)
                .doc("Open the output window on its display. Off closes the window and the node does nothing"),
        )
        .param(
            ParamSpec::choice("fit", &["fit", "fill", "stretch"])
                .doc("How the picture sits in the viewport when their aspects differ")
                .choice_doc("fit", "all visible, black bars")
                .choice_doc("fill", "covered, cropped")
                .choice_doc("stretch", "covered, distorted"),
        )
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
        .param(ParamSpec::bool("correct", true).group(COLOUR).doc(
            "Apply the colour correction below. Off shows the plain picture, to compare. The test pattern is never corrected",
        ))
        .param(ParamSpec::float("strength", 1.0).clamp(0.0, 1.0).group(COLOUR).doc(
            "How much of the correction applies, matrix and curves alike: 1 all of it, 0 the projector \
             uncorrected. A lower value keeps more of a brighter projector's brightness, but leaves more \
             of the difference in colour and brightness between projectors",
        ))
        .param(ParamSpec::float("gain", 1.0).clamp(0.0, 1.0).group(COLOUR).doc(
            "Brightness by hand, in linear light, on top of the matrix (0.8: a fifth less light)",
        ))
        .param(ParamSpec::text("matrix", "").multiline().group(COLOUR).doc(
            "3x3 matrix in linear light, drive = matrix x colour. Three rows of three numbers (`1 0 0 / 0 1 0 / 0 0 1`); empty is none. projector.match writes this matrix",
        ))
        .param(ParamSpec::text("curves", "").multiline().group(COLOUR).doc(
            "Tone curve per channel, from drive (sRGB-encoded, evenly spaced from 0 to 1) to the signal sent. Lines `r: 0 0.06 ... 1`, `g: ...`, `b: ...` with 2 to 64 samples each, or one line for all three. A straight line 0..1 or empty is none. projector.match writes these curves",
        ))
        .param(ParamSpec::float("flatten", 1.0).clamp(0.0, 1.0).group(COLOUR).doc(
            "How much of the gain map at the `gain` input applies: 1 all of it (even brightness and colour across the picture), 0 none. projector.flatfield measures the map",
        ))
        .param(ParamSpec::float("lift", 1.0).clamp(0.0, 4.0).group(COLOUR).doc(
            "Scales the black lift at the `black` input: 1 as measured, 0 none. Raise it where the camera underrated the black (black outside an overlap still darker than inside). projector.flatfield measures the lift",
        ))
        .input("in", DataKind::Tex)
        .input("gain", DataKind::Tex)
        .input("black", DataKind::Tex)
        // Not reading the clock; declared so the node picks up what the output side reports
        // (a display plugged in or out) every frame. Cooking it costs nothing.
        .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Projector) as Box<dyn Operator>))?;
    ndi::register(types)?;
    spout::register(types)?;
    video::register(types)
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
        // A broken text is an error here; the outputs keep the last good correction.
        correct::correction(|name| cx.param(name)).map_err(CookError::Failed)?;
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
pub struct Mappings(
    HashMap<NodeId, (PlainKey, Arc<Mapping>)>,
    /// The last good colour correction of each projector, with the params it came from.
    HashMap<NodeId, (Vec<Option<Value>>, CorrectionRef)>,
);

type CorrectionRef = Option<Arc<Correction>>;

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
        let whole = test
            || board
                .as_ref()
                .is_some_and(|b| b.flat.contains_key(&node.id) || b.stripes.contains_key(&node.id));
        let placed = if whole { Some((Fit::Stretch, (1, 1))) } else { size.map(|s| (fit, s)) };
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
        let flat = board.as_ref().and_then(|b| b.flat.get(&node.id).copied());
        let stripes = board.as_ref().and_then(|b| b.stripes.get(&node.id).copied());
        let correction = {
            let key: Vec<Option<Value>> = correct::PARAMS.iter().map(|n| param(n).cloned()).collect();
            match cache.1.get(&node.id) {
                Some((k, c)) if *k == key => c.clone(),
                last => match correct::correction(param) {
                    Ok(c) => {
                        cache.1.insert(node.id, (key, c.clone()));
                        c
                    }
                    Err(_) => last.and_then(|(_, c)| c.clone()),
                },
            }
        };
        let on = !matches!(param("correct"), Some(Value::Bool(false)));
        let amount = |name: &str| match param(name).and_then(Value::as_f64) {
            Some(v) if on && v.is_finite() => v as f32,
            Some(_) => 0.0,
            None if on => 1.0,
            None => 0.0,
        };
        let input =
            |k: usize| node.inputs.get(k).copied().flatten().and_then(|port| cooker.output(port).cloned());
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
            correction,
            gain: input(1),
            black: input(2),
            flatten: amount("flatten"),
            lift: amount("lift"),
            flat,
            stripes,
        });
    }
    let alive: Vec<NodeId> = out.iter().map(|p| p.node).collect();
    cache.0.retain(|id, _| alive.contains(id));
    cache.1.retain(|id, _| alive.contains(id));
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
            correction: None,
            flatten: 0.0,
            lift: 0.0,
            flat: None,
            stripes: None,
        },
        gain: None,
        black: None,
    }
}

/// How a projector draws: its input placed by `fit` into `viewport` (target pixels) and colour
/// corrected, or its test pattern, or a flat signal or structured light stripes (`blank` stands
/// in for the texture, which none of those reads). `None` without an input.
pub fn item<'a>(
    frame: &'a ProjectorFrame,
    blank: &'a Tex,
    viewport: [f32; 4],
) -> Option<penumbra_gpu::projection::Item<'a>> {
    let mapping = frame.mapping.as_deref()?;
    if let Some(flat) = frame.flat {
        let mut item = item_for(blank, mapping, &[], [1.0; 3], viewport);
        item.look.flat = Some(flat);
        return Some(item);
    }
    if let Some(stripes) = frame.stripes {
        let mut item = item_for(blank, mapping, &[], [1.0; 3], viewport);
        item.look.stripes = Some(stripes);
        return Some(item);
    }
    if frame.test {
        let mut item = item_for(blank, mapping, &[], frame.tint, viewport);
        item.look.test = true;
        return Some(item);
    }
    let tex = frame.source.as_ref()?.get::<Tex>()?;
    let mut item = item_for(tex, mapping, &[], [1.0; 3], viewport);
    item.look.correction = frame.correction.clone();
    item.gain = frame.gain.as_ref().and_then(|d| d.get::<Tex>());
    item.black = frame.black.as_ref().and_then(|d| d.get::<Tex>());
    item.look.flatten = frame.flatten;
    item.look.lift = frame.lift;
    Some(item)
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
