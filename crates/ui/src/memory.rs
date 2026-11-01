//! Memory in the UI: how numbers of bytes read, the colour of the video memory share, and the
//! memory section of the cook time details (video memory against its budget as a bar, what
//! wgpu holds, the working set). The numbers come from [`penumbra_engine::Engine::memory`],
//! read every few seconds on a thread of their own, so drawing them costs nothing per frame.

use egui::{Color32, RichText, Sense, Stroke, Ui, pos2, vec2};
use penumbra_engine::memory::{INTERVAL, Memory};
use penumbra_gpu::vram::CREATE_PERCENT;

use crate::{automation, theme};

/// From this share of the budget the video memory shows in the warning colour.
pub const VRAM_WARN_PERCENT: f64 = 80.0;
/// From this share wgpu refuses new textures and buffers; the number shows in the error colour.
pub const VRAM_FULL_PERCENT: f64 = CREATE_PERCENT as f64;

const BAR_HEIGHT: f32 = 16.0;

/// A number of bytes with three significant digits in binary units: `512 B`, `12.3 KiB`,
/// `3.62 GiB`, `128 MiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    let digits = if v < 9.995 {
        2
    } else if v < 99.95 {
        1
    } else {
        0
    };
    format!("{v:.digits$} {}", UNITS.get(unit).copied().unwrap_or("TiB"))
}

/// The colour of a video memory share: the weak text colour while there is room, the warning
/// colour from [`VRAM_WARN_PERCENT`], the error colour from [`VRAM_FULL_PERCENT`].
pub fn vram_colour(percent: f64, normal: Color32) -> Color32 {
    if percent >= VRAM_FULL_PERCENT {
        theme::ERROR
    } else if percent >= VRAM_WARN_PERCENT {
        theme::WARNING
    } else {
        normal
    }
}

/// The status bar's text: `VRAM  16 %`, padded so a growing number does not shift what follows;
/// `None` where video memory is not known.
pub fn status_text(m: &Memory) -> Option<(String, Color32)> {
    let p = m.vram_percent()?;
    Some((format!("VRAM {p:>3.0} %"), vram_colour(p, theme::TEXT_WEAK)))
}

/// The hover text of the status bar's VRAM share.
pub fn status_hint() -> String {
    format!(
        "VRAM: video memory this process holds, in percent of what Windows grants it now. From \
         {VRAM_FULL_PERCENT:.0} % new textures are refused"
    )
}

/// The memory section of the cook time details, `width` wide; `m` from [`penumbra_engine::Engine::memory`].
pub fn section(ui: &mut Ui, m: &Memory, width: f32, auto: &mut automation::Registry) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("memory").color(theme::TEXT_WEAK).small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("read every {:.0} s", INTERVAL.as_secs_f64()))
                    .color(theme::TEXT_WEAK)
                    .small(),
            );
        });
    });
    if let Some(v) = m.vram {
        let r = bar(ui, v.usage, v.budget, width);
        auto.add("memory.vram", r.rect, "video memory against its budget");
    }
    let vram = m.vram.map_or("-".to_string(), |v| format!("{} of {}", bytes(v.usage), bytes(v.budget)));
    let (wgpu, wgpu_hint) = match m.wgpu {
        Some(w) => (
            bytes(w.allocated),
            format!(
                "Textures and buffers penumbra holds, as wgpu's allocator counts them: {} allocations \
                 in {} blocks of {} reserved. The rest of the video memory goes to the driver, \
                 the windows and the UI",
                w.allocations,
                w.blocks,
                bytes(w.reserved)
            ),
        ),
        None => ("-".to_string(), "wgpu's allocator does not report on this graphics backend".to_string()),
    };
    let working = m.process.map_or("-".to_string(), |p| bytes(p.working_set));
    let rows = [
        (
            "VRAM",
            vram,
            format!(
                "Dedicated video memory this process holds, of the budget Windows grants it now. \
                 The budget shrinks while other programs hold video memory. From \
                 {VRAM_FULL_PERCENT:.0} % of it new textures and buffers are refused"
            ),
        ),
        ("wgpu allocated", wgpu, wgpu_hint),
        (
            "working set",
            working,
            "This process's memory in RAM, as Windows counts it for the process".to_string(),
        ),
    ];
    egui::Grid::new("memory_numbers").num_columns(2).min_col_width(NAME_WIDTH).spacing(vec2(16.0, 2.0)).show(
        ui,
        |ui| {
            for (name, value, hint) in rows {
                ui.label(RichText::new(name).color(theme::TEXT_WEAK)).on_hover_text(hint);
                ui.label(RichText::new(value).monospace());
                ui.end_row();
            }
        },
    );
    if let Some(e) = &m.error {
        ui.label(RichText::new(e).color(theme::TEXT_WEAK).small());
    }
}

/// Width of the name column in the details' grids, so the cook and memory numbers line up.
pub const NAME_WIDTH: f32 = 96.0;

/// `used` of `budget` as a filled bar with the share on it, and a mark where wgpu starts to
/// refuse.
fn bar(ui: &mut Ui, used: u64, budget: u64, width: f32) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(width, BAR_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let percent = if budget > 0 { used as f64 * 100.0 / budget as f64 } else { 0.0 };
    let filled = (percent / 100.0).clamp(0.0, 1.0) as f32;
    let fill = vram_colour(percent, theme::ACCENT).gamma_multiply(0.55);
    painter.rect_filled(
        egui::Rect::from_min_size(rect.min, vec2(rect.width() * filled, rect.height())),
        2.0,
        fill,
    );
    let x = rect.left() + rect.width() * (VRAM_FULL_PERCENT / 100.0) as f32;
    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, theme::ERROR));
    let font = egui::FontId::monospace(11.0);
    painter.text(
        rect.left_center() + vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("{percent:.0} % of the budget"),
        font.clone(),
        theme::TEXT,
    );
    painter.text(
        pos2(x - 4.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        "refused from here",
        font,
        // Readable on the fill too, once the fill reaches the label.
        if percent > VRAM_WARN_PERCENT { theme::TEXT } else { theme::TEXT_WEAK },
    );
    r.on_hover_text(format!(
        "{} of {} in use. From the red mark ({VRAM_FULL_PERCENT:.0} %) new textures and buffers \
         are refused",
        bytes(used),
        bytes(budget)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_with_three_digits() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(12_595), "12.3 KiB");
        assert_eq!(bytes(128 << 20), "128 MiB");
        assert_eq!(bytes(3_886_000_000), "3.62 GiB");
        assert_eq!(bytes(24 << 30), "24.0 GiB");
        assert_eq!(bytes(u64::MAX), "16777216 TiB");
    }

    #[test]
    fn the_share_turns_yellow_then_red() {
        assert_eq!(vram_colour(79.9, theme::TEXT_WEAK), theme::TEXT_WEAK);
        assert_eq!(vram_colour(80.0, theme::TEXT_WEAK), theme::WARNING);
        assert_eq!(vram_colour(90.0, theme::TEXT_WEAK), theme::ERROR);
        let m = Memory { vram: Some(penumbra_gpu::Vram { budget: 100, usage: 85 }), ..Memory::default() };
        assert_eq!(status_text(&m), Some(("VRAM  85 %".to_string(), theme::WARNING)));
        assert_eq!(status_text(&Memory::default()), None);
    }
}
