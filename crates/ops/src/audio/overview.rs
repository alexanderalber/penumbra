//! The overview of a sound file that `chan.beat_grid` shows: the whole file in one lane, the
//! time around the playhead in a second one below it.
//!
//! Each lane draws the file's level as a waveform (from the envelopes, one column per pixel:
//! the loudest hop of the column faint, the mean bright), coloured by the share of low, mid and
//! high (red, green, blue). Above it runs a strip of sections, coloured by label, with a taller
//! white edge where a track starts and a gradient where one section blends into the next;
//! below it a time axis. Bar lines appear where bars are at least a few units apart, beats where beats
//! are. Where the grid is not final yet the lane is veiled, where the levels are not measured
//! yet it is hatched grey. A red line marks the playhead; in the upper lane a box marks what the
//! lower one shows.
//!
//! One description serves two painters, as [`crate::meters`] does: the viewer paints the
//! shapes with egui, the node's `overview` output draws them into a texture. Clicks are turned
//! back into seconds by [`Layout::seconds_at`], from the same layout.

use std::collections::HashMap;

use penumbra_audio::beats::{Envelopes, LEVEL_LAG_HOPS};

use crate::meters::{Anchor, Color, Measure, Palette, Rect, Shape, fade, white_alpha};

/// Seconds the lower lane shows when nothing sensible is asked for.
pub const DEFAULT_SPAN: f64 = 60.0;
/// The shortest and longest the lower lane shows.
pub const MIN_SPAN: f64 = 2.0;
pub const MAX_SPAN: f64 = 3600.0;
/// Most columns one lane computes.
pub const MAX_COLUMNS: usize = 8192;
/// Below this height (layout units) only the upper lane is drawn.
const ONE_LANE: f32 = 110.0;
/// Bars closer than this (layout units) are not drawn; beats likewise.
const MIN_BAR_GAP: f32 = 4.0;
const MIN_BEAT_GAP: f32 = 6.0;
/// Most bar or beat lines drawn in one lane.
const MAX_LINES: usize = 2048;
/// Time labels at least this far apart, layout units.
const LABEL_GAP: f32 = 64.0;
/// The type of the labels.
const TEXT: f32 = 10.0;
const BACKGROUND: Color = [22, 24, 28, 255];
const UNKNOWN: Color = [44, 46, 52, 255];
/// The section colours, by order of first appearance of a label.
const SECTION_COLORS: [Color; 10] = [
    [238, 136, 52, 255],
    [70, 180, 90, 255],
    [150, 70, 190, 255],
    [70, 110, 220, 255],
    [230, 200, 40, 255],
    [220, 70, 190, 255],
    [60, 200, 220, 255],
    [220, 70, 70, 255],
    [160, 210, 60, 255],
    [240, 150, 170, 255],
];

/// One section of the file: from `start` to `end` seconds, with a label that repeats where the
/// music repeats. Sections of the same label share a colour; `text` is written on the section
/// where it fits.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub text: String,
    /// The section begins a track: its start gets a taller, brighter edge.
    pub track_start: bool,
    /// Seconds over which the section before blends into this one, around `start`: drawn as a
    /// gradient from the colour before to this one's. Equal ends: a cut.
    pub zone: (f64, f64),
}

/// The colour a track start's edge is drawn in, and how far it reaches below the strip.
const TRACK_EDGE: Color = [245, 245, 245, 255];
const TRACK_EDGE_REACH: f32 = 6.0;
/// The widest step of a zone's gradient, layout units, and the most steps per zone.
const ZONE_STEP: f32 = 2.0;
const ZONE_STEPS: usize = 48;
/// The line under a zone, so a blend between two sections of one colour still shows.
const ZONE_LINE: Color = white_alpha(170);

/// `a` mixed with `b` by `f` (0 is `a`).
fn mix(a: Color, b: Color, f: f32) -> Color {
    let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
    let mut out = [0u8; 4];
    for ((o, x), y) in out.iter_mut().zip(a).zip(b) {
        // `as`: between two u8 values.
        *o = (f32::from(x) + (f32::from(y) - f32::from(x)) * f).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The shapes of a sections strip `strip` for `lane`: a block per section in its label's colour,
/// a gradient over each blend zone with a line under it, a dark edge at each boundary and a
/// taller white one where a track starts, and with `text` the sections' letters.
fn strip_shapes(
    out: &mut Vec<Shape>,
    lane: &Lane,
    sections: &[Section],
    colors: &HashMap<&str, Color>,
    text: bool,
    measure: &dyn Measure,
) {
    let strip = lane.sections;
    let ok = |s: &Section| s.start.is_finite() && s.end.is_finite() && s.end > s.start;
    let shown = |a: f64, b: f64| b >= lane.from && a <= lane.to;
    let color_of = |s: &Section| colors.get(s.label.as_str()).copied().unwrap_or(UNKNOWN);
    for s in sections.iter().filter(|s| ok(s) && shown(s.start, s.end)) {
        let x0 = lane.x_of(s.start.max(lane.from));
        let x1 = lane.x_of(s.end.min(lane.to));
        out.push(Shape::Rect {
            rect: Rect::from_min_max([x0, strip.min[1]], [x1, strip.max[1]]),
            radius: 0.0,
            color: color_of(s),
        });
    }
    // Blend zones, over the blocks: the colour before fades into the colour after.
    let pairs = sections.windows(2).filter_map(|w| Some((w.first()?, w.get(1)?)));
    for (prev, s) in pairs {
        let (za, zb) = s.zone;
        if !(ok(s) && ok(prev) && za.is_finite() && zb.is_finite() && zb > za && shown(za, zb)) {
            continue;
        }
        let (xa, xb) = (lane.x_of(za), lane.x_of(zb));
        let (from, to) = (color_of(prev), color_of(s));
        let (left, right) = (lane.x_of(za.max(lane.from)), lane.x_of(zb.min(lane.to)));
        let width = right - left;
        if !width.is_finite() || width <= 0.0 {
            continue;
        }
        // `as`: a step count, bounded by ZONE_STEPS.
        let steps = ((width / ZONE_STEP).ceil().max(1.0) as usize).min(ZONE_STEPS);
        let w = width / steps as f32;
        for k in 0..steps {
            let a = left + w * k as f32;
            // The colour at the step's middle, by its place in the whole zone.
            let f = (a + w / 2.0 - xa) / (xb - xa);
            out.push(Shape::Rect {
                rect: Rect::from_min_max([a, strip.min[1]], [a + w + 0.05, strip.max[1]]),
                radius: 0.0,
                color: mix(from, to, f),
            });
        }
        let y = strip.max[1] - 1.0;
        out.push(Shape::Line {
            from: [left, y],
            to: [right.max(left + 1.0), y],
            width: 2.0,
            color: ZONE_LINE,
        });
    }
    for s in sections.iter().filter(|s| ok(s) && s.start >= lane.from && s.start <= lane.to) {
        let x = lane.x_of(s.start);
        let shape = if s.track_start {
            Shape::Line {
                from: [x, strip.min[1]],
                to: [x, strip.max[1] + TRACK_EDGE_REACH],
                width: 2.0,
                color: TRACK_EDGE,
            }
        } else {
            Shape::Line { from: [x, strip.min[1]], to: [x, strip.max[1]], width: 1.0, color: BACKGROUND }
        };
        out.push(shape);
    }
    if !text {
        return;
    }
    for s in sections.iter().filter(|s| ok(s) && shown(s.start, s.end) && !s.text.is_empty()) {
        let x0 = lane.x_of(s.start.max(lane.from));
        let x1 = lane.x_of(s.end.min(lane.to));
        let [tw, _] = measure.text_size(&s.text, TEXT);
        if x1 - x0 > tw + 8.0 {
            out.push(Shape::Text {
                at: [x0 + 4.0, (strip.min[1] + strip.max[1]) * 0.5],
                anchor: Anchor::LEFT_CENTER,
                size: TEXT,
                text: s.text.clone(),
                color: [16, 16, 16, 255],
            });
        }
    }
}

/// What the overview shows. Every field may be empty, missing or nonsense; the picture then
/// leaves out what it cannot draw.
#[derive(Clone, Copy, Debug)]
pub struct Overview<'a> {
    /// The file's length in seconds; 0 while unknown (then the beats and levels known decide).
    pub duration: f64,
    /// Seconds into the file the playhead stands at.
    pub playhead: Option<f64>,
    /// Beat and bar starts, seconds, ascending.
    pub beats: &'a [f64],
    pub bars: &'a [f64],
    /// Seconds before which the grid is final; infinite once complete, minus infinity before
    /// any of it is. `None`: there is no grid at all.
    pub final_until: Option<f64>,
    pub envelopes: Option<&'a Envelopes>,
    pub sections: &'a [Section],
    /// Seconds the lower lane shows around the playhead.
    pub span: f64,
    /// A line on how the analysis stands (`finding beats 43 %`), drawn in the upper lane.
    pub status: Option<&'a str>,
}

impl Overview<'_> {
    /// The length the lanes cover: the file's, else as far as beats or levels reach.
    pub fn length(&self) -> f64 {
        if self.duration.is_finite() && self.duration > 0.0 {
            return self.duration;
        }
        let beats = self.beats.iter().rev().find(|t| t.is_finite()).copied().unwrap_or(0.0);
        let levels = self.envelopes.map_or(0.0, |e| e.len() as f64 * e.hop_seconds());
        let l = beats.max(levels);
        if l.is_finite() && l > 0.0 { l } else { 0.0 }
    }
}

/// The seconds the lower lane shows: `span` around the playhead, kept inside the file.
pub fn window(playhead: Option<f64>, span: f64, length: f64) -> (f64, f64) {
    let length = if length.is_finite() { length.max(0.0) } else { 0.0 };
    let span = if span.is_finite() { span.clamp(MIN_SPAN, MAX_SPAN) } else { DEFAULT_SPAN };
    if length <= span {
        return (0.0, length.max(MIN_SPAN));
    }
    let p = playhead.filter(|p| p.is_finite()).unwrap_or(0.0);
    let from = (p - span / 2.0).clamp(0.0, length - span);
    (from, from + span)
}

/// One lane: its room and the seconds it spans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lane {
    pub rect: Rect,
    pub from: f64,
    pub to: f64,
    /// The strip of sections at its top, the waveform, the time axis at its bottom.
    pub sections: Rect,
    pub wave: Rect,
    pub axis: Rect,
}

impl Lane {
    fn new(rect: Rect, from: f64, to: f64, strip: f32) -> Lane {
        let axis_h = 12.0f32.min(rect.height() * 0.25);
        let strip = strip.min(rect.height() * 0.2);
        let sections = Rect::from_min_max(rect.min, [rect.max[0], rect.min[1] + strip]);
        let axis = Rect::from_min_max([rect.min[0], rect.max[1] - axis_h], rect.max);
        let wave = Rect::from_min_max([rect.min[0], sections.max[1] + 1.0], [rect.max[0], axis.min[1]]);
        Lane { rect, from, to, sections, wave, axis }
    }

    /// The x of `t` seconds.
    pub fn x_of(&self, t: f64) -> f32 {
        let span = self.to - self.from;
        if span.is_nan() || span <= 0.0 {
            return self.rect.min[0];
        }
        // `as`: layout units.
        self.rect.min[0] + (((t - self.from) / span) as f32) * self.rect.width()
    }

    /// The seconds at `x`, inside the lane's span.
    pub fn t_of(&self, x: f32) -> f64 {
        let w = self.rect.width();
        let f = if w > 0.0 { f64::from(((x - self.rect.min[0]) / w).clamp(0.0, 1.0)) } else { 0.0 };
        self.from + f * (self.to - self.from)
    }

    fn contains(&self, p: [f32; 2]) -> bool {
        (self.rect.min[0]..=self.rect.max[0]).contains(&p[0])
            && (self.rect.min[1]..=self.rect.max[1]).contains(&p[1])
    }
}

/// Where the lanes go in a room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The whole file; `None` when the room is too small or the length is not known.
    pub whole: Option<Lane>,
    /// The time around the playhead; `None` in a low room.
    pub zoom: Option<Lane>,
    /// The file's length the lanes were laid out for.
    pub length: f64,
}

impl Layout {
    pub fn new(room: Rect, o: &Overview<'_>) -> Layout {
        let length = o.length();
        let (w, h) = (room.width(), room.height());
        if !(w >= 40.0 && h >= 24.0 && length > 0.0) {
            return Layout { whole: None, zoom: None, length };
        }
        let pad = 4.0;
        let inner = Rect::from_min_max(
            [room.min[0] + pad, room.min[1] + pad],
            [room.max[0] - pad, room.max[1] - pad],
        );
        if h < ONE_LANE {
            return Layout { whole: Some(Lane::new(inner, 0.0, length, 6.0)), zoom: None, length };
        }
        let gap = 10.0;
        let upper = (inner.height() - gap) * 0.4;
        let top = Rect::from_min_max(inner.min, [inner.max[0], inner.min[1] + upper]);
        let bottom = Rect::from_min_max([inner.min[0], top.max[1] + gap], inner.max);
        let (a, b) = window(o.playhead, o.span, length);
        Layout {
            whole: Some(Lane::new(top, 0.0, length, 7.0)),
            zoom: Some(Lane::new(bottom, a, b, 16.0)),
            length,
        }
    }

    /// The seconds into the file a click at `p` asks for: in either lane, kept within the file.
    /// `None` outside the lanes.
    pub fn seconds_at(&self, p: [f32; 2]) -> Option<f64> {
        if !(p[0].is_finite() && p[1].is_finite()) {
            return None;
        }
        let lane = [self.whole, self.zoom].into_iter().flatten().find(|l| l.contains(p))?;
        let t = lane.t_of(p[0]);
        Some(t.clamp(0.0, self.length.max(0.0)))
    }
}

/// One column of a lane's waveform.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Column {
    /// The loudest hop's and the mean level as amplitude, 1 for the loudest hop of the file.
    pub peak: f32,
    pub mean: f32,
    /// The mean amplitude of low, mid and high, the largest 1.
    pub bands: [f32; 3],
    /// Some hop of the column is measured.
    pub known: bool,
}

/// The power of the loudest hop of `env` (amplitude squared, full scale 1); 0 for none.
pub fn loudest(env: &Envelopes) -> f64 {
    (0..env.len())
        .filter_map(|i| env.get(i))
        .map(|h| h.power()[0])
        .filter(|p| p.is_finite())
        .fold(0.0, f64::max)
}

/// 0 to 1: the amplitude of `power` against that of `loudest`.
fn height(power: f64, loudest: f64) -> f32 {
    if !(power.is_finite() && power > 0.0 && loudest > 0.0) {
        return 0.0;
    }
    // `as`: 0 to 1.
    (power / loudest).sqrt().clamp(0.0, 1.0) as f32
}

/// The waveform of `from` to `to` seconds in `n` columns (at most [`MAX_COLUMNS`]), its height
/// against the power of the `loudest` hop ([`loudest`]). A column narrower than a hop takes the
/// hop nearest its middle.
pub fn columns(env: &Envelopes, from: f64, to: f64, n: usize, loudest: f64) -> Vec<Column> {
    let n = n.min(MAX_COLUMNS);
    let mut out = vec![Column::default(); n];
    let h = env.hop_seconds();
    if !(h > 0.0 && from.is_finite() && to.is_finite() && to > from && n > 0) {
        return out;
    }
    let len = env.len();
    let index = |t: f64| t / h + LEVEL_LAG_HOPS - 1.0;
    let step = (to - from) / n as f64;
    for (c, col) in out.iter_mut().enumerate() {
        let t0 = from + step * c as f64;
        let (a, b) = (index(t0).ceil(), index(t0 + step).ceil());
        let range = if b > a {
            a..b
        } else {
            let mid = index(t0 + step / 2.0).round();
            mid..mid + 1.0
        };
        // `as`: hop counts, clamped to those known.
        let first = range.start.max(0.0).min(len as f64) as usize;
        let last = range.end.max(0.0).min(len as f64) as usize;
        if first >= last {
            continue;
        }
        let (mut peak, mut sum, mut bands) = (0.0f64, 0.0, [0.0f64; 3]);
        let mut count = 0usize;
        for i in first..last {
            let Some(hop) = env.get(i) else { continue };
            let [level, low, mid, high] = hop.power();
            peak = peak.max(level);
            sum += level;
            for (s, p) in bands.iter_mut().zip([low, mid, high]) {
                *s += p;
            }
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let k = count as f64;
        let amp = bands.map(|p| (p / k).max(0.0).sqrt());
        let total: f64 = amp.iter().sum();
        // `as`: 0 to 1.
        let bands = if total > 0.0 { amp.map(|a| (a / total) as f32) } else { [0.0; 3] };
        *col = Column { peak: height(peak, loudest), mean: height(sum / k, loudest), bands, known: true };
    }
    out
}

/// The colours low, mid and high mix from.
const BAND_COLORS: [[f32; 3]; 3] = [[235.0, 70.0, 50.0], [70.0, 210.0, 90.0], [70.0, 130.0, 255.0]];

/// The colour of a column: low, mid and high as red, green and blue, mixed by their shares and
/// brightened to full.
fn column_color(c: &Column) -> Color {
    let mut rgb = [0.0f32; 3];
    for (share, color) in c.bands.iter().zip(BAND_COLORS) {
        for (v, b) in rgb.iter_mut().zip(color) {
            *v += share.clamp(0.0, 1.0) * b;
        }
    }
    let top = rgb.iter().copied().fold(0.0, f32::max);
    let k = if top > 0.0 { 235.0 / top } else { 0.0 };
    // `as`: 0 to 235.
    let [r, g, b] = rgb.map(|v| (v * k).clamp(0.0, 255.0) as u8);
    [r, g, b, 255]
}

/// The upper lane's columns, kept while the envelopes, the span and the width stay.
#[derive(Default)]
pub struct ColumnCache {
    key: Option<(usize, usize, bool, usize, u64, u64)>,
    columns: Vec<Column>,
    /// The envelopes [`loudest`] was measured on (address, length) and its result.
    loudest: Option<((usize, usize), f64)>,
}

impl ColumnCache {
    /// [`loudest`], measured again only when the envelopes changed.
    pub fn loudest(&mut self, env: &Envelopes) -> f64 {
        let key = (std::ptr::from_ref(env) as usize, env.len());
        match self.loudest {
            Some((k, v)) if k == key => v,
            _ => {
                let v = loudest(env);
                self.loudest = Some((key, v));
                v
            }
        }
    }

    /// [`columns`], from the cache when asked for the same again.
    pub fn get(&mut self, env: &Envelopes, from: f64, to: f64, n: usize) -> &[Column] {
        let top = self.loudest(env);
        let key =
            (std::ptr::from_ref(env) as usize, env.len(), env.complete(), n, from.to_bits(), to.to_bits());
        if self.key != Some(key) {
            self.columns = columns(env, from, to, n, top);
            self.key = Some(key);
        }
        &self.columns
    }
}

/// Seconds as h:mm:ss, or m:ss under an hour.
pub fn clock(seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    // `as`: whole seconds of a sound file.
    let total = s.floor() as u64;
    let (h, m, sec) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{sec:02}") }
}

/// The step between time labels: the first of a list of round steps that leaves at least
/// [`LABEL_GAP`] between labels.
fn label_step(span: f64, width: f32) -> f64 {
    const STEPS: [f64; 14] =
        [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0];
    if !(span > 0.0 && width > 0.0) {
        return 3600.0;
    }
    let per_unit = span / f64::from(width);
    STEPS.iter().copied().find(|s| s / per_unit >= f64::from(LABEL_GAP)).unwrap_or(14_400.0)
}

/// The colour of every label, by order of first appearance.
fn section_colors(sections: &[Section]) -> HashMap<&str, Color> {
    let mut map = HashMap::new();
    for s in sections {
        let n = map.len();
        map.entry(s.label.as_str())
            .or_insert_with(|| SECTION_COLORS.get(n % SECTION_COLORS.len()).copied().unwrap_or(UNKNOWN));
    }
    map
}

/// The bar the moment `t` lies in, counted from 0, and how many bars there are.
pub fn bar_at(bars: &[f64], t: f64) -> Option<(usize, usize)> {
    if !t.is_finite() || bars.is_empty() {
        return None;
    }
    let i = bars.partition_point(|b| *b <= t);
    Some((i.checked_sub(1)?, bars.len()))
}

/// The tempo around `t`: beats per minute from the median spacing of the eight beats around it.
pub fn bpm_at(beats: &[f64], t: f64) -> Option<f64> {
    if !t.is_finite() {
        return None;
    }
    let i = beats.partition_point(|b| *b <= t);
    let near = beats.get(i.saturating_sub(4)..(i + 4).min(beats.len()))?;
    let mut gaps: Vec<f64> = near
        .windows(2)
        .filter_map(|w| Some(w.get(1)? - w.first()?))
        .filter(|g| g.is_finite() && *g > 0.0)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let g = *gaps.get(gaps.len() / 2)?;
    Some(60.0 / g)
}

/// Lays out and lists the shapes of `o` in `room`. `detail` is pixels per layout unit (the
/// waveform gets a column per pixel); `cache` keeps the upper lane's columns between calls.
pub fn shapes(
    room: Rect,
    o: &Overview<'_>,
    detail: f32,
    cache: &mut ColumnCache,
    palette: &Palette,
    measure: &dyn Measure,
) -> (Layout, Vec<Shape>) {
    let layout = Layout::new(room, o);
    let mut out = Vec::new();
    let Some(whole) = layout.whole else {
        if room.width() > 40.0 && room.height() > 16.0 {
            out.push(Shape::Text {
                at: room.center(),
                anchor: Anchor::CENTER_CENTER,
                size: 12.0,
                text: o.status.unwrap_or("no length yet").to_string(),
                color: palette.text_weak,
            });
        }
        return (layout, out);
    };
    let detail = if detail.is_finite() { detail.clamp(0.25, 8.0) } else { 1.0 };
    // `as`: a column count, bounded by MAX_COLUMNS.
    let n = |lane: &Lane| ((lane.wave.width() * detail).round().max(1.0) as usize).min(MAX_COLUMNS);
    let colors = section_colors(o.sections);
    let whole_cols = o.envelopes.map(|e| cache.get(e, whole.from, whole.to, n(&whole)).to_vec());
    lane(&mut out, &whole, o, whole_cols.as_deref(), &colors, false, palette, measure);
    if let Some(zoom) = layout.zoom {
        let cols = o.envelopes.map(|e| columns(e, zoom.from, zoom.to, n(&zoom), cache.loudest(e)));
        lane(&mut out, &zoom, o, cols.as_deref(), &colors, true, palette, measure);
        // What the lower lane shows, as a box in the upper one.
        let (a, b) = (whole.x_of(zoom.from), whole.x_of(zoom.to).max(whole.x_of(zoom.from) + 2.0));
        out.push(Shape::Frame {
            rect: Rect::from_min_max([a, whole.rect.min[1]], [b, whole.axis.min[1]]),
            radius: 0.0,
            width: 1.0,
            color: white_alpha(150),
        });
        if let Some(p) = o.playhead.filter(|p| p.is_finite()) {
            let mut line = format!("{} / {}", clock(p), clock(layout.length));
            // Bar and tempo only among the beats found: beyond them they would be guesses.
            let found = matches!((o.beats.first(), o.beats.last()), (Some(a), Some(b)) if *a <= p && p <= *b);
            if found {
                if let Some((bar, count)) = bar_at(o.bars, p) {
                    line.push_str(&format!("   bar {} of {count}", bar + 1));
                }
                if let Some(bpm) = bpm_at(o.beats, p) {
                    line.push_str(&format!("   {bpm:.1} BPM"));
                }
            }
            out.push(Shape::Text {
                at: [zoom.wave.min[0] + 4.0, zoom.wave.min[1] + 2.0],
                anchor: Anchor::LEFT_TOP,
                size: TEXT,
                text: line,
                color: palette.text,
            });
        }
    }
    if let Some(status) = o.status {
        let status = fit(status, whole.wave.width() - 12.0, TEXT, measure);
        let [tw, _] = measure.text_size(&status, TEXT);
        let at = [whole.wave.max[0] - 4.0 - tw, whole.wave.min[1] + 2.0];
        out.push(Shape::Rect {
            rect: Rect::from_min_size([at[0] - 3.0, at[1] - 1.0], [tw + 6.0, TEXT + 4.0]),
            radius: 2.0,
            color: fade(BACKGROUND, 0.85),
        });
        out.push(Shape::Text {
            at,
            anchor: Anchor::LEFT_TOP,
            size: TEXT,
            text: status,
            color: palette.warning,
        });
    }
    (layout, out)
}

/// `text` cut short with an ellipsis to fit `width` at `size`.
fn fit(text: &str, width: f32, size: f32, measure: &dyn Measure) -> String {
    if measure.text_size(text, size)[0] <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        n -= 1;
        let cut: String = chars.iter().take(n).collect::<String>() + "...";
        if measure.text_size(&cut, size)[0] <= width {
            return cut;
        }
    }
    String::new()
}

/// The shapes of one lane.
#[allow(clippy::too_many_arguments)]
fn lane(
    out: &mut Vec<Shape>,
    lane: &Lane,
    o: &Overview<'_>,
    columns: Option<&[Column]>,
    colors: &HashMap<&str, Color>,
    zoomed: bool,
    palette: &Palette,
    measure: &dyn Measure,
) {
    out.push(Shape::Rect { rect: lane.rect, radius: 0.0, color: BACKGROUND });
    let wave = lane.wave;
    let mid = (wave.min[1] + wave.max[1]) * 0.5;
    let half = wave.height() * 0.5;

    // Not measured: grey where no column is known, merged into runs.
    let cols = columns.unwrap_or(&[]);
    let n = cols.len().max(1);
    let cw = wave.width() / n as f32;
    let x_col = |i: usize| wave.min[0] + cw * i as f32;
    let mut run: Option<usize> = None;
    let unknown_at = |i: usize| columns.is_none() || cols.get(i).is_some_and(|c| !c.known);
    for i in 0..=n {
        let unknown = i < n && unknown_at(i);
        match (unknown, run) {
            (true, None) => run = Some(i),
            (false, Some(s)) => {
                out.push(Shape::Rect {
                    rect: Rect::from_min_max([x_col(s), wave.min[1]], [x_col(i), wave.max[1]]),
                    radius: 0.0,
                    color: UNKNOWN,
                });
                run = None;
            }
            _ => {}
        }
    }
    out.push(Shape::Line {
        from: [wave.min[0], mid],
        to: [wave.max[0], mid],
        width: 1.0,
        color: white_alpha(28),
    });
    for (i, c) in cols.iter().enumerate() {
        if !c.known {
            continue;
        }
        let color = column_color(c);
        let x0 = x_col(i);
        // A hair wider than the column: no seams between them.
        let x1 = x0 + cw + 0.05;
        let hp = (c.peak * half).max(0.5);
        out.push(Shape::Rect {
            rect: Rect::from_min_max([x0, mid - hp], [x1, mid + hp]),
            radius: 0.0,
            color: fade(color, 0.42),
        });
        let hm = (c.mean * half).max(0.5);
        out.push(Shape::Rect {
            rect: Rect::from_min_max([x0, mid - hm], [x1, mid + hm]),
            radius: 0.0,
            color,
        });
    }

    // Bars and beats, where they are far enough apart to tell.
    let span = lane.to - lane.from;
    let visible = |ts: &[f64]| -> (usize, usize) {
        let a = ts.partition_point(|t| *t < lane.from);
        let b = ts.partition_point(|t| *t <= lane.to);
        (a, b.max(a))
    };
    let gap_ok = |ts: &[f64], min: f32| -> bool {
        let (a, b) = visible(ts);
        let count = b - a;
        // `as`: layout units per line.
        count > 0 && count <= MAX_LINES && span > 0.0 && wave.width() / count as f32 >= min
    };
    if gap_ok(o.beats, MIN_BEAT_GAP) {
        let (a, b) = visible(o.beats);
        for t in o.beats.get(a..b).unwrap_or(&[]).iter().filter(|t| t.is_finite()) {
            let x = lane.x_of(*t);
            out.push(Shape::Line {
                from: [x, wave.max[1] - 5.0],
                to: [x, wave.max[1]],
                width: 1.0,
                color: white_alpha(90),
            });
        }
    }
    if gap_ok(o.bars, MIN_BAR_GAP) {
        let (a, b) = visible(o.bars);
        for t in o.bars.get(a..b).unwrap_or(&[]).iter().filter(|t| t.is_finite()) {
            let x = lane.x_of(*t);
            out.push(Shape::Line {
                from: [x, wave.min[1]],
                to: [x, wave.max[1]],
                width: 1.0,
                color: white_alpha(60),
            });
        }
    }

    // Not final: veiled from where the grid stops being final.
    let final_from = match o.final_until {
        None => Some(lane.from),
        Some(f) if f == f64::INFINITY => None,
        Some(f) if f.is_nan() => Some(lane.from),
        Some(f) => Some(f.max(lane.from)),
    };
    if let Some(f) = final_from.filter(|f| *f < lane.to) {
        let x = lane.x_of(f);
        out.push(Shape::Rect {
            rect: Rect::from_min_max([x, wave.min[1]], [wave.max[0], wave.max[1]]),
            radius: 0.0,
            color: fade(BACKGROUND, 0.45),
        });
        out.push(Shape::Line {
            from: [x, wave.min[1]],
            to: [wave.max[0], wave.min[1]],
            width: 2.0,
            color: fade(palette.warning, 0.8),
        });
        if zoomed {
            let text = if o.final_until.is_none() { "no grid yet" } else { "grid not final" };
            let [tw, _] = measure.text_size(text, TEXT);
            if wave.max[0] - x > tw + 8.0 {
                out.push(Shape::Text {
                    at: [x + 4.0, wave.max[1] - 2.0],
                    anchor: Anchor([0.0, 1.0]),
                    size: TEXT,
                    text: text.to_string(),
                    color: palette.warning,
                });
            }
        }
    }

    // Sections, or a quiet empty strip.
    let strip = lane.sections;
    out.push(Shape::Rect { rect: strip, radius: 0.0, color: white_alpha(10) });
    strip_shapes(out, lane, o.sections, colors, zoomed, measure);

    // The time axis.
    let axis = lane.axis;
    let step = label_step(span, lane.rect.width());
    let first = (lane.from / step).ceil();
    // `as`: a label count, small by the step's choice.
    let count = ((lane.to / step).floor() - first + 1.0).clamp(0.0, 512.0) as usize;
    for k in 0..count {
        let t = (first + k as f64) * step;
        let x = lane.x_of(t);
        out.push(Shape::Line {
            from: [x, axis.min[1]],
            to: [x, axis.min[1] + 3.0],
            width: 1.0,
            color: white_alpha(80),
        });
        let text = clock(t);
        let [tw, _] = measure.text_size(&text, TEXT - 1.0);
        if x + 2.0 + tw <= axis.max[0] {
            out.push(Shape::Text {
                at: [x + 2.0, axis.max[1]],
                anchor: Anchor([0.0, 1.0]),
                size: TEXT - 1.0,
                text,
                color: palette.text_weak,
            });
        }
    }

    // The playhead.
    if let Some(p) = o.playhead.filter(|p| p.is_finite() && *p >= lane.from && *p <= lane.to) {
        let x = lane.x_of(p);
        out.push(Shape::Line {
            from: [x, lane.rect.min[1]],
            to: [x, axis.min[1]],
            width: 1.5,
            color: palette.error,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Measure for Fixed {
        fn text_size(&self, text: &str, size: f32) -> [f32; 2] {
            [text.chars().count() as f32 * size * 0.55, size * 1.2]
        }
    }

    fn room(w: f32, h: f32) -> Rect {
        Rect::from_min_size([0.0, 0.0], [w, h])
    }

    fn empty<'a>() -> Overview<'a> {
        Overview {
            duration: 0.0,
            playhead: None,
            beats: &[],
            bars: &[],
            final_until: None,
            envelopes: None,
            sections: &[],
            span: DEFAULT_SPAN,
            status: None,
        }
    }

    fn draw(o: &Overview<'_>, w: f32, h: f32) -> (Layout, Vec<Shape>) {
        shapes(room(w, h), o, 1.0, &mut ColumnCache::default(), &Palette::STANDARD, &Fixed)
    }

    fn finite(shapes: &[Shape]) -> bool {
        let ok = |p: [f32; 2]| p[0].is_finite() && p[1].is_finite();
        shapes.iter().all(|s| match s {
            Shape::Rect { rect, .. } | Shape::Frame { rect, .. } => ok(rect.min) && ok(rect.max),
            Shape::Line { from, to, .. } => ok(*from) && ok(*to),
            Shape::Text { at, .. } => ok(*at),
            Shape::Circle { center, .. } | Shape::Arc { center, .. } => ok(*center),
        })
    }

    /// A tone that is loud in its second half, at 48 kHz.
    fn tone(seconds: f64) -> Envelopes {
        let rate = 48_000.0;
        // `as`: a sample count.
        let n = (seconds * rate) as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                let a = if t < seconds / 2.0 { 0.01 } else { 0.5 };
                (a * (std::f64::consts::TAU * 200.0 * t).sin()) as f32
            })
            .collect();
        penumbra_audio::beats::envelopes_of(&samples, 1, rate).unwrap()
    }

    #[test]
    fn the_window_follows_the_playhead_inside_the_file() {
        assert_eq!(window(Some(100.0), 60.0, 7000.0), (70.0, 130.0));
        assert_eq!(window(Some(5.0), 60.0, 7000.0), (0.0, 60.0));
        assert_eq!(window(Some(6990.0), 60.0, 7000.0), (6940.0, 7000.0));
        // A file shorter than the span shows all of it.
        assert_eq!(window(Some(10.0), 60.0, 30.0), (0.0, 30.0));
        // Nonsense: the default span from the start; a span clamped.
        assert_eq!(window(None, f64::NAN, 7000.0), (0.0, DEFAULT_SPAN));
        assert_eq!(window(Some(f64::NAN), 0.0, 7000.0), (0.0, MIN_SPAN));
        assert_eq!(window(Some(1.0), 60.0, f64::NAN), (0.0, MIN_SPAN));
    }

    #[test]
    fn a_click_is_the_moment_under_it() {
        let o = Overview { duration: 7200.0, playhead: Some(3600.0), span: 60.0, ..empty() };
        let l = Layout::new(room(808.0, 300.0), &o);
        let whole = l.whole.unwrap();
        let zoom = l.zoom.unwrap();
        assert!(whole.rect.max[1] < zoom.rect.min[1]);
        let mid = |r: &Rect| [r.center()[0], r.center()[1]];
        assert!((l.seconds_at(mid(&whole.rect)).unwrap() - 3600.0).abs() < 1.0);
        assert!((l.seconds_at([whole.rect.min[0], whole.rect.center()[1]]).unwrap()).abs() < 1e-9);
        // The lower lane: the minute around the playhead.
        assert!((l.seconds_at(mid(&zoom.rect)).unwrap() - 3600.0).abs() < 0.1);
        assert!((l.seconds_at([zoom.rect.max[0], zoom.rect.center()[1]]).unwrap() - 3630.0).abs() < 1e-6);
        // Between the lanes and outside: nothing.
        assert_eq!(l.seconds_at([400.0, (whole.rect.max[1] + zoom.rect.min[1]) / 2.0]), None);
        assert_eq!(l.seconds_at([-5.0, 10.0]), None);
        assert_eq!(l.seconds_at([f32::NAN, 10.0]), None);
        // x and t agree both ways.
        assert!((whole.t_of(whole.x_of(1234.5)) - 1234.5).abs() < 0.1);
    }

    #[test]
    fn a_low_room_has_one_lane_and_a_tiny_one_none() {
        let o = Overview { duration: 60.0, ..empty() };
        let l = Layout::new(room(400.0, 80.0), &o);
        assert!(l.whole.is_some() && l.zoom.is_none());
        let l = Layout::new(room(30.0, 20.0), &o);
        assert!(l.whole.is_none() && l.seconds_at([5.0, 5.0]).is_none());
    }

    #[test]
    fn hostile_input_draws_without_failing() {
        // Nothing at all: a line of text, no lanes.
        let (l, s) = draw(&empty(), 640.0, 360.0);
        assert!(l.whole.is_none());
        assert!(matches!(s.as_slice(), [Shape::Text { .. }]));
        // Zero and nonsense sizes.
        assert!(draw(&empty(), 0.0, 0.0).1.is_empty());
        assert!(finite(&draw(&Overview { duration: 10.0, ..empty() }, f32::NAN, 100.0).1));
        // NaN everywhere.
        let nan = [f64::NAN, 1.0, f64::NAN, 2.0];
        let sections = [
            Section {
                start: f64::NAN,
                end: 3.0,
                label: "A".into(),
                text: "A".into(),
                track_start: true,
                zone: (0.0, 1.0),
            },
            Section {
                start: 5.0,
                end: 4.0,
                label: "B".into(),
                text: "B".into(),
                track_start: false,
                zone: (f64::NAN, 9.0),
            },
            Section {
                start: 1.0,
                end: f64::INFINITY,
                label: String::new(),
                text: String::new(),
                track_start: true,
                zone: (f64::NEG_INFINITY, f64::INFINITY),
            },
            Section {
                start: 6.0,
                end: 8.0,
                label: "A".into(),
                text: String::new(),
                track_start: false,
                zone: (7.0, 5.0),
            },
            Section {
                start: 8.0,
                end: 9.0,
                label: "B".into(),
                text: String::new(),
                track_start: true,
                zone: (7.5, 8.5),
            },
        ];
        let o = Overview {
            duration: f64::NAN,
            playhead: Some(f64::NAN),
            beats: &nan,
            bars: &nan,
            final_until: Some(f64::NAN),
            sections: &sections,
            span: f64::NAN,
            status: Some("finding beats 3 %"),
            ..empty()
        };
        let (_, s) = draw(&o, 640.0, 360.0);
        assert!(finite(&s));
        // Envelopes with no hops: grey, not measured.
        let none = penumbra_audio::beats::envelopes_of(&[], 1, 48_000.0).unwrap();
        let o = Overview { duration: 30.0, envelopes: Some(&none), ..empty() };
        let (_, s) = draw(&o, 640.0, 360.0);
        assert!(finite(&s));
        assert!(s.iter().any(|s| matches!(s, Shape::Rect { color, .. } if *color == UNKNOWN)));
    }

    #[test]
    fn columns_show_where_the_sound_is_loud() {
        let env = tone(4.0);
        let top = loudest(&env);
        assert!(top > 0.0);
        let cols = columns(&env, 0.0, 4.0, 40, top);
        assert_eq!(cols.len(), 40);
        assert!(cols.iter().all(|c| c.known && c.peak >= c.mean));
        let quiet = cols.get(5).unwrap();
        let loud = cols.get(35).unwrap();
        // Amplitude 0.01 against 0.5: a fiftieth of the height.
        assert!(quiet.mean < 0.05 && loud.mean > 0.8, "{quiet:?} {loud:?}");
        // 200 Hz lies above the low band's 150 Hz: mid leads, the shares add up to 1.
        assert!(loud.bands[1] > 0.5, "{loud:?}");
        assert!((loud.bands.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{loud:?}");
        // Past the measured end: unknown. Narrower than a hop: still one hop each.
        let beyond = columns(&env, 3.0, 6.0, 30, top);
        assert!(beyond.first().unwrap().known && !beyond.last().unwrap().known);
        let fine = columns(&env, 1.0, 1.01, 50, top);
        assert!(fine.iter().all(|c| c.known));
        // Nonsense spans, counts and references.
        assert!(columns(&env, 2.0, 1.0, 10, top).iter().all(|c| !c.known));
        assert!(columns(&env, f64::NAN, 1.0, 10, top).iter().all(|c| !c.known));
        assert!(columns(&env, 0.0, 1.0, 0, top).is_empty());
        assert_eq!(columns(&env, 0.0, 1.0, usize::MAX, top).len(), MAX_COLUMNS);
        for bad in [0.0, f64::NAN, -1.0] {
            assert!(columns(&env, 0.0, 4.0, 10, bad).iter().all(|c| c.known && c.peak == 0.0));
        }
    }

    #[test]
    fn the_cache_hands_back_the_same_columns_until_something_changes() {
        let env = tone(2.0);
        let mut cache = ColumnCache::default();
        let a = cache.get(&env, 0.0, 2.0, 100).to_vec();
        assert_eq!(cache.get(&env, 0.0, 2.0, 100), a.as_slice());
        assert_eq!(cache.get(&env, 0.0, 2.0, 50).len(), 50);
    }

    #[test]
    fn bars_and_beats_show_only_when_far_enough_apart() {
        let beats: Vec<f64> = (0..14_000).map(|i| f64::from(i) * 0.5).collect();
        let bars: Vec<f64> = beats.iter().step_by(4).copied().collect();
        let lines = |o: &Overview<'_>| {
            draw(o, 808.0, 300.0)
                .1
                .iter()
                .filter(|s| matches!(s, Shape::Line { width, .. } if *width == 1.0))
                .count()
        };
        // Two hours: no bar lines in the upper lane; a minute below: 30 bars and 120 beats.
        let o = Overview {
            duration: 7000.0,
            playhead: Some(3000.0),
            beats: &beats,
            bars: &bars,
            final_until: Some(f64::INFINITY),
            span: 60.0,
            ..empty()
        };
        let n = lines(&o);
        assert!(n > 150 && n < 200, "{n}");
        // A short span shows bars in both lanes.
        let short = Overview { duration: 60.0, ..o };
        assert!(lines(&short) > n);
    }

    #[test]
    fn sections_are_coloured_by_label() {
        let s = |a: f64, b: f64, l: &str| Section {
            start: a,
            end: b,
            label: l.into(),
            text: l.into(),
            track_start: false,
            zone: (a, a),
        };
        let sections = [s(0.0, 10.0, "A"), s(10.0, 20.0, "B"), s(20.0, 30.0, "A")];
        let colors = section_colors(&sections);
        assert_eq!(colors.len(), 2);
        assert_ne!(colors.get("A"), colors.get("B"));
        let o = Overview {
            duration: 30.0,
            playhead: Some(15.0),
            sections: &sections,
            final_until: Some(f64::INFINITY),
            ..empty()
        };
        let (_, shapes) = draw(&o, 640.0, 360.0);
        let a = colors.get("A").copied().unwrap();
        // Two A sections in each lane.
        assert_eq!(
            shapes.iter().filter(|x| matches!(x, Shape::Rect { color, .. } if *color == a)).count(),
            4
        );
        assert!(shapes.iter().any(|x| matches!(x, Shape::Text { text, .. } if text == "B")));
    }

    #[test]
    fn track_starts_and_blend_zones_are_marked_in_the_strip() {
        let s = |a: f64, b: f64, l: &str, track_start: bool, zone: (f64, f64)| Section {
            start: a,
            end: b,
            label: l.into(),
            text: String::new(),
            track_start,
            zone,
        };
        // Two tracks: A then B with a cut, then a blend from B into C over 18 to 22 s, which
        // starts the second track.
        let sections = [
            s(0.0, 10.0, "A", true, (0.0, 0.0)),
            s(10.0, 20.0, "B", false, (10.0, 10.0)),
            s(20.0, 30.0, "C", true, (18.0, 22.0)),
        ];
        let colors = section_colors(&sections);
        let (b, c) = (colors.get("B").copied().unwrap(), colors.get("C").copied().unwrap());
        let o = Overview { duration: 30.0, sections: &sections, final_until: Some(f64::INFINITY), ..empty() };
        let (layout, shapes) = draw(&o, 640.0, 360.0);
        let zoom = layout.zoom.unwrap();
        let strip = zoom.sections;
        let in_zoom_strip = |y: f32| y >= strip.min[1] - 0.01 && y <= strip.max[1] + 0.01;
        // Track starts: white edges reaching below the strip, at 0 and 20 s in each lane.
        let edges: Vec<f32> = shapes
            .iter()
            .filter_map(|x| match x {
                Shape::Line { from, to, color, .. } if *color == TRACK_EDGE && in_zoom_strip(from[1]) => {
                    assert!(to[1] > strip.max[1]);
                    Some(from[0])
                }
                _ => None,
            })
            .collect();
        assert_eq!(edges.len(), 2, "{edges:?}");
        assert!((edges[1] - zoom.x_of(20.0)).abs() < 1e-3);
        // The section boundary at 10 s is a plain dark edge.
        assert!(shapes.iter().any(|x| matches!(x, Shape::Line { from, color, .. }
            if *color == BACKGROUND && (from[0] - zoom.x_of(10.0)).abs() < 1e-3 && in_zoom_strip(from[1]))));
        // The zone: steps in the strip from 18 to 22 s, B's colour fading into C's, a line under.
        let steps: Vec<(f32, Color)> = shapes
            .iter()
            .filter_map(|x| match x {
                Shape::Rect { rect, color, .. }
                    if in_zoom_strip(rect.min[1])
                        && rect.min[0] >= zoom.x_of(18.0) - 0.01
                        && rect.max[0] <= zoom.x_of(22.0) + 0.1
                        && rect.width() < 10.0 =>
                {
                    Some((rect.min[0], *color))
                }
                _ => None,
            })
            .collect();
        assert!(steps.len() >= 8, "{}", steps.len());
        let (first, last) = (steps.first().unwrap().1, steps.last().unwrap().1);
        let near = |x: Color, y: Color| x.iter().zip(y).all(|(p, q)| p.abs_diff(q) < 40);
        assert!(near(first, b) && near(last, c), "{first:?} {last:?}");
        assert!(steps.iter().any(|(_, k)| *k != b && *k != c));
        assert!(shapes.iter().any(|x| matches!(x, Shape::Line { from, to, color, .. }
            if *color == ZONE_LINE && (from[0] - zoom.x_of(18.0)).abs() < 1e-3 && (to[0] - zoom.x_of(22.0)).abs() < 1e-3)));
        // A zone of one colour still gets its line; a cut gets none.
        let same = [s(0.0, 10.0, "A", true, (0.0, 0.0)), s(10.0, 20.0, "A", true, (9.0, 11.0))];
        let o = Overview { duration: 20.0, sections: &same, ..empty() };
        let lines = draw(&o, 640.0, 360.0)
            .1
            .iter()
            .filter(|x| matches!(x, Shape::Line { color, .. } if *color == ZONE_LINE))
            .count();
        assert_eq!(lines, 2);
        assert_eq!(mix([0, 0, 0, 255], [200, 100, 50, 255], 0.5), [100, 50, 25, 255]);
        assert_eq!(mix([0, 0, 0, 255], [200, 100, 50, 255], f32::NAN), [0, 0, 0, 255]);
    }

    #[test]
    fn what_is_not_final_is_veiled() {
        let veils = |f: Option<f64>| {
            let o = Overview { duration: 100.0, final_until: f, ..empty() };
            draw(&o, 640.0, 360.0)
                .1
                .iter()
                .filter(|s| matches!(s, Shape::Line { width, .. } if *width == 2.0))
                .count()
        };
        assert_eq!(veils(Some(f64::INFINITY)), 0);
        // The upper lane only: the lower one shows the first minute.
        assert_eq!(veils(Some(80.0)), 1);
        assert_eq!(veils(Some(f64::NEG_INFINITY)), 2);
        assert_eq!(veils(None), 2);
    }

    #[test]
    fn clocks_labels_bars_and_tempo() {
        let long = "no beats: the beat network is not in assets/models beside penumbra";
        let cut = fit(long, 100.0, 10.0, &Fixed);
        assert!(cut.ends_with("...") && Fixed.text_size(&cut, 10.0)[0] <= 100.0, "{cut}");
        assert_eq!(fit("short", 100.0, 10.0, &Fixed), "short");
        assert_eq!(fit("short", -1.0, 10.0, &Fixed), "");
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(75.9), "1:15");
        assert_eq!(clock(7041.0), "1:57:21");
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(clock(-3.0), "0:00");
        assert_eq!(label_step(7200.0, 800.0), 600.0);
        assert_eq!(label_step(60.0, 800.0), 5.0);
        assert_eq!(label_step(0.0, 800.0), 3600.0);
        let bars = [1.0, 3.0, 5.0];
        assert_eq!(bar_at(&bars, 0.5), None);
        assert_eq!(bar_at(&bars, 3.5), Some((1, 3)));
        assert_eq!(bar_at(&bars, f64::NAN), None);
        let beats: Vec<f64> = (0..20).map(|i| f64::from(i) * 0.5).collect();
        assert!((bpm_at(&beats, 5.0).unwrap() - 120.0).abs() < 1e-9);
        assert_eq!(bpm_at(&[], 5.0), None);
        assert_eq!(bpm_at(&beats, f64::NAN), None);
    }
}
