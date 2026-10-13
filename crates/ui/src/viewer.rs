//! The viewer: the output of the selected node, or of a pinned one, at full rate. Textures fit
//! the panel; channels show their values and a short history (or all samples when a channel
//! carries many); tables show their first rows; geometry and materials their pictures, lights
//! a line each.

use std::collections::{BTreeMap, VecDeque};

use egui::emath::GuiRounding as _;
use egui::{Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use penumbra_core::{Chans, Name, NodeId, Table};
use penumbra_graph::{Data, DataKind, Network};
use penumbra_ops::freeview::Request;
use penumbra_ops::view::{Lens, Viewpoint};

use crate::automation;
use crate::editor::format_value;
use crate::mapping::{MapEditor, MapInputs};
use crate::navigate::{Mode, Move, Navigation, Scene};
use crate::shapes::{ShapeEditor, ShapeInputs};
use crate::textures::Textures;
use crate::{Action, theme};

/// Channel samples kept for the curve (four seconds at 60 fps).
const HISTORY: usize = 240;
/// Channels drawn as curves and listed; more are counted.
const MAX_CURVES: usize = 12;
/// Table rows drawn.
const MAX_ROWS: usize = 200;

#[derive(Default)]
pub struct Viewer {
    pub pinned: Option<NodeId>,
    pub output: u32,
    /// Per channel: (show frame, value), oldest first.
    history: BTreeMap<Name, VecDeque<(i64, f64)>>,
    history_of: Option<(NodeId, u32)>,
    last_frame: Option<i64>,
    pub map: MapEditor,
    pub shapes: ShapeEditor,
    /// The node shown in the viewer's window of its own, if one is open (the main viewer's).
    pub window: Option<NodeId>,
    /// The free view's cameras and the modes chosen for 3D nodes.
    pub nav: Navigation,
}

/// What the viewer needs to look around in a 3D node.
pub struct NavInputs<'a> {
    pub scene: &'a Scene,
    /// The free view as the engine drew it for this viewer, if it asked.
    pub free: Option<&'a Result<penumbra_ops::freeview::Picture, String>>,
    pub net: &'a Network,
    pub types: &'a penumbra_graph::NodeTypes,
    pub can_edit: bool,
    pub new_group: &'a dyn Fn() -> penumbra_core::GroupId,
}

impl Viewer {
    /// The node shown: the pinned one, else the given selection.
    pub fn target(&self, net: &Network, selected: Option<NodeId>) -> Option<NodeId> {
        let id = self.pinned.filter(|id| net.node(*id).is_some()).or(selected)?;
        // A sink (`out` family) has nothing to show itself; show what it sends out.
        match net.node(id) {
            Some(n) if n.kind.starts_with("out.") => {
                n.inputs.first().copied().flatten().map(|p| p.node).or(Some(id))
            }
            // A component shows its first output (its output nodes are `tex.out`, `chan.out`, ...).
            Some(n) if n.kind == penumbra_ops::comp::COMP_KIND => net
                .children(Some(id))
                .filter(|c| c.kind.ends_with(".out"))
                .min_by(|a, b| penumbra_graph::natural_cmp(a.name.as_str(), b.name.as_str()))
                .map(|c| c.id)
                .or(Some(id)),
            _ => Some(id),
        }
    }

    /// The header line: what is shown, pin, window, output switch (by the outputs' `names`),
    /// size. Drawn into the section header; in the viewer's window (`in_window`) without pin and
    /// window button.
    #[allow(clippy::too_many_arguments)]
    pub fn header(
        &mut self,
        ui: &mut Ui,
        net: &Network,
        target: Option<NodeId>,
        outputs: Option<&Vec<Data>>,
        names: &[String],
        in_window: bool,
        scene: Option<&Scene>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        {
            let title = target
                .and_then(|id| net.path_of(id))
                .map_or("nothing selected".to_string(), |p| p.to_string());
            ui.label(RichText::new(title).color(theme::TEXT_WEAK));
            if !in_window {
                let mut pinned = self.pinned.is_some();
                let r = ui.toggle_value(&mut pinned, "pin");
                auto.add("viewer.pin", r.rect, "pin");
                if r.changed() {
                    actions.push(Action::Pin(if pinned { target } else { None }));
                }
                if self.window.is_none()
                    && let Some(id) = target
                {
                    let r = ui.button("window").on_hover_text("Show this node in a window of its own");
                    auto.add("viewer.window", r.rect, "window");
                    if r.clicked() {
                        self.window = Some(id);
                    }
                }
            }
            let count = outputs.map_or(0, Vec::len);
            if count > 1 && !scene.is_some_and(|sc| self.nav.mode(sc) == Mode::Free) {
                for o in 0..count {
                    let name = names.get(o).cloned().unwrap_or_else(|| format!("out {o}"));
                    let o = o as u32;
                    let r = ui.selectable_label(self.output == o, name);
                    auto.add(format!("viewer.output.{o}"), r.rect, "output");
                    if r.clicked() {
                        self.output = o;
                    }
                }
            }
            let data = outputs.and_then(|o| o.get(self.output as usize));
            let size = match data {
                Some(d) if d.kind() == DataKind::Tex => {
                    d.get::<penumbra_gpu::Tex>().map(|t| format!("{} x {}", t.width(), t.height()))
                }
                Some(d) if d.kind() == DataKind::Chan => d.get::<Chans>().map(|c| {
                    let n = c.channels().len();
                    let rate = c.rate();
                    // Audio comes in blocks whose length changes from frame to frame (one or two
                    // device callbacks); the count would flicker and says nothing.
                    if c.len() > 1 && rate < penumbra_audio::analysis::MIN_RATE {
                        format!("{n} ch x {} @ {rate} Hz", c.len())
                    } else {
                        format!("{n} ch @ {rate} Hz")
                    }
                }),
                Some(d) if d.kind() == DataKind::Table => {
                    d.get::<Table>().map(|t| format!("{} x {}", t.num_rows(), t.num_cols()))
                }
                Some(d) if d.kind() == DataKind::Mat => {
                    d.get::<penumbra_gpu::Mat>().map(|m| match m.material.shading {
                        penumbra_gpu::geo::Shading::Constant => "constant".to_string(),
                        penumbra_gpu::geo::Shading::Phong => "phong".to_string(),
                        penumbra_gpu::geo::Shading::Pbr => "pbr".to_string(),
                    })
                }
                Some(d) if d.kind() == DataKind::Light => d.get::<penumbra_gpu::Lights>().map(|l| {
                    let n = l.0.len();
                    format!("{n} light{}", if n == 1 { "" } else { "s" })
                }),
                Some(d) if d.kind() == DataKind::View => d.get::<Viewpoint>().map(|v| match v.lens {
                    Lens::Fov { .. } => "camera".to_string(),
                    Lens::Calibrated(_) => "calibrated camera".to_string(),
                }),
                Some(d) if d.kind() == DataKind::Geo => d.get::<penumbra_gpu::Geo>().map(|g| {
                    let parts = g.part_count();
                    format!(
                        "{parts} part{}, {} triangles",
                        if parts == 1 { "" } else { "s" },
                        g.triangle_count()
                    )
                }),
                _ => None,
            };
            if let Some(size) = size {
                ui.label(RichText::new(size).color(theme::TEXT_WEAK));
            }
        }
    }

    /// The picture (or the channel's curve), filling the rest of `ui`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        target: Option<NodeId>,
        outputs: Option<&Vec<Data>>,
        frame: Option<i64>,
        background: Color32,
        textures: &mut Textures,
        map: Option<&MapInputs<'_>>,
        shapes: Option<&ShapeInputs<'_>>,
        nav: Option<&NavInputs<'_>>,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        if let Some(map) = map {
            // A projector's mapping controls get their own line: the header is full already.
            ui.horizontal(|ui| self.map.header(ui, map, auto, actions));
        }
        if let Some(input) = shapes {
            self.shapes.header(ui, input, auto, actions);
        }
        if let Some(nav) = nav.filter(|n| n.scene.render) {
            // A line of its own, as the mapping controls: the header is full already.
            ui.horizontal(|ui| self.mode_switch(ui, nav.net, nav.scene, auto, actions));
        }
        let mode = nav.map(|n| self.nav.mode(n.scene));
        let sense =
            if mode.is_some_and(|m| m != Mode::Picture) { Sense::click_and_drag() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), sense);
        auto.add("viewer", rect, "viewer");
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, background);
        let Some(id) = target else { return };
        if let (Some(nav), Some(mode)) = (nav, mode)
            && mode != Mode::Picture
        {
            let ppp = painter.pixels_per_point();
            let area = rect.shrink(4.0);
            self.nav.size = (
                (area.width() * ppp).round().max(16.0) as u32,
                (area.height() * ppp).round().max(16.0) as u32,
            );
            let m = crate::navigate::mouse_move(ui, &response, area.height());
            let now = std::time::Instant::now();
            if response.double_clicked() && mode == Mode::Free {
                self.nav.home(nav.scene.node);
            } else if let Err(e) =
                self.nav.apply(&m, nav.scene, nav.net, nav.types, nav.can_edit, nav.new_group, now, actions)
            {
                actions.push(Action::Message(e));
            }
            self.nav.settle(now, response.dragged() || !m.is_none());
            if !self.nav.horizon_locked {
                painter.text(
                    rect.left_bottom() + vec2(8.0, -6.0),
                    egui::Align2::LEFT_BOTTOM,
                    "3D mouse: horizon unlocked",
                    egui::FontId::proportional(11.0),
                    theme::TEXT_WEAK,
                );
            }
            if mode == Mode::Free {
                self.free_picture(&painter, rect, id, nav, textures);
                return;
            }
        }
        let data = outputs.and_then(|o| o.get(self.output as usize).or_else(|| o.first()));
        let Some(data) = data else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "no output yet",
                egui::FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
            return;
        };
        match data.kind() {
            DataKind::Tex => {
                if let Some(tex) = data.get::<penumbra_gpu::Tex>()
                    && let Some(tid) = textures.show((id, self.output), tex)
                {
                    let aspect = tex.width() as f32 / tex.height().max(1) as f32;
                    if let Some(map) = map {
                        self.map.canvas(ui, rect, tid, aspect, map, auto, actions);
                        return;
                    }
                    if let Some(input) = shapes {
                        self.shapes.canvas(ui, rect, tid, aspect, tex.size(), input, auto, actions);
                        return;
                    }
                    let area = rect.shrink(4.0);
                    let w = area.width().min(area.height() * aspect);
                    // On whole pixels: the checker's anti-aliased edge would show as a grey
                    // line below or beside a picture ending inside a pixel.
                    let fit = Rect::from_center_size(area.center(), vec2(w, w / aspect))
                        .round_to_pixels(painter.pixels_per_point());
                    theme::checker(&painter, fit, 1.5);
                    painter.image(
                        tid,
                        fit,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            DataKind::Chan => {
                if let Some(chans) = data.get::<Chans>() {
                    self.channels(&painter, rect, id, chans, frame);
                }
            }
            DataKind::Table => {
                if let Some(table) = data.get::<Table>() {
                    table_grid(&painter, rect.shrink(8.0), table);
                }
            }
            DataKind::Geo | DataKind::Mat => {
                // The geometry's picture (framed, lit; a tex.render shows it from a camera), or
                // the material's on a sphere.
                if let Some(tex) = preview_of(data)
                    && let Some(tid) = textures.show((id, self.output), tex)
                {
                    let aspect = tex.width() as f32 / tex.height().max(1) as f32;
                    let area = rect.shrink(4.0);
                    let w = area.width().min(area.height() * aspect);
                    // On whole pixels: the checker's anti-aliased edge would show as a grey
                    // line below or beside a picture ending inside a pixel.
                    let fit = Rect::from_center_size(area.center(), vec2(w, w / aspect))
                        .round_to_pixels(painter.pixels_per_point());
                    theme::checker(&painter, fit, 1.5);
                    painter.image(
                        tid,
                        fit,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            DataKind::Light => {
                if let Some(lights) = data.get::<penumbra_gpu::Lights>() {
                    let mut y = rect.min.y + 8.0;
                    for l in &lights.0 {
                        let swatch = Rect::from_min_size(pos2(rect.min.x + 8.0, y + 2.0), vec2(12.0, 12.0));
                        painter.rect_filled(swatch, CornerRadius::same(2), light_swatch(l));
                        painter.text(
                            pos2(swatch.max.x + 6.0, y),
                            egui::Align2::LEFT_TOP,
                            light_line(l),
                            egui::FontId::proportional(13.0),
                            theme::TEXT,
                        );
                        y += 18.0;
                    }
                }
            }
            DataKind::View => {
                if let Some(v) = data.get::<Viewpoint>() {
                    for (i, line) in view_lines(v).into_iter().enumerate() {
                        painter.text(
                            pos2(rect.min.x + 8.0, rect.min.y + 8.0 + i as f32 * 18.0),
                            egui::Align2::LEFT_TOP,
                            line,
                            egui::FontId::proportional(13.0),
                            theme::TEXT,
                        );
                    }
                }
            }
        }
    }

    /// The free view's picture with its lines, fitted into `rect`.
    fn free_picture(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        id: NodeId,
        nav: &NavInputs<'_>,
        textures: &mut Textures,
    ) {
        let message = |text: &str| {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
        };
        match nav.free {
            Some(Ok(picture)) if picture.node == id => {
                self.nav.arrived(picture);
                // A slot of its own in the texture cache: the node's outputs use their numbers.
                let Some(tid) = textures.show((id, u32::MAX), &picture.tex) else { return };
                let (w, h) = picture.tex.size();
                let aspect = w as f32 / h.max(1) as f32;
                let area = rect.shrink(4.0);
                let fw = area.width().min(area.height() * aspect);
                let fit = Rect::from_center_size(area.center(), vec2(fw, fw / aspect))
                    .round_to_pixels(painter.pixels_per_point());
                theme::checker(painter, fit, 1.5);
                painter.image(tid, fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                crate::navigate::draw_lines(painter, fit, picture);
            }
            Some(Err(e)) => message(e),
            _ => message("drawing the free view"),
        }
    }

    /// What the free view asks the engine for, while the viewer shows one of `scene`.
    pub fn free_request(&self, scene: &Scene) -> Option<Request> {
        (self.nav.mode(scene) == Mode::Free).then(|| Request {
            node: scene.node,
            output: self.output,
            camera: self.nav.camera(scene.node),
            size: self.nav.size,
        })
    }

    /// A move from elsewhere (the 3D mouse, a command), applied as the mouse's are.
    pub fn navigate(
        &mut self,
        m: &Move,
        nav: &NavInputs<'_>,
        actions: &mut Vec<Action>,
    ) -> Result<(), String> {
        let now = std::time::Instant::now();
        let r = self.nav.apply(m, nav.scene, nav.net, nav.types, nav.can_edit, nav.new_group, now, actions);
        self.nav.settle(now, !m.is_none());
        r
    }

    /// Picture, free view, steer; and taking the free view into the camera node.
    fn mode_switch(
        &mut self,
        ui: &mut Ui,
        net: &Network,
        scene: &Scene,
        auto: &mut automation::Registry,
        actions: &mut Vec<Action>,
    ) {
        let current = self.nav.mode(scene);
        let free_tip = "Look around with a camera of the viewer's own: drag to turn, middle-drag to move, \
                        right-drag or the wheel to come closer, double-click to start over";
        let mut modes = vec![(Mode::Free, free_tip)];
        if scene.render {
            modes.insert(0, (Mode::Picture, "The render's own picture"));
            modes.push((Mode::Steer, "Navigating moves the render's camera; each movement is one undo step"));
        }
        for (mode, tip) in modes {
            let tip = match (&scene.steer, mode) {
                (Err(why), Mode::Steer) => why.as_str(),
                _ => tip,
            };
            let enabled = mode != Mode::Steer || scene.steer.is_ok();
            let r = ui
                .add_enabled(enabled, egui::Button::selectable(current == mode, mode.name()))
                .on_hover_text(tip)
                .on_disabled_hover_text(tip);
            auto.add(format!("viewer.mode.{}", mode.name()), r.rect, "mode");
            if r.clicked() {
                self.nav.set_mode(scene.node, mode);
            }
        }
        if current == Mode::Free && scene.render {
            let r = ui
                .add_enabled(scene.steer.is_ok(), egui::Button::new("take view"))
                .on_hover_text("Move the render's camera to where the free view looks from (one undo step)");
            auto.add("viewer.take_view", r.rect, "take view");
            if r.clicked() {
                match self.nav.take_view(scene, net) {
                    Ok(ops) => actions.push(Action::Edit {
                        ops,
                        label: "take the view".into(),
                        group: None,
                        place: None,
                    }),
                    Err(e) => actions.push(Action::Message(e)),
                }
            }
        }
    }

    /// Curves of the channels: their samples when they carry many, else the last four seconds
    /// of their values. A list of names and values on top.
    ///
    /// The history is keyed by show frame, not by UI frame or cook pass: the UI repaints on its
    /// own beat, so it sometimes misses a pass, and the clock may skip a late frame. Placing each
    /// value at its frame keeps the curve's time axis true instead of putting a kink there.
    fn channels(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        id: NodeId,
        chans: &Chans,
        frame: Option<i64>,
    ) {
        let shown: Vec<&penumbra_core::Channel> = chans.channels().iter().take(MAX_CURVES).collect();
        // A seek backwards starts over, like another node.
        if self.history_of != Some((id, self.output)) || frame < self.last_frame {
            self.history.clear();
            self.history_of = Some((id, self.output));
        }
        let sampled = chans.len() > 1;
        if let Some(now) = frame
            && !sampled
        {
            self.last_frame = Some(now);
            self.history.retain(|name, _| shown.iter().any(|c| &c.name == name));
            for c in &shown {
                let h = self.history.entry(c.name.clone()).or_default();
                // The same frame again (paused, or a repaint between passes) updates its value,
                // so an edit while paused still shows.
                match h.back_mut() {
                    Some(last) if last.0 == now => last.1 = c.value(),
                    _ => h.push_back((now, c.value())),
                }
                while h.front().is_some_and(|(f, _)| now - f >= HISTORY as i64) {
                    h.pop_front();
                }
            }
        }
        let series: Vec<(usize, Vec<(f64, f64)>)> = shown
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let points = if sampled {
                    let last = c.samples.len().saturating_sub(1);
                    c.samples.iter().enumerate().map(|(k, v)| ((last - k) as f64, *v)).collect()
                } else {
                    let now = frame.unwrap_or(0);
                    self.history
                        .get(&c.name)
                        .map(|h| h.iter().map(|(f, v)| ((now - f) as f64, *v)).collect())
                        .unwrap_or_default()
                };
                (i, points)
            })
            .collect();
        let span = if sampled { chans.len() } else { HISTORY };
        curves(painter, rect.shrink(12.0), &series, span);
        let font =
            if shown.len() == 1 { egui::FontId::monospace(22.0) } else { egui::FontId::monospace(13.0) };
        let mut y = rect.min.y + 10.0;
        for (i, c) in shown.iter().enumerate() {
            let text = if shown.len() == 1 && c.name.as_str() == "value" {
                format_value(c.value())
            } else {
                format!("{}  {}", c.name, format_value(c.value()))
            };
            let r = painter.text(
                pos2(rect.min.x + 12.0, y),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                if shown.len() == 1 { theme::TEXT } else { theme::channel_color(i) },
            );
            y = r.max.y + 2.0;
        }
        let more = chans.channels().len().saturating_sub(MAX_CURVES);
        if more > 0 {
            painter.text(
                pos2(rect.min.x + 12.0, y),
                egui::Align2::LEFT_TOP,
                format!("+{more} more"),
                egui::FontId::monospace(13.0),
                theme::TEXT_WEAK,
            );
        }
        if chans.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "no channels",
                egui::FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
        }
    }
}

/// Lines through `series` (channel index, points as (age, value)) on one shared scale; age 0
/// at the right edge, `span` samples across the width.
fn curves(painter: &egui::Painter, rect: Rect, series: &[(usize, Vec<(f64, f64)>)], span: usize) {
    let (lo, hi) = series.iter().flat_map(|(_, p)| p.iter().map(|(_, v)| v)).fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(lo, hi), v| {
            if v.is_finite() { (lo.min(*v), hi.max(*v)) } else { (lo, hi) }
        },
    );
    if !(lo.is_finite() && hi.is_finite()) {
        return;
    }
    let (lo, hi) = if hi - lo < 1e-9 { (lo - 1.0, hi + 1.0) } else { (lo, hi) };
    let n = span.saturating_sub(1).max(1) as f32;
    for (i, values) in series {
        if values.len() < 2 {
            continue;
        }
        let points: Vec<Pos2> = values
            .iter()
            .filter(|(_, v)| v.is_finite())
            .map(|(age, v)| {
                let x = rect.max.x - *age as f32 / n * rect.width();
                let y = rect.max.y - ((v - lo) / (hi - lo)) as f32 * rect.height();
                pos2(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.5, theme::channel_color(*i))));
    }
    if lo < 0.0 && hi > 0.0 {
        let y = rect.max.y - ((0.0 - lo) / (hi - lo)) as f32 * rect.height();
        painter.line_segment(
            [pos2(rect.min.x, y), pos2(rect.max.x, y)],
            Stroke::new(1.0, Color32::from_white_alpha(25)),
        );
    }
}

/// The first rows of a table as a grid of monospace cells, columns as wide as their longest
/// shown cell (capped).
pub fn table_grid(painter: &egui::Painter, rect: Rect, table: &Table) {
    let font = egui::FontId::monospace(12.0);
    let row_h = 16.0;
    let rows = table.rows().iter().take(MAX_ROWS.min((rect.height() / row_h).floor().max(0.0) as usize));
    let shown: Vec<&Vec<String>> = rows.collect();
    let cols = shown.iter().map(|r| r.len()).max().unwrap_or(0).min(64);
    let char_w = 7.2;
    let widths: Vec<f32> = (0..cols)
        .map(|c| {
            let chars = shown.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0);
            (chars.clamp(1, 40) as f32 * char_w + 12.0).min(rect.width())
        })
        .collect();
    for (r, row) in shown.iter().enumerate() {
        let y = rect.min.y + r as f32 * row_h;
        if r % 2 == 1 {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.min.x, y), vec2(rect.width(), row_h)),
                CornerRadius::ZERO,
                Color32::from_white_alpha(6),
            );
        }
        let mut x = rect.min.x + 4.0;
        for (c, cell) in row.iter().enumerate().take(cols) {
            if x > rect.max.x {
                break;
            }
            let text: String =
                cell.chars().take(40).map(|ch| if ch.is_control() { ' ' } else { ch }).collect();
            painter.text(pos2(x, y + 1.0), egui::Align2::LEFT_TOP, text, font.clone(), theme::TEXT);
            x += widths.get(c).copied().unwrap_or(0.0);
        }
    }
    if table.num_rows() == 0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "empty table",
            egui::FontId::proportional(14.0),
            theme::TEXT_WEAK,
        );
    } else if table.num_rows() > shown.len() {
        painter.text(
            rect.right_bottom(),
            egui::Align2::RIGHT_BOTTOM,
            format!("{} of {} rows", shown.len(), table.num_rows()),
            egui::FontId::proportional(11.0),
            theme::TEXT_WEAK,
        );
    }
}

/// The picture a geometry or material output shows of itself.
pub(crate) fn preview_of(data: &Data) -> Option<&penumbra_gpu::Tex> {
    match data.kind() {
        DataKind::Geo => data.get::<penumbra_gpu::Geo>().and_then(|g| g.preview.as_ref()),
        DataKind::Mat => data.get::<penumbra_gpu::Mat>().and_then(|m| m.preview.as_ref()),
        _ => None,
    }
}

/// A light's colour for a swatch: its hue at full brightness (intensity aside), to sRGB.
pub(crate) fn light_swatch(l: &penumbra_gpu::render::Light) -> Color32 {
    let peak = l.color.iter().copied().fold(0.0f32, f32::max);
    if !peak.is_finite() || peak <= 1e-6 {
        return Color32::from_gray(30);
    }
    let [r, g, b] = l.color.map(|c| {
        let v = (c / peak).clamp(0.0, 1.0).powf(1.0 / 2.2);
        (v * 255.0).round() as u8
    });
    Color32::from_rgb(r, g, b)
}

pub(crate) fn light_name(l: &penumbra_gpu::render::Light) -> &'static str {
    use penumbra_gpu::render::LightKind;
    match l.kind {
        LightKind::Ambient => "ambient",
        LightKind::Directional => "directional",
        LightKind::Point => "point",
        LightKind::Spot => "spot",
        LightKind::Projector => "projector",
        LightKind::Environment => "environment",
    }
}

/// Where a camera is, its lens, its depth range.
pub(crate) fn view_lines(v: &Viewpoint) -> [String; 3] {
    let p = v.position();
    let lens = match v.lens {
        Lens::Fov { .. } => format!("fov {}°", format_value(v.fov())),
        Lens::Calibrated(_) => format!("calibrated, fov {}°", format_value(v.fov())),
    };
    [
        format!("at ({}, {}, {})", format_value(p[0]), format_value(p[1]), format_value(p[2])),
        lens,
        format!("near {}, far {}", format_value(v.near), format_value(v.far)),
    ]
}

/// What kind, where, how bright.
fn light_line(l: &penumbra_gpu::render::Light) -> String {
    use penumbra_gpu::render::LightKind;
    let p = |v: [f64; 3]| format!("({}, {}, {})", format_value(v[0]), format_value(v[1]), format_value(v[2]));
    let peak = l.color.iter().copied().fold(0.0f32, f32::max);
    let at = match l.kind {
        LightKind::Ambient => String::new(),
        LightKind::Directional => format!(" towards {}", p(l.direction)),
        LightKind::Point | LightKind::Spot | LightKind::Projector => format!(" at {}", p(l.position)),
        LightKind::Environment => match &l.environment {
            Some(e) => format!(" turned {}°", format_value(e.turn)),
            None => String::new(),
        },
    };
    format!("{}{at}, {}", light_name(l), format_value(f64::from(peak)))
}
