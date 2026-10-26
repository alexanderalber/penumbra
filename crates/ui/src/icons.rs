//! Icons for action buttons, after the website's convention (`alexanderalber.github.io`,
//! `assets/js/icons.js`): 24x24 path data, no fill, stroke 2 with round caps, stroked in the
//! button's text colour. Only buttons that perform an action get one; choices, toggles and tabs
//! stay bare, removal is an X. The path data is the website's where it has the icon (`play`,
//! `pause`, `remove`, `reset`), so both read as one family; `undo`, `redo` and `take_over` are
//! drawn here in the same style, `bind` is Feather's `link` (MIT), which the website's set
//! lacks, and `info` is Feather's `info` (MIT), a circled "i".
//!
//! The painter understands the SVG path subset the icons use (`M L H V Z A`, absolute and
//! relative); anything else ends the path, it never panics.

use egui::{Atom, Color32, Id, Pos2, Rect, Response, Shape, Stroke, Ui, Vec2, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Remove,
    Reset,
    Undo,
    Redo,
    TakeOver,
    Bind,
    Folder,
    Info,
    Lock,
}

impl Icon {
    pub const ALL: [Icon; 11] = [
        Icon::Play,
        Icon::Pause,
        Icon::Remove,
        Icon::Reset,
        Icon::Undo,
        Icon::Redo,
        Icon::TakeOver,
        Icon::Bind,
        Icon::Folder,
        Icon::Info,
        Icon::Lock,
    ];

    /// The `d` attributes of the icon's paths.
    fn paths(self) -> &'static [&'static str] {
        match self {
            Icon::Play => &["M6 4l13 8-13 8z"],
            Icon::Pause => &["M8 4v16", "M16 4v16"],
            Icon::Remove => &["M18 6L6 18", "M6 6l12 12"],
            Icon::Reset => &["M3 12a9 9 0 1 0 3-6.7", "M3 4v5h5"],
            // A hooked arrow back to the left, and its mirror image.
            Icon::Undo => &["M9 14L4 9l5-5", "M4 9h11a5 5 0 0 1 0 10h-4"],
            Icon::Redo => &["M15 14l5-5-5-5", "M20 9H9a5 5 0 0 0 0 10h4"],
            // An arrow entering a door frame: take the editing over.
            Icon::TakeOver => &["M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4", "M10 17l5-5-5-5", "M15 12H3"],
            // Two chain links: drive a param with a channel.
            Icon::Bind => &[
                "M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71",
                "M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71",
            ],
            // A folder with a tab: the project menu.
            Icon::Folder => &["M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"],
            // A circled "i": what a param means.
            Icon::Info => &["M2 12a10 10 0 1 0 20 0a10 10 0 1 0-20 0", "M12 16v-4", "M12 8h.01"],
            // A padlock: show mode.
            Icon::Lock => &["M5 11h14v10H5z", "M8 11V7a4 4 0 0 1 8 0v4"],
        }
    }
}

/// Size of an icon in a button, in points (the website renders them at 14 px).
pub const SIZE: f32 = 14.0;

/// Paints `icon` into `rect` (scaled from 24x24) in `color`.
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let scale = rect.width().min(rect.height()) / 24.0;
    let origin = rect.center() - Vec2::splat(12.0 * scale);
    let stroke = Stroke::new((2.0 * scale).max(1.0), color);
    for d in icon.paths() {
        for (points, closed) in polylines(d) {
            let pts: Vec<Pos2> = points.iter().map(|p| origin + p.to_vec2() * scale).collect();
            match pts.as_slice() {
                [] => {}
                [p] => {
                    painter.circle_filled(*p, stroke.width / 2.0, color);
                }
                _ => {
                    // Round caps: a dot at each open end.
                    if !closed && let (Some(a), Some(b)) = (pts.first(), pts.last()) {
                        painter.circle_filled(*a, stroke.width / 2.0, color);
                        painter.circle_filled(*b, stroke.width / 2.0, color);
                    }
                    let shape =
                        if closed { Shape::closed_line(pts, stroke) } else { Shape::line(pts, stroke) };
                    painter.add(shape);
                }
            }
        }
    }
}

/// A button with an icon before its text. The icon takes the button's text colour (weak when
/// disabled), as on the website.
pub fn button(ui: &mut Ui, icon: Icon, text: &str) -> Response {
    let id = Id::new(("icon", icon as u8, text));
    let atom = Atom::custom(id, Vec2::splat(SIZE));
    let r = if text.is_empty() {
        egui::Button::new(atom).atom_ui(ui)
    } else {
        egui::Button::new((atom, text)).atom_ui(ui)
    };
    if let Some(rect) = r.rect(id) {
        let visuals = ui.style().interact(&r.response);
        let color = if r.response.enabled() {
            visuals.fg_stroke.color
        } else {
            ui.visuals().widgets.noninteractive.fg_stroke.color.gamma_multiply(0.5)
        };
        paint(ui.painter(), rect, icon, color);
    }
    r.response
}

/// An icon-only button without a frame (inline, e.g. a param's reset).
pub fn small_button(ui: &mut Ui, icon: Icon) -> Response {
    let (rect, r) = ui.allocate_exact_size(Vec2::splat(SIZE + 4.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let color =
            if r.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
        paint(ui.painter(), rect.shrink(2.0), icon, color);
    }
    r
}

/// The circled "i" that opens a param's help: smaller than [`small_button`] and weak, so a
/// column of them beside the param names stays quiet; strong when hovered, in the accent colour
/// while its help is `open`. Clickable even in a disabled `ui` (the param panel while Claude
/// edits): reading the help changes nothing, so `clicked` comes from the pointer then.
pub fn info_button(ui: &mut Ui, open: bool) -> (Response, bool) {
    let side = SIZE - 1.0;
    let (rect, r) = ui.allocate_exact_size(vec2(side, side), egui::Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let clicked =
        if r.enabled() { r.clicked() } else { hovered && ui.input(|i| i.pointer.primary_clicked()) };
    if ui.is_rect_visible(rect) {
        let color = if open {
            crate::theme::ACCENT
        } else if hovered {
            ui.visuals().strong_text_color()
        } else {
            crate::theme::TEXT_WEAK.gamma_multiply(0.8)
        };
        paint(ui.painter(), rect.shrink(0.5), Icon::Info, color);
    }
    (r, clicked)
}

/// The path as polylines in icon units, with whether each is closed.
fn polylines(d: &str) -> Vec<(Vec<Pos2>, bool)> {
    let mut out: Vec<(Vec<Pos2>, bool)> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut at = pos2(0.0, 0.0);
    let mut start = at;
    let mut tokens = Tokens { s: d.as_bytes(), i: 0 };
    let mut command = b'M';
    while let Some(c) = tokens.command_or_number() {
        if let Some(cmd) = c {
            command = cmd;
        }
        let rel = command.is_ascii_lowercase();
        let base = if rel { at.to_vec2() } else { Vec2::ZERO };
        match command.to_ascii_uppercase() {
            b'M' => {
                let (Some(x), Some(y)) = (tokens.number(), tokens.number()) else { break };
                if cur.len() > 1 {
                    out.push((std::mem::take(&mut cur), false));
                }
                at = pos2(x, y) + base;
                start = at;
                cur = vec![at];
                // Further pairs after a moveto are linetos.
                command = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let (Some(x), Some(y)) = (tokens.number(), tokens.number()) else { break };
                at = pos2(x, y) + base;
                cur.push(at);
            }
            b'H' => {
                let Some(x) = tokens.number() else { break };
                at.x = x + base.x;
                cur.push(at);
            }
            b'V' => {
                let Some(y) = tokens.number() else { break };
                at.y = y + base.y;
                cur.push(at);
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(x), Some(y)) = (
                    tokens.number(),
                    tokens.number(),
                    tokens.number(),
                    tokens.flag(),
                    tokens.flag(),
                    tokens.number(),
                    tokens.number(),
                ) else {
                    break;
                };
                let to = pos2(x, y) + base;
                arc(&mut cur, at, to, vec2(rx, ry), rot, large, sweep);
                at = to;
            }
            b'Z' => {
                if cur.len() > 1 {
                    out.push((std::mem::take(&mut cur), true));
                }
                at = start;
                cur = vec![at];
            }
            _ => break,
        }
    }
    if !cur.is_empty() {
        // A lone point is a dot (zero-length path with a round cap).
        let dot = cur.len() == 2 && cur.first() == cur.last();
        if dot {
            cur.truncate(1);
        }
        if cur.len() > 1 || dot {
            out.push((cur, false));
        }
    }
    out
}

/// Appends an SVG elliptical arc from `a` to `b` (endpoint parameterisation, SVG spec F.6.5).
fn arc(out: &mut Vec<Pos2>, a: Pos2, b: Pos2, r: Vec2, rot_deg: f32, large: bool, sweep: bool) {
    let (mut rx, mut ry) = (r.x.abs(), r.y.abs());
    if rx < 1e-6 || ry < 1e-6 || a == b {
        out.push(b);
        return;
    }
    let (sin, cos) = rot_deg.to_radians().sin_cos();
    let d = (a - b) / 2.0;
    let p = vec2(cos * d.x + sin * d.y, -sin * d.x + cos * d.y);
    let lambda = (p.x * p.x) / (rx * rx) + (p.y * p.y) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * p.y * p.y - ry * ry * p.x * p.x;
    let den = rx * rx * p.y * p.y + ry * ry * p.x * p.x;
    let mut k = if den > 0.0 { (num / den).max(0.0).sqrt() } else { 0.0 };
    if large == sweep {
        k = -k;
    }
    let c1 = vec2(k * rx * p.y / ry, -k * ry * p.x / rx);
    let mid = (a.to_vec2() + b.to_vec2()) / 2.0;
    let center = vec2(cos * c1.x - sin * c1.y, sin * c1.x + cos * c1.y) + mid;
    let angle = |v: Vec2| v.y.atan2(v.x);
    let theta1 = angle(vec2((p.x - c1.x) / rx, (p.y - c1.y) / ry));
    let mut delta = angle(vec2((-p.x - c1.x) / rx, (-p.y - c1.y) / ry)) - theta1;
    let tau = std::f32::consts::TAU;
    if sweep && delta < 0.0 {
        delta += tau;
    } else if !sweep && delta > 0.0 {
        delta -= tau;
    }
    let steps = ((delta.abs() / tau * 48.0).ceil() as usize).clamp(2, 64);
    for i in 1..=steps {
        let t = theta1 + delta * i as f32 / steps as f32;
        let (s, c) = t.sin_cos();
        let e = vec2(rx * c, ry * s);
        out.push((center + vec2(cos * e.x - sin * e.y, sin * e.x + cos * e.y)).to_pos2());
    }
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn skip(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace() || *c == b',') {
            self.i += 1;
        }
    }

    /// `Some(Some(cmd))` for a command letter, `Some(None)` when a number follows (an implicit
    /// repeat of the last command), `None` at the end.
    fn command_or_number(&mut self) -> Option<Option<u8>> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(Some(c))
        } else {
            Some(None)
        }
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip();
        let begin = self.i;
        let mut seen_dot = false;
        let mut seen_digit = false;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        while let Some(c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => seen_digit = true,
                b'.' if !seen_dot => seen_dot = true,
                _ => break,
            }
            self.i += 1;
        }
        if !seen_digit {
            self.i = begin;
            return None;
        }
        std::str::from_utf8(self.s.get(begin..self.i)?).ok()?.parse().ok().filter(|v: &f32| v.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_into_the_box() {
        for icon in Icon::ALL {
            for d in icon.paths() {
                let lines = polylines(d);
                assert!(!lines.is_empty(), "{icon:?}: {d}");
                for (pts, _) in lines {
                    for p in pts {
                        assert!(
                            (0.0..=24.0).contains(&p.x) && (0.0..=24.0).contains(&p.y),
                            "{icon:?}: {p:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn paths_follow_the_svg_rules() {
        // Relative moves and implicit linetos, compact numbers.
        let l = polylines("M6 4l13 8-13 8z");
        assert_eq!(l.len(), 1);
        assert!(l[0].1, "closed");
        assert_eq!(l[0].0, vec![pos2(6.0, 4.0), pos2(19.0, 12.0), pos2(6.0, 20.0)]);
        // A half circle from (0, 5) to (10, 5) around (5, 5) passes (5, 0) or (5, 10).
        let l = polylines("M0 5a5 5 0 0 1 10 0");
        let mid = l[0].0.iter().fold(f32::INFINITY, |m, p| m.min(p.y));
        assert!((mid - 0.0).abs() < 0.1, "sweep 1 goes through the top: {mid}");
        // Garbage ends the path quietly.
        assert!(polylines("M1 1 Q 2 2").is_empty());
        assert!(polylines("").is_empty());
    }
}
