//! The viewer's picture of sound (an `audio` output): the waveform of the last blocks, one lane
//! per channel on the full scale -1 to 1, and a peak meter per channel in dBFS with a held peak.
//!
//! The blocks are kept here, not in the node: a short history of each channel, fed with every
//! new block the viewer sees. The waveform ends at the last rising zero crossing of the first
//! channel, so a steady tone stands still instead of scrolling. Gaps or jumps between blocks
//! show as breaks in the curve.

use std::collections::VecDeque;

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Rect, Stroke, pos2, vec2};
use penumbra_core::{Chans, Name, NodeId};
use penumbra_graph::{Data, DataKind};

use crate::{plot, theme};

/// Seconds of each channel kept.
const KEEP_SECONDS: f64 = 0.2;
/// Seconds of the waveform drawn.
const WINDOW_SECONDS: f64 = 0.05;
/// How far back the zero crossing that ends the waveform is looked for.
const TRIGGER_SECONDS: f64 = 0.03;
/// How long a peak is held on the meter.
const HOLD_SECONDS: f64 = 1.5;
/// The bottom of the meter.
const FLOOR_DB: f64 = -60.0;
/// Channels drawn; more are counted.
const MAX_LANES: usize = 8;
const METER_WIDTH: f32 = 64.0;

/// The peak of one channel: of the newest block, and the highest of the last
/// [`HOLD_SECONDS`] with when it was reached.
#[derive(Clone, Copy, Debug, Default)]
struct Peak {
    now: f64,
    held: f64,
    at: f64,
}

#[derive(Default)]
pub struct Scope {
    of: Option<(NodeId, u32)>,
    last: Option<Data>,
    rate: f64,
    names: Vec<Name>,
    history: Vec<VecDeque<f64>>,
    peaks: Vec<Peak>,
    block: usize,
}

/// A level as dBFS, [`FLOOR_DB`] for silence.
fn db(x: f64) -> f64 {
    if x > 0.0 { (20.0 * x.log10()).max(FLOOR_DB) } else { FLOOR_DB }
}

impl Scope {
    /// Takes in output `output` of node `id` as the viewer sees it now (`now` in seconds of
    /// the UI clock); a block seen before is not taken twice.
    pub fn feed(&mut self, id: NodeId, output: u32, data: &Data, now: f64) {
        if self.of != Some((id, output)) {
            *self = Scope { of: Some((id, output)), ..Scope::default() };
        }
        if self.last.as_ref().is_some_and(|l| l.same(data)) {
            return;
        }
        let Some(chans) = data.get::<Chans>() else { return };
        self.last = Some(data.clone());
        let names: Vec<Name> = chans.names().take(MAX_LANES).cloned().collect();
        if chans.rate() != self.rate || names != self.names {
            self.rate = chans.rate();
            self.names = names;
            self.history = vec![VecDeque::new(); self.names.len()];
            self.peaks = vec![Peak::default(); self.names.len()];
        }
        self.block = chans.len();
        let keep = (KEEP_SECONDS * self.rate).ceil() as usize;
        for ((c, h), p) in chans.channels().iter().zip(&mut self.history).zip(&mut self.peaks) {
            h.extend(c.samples.iter().copied());
            while h.len() > keep {
                h.pop_front();
            }
            let peak = c.samples.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
            p.now = peak;
            if peak >= p.held || now - p.at > HOLD_SECONDS {
                p.held = peak;
                p.at = now;
            }
        }
    }

    pub fn draw(&self, painter: &Painter, rect: Rect) {
        let message = |text: &str| {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
        };
        let Some(chans) = self.last.as_ref().and_then(|d| d.get::<Chans>()) else {
            return message("no sound yet");
        };
        if self.names.is_empty() {
            return message("no sound yet: no channels");
        }
        let colour = theme::kind_color(DataKind::Audio);
        let faint = Stroke::new(1.0, Color32::from_white_alpha(28));
        let line_h = 18.0;
        let more = chans.channels().len().saturating_sub(MAX_LANES);
        let mut head = format!("{} Hz, {} samples in the last block", self.rate, self.block);
        if more > 0 {
            head.push_str(&format!(", {more} more channels not drawn"));
        }
        painter.text(
            pos2(rect.min.x + 8.0, rect.min.y + 6.0),
            Align2::LEFT_TOP,
            head,
            FontId::proportional(13.0),
            theme::TEXT_WEAK,
        );
        let area = Rect::from_min_max(
            pos2(rect.min.x + 8.0, rect.min.y + 6.0 + line_h + 4.0),
            rect.max - vec2(8.0, 8.0),
        );
        if area.width() < METER_WIDTH + 40.0 || area.height() < 20.0 {
            return;
        }
        let wave_w = area.width() - METER_WIDTH - 8.0;
        let lane_h = area.height() / self.names.len() as f32;
        let window = ((WINDOW_SECONDS * self.rate).round() as usize).max(2);
        let end = self.trigger(window);
        for (i, (name, h)) in self.names.iter().zip(&self.history).enumerate() {
            let top = area.min.y + lane_h * i as f32;
            let lane = Rect::from_min_size(pos2(area.min.x, top), vec2(wave_w, lane_h - 4.0));
            let to_y = |v: f64| lane.center().y - (v.clamp(-1.05, 1.05) as f32) * lane.height() / 2.0;
            painter.line_segment([pos2(lane.min.x, to_y(0.0)), pos2(lane.max.x, to_y(0.0))], faint);
            for v in [-1.0, 1.0] {
                painter.line_segment([pos2(lane.min.x, to_y(v)), pos2(lane.max.x, to_y(v))], faint);
            }
            let end = end.min(h.len());
            let start = end.saturating_sub(window);
            let samples: Vec<f64> = h.range(start..end).copied().collect();
            let offset = window - samples.len();
            let span = (window - 1) as f32;
            let to_x = |k: usize| lane.min.x + (offset + k) as f32 / span * lane.width();
            let points = plot::envelope(&samples, lane.width(), to_x, to_y);
            if points.len() >= 2 {
                painter.add(egui::Shape::line(points, Stroke::new(1.3, colour)));
            }
            painter.text(
                lane.left_top() + vec2(2.0, 0.0),
                Align2::LEFT_TOP,
                name.as_str(),
                FontId::proportional(12.0),
                theme::TEXT,
            );
            if let Some(p) = self.peaks.get(i) {
                self.meter(
                    painter,
                    Rect::from_min_size(pos2(lane.max.x + 8.0, top), vec2(METER_WIDTH, lane_h - 4.0)),
                    *p,
                );
            }
        }
    }

    /// One channel's peak meter: a bar from [`FLOOR_DB`] to 0 dBFS, the held peak as a line and
    /// in numbers; red from full scale on.
    fn meter(&self, painter: &Painter, rect: Rect, p: Peak) {
        let bar = Rect::from_min_max(rect.min, pos2(rect.min.x + 14.0, rect.max.y));
        painter.rect_filled(bar, CornerRadius::same(2), Color32::from_white_alpha(16));
        let share = |x: f64| ((db(x) - FLOOR_DB) / -FLOOR_DB) as f32;
        let clip = p.held >= 1.0;
        let fill = if p.now >= 1.0 { theme::ERROR } else { theme::kind_color(DataKind::Audio) };
        let y = bar.max.y - share(p.now) * bar.height();
        painter.rect_filled(Rect::from_min_max(pos2(bar.min.x, y), bar.max), CornerRadius::same(2), fill);
        let hy = bar.max.y - share(p.held) * bar.height();
        painter.line_segment(
            [pos2(bar.min.x, hy), pos2(bar.max.x, hy)],
            Stroke::new(2.0, if clip { theme::ERROR } else { theme::TEXT }),
        );
        let text = if p.held > 0.0 { format!("{:.1} dB", db(p.held)) } else { "silent".to_string() };
        painter.text(
            pos2(bar.max.x + 4.0, rect.min.y),
            Align2::LEFT_TOP,
            text,
            FontId::proportional(11.0),
            if clip { theme::ERROR } else { theme::TEXT_WEAK },
        );
    }

    /// Where the waveform of `window` samples ends: just after the last rising zero crossing of
    /// the first channel within [`TRIGGER_SECONDS`], else at the newest sample.
    fn trigger(&self, window: usize) -> usize {
        let Some(h) = self.history.first() else { return 0 };
        let len = h.len();
        let back = (TRIGGER_SECONDS * self.rate) as usize;
        let lowest = window.max(len.saturating_sub(back)).max(1);
        (lowest..len)
            .rev()
            .find(|&i| h.get(i - 1).is_some_and(|a| *a < 0.0) && h.get(i).is_some_and(|b| *b >= 0.0))
            .unwrap_or(len)
    }
}
