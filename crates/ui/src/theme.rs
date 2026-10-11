//! Colours and small glyphs shared by the panels.

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use penumbra_graph::DataKind;

pub const TEXT: Color32 = Color32::from_gray(220);
pub const TEXT_WEAK: Color32 = Color32::from_gray(140);
pub const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);
/// Claude's colour: the editing-mode tint, its change-log entries, nodes it just touched.
pub const CLAUDE: Color32 = Color32::from_rgb(110, 170, 255);
pub const FILE: Color32 = Color32::from_rgb(90, 200, 180);
pub const ERROR: Color32 = Color32::from_rgb(235, 80, 70);
pub const WARNING: Color32 = Color32::from_rgb(240, 180, 60);
/// A single channel's value bar while the value is negative.
pub const NEGATIVE: Color32 = Color32::from_rgb(225, 95, 85);

/// Editor background while the user edits (dark grey) and while Claude edits (dark blue).
pub const BG_USER: Color32 = Color32::from_gray(28);
pub const BG_CLAUDE: Color32 = Color32::from_rgb(18, 30, 58);

/// The whole window's look: egui's dark theme while the user edits, the same turned dark blue
/// while Claude edits, so the mode reads as a global theme switch (every panel, not only the
/// editor). Widgets stay usable to look at; editing ones are disabled by the panels.
pub fn visuals(claude: bool) -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    if !claude {
        return v;
    }
    // Every grey of the dark theme moved towards blue, keeping its brightness order.
    let blue = |g: Color32| {
        let l = f32::from(g.r());
        let c = |k: f32| (l * k).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_premultiplied(c(0.62), c(0.95), c(1.75).max(g.r()), g.a())
    };
    v.panel_fill = blue(v.panel_fill);
    v.window_fill = blue(v.window_fill);
    v.extreme_bg_color = blue(v.extreme_bg_color);
    v.faint_bg_color = blue(v.faint_bg_color);
    v.code_bg_color = blue(v.code_bg_color);
    v.window_stroke.color = blue(v.window_stroke.color);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = blue(w.bg_fill);
        w.weak_bg_fill = blue(w.weak_bg_fill);
        w.bg_stroke.color = blue(w.bg_stroke.color);
    }
    v
}

/// Background of picture areas (viewer) and of the canvas in the current mode.
pub fn picture_bg(claude: bool) -> Color32 {
    if claude { Color32::from_rgb(10, 16, 32) } else { Color32::from_gray(16) }
}

/// How long nodes touched by Claude or the file stay outlined.
pub const RECENT_SECONDS: f32 = 8.0;

pub fn kind_color(kind: DataKind) -> Color32 {
    match kind {
        DataKind::Tex => Color32::from_rgb(190, 120, 240),
        DataKind::Chan => Color32::from_rgb(110, 210, 120),
        DataKind::Table => Color32::from_rgb(110, 170, 240),
        DataKind::Geo => Color32::from_rgb(240, 160, 80),
        DataKind::Mat => Color32::from_rgb(230, 110, 160),
        DataKind::Light => Color32::from_rgb(235, 215, 120),
        DataKind::View => Color32::from_rgb(200, 200, 220),
    }
}

/// Colour of channel `i` in curves and lists: the family's green first, then distinct hues.
pub fn channel_color(i: usize) -> Color32 {
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(110, 210, 120),
        Color32::from_rgb(240, 160, 80),
        Color32::from_rgb(110, 170, 240),
        Color32::from_rgb(230, 110, 160),
        Color32::from_rgb(220, 210, 90),
        Color32::from_rgb(170, 130, 240),
    ];
    COLORS.get(i % COLORS.len()).copied().unwrap_or(Color32::WHITE)
}

/// Header colour of a node family (`tex`, `chan`, ...).
pub fn family_color(family: &str) -> Color32 {
    let c = match family {
        "tex" => kind_color(DataKind::Tex),
        "chan" => kind_color(DataKind::Chan),
        "table" => kind_color(DataKind::Table),
        "geo" => kind_color(DataKind::Geo),
        "mat" => kind_color(DataKind::Mat),
        "light" => kind_color(DataKind::Light),
        "view" => kind_color(DataKind::View),
        // Scripts output channels and tables: neither family's colour, Python's yellow.
        "script" => Color32::from_rgb(225, 205, 80),
        // Components hold nodes of every family: a colour of their own, turquoise, away from
        // tex's violet (the two looked alike).
        "comp" => Color32::from_rgb(60, 190, 180),
        _ => Color32::from_gray(120),
    };
    c.gamma_multiply(0.55)
}

/// Node body colour for a cook load (average cook time / frame period): neutral when cheap,
/// towards red as a node takes half a frame or more.
pub fn load_tint(load: f64) -> Color32 {
    let base = Color32::from_gray(44);
    let hot = Color32::from_rgb(130, 40, 36);
    let t = if load.is_finite() { (load / 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
    // Below about 2 % of a frame nothing shows, so ordinary nodes stay calm.
    let t = ((t - 0.04) / 0.96).max(0.0).sqrt();
    lerp(base, hot, t)
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix =
        |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A checkerboard behind pictures, so transparency is visible.
pub fn checker(painter: &egui::Painter, rect: Rect, zoom: f32) {
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(60));
    let cell = (8.0 * zoom).max(4.0);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    if cols * rows > 4096 {
        return;
    }
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                continue;
            }
            let min = rect.min + vec2(x as f32 * cell, y as f32 * cell);
            let r = Rect::from_min_size(min, vec2(cell, cell)).intersect(rect);
            painter.rect_filled(r, CornerRadius::ZERO, Color32::from_gray(80));
        }
    }
}

/// A small padlock centred at `c`.
pub fn lock_glyph(painter: &egui::Painter, c: Pos2, zoom: f32) {
    let body = Rect::from_center_size(c + vec2(0.0, 2.0 * zoom), vec2(9.0 * zoom, 7.0 * zoom));
    painter.rect_filled(body, CornerRadius::same(1), Color32::WHITE);
    let shackle = Rect::from_center_size(pos2(c.x, c.y - 2.5 * zoom), vec2(6.0 * zoom, 7.0 * zoom));
    painter.rect_stroke(
        shackle,
        CornerRadius::same(3),
        Stroke::new(1.5 * zoom, Color32::WHITE),
        StrokeKind::Middle,
    );
}
