//! The cook time in the status bar: the smoothed pass time as a button, and on click the last
//! seconds' passes as a plot with the numbers behind them and the machine's CPU and GPU load.

use std::time::Instant;

use egui::{Color32, PopupCloseBehavior, RectAlign, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use penumbra_engine::Engine;
use penumbra_engine::meter::{CookMeter, WINDOW};

use crate::{automation, theme};

/// The reference line: a frame at 60 Hz.
const FRAME_60_MS: f64 = 1000.0 / 60.0;
const PLOT_SIZE: egui::Vec2 = vec2(360.0, 120.0);
const PASS: Color32 = Color32::from_gray(120);

/// The status bar entry and its popup.
pub fn status(ui: &mut Ui, engine: &Engine, auto: &mut automation::Registry) {
    // Monospace and padded, so a cook time growing a digit does not shift everything to its
    // right.
    let text = match engine.cook_smoothed() {
        Some(ms) => format!("cook {ms:>6.2} ms"),
        None => "cook      - ms".to_string(),
    };
    let r = ui
        .add(
            egui::Button::new(RichText::new(text).monospace().color(theme::TEXT_WEAK))
                .frame_when_inactive(false),
        )
        .on_hover_text(
            "Time per cook pass, smoothed over about a second. Click for the last seconds and the load",
        );
    auto.add("status.cook", r.rect, "cook time");
    egui::Popup::from_toggle_button_response(&r)
        .align(RectAlign::TOP_START)
        .gap(4.0)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| details(ui, engine, auto));
}

fn details(ui: &mut Ui, engine: &Engine, auto: &mut automation::Registry) {
    let now = Instant::now();
    let meter = engine.cook_meter();
    let load = engine.load().latest();
    let s = meter.summary(now);
    ui.set_width(PLOT_SIZE.x);
    let r = plot(ui, &meter, s.high_ms, now);
    auto.add("cook.plot", r.rect, "cook time plot");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.label(RichText::new("\u{2014} each pass").color(PASS).small());
        ui.label(RichText::new("\u{2014} smoothed (1 s)").color(theme::ACCENT).small());
        ui.label(RichText::new("- - 60 fps").color(theme::WARNING).small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("last {:.0} s", WINDOW.as_secs_f64())).color(theme::TEXT_WEAK).small(),
            );
        });
    });
    ui.add_space(4.0);
    let ms = |v: f64| if s.passes > 0 { format!("{v:.2} ms") } else { "-".to_string() };
    let percent = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.0} %"));
    let rows = [
        ("last pass", ms(s.last_ms), "The newest cook pass"),
        (
            "smoothed",
            ms(s.smoothed_ms),
            "First-order lag with a time constant of 1 s (the status bar's number)",
        ),
        ("median", ms(s.median_ms), "Half the passes of the last 8 s were faster"),
        ("1 % high", ms(s.high_ms), "One pass in a hundred of the last 8 s was slower (99th percentile)"),
        ("slowest", ms(s.max_ms), "The slowest pass of the last 8 s"),
        (
            "cook thread",
            if s.passes > 0 { format!("{:.0} % busy", s.busy * 100.0) } else { "-".to_string() },
            "Share of the last 8 s the cook thread spent in passes. Near 100 % it cannot keep up, \
             whatever the CPU as a whole says",
        ),
        ("CPU", percent(load.cpu), "All cores of the machine, every process (as Task Manager)"),
        (
            "GPU",
            percent(load.gpu),
            "The busiest GPU engine, every process (as Task Manager). Cook passes only queue GPU \
             work, so a slow GPU shows here and not in the pass times",
        ),
    ];
    egui::Grid::new("cook_numbers").num_columns(2).spacing(vec2(16.0, 2.0)).show(ui, |ui| {
        for (name, value, hint) in rows {
            ui.label(RichText::new(name).color(theme::TEXT_WEAK)).on_hover_text(hint);
            ui.label(RichText::new(value).monospace());
            ui.end_row();
        }
    });
    if let Some(e) = &load.error {
        ui.label(RichText::new(e).color(theme::TEXT_WEAK).small());
    }
}

/// The passes of the last [`WINDOW`], newest on the right, with the 60 fps line dashed. The
/// scale reaches the 1 % high, not the slowest pass: one hitch (a shader compiling) would
/// otherwise flatten everything else; passes above it are cut off at the top.
fn plot(ui: &mut Ui, meter: &CookMeter, high_ms: f64, now: Instant) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(PLOT_SIZE, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let highest = meter.samples(now).map(|s| s.smoothed).fold(FRAME_60_MS.max(high_ms), f64::max);
    let top = scale(highest * 1.15);
    let window = WINDOW.as_secs_f64();
    let point = |at: Instant, ms: f64| {
        let ago = now.saturating_duration_since(at).as_secs_f64();
        let x = rect.right() - (ago / window) as f32 * rect.width();
        let y = rect.bottom() - (ms / top).min(1.0) as f32 * rect.height();
        pos2(x, y)
    };
    let passes: Vec<_> = meter.samples(now).map(|s| point(s.at, s.ms)).collect();
    let smoothed: Vec<_> = meter.samples(now).map(|s| point(s.at, s.smoothed)).collect();
    painter.add(Shape::line(passes, Stroke::new(1.0, PASS)));
    painter.add(Shape::line(smoothed, Stroke::new(1.5, theme::ACCENT)));
    let y = rect.bottom() - (FRAME_60_MS / top) as f32 * rect.height();
    painter.extend(Shape::dashed_line(
        &[pos2(rect.left(), y), pos2(rect.right(), y)],
        Stroke::new(1.0, theme::WARNING),
        6.0,
        4.0,
    ));
    painter.text(
        rect.left_top() + vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        format!("{top} ms"),
        egui::FontId::monospace(10.0),
        theme::TEXT_WEAK,
    );
    r
}

/// The plot's top: the next round number of milliseconds at or above `ms`.
fn scale(ms: f64) -> f64 {
    let mut decade = 1.0;
    loop {
        for step in [1.0, 2.0, 2.5, 5.0] {
            if step * decade >= ms || decade >= 1e6 {
                return step * decade;
            }
        }
        decade *= 10.0;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scale_rounds_up() {
        assert_eq!(super::scale(19.2), 20.0);
        assert_eq!(super::scale(20.5), 25.0);
        assert_eq!(super::scale(26.0), 50.0);
        assert_eq!(super::scale(130.0), 200.0);
    }
}
