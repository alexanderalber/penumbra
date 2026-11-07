//! The "Model mapping" window: fit a projector to a 3D model with no camera. Choose a corner on the
//! model, move a crosshair on the projector onto the real corner, press Enter. From six points on, the
//! fit runs after every point, the next crosshair starts where the fit expects the corner, and the
//! model's lines appear on the real object.
//!
//! The window belongs to one mapped camera: a component from the library with a geometry input, a
//! camera output and a guide picture. The point list lives in its inner `view.camera`, where the
//! `calib.points` commands edit it too. Edits go in as the user's, so they never wait for an edit
//! claim.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, Key, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use penumbra_calib::Camera;
use penumbra_calib::points::{GroupDeviation, PictureKind, Point, PointSet, Quality, Report};
use penumbra_core::{GroupId, Name, NodeId, NodePath, ParamPath, Value, lock};
use penumbra_engine::Engine;
use penumbra_engine::calib_points::{ModelPart, model_parts};
use penumbra_graph::{Network, NodeDef, NodeStatus, NodeTypes, Op};

use crate::{Action, automation, theme};

/// How near a corner the pointer must be to choose it, in points.
const GRAB: f32 = 14.0;
/// Corners this close to the nearest count as level with it, and the one in front wins, in points.
const LEVEL: f32 = 3.0;
/// The crosshair in projector pixels: the free middle and the thickness of each line.
const CROSS_GAP: f64 = 6.0;
const CROSS_LINE: f64 = 2.0;
/// Half the size of the mark of a set point on the object, in projector pixels.
const MARK_ARM: f64 = 9.0;
/// How bright the coloured parts are on the object.
const COLOUR_GAIN: f64 = 0.25;
/// Lines from the point: the classic cross, or enough that some of them reach the object when most
/// of the picture goes past it.
const FEW_LINES: u32 = 4;
const MANY_LINES: u32 = 16;
/// How far a line's end may lie outside the picture, in picture sizes (`tex.shapes` allows ±4).
const CROSS_REACH: f64 = 3.9;
/// Points a fit needs at least.
const MIN_POINTS: usize = 6;
/// The window at its normal size, without its frame, in points. A smaller app makes it smaller.
const NORMAL_SIZE: egui::Vec2 = vec2(1060.0, 720.0);
/// Free space between the window and the edge of the app, in points.
const WINDOW_MARGIN: f32 = 4.0;
/// The smallest room the two columns are laid out for, in points. Below it they are cut off.
const SMALLEST_ROOM: egui::Vec2 = vec2(320.0, 240.0);
/// The projector column takes at most this share of the width, so the model stays usable.
const PAD_SHARE: f32 = 0.62;
/// The projector column is at least this wide for its text and its list, in points.
const RIGHT_MIN: f32 = 460.0;
/// Rows of controls, status and list kept free under the projector picture.
const LOWER_ROWS: f32 = 10.0;
/// Below this column width the row above the projector picture takes two lines, in points.
const ONE_LINE_HEADER: f32 = 640.0;
/// Rows beside the model picture: its title and a hint of up to two lines.
const MODEL_TEXT_ROWS: f32 = 3.0;

/// How the room of the window is shared between the two columns, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Columns {
    model: [f32; 2],
    /// Width of the projector picture.
    pad: f32,
    /// Width of the column it stands in.
    right: f32,
}

impl Columns {
    /// `aspect` is the projector picture's height over its width, `row` the height of a row of
    /// controls, `gap` the space between the columns.
    fn for_room(room: egui::Vec2, aspect: f32, row: f32, gap: f32) -> Self {
        let room = room.max(SMALLEST_ROOM);
        let widest = room.x * PAD_SHARE;
        let aspect = aspect.max(0.01);
        let width_for = |header_rows: f32| {
            ((room.y - row * (header_rows + LOWER_ROWS)).max(row * 3.0) / aspect).min(widest)
        };
        let pad = width_for(1.0);
        // A narrow column breaks the row above the picture, which leaves the picture less height.
        let pad = if pad < ONE_LINE_HEADER { width_for(2.0) } else { pad };
        let right = pad.max(RIGHT_MIN.min(widest));
        Self { model: [room.x - right - gap, room.y - row * MODEL_TEXT_ROWS], pad, right }
    }
}

/// What the window needs besides its own state.
pub struct Input<'a> {
    pub net: &'a Network,
    pub engine: &'a Arc<Engine>,
    pub status: &'a BTreeMap<NodeId, NodeStatus>,
    pub can_edit: bool,
    pub new_group: &'a dyn Fn() -> GroupId,
}

/// The nodes the window works on, read anew each frame.
struct Rig {
    camera: NodePath,
    geo: NodePath,
    cross: Option<NodePath>,
    /// What the crosshair node draws now.
    cross_text: String,
    wire: Option<NodePath>,
    points: String,
    calibration: String,
    /// The camera's own params a fit writes, with their values now.
    placement: Vec<(&'static str, Value)>,
    /// Changes when the geometry coming in changes.
    version: u64,
    /// The picture size a new point list starts with.
    size: [u32; 2],
    wire_color: [f64; 4],
    render: Option<NodePath>,
    dim: Option<NodePath>,
    show: Show,
}

/// What the projector shows of the model.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    Nothing,
    Lines,
    /// Every part in a colour of its own, dim enough that the crosshair stays visible on top.
    Colours,
}

struct Model {
    key: String,
    parts: Vec<ModelPart>,
    centre: [f32; 3],
    radius: f32,
}

/// The corner being placed.
#[derive(Clone)]
struct Pick {
    /// The point being changed; `None` for a new one.
    id: Option<u32>,
    world: [f64; 3],
    group: Option<String>,
    at: [f64; 2],
    /// The crosshair was moved since the corner was chosen. Only then choosing the next corner
    /// sets this one: a crosshair nobody placed says nothing about the object.
    moved: bool,
    /// The edits that put everything back as it was when the corner was chosen, for a point
    /// already in the list.
    restore: Vec<Op>,
    /// A move of a point already in the list has gone into the project.
    written: bool,
}

/// What the list of points says, kept until the list changes.
struct Fit {
    text: String,
    set: Option<PointSet>,
    outcome: Result<Judged, String>,
}

struct Judged {
    report: Report,
    deviation: Vec<GroupDeviation>,
    quality: Option<Quality>,
}

/// How the model is turned in the window.
#[derive(Clone, Copy)]
struct View {
    yaw: f32,
    pitch: f32,
    zoom: f32,
    pan: [f32; 2],
}

impl Default for View {
    fn default() -> Self {
        Self { yaw: 0.0, pitch: 0.0, zoom: 1.0, pan: [0.0, 0.0] }
    }
}

impl View {
    /// `p` turned about `centre`: x to the right, y away from the viewer, z up.
    fn turn(&self, p: [f32; 3], centre: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let (x1, y1) = (x * cy - y * sy, x * sy + y * cy);
        [x1, y1 * cp - z * sp, y1 * sp + z * cp]
    }

    /// Where `p` lands in `rect`, and how far away it is.
    fn place(&self, p: [f32; 3], model: &Model, rect: Rect) -> (Pos2, f32) {
        let [x, depth, z] = self.turn(p, model.centre);
        let scale = self.zoom * rect.width().min(rect.height()) * 0.46 / model.radius.max(1e-6);
        (rect.center() + vec2(self.pan[0] + x * scale, self.pan[1] - z * scale), depth)
    }
}

/// A line of `CROSS_LINE` pixels from `a` to `b` for `tex.shapes`.
fn line_shape(a: [f64; 2], b: [f64; 2], size: [u32; 2]) -> String {
    let [w, h] = size.map(f64::from);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx.hypot(dy).max(1e-9);
    let (nx, ny) = (-dy / length * CROSS_LINE / 2.0, dx / length * CROSS_LINE / 2.0);
    let point = |px: f64, py: f64| {
        let (u, v) = ((px / w).clamp(-CROSS_REACH, CROSS_REACH), (py / h).clamp(-CROSS_REACH, CROSS_REACH));
        format!("{u:.6},{v:.6}")
    };
    format!(
        "cross poly {} {} {} {}",
        point(a[0] + nx, a[1] + ny),
        point(b[0] + nx, b[1] + ny),
        point(b[0] - nx, b[1] - ny),
        point(a[0] - nx, a[1] - ny)
    )
}

/// The crosshair for `tex.shapes`: `lines` thin lines from a free middle, so the real corner stays
/// visible, each running past the edge of the picture. A line that reaches the object points back
/// at the crosshair, wherever on the picture that lies.
fn cross_shapes(at: [f64; 2], size: [u32; 2], lines: u32) -> Vec<String> {
    let [w, h] = size.map(f64::from);
    let [x, y] = at;
    let length = w.hypot(h);
    (0..lines)
        .map(|i| {
            let (dy, dx) = (f64::from(i) * std::f64::consts::TAU / f64::from(lines)).sin_cos();
            line_shape([x + dx * CROSS_GAP, y + dy * CROSS_GAP], [x + dx * length, y + dy * length], size)
        })
        .collect()
}

/// A small plus at every point in the fit, and a line from it to where the fit puts its corner: the
/// longer the line, the more that point disagrees with the others. `skip` is the point under the
/// crosshair.
fn point_marks(set: &PointSet, camera: Option<&Camera>, skip: Option<u32>) -> Vec<String> {
    let mut out = Vec::new();
    for p in set.points.iter().filter(|p| !p.off && Some(p.id) != skip) {
        let [x, y] = p.picture;
        out.push(line_shape([x - MARK_ARM, y], [x + MARK_ARM, y], set.size));
        out.push(line_shape([x, y - MARK_ARM], [x, y + MARK_ARM], set.size));
        if let Some(q) = camera.and_then(|c| c.project(p.world))
            && (q[0] - x).hypot(q[1] - y) > 1.0
        {
            out.push(line_shape(p.picture, q, set.size));
        }
    }
    out
}

fn to_f32(p: [f64; 3]) -> [f32; 3] {
    p.map(|v| v as f32)
}

fn set_op(path: &NodePath, name: &str, value: Value) -> Option<Op> {
    let param = ParamPath::new(path.clone(), Name::new(name).ok()?).ok()?;
    Some(Op::Set { param, value })
}

/// The node `name` of `kind` directly inside the component `id`.
fn child<'a>(net: &'a Network, id: NodeId, kind: &str, name: &str) -> Option<&'a Arc<NodeDef>> {
    let suffix = format!("/{name}");
    net.children(Some(id))
        .find(|n| n.kind == kind && net.path_of(n.id).is_some_and(|p| p.to_string().ends_with(&suffix)))
}

fn text_param(types: &NodeTypes, node: &NodeDef, name: &str) -> String {
    types
        .desc(&node.kind)
        .and_then(|d| node.param(d, name))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The edits that clear the crosshair of every mapped camera except `open_on`. A crosshair the
/// window no longer drives would stay on the object, for example from a project saved during a pick.
fn stray_crosshairs(net: &Network, types: &NodeTypes, open_on: Option<NodeId>) -> Vec<Op> {
    net.nodes()
        .filter(|n| n.kind == "tex.shapes" && n.parent != open_on)
        .filter(|n| n.parent.is_some_and(|parent| is_mapped(net, parent)))
        .filter(|n| net.path_of(n.id).is_some_and(|p| p.to_string().ends_with("/crosshair")))
        .filter(|n| !text_param(types, n, "shapes").is_empty())
        .filter_map(|n| set_op(&net.path_of(n.id)?, "shapes", Value::Text(String::new())))
        .collect()
}

/// Whether `id` is a mapped camera, the component this window works on.
pub fn is_mapped(net: &Network, id: NodeId) -> bool {
    net.node(id).is_some_and(|n| n.kind == "comp.base")
        && child(net, id, "view.camera", "camera").is_some()
        && child(net, id, "tex.shapes", "crosshair").is_some()
}

/// Whether the exposed param `name` of the mapped camera `id` is shown greyed. A calibration
/// replaces all of these; a fit is always a perspective.
pub fn greyed(net: &Network, types: &NodeTypes, id: NodeId, name: &str) -> bool {
    if !is_mapped(net, id) {
        return false;
    }
    if name == "projection" {
        return true;
    }
    ["position", "target", "fov", "size", "roll", "shift"].contains(&name)
        && child(net, id, "view.camera", "camera")
            .is_some_and(|c| !text_param(types, c, "calibration").trim().is_empty())
}

/// A param's value as read-only text.
pub fn value_text(value: Option<&Value>) -> String {
    let join = |v: &[f64]| v.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>().join(", ");
    match value {
        Some(Value::Float(v)) => format!("{v:.3}"),
        Some(Value::Vec2(v)) => join(v),
        Some(Value::Vec3(v)) => join(v),
        Some(Value::Vec4(v)) => join(v),
        Some(Value::Text(v)) => v.clone(),
        Some(Value::Int(v)) => v.to_string(),
        Some(Value::Bool(v)) => v.to_string(),
        None => String::new(),
    }
}

/// The buttons of a mapped camera in the parameter panel.
pub fn params_row(
    ui: &mut Ui,
    net: &Network,
    types: &NodeTypes,
    id: NodeId,
    can_edit: bool,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) {
    let camera = child(net, id, "view.camera", "camera");
    let anything = camera.is_some_and(|c| {
        !text_param(types, c, "calibration").trim().is_empty()
            || !text_param(types, c, "points").trim().is_empty()
    });
    ui.horizontal(|ui| {
        let r = ui.add_enabled(can_edit, egui::Button::new("Open mapping")).on_hover_text(
            "Fit this camera to the projector: choose a corner on the model, move a crosshair onto the real corner",
        );
        auto.add("param.open_mapping", r.rect, "Open mapping");
        if r.clicked() {
            actions.push(Action::OpenModelMap(id));
        }
        let r = ui.add_enabled(can_edit && anything, egui::Button::new("Discard fit")).on_hover_text(
            "Deletes all points and the fit, and the camera starts over. Undo brings them back",
        );
        auto.add("param.discard_fit", r.rect, "Discard fit");
        if r.clicked() {
            actions.push(Action::Edit { ops: discard_ops(net, types, id), label: "discard fit".into(), group: None });
        }
    });
}

/// The edits that take the mapped camera `id` back to the start: no points, no fit, the camera at
/// its defaults and nothing of the model on the projector.
fn discard_ops(net: &Network, types: &NodeTypes, id: NodeId) -> Vec<Op> {
    let path_of = |kind: &str, name: &str| child(net, id, kind, name).and_then(|n| net.path_of(n.id));
    let mut ops: Vec<Op> = Vec::new();
    if let Some(camera) = path_of("view.camera", "camera") {
        for name in ["points", "calibration", "position", "target", "fov"] {
            ops.extend(
                Name::new(name)
                    .ok()
                    .and_then(|n| ParamPath::new(camera.clone(), n).ok())
                    .map(|param| Op::Reset { param }),
            );
        }
    }
    if let Some(node) = child(net, id, "mat.wireframe", "model_wire")
        && let Some(path) = net.path_of(node.id)
        && let Some(Value::Vec4([r, g, b, _])) = types.desc(&node.kind).and_then(|d| node.param(d, "color"))
    {
        ops.extend(set_op(&path, "color", Value::Vec4([*r, *g, *b, 0.0])));
    }
    if let Some(render) = path_of("tex.render", "render") {
        ops.extend(set_op(&render, "surface", Value::Text("auto".to_string())));
    }
    if let Some(dim) = path_of("tex.level", "dim") {
        ops.extend(set_op(&dim, "brightness", Value::Float(1.0)));
    }
    ops
}

fn find_rig(input: &Input<'_>, id: NodeId) -> Result<Rig, String> {
    let types = input.engine.types();
    let net = input.net;
    if !is_mapped(net, id) {
        return Err("This mapped camera is gone.".to_string());
    }
    let camera = child(net, id, "view.camera", "camera").ok_or("The camera is missing.")?;
    let geo = child(net, id, "geo.in", "geo").ok_or("The geometry input is missing.")?;
    let cross = child(net, id, "tex.shapes", "crosshair").and_then(|n| net.path_of(n.id));
    let wire = child(net, id, "mat.wireframe", "model_wire");
    let render = child(net, id, "tex.render", "render");
    let number = |name: &str| {
        render
            .and_then(|n| types.desc(&n.kind).and_then(|d| n.param(d, name)))
            .and_then(|v| if let Value::Int(i) = v { u32::try_from(*i).ok() } else { None })
    };
    let size = [number("width").unwrap_or(1920), number("height").unwrap_or(1080)];
    let wire_color = wire
        .and_then(|n| types.desc(&n.kind).and_then(|d| n.param(d, "color")))
        .and_then(|v| if let Value::Vec4(c) = v { Some(*c) } else { None })
        .unwrap_or([1.0; 4]);
    let coloured = render.is_some_and(|n| text_param(types, n, "surface") == "parts");
    let show = if coloured {
        Show::Colours
    } else if wire_color[3] > 0.0 {
        Show::Lines
    } else {
        Show::Nothing
    };
    let cross_text =
        child(net, id, "tex.shapes", "crosshair").map_or(String::new(), |n| text_param(types, n, "shapes"));
    let placement = ["position", "target", "fov"]
        .into_iter()
        .filter_map(|name| {
            Some((name, types.desc(&camera.kind).and_then(|d| camera.param(d, name))?.clone()))
        })
        .collect();
    Ok(Rig {
        camera: net.path_of(camera.id).ok_or("The camera has no path.")?,
        geo: net.path_of(geo.id).ok_or("The input has no path.")?,
        cross,
        cross_text,
        wire: wire.and_then(|n| net.path_of(n.id)),
        points: text_param(types, camera, "points"),
        calibration: text_param(types, camera, "calibration"),
        placement,
        version: input.status.get(&geo.id).map_or(0, |s| s.version),
        size,
        wire_color,
        render: render.and_then(|n| net.path_of(n.id)),
        dim: child(net, id, "tex.level", "dim").and_then(|n| net.path_of(n.id)),
        show,
    })
}

/// The edits that bring back the point list and all that a fit of it writes, as `rig` has them.
fn restore_ops(rig: &Rig) -> Vec<Op> {
    let mut ops: Vec<Op> = [("points", &rig.points), ("calibration", &rig.calibration)]
        .into_iter()
        .filter_map(|(name, text)| set_op(&rig.camera, name, Value::Text(text.clone())))
        .collect();
    ops.extend(rig.placement.iter().filter_map(|(name, value)| set_op(&rig.camera, name, value.clone())));
    if let Some(wire) = &rig.wire {
        ops.extend(set_op(wire, "color", Value::Vec4(rig.wire_color)));
    }
    ops
}

/// The edits that make the projector show `show`.
fn show_ops(rig: &Rig, show: Show) -> Vec<Op> {
    let [r, g, b, _] = rig.wire_color;
    let mut ops = Vec::new();
    if let Some(wire) = &rig.wire {
        let alpha = if show == Show::Nothing { 0.0 } else { 1.0 };
        ops.extend(set_op(wire, "color", Value::Vec4([r, g, b, alpha])));
    }
    if let Some(render) = &rig.render {
        let surface = if show == Show::Colours { "parts" } else { "auto" };
        ops.extend(set_op(render, "surface", Value::Text(surface.to_string())));
    }
    if let Some(dim) = &rig.dim {
        let gain = if show == Show::Colours { COLOUR_GAIN } else { 1.0 };
        ops.extend(set_op(dim, "brightness", Value::Float(gain)));
    }
    ops
}

fn load_model(engine: &Engine, geo: &NodePath, key: String) -> Result<Model, String> {
    let parts = model_parts(engine, geo).map_err(|e| e.0)?;
    let mut low = [f32::MAX; 3];
    let mut high = [f32::MIN; 3];
    for [a, b] in parts.iter().flat_map(|p| p.edges.iter()) {
        for p in [a, b] {
            for ((l, h), v) in low.iter_mut().zip(high.iter_mut()).zip(p) {
                *l = l.min(*v);
                *h = h.max(*v);
            }
        }
    }
    if low[0] > high[0] {
        return Err("The model has no lines to draw.".to_string());
    }
    let centre = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0, (low[2] + high[2]) / 2.0];
    let radius = (high[0] - low[0]).max(high[1] - low[1]).max(high[2] - low[2]) / 2.0;
    Ok(Model { key, parts, centre, radius })
}

fn quality_word(q: Quality) -> &'static str {
    match q {
        Quality::Good => "good",
        Quality::Fair => "fair",
        Quality::Weak => "weak",
        Quality::Unusable => "unusable",
    }
}

type Loading = Arc<Mutex<Option<(String, Result<Model, String>)>>>;

/// How long a geometry that keeps changing or failing waits before it is read again.
const RELOAD: Duration = Duration::from_secs(1);
/// How often other mapped cameras are checked for a crosshair left on.
const SWEEP: Duration = Duration::from_secs(1);

/// What a row of the point list asked for.
enum RowEvent {
    Change(Point),
    Toggle(u32, bool),
    Remove(u32),
}

#[derive(Default)]
pub struct ModelMap {
    pub open: bool,
    /// The mapped camera the window is open on.
    target: Option<NodeId>,
    model: Option<Arc<Model>>,
    model_error: Option<String>,
    /// A model being read on its own thread: cooking waits for the cook thread, too long for a frame.
    loading: Option<Loading>,
    /// The geometry that failed to read and when it was last tried.
    tried: Option<(String, Instant)>,
    view: View,
    pick: Option<Pick>,
    /// Where the last crosshair ended, the start of the next while there is no fit.
    last_at: Option<[f64; 2]>,
    fit: Option<Fit>,
    /// When other mapped cameras were last checked for a crosshair left on.
    swept: Option<Instant>,
    /// Sixteen lines from the crosshair in place of four.
    many_lines: bool,
    /// Every set point is marked on the object.
    show_points: bool,
    /// Tab or Shift+Tab pressed this frame: 1, -1 or 0.
    tab: i32,
    /// The window fills the app.
    large: bool,
    columns: Columns,
    /// What the window's title and border add to its content, in points, as measured last frame.
    frame: egui::Vec2,
    /// The undo step the moves of the point being changed belong to.
    group: Option<GroupId>,
}

impl ModelMap {
    /// Opens the window on the mapped camera `node`.
    pub fn open_on(&mut self, node: NodeId) {
        if self.target != Some(node) {
            self.target = Some(node);
            self.pick = None;
            self.model = None;
            self.model_error = None;
            self.fit = None;
            self.tried = None;
        }
        self.open = true;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        input: &Input<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if input.can_edit && self.swept.is_none_or(|at| at.elapsed() >= SWEEP) {
            self.swept = Some(Instant::now());
            let open_on = self.target.filter(|_| self.open);
            let ops = stray_crosshairs(input.net, input.engine.types(), open_on);
            if !ops.is_empty() {
                actions.push(Action::SetLive(ops));
            }
        }
        let Some(target) = self.target else { return };
        let rig = find_rig(input, target);
        if !self.open {
            // A crosshair must not stay on the object after the window closes.
            self.pick = None;
            self.group = None;
            if let Ok(rig) = &rig {
                self.sync_cross(rig, input, actions);
            }
            return;
        }
        if let Ok(rig) = &rig {
            self.refresh(rig, input);
            if input.can_edit {
                self.keys(ctx, rig, input, actions);
            }
        }
        let mut open = self.open;
        let area = ctx.content_rect();
        let room = (area.size() - self.frame - egui::Vec2::splat(2.0 * WINDOW_MARGIN)).max(SMALLEST_ROOM);
        let inner = if self.large { room } else { room.min(NORMAL_SIZE) };
        // One id per size, so the normal window comes back to where it was.
        let id = if self.large { "model_map_large" } else { "model_map_normal" };
        let mut window = egui::Window::new("Model mapping")
            .id(egui::Id::new(id))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size(inner);
        if self.large {
            window = window.fixed_pos(area.min + egui::Vec2::splat(WINDOW_MARGIN));
        }
        let r = window.show(ctx, |ui| match &rig {
            Err(message) => {
                ui.colored_label(theme::WARNING, message);
            }
            Ok(rig) => self.body(ui, rig, input, auto, actions),
        });
        if let Some(r) = r {
            auto.add("model_map", r.response.rect, "Model mapping");
            self.frame = (r.response.rect.size() - inner).max(egui::Vec2::ZERO);
        }
        self.open = open;
        if let Ok(rig) = &rig {
            self.sync_cross(rig, input, actions);
        }
    }

    /// Reads the model and fits the list again when they changed.
    fn refresh(&mut self, rig: &Rig, input: &Input<'_>) {
        if let Some(slot) = &self.loading {
            let done = lock(slot).take();
            if let Some((_, result)) = done {
                self.loading = None;
                match result {
                    Ok(model) => {
                        self.model = Some(Arc::new(model));
                        self.model_error = None;
                        self.fit = None;
                    }
                    Err(e) => self.model_error = Some(e),
                }
            }
        }
        let key = format!("{}|{}", rig.geo, rig.version);
        let current = self.model.as_ref().is_some_and(|m| m.key == key);
        let waited = self.tried.as_ref().is_none_or(|(k, at)| *k != key || at.elapsed() >= RELOAD);
        if !current && self.loading.is_none() && waited {
            self.tried = Some((key.clone(), Instant::now()));
            let slot: Loading = Arc::new(Mutex::new(None));
            let (engine, geo, out) = (Arc::clone(input.engine), rig.geo.clone(), Arc::clone(&slot));
            std::thread::spawn(move || {
                let result = load_model(&engine, &geo, key.clone());
                *lock(&out) = Some((key, result));
            });
            self.loading = Some(slot);
        }
        if self.fit.as_ref().is_none_or(|f| f.text != rig.points) {
            let previous = serde_json::from_str::<Camera>(&rig.calibration).ok();
            let fit = self.judge(rig, previous.as_ref());
            // The list changed under the crosshair, by undo or by discarding the fit.
            if let (Some(set), Some(id)) = (&fit.set, self.pick.as_ref().and_then(|p| p.id))
                && !set.points.iter().any(|p| p.id == id)
            {
                self.pick = None;
                self.group = None;
            }
            if fit.set.as_ref().is_some_and(|set| set.points.is_empty()) {
                self.last_at = None;
            }
            self.fit = Some(fit);
        }
    }

    fn judge(&self, rig: &Rig, previous: Option<&Camera>) -> Fit {
        let text = rig.points.as_str();
        // A camera that was never mapped starts with an empty list of its picture's size.
        let set: Option<PointSet> = if text.trim().is_empty() {
            Some(PointSet::new(rig.size, PictureKind::Projector))
        } else {
            serde_json::from_str(text).ok()
        };
        let outcome = match &set {
            None => Err("The point list does not read.".to_string()),
            Some(set) => {
                let active = set.points.iter().filter(|p| !p.off).count();
                if active < MIN_POINTS {
                    Err(format!("{active} of {MIN_POINTS} points: the fit starts at {MIN_POINTS}."))
                } else {
                    set.fit_from(previous).map_err(|e| e.0).map(|report| {
                        let groups: BTreeMap<String, Vec<[f64; 3]>> = self
                            .model
                            .iter()
                            .flat_map(|m| m.parts.iter())
                            .filter(|p| !p.corners.is_empty())
                            .map(|p| (p.name.clone(), p.corners.clone()))
                            .collect();
                        let judged = set.deviation(&report.solution.camera, &groups, Some(report.noise));
                        let (deviation, quality) = match judged {
                            Ok((d, q)) => (d, Some(q)),
                            Err(_) => (Vec::new(), None),
                        };
                        Judged { report, deviation, quality }
                    })
                }
            }
        };
        Fit { text: text.to_string(), set, outcome }
    }

    /// Takes Tab before the editor sees it, which would open its add dialog.
    pub fn take_tab(&mut self, ctx: &egui::Context) {
        if !self.open || ctx.text_edit_focused() {
            return;
        }
        self.tab = ctx.input_mut(|i| {
            // Shift first: `consume_key` matches logically, so without Shift it would take both.
            if i.consume_key(Modifiers::SHIFT, Key::Tab) {
                -1
            } else if i.consume_key(Modifiers::NONE, Key::Tab) {
                1
            } else {
                0
            }
        });
    }

    fn keys(&mut self, ctx: &egui::Context, rig: &Rig, input: &Input<'_>, actions: &mut Vec<Action>) {
        if ctx.text_edit_focused() {
            return;
        }
        // The keys belong to the crosshair: a button that took the focus would answer to Enter.
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        let step = std::mem::take(&mut self.tab);
        if step != 0 {
            self.tab_to(step, rig, actions);
        }
        if self.pick.is_none() {
            return;
        }
        let (dx, dy) = ctx.input_mut(crate::mapping::arrow_nudge);
        if dx != 0.0 || dy != 0.0 {
            self.move_cross(dx, dy);
            self.write_moved(rig, input.new_group, actions);
        }
        let (enter, escape) = ctx.input_mut(|i| {
            (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape))
        });
        if enter {
            self.place_pick(rig, actions);
        }
        if escape {
            self.cancel_pick(actions);
        }
    }

    /// A point already in the list moves at once, and the lines on the object follow.
    fn write_moved(&mut self, rig: &Rig, new_group: &dyn Fn() -> GroupId, actions: &mut Vec<Action>) {
        let Some(pick) = self.pick.clone().filter(|p| p.id.is_some()) else { return };
        let group = *self.group.get_or_insert_with(new_group);
        self.write_pick(&pick, rig, Some(group), actions);
        if let Some(pick) = &mut self.pick {
            pick.written = true;
        }
    }

    /// The crosshair goes, and a point it moved returns to where it was.
    fn cancel_pick(&mut self, actions: &mut Vec<Action>) {
        if let Some(pick) = self.pick.take().filter(|p| p.written) {
            actions.push(Action::Edit {
                ops: pick.restore,
                label: "calibration point".into(),
                group: self.group,
            });
        }
        self.group = None;
    }

    /// Goes to the next (`step` 1) or the previous (-1) point in the fit.
    fn tab_to(&mut self, step: i32, rig: &Rig, actions: &mut Vec<Action>) {
        let Some(set) = self.fit.as_ref().and_then(|f| f.set.clone()) else { return };
        let points: Vec<&Point> = set.points.iter().filter(|p| !p.off).collect();
        if points.is_empty() {
            return;
        }
        let count = points.len() as i32;
        let current = self.pick.as_ref().and_then(|c| points.iter().position(|p| Some(p.id) == c.id));
        let next = match current {
            Some(i) => (i as i32 + step).rem_euclid(count),
            None if step > 0 => 0,
            None => count - 1,
        };
        if let Some(p) = points.get(next as usize) {
            self.start_pick(rig, p.world, p.group.clone(), Some((p.id, p.picture)), actions);
        }
    }

    fn size(&self) -> [u32; 2] {
        self.fit.as_ref().and_then(|f| f.set.as_ref()).map_or([1920, 1080], |s| s.size)
    }

    fn move_cross(&mut self, dx: f64, dy: f64) {
        let [w, h] = self.size().map(f64::from);
        if let Some(pick) = &mut self.pick
            && (dx != 0.0 || dy != 0.0)
        {
            pick.at = [(pick.at[0] + dx).clamp(0.0, w), (pick.at[1] + dy).clamp(0.0, h)];
            pick.moved = true;
        }
    }

    /// Starts placing a corner, after setting the one before if its crosshair was moved. A corner
    /// that already has a point changes that point. A changed point keeps its place, a new one
    /// starts where the fit expects it.
    fn start_pick(
        &mut self,
        rig: &Rig,
        world: [f64; 3],
        group: Option<String>,
        known: Option<(u32, [f64; 2])>,
        actions: &mut Vec<Action>,
    ) {
        if self.pick.as_ref().is_some_and(|p| p.world == world) {
            return;
        }
        let existing = self
            .fit
            .as_ref()
            .and_then(|f| f.set.as_ref())
            .and_then(|set| set.points.iter().find(|p| p.world == world).map(|p| (p.id, p.picture)));
        let known = known.or(existing);
        if self.pick.as_ref().is_some_and(|p| p.moved) {
            self.place_pick(rig, actions);
        }
        let [w, h] = self.size().map(f64::from);
        let expected = serde_json::from_str::<Camera>(&rig.calibration).ok().and_then(|c| c.project(world));
        let at = known.map(|(_, at)| at).or(expected).or(self.last_at).unwrap_or([w / 2.0, h / 2.0]);
        let at = [at[0].clamp(0.0, w), at[1].clamp(0.0, h)];
        let restore = if known.is_some() { restore_ops(rig) } else { Vec::new() };
        self.pick = Some(Pick {
            id: known.map(|(id, _)| id),
            world,
            group,
            at,
            moved: false,
            restore,
            written: false,
        });
        self.group = None;
    }

    /// The crosshair's place becomes a point of the list, and the crosshair goes. A point already
    /// in the list went in with each move.
    fn place_pick(&mut self, rig: &Rig, actions: &mut Vec<Action>) {
        let Some(pick) = self.pick.take() else { return };
        self.last_at = Some(pick.at);
        if pick.id.is_none() {
            self.write_pick(&pick, rig, None, actions);
        }
        self.group = None;
    }

    fn write_pick(&self, pick: &Pick, rig: &Rig, group: Option<GroupId>, actions: &mut Vec<Action>) {
        let Some(mut next) = self.fit.as_ref().and_then(|f| f.set.clone()) else { return };
        match pick.id {
            Some(id) => {
                if let Some(p) = next.points.iter_mut().find(|p| p.id == id) {
                    p.picture = pick.at;
                }
            }
            None => {
                let id = next.next_id();
                next.points.push(Point {
                    id,
                    world: pick.world,
                    picture: pick.at,
                    group: pick.group.clone(),
                    off: false,
                });
            }
        }
        self.commit(rig, &next, "calibration point", group, actions);
    }

    /// Writes the list and the fit it gives as one undo step, so undo takes both back.
    fn commit(
        &self,
        rig: &Rig,
        set: &PointSet,
        label: &str,
        group: Option<GroupId>,
        actions: &mut Vec<Action>,
    ) {
        let Ok(text) = serde_json::to_string(set) else { return };
        let mut ops: Vec<Op> = set_op(&rig.camera, "points", Value::Text(text)).into_iter().collect();
        let active = set.points.iter().filter(|p| !p.off).count();
        let previous = serde_json::from_str::<Camera>(&rig.calibration).ok();
        let fitted = if active >= MIN_POINTS { set.fit_from(previous.as_ref()).ok() } else { None };
        let calibration = fitted.as_ref().and_then(|r| serde_json::to_string(&r.solution.camera).ok());
        // The camera's own params show where the fit puts it.
        if let Some(r) = &fitted {
            let [x, y, z] = r.pose.position;
            let [fx, fy, fz] = r.pose.forward;
            let target = [x + fx * r.pose.distance, y + fy * r.pose.distance, z + fz * r.pose.distance];
            let lens = &r.solution.camera.intrinsics;
            let fov = 2.0 * (f64::from(lens.size[1]) / 2.0 / lens.focal[1]).atan().to_degrees();
            ops.extend(set_op(&rig.camera, "position", Value::Vec3(r.pose.position)));
            ops.extend(set_op(&rig.camera, "target", Value::Vec3(target)));
            ops.extend(set_op(&rig.camera, "fov", Value::Float(fov)));
        }
        // A fit that fails keeps the last one: the lines must not vanish while points are tuned.
        if let Some(text) = calibration {
            // The lines appear with the first fit: before it they would land anywhere.
            if rig.calibration.trim().is_empty()
                && let Some(wire) = &rig.wire
            {
                let [r, g, b, _] = rig.wire_color;
                ops.extend(set_op(wire, "color", Value::Vec4([r, g, b, 1.0])));
            }
            ops.extend(set_op(&rig.camera, "calibration", Value::Text(text)));
        } else if set.points.is_empty() {
            ops.extend(set_op(&rig.camera, "calibration", Value::Text(String::new())));
        }
        actions.push(Action::Edit { ops, label: label.to_string(), group });
    }

    fn sync_cross(&mut self, rig: &Rig, input: &Input<'_>, actions: &mut Vec<Action>) {
        let lines = if self.many_lines { MANY_LINES } else { FEW_LINES };
        let mut shapes = self.pick.as_ref().map_or(Vec::new(), |p| cross_shapes(p.at, self.size(), lines));
        if self.open
            && self.show_points
            && let Some(fit) = &self.fit
            && let Some(set) = &fit.set
        {
            let camera = fit.outcome.as_ref().ok().map(|j| &j.report.solution.camera);
            shapes.extend(point_marks(set, camera, self.pick.as_ref().and_then(|p| p.id)));
        }
        let wanted = shapes.join("\n");
        if wanted == rig.cross_text || !input.can_edit {
            return;
        }
        // Outside the history: undo is for the points, and a crosshair it brought back would stay.
        if let Some(cross) = &rig.cross
            && let Some(op) = set_op(cross, "shapes", Value::Text(wanted))
        {
            actions.push(Action::SetLive(vec![op]));
        }
    }

    fn body(
        &mut self,
        ui: &mut Ui,
        rig: &Rig,
        input: &Input<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if rig.cross.is_none() {
            ui.colored_label(
                theme::WARNING,
                "No tex.shapes named crosshair: nothing shows on the projector.",
            );
        }
        if let Some(e) = &self.model_error {
            ui.colored_label(theme::ERROR, e);
        }
        let r = ui.button(if self.large { "Normal size" } else { "Fill the app" });
        auto.add("model_map.large", r.rect, "Fill the app");
        if r.clicked() {
            self.large = !self.large;
        }
        let room = ui.available_size();
        let [w, h] = self.size().map(|v| v as f32);
        let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        self.columns = Columns::for_room(room, h / w.max(1.0), row, ui.spacing().item_spacing.x);
        let columns = self.columns;
        ui.add_enabled_ui(input.can_edit, |ui| {
            // A plain row is only as tall as its tallest finished part; this one offers all the
            // height there is, so the list of points reaches the bottom.
            ui.allocate_ui_with_layout(room, egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                ui.vertical(|ui| {
                    ui.set_width(columns.model[0]);
                    self.model_side(ui, rig, auto, actions);
                });
                ui.vertical(|ui| {
                    ui.set_width(columns.right);
                    self.projector_side(ui, rig, input, auto, actions);
                });
            });
        });
    }

    fn model_side(
        &mut self,
        ui: &mut Ui,
        rig: &Rig,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let Some(model) = self.model.clone() else {
            ui.label("Loading the model.");
            return;
        };
        ui.label(RichText::new("1. Choose a corner").strong());
        let (rect, response) = ui.allocate_exact_size(self.columns.model.into(), Sense::click_and_drag());
        auto.add("model_map.model", rect, "Model");
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_gray(18));

        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.view.yaw += d.x * 0.01;
            self.view.pitch = (self.view.pitch + d.y * 0.01).clamp(-1.5, 1.5);
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
        {
            let d = response.drag_delta();
            self.view.pan = [self.view.pan[0] + d.x, self.view.pan[1] + d.y];
        }
        if response.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            self.view.zoom = (self.view.zoom * (wheel * 0.002).exp()).clamp(0.3, 30.0);
        }
        let view = self.view;
        let parts: &[ModelPart] = &model.parts;
        let (corners, owners): (Vec<Corner>, Vec<usize>) = parts
            .iter()
            .enumerate()
            .flat_map(|(i, part)| part.corners.iter().map(move |w| (i, *w)))
            .map(|(i, w)| {
                let (at, depth) = view.place(to_f32(w), &model, rect);
                ((w, at, depth), i)
            })
            .unzip();
        let hover = response.hover_pos().and_then(|p| nearest_corner(&corners, p));
        // The part under the pointer stands out, so it is clear what a corner belongs to.
        let lit = hover.and_then(|i| owners.get(i).copied());
        for (i, part) in parts.iter().enumerate() {
            let stroke = Stroke::new(1.0, Color32::from_gray(if lit == Some(i) { 240 } else { 130 }));
            for [a, b] in &part.edges {
                let (pa, _) = view.place(*a, &model, rect);
                let (pb, _) = view.place(*b, &model, rect);
                painter.line_segment([pa, pb], stroke);
            }
        }
        for (_, at, _) in &corners {
            painter.circle_filled(*at, 2.5, theme::ACCENT);
        }
        if let Some(i) = hover
            && let Some((_, at, _)) = corners.get(i)
        {
            painter.circle_stroke(*at, 7.0, Stroke::new(1.5, Color32::WHITE));
        }
        if response.clicked()
            && let Some(i) = hover
            && let Some((world, _, _)) = corners.get(i)
        {
            let group = owners.get(i).and_then(|o| parts.get(*o)).map(|p| p.name.clone());
            self.start_pick(rig, *world, group, None, actions);
        }
        // The points already set, and the corner being placed.
        if let Some(set) = self.fit.as_ref().and_then(|f| f.set.as_ref()) {
            for p in &set.points {
                let (at, _) = view.place(to_f32(p.world), &model, rect);
                let color = if p.off { theme::TEXT_WEAK } else { Color32::from_rgb(90, 220, 120) };
                painter.circle_filled(at, 4.0, color);
            }
        }
        if let Some(pick) = &self.pick {
            let (at, _) = view.place(to_f32(pick.world), &model, rect);
            painter.circle_stroke(at, 9.0, Stroke::new(2.0, theme::WARNING));
        }
        ui.label(
            RichText::new(
                "Click: choose. Drag: turn. Right drag: move. Wheel: zoom. Turn it to reach a back edge.",
            )
            .color(theme::TEXT_WEAK),
        );
    }

    fn projector_side(
        &mut self,
        ui: &mut Ui,
        rig: &Rig,
        input: &Input<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("2. Move the crosshair onto the real corner").strong());
            let r = ui.checkbox(&mut self.many_lines, "16 lines").on_hover_text(
                "Sixteen lines from the point in place of four: when most of the picture goes past the object, some still land on it and point at the crosshair",
            );
            auto.add("model_map.many_lines", r.rect, "16 lines");
            let r = ui.checkbox(&mut self.show_points, "All points on the object").on_hover_text(
                "A small plus at every set point, and a line to where the fit puts its corner: a long line is a point to look at",
            );
            auto.add("model_map.show_points", r.rect, "All points on the object");
        });
        let size = self.size();
        let [w, h] = size.map(|v| v as f32);
        let pad_size = vec2(self.columns.pad, self.columns.pad * h / w.max(1.0));
        let (rect, response) = ui.allocate_exact_size(pad_size, Sense::click_and_drag());
        auto.add("model_map.pad", rect, "Projector picture");
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_gray(18));
        painter.rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_gray(70)), egui::StrokeKind::Inside);
        let scale = rect.width() / w.max(1.0);
        let to_pad = |p: [f64; 2]| rect.min + vec2(p[0] as f32 * scale, p[1] as f32 * scale);

        let held = self.pick.as_ref().map(|p| p.at);
        if self.pick.is_some() {
            if response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                let at = (p - rect.min) / scale;
                if let Some(pick) = &mut self.pick {
                    pick.at = [f64::from(at.x), f64::from(at.y)];
                    pick.moved = true;
                }
            }
            if response.dragged() {
                let slow = ui.input(|i| i.modifiers.shift);
                let d = response.drag_delta() / scale * if slow { 0.1 } else { 1.0 };
                self.move_cross(f64::from(d.x), f64::from(d.y));
            }
        }
        if self.pick.as_ref().map(|p| p.at) != held {
            self.write_moved(rig, input.new_group, actions);
        }
        let suspects: Vec<u32> = self
            .fit
            .as_ref()
            .and_then(|f| f.outcome.as_ref().ok())
            .map(|j| j.report.suspects.clone())
            .unwrap_or_default();
        if let Some(set) = self.fit.as_ref().and_then(|f| f.set.as_ref()) {
            for p in &set.points {
                let color = if p.off {
                    theme::TEXT_WEAK
                } else if suspects.contains(&p.id) {
                    theme::ERROR
                } else {
                    Color32::from_rgb(90, 220, 120)
                };
                painter.circle_filled(to_pad(p.picture), 3.0, color);
                painter.text(
                    to_pad(p.picture) + vec2(5.0, -5.0),
                    egui::Align2::LEFT_BOTTOM,
                    p.id.to_string(),
                    egui::FontId::monospace(10.0),
                    color,
                );
            }
        }
        if let Some(pick) = &self.pick {
            let c = to_pad(pick.at);
            let stroke = Stroke::new(1.0, theme::WARNING);
            painter.line_segment([pos2(rect.left(), c.y), pos2(rect.right(), c.y)], stroke);
            painter.line_segment([pos2(c.x, rect.top()), pos2(c.x, rect.bottom())], stroke);
        }
        ui.horizontal_wrapped(|ui| {
            let placing = self.pick.is_some();
            let r = ui.add_enabled(placing, egui::Button::new("Set point (Enter)"));
            auto.add("model_map.set", r.rect, "Set point");
            if r.clicked() {
                self.place_pick(rig, actions);
            }
            let r = ui.add_enabled(placing, egui::Button::new("Cancel (Esc)"));
            auto.add("model_map.cancel", r.rect, "Cancel");
            if r.clicked() {
                self.cancel_pick(actions);
            }
            match &self.pick {
                Some(pick) => ui.monospace(format!("{:.1}, {:.1}", pick.at[0], pick.at[1])),
                None => ui.label(RichText::new("Choose a corner first.").color(theme::TEXT_WEAK)),
            };
            ui.label(
                RichText::new("Arrows: 1 px, Shift 10. Shift drag: fine. Tab: next point.")
                    .color(theme::TEXT_WEAK),
            );
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("3. Check").strong());
            for (show, word, id) in [
                (Show::Nothing, "Nothing", "model_map.show.nothing"),
                (Show::Lines, "Lines", "model_map.show.lines"),
                (Show::Colours, "Colours", "model_map.show.colours"),
            ] {
                let r = ui.selectable_label(rig.show == show, word).on_hover_text(match show {
                    Show::Nothing => "Only the crosshair on the object",
                    Show::Lines => "The model's lines on the object: the best view of how well the fit sits",
                    Show::Colours => "Every part of the model in a dim colour of its own: the crosshair stays visible on top, for tuning",
                });
                auto.add(id, r.rect, word);
                if r.clicked() && rig.show != show {
                    actions.push(Action::Edit { ops: show_ops(rig, show), label: "model view".into(), group: None });
                }
            }
        });
        self.status(ui);
        self.point_list(ui, rig, auto, actions);
    }

    fn status(&self, ui: &mut Ui) {
        let Some(fit) = &self.fit else { return };
        match &fit.outcome {
            Err(message) => {
                let enough = fit
                    .set
                    .as_ref()
                    .is_some_and(|set| set.points.iter().filter(|p| !p.off).count() >= MIN_POINTS);
                ui.label(RichText::new(message).color(if enough {
                    theme::WARNING
                } else {
                    theme::TEXT_WEAK
                }));
            }
            Ok(judged) => {
                let worst = judged.deviation.iter().map(|d| d.max).fold(0.0, f64::max);
                let expected = match judged.quality {
                    Some(q) if !judged.deviation.is_empty() => {
                        format!("expected off by up to {worst:.1} px on the model: {}", quality_word(q))
                    }
                    _ => "the error away from the points cannot be estimated".to_string(),
                };
                ui.label(format!(
                    "{} points, error {:.2} px (largest {:.2}), {expected}",
                    judged.report.points.len(),
                    judged.report.solution.rms,
                    judged.report.solution.max_error,
                ));
            }
        }
    }

    /// What is weak about the fit. Scrolls with the list, so a long text cannot squeeze it out.
    fn warnings(&self, ui: &mut Ui) {
        let Some(Ok(judged)) = self.fit.as_ref().map(|f| f.outcome.as_ref()) else { return };
        for w in &judged.report.warnings {
            ui.colored_label(theme::WARNING, w);
        }
    }

    fn point_list(
        &mut self,
        ui: &mut Ui,
        rig: &Rig,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let Some(set) = self.fit.as_ref().and_then(|f| f.set.clone()) else { return };
        let mut event = None;
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        let scroll = egui::ScrollArea::vertical()
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
        scroll.show(ui, |ui| {
            self.warnings(ui);
            egui::Grid::new("model_map_points").striped(true).num_columns(7).show(ui, |ui| {
                for head in ["#", "part", "error", "left out", "ratio", "", ""] {
                    ui.label(RichText::new(head).color(theme::TEXT_WEAK));
                }
                ui.end_row();
                for p in &set.points {
                    let row = self
                        .fit
                        .as_ref()
                        .and_then(|f| f.outcome.as_ref().ok())
                        .and_then(|j| j.report.points.iter().find(|r| r.id == p.id).map(|r| (r, j)));
                    let suspect = row.is_some_and(|(_, j)| j.report.suspects.contains(&p.id));
                    let number = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.2}"));
                    let id = RichText::new(p.id.to_string());
                    let current = self.pick.as_ref().is_some_and(|c| c.id == Some(p.id));
                    ui.label(if current {
                        id.color(theme::WARNING).strong()
                    } else if suspect {
                        id.color(theme::ERROR)
                    } else {
                        id
                    });
                    ui.label(p.group.as_deref().unwrap_or("-"));
                    ui.monospace(number(row.map(|(r, _)| r.error)));
                    ui.monospace(number(row.and_then(|(r, _)| r.left_out)));
                    ui.monospace(number(row.and_then(|(r, _)| r.ratio)));
                    let mut on = !p.off;
                    let r = ui.checkbox(&mut on, "");
                    auto.add(format!("model_map.point.{}.on", p.id), r.rect, "In the fit");
                    if r.changed() {
                        event = Some(RowEvent::Toggle(p.id, !on));
                    }
                    ui.horizontal(|ui| {
                        let r = ui.small_button("Move");
                        auto.add(format!("model_map.point.{}.move", p.id), r.rect, "Move");
                        if r.clicked() {
                            event = Some(RowEvent::Change(p.clone()));
                        }
                        let r = ui.small_button("Remove");
                        auto.add(format!("model_map.point.{}.remove", p.id), r.rect, "Remove");
                        if r.clicked() {
                            event = Some(RowEvent::Remove(p.id));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        match event {
            Some(RowEvent::Change(p)) => {
                self.start_pick(rig, p.world, p.group, Some((p.id, p.picture)), actions)
            }
            Some(RowEvent::Toggle(id, off)) => {
                let mut next = set;
                if let Some(p) = next.points.iter_mut().find(|p| p.id == id) {
                    p.off = off;
                }
                // Cancelling the point under the crosshair must not take this back too.
                if let Some(pick) = &mut self.pick {
                    pick.written = false;
                }
                self.commit(rig, &next, "calibration point", None, actions);
            }
            Some(RowEvent::Remove(id)) => {
                let mut next = set;
                next.points.retain(|p| p.id != id);
                if self.pick.as_ref().is_some_and(|p| p.id == Some(id)) {
                    self.pick = None;
                }
                self.commit(rig, &next, "calibration point removed", None, actions);
            }
            None => {}
        }
    }
}

/// A corner of the model: where it is, where the window draws it and how far away it is.
type Corner = ([f64; 3], Pos2, f32);

/// The corner nearest to `pointer` within reach. Of corners level with it the frontmost wins, as a
/// body's front and back corner coincide when seen head on.
fn nearest_corner(corners: &[Corner], pointer: Pos2) -> Option<usize> {
    let distance = |at: &Pos2| at.distance(pointer);
    let best = corners.iter().map(|(_, at, _)| distance(at)).fold(f32::MAX, f32::min);
    if best > GRAB {
        return None;
    }
    corners
        .iter()
        .enumerate()
        .filter(|(_, (_, at, _))| distance(at) <= best + LEVEL)
        .min_by(|(_, a), (_, b)| a.2.total_cmp(&b.2))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_columns_fit_any_room() {
        let (row, gap, aspect) = (26.0, 8.0, 0.5625);
        for [x, y] in [
            [1060.0, 720.0],
            [1900.0, 980.0],
            [3800.0, 2000.0],
            [3800.0, 900.0],
            [900.0, 1400.0],
            [500.0, 300.0],
        ] {
            let c = Columns::for_room(vec2(x, y), aspect, row, gap);
            assert!(c.model[0] + c.right + gap <= x + 0.01, "{x} {y} {c:?}");
            assert!(c.model[1] <= y && c.model[0] > 100.0, "{x} {y} {c:?}");
            assert!(c.pad <= c.right, "{x} {y} {c:?}");
            assert!(c.pad * aspect + row * LOWER_ROWS <= y.max(row * (LOWER_ROWS + 3.0)), "{x} {y} {c:?}");
        }
    }

    fn mapped() -> (Network, NodeTypes, NodeId) {
        let types = penumbra_ops::builtin_types().unwrap();
        let create = |path: &str, kind: &str| Op::Create {
            path: NodePath::parse(path).unwrap(),
            kind: kind.to_string(),
            params: Default::default(),
        };
        let set = |param: &str, value: Value| Op::Set { param: ParamPath::parse(param).unwrap(), value };
        let ops = [
            create("/m", "comp.base"),
            create("/m/camera", "view.camera"),
            create("/m/crosshair", "tex.shapes"),
            create("/m/model_wire", "mat.wireframe"),
            set("/m/camera.points", Value::Text("{}".to_string())),
            set("/m/camera.calibration", Value::Text("{}".to_string())),
            set("/m/camera.fov", Value::Float(12.0)),
            set("/m/model_wire.color", Value::Vec4([0.5, 0.5, 0.5, 1.0])),
            set("/m/crosshair.shapes", Value::Text("a poly 0,0 1,0 1,1".to_string())),
        ];
        let net = Network::new().apply(&types, &ops).unwrap().network;
        let id = net.resolve(&NodePath::parse("/m").unwrap()).unwrap();
        (net, types, id)
    }

    #[test]
    fn discarding_the_fit_leaves_a_camera_with_nothing_set() {
        let (net, types, id) = mapped();
        let after = net.apply(&types, &discard_ops(&net, &types, id)).unwrap().network;
        let camera = child(&after, id, "view.camera", "camera").unwrap();
        assert_eq!(text_param(&types, camera, "points"), "");
        assert_eq!(text_param(&types, camera, "calibration"), "");
        assert!(after.stored_param(camera.id, &Name::new("fov").unwrap()).is_none_or(|v| v.is_none()));
        let wire = child(&after, id, "mat.wireframe", "model_wire").unwrap();
        let color = types.desc(&wire.kind).and_then(|d| wire.param(d, "color"));
        assert_eq!(color, Some(&Value::Vec4([0.5, 0.5, 0.5, 0.0])));
    }

    #[test]
    fn a_crosshair_stays_only_on_the_camera_the_window_is_open_on() {
        let (net, types, id) = mapped();
        assert_eq!(stray_crosshairs(&net, &types, None).len(), 1);
        assert!(stray_crosshairs(&net, &types, Some(id)).is_empty());
    }

    #[test]
    fn the_crosshair_leaves_its_middle_free_and_reads_as_one_shape_per_line() {
        let text = cross_shapes([960.0, 540.0], [1920, 1080], MANY_LINES).join("\n");
        let shapes = penumbra_ops::shapes::parse(&text).unwrap();
        assert_eq!(shapes.len(), 16);
        let middle = [0.5, 0.5];
        assert!(shapes.iter().all(|s| !s.geom.contains(middle)));
    }

    #[test]
    fn a_crosshair_at_the_edge_of_the_picture_still_reads() {
        let shapes =
            penumbra_ops::shapes::parse(&cross_shapes([0.0, 1080.0], [1920, 1080], FEW_LINES).join("\n"))
                .unwrap();
        assert_eq!(shapes.len(), 4);
    }

    #[test]
    fn marks_leave_out_the_point_under_the_crosshair_and_points_off_the_fit() {
        let mut set = PointSet::new([1920, 1080], PictureKind::Projector);
        for (id, off) in [(1, false), (2, false), (3, true)] {
            set.points.push(Point { id, world: [0.0; 3], picture: [100.0, 100.0], group: None, off });
        }
        let marks = point_marks(&set, None, Some(2)).join("\n");
        assert_eq!(penumbra_ops::shapes::parse(&marks).unwrap().len(), 2);
    }

    #[test]
    fn of_two_corners_seen_on_top_of_each_other_the_front_one_is_chosen() {
        let at = pos2(10.0, 10.0);
        let corners = [([0.0; 3], at, 0.5), ([1.0; 3], at, -0.5), ([2.0; 3], pos2(200.0, 0.0), -9.0)];
        assert_eq!(nearest_corner(&corners, pos2(11.0, 10.0)), Some(1));
        assert_eq!(nearest_corner(&corners, pos2(100.0, 100.0)), None);
    }

    #[test]
    fn seen_head_on_the_model_keeps_its_right_and_its_up() {
        let view = View::default();
        assert_eq!(view.turn([1.0, -2.0, 3.0], [0.0; 3]), [1.0, -2.0, 3.0]);
    }
}
