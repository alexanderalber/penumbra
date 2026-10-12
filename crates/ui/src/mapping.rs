//! Editing a `tex.mapper`: in the viewer (or the viewer's window), the mapper's picture with a
//! handle on each corner and on each mesh point. Dragging a handle moves that corner (its
//! `corner_*` param) or mesh point (one entry of `warp`); Shift while dragging moves a tenth as
//! far. A click selects a handle, and the arrow keys then move it by one picture pixel (Shift:
//! ten), while the pointer is over the picture. The mouse wheel zooms around the pointer, the right
//! or middle button pans, so corners dragged beyond the picture stay reachable. The toolbar offers
//! the mesh size, the grid pattern, a reset and the zoom.
//!
//! Positions are in picture sizes, as the params store them, so what the handles show is where
//! the points land on the wall.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use penumbra_core::{GroupId, NodeId, ParamPath, Value};
use penumbra_graph::{Network, NodeTypes, Op};
use penumbra_ops::mapping::{Mapping, Mesh};
use penumbra_ops::tex::mapper::CORNERS;

use crate::{Action, automation, theme};

/// Grab distance around a handle, in points.
const GRAB: f32 = 10.0;
/// Mesh sizes offered in the header (points per side); 0 is no mesh.
pub const MESH_SIZES: [usize; 5] = [0, 3, 5, 9, 17];
/// Zoom range of the canvas, relative to the picture fitting it.
const ZOOM: (f32, f32) = (0.1, 8.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Corner(usize),
    Mesh(usize, usize),
}

struct Drag {
    handle: Handle,
    group: GroupId,
    /// The handle's offset and the pointer when the drag started.
    offset: [f64; 2],
    start: Pos2,
}

#[derive(Default)]
pub struct MapEditor {
    node: Option<NodeId>,
    selected: Option<Handle>,
    drag: Option<Drag>,
    /// The history group of the arrow-key nudges in a row.
    nudge_group: Option<GroupId>,
    /// Zoom and pan of the canvas (the viewer and the viewer window each have their own editor).
    view: View,
}

/// Canvas zoom (1: the picture fits) and pan in points. Shared with the shape editor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    zoom: f32,
    pan: egui::Vec2,
}

impl Default for View {
    fn default() -> Self {
        Self { zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

impl View {
    /// Where the picture (`aspect` wide per high) lies in `area` at this zoom and pan.
    pub(crate) fn picture(&self, area: Rect, aspect: f32) -> Rect {
        let inner = area.shrink(4.0);
        let w = inner.width().min(inner.height() * aspect);
        let fitted = vec2(w, w / aspect.max(1e-6));
        Rect::from_center_size(inner.center() + self.pan, fitted * self.zoom)
    }

    /// The wheel zooms around the pointer, the right or middle button pans.
    pub(crate) fn steer(&mut self, ui: &Ui, response: &egui::Response, area: Rect) {
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0
                && let Some(at) = response.hover_pos()
            {
                let zoom = (self.zoom * (scroll * 0.002).exp()).clamp(ZOOM.0, ZOOM.1);
                let centre = area.shrink(4.0).center();
                let picture_centre = centre + self.pan;
                let moved = at + (picture_centre - at) * (zoom / self.zoom);
                self.pan = moved - centre;
                self.zoom = zoom;
            }
        }
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            self.pan += response.drag_delta();
        }
    }

    /// The zoom field and the fit button.
    pub(crate) fn controls(&mut self, ui: &mut Ui, auto: &mut automation::Registry) {
        let mut percent = self.zoom * 100.0;
        let r = ui.add(
            egui::DragValue::new(&mut percent)
                .range(ZOOM.0 * 100.0..=ZOOM.1 * 100.0)
                .speed(1.0)
                .suffix(" %")
                .fixed_decimals(0),
        );
        auto.add("viewer.zoom", r.rect, "zoom");
        if r.changed() {
            self.zoom = (percent / 100.0).clamp(ZOOM.0, ZOOM.1);
        }
        let r = ui.button("fit").on_hover_text("Zoom 100 %, centred");
        auto.add("viewer.fit", r.rect, "fit");
        if r.clicked() {
            *self = View::default();
        }
    }
}

/// What the overlay needs from the app.
pub struct MapInputs<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    pub node: NodeId,
    pub mapping: &'a Mapping,
    pub can_edit: bool,
    pub new_group: &'a dyn Fn() -> GroupId,
}

fn param(node: NodeId, net: &Network, name: &str) -> Option<ParamPath> {
    format!("{}.{name}", net.path_of(node)?).parse().ok()
}

fn stored<'a>(input: &'a MapInputs<'_>, name: &str) -> Option<&'a Value> {
    let node = input.net.node(input.node)?;
    let desc = input.types.desc(&node.kind)?;
    node.param(desc, name)
}

/// The current mesh as stored (not as last cooked, which may be a frame behind).
fn stored_mesh(input: &MapInputs<'_>) -> Option<Mesh> {
    Mesh::parse(stored(input, "warp").and_then(Value::as_str).unwrap_or_default()).ok().flatten()
}

fn offset_of(input: &MapInputs<'_>, handle: Handle) -> [f64; 2] {
    match handle {
        Handle::Corner(i) => {
            match CORNERS.get(i).and_then(|n| stored(input, n)).and_then(Value::components) {
                Some(&[x, y]) => [x, y],
                _ => [0.0, 0.0],
            }
        }
        Handle::Mesh(c, r) => stored_mesh(input).map_or([0.0, 0.0], |m| m.get(c, r)),
    }
}

/// The edit that gives `handle` the offset `value`.
fn set_offset(input: &MapInputs<'_>, handle: Handle, value: [f64; 2]) -> Option<Op> {
    let value = value.map(|v| (v * 100_000.0).round() / 100_000.0);
    match handle {
        Handle::Corner(i) => {
            Some(Op::Set { param: param(input.node, input.net, CORNERS.get(i)?)?, value: Value::Vec2(value) })
        }
        Handle::Mesh(c, r) => {
            let mut mesh = stored_mesh(input)?;
            mesh.set(c, r, value);
            Some(Op::Set { param: param(input.node, input.net, "warp")?, value: Value::Text(mesh.to_text()) })
        }
    }
}

impl MapEditor {
    fn view(&mut self) -> &mut View {
        &mut self.view
    }

    /// The header controls: mesh size, grid pattern, reset.
    pub fn header(
        &mut self,
        ui: &mut Ui,
        input: &MapInputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        ui.add_enabled_ui(input.can_edit, |ui| {
            let mesh = stored_mesh(input);
            let current = mesh.as_ref().map_or(0, Mesh::cols);
            let label = |n: usize| if n == 0 { "no mesh".to_string() } else { format!("mesh {n}x{n}") };
            let mut chosen = None;
            let r =
                egui::ComboBox::from_id_salt("viewer.mesh").selected_text(label(current)).show_ui(ui, |ui| {
                    for (i, n) in MESH_SIZES.into_iter().enumerate() {
                        let r = ui.selectable_label(n == current, label(n));
                        auto.add(format!("viewer.mesh.{i}"), r.rect, label(n));
                        if r.clicked() {
                            chosen = Some(n);
                        }
                    }
                });
            auto.add("viewer.mesh", r.response.rect, "mesh");
            if let Some(n) = chosen.filter(|n| *n != current)
                && let Some(path) = param(input.node, input.net, "warp")
            {
                let text = if n == 0 {
                    String::new()
                } else {
                    let from = mesh.or_else(|| Mesh::new(2, 2));
                    from.and_then(|m| m.resample(n, n)).map(|m| m.to_text()).unwrap_or_default()
                };
                self.selected = None;
                actions.push(Action::Edit {
                    ops: vec![Op::Set { param: path, value: Value::Text(text) }],
                    label: "mesh size".into(),
                    group: None,
                    place: None,
                });
            }
            let mut pattern = stored(input, "pattern").and_then(Value::as_bool).unwrap_or(false);
            let r = ui.toggle_value(&mut pattern, "grid");
            auto.add("viewer.grid", r.rect, "grid");
            if r.changed()
                && let Some(path) = param(input.node, input.net, "pattern")
            {
                actions.push(Action::edit(vec![Op::Set { param: path, value: Value::Bool(pattern) }]));
            }
            let r = ui.button("reset").on_hover_text("Corners back to their fitted place, mesh flat");
            let reset = r.clicked();
            auto.add("viewer.reset", r.rect, "reset");
            if reset {
                let mut ops: Vec<Op> = CORNERS
                    .iter()
                    .filter_map(|n| param(input.node, input.net, n))
                    .map(|param| Op::Reset { param })
                    .collect();
                if let Some(mesh) = stored_mesh(input)
                    && let (Some(flat), Some(path)) =
                        (Mesh::new(mesh.cols(), mesh.rows()), param(input.node, input.net, "warp"))
                {
                    ops.push(Op::Set { param: path, value: Value::Text(flat.to_text()) });
                }
                actions.push(Action::Edit { ops, label: "reset mapping".into(), group: None, place: None });
            }
        });
        // View controls work in either editing mode.
        self.view().controls(ui, auto);
    }

    /// The picture (`texture`, `aspect` wide per high) in `area` at the canvas zoom and pan,
    /// with the handles over it.
    #[allow(clippy::too_many_arguments)]
    pub fn canvas(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        texture: egui::TextureId,
        aspect: f32,
        input: &MapInputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let picture = self.view().picture(area, aspect);
        let painter = ui.painter_at(area);
        painter.image(texture, picture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // The picture's edge, so corners dragged outside it show where it is cut off.
        painter.rect_stroke(
            picture,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_white_alpha(60)),
            egui::StrokeKind::Outside,
        );
        self.overlay(ui, area, picture, input, auto, actions);
    }

    /// Handles over the picture drawn in `fit` (the projector's image on screen); `area` is the
    /// whole viewer.
    pub fn overlay(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        fit: Rect,
        input: &MapInputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if self.node != Some(input.node) {
            *self = MapEditor { node: Some(input.node), ..MapEditor::default() };
        }
        let m = input.mapping;
        let to_screen =
            |p: [f64; 2]| pos2(fit.min.x + p[0] as f32 * fit.width(), fit.min.y + p[1] as f32 * fit.height());
        let painter = ui.painter_at(area);
        let line = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 200, 80, 140));

        // The mapped region's outline and the mesh lines, through the whole mapping.
        let mesh = m.mesh.as_ref();
        let steps = 48;
        let curve = |from: [f64; 2], to: [f64; 2]| -> Vec<Pos2> {
            (0..=steps)
                .filter_map(|k| {
                    let t = k as f64 / steps as f64;
                    m.project([from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t])
                        .map(to_screen)
                })
                .collect()
        };
        for (a, b) in [
            ([0.0, 0.0], [1.0, 0.0]),
            ([1.0, 0.0], [1.0, 1.0]),
            ([1.0, 1.0], [0.0, 1.0]),
            ([0.0, 1.0], [0.0, 0.0]),
        ] {
            painter.add(egui::Shape::line(curve(a, b), line));
        }
        let mut handles: Vec<(Handle, Pos2)> = Vec::new();
        if let Some(mesh) = mesh {
            let thin = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 200, 80, 60));
            for r in 1..mesh.rows().saturating_sub(1) {
                let v = mesh.grid_uv(0, r)[1];
                painter.add(egui::Shape::line(curve([0.0, v], [1.0, v]), thin));
            }
            for c in 1..mesh.cols().saturating_sub(1) {
                let u = mesh.grid_uv(c, 0)[0];
                painter.add(egui::Shape::line(curve([u, 0.0], [u, 1.0]), thin));
            }
            for r in 0..mesh.rows() {
                for c in 0..mesh.cols() {
                    if let Some(p) = m.project(mesh.grid_uv(c, r)) {
                        handles.push((Handle::Mesh(c, r), to_screen(p)));
                    }
                }
            }
        }
        // Corners where the params put them, so a handle follows the pointer even where the
        // corners cross (the picture then keeps its last good mapping and the node shows why).
        for (i, b) in m.base.iter().enumerate() {
            let o = offset_of(input, Handle::Corner(i));
            handles.push((Handle::Corner(i), to_screen([b[0] + o[0], b[1] + o[1]])));
        }

        // Interaction: the whole viewer, so handles dragged past the picture stay reachable.
        let response = ui.interact(area, ui.id().with("map_editor"), Sense::click_and_drag());
        let pointer = response.interact_pointer_pos().or_else(|| response.hover_pos());
        let nearest = |at: Pos2| {
            handles
                .iter()
                // Corners first among equals: they sit on top of the mesh's corner points.
                .rev()
                .map(|(h, p)| (*h, p.distance(at)))
                .filter(|(_, d)| *d <= GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(h, _)| h)
        };
        let px = [1.0 / f64::from(m.raster.0.max(1)), 1.0 / f64::from(m.raster.1.max(1))];
        self.view().steer(ui, &response, area);
        if input.can_edit {
            // Where the button went down: by the time a drag registers, the pointer has moved.
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(at) = origin
                && let Some(handle) = nearest(at)
            {
                self.selected = Some(handle);
                self.drag = Some(Drag {
                    handle,
                    group: (input.new_group)(),
                    offset: offset_of(input, handle),
                    start: at,
                });
            }
            if let Some(drag) = &self.drag
                && let Some(at) = pointer
                && response.dragged_by(egui::PointerButton::Primary)
            {
                let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
                let d = at - drag.start;
                let value = [
                    drag.offset[0] + f64::from(d.x / fit.width().max(1.0)) * fine,
                    drag.offset[1] + f64::from(d.y / fit.height().max(1.0)) * fine,
                ];
                if let Some(op) = set_offset(input, drag.handle, value) {
                    actions.push(Action::Edit {
                        ops: vec![op],
                        label: "map".into(),
                        group: Some(drag.group),
                        place: None,
                    });
                }
            }
            if response.drag_stopped() {
                self.drag = None;
            }
            if response.clicked() {
                self.selected = pointer.and_then(nearest);
            }
            // Arrow keys move the selected handle by projector pixels while over the viewer;
            // the nudges in a row are one undo step.
            if let Some(handle) = self.selected
                && response.hovered()
            {
                let (dx, dy) = ui.input_mut(|i| {
                    let mut d = (0.0, 0.0);
                    for (key, step) in [
                        (egui::Key::ArrowLeft, (-1.0, 0.0)),
                        (egui::Key::ArrowRight, (1.0, 0.0)),
                        (egui::Key::ArrowUp, (0.0, -1.0)),
                        (egui::Key::ArrowDown, (0.0, 1.0)),
                    ] {
                        // Shift first: `consume_key` matches logically, so without Shift it would take both.
                        for m in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                            if i.consume_key(m, key) {
                                let k = if m.shift { 10.0 } else { 1.0 };
                                d = (d.0 + step.0 * k, d.1 + step.1 * k);
                            }
                        }
                    }
                    d
                });
                if dx != 0.0 || dy != 0.0 {
                    let o = offset_of(input, handle);
                    let group = *self.nudge_group.get_or_insert_with(|| (input.new_group)());
                    if let Some(op) = set_offset(input, handle, [o[0] + dx * px[0], o[1] + dy * px[1]]) {
                        actions.push(Action::Edit {
                            ops: vec![op],
                            label: "nudge".into(),
                            group: Some(group),
                            place: None,
                        });
                    }
                }
            }
            if !response.hovered() {
                self.nudge_group = None;
            }
        }

        let hovered = if self.drag.is_none() { pointer.and_then(nearest) } else { None };
        for (handle, p) in &handles {
            let corner = matches!(handle, Handle::Corner(_));
            let active =
                self.selected == Some(*handle) || self.drag.as_ref().is_some_and(|d| d.handle == *handle);
            let radius = if corner { 6.0 } else { 4.0 };
            let fill = if active {
                theme::ACCENT
            } else if hovered == Some(*handle) {
                Color32::WHITE
            } else {
                Color32::from_rgba_unmultiplied(255, 200, 80, 200)
            };
            painter.circle(*p, radius, fill, Stroke::new(1.5, Color32::BLACK));
            let id = match handle {
                Handle::Corner(i) => format!("viewer.corner.{i}"),
                Handle::Mesh(c, r) => format!("viewer.mesh.{c}.{r}"),
            };
            auto.add(id, Rect::from_center_size(*p, vec2(radius * 2.0, radius * 2.0)), "handle");
        }
        if let Some(handle) = self.selected {
            let o = offset_of(input, handle);
            let text = format!(
                "{}  {:+.1}, {:+.1} px",
                match handle {
                    Handle::Corner(i) => ["top left", "top right", "bottom right", "bottom left"]
                        .get(i)
                        .copied()
                        .unwrap_or("corner")
                        .to_string(),
                    Handle::Mesh(c, r) => format!("mesh {c}, {r}"),
                },
                o[0] * f64::from(m.raster.0),
                o[1] * f64::from(m.raster.1)
            );
            painter.text(
                area.left_bottom() + vec2(8.0, -6.0),
                egui::Align2::LEFT_BOTTOM,
                text,
                egui::FontId::monospace(12.0),
                theme::TEXT_WEAK,
            );
        }
    }
}
