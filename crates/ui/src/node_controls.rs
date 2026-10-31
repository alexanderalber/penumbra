//! The controls drawn on the body of a `chan.button` or `chan.slider` in the editor: what they
//! show, where they sit, how a pointer position becomes a value and what a press does
//! ([`interact`]). The node editor ([`crate::editor`]) and the show screen both draw and press
//! them through this module; they only place them.
//!
//! The body has two parts: a line with the channel's value on top, the control below it. The
//! top line stays free, so a click on the node's centre (where `ui.click {id: node./b}` lands)
//! still selects the node instead of pressing the button.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Painter, PointerButton, Rect, Response, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use penumbra_core::{GroupId, NodeId, ParamPath, Value};
use penumbra_graph::{DataKind, NodeDef, NodeStatus, NodeTypes, Op};
use penumbra_ops::chan::controls::{BUTTON, SLIDER, TOGGLE, slider_value};

use crate::{Action, theme};

/// Body height of a node with a control, in graph units: the value line and the control.
pub const BODY: f32 = 46.0;
/// Room left of and above the control inside the body, in graph units.
const PAD: f32 = 8.0;
/// Height of the value line above the control, in graph units.
const LINE: f32 = 18.0;
/// Steps a slider's range is divided into: a drag lands on one of them.
const STEPS: f64 = 1000.0;

/// Whether nodes of `kind` carry a control on their body.
pub fn has_control(kind: &str) -> bool {
    kind == BUTTON || kind == SLIDER
}

/// A control as it stands now.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Button {
        toggle: bool,
        /// Down (momentary) or on (toggle).
        on: bool,
        label: String,
        /// `value` follows a channel, so a click cannot switch it.
        bound: bool,
    },
    Slider {
        value: f64,
        min: f64,
        max: f64,
        bound: bool,
    },
}

impl Control {
    /// Whether a click or drag on it would be an edit (a toggle, a slider).
    pub fn edits(&self) -> bool {
        !matches!(self, Control::Button { toggle: false, .. })
    }

    pub fn bound(&self) -> bool {
        match self {
            Control::Button { bound, .. } | Control::Slider { bound, .. } => *bound,
        }
    }
}

/// The control of `node`, if its kind has one. `status` gives the values of bound params,
/// `held` the value of a momentary button's channel at its last cook.
pub fn of(
    node: &NodeDef,
    types: &NodeTypes,
    status: Option<&NodeStatus>,
    held: Option<f64>,
) -> Option<Control> {
    let desc = types.desc(&node.kind)?;
    let param = |name: &str| -> Option<&Value> {
        status.and_then(|s| s.bound.get(name)).or_else(|| node.param(desc, name))
    };
    let float = |name: &str, fallback: f64| param(name).and_then(Value::as_f64).unwrap_or(fallback);
    let bound = node.is_bound("value");
    match node.kind.as_str() {
        BUTTON => {
            let toggle = param("mode").and_then(Value::as_str) == Some(TOGGLE);
            let on = if toggle {
                param("value").and_then(Value::as_bool).unwrap_or(false)
            } else {
                held.is_some_and(|v| v > 0.5)
            };
            let label = param("label").and_then(Value::as_str).unwrap_or_default().trim().to_string();
            Some(Control::Button { toggle, on, label, bound: toggle && bound })
        }
        SLIDER => {
            let (min, max) = (float("min", 0.0), float("max", 1.0));
            let value = slider_value(float("value", 0.0), min, max);
            Some(Control::Slider { value, min, max, bound })
        }
        _ => None,
    }
}

/// Where the control sits in a node's `body` (screen points) at `zoom`: below the value line,
/// clear of the output port on the right (`port_pick`: its pick radius in points).
pub fn rect(body: Rect, zoom: f32, port_pick: f32) -> Rect {
    let right = (port_pick + 3.0).max(PAD * zoom);
    let min = pos2(body.min.x + PAD * zoom, body.min.y + LINE * zoom);
    let max = pos2((body.max.x - right).max(min.x + 1.0), (body.max.y - 5.0 * zoom).max(min.y + 1.0));
    Rect::from_min_max(min, max)
}

/// The slider's value for a pointer at `x` over `rect`: the range in [`STEPS`] steps.
pub fn slider_at(rect: Rect, x: f32, min: f64, max: f64) -> f64 {
    let t = f64::from(((x - rect.min.x) / rect.width().max(1.0)).clamp(0.0, 1.0));
    let v = min + (max - min) * (t * STEPS).round() / STEPS;
    // Two decimals more than a step needs, so 0.437 does not come out as 0.43700000000000006.
    let step = ((max - min).abs() / STEPS).max(1e-12);
    let decimals = (-step.log10()).ceil().clamp(0.0, 12.0) + 2.0;
    let scale = 10f64.powf(decimals);
    let rounded = (v * scale).round() / scale;
    if rounded.is_finite() { rounded } else { v }
}

/// Draws the value line and the control. `pressed`: held by the pointer now; `hovered`: the
/// pointer is over it; `active`: a click would do something (not bound, editing allowed).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    painter: &Painter,
    body: Rect,
    control: &Control,
    line: &str,
    pressed: bool,
    hovered: bool,
    active: bool,
    zoom: f32,
    port_pick: f32,
) {
    let r = rect(body, zoom, port_pick);
    let chan = theme::kind_color(DataKind::Chan);
    painter.text(
        pos2(r.center().x, body.min.y + LINE * zoom / 2.0 + zoom),
        Align2::CENTER_CENTER,
        line,
        FontId::monospace(11.0 * zoom),
        theme::TEXT,
    );
    let round = CornerRadius::same((3.0 * zoom).round().clamp(0.0, 255.0) as u8);
    let edge = if hovered && active { theme::TEXT } else { Color32::from_gray(90) };
    match control {
        Control::Button { toggle, on, label, .. } => {
            let lit = *on || pressed;
            let fill = if lit {
                if active { chan } else { chan.gamma_multiply(0.55) }
            } else if hovered && active {
                Color32::from_gray(70)
            } else {
                Color32::from_gray(58)
            };
            // Pressed in: one point lower while held.
            let face = if pressed { r.translate(vec2(0.0, zoom)) } else { r };
            painter.rect_filled(face, round, fill);
            painter.rect_stroke(face, round, Stroke::new(1.0, edge), StrokeKind::Inside);
            let text = if !label.is_empty() {
                label.as_str()
            } else if !*toggle {
                "press"
            } else if *on {
                "on"
            } else {
                "off"
            };
            let color = if lit { Color32::from_gray(15) } else { theme::TEXT };
            painter.text(
                face.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(12.0 * zoom),
                color,
            );
        }
        Control::Slider { value, min, max, .. } => {
            let span = max - min;
            let t = if span.abs() > 0.0 { ((value - min) / span).clamp(0.0, 1.0) as f32 } else { 0.0 };
            painter.rect_filled(r, round, Color32::from_gray(30));
            let filled = Rect::from_min_max(r.min, pos2(r.min.x + r.width() * t, r.max.y));
            let fill = if active { chan.gamma_multiply(0.75) } else { chan.gamma_multiply(0.4) };
            painter.rect_filled(filled, round, fill);
            let w = 3.0 * zoom.max(0.5);
            let x = (r.min.x + r.width() * t)
                .clamp(r.min.x + w / 2.0, (r.max.x - w / 2.0).max(r.min.x + w / 2.0));
            let handle = Rect::from_center_size(pos2(x, r.center().y), vec2(w, r.height()));
            let grip = if pressed {
                Color32::WHITE
            } else if active {
                theme::TEXT
            } else {
                theme::TEXT_WEAK
            };
            painter.rect_filled(handle, CornerRadius::ZERO, grip);
            painter.rect_stroke(r, round, Stroke::new(1.0, edge), StrokeKind::Inside);
        }
    }
}

/// A press on a control, from the primary button going down on it until it goes up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub node: NodeId,
    pub what: Pressed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pressed {
    /// A momentary button, held in the engine until the release.
    Button,
    /// A toggle: it switches on the click.
    Toggle,
    /// A slider being dragged: every edit of the drag joins `group`; `value` is the last one set.
    Slider { group: GroupId, value: f64 },
}

/// What the pointer does to a control this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    /// The primary button is down on it now.
    pub down: bool,
    /// Drawn pressed in: down on it, or a hold or drag that started on it goes on.
    pub pressed: bool,
}

/// One frame of the pointer on `control` of `node`, whose response `resp` covers `rect`. A
/// momentary button is held in the engine ([`Action::PressButton`]); a toggle's click and a
/// slider's drag set `param` (one history group per drag, from `new_group`). `control` shows
/// the new value at once. `press` is the press going on: kept by the caller across frames and
/// ended by [`release`]. Nothing happens unless `active`.
#[allow(clippy::too_many_arguments)]
pub fn interact(
    ui: &Ui,
    resp: &Response,
    rect: Rect,
    node: NodeId,
    control: &mut Control,
    param: Option<ParamPath>,
    active: bool,
    press: &mut Option<Press>,
    new_group: &dyn Fn() -> GroupId,
    actions: &mut Vec<Action>,
) -> Pointer {
    let primary = ui.input(|i| i.pointer.button_down(PointerButton::Primary) && !i.pointer.secondary_down());
    let down = resp.is_pointer_button_down_on() && primary;
    let mine = press.filter(|p| p.node == node).map(|p| p.what);
    if active {
        match control {
            Control::Button { toggle: false, .. } => {
                if down && mine.is_none() {
                    actions.push(Action::PressButton { node, down: true });
                    *press = Some(Press { node, what: Pressed::Button });
                } else if resp.clicked() && mine.is_none() {
                    // Pressed and released within one frame: a tap.
                    actions.push(Action::PressButton { node, down: true });
                    actions.push(Action::PressButton { node, down: false });
                }
            }
            Control::Button { on, .. } => {
                if down && mine.is_none() {
                    *press = Some(Press { node, what: Pressed::Toggle });
                }
                if resp.clicked()
                    && let Some(param) = param
                {
                    actions.push(Action::edit(vec![Op::Set { param, value: Value::Bool(!*on) }]));
                    *on = !*on;
                }
            }
            Control::Slider { value, min, max, .. } => {
                if down && let Some(p) = resp.interact_pointer_pos() {
                    let v = slider_at(rect, p.x, *min, *max);
                    let (group, last) = match mine {
                        Some(Pressed::Slider { group, value }) => (group, Some(value)),
                        _ => (new_group(), None),
                    };
                    let changed = match last {
                        Some(last) => last != v,
                        None => *value != v,
                    };
                    if changed && let Some(param) = param {
                        actions.push(Action::Edit {
                            ops: vec![Op::Set { param, value: Value::Float(v) }],
                            label: String::new(),
                            group: Some(group),
                        });
                    }
                    *press = Some(Press { node, what: Pressed::Slider { group, value: v } });
                    *value = v;
                }
            }
        }
    }
    let pressed = down || matches!(mine, Some(Pressed::Button | Pressed::Slider { .. }));
    Pointer { down, pressed }
}

/// The primary button went up: a momentary button held from a control is let go, a slider's
/// drag ends (its next drag is a new undo step).
pub fn release(ui: &Ui, press: &mut Option<Press>, actions: &mut Vec<Action>) {
    let Some(p) = *press else { return };
    if ui.input(|i| i.pointer.button_down(PointerButton::Primary)) {
        return;
    }
    if p.what == Pressed::Button {
        actions.push(Action::PressButton { node: p.node, down: false });
    }
    *press = None;
}

/// The cursor over a control: a hand on a button, arrows on a slider, "not allowed" when a
/// press would do nothing.
pub fn cursor(ui: &Ui, resp: &Response, control: &Control, active: bool) {
    if resp.hovered() {
        let icon = match (control, active) {
            (_, false) => CursorIcon::NotAllowed,
            (Control::Slider { .. }, true) => CursorIcon::ResizeHorizontal,
            (Control::Button { .. }, true) => CursorIcon::PointingHand,
        };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// The value the control's channel shows: 1 or 0 for a button (a momentary one also while
/// `pressed`), the slider's value.
pub fn shown_value(control: &Control, pressed: bool) -> f64 {
    match control {
        Control::Button { on, toggle, .. } => {
            if *on || (pressed && !*toggle) {
                1.0
            } else {
                0.0
            }
        }
        Control::Slider { value, .. } => *value,
    }
}

/// The value line's text:the channel's name and value (a lone `value` shows just the number).
pub fn line(name: &str, value: f64) -> String {
    let v = crate::editor::format_value(value);
    if name == "value" { v } else { format!("{name} {v}") }
}

/// The hover text of a control. `show`: on the show screen, where a change makes no history
/// entry.
pub fn tip(control: &Control, can_edit: bool, show: bool) -> String {
    let kept = "Kept without an undo step and saved when show mode ends";
    match control {
        _ if control.bound() => "value follows a channel: unbind it to set it here".to_string(),
        _ if control.edits() && !can_edit => "Claude is editing: wait to set it".to_string(),
        Control::Button { toggle: false, .. } => "1 while the mouse button is held here".to_string(),
        Control::Button { .. } if show => format!("A click switches between on (1) and off (0). {kept}"),
        Control::Button { .. } => "A click switches between on (1) and off (0)".to_string(),
        Control::Slider { min, max, .. } => {
            let undo = if show { kept } else { "One drag is one undo step" };
            format!(
                "Drag to set the value between {} and {}. {undo}",
                crate::editor::format_value(*min),
                crate::editor::format_value(*max)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_positions_become_round_values() {
        let r = Rect::from_min_size(pos2(10.0, 0.0), vec2(100.0, 20.0));
        assert_eq!(slider_at(r, 10.0, 0.0, 1.0), 0.0);
        assert_eq!(slider_at(r, 110.0, 0.0, 1.0), 1.0);
        assert_eq!(slider_at(r, 500.0, 0.0, 1.0), 1.0, "past the end: max");
        assert_eq!(slider_at(r, -50.0, 0.0, 1.0), 0.0);
        assert_eq!(slider_at(r, 53.7, 0.0, 1.0), 0.437);
        assert_eq!(slider_at(r, 60.0, -10.0, 10.0), 0.0);
        assert_eq!(slider_at(r, 60.0, 1.0, 0.0), 0.5, "max below min runs the other way");
    }

    #[test]
    fn the_control_leaves_the_value_line_and_the_port_free() {
        let body = Rect::from_min_size(pos2(0.0, 20.0), vec2(150.0, BODY));
        let r = rect(body, 1.0, 11.0);
        assert!(r.min.y >= body.min.y + LINE && r.max.y < body.max.y);
        assert!(r.max.x <= body.max.x - 11.0, "clear of the output port");
        // The node's centre (header 20 plus body) lies above the control.
        let centre_y = (body.max.y + 0.0) / 2.0;
        assert!(!r.contains(pos2(75.0, centre_y)));
    }
}
