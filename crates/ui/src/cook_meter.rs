//! The cook time in the status bar: the smoothed pass time as a button, and on click the last
//! seconds' passes as a plot with the numbers behind them and the machine's CPU and GPU load,
//! on NVIDIA GPUs also the GPU's power, temperature, clock and what holds the clock back.

use std::time::Instant;

use egui::{Color32, PopupCloseBehavior, RectAlign, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use penumbra_engine::Engine;
use penumbra_engine::meter::{CookMeter, WINDOW};
use penumbra_system::GpuSensors;

use crate::{automation, theme};

const PLOT_SIZE: egui::Vec2 = vec2(360.0, 120.0);
const PASS: Color32 = Color32::from_gray(120);

/// The status bar entry and its popup; `fps` is the show rate, whose frame time the plot draws
/// as its reference line.
pub fn status(ui: &mut Ui, engine: &Engine, fps: f64, auto: &mut automation::Registry) {
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
        .show(|ui| details(ui, engine, fps, auto));
}

fn details(ui: &mut Ui, engine: &Engine, fps: f64, auto: &mut automation::Registry) {
    let now = Instant::now();
    let meter = engine.cook_meter();
    let load = engine.load().latest();
    let s = meter.summary(now);
    ui.set_width(PLOT_SIZE.x);
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 60.0 };
    let r = plot(ui, &meter, s.high_ms, 1000.0 / fps, now);
    auto.add("cook.plot", r.rect, "cook time plot");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.label(RichText::new("\u{2014} each pass").color(PASS).small());
        ui.label(RichText::new("\u{2014} smoothed (1 s)").color(theme::ACCENT).small());
        ui.label(RichText::new(format!("- - {}", fps_label(fps))).color(theme::WARNING).small());
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
        for (name, value, hint) in load.nvidia.as_ref().map(sensor_rows).unwrap_or_default() {
            ui.label(RichText::new(name).color(theme::TEXT_WEAK)).on_hover_text(hint);
            ui.label(RichText::new(value).monospace());
            ui.end_row();
        }
    });
    if let Some(e) = &load.error {
        ui.label(RichText::new(e).color(theme::TEXT_WEAK).small());
    }
    if let Some(n) = load.nvidia.as_ref().filter(|n| !n.matched) {
        ui.label(
            RichText::new(format!("GPU sensors: {} (not found as the adapter penumbra renders on)", n.name))
                .color(theme::TEXT_WEAK)
                .small(),
        );
    }
}

/// The NVIDIA GPU's rows; a value the GPU did not report gets no row.
fn sensor_rows(n: &GpuSensors) -> Vec<(&'static str, String, String)> {
    let mut rows = Vec::new();
    let power = match (n.power_w, n.power_limit_w) {
        (Some(w), Some(limit)) => Some(format!("{w:.0} / {limit:.0} W")),
        (Some(w), None) => Some(format!("{w:.0} W")),
        _ => None,
    };
    if let Some(power) = power {
        rows.push((
            "GPU power",
            power,
            format!("{}: power draw and the limit the driver holds it to", n.name),
        ));
    }
    if let Some(t) = n.temperature_c {
        rows.push(("GPU temp", format!("{t} \u{b0}C"), format!("{}: the GPU die", n.name)));
    }
    if let Some(mhz) = n.clock_mhz {
        rows.push(("GPU clock", format!("{mhz} MHz"), format!("{}: graphics clock", n.name)));
    }
    if let Some(reasons) = &n.limited_by {
        rows.push((
            "GPU held by",
            if reasons.is_empty() { "nothing".to_string() } else { reasons.join(", ") },
            format!(
                "{}: what keeps the clock below its maximum right now. \"idle\" is normal without \
                 work; \"power cap\" or \"thermal\" under load mean the GPU runs as fast as its \
                 power limit or cooling allows",
                n.name
            ),
        ));
    }
    rows
}

/// The passes of the last [`WINDOW`], newest on the right, with one frame at the show rate
/// (`frame_ms`) as a dashed line. The
/// scale reaches the 1 % high, not the slowest pass: one hitch (a shader compiling) would
/// otherwise flatten everything else; passes above it are cut off at the top.
fn plot(ui: &mut Ui, meter: &CookMeter, high_ms: f64, frame_ms: f64, now: Instant) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(PLOT_SIZE, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let highest = meter.samples(now).map(|s| s.smoothed).fold(frame_ms.max(high_ms), f64::max);
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
    let y = rect.bottom() - (frame_ms / top) as f32 * rect.height();
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

/// A rate for the legend: whole rates without decimals, others (29.97) with two.
fn fps_label(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.005 { format!("{fps:.0} fps") } else { format!("{fps:.2} fps") }
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

    #[test]
    fn the_legend_names_the_rate() {
        assert_eq!(super::fps_label(50.0), "50 fps");
        assert_eq!(super::fps_label(60.0), "60 fps");
        assert_eq!(super::fps_label(30000.0 / 1001.0), "29.97 fps");
    }
}
