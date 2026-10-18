//! The viewer's view of a `chan.audio_analysis`, made to be read at a glance during a show:
//! level and band meters with a peak hold, a beat light with the beat's phase as a ring, the
//! bar as one cell per beat, the tempo in large type (dimmed while unsure), a strip of recent
//! kicks, onsets and beats, and the spectrum.
//!
//! The analysis only reports the frame an event happens in; the short memory a meter needs
//! (peaks, recent hits, when the last beat came) is kept here, per viewed node, not in the op.

use std::collections::VecDeque;

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use penumbra_core::{Chans, NodeId};
use penumbra_graph::Data;

use crate::theme;

/// Seconds a peak stays before it falls.
const PEAK_HOLD: f64 = 1.0;
/// How fast a held peak falls, in full scale per second.
const PEAK_FALL: f64 = 0.8;
/// Seconds of hits the strip shows.
const STRIP_SECONDS: f64 = 4.0;
/// Most hits kept (a busy onset channel at 60 fps for four seconds is 240).
const MAX_HITS: usize = 512;
/// Seconds a flash takes to fade.
const FLASH: f64 = 0.25;
/// Two beats closer than this are one (a tick and the phase wrapping in the next frame).
const BEAT_DEBOUNCE: f64 = 0.12;
/// Most bar cells drawn; a longer bar shows its position as a number.
const MAX_CELLS: usize = 16;
/// Bars of the spectrum drawn at most; more bands are combined (the highest of each group).
const MAX_SPECTRUM_BARS: usize = 128;

/// The meters, in the analysis' channel order: channel, label, label where little room.
const METERS: [(&str, &str, &str); 4] =
    [("level", "level", "lvl"), ("low", "low", "lo"), ("mid", "mid", "mid"), ("high", "high", "hi")];
/// The smallest type drawn, in points.
const MIN_TEXT: f32 = 9.0;
/// Below this height (points) the view leaves out the spectrum and the strip's labels.
const COMPACT: f32 = 150.0;

/// Proportional type of `size`, never smaller than [`MIN_TEXT`].
fn text(size: f32) -> FontId {
    FontId::proportional(size.max(MIN_TEXT))
}

/// The kinds of hit the strip shows, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Kick,
    Onset,
    Beat,
}

const HITS: [Hit; 3] = [Hit::Kick, Hit::Onset, Hit::Beat];

impl Hit {
    fn label(self) -> &'static str {
        match self {
            Hit::Kick => "kick",
            Hit::Onset => "onset",
            Hit::Beat => "beat",
        }
    }

    fn color(self) -> Color32 {
        match self {
            // As the low band: a kick is a hit in the low band.
            Hit::Kick => theme::channel_color(1),
            Hit::Onset => theme::channel_color(2),
            Hit::Beat => theme::TEXT,
        }
    }
}

/// What the view needs of the analysis node besides its outputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioNode {
    pub id: NodeId,
    /// The bar's length in beats (`beats_per_bar`).
    pub beats_per_bar: u32,
    /// Where the low band ends and the high one starts, in Hz, for the spectrum's colours.
    pub low_hz: f64,
    pub high_hz: f64,
}

impl AudioNode {
    /// The analysis node `id` of `net`, if it is one.
    pub fn of(net: &penumbra_graph::Network, id: NodeId) -> Option<AudioNode> {
        let node = net.node(id)?;
        if node.kind != penumbra_ops::audio::AUDIO_ANALYSIS {
            return None;
        }
        let float = |name: &str, default: f64| {
            node.params
                .get(name)
                .and_then(penumbra_core::Value::as_f64)
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let beats = node.params.get("beats_per_bar").and_then(penumbra_core::Value::as_i64).unwrap_or(4);
        Some(AudioNode {
            id,
            beats_per_bar: u32::try_from(beats.clamp(1, 64)).unwrap_or(4),
            low_hz: float("low", 150.0),
            high_hz: float("high", 2500.0),
        })
    }
}

/// The view's memory of one node: held peaks, recent hits, the last beat.
#[derive(Default)]
pub struct AudioMeters {
    node: Option<NodeId>,
    /// The `out` output seen last, to tell a new cook from a repaint of the same one; held, so
    /// a new output cannot be allocated where it was.
    last: Option<Data>,
    /// Per meter: the held value and when it was reached.
    peaks: [(f64, f64); 4],
    /// (time, kind, whether a bar's first beat), oldest first.
    hits: VecDeque<(f64, Hit, bool)>,
    last_beat: Option<f64>,
    /// Whether the last beat was a bar's first.
    last_beat_down: bool,
    last_onset: Option<f64>,
    last_kick: Option<f64>,
    beat_phase: Option<f64>,
}

/// The values read from one `out` output; channels that are missing read 0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Reading {
    meters: [f64; 4],
    onset: bool,
    kick: bool,
    tick: bool,
    beat: f64,
    bar: f64,
    bar_beat: u32,
    bpm: f64,
    confidence: f64,
}

impl Reading {
    fn of(chans: &Chans) -> Reading {
        let v = |name: &str| chans.value(Some(name)).filter(|v| v.is_finite()).unwrap_or(0.0);
        Reading {
            meters: METERS.map(|(name, _, _)| v(name).clamp(0.0, 1.0)),
            onset: v("onset") > 0.5,
            kick: v("kick") > 0.5,
            tick: v("tick") > 0.5,
            beat: v("beat").clamp(0.0, 1.0),
            bar: v("bar").clamp(0.0, 1.0),
            // `as`: a small count, clamped first.
            bar_beat: v("bar_beat").round().clamp(0.0, 1000.0) as u32,
            bpm: v("bpm").max(0.0),
            confidence: v("confidence").clamp(0.0, 1.0),
        }
    }
}

impl AudioMeters {
    /// Takes in the newest outputs at `now` (seconds, any steady clock): a new cook adds its
    /// hits, every call lets the peaks fall.
    fn update(&mut self, id: NodeId, out: Option<&Data>, now: f64) -> Reading {
        if self.node != Some(id) {
            *self = AudioMeters { node: Some(id), ..AudioMeters::default() };
        }
        let chans = out.and_then(|d| d.get::<Chans>());
        let reading = chans.map(Reading::of).unwrap_or_default();
        let fresh = match (out, &self.last) {
            (Some(o), Some(last)) => !o.same(last),
            (Some(_), None) => true,
            (None, _) => false,
        };
        if fresh {
            self.last = out.cloned();
            if reading.kick {
                self.hit(now, Hit::Kick, false);
                self.last_kick = Some(now);
            }
            if reading.onset {
                self.hit(now, Hit::Onset, false);
                self.last_onset = Some(now);
            }
            // A tick missed between two repaints still shows: the phase has started over.
            let wrapped = self.beat_phase.is_some_and(|p| reading.beat + 0.3 < p);
            let recent = self.last_beat.is_some_and(|t| now - t < BEAT_DEBOUNCE && now >= t);
            if (reading.tick || wrapped) && !recent && reading.bpm > 0.0 {
                self.last_beat = Some(now);
                self.last_beat_down = reading.bar_beat == 0;
                self.hit(now, Hit::Beat, self.last_beat_down);
            }
            self.beat_phase = Some(reading.beat);
        }
        for (peak, v) in self.peaks.iter_mut().zip(reading.meters) {
            let held = falling(*peak, now);
            if v >= held {
                *peak = (v, now);
            }
        }
        while self.hits.front().is_some_and(|(t, _, _)| now - t > STRIP_SECONDS || *t > now) {
            self.hits.pop_front();
        }
        reading
    }

    fn hit(&mut self, now: f64, kind: Hit, down: bool) {
        self.hits.push_back((now, kind, down));
        while self.hits.len() > MAX_HITS {
            self.hits.pop_front();
        }
    }

    /// Draws the view of `node` into `rect`: `out` and `spectrum` are its two outputs (either
    /// may be missing or not cooked yet), `now` in seconds.
    pub fn draw(
        &mut self,
        painter: &Painter,
        rect: Rect,
        node: &AudioNode,
        out: Option<&Data>,
        spectrum: Option<&Data>,
        now: f64,
    ) {
        let r = self.update(node.id, out, now);
        let compact = rect.height() < COMPACT;
        let area = rect.shrink(if compact { 5.0 } else { 8.0 });
        if area.width() < 40.0 || area.height() < 40.0 {
            return;
        }
        // Everything scales with the room: 1 at about 250 x 135.
        let s = (area.width() / 250.0).min(area.height() / 135.0).clamp(0.7, 3.0);
        let gap = 8.0 * s;
        // The spectrum is the first thing to go where room is short.
        let spectrum =
            spectrum.filter(|_| !compact).and_then(|d| d.get::<Chans>()).and_then(|c| c.channels().first());
        let spectrum_h = if spectrum.is_some() { (area.height() * 0.24).max(18.0) } else { 0.0 };
        let top =
            Rect::from_min_max(area.min, pos2(area.max.x, area.max.y - spectrum_h - gap.min(spectrum_h)));
        let meters_w = (top.width() * 0.34).min(top.height() * 1.6);
        let meters = Rect::from_min_max(top.min, pos2(top.min.x + meters_w, top.max.y));
        let right = Rect::from_min_max(pos2(meters.max.x + gap * 1.5, top.min.y), top.max);
        self.meters(painter, meters, &r, now, s);
        let tempo_h = right.height() * 0.45;
        let cells_h = right.height() * 0.24;
        let tempo = Rect::from_min_size(right.min, vec2(right.width(), tempo_h));
        let cells =
            Rect::from_min_size(pos2(right.min.x, tempo.max.y + gap * 0.5), vec2(right.width(), cells_h));
        let strip = Rect::from_min_max(pos2(right.min.x, cells.max.y + gap * 0.75), right.max);
        self.tempo(painter, tempo, &r, now, s);
        self.cells(painter, cells, &r, node.beats_per_bar, s);
        self.strip(painter, strip, now, s, !compact);
        if let Some(spectrum) = spectrum {
            let at = Rect::from_min_max(pos2(area.min.x, area.max.y - spectrum_h), area.max);
            spectrum_bars(painter, at, &spectrum.samples, node, s);
        }
        if out.and_then(|d| d.get::<Chans>()).is_none_or(Chans::is_empty) {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "no analysis yet",
                FontId::proportional(14.0),
                theme::TEXT_WEAK,
            );
        }
    }

    /// Four vertical bars with a peak line each, labelled below.
    fn meters(&self, painter: &Painter, rect: Rect, r: &Reading, now: f64, s: f32) {
        let font = text(9.0 * s);
        let label_h = font.size + 3.0 * s;
        let n = METERS.len() as f32;
        let gap = 4.0 * s;
        let w = ((rect.width() - gap * (n - 1.0)) / n).max(2.0);
        let faint = Color32::from_white_alpha(14);
        for (i, ((_, label, short), (v, peak))) in
            METERS.iter().zip(r.meters.iter().zip(self.peaks)).enumerate()
        {
            let x = rect.min.x + i as f32 * (w + gap);
            let well = Rect::from_min_max(pos2(x, rect.min.y), pos2(x + w, rect.max.y - label_h));
            painter.rect_filled(well, CornerRadius::same(2), faint);
            let color = theme::channel_color(i);
            let to_y = |v: f64| well.max.y - (v.clamp(0.0, 1.0) as f32) * well.height();
            let fill = Rect::from_min_max(pos2(well.min.x, to_y(*v)), well.max);
            painter.rect_filled(fill, CornerRadius::same(2), color);
            let held = falling(peak, now);
            if held > 0.005 {
                let y = to_y(held);
                painter.line_segment(
                    [pos2(well.min.x, y), pos2(well.max.x, y)],
                    Stroke::new(2.0 * s.min(1.5), theme::TEXT),
                );
            }
            painter.text(
                pos2(well.center().x, rect.max.y),
                Align2::CENTER_BOTTOM,
                if w < 26.0 { *short } else { *label },
                font.clone(),
                theme::TEXT_WEAK,
            );
        }
    }

    /// The tempo in large type with its confidence as a bar, and the beat light: a disc that
    /// flashes on each beat (amber on a bar's first) inside a ring filling with the beat's phase.
    fn tempo(&self, painter: &Painter, rect: Rect, r: &Reading, now: f64, s: f32) {
        let d = rect.height().min(rect.width() * 0.4);
        let center = pos2(rect.max.x - d * 0.5, rect.center().y);
        let radius = d * 0.5 - 2.0 * s;
        let flash = flash(self.last_beat, now);
        let beat_color = if self.last_beat_down { theme::WARNING } else { theme::ACCENT };
        painter.circle_filled(center, radius, Color32::from_white_alpha(10));
        if flash > 0.0 {
            painter.circle_filled(center, radius * 0.78, beat_color.gamma_multiply(flash));
        }
        let ring = Stroke::new((3.0 * s).max(2.0), beat_color.gamma_multiply(0.85));
        if r.bpm > 0.0 {
            arc(painter, center, radius, r.beat, ring);
        }
        // The number, as large as fits left of the light.
        let text_w = (center.x - radius - rect.min.x - 6.0 * s).max(0.0);
        let unsure = 0.3 + 0.7 * r.confidence as f32;
        let (number, color) = if r.bpm > 0.0 {
            (format!("{:.0}", r.bpm), theme::TEXT.gamma_multiply(unsure))
        } else {
            ("--".to_string(), theme::TEXT_WEAK)
        };
        let conf_h = 4.0 * s;
        let caption = text(10.0 * s);
        let caption_h = caption.size + 2.0 * s;
        let size = ((rect.height() - conf_h - caption_h - 4.0 * s) * 0.95).clamp(8.0, 96.0 * s);
        let mut font = FontId::proportional(size);
        let mut galley = painter.layout_no_wrap(number.clone(), font.clone(), color);
        if galley.size().x > text_w && galley.size().x > 0.0 {
            font.size = (size * text_w / galley.size().x).max(6.0);
            galley = painter.layout_no_wrap(number, font, color);
        }
        let number_at = pos2(rect.min.x, rect.min.y);
        let number_h = galley.size().y;
        painter.galley(number_at, galley, color);
        painter.text(
            pos2(rect.min.x, number_at.y + number_h),
            Align2::LEFT_TOP,
            if r.bpm > 0.0 {
                format!("bpm, {:.0} % sure", r.confidence * 100.0)
            } else {
                "no tempo yet".to_string()
            },
            caption,
            theme::TEXT_WEAK,
        );
        let bar = Rect::from_min_size(
            pos2(rect.min.x, (number_at.y + number_h + caption_h + 2.0 * s).min(rect.max.y - conf_h)),
            vec2(text_w, conf_h),
        );
        painter.rect_filled(bar, CornerRadius::same(1), Color32::from_white_alpha(14));
        let sure = Rect::from_min_size(bar.min, vec2(bar.width() * r.confidence as f32, bar.height()));
        painter.rect_filled(sure, CornerRadius::same(1), confidence_color(r.confidence));
    }

    /// One cell per beat of the bar, the current one lit and filling with the beat's phase, the
    /// first marked; the bar's phase as a line below.
    fn cells(&self, painter: &Painter, rect: Rect, r: &Reading, beats_per_bar: u32, s: f32) {
        let line_h = 3.0 * s;
        let cells = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - line_h - 3.0 * s));
        let count = beats_per_bar.max(1) as usize;
        let current = r.bar_beat as usize;
        let known = r.bpm > 0.0;
        if count > MAX_CELLS {
            painter.text(
                cells.left_center(),
                Align2::LEFT_CENTER,
                format!("beat {} of {count}", current + 1),
                text(cells.height().min(18.0 * s)),
                theme::TEXT,
            );
        } else {
            let gap = 4.0 * s;
            let w = ((cells.width() - gap * (count as f32 - 1.0)) / count as f32).max(1.0);
            let font = text((cells.height() * 0.5).min(16.0 * s));
            for k in 0..count {
                let x = cells.min.x + k as f32 * (w + gap);
                let cell = Rect::from_min_max(pos2(x, cells.min.y), pos2(x + w, cells.max.y));
                let first = k == 0;
                let color = if first { theme::WARNING } else { theme::ACCENT };
                let lit = known && k == current;
                painter.rect_filled(
                    cell,
                    CornerRadius::same(3),
                    Color32::from_white_alpha(if lit { 0 } else { 12 }),
                );
                if lit {
                    painter.rect_filled(cell, CornerRadius::same(3), color.gamma_multiply(0.35));
                    let done = Rect::from_min_size(cell.min, vec2(w * r.beat as f32, cell.height()));
                    painter.rect_filled(done, CornerRadius::same(3), color.gamma_multiply(0.8));
                }
                if first {
                    painter.rect_stroke(
                        cell,
                        CornerRadius::same(3),
                        Stroke::new(1.5 * s.min(1.5), theme::WARNING),
                        StrokeKind::Inside,
                    );
                }
                if w > 14.0 {
                    painter.text(
                        cell.center(),
                        Align2::CENTER_CENTER,
                        format!("{}", k + 1),
                        font.clone(),
                        if lit { theme::TEXT } else { theme::TEXT_WEAK },
                    );
                }
            }
        }
        let line = Rect::from_min_size(pos2(rect.min.x, rect.max.y - line_h), vec2(rect.width(), line_h));
        painter.rect_filled(line, CornerRadius::ZERO, Color32::from_white_alpha(14));
        if known {
            let done = Rect::from_min_size(line.min, vec2(line.width() * r.bar as f32, line_h));
            painter.rect_filled(done, CornerRadius::ZERO, theme::WARNING.gamma_multiply(0.8));
        }
    }

    /// The last seconds of kicks, onsets and beats as marks moving left, a row each, with a
    /// light at the start of each row that flashes on its hit (and the row's name, if
    /// `labels`).
    fn strip(&self, painter: &Painter, rect: Rect, now: f64, s: f32, labels: bool) {
        if rect.height() < 6.0 {
            return;
        }
        let rows = HITS.len() as f32;
        let row_h = rect.height() / rows;
        let font = text((row_h * 0.75).min(11.0 * s));
        let dot = (row_h * 0.3).clamp(2.0, 4.0 * s);
        let labels = labels && row_h >= MIN_TEXT;
        let label_w = if labels { 40.0 * s.max(1.0) } else { dot * 2.0 + 4.0 };
        let track = Rect::from_min_max(pos2(rect.min.x + label_w, rect.min.y), rect.max);
        painter.rect_filled(track, CornerRadius::same(2), Color32::from_white_alpha(8));
        for (i, kind) in HITS.iter().enumerate() {
            let y0 = rect.min.y + i as f32 * row_h;
            let mid = y0 + row_h * 0.5;
            let last = match kind {
                Hit::Kick => self.last_kick,
                Hit::Onset => self.last_onset,
                Hit::Beat => self.last_beat,
            };
            let lit = flash(last, now);
            painter.circle_filled(
                pos2(rect.min.x + dot, mid),
                dot,
                if lit > 0.0 {
                    kind.color().gamma_multiply(0.3 + 0.7 * lit)
                } else {
                    kind.color().gamma_multiply(0.25)
                },
            );
            if labels {
                painter.text(
                    pos2(rect.min.x + dot * 2.0 + 4.0 * s, mid),
                    Align2::LEFT_CENTER,
                    kind.label(),
                    font.clone(),
                    theme::TEXT_WEAK,
                );
            }
            let mark_h = (row_h * 0.7).max(2.0);
            for (t, _, down) in self.hits.iter().filter(|(_, k, _)| k == kind) {
                let age = (now - t).max(0.0) / STRIP_SECONDS;
                let x = track.max.x - age as f32 * track.width();
                let a = (1.0 - age as f32 * 0.6).clamp(0.2, 1.0);
                painter.line_segment(
                    [pos2(x, mid - mark_h * 0.5), pos2(x, mid + mark_h * 0.5)],
                    Stroke::new(
                        (2.0 * s).max(1.5),
                        if *down { theme::WARNING } else { kind.color() }.gamma_multiply(a),
                    ),
                );
            }
        }
    }
}

/// A held peak's value at `now`: as reached for [`PEAK_HOLD`], then falling.
fn falling((value, at): (f64, f64), now: f64) -> f64 {
    let after = now - at - PEAK_HOLD;
    if after <= 0.0 { value } else { (value - after * PEAK_FALL).max(0.0) }
}

/// How bright a flash for something that happened at `at` is at `now`: 1 then, 0 after [`FLASH`].
fn flash(at: Option<f64>, now: f64) -> f32 {
    match at {
        Some(t) if now >= t && now - t < FLASH => (1.0 - (now - t) / FLASH) as f32,
        _ => 0.0,
    }
}

/// Red when unsure, through amber to green when sure.
fn confidence_color(c: f64) -> Color32 {
    if c < 0.35 {
        theme::ERROR.gamma_multiply(0.8)
    } else if c < 0.65 {
        theme::WARNING.gamma_multiply(0.8)
    } else {
        theme::channel_color(0)
    }
}

/// A ring from the top, clockwise, for `fraction` of a turn.
fn arc(painter: &Painter, center: Pos2, radius: f32, fraction: f64, stroke: Stroke) {
    let fraction = fraction.clamp(0.0, 1.0) as f32;
    let steps = ((fraction * 48.0).ceil() as usize).max(1);
    let points: Vec<Pos2> = (0..=steps)
        .map(|k| {
            let a = std::f32::consts::TAU * fraction * k as f32 / steps as f32 - std::f32::consts::FRAC_PI_2;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    if points.len() >= 2 {
        painter.add(egui::Shape::line(points, stroke));
    }
}

/// The spectrum's bands as bars, low to high, coloured as the low, mid and high meters.
fn spectrum_bars(painter: &Painter, rect: Rect, samples: &[f64], node: &AudioNode, s: f32) {
    painter.rect_filled(rect, CornerRadius::same(2), Color32::from_white_alpha(8));
    let n = samples.len();
    if n == 0 || rect.width() < 4.0 {
        return;
    }
    let bars = n.min(MAX_SPECTRUM_BARS).min((rect.width() / 2.0).max(1.0) as usize).max(1);
    let gap = if rect.width() / bars as f32 > 4.0 * s { 1.0 * s } else { 0.0 };
    let w = rect.width() / bars as f32;
    let (lo_f, hi_f) = (penumbra_audio::analysis::SPECTRUM_LOW, penumbra_audio::analysis::SPECTRUM_HIGH);
    for b in 0..bars {
        let from = b * n / bars;
        let to = ((b + 1) * n / bars).max(from + 1).min(n);
        let v = samples
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0f64, f64::max)
            .clamp(0.0, 1.0);
        // The band's middle frequency on the spectrum's log scale.
        let mid = (from + to) as f64 * 0.5 / n as f64;
        let hz = lo_f * (hi_f / lo_f).powf(mid);
        let color = if hz < node.low_hz {
            theme::channel_color(1)
        } else if hz > node.high_hz {
            theme::channel_color(3)
        } else {
            theme::channel_color(2)
        };
        let x = rect.min.x + b as f32 * w;
        let h = v as f32 * rect.height();
        let bar = Rect::from_min_max(pos2(x, rect.max.y - h), pos2(x + w - gap, rect.max.y));
        painter.rect_filled(bar, CornerRadius::ZERO, color.gamma_multiply(0.85));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::{Channel, Name};
    use penumbra_graph::DataKind;

    fn out(values: &[(&str, f64)]) -> Data {
        let channels =
            values.iter().map(|(n, v)| Channel { name: Name::new(n).unwrap(), samples: vec![*v] }).collect();
        Data::new(DataKind::Chan, Chans::new(60.0, channels).unwrap())
    }

    #[test]
    fn hits_count_once_per_cook_and_peaks_hold_then_fall() {
        let mut m = AudioMeters::default();
        let id = NodeId(1);
        let a = out(&[("level", 0.9), ("kick", 1.0), ("tick", 1.0), ("bpm", 120.0), ("beat", 0.0)]);
        m.update(id, Some(&a), 10.0);
        // A repaint of the same cook adds nothing.
        m.update(id, Some(&a), 10.01);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Kick).count(), 1);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Beat).count(), 1);
        let quiet = out(&[("level", 0.1), ("bpm", 120.0), ("beat", 0.5)]);
        m.update(id, Some(&quiet), 10.5);
        assert_eq!(falling(m.peaks[0], 10.5), 0.9, "held");
        assert!(falling(m.peaks[0], 11.5) < 0.9, "falling after the hold");
        // A missed tick shows through the phase starting over.
        let next = out(&[("level", 0.1), ("bpm", 120.0), ("beat", 0.02)]);
        m.update(id, Some(&next), 10.6);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Beat).count(), 2);
        // Old hits leave the strip; another node starts over.
        m.update(id, Some(&next), 20.0);
        assert!(m.hits.is_empty());
        m.update(NodeId(2), Some(&a), 20.0);
        assert_eq!(m.node, Some(NodeId(2)));
    }

    #[test]
    fn missing_channels_read_zero() {
        let mut m = AudioMeters::default();
        let r = m.update(NodeId(1), None, 0.0);
        assert_eq!(r, Reading::default());
        let odd = out(&[("level", f64::NAN), ("bar_beat", -3.0)]);
        let r = m.update(NodeId(1), Some(&odd), 0.1);
        assert_eq!(r.meters[0], 0.0);
        assert_eq!(r.bar_beat, 0);
    }
}
