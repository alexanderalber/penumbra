//! The viewer's channel plot: curves with a labelled value axis and, along the bottom, time in
//! seconds before now (or the sample index, for samples that are not moments), either on one
//! shared scale or as stacked lanes, each channel on a scale of its own.
//!
//! [`body`] is the small plot in a chan node's body: curves only, downsampled to its width.
//!
//! Everything here takes data as it comes: non-finite values are skipped, a flat channel gets a
//! range around its value, and a plot too small for labels draws the curves alone.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, pos2, vec2};

use penumbra_core::Chans;

use crate::editor::format_value;
use crate::theme;

/// One curve: its colour index, its name and value now, and its points as (x, value), x in the
/// units of the plot's [`XAxis`].
pub struct Series<'a> {
    pub color: usize,
    pub name: &'a str,
    pub value: f64,
    pub points: Vec<(f64, f64)>,
}

/// What runs along the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum XAxis {
    /// Seconds relative to now, from `-span` at the left edge to 0 at the right.
    Seconds { span: f64 },
    /// Sample index, from 0 at the left edge to `last` at the right.
    Index { last: f64 },
}

impl XAxis {
    fn range(self) -> (f64, f64) {
        match self {
            XAxis::Seconds { span } => (-finite_or(span, 1.0).abs().max(1e-9), 0.0),
            XAxis::Index { last } => (0.0, finite_or(last, 1.0).max(1.0)),
        }
    }
}

/// How the channels share the value axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Shared,
    Lanes,
}

/// A lane narrower than this (in points) is not drawn; the rest are counted.
const MIN_LANE: f32 = 16.0;
/// About this much room per value label, in points.
const VALUE_TICK_SPACING: f32 = 30.0;
/// About this much room per time label, in points.
const TIME_TICK_SPACING: f32 = 64.0;
/// Below this width or height the plot draws no labels.
const MIN_LABELLED: f32 = 90.0;

fn label_font() -> FontId {
    FontId::monospace(10.0)
}

fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

/// The step nearest to `raw` of 1, 2 or 5 times a power of ten.
fn nice_step(raw: f64) -> Option<f64> {
    if !(raw.is_finite() && raw > 0.0) {
        return None;
    }
    let power = 10f64.powf(raw.log10().floor());
    let fraction = raw / power;
    let nice = if fraction < 1.5 {
        1.0
    } else if fraction < 3.0 {
        2.0
    } else if fraction < 7.0 {
        5.0
    } else {
        10.0
    };
    let step = nice * power;
    (step.is_finite() && step > 0.0).then_some(step)
}

/// Round values in `lo..=hi`, about `count` of them (at least one when the range holds one),
/// spaced 1, 2 or 5 times a power of ten, and that spacing. Empty for a range that is not one.
pub fn nice_ticks(lo: f64, hi: f64, count: usize) -> (Vec<f64>, f64) {
    if !(lo.is_finite() && hi.is_finite() && hi > lo) {
        return (Vec::new(), 0.0);
    }
    let Some(step) = nice_step((hi - lo) / count.clamp(1, 50) as f64) else {
        return (Vec::new(), 0.0);
    };
    let first = (lo / step).ceil();
    let mut ticks = Vec::new();
    for k in 0..=100 {
        let v = (first + f64::from(k)) * step;
        if v > hi + step * 1e-9 {
            break;
        }
        // No "-0" for a tick that should be 0.
        ticks.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
    }
    (ticks, step)
}

/// `v` written with as many decimals as ticks `step` apart need.
pub fn tick_label(v: f64, step: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let big = v.abs().max(step.abs());
    if big >= 1e6 || (big > 0.0 && big < 1e-4) {
        return format!("{v:.1e}");
    }
    let decimals = if step > 0.0 { (-step.log10().floor()).clamp(0.0, 6.0) as usize } else { 2 };
    format!("{v:.decimals$}")
}

/// A time tick: seconds, or milliseconds when the ticks are less than a second apart and the
/// whole span is short.
fn time_label(v: f64, step: f64, span: f64) -> String {
    if span < 1.0 {
        let ms = v * 1000.0;
        format!("{} ms", tick_label(ms, step * 1000.0))
    } else {
        format!("{} s", tick_label(v, step))
    }
}

/// `lo .. hi` as a lane's range, with the decimals a tenth of it needs; a flat channel's value
/// alone.
fn range_text(lo: f64, hi: f64) -> String {
    match nice_step((hi - lo) / 10.0) {
        Some(step) => format!("{} .. {}", tick_label(lo, step), tick_label(hi, step)),
        None => format!("all {}", format_value(lo)),
    }
}

/// The lowest and highest finite value of the series.
fn range_of<'a>(points: impl Iterator<Item = &'a (f64, f64)>) -> Option<(f64, f64)> {
    let (lo, hi) = points
        .map(|(_, v)| *v)
        .filter(|v| v.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// A range for drawing `lo..hi`: a little room above and below, and a flat channel spread
/// around its value.
fn padded(lo: f64, hi: f64) -> (f64, f64) {
    let span = hi - lo;
    if !span.is_finite() || span <= lo.abs().max(hi.abs()) * 1e-9 || span < 1e-12 {
        let pad = (lo.abs() * 0.1).max(1.0);
        return (lo - pad, hi + pad);
    }
    (lo - span * 0.06, hi + span * 0.06)
}

/// Whether "auto" draws lanes: when a channel that moves covers less than a tenth of the range
/// all channels span together (an analysis with `bpm` near 120 next to levels of 0 to 1), it
/// would be a flat line on the shared scale. `before` is the last decision; it holds until the
/// ratio is clearly past the other side (a fifth), so a plot does not flip back and forth.
pub fn auto_lanes(series: &[Series<'_>], before: bool) -> bool {
    if series.len() < 2 {
        return false;
    }
    let ranges: Vec<(f64, f64)> = series.iter().filter_map(|s| range_of(s.points.iter())).collect();
    lanes_for(&ranges, before)
}

/// [`auto_lanes`] on the channels' ranges (lowest and highest value of each).
fn lanes_for(ranges: &[(f64, f64)], before: bool) -> bool {
    let (Some(lo), Some(hi)) =
        (ranges.iter().map(|r| r.0).reduce(f64::min), ranges.iter().map(|r| r.1).reduce(f64::max))
    else {
        return before;
    };
    let all = hi - lo;
    if !(all.is_finite() && all > 0.0) {
        return false;
    }
    // Channels standing still say nothing about the scale they need.
    let smallest = ranges.iter().map(|(a, b)| b - a).filter(|s| *s > all * 1e-9).reduce(f64::min);
    let Some(smallest) = smallest else { return false };
    let ratio = smallest / all;
    if before { ratio < 0.2 } else { ratio < 0.1 }
}

/// The plot of `series` in `rect`: `Scale::Shared` with the names and values listed at the top
/// left, `Scale::Lanes` with them at the top of each lane. `more` series were left out before;
/// they are counted with those whose lanes do not fit.
pub fn draw(painter: &Painter, rect: Rect, series: &[Series<'_>], more: usize, x: XAxis, scale: Scale) {
    let labelled = rect.width() >= MIN_LABELLED && rect.height() >= MIN_LABELLED;
    let font = label_font();
    let text_h = painter.layout_no_wrap("0".into(), font.clone(), theme::TEXT_WEAK).size().y;
    // Room for the time labels at the bottom.
    let bottom = if labelled { text_h + 6.0 } else { 0.0 };
    let outer = Rect::from_min_max(rect.min + vec2(0.0, 8.0), rect.max - vec2(10.0, 6.0 + bottom));
    if outer.height() < 8.0 || outer.width() < 8.0 {
        return;
    }
    let lanes: Vec<(Rect, Vec<&Series<'_>>)> = match scale {
        Scale::Shared => vec![(outer, series.iter().collect())],
        Scale::Lanes => {
            let fit = ((outer.height() / MIN_LANE).floor() as usize).max(1);
            let n = series.len().min(fit).max(1);
            let h = outer.height() / n as f32;
            series
                .iter()
                .take(n)
                .enumerate()
                .map(|(k, s)| {
                    let top = outer.min.y + h * k as f32;
                    (Rect::from_min_max(pos2(outer.min.x, top), pos2(outer.max.x, top + h)), vec![s])
                })
                .collect()
        }
    };
    let hidden = more + if scale == Scale::Lanes { series.len().saturating_sub(lanes.len()) } else { 0 };
    // Each lane's range and value ticks, then the gutter as wide as the widest label.
    type Lane<'s, 'a> = (Rect, Vec<&'s Series<'a>>, (f64, f64), (f64, f64), Vec<(f64, String)>);
    let scaled: Vec<Lane<'_, '_>> = lanes
        .into_iter()
        .map(|(r, ss)| {
            let data = range_of(ss.iter().flat_map(|s| s.points.iter())).unwrap_or((0.0, 1.0));
            let (lo, hi) = padded(data.0, data.1);
            let count = ((r.height() / VALUE_TICK_SPACING).floor() as usize).max(1);
            let (ticks, step) = if labelled { nice_ticks(lo, hi, count) } else { (Vec::new(), 0.0) };
            let labels = ticks.into_iter().map(|v| (v, tick_label(v, step))).collect();
            (r, ss, data, (lo, hi), labels)
        })
        .collect();
    let gutter = if labelled {
        let widest = scaled
            .iter()
            .flat_map(|(_, _, _, _, l)| l.iter())
            .map(|(_, t)| painter.layout_no_wrap(t.clone(), font.clone(), theme::TEXT_WEAK).size().x)
            .fold(0.0f32, f32::max);
        widest + 8.0
    } else {
        0.0
    };
    let plot_x = (outer.min.x + gutter, outer.max.x);
    let (x0, x1) = x.range();
    let to_x = |v: f64| plot_x.0 + ((v - x0) / (x1 - x0)) as f32 * (plot_x.1 - plot_x.0);
    let grid = Stroke::new(1.0, Color32::from_white_alpha(14));
    // Channels left out are counted at the bottom left, under the value labels, or at the top
    // right of a plot without labels.
    let more_label = (hidden > 0).then(|| {
        let galley = painter.layout_no_wrap(format!("+{hidden} more"), font.clone(), theme::TEXT_WEAK);
        let at = if labelled {
            pos2(outer.min.x, outer.max.y + 3.0)
        } else {
            pos2(outer.max.x - galley.size().x, outer.min.y)
        };
        let right = at.x + galley.size().x;
        backdrop(painter, Rect::from_min_size(at, galley.size()));
        painter.galley(at, galley, theme::TEXT_WEAK);
        if labelled { right } else { f32::NEG_INFINITY }
    });
    let time_left = more_label.unwrap_or(f32::NEG_INFINITY);
    // Time (or index) grid and labels.
    if labelled {
        let count = (((plot_x.1 - plot_x.0) / TIME_TICK_SPACING).floor() as usize).max(1);
        let (ticks, step) = nice_ticks(x0, x1, count);
        // From the right, so the label at now (or the last index) is always there.
        let mut left_of_last = f32::INFINITY;
        for v in ticks.into_iter().rev() {
            let px = to_x(v);
            painter.line_segment([pos2(px, outer.min.y), pos2(px, outer.max.y)], grid);
            let text = match x {
                XAxis::Seconds { .. } => time_label(v, step, x1 - x0),
                XAxis::Index { .. } => tick_label(v, step),
            };
            let galley = painter.layout_no_wrap(text, font.clone(), theme::TEXT_WEAK);
            let w = galley.size().x;
            // Centred under the tick, kept inside the plot, and never over the label beside.
            let left = (px - w / 2.0).min(plot_x.1 - w).max(plot_x.0);
            if left + w > left_of_last - 6.0 || left < time_left + 6.0 {
                continue;
            }
            left_of_last = left;
            painter.galley(pos2(left, outer.max.y + 3.0), galley, theme::TEXT_WEAK);
        }
    }
    let shared = scale == Scale::Shared;
    for (k, (r, ss, data, (lo, hi), labels)) in scaled.iter().enumerate() {
        let to_y = |v: f64| r.max.y - ((v - lo) / (hi - lo)) as f32 * r.height();
        if k > 0 {
            painter.line_segment(
                [pos2(outer.min.x, r.min.y), pos2(outer.max.x, r.min.y)],
                Stroke::new(1.0, Color32::from_white_alpha(40)),
            );
        }
        // Value grid and labels, a label left out when it would touch the one above or reach
        // out of its lane.
        let mut above = f32::NEG_INFINITY;
        let mut placed = 0;
        for (v, text) in labels.iter().rev() {
            let py = to_y(*v);
            if *v == 0.0 && *lo < 0.0 && *hi > 0.0 {
                painter.line_segment(
                    [pos2(plot_x.0, py), pos2(plot_x.1, py)],
                    Stroke::new(1.0, Color32::from_white_alpha(32)),
                );
            } else {
                painter.line_segment([pos2(plot_x.0, py), pos2(plot_x.1, py)], grid);
            }
            let (top, bottom) = (py - text_h / 2.0, py + text_h / 2.0);
            let outside = !shared && (top < r.min.y || bottom > r.max.y);
            if top < above + 1.0 || top < r.min.y - 2.0 || outside {
                continue;
            }
            above = bottom;
            placed += 1;
            painter.text(
                pos2(plot_x.0 - 5.0, py),
                Align2::RIGHT_CENTER,
                text,
                font.clone(),
                theme::TEXT_WEAK,
            );
        }
        if labelled {
            painter.line_segment(
                [pos2(plot_x.0, r.min.y), pos2(plot_x.0, r.max.y)],
                Stroke::new(1.0, Color32::from_white_alpha(40)),
            );
        }
        let clip =
            painter.with_clip_rect(Rect::from_min_max(pos2(plot_x.0, r.min.y), pos2(plot_x.1, r.max.y)));
        for s in ss {
            let points: Vec<Pos2> = s
                .points
                .iter()
                .filter(|(px, v)| px.is_finite() && v.is_finite())
                .map(|(px, v)| pos2(to_x(*px), to_y(*v)))
                .collect();
            if points.len() >= 2 {
                clip.add(egui::Shape::line(points, Stroke::new(1.5, theme::channel_color(s.color))));
            }
        }
        if !shared && let Some(s) = ss.first() {
            let at = pos2(plot_x.0 + 6.0, r.min.y + 2.0);
            let name = painter.layout_no_wrap(
                format!("{}  {}", s.name, format_value(s.value)),
                FontId::monospace(11.0),
                theme::channel_color(s.color),
            );
            // A lane too low for two labels on its axis says its range in words.
            // Only where it leaves room for the curve.
            let range = (labelled && placed < 2)
                .then(|| painter.layout_no_wrap(range_text(data.0, data.1), font.clone(), theme::TEXT_WEAK))
                .filter(|g| name.size().x + g.size().x + 10.0 < (plot_x.1 - plot_x.0) * 0.6);
            let width = name.size().x + range.as_ref().map_or(0.0, |g| g.size().x + 10.0);
            backdrop(painter, Rect::from_min_size(at, vec2(width, name.size().y)));
            let name_h = name.size().y;
            painter.galley(at, name, theme::TEXT);
            if let Some(range) = range {
                let y = at.y + (name_h - range.size().y) / 2.0;
                painter.galley(pos2(at.x + width - range.size().x, y), range, theme::TEXT_WEAK);
            }
        }
    }
    if shared {
        list(painter, pos2(plot_x.0 + 6.0, outer.min.y + 2.0), series);
    }
}

/// The names and values of a shared plot, one line each; a single channel named `value` (a
/// generator's) shows its value large.
fn list(painter: &Painter, at: Pos2, series: &[Series<'_>]) {
    let single = series.len() == 1;
    let font = if single { FontId::monospace(22.0) } else { FontId::monospace(13.0) };
    let lines: Vec<_> = series
        .iter()
        .map(|s| {
            let text = if single && s.name == "value" {
                format_value(s.value)
            } else {
                format!("{}  {}", s.name, format_value(s.value))
            };
            let color = if single { theme::TEXT } else { theme::channel_color(s.color) };
            painter.layout_no_wrap(text, font.clone(), color)
        })
        .collect();
    let width = lines.iter().map(|g| g.size().x).fold(0.0f32, f32::max);
    let height: f32 = lines.iter().map(|g| g.size().y + 2.0).sum();
    backdrop(painter, Rect::from_min_size(at, vec2(width, height - 2.0)));
    let mut y = at.y;
    for g in lines {
        let h = g.size().y;
        painter.galley(pos2(at.x, y), g, theme::TEXT);
        y += h + 2.0;
    }
}

/// Most curves a node body draws; the rest are left out.
const BODY_CURVES: usize = 8;
/// A lane in a node body is at least this tall (graph units), else the channels share a scale.
const BODY_MIN_LANE: f32 = 12.0;

/// The small plot in a chan node's body at `zoom`: each channel's samples as a curve, the
/// newest at the right edge and a window that is not full yet (a growing `chan.trail`) growing
/// in from there; stacked lanes when a moving channel would be flat on the shared scale (as the
/// viewer's `auto`), a faint zero line where the range crosses 0, and the first channel's value
/// now at the top left, `text_left` points further right (past the input labels). Costs one
/// pass over the samples and a few points per pixel of width.
pub fn body(painter: &Painter, rect: Rect, chans: &Chans, zoom: f32, text_left: f32) {
    let channels = chans.channels();
    let channels = channels.get(..channels.len().min(BODY_CURVES)).unwrap_or_default();
    let area = rect.shrink2(vec2(5.0, 4.0) * zoom);
    if channels.is_empty() || area.width() < 4.0 || area.height() < 4.0 {
        return;
    }
    let ranges: Vec<Option<(f64, f64)>> = channels.iter().map(|c| range_of_samples(&c.samples)).collect();
    let found: Vec<(f64, f64)> = ranges.iter().flatten().copied().collect();
    let lanes = channels.len() >= 2
        && area.height() / channels.len() as f32 >= BODY_MIN_LANE * zoom
        && lanes_for(&found, false);
    let all = found.iter().copied().reduce(|a, b| (a.0.min(b.0), a.1.max(b.1))).unwrap_or((0.0, 1.0));
    let lane_h = if lanes { area.height() / channels.len() as f32 } else { area.height() };
    let span = (chans.window().max(2) - 1) as f32;
    let offset = chans.window().saturating_sub(chans.len());
    let stroke = (1.2 * zoom).clamp(1.0, 2.0);
    let faint = Stroke::new(1.0, Color32::from_white_alpha(28));
    for (i, c) in channels.iter().enumerate() {
        let lane = if lanes {
            let top = area.min.y + lane_h * i as f32;
            Rect::from_min_max(pos2(area.min.x, top), pos2(area.max.x, top + lane_h))
        } else {
            area
        };
        let range = if lanes { ranges.get(i).copied().flatten().unwrap_or(all) } else { all };
        let (lo, hi) = padded(range.0, range.1);
        let to_y = |v: f64| lane.max.y - ((v - lo) / (hi - lo)) as f32 * lane.height();
        // Once per lane (once in all on a shared scale): the zero line and the lane border.
        if lanes || i == 0 {
            if lo < 0.0 && hi > 0.0 {
                let y = to_y(0.0);
                painter.line_segment([pos2(lane.min.x, y), pos2(lane.max.x, y)], faint);
            }
            if lanes && i > 0 {
                painter.line_segment([pos2(rect.min.x, lane.min.y), pos2(rect.max.x, lane.min.y)], faint);
            }
        }
        let to_x = |k: usize| lane.min.x + (offset + k) as f32 / span * lane.width();
        let points = envelope(&c.samples, lane.width(), to_x, to_y);
        if points.len() >= 2 {
            painter.add(egui::Shape::line(points, Stroke::new(stroke, theme::channel_color(i))));
        }
    }
    if zoom > 0.6
        && let Some(first) = channels.first()
    {
        let text = if channels.len() == 1 && first.name.as_str() == "value" {
            format_value(first.value())
        } else {
            format!("{} {}", first.name, format_value(first.value()))
        };
        let color = if channels.len() == 1 { theme::TEXT } else { theme::channel_color(0) };
        let galley = painter.layout_no_wrap(text, FontId::monospace(10.0 * zoom), color);
        let at = pos2(rect.min.x + text_left.max(0.0) + 4.0 * zoom, rect.min.y + 2.0 * zoom);
        if at.x + galley.size().x < rect.max.x - 4.0 * zoom {
            painter.rect_filled(
                Rect::from_min_size(at, galley.size()).expand2(vec2(2.0, 0.0) * zoom),
                2.0 * zoom,
                Color32::from_black_alpha(140),
            );
            painter.galley(at, galley, color);
        }
    }
}

/// The lowest and highest finite sample.
fn range_of_samples(samples: &[f64]) -> Option<(f64, f64)> {
    let (lo, hi) = samples
        .iter()
        .filter(|v| v.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// A pixel column of a body curve being gathered: its column, its x, and the (index, value) of
/// its lowest and highest sample.
type Bucket = (i64, f32, (usize, f64), (usize, f64));

/// The points of a curve through `samples` with at most two per pixel of `width`: a pixel that
/// covers several samples gets their lowest and highest, in the order they came, so a dense
/// signal (an audio block) keeps its swing instead of aliasing.
fn envelope(
    samples: &[f64],
    width: f32,
    to_x: impl Fn(usize) -> f32,
    to_y: impl Fn(f64) -> f32,
) -> Vec<Pos2> {
    let mut points = Vec::with_capacity(samples.len().min((width.max(1.0) as usize + 1) * 2));
    let flush = |points: &mut Vec<Pos2>, (_, x, lo, hi): Bucket| {
        if lo.0 == hi.0 {
            points.push(pos2(x, to_y(lo.1)));
        } else {
            let (a, b) = if lo.0 < hi.0 { (lo, hi) } else { (hi, lo) };
            points.push(pos2(x, to_y(a.1)));
            points.push(pos2(x, to_y(b.1)));
        }
    };
    let mut bucket: Option<Bucket> = None;
    for (k, v) in samples.iter().enumerate() {
        if !v.is_finite() {
            continue;
        }
        let x = to_x(k);
        let col = x.floor() as i64;
        match &mut bucket {
            Some((c, _, lo, hi)) if *c == col => {
                if *v < lo.1 {
                    *lo = (k, *v);
                }
                if *v > hi.1 {
                    *hi = (k, *v);
                }
            }
            _ => {
                if let Some(b) = bucket.take() {
                    flush(&mut points, b);
                }
                bucket = Some((col, x, (k, *v), (k, *v)));
            }
        }
    }
    if let Some(b) = bucket {
        flush(&mut points, b);
    }
    points
}

/// A dark patch behind text drawn over curves, so both stay readable.
fn backdrop(painter: &Painter, around: Rect) {
    painter.rect_filled(around.expand2(vec2(4.0, 1.0)), 3.0, Color32::from_black_alpha(150));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_round_and_inside() {
        let (t, step) = nice_ticks(-1.05, 1.05, 4);
        assert_eq!(step, 0.5);
        assert_eq!(t, [-1.0, -0.5, 0.0, 0.5, 1.0]);
        let (t, step) = nice_ticks(118.0, 122.0, 2);
        assert_eq!(step, 2.0);
        assert_eq!(t, [118.0, 120.0, 122.0]);
        let (t, _) = nice_ticks(-5.0, 0.0, 5);
        assert_eq!(t, [-5.0, -4.0, -3.0, -2.0, -1.0, 0.0]);
        for (lo, hi) in [(1.0, 1.0), (2.0, 1.0), (f64::NAN, 1.0), (0.0, f64::INFINITY)] {
            assert!(nice_ticks(lo, hi, 4).0.is_empty(), "{lo} {hi}");
        }
        // A huge range or count stays bounded.
        assert!(nice_ticks(-1e300, 1e300, 1000).0.len() <= 101);
        assert!(nice_ticks(0.0, 1e-300, 4).0.len() <= 101);
    }

    #[test]
    fn labels_have_the_decimals_the_step_needs() {
        assert_eq!(tick_label(0.5, 0.5), "0.5");
        assert_eq!(tick_label(120.0, 2.0), "120");
        assert_eq!(tick_label(0.25, 0.05), "0.25");
        assert_eq!(tick_label(-0.002, 0.001), "-0.002");
        assert_eq!(tick_label(2e7, 1e7), "2.0e7");
        assert_eq!(tick_label(2.2e-5, 1e-5), "2.2e-5");
        assert_eq!(tick_label(92.975, 1e-5), "92.97500");
        assert_eq!(tick_label(f64::NAN, 1.0), "");
        assert_eq!(time_label(-2.0, 1.0, 5.0), "-2 s");
        assert_eq!(time_label(-0.01, 0.005, 0.02), "-10 ms");
        assert_eq!(range_text(0.0, 1.0), "0.0 .. 1.0");
        assert_eq!(range_text(88.5, 92.0), "88.5 .. 92.0");
        assert_eq!(range_text(2.0, 2.0), "all 2.000");
    }

    fn s(points: &[f64]) -> Series<'static> {
        Series {
            color: 0,
            name: "x",
            value: 0.0,
            points: points.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect(),
        }
    }

    #[test]
    fn auto_draws_lanes_when_a_moving_channel_would_be_flat() {
        // bpm near 120 next to a level of 0 to 1.
        let analysis = [s(&[0.0, 1.0, 0.5]), s(&[119.0, 121.0, 120.0])];
        assert!(auto_lanes(&analysis, false));
        // Two lfos of about the same swing, and a constant next to an lfo.
        assert!(!auto_lanes(&[s(&[-1.0, 1.0]), s(&[-0.5, 0.5])], false));
        assert!(!auto_lanes(&[s(&[-1.0, 1.0]), s(&[0.5, 0.5])], false));
        // Hysteresis: 0.15 of the range keeps lanes, does not start them.
        let near = [s(&[0.0, 0.15]), s(&[0.0, 1.0])];
        assert!(!auto_lanes(&near, false));
        assert!(auto_lanes(&near, true));
        // One channel, no channel, nothing finite.
        assert!(!auto_lanes(&[s(&[0.0, 100.0])], true));
        assert!(!auto_lanes(&[], true));
        assert!(auto_lanes(&[s(&[f64::NAN]), s(&[f64::INFINITY])], true));
    }

    #[test]
    fn the_body_curve_has_at_most_two_points_per_pixel() {
        let samples: Vec<f64> = (0..10_000).map(|i| (i as f64 * 0.37).sin()).collect();
        let n = samples.len();
        let pts = envelope(&samples, 100.0, |k| k as f32 / (n - 1) as f32 * 100.0, |v| v as f32);
        assert!(pts.len() <= 202, "{}", pts.len());
        // The swing survives.
        assert!(pts.iter().any(|p| p.y > 0.99) && pts.iter().any(|p| p.y < -0.99));
        // Few samples keep every one, at its own place; non-finite ones are skipped.
        let pts = envelope(&[0.0, f64::NAN, 2.0], 100.0, |k| k as f32 * 50.0, |v| v as f32);
        assert_eq!(pts, [pos2(0.0, 0.0), pos2(100.0, 2.0)]);
        assert!(envelope(&[], 100.0, |k| k as f32, |v| v as f32).is_empty());
    }

    #[test]
    fn flat_ranges_are_spread() {
        assert_eq!(padded(120.0, 120.0), (108.0, 132.0));
        assert_eq!(padded(0.0, 0.0), (-1.0, 1.0));
        let (lo, hi) = padded(0.0, 1.0);
        assert!(lo < 0.0 && hi > 1.0);
        assert_eq!(XAxis::Seconds { span: f64::NAN }.range(), (-1.0, 0.0));
        assert_eq!(XAxis::Index { last: 0.0 }.range(), (0.0, 1.0));
    }
}
