//! Editing a `tex.shapes` in the viewer: the node's picture with every shape outlined in its tag's
//! colour, at the canvas zoom and pan the mapping editor has (wheel, right or middle button).
//!
//! Tools in the header: **select** (click a shape to select it, drag it to move it, drag a
//! handle on its corners or edges, or a polygon's points, to reshape it; the diamond on the
//! last copy of a repeat sets the spacing), **rect** and **ellipse** (drag a box), **poly**
//! (click the points; a double click, Enter or a click on the first point closes it, Escape
//! cancels). Shift while dragging moves a tenth as far. With a shape selected: its tag, its
//! repeat (columns x rows), explode (every copy a shape of its own), delete; the arrow keys move
//! it by one surface pixel (Shift: ten), Delete removes it, while the pointer is over the picture.
//!
//! Every change rewrites only the shape's own line of the `shapes` text, so comments and the
//! order of the other lines stay.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use penumbra_core::{GroupId, Name, NodeId, ParamPath, Value};
use penumbra_graph::{Network, NodeTypes, Op};
use penumbra_ops::shapes::{self, Geom, Shape};
use penumbra_ops::tex::shapes::SHAPES;

use crate::mapping::View;

/// Screen to surface and surface to screen.
type Convert<'a> = (&'a dyn Fn(Pos2) -> [f64; 2], &'a dyn Fn([f64; 2]) -> Pos2);
use crate::{Action, automation, theme};

/// Grab distance around a handle, in points.
const GRAB: f32 = 8.0;
/// Smallest box a drag creates, in points (a click is not a shape).
const MIN_DRAG: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Poly,
}

impl Tool {
    const ALL: [Tool; 4] = [Tool::Select, Tool::Rect, Tool::Ellipse, Tool::Poly];

    fn name(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Poly => "poly",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Body,
    Handle(usize),
    Spacing,
    /// A new box from `from` (surface) to the pointer.
    Create,
}

struct Drag {
    grab: Grab,
    group: GroupId,
    index: usize,
    /// The shape as it was and the surface point where the button went down.
    original: Shape,
    from: [f64; 2],
}

pub struct ShapeEditor {
    node: Option<NodeId>,
    pub tool: Tool,
    selected: Option<usize>,
    drag: Option<Drag>,
    nudge_group: Option<GroupId>,
    view: View,
    /// The points of a polygon being drawn.
    drawing: Vec<[f64; 2]>,
    /// The tag new shapes get (the selected shape's, once one is selected).
    pub tag: String,
}

impl Default for ShapeEditor {
    fn default() -> Self {
        Self {
            node: None,
            tool: Tool::Select,
            selected: None,
            drag: None,
            nudge_group: None,
            view: View::default(),
            drawing: Vec::new(),
            tag: "shape".into(),
        }
    }
}

/// What the editor needs from the app.
pub struct ShapeInputs<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    pub node: NodeId,
    pub can_edit: bool,
    pub new_group: &'a dyn Fn() -> GroupId,
}

impl ShapeInputs<'_> {
    fn text(&self) -> &str {
        let stored = || {
            let node = self.net.node(self.node)?;
            let desc = self.types.desc(&node.kind)?;
            node.param(desc, "shapes")?.as_str()
        };
        stored().unwrap_or_default()
    }

    /// Whether the shapes come from the table input (then they are not edited here).
    fn fed_by_table(&self) -> bool {
        self.net.node(self.node).is_some_and(|n| n.inputs.first().copied().flatten().is_some())
    }

    fn path(&self) -> Option<ParamPath> {
        format!("{}.shapes", self.net.path_of(self.node)?).parse().ok()
    }

    /// The edit that puts `with` in place of shape `index` (appends past the end).
    fn replace(&self, index: usize, with: &[Shape]) -> Option<Op> {
        let text = shapes::replace(self.text(), index, with);
        Some(Op::Set { param: self.path()?, value: Value::Text(text) })
    }
}

/// Whether `id` is a `tex.shapes` (the viewer edits its shapes).
pub fn is_shapes(net: &Network, id: NodeId) -> bool {
    net.node(id).is_some_and(|n| n.kind == SHAPES)
}

fn color(tags: &[String], tag: &str, alpha: u8) -> Color32 {
    let [r, g, b] = shapes::tag_color(tags, tag);
    Color32::from_rgba_unmultiplied((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, alpha)
}

impl ShapeEditor {
    fn edit(actions: &mut Vec<Action>, op: Option<Op>, label: &str, group: Option<GroupId>) {
        if let Some(op) = op {
            actions.push(Action::Edit { ops: vec![op], label: label.into(), group, place: None });
        }
    }

    /// The header: tools, then what can be done with the selected shape, then the view.
    pub fn header(
        &mut self,
        ui: &mut Ui,
        input: &ShapeInputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let list = shapes::parse(input.text()).unwrap_or_default();
        let editable = input.can_edit && !input.fed_by_table();
        // Two rows, so a narrow column does not wrap them in odd places: tools and view, then
        // the tag and what can be done with the selected shape.
        ui.horizontal(|ui| {
            ui.add_enabled_ui(editable, |ui| {
                for tool in Tool::ALL {
                    let r = ui.selectable_label(self.tool == tool, tool.name());
                    auto.add(format!("viewer.tool.{}", tool.name()), r.rect, tool.name());
                    if r.clicked() {
                        self.tool = tool;
                        self.drawing.clear();
                    }
                }
            });
            ui.add_space(6.0);
            self.view.controls(ui, auto);
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(editable, |ui| self.shape_row(ui, input, &list, auto, actions));
        });
    }

    /// The tag (of the selected shape, or for new ones), the selected shape's repeat, explode
    /// and delete.
    fn shape_row(
        &mut self,
        ui: &mut Ui,
        input: &ShapeInputs<'_>,
        list: &[Shape],
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let selected = self.selected.and_then(|i| Some((i, list.get(i)?.clone())));
        let mut tag = selected.as_ref().map_or(self.tag.clone(), |(_, s)| s.tag.clone());
        let r = ui.add(egui::TextEdit::singleline(&mut tag).desired_width(80.0).hint_text("tag"));
        auto.add("viewer.tag", r.rect, "tag");
        let valid = Name::new(tag.trim()).is_ok();
        if r.changed() && valid {
            let tag = tag.trim().to_string();
            self.tag = tag.clone();
            if let Some((i, mut shape)) = selected.clone() {
                shape.tag = tag;
                Self::edit(actions, input.replace(i, &[shape]), "tag", None);
            }
        }
        if !valid && !tag.is_empty() {
            r.on_hover_text("Letters, digits and _, not starting with a digit");
        }
        let Some((i, shape)) = selected else {
            ui.label(egui::RichText::new("tag of new shapes").color(theme::TEXT_WEAK));
            return;
        };
        let r0 = shape.repeat.map_or((1, 1), |r| (r.cols, r.rows));
        let (mut cols, mut rows) = r0;
        let spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 3.0;
        let rc = ui.add(egui::DragValue::new(&mut cols).range(1..=shapes::MAX_REPEAT).speed(0.05));
        auto.add("viewer.repeat.cols", rc.on_hover_text("Columns of copies").rect, "columns");
        ui.label("x");
        let rr = ui.add(egui::DragValue::new(&mut rows).range(1..=shapes::MAX_REPEAT).speed(0.05));
        auto.add("viewer.repeat.rows", rr.on_hover_text("Rows of copies").rect, "rows");
        ui.spacing_mut().item_spacing.x = spacing;
        if (cols, rows) != r0 {
            let mut s = shape.clone();
            s.set_repeat(cols, rows);
            Self::edit(actions, input.replace(i, &[s]), "repeat", None);
        }
        if shape.repeat.is_some() {
            let r = ui.button("explode").on_hover_text("Every copy a shape of its own");
            auto.add("viewer.explode", r.rect, "explode");
            if r.clicked() {
                Self::edit(actions, input.replace(i, &shape.explode()), "explode", None);
            }
        }
        let r = ui.button("delete");
        auto.add("viewer.delete", r.rect, "delete");
        if r.clicked() {
            self.selected = None;
            Self::edit(actions, input.replace(i, &[]), "delete shape", None);
        }
    }

    /// The picture (`texture`, `aspect` wide per high) in `area` with the shapes over it.
    #[allow(clippy::too_many_arguments)]
    pub fn canvas(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        texture: egui::TextureId,
        aspect: f32,
        raster: (u32, u32),
        input: &ShapeInputs<'_>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if self.node != Some(input.node) {
            *self = ShapeEditor { node: Some(input.node), tag: self.tag.clone(), ..ShapeEditor::default() };
        }
        let picture = self.view.picture(area, aspect);
        let painter = ui.painter_at(area);
        theme::checker(&painter, picture.intersect(area), 1.5);
        painter.image(texture, picture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        painter.rect_stroke(
            picture,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_white_alpha(60)),
            egui::StrokeKind::Outside,
        );
        let to_screen = |p: [f64; 2]| {
            pos2(
                picture.min.x + p[0] as f32 * picture.width(),
                picture.min.y + p[1] as f32 * picture.height(),
            )
        };
        let to_surface = |p: Pos2| {
            [
                f64::from((p.x - picture.min.x) / picture.width().max(1.0)),
                f64::from((p.y - picture.min.y) / picture.height().max(1.0)),
            ]
        };
        let parsed = shapes::parse(input.text());
        let list = parsed.clone().unwrap_or_default();
        if self.selected.is_some_and(|i| i >= list.len()) {
            self.selected = None;
        }
        let tags = shapes::tags(&list);
        let editable = input.can_edit && !input.fed_by_table() && parsed.is_ok();

        let response = ui.interact(area, ui.id().with("shape_editor"), Sense::click_and_drag());
        self.view.steer(ui, &response, area);
        let pointer = response.interact_pointer_pos().or_else(|| response.hover_pos());
        // Hit testing: handles of the selected shape first, then shapes, last drawn on top.
        let handles: Vec<(Grab, Pos2)> = self
            .selected
            .and_then(|i| list.get(i))
            .map(|s| {
                // A box's edge handles only where it is large enough on screen to tell them
                // from the corners.
                let [_, _, w, h] = s.geom.bounds();
                let roomy = !matches!(s.geom, Geom::Poly(_))
                    && w as f32 * picture.width() >= 40.0
                    && h as f32 * picture.height() >= 40.0;
                let mut h: Vec<(Grab, Pos2)> = s
                    .handles()
                    .into_iter()
                    .enumerate()
                    .filter(|(k, _)| matches!(s.geom, Geom::Poly(_)) || *k < 4 || roomy)
                    .map(|(k, p)| (Grab::Handle(k), to_screen(p)))
                    .collect();
                if let Some(a) = s.spacing_anchor() {
                    h.push((Grab::Spacing, to_screen(a)));
                }
                h
            })
            .unwrap_or_default();
        let instances = shapes::instances(&list);
        let hit_handle = |at: Pos2| {
            handles
                .iter()
                .map(|(g, p)| (*g, p.distance(at)))
                .filter(|(_, d)| *d <= GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(g, _)| g)
        };
        let hit_shape =
            |at: Pos2| instances.iter().rev().find(|i| i.geom.contains(to_surface(at))).map(|i| i.shape);

        if editable {
            self.interact(
                ui,
                &response,
                pointer,
                input,
                &list,
                &hit_handle,
                &hit_shape,
                (&to_surface, &to_screen),
                raster,
                actions,
            );
        }

        // Outlines, the selected shape's thicker; its copies' too.
        let hovered =
            if self.drag.is_none() && self.tool == Tool::Select { pointer.and_then(hit_shape) } else { None };
        for inst in &instances {
            let selected = self.selected == Some(inst.shape);
            let alpha = if selected || hovered == Some(inst.shape) { 255 } else { 190 };
            let width = if selected { 2.5 } else { 1.5 };
            let mut points: Vec<Pos2> = inst.geom.outline().into_iter().map(to_screen).collect();
            if let Some(first) = points.first().copied() {
                points.push(first);
            }
            painter.add(egui::Shape::line(points, Stroke::new(width, color(&tags, &inst.tag, alpha))));
        }
        for (i, s) in list.iter().enumerate() {
            let [x, y, w, h] = s.geom.bounds();
            let c = to_screen([x + w / 2.0, y + h / 2.0]);
            auto.add(format!("viewer.shape.{i}"), Rect::from_center_size(c, vec2(8.0, 8.0)), s.tag.clone());
        }
        for (grab, p) in handles.iter().copied() {
            let (id, fill) = match grab {
                Grab::Handle(k) => (format!("viewer.handle.{k}"), Color32::WHITE),
                _ => ("viewer.spacing".to_string(), theme::ACCENT),
            };
            let r = Rect::from_center_size(p, vec2(8.0, 8.0));
            if grab == Grab::Spacing {
                let pts =
                    vec![p + vec2(0.0, -6.0), p + vec2(6.0, 0.0), p + vec2(0.0, 6.0), p + vec2(-6.0, 0.0)];
                painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::new(1.5, Color32::BLACK)));
            } else {
                painter.rect(
                    r,
                    egui::CornerRadius::ZERO,
                    fill,
                    Stroke::new(1.5, Color32::BLACK),
                    egui::StrokeKind::Middle,
                );
            }
            auto.add(id, r, "handle");
        }
        // The polygon being drawn, with a line to the pointer.
        if !self.drawing.is_empty() {
            let mut pts: Vec<Pos2> = self.drawing.iter().copied().map(to_screen).collect();
            if let Some(p) = response.hover_pos() {
                pts.push(p);
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));
            if let Some(first) = self.drawing.first() {
                painter.circle_stroke(to_screen(*first), GRAB, Stroke::new(1.0, theme::ACCENT));
            }
        }

        let status = match (&parsed, self.selected.and_then(|i| list.get(i))) {
            (Err(e), _) => Some((e.clone(), theme::ERROR)),
            (Ok(_), _) if input.fed_by_table() => {
                Some(("shapes come from the table input; edit them at their source".into(), theme::TEXT_WEAK))
            }
            (Ok(_), Some(s)) => {
                let [x, y, w, h] = s.geom.bounds();
                let (rw, rh) = (f64::from(raster.0), f64::from(raster.1));
                let mut text = format!(
                    "{} {}  x {:.0}  y {:.0}  w {:.0}  h {:.0} px",
                    s.tag,
                    s.geom.kind(),
                    x * rw,
                    y * rh,
                    w * rw,
                    h * rh
                );
                if let Some(r) = s.repeat {
                    text.push_str(&format!(
                        "  {}x{} every {:.0}, {:.0} px",
                        r.cols,
                        r.rows,
                        r.step[0] * rw,
                        r.step[1] * rh
                    ));
                }
                Some((text, theme::TEXT_WEAK))
            }
            (Ok(_), None) if self.tool == Tool::Poly => Some((
                "click the points; double click, Enter or the first point closes".into(),
                theme::TEXT_WEAK,
            )),
            _ => None,
        };
        if let Some((text, c)) = status {
            let galley = painter.layout_no_wrap(text, egui::FontId::monospace(12.0), c);
            let at = area.left_bottom() + vec2(6.0, -6.0 - galley.size().y);
            let back = Rect::from_min_size(at, galley.size()).expand(3.0);
            painter.rect_filled(back, egui::CornerRadius::same(3), Color32::from_black_alpha(170));
            painter.galley(at, galley, c);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn interact(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        pointer: Option<Pos2>,
        input: &ShapeInputs<'_>,
        list: &[Shape],
        hit_handle: &dyn Fn(Pos2) -> Option<Grab>,
        hit_shape: &dyn Fn(Pos2) -> Option<usize>,
        (to_surface, to_screen): Convert<'_>,
        raster: (u32, u32),
        actions: &mut Vec<Action>,
    ) {
        // Where the button went down: by the time a drag registers, the pointer has moved.
        let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
        let typing = ui.memory(|m| m.focused().is_some());
        if self.tool == Tool::Poly {
            self.draw_poly(ui, response, pointer, input, list, (to_surface, to_screen), typing, actions);
            return;
        }
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(at) = origin
        {
            let from = to_surface(at);
            let existing = |grab: Grab, i: usize| Some((grab, i, list.get(i)?.clone()));
            let start = match self.tool {
                Tool::Select => match hit_handle(at) {
                    Some(grab) => self.selected.and_then(|i| existing(grab, i)),
                    None => hit_shape(at).and_then(|i| existing(Grab::Body, i)),
                },
                Tool::Rect | Tool::Ellipse => (list.len() < shapes::MAX_INSTANCES).then(|| {
                    let r = [from[0], from[1], 0.0, 0.0];
                    let geom = if self.tool == Tool::Rect { Geom::Rect(r) } else { Geom::Ellipse(r) };
                    (Grab::Create, list.len(), Shape { tag: self.tag.clone(), geom, repeat: None })
                }),
                Tool::Poly => None,
            };
            match start {
                Some((grab, index, original)) => {
                    self.selected = Some(index);
                    self.tag = original.tag.clone();
                    self.drag = Some(Drag { grab, group: (input.new_group)(), index, original, from });
                }
                None if self.tool == Tool::Select => self.selected = None,
                None => {}
            }
        }
        if let Some(drag) = &self.drag
            && let Some(at) = pointer
            && response.dragged_by(egui::PointerButton::Primary)
        {
            let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
            let p = to_surface(at);
            let d = [(p[0] - drag.from[0]) * fine, (p[1] - drag.from[1]) * fine];
            let mut s = drag.original.clone();
            let label = match drag.grab {
                Grab::Body => {
                    s.move_by(d);
                    "move shape"
                }
                Grab::Handle(k) => {
                    if let Some(h) = drag.original.handles().get(k) {
                        s.set_handle(k, [h[0] + d[0], h[1] + d[1]]);
                    }
                    "reshape"
                }
                Grab::Spacing => {
                    if let Some(a) = drag.original.spacing_anchor() {
                        s.set_spacing_to([a[0] + d[0], a[1] + d[1]]);
                    }
                    "spacing"
                }
                Grab::Create => {
                    let r = shapes::rect_from(drag.from, p);
                    s.geom = match s.geom {
                        Geom::Ellipse(_) => Geom::Ellipse(r),
                        _ => Geom::Rect(r),
                    };
                    "new shape"
                }
            };
            // A box only once the pointer has really moved: a click is not a shape.
            let moved = origin.is_some_and(|o| o.distance(at) >= MIN_DRAG);
            if drag.grab != Grab::Create || moved {
                Self::edit(actions, input.replace(drag.index, &[s]), label, Some(drag.group));
            }
        }
        if response.drag_stopped() {
            if self.drag.as_ref().is_some_and(|d| d.grab == Grab::Create) {
                self.tool = Tool::Select;
            }
            self.drag = None;
        }
        if response.clicked() && self.tool == Tool::Select {
            self.selected = pointer.and_then(hit_shape);
            if let Some(s) = self.selected.and_then(|i| list.get(i)) {
                self.tag = s.tag.clone();
            }
        }
        // Keys over the picture: arrows nudge by surface pixels, Delete removes.
        if let Some(i) = self.selected
            && response.hovered()
            && !typing
            && let Some(shape) = list.get(i)
        {
            let (dx, dy, delete) = ui.input_mut(|inp| {
                let mut d = (0.0, 0.0);
                for (key, step) in [
                    (egui::Key::ArrowLeft, (-1.0, 0.0)),
                    (egui::Key::ArrowRight, (1.0, 0.0)),
                    (egui::Key::ArrowUp, (0.0, -1.0)),
                    (egui::Key::ArrowDown, (0.0, 1.0)),
                ] {
                    // Shift first: `consume_key` matches logically, so without Shift it would take both.
                    for m in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                        if inp.consume_key(m, key) {
                            let k = if m.shift { 10.0 } else { 1.0 };
                            d = (d.0 + step.0 * k, d.1 + step.1 * k);
                        }
                    }
                }
                let delete = inp.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || inp.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
                (d.0, d.1, delete)
            });
            if delete {
                self.selected = None;
                Self::edit(actions, input.replace(i, &[]), "delete shape", None);
            } else if dx != 0.0 || dy != 0.0 {
                let group = *self.nudge_group.get_or_insert_with(|| (input.new_group)());
                let mut s = shape.clone();
                s.move_by([dx / f64::from(raster.0.max(1)), dy / f64::from(raster.1.max(1))]);
                Self::edit(actions, input.replace(i, &[s]), "nudge shape", Some(group));
            }
        }
        if !response.hovered() {
            self.nudge_group = None;
        }
    }

    /// The poly tool: clicks add points; a double click, Enter or a click on the first point
    /// closes the polygon, Escape drops it.
    #[allow(clippy::too_many_arguments)]
    fn draw_poly(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        pointer: Option<Pos2>,
        input: &ShapeInputs<'_>,
        list: &[Shape],
        (to_surface, to_screen): Convert<'_>,
        typing: bool,
        actions: &mut Vec<Action>,
    ) {
        let (enter, escape, back) = if response.hovered() && !typing {
            ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                )
            })
        } else {
            (false, false, false)
        };
        if escape {
            self.drawing.clear();
            return;
        }
        if back {
            self.drawing.pop();
        }
        let mut close = enter || response.double_clicked();
        if response.clicked()
            && !response.double_clicked()
            && let Some(at) = pointer
        {
            let p = to_surface(at);
            let near_first = self.drawing.len() >= 3
                && self.drawing.first().is_some_and(|f| to_screen(*f).distance(at) <= GRAB);
            if near_first || self.drawing.len() >= shapes::MAX_POINTS {
                close = true;
            } else {
                self.drawing.push(p);
            }
        }
        if close && self.drawing.len() >= 3 && list.len() < shapes::MAX_INSTANCES {
            let shape = Shape {
                tag: self.tag.clone(),
                geom: Geom::Poly(std::mem::take(&mut self.drawing)),
                repeat: None,
            };
            self.selected = Some(list.len());
            self.tool = Tool::Select;
            Self::edit(actions, input.replace(list.len(), &[shape]), "new shape", None);
        } else if close {
            self.drawing.clear();
        }
    }
}
