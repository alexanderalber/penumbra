//! The controls on the show screen: every `chan.button`, `chan.slider` and `chan.dropdown` with
//! `show` on, as a row of widgets below Unlock ([`controls`]). They are drawn and pressed like
//! the controls on a node body ([`node_controls`]), only larger and with a name above
//! ([`caption`]). The order is the network's: top to bottom, then left to right. The engine
//! lists them and sets their values while show mode locks the project
//! ([`penumbra_engine::buttons::show_controls`], [`penumbra_engine::buttons::set_in_show`]).

use std::collections::BTreeMap;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Ui, pos2, vec2};
use penumbra_core::{GroupId, Name, NodeId, ParamPath};
use penumbra_graph::{Network, NodeStatus, NodeTypes};
use penumbra_ops::chan::controls::Buttons;

use crate::node_controls::{self, Press};
use crate::{Action, automation, theme};

/// Width of one control on the show screen, in points.
pub const WIDTH: f32 = 200.0;
/// The controls are drawn at this zoom of a node body: larger targets for a show.
const ZOOM: f32 = 1.5;
/// Room between two controls, in points.
const GAP: f32 = 16.0;
/// Height of the name above a control, in points.
const CAPTION: f32 = 20.0;

/// The name above a control: its label from the list. A button with a `label` shows that on its
/// face already, so only the component it sits in stays above it (nothing at the top level).
/// The cell keeps its height either way, so the controls of a row stay level.
pub fn caption<'a>(path: &'a str, label: &'a str, control: &node_controls::Control) -> &'a str {
    match control {
        node_controls::Control::Button { label: face, .. } if !face.is_empty() => {
            path.rsplit_once('/').map_or("", |(parent, _)| parent.trim_start_matches('/'))
        }
        _ => label,
    }
}

/// What [`controls`] reads.
pub struct Inputs<'a> {
    pub net: &'a Network,
    pub types: &'a NodeTypes,
    /// The cooked values of bound params ([`penumbra_engine::ViewFrame::status`]).
    pub status: &'a BTreeMap<NodeId, NodeStatus>,
    pub buttons: &'a Buttons,
    /// A fresh history group for a slider's drag (the locked project records none, but a drag
    /// keeps one group like on the node body).
    pub new_group: &'a dyn Fn() -> GroupId,
}

/// Draws the show screen's controls in rows centred in the space left, and presses them.
/// `press` is the press going on, kept across frames. Returns how many controls it drew.
pub fn controls(
    ui: &mut Ui,
    input: &Inputs<'_>,
    press: &mut Option<Press>,
    auto: &mut automation::Registry,
    actions: &mut Vec<Action>,
) -> usize {
    let listed =
        penumbra_engine::buttons::show_controls(input.net, input.types, input.buttons, Some(input.status));
    if listed.is_empty() {
        node_controls::release(ui, press, actions);
        return 0;
    }
    let width = ui.available_width().max(WIDTH);
    let per_row = (((width + GAP) / (WIDTH + GAP)).floor() as usize).clamp(1, listed.len());
    let rows = listed.len().div_ceil(per_row);
    let body_h = node_controls::BODY * ZOOM;
    let cell_h = CAPTION + body_h;
    let total = vec2(width, rows as f32 * cell_h + rows.saturating_sub(1) as f32 * GAP);
    let (area, _) = ui.allocate_exact_size(total, Sense::hover());
    let painter = ui.painter_at(area.expand(4.0));
    for (i, shown) in listed.iter().enumerate() {
        let (row, col) = (i / per_row, i % per_row);
        let in_row = if row + 1 == rows { listed.len() - row * per_row } else { per_row };
        let row_w = in_row as f32 * WIDTH + in_row.saturating_sub(1) as f32 * GAP;
        let x = area.center().x - row_w / 2.0 + col as f32 * (WIDTH + GAP);
        let y = area.min.y + row as f32 * (cell_h + GAP);
        let Some(node) = input.net.node(shown.id) else { continue };
        let held = Some(if input.buttons.view(shown.id).held { 1.0 } else { 0.0 });
        let Some(mut control) = node_controls::of(node, input.types, input.status.get(&shown.id), held)
        else {
            continue;
        };
        let caption = caption(&shown.path, &shown.label, &control);
        if !caption.is_empty() {
            painter.text(
                pos2(x + WIDTH / 2.0, y + CAPTION / 2.0),
                Align2::CENTER_CENTER,
                caption,
                FontId::proportional(14.0),
                theme::TEXT,
            );
        }
        let body = Rect::from_min_size(pos2(x, y + CAPTION), vec2(WIDTH, body_h));
        painter.rect_filled(body, CornerRadius::same(6), Color32::from_gray(38));
        let rect = node_controls::rect(body, ZOOM, 0.0);
        let resp = ui.interact(rect, ui.id().with(("show_control", shown.id)), Sense::click_and_drag());
        auto.add(format!("show.control.{}", shown.path), rect, shown.kind.clone());
        let active = !control.bound();
        let param = shown.path.parse().ok().and_then(|p| ParamPath::new(p, Name::new("value").ok()?).ok());
        let prefix = format!("show.control.{}", shown.path);
        node_controls::menu(&resp, &control, param.clone(), active, &prefix, auto, actions);
        let pointer = node_controls::interact(
            ui,
            &resp,
            rect,
            shown.id,
            &mut control,
            param,
            active,
            press,
            input.new_group,
            actions,
        );
        let name = input
            .types
            .desc(&node.kind)
            .and_then(|d| node.param(d, "name"))
            .and_then(penumbra_core::Value::as_str)
            .unwrap_or("value");
        let line = node_controls::line(name, &control, node_controls::shown_value(&control, pointer.pressed));
        let hovered = resp.hovered() || pointer.pressed;
        node_controls::draw(&painter, body, &control, &line, pointer.pressed, hovered, active, ZOOM, 0.0);
        node_controls::cursor(ui, &resp, &control, active);
        if !pointer.down {
            resp.on_hover_text(node_controls::tip(&control, true, true));
        }
    }
    // After the controls, so the frame the button goes up still knows the press (a release is
    // no extra tap).
    node_controls::release(ui, press, actions);
    // A held button or a drag shows its new state at once; a change from the control channel
    // or OSC within a tenth of a second.
    if press.is_some() {
        ui.ctx().request_repaint();
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    listed.len()
}
