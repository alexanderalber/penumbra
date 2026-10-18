//! The look of an audio analysis' meters, made to be read at a glance during a show: level and
//! band meters with a peak hold, a beat light with the beat's phase as a ring, the bar as one
//! cell per beat, the tempo in large type (dimmed while unsure), a strip of recent kicks,
//! onsets and beats, and the spectrum.
//!
//! One description serves two painters: the viewer paints the shapes [`Meters::shapes`] lists
//! with egui, `tex.audio_meters` draws the same list into a texture on the GPU. Nothing here
//! knows either; text is measured through [`Measure`], which each painter provides.
//!
//! The analysis only reports the frame an event happens in; the short memory a meter needs
//! (peaks, recent hits, when the last beat came) is kept in [`Meters`], by whoever draws.

use std::collections::VecDeque;

use penumbra_core::Chans;

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
/// The bar length and band edges assumed when the analysis does not say (older outputs).
const DEFAULT_BEATS_PER_BAR: u32 = 4;
const DEFAULT_LOW_HZ: f64 = 150.0;
const DEFAULT_HIGH_HZ: f64 = 2500.0;

/// The meters, in the analysis' channel order: channel, label, label where little room.
const METERS: [(&str, &str, &str); 4] =
    [("level", "level", "lvl"), ("low", "low", "lo"), ("mid", "mid", "mid"), ("high", "high", "hi")];
/// The smallest type drawn, in layout units.
const MIN_TEXT: f32 = 9.0;
/// Below this height (layout units) the meters leave out the spectrum and the strip's labels.
pub const COMPACT: f32 = 150.0;

/// A colour as an 8-bit UI has it: sRGB-encoded, premultiplied alpha (egui's `Color32`).
pub type Color = [u8; 4];

/// `c` with every component (alpha too) scaled by `f`, as egui's `gamma_multiply`.
pub fn fade(c: Color, f: f32) -> Color {
    let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
    // `as`: 0 to 255.5, truncated as egui rounds.
    c.map(|v| (f32::from(v) * f + 0.5) as u8)
}

/// White at `a` of full coverage.
pub const fn white_alpha(a: u8) -> Color {
    [a, a, a, a]
}

/// The few colours the meters use; the UI's theme owns them, [`Palette::STANDARD`] is the same
/// set for drawing outside the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub text_weak: Color,
    pub accent: Color,
    pub warning: Color,
    pub error: Color,
    /// The colours of the first four channels: level, low, mid, high.
    pub channels: [Color; 4],
}

impl Palette {
    /// The UI's colours (`penumbra-ui`'s theme checks that it matches).
    pub const STANDARD: Palette = Palette {
        text: [220, 220, 220, 255],
        text_weak: [140, 140, 140, 255],
        accent: [90, 160, 255, 255],
        warning: [240, 180, 60, 255],
        error: [235, 80, 70, 255],
        channels: [[110, 210, 120, 255], [240, 160, 80, 255], [110, 170, 240, 255], [230, 110, 160, 255]],
    };

    fn channel(&self, i: usize) -> Color {
        self.channels.get(i).copied().unwrap_or(self.text)
    }
}

/// An axis-aligned rectangle in layout units, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Rect {
    pub fn from_min_max(min: [f32; 2], max: [f32; 2]) -> Rect {
        Rect { min, max }
    }

    pub fn from_min_size(min: [f32; 2], size: [f32; 2]) -> Rect {
        Rect { min, max: [min[0] + size[0], min[1] + size[1]] }
    }

    pub fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    pub fn center(&self) -> [f32; 2] {
        [(self.min[0] + self.max[0]) * 0.5, (self.min[1] + self.max[1]) * 0.5]
    }

    fn shrink(&self, by: f32) -> Rect {
        Rect { min: [self.min[0] + by, self.min[1] + by], max: [self.max[0] - by, self.max[1] - by] }
    }
}

/// Where a text's box sits relative to its point: 0 left or top, 0.5 centre, 1 right or bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor(pub [f32; 2]);

impl Anchor {
    pub const LEFT_TOP: Anchor = Anchor([0.0, 0.0]);
    pub const LEFT_CENTER: Anchor = Anchor([0.0, 0.5]);
    pub const CENTER_CENTER: Anchor = Anchor([0.5, 0.5]);
    pub const CENTER_BOTTOM: Anchor = Anchor([0.5, 1.0]);
}

/// One thing to paint, in layout units, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A filled rectangle with rounded corners.
    Rect {
        rect: Rect,
        radius: f32,
        color: Color,
    },
    /// A rectangle's outline, `width` wide, inside its edge.
    Frame {
        rect: Rect,
        radius: f32,
        width: f32,
        color: Color,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
        color: Color,
    },
    /// A ring's line from the top, clockwise, for `fraction` of a turn.
    Arc {
        center: [f32; 2],
        radius: f32,
        width: f32,
        fraction: f32,
        color: Color,
    },
    Line {
        from: [f32; 2],
        to: [f32; 2],
        width: f32,
        color: Color,
    },
    /// One line of proportional text of `size`, its box (as [`Measure`] reports it) placed by
    /// `anchor` at `at`.
    Text {
        at: [f32; 2],
        anchor: Anchor,
        size: f32,
        text: String,
        color: Color,
    },
}

impl Shape {
    /// The shape with every coordinate and length times `k`.
    pub fn scaled(self, k: f32) -> Shape {
        let p = |p: [f32; 2]| [p[0] * k, p[1] * k];
        let r = |r: Rect| Rect { min: p(r.min), max: p(r.max) };
        match self {
            Shape::Rect { rect, radius, color } => Shape::Rect { rect: r(rect), radius: radius * k, color },
            Shape::Frame { rect, radius, width, color } => {
                Shape::Frame { rect: r(rect), radius: radius * k, width: width * k, color }
            }
            Shape::Circle { center, radius, color } => {
                Shape::Circle { center: p(center), radius: radius * k, color }
            }
            Shape::Arc { center, radius, width, fraction, color } => {
                Shape::Arc { center: p(center), radius: radius * k, width: width * k, fraction, color }
            }
            Shape::Line { from, to, width, color } => {
                Shape::Line { from: p(from), to: p(to), width: width * k, color }
            }
            Shape::Text { at, anchor, size, text, color } => {
                Shape::Text { at: p(at), anchor, size: size * k, text, color }
            }
        }
    }
}

/// Measures text for the layout: the size of one line of `text` in proportional type of `size`
/// (the line's box: width, and height including the line gap).
pub trait Measure {
    fn text_size(&self, text: &str, size: f32) -> [f32; 2];
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

    fn color(self, palette: &Palette) -> Color {
        match self {
            // As the low band: a kick is a hit in the low band.
            Hit::Kick => palette.channel(1),
            Hit::Onset => palette.channel(2),
            Hit::Beat => palette.text,
        }
    }
}

/// The values read from one `out` output of the analysis; channels that are missing read 0
/// (the bar and the band edges their defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub meters: [f64; 4],
    pub onset: bool,
    pub kick: bool,
    pub tick: bool,
    pub beat: f64,
    pub bar: f64,
    pub bar_beat: u32,
    pub bpm: f64,
    pub confidence: f64,
    /// The bar's length in beats.
    pub beats_per_bar: u32,
    /// Where the low band ends and the high one starts, in Hz, for the spectrum's colours.
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Default for Reading {
    fn default() -> Self {
        Reading {
            meters: [0.0; 4],
            onset: false,
            kick: false,
            tick: false,
            beat: 0.0,
            bar: 0.0,
            bar_beat: 0,
            bpm: 0.0,
            confidence: 0.0,
            beats_per_bar: DEFAULT_BEATS_PER_BAR,
            low_hz: DEFAULT_LOW_HZ,
            high_hz: DEFAULT_HIGH_HZ,
        }
    }
}

impl Reading {
    pub fn of(chans: &Chans) -> Reading {
        let get = |name: &str| chans.value(Some(name)).filter(|v| v.is_finite());
        let v = |name: &str| get(name).unwrap_or(0.0);
        Reading {
            meters: METERS.map(|(name, _, _)| v(name).clamp(0.0, 1.0)),
            onset: v("onset") > 0.5,
            kick: v("kick") > 0.5,
            tick: v("tick") > 0.5,
            beat: v("beat").clamp(0.0, 1.0),
            bar: v("bar").clamp(0.0, 1.0),
            // `as`: small counts, clamped first.
            bar_beat: v("bar_beat").round().clamp(0.0, 1000.0) as u32,
            bpm: v("bpm").max(0.0),
            confidence: v("confidence").clamp(0.0, 1.0),
            beats_per_bar: get("beats_per_bar")
                .map_or(DEFAULT_BEATS_PER_BAR, |b| b.round().clamp(1.0, 64.0) as u32),
            low_hz: get("low_hz").filter(|h| *h > 0.0).map_or(DEFAULT_LOW_HZ, |h| h.min(100_000.0)),
            high_hz: get("high_hz").filter(|h| *h > 0.0).map_or(DEFAULT_HIGH_HZ, |h| h.min(100_000.0)),
        }
    }
}

/// The meters' memory: held peaks, recent hits, the last beat. Times are seconds on any
/// steady clock.
#[derive(Clone, Debug, Default)]
pub struct Meters {
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
    /// The latest time seen, to notice the clock going back.
    seen: Option<f64>,
}

impl Meters {
    /// Takes in the analysis' `out` at `now`: when `fresh` (a new cook, not a repaint of the
    /// same one) its hits are added; every call lets the peaks fall. A clock that went back
    /// (a jump in the show) starts the memory over.
    pub fn update(&mut self, out: Option<&Chans>, fresh: bool, now: f64) -> Reading {
        let reading = out.map(Reading::of).unwrap_or_default();
        if !now.is_finite() {
            return reading;
        }
        if self.seen.is_some_and(|t| now < t) {
            *self = Meters::default();
        }
        self.seen = Some(now);
        if fresh && out.is_some() {
            if reading.kick {
                self.hit(now, Hit::Kick, false);
                self.last_kick = Some(now);
            }
            if reading.onset {
                self.hit(now, Hit::Onset, false);
                self.last_onset = Some(now);
            }
            // A tick missed between two draws still shows: the phase has started over.
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

    /// Hits in the strip.
    pub fn hit_count(&self) -> usize {
        self.hits.len()
    }

    /// The highest held peak at `now`.
    pub fn highest_peak(&self, now: f64) -> f64 {
        self.peaks.iter().map(|p| falling(*p, now)).fold(0.0, f64::max)
    }

    /// The meters for `r` in `rect` at `now` (as last passed to [`Meters::update`]): `spectrum`
    /// is the analysis' spectrum output's samples, if there is one; `analysed` whether there
    /// is an `out` with channels at all.
    #[allow(clippy::too_many_arguments)]
    pub fn shapes(
        &self,
        rect: Rect,
        r: &Reading,
        spectrum: Option<&[f64]>,
        analysed: bool,
        palette: &Palette,
        measure: &dyn Measure,
        now: f64,
    ) -> Vec<Shape> {
        let mut out = Vec::new();
        let compact = rect.height() < COMPACT;
        let area = rect.shrink(if compact { 5.0 } else { 8.0 });
        if area.width() < 40.0 || area.height() < 40.0 {
            return out;
        }
        let draw = Draw { out: &mut out, palette, measure, now, s: 1.0 };
        // Everything scales with the room: 1 at about 250 x 135.
        let s = (area.width() / 250.0).min(area.height() / 135.0).clamp(0.7, 3.0);
        let mut draw = Draw { s, ..draw };
        let gap = 8.0 * s;
        // The spectrum is the first thing to go where room is short.
        let spectrum = spectrum.filter(|_| !compact);
        let spectrum_h = if spectrum.is_some() { (area.height() * 0.24).max(18.0) } else { 0.0 };
        let top = Rect::from_min_max(area.min, [area.max[0], area.max[1] - spectrum_h - gap.min(spectrum_h)]);
        let meters_w = (top.width() * 0.34).min(top.height() * 1.6);
        let meters = Rect::from_min_max(top.min, [top.min[0] + meters_w, top.max[1]]);
        let right = Rect::from_min_max([meters.max[0] + gap * 1.5, top.min[1]], top.max);
        draw.meters(self, meters, r);
        let tempo_h = right.height() * 0.45;
        let cells_h = right.height() * 0.24;
        let tempo = Rect::from_min_size(right.min, [right.width(), tempo_h]);
        let cells = Rect::from_min_size([right.min[0], tempo.max[1] + gap * 0.5], [right.width(), cells_h]);
        let strip = Rect::from_min_max([right.min[0], cells.max[1] + gap * 0.75], right.max);
        draw.tempo(self, tempo, r);
        draw.cells(cells, r);
        draw.strip(self, strip, !compact);
        if let Some(spectrum) = spectrum {
            let at = Rect::from_min_max([area.min[0], area.max[1] - spectrum_h], area.max);
            draw.spectrum(at, spectrum, r);
        }
        if !analysed {
            out.push(Shape::Text {
                at: rect.center(),
                anchor: Anchor::CENTER_CENTER,
                size: 14.0,
                text: "no analysis yet".into(),
                color: palette.text_weak,
            });
        }
        out
    }
}

/// What the parts of [`Meters::shapes`] share.
struct Draw<'a> {
    out: &'a mut Vec<Shape>,
    palette: &'a Palette,
    measure: &'a dyn Measure,
    now: f64,
    /// The scale: 1 at about 250 x 135.
    s: f32,
}

impl Draw<'_> {
    fn rect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.out.push(Shape::Rect { rect, radius, color });
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], width: f32, color: Color) {
        self.out.push(Shape::Line { from, to, width, color });
    }

    fn text(&mut self, at: [f32; 2], anchor: Anchor, size: f32, text: impl Into<String>, color: Color) {
        self.out.push(Shape::Text { at, anchor, size, text: text.into(), color });
    }

    /// Four vertical bars with a peak line each, labelled below.
    fn meters(&mut self, m: &Meters, rect: Rect, r: &Reading) {
        let s = self.s;
        let font = text_size(9.0 * s);
        let label_h = font + 3.0 * s;
        let n = METERS.len() as f32;
        let gap = 4.0 * s;
        let w = ((rect.width() - gap * (n - 1.0)) / n).max(2.0);
        let faint = white_alpha(14);
        for (i, ((_, label, short), (v, peak))) in METERS.iter().zip(r.meters.iter().zip(m.peaks)).enumerate()
        {
            let x = rect.min[0] + i as f32 * (w + gap);
            let well = Rect::from_min_max([x, rect.min[1]], [x + w, rect.max[1] - label_h]);
            self.rect(well, 2.0, faint);
            let color = self.palette.channel(i);
            let to_y = |v: f64| well.max[1] - (v.clamp(0.0, 1.0) as f32) * well.height();
            let fill = Rect::from_min_max([well.min[0], to_y(*v)], well.max);
            self.rect(fill, 2.0, color);
            let held = falling(peak, self.now);
            if held > 0.005 {
                let y = to_y(held);
                self.line([well.min[0], y], [well.max[0], y], 2.0 * s.min(1.5), self.palette.text);
            }
            let weak = self.palette.text_weak;
            self.text(
                [well.center()[0], rect.max[1]],
                Anchor::CENTER_BOTTOM,
                font,
                if w < 26.0 { *short } else { *label },
                weak,
            );
        }
    }

    /// The tempo in large type with its confidence as a bar, and the beat light: a disc that
    /// flashes on each beat (amber on a bar's first) inside a ring filling with the beat's phase.
    fn tempo(&mut self, m: &Meters, rect: Rect, r: &Reading) {
        let s = self.s;
        let p = *self.palette;
        let d = rect.height().min(rect.width() * 0.4);
        let center = [rect.max[0] - d * 0.5, rect.center()[1]];
        let radius = d * 0.5 - 2.0 * s;
        let flash = flash(m.last_beat, self.now);
        let beat_color = if m.last_beat_down { p.warning } else { p.accent };
        self.out.push(Shape::Circle { center, radius, color: white_alpha(10) });
        if flash > 0.0 {
            self.out.push(Shape::Circle { center, radius: radius * 0.78, color: fade(beat_color, flash) });
        }
        if r.bpm > 0.0 {
            self.out.push(Shape::Arc {
                center,
                radius,
                width: (3.0 * s).max(2.0),
                fraction: r.beat.clamp(0.0, 1.0) as f32,
                color: fade(beat_color, 0.85),
            });
        }
        // The number, as large as fits left of the light.
        let text_w = (center[0] - radius - rect.min[0] - 6.0 * s).max(0.0);
        let unsure = 0.3 + 0.7 * r.confidence as f32;
        let (number, color) = if r.bpm > 0.0 {
            (format!("{:.0}", r.bpm), fade(p.text, unsure))
        } else {
            ("--".to_string(), p.text_weak)
        };
        let conf_h = 4.0 * s;
        let caption = text_size(10.0 * s);
        let caption_h = caption + 2.0 * s;
        let mut size = ((rect.height() - conf_h - caption_h - 4.0 * s) * 0.95).clamp(8.0, 96.0 * s);
        let mut box_size = self.measure.text_size(&number, size);
        if box_size[0] > text_w && box_size[0] > 0.0 {
            size = (size * text_w / box_size[0]).max(6.0);
            box_size = self.measure.text_size(&number, size);
        }
        let number_at = rect.min;
        let number_h = box_size[1];
        self.text(number_at, Anchor::LEFT_TOP, size, number, color);
        self.text(
            [rect.min[0], number_at[1] + number_h],
            Anchor::LEFT_TOP,
            caption,
            if r.bpm > 0.0 {
                format!("bpm, {:.0} % sure", r.confidence * 100.0)
            } else {
                "no tempo yet".to_string()
            },
            p.text_weak,
        );
        let bar = Rect::from_min_size(
            [rect.min[0], (number_at[1] + number_h + caption_h + 2.0 * s).min(rect.max[1] - conf_h)],
            [text_w, conf_h],
        );
        self.rect(bar, 1.0, white_alpha(14));
        let sure = Rect::from_min_size(bar.min, [bar.width() * r.confidence as f32, bar.height()]);
        self.rect(sure, 1.0, confidence_color(r.confidence, &p));
    }

    /// One cell per beat of the bar, the current one lit and filling with the beat's phase, the
    /// first marked; the bar's phase as a line below.
    fn cells(&mut self, rect: Rect, r: &Reading) {
        let s = self.s;
        let p = *self.palette;
        let line_h = 3.0 * s;
        let cells = Rect::from_min_max(rect.min, [rect.max[0], rect.max[1] - line_h - 3.0 * s]);
        let count = r.beats_per_bar.max(1) as usize;
        let current = r.bar_beat as usize;
        let known = r.bpm > 0.0;
        if count > MAX_CELLS {
            self.text(
                [cells.min[0], cells.center()[1]],
                Anchor::LEFT_CENTER,
                text_size(cells.height().min(18.0 * s)),
                format!("beat {} of {count}", current + 1),
                p.text,
            );
        } else {
            let gap = 4.0 * s;
            let w = ((cells.width() - gap * (count as f32 - 1.0)) / count as f32).max(1.0);
            let font = text_size((cells.height() * 0.5).min(16.0 * s));
            for k in 0..count {
                let x = cells.min[0] + k as f32 * (w + gap);
                let cell = Rect::from_min_max([x, cells.min[1]], [x + w, cells.max[1]]);
                let first = k == 0;
                let color = if first { p.warning } else { p.accent };
                let lit = known && k == current;
                self.rect(cell, 3.0, white_alpha(if lit { 0 } else { 12 }));
                if lit {
                    self.rect(cell, 3.0, fade(color, 0.35));
                    let done = Rect::from_min_size(cell.min, [w * r.beat as f32, cell.height()]);
                    self.rect(done, 3.0, fade(color, 0.8));
                }
                if first {
                    self.out.push(Shape::Frame {
                        rect: cell,
                        radius: 3.0,
                        width: 1.5 * s.min(1.5),
                        color: p.warning,
                    });
                }
                if w > 14.0 {
                    self.text(
                        cell.center(),
                        Anchor::CENTER_CENTER,
                        font,
                        format!("{}", k + 1),
                        if lit { p.text } else { p.text_weak },
                    );
                }
            }
        }
        let line = Rect::from_min_size([rect.min[0], rect.max[1] - line_h], [rect.width(), line_h]);
        self.rect(line, 0.0, white_alpha(14));
        if known {
            let done = Rect::from_min_size(line.min, [line.width() * r.bar as f32, line_h]);
            self.rect(done, 0.0, fade(p.warning, 0.8));
        }
    }

    /// The last seconds of kicks, onsets and beats as marks moving left, a row each, with a
    /// light at the start of each row that flashes on its hit (and the row's name, if
    /// `labels`).
    fn strip(&mut self, m: &Meters, rect: Rect, labels: bool) {
        if rect.height() < 6.0 {
            return;
        }
        let s = self.s;
        let p = *self.palette;
        let rows = HITS.len() as f32;
        let row_h = rect.height() / rows;
        let font = text_size((row_h * 0.75).min(11.0 * s));
        let dot = (row_h * 0.3).clamp(2.0, (4.0 * s).max(2.0));
        let labels = labels && row_h >= MIN_TEXT;
        let label_w = if labels { 40.0 * s.max(1.0) } else { dot * 2.0 + 4.0 };
        let track = Rect::from_min_max([rect.min[0] + label_w, rect.min[1]], rect.max);
        self.rect(track, 2.0, white_alpha(8));
        for (i, kind) in HITS.iter().enumerate() {
            let y0 = rect.min[1] + i as f32 * row_h;
            let mid = y0 + row_h * 0.5;
            let last = match kind {
                Hit::Kick => m.last_kick,
                Hit::Onset => m.last_onset,
                Hit::Beat => m.last_beat,
            };
            let lit = flash(last, self.now);
            let color = kind.color(&p);
            self.out.push(Shape::Circle {
                center: [rect.min[0] + dot, mid],
                radius: dot,
                color: if lit > 0.0 { fade(color, 0.3 + 0.7 * lit) } else { fade(color, 0.25) },
            });
            if labels {
                self.text(
                    [rect.min[0] + dot * 2.0 + 4.0 * s, mid],
                    Anchor::LEFT_CENTER,
                    font,
                    kind.label(),
                    p.text_weak,
                );
            }
            let mark_h = (row_h * 0.7).max(2.0);
            for (t, _, down) in m.hits.iter().filter(|(_, k, _)| k == kind) {
                let age = (self.now - t).max(0.0) / STRIP_SECONDS;
                let x = track.max[0] - age as f32 * track.width();
                let a = (1.0 - age as f32 * 0.6).clamp(0.2, 1.0);
                self.line(
                    [x, mid - mark_h * 0.5],
                    [x, mid + mark_h * 0.5],
                    (2.0 * s).max(1.5),
                    fade(if *down { p.warning } else { color }, a),
                );
            }
        }
    }

    /// The spectrum's bands as bars, low to high, coloured as the low, mid and high meters.
    fn spectrum(&mut self, rect: Rect, samples: &[f64], r: &Reading) {
        let s = self.s;
        let p = *self.palette;
        self.rect(rect, 2.0, white_alpha(8));
        let n = samples.len();
        if n == 0 || rect.width() < 4.0 {
            return;
        }
        // `as`: a count of bars, at least one, bounded by MAX_SPECTRUM_BARS.
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
            let color = if hz < r.low_hz {
                p.channel(1)
            } else if hz > r.high_hz {
                p.channel(3)
            } else {
                p.channel(2)
            };
            let x = rect.min[0] + b as f32 * w;
            let h = v as f32 * rect.height();
            let bar = Rect::from_min_max([x, rect.max[1] - h], [x + w - gap, rect.max[1]]);
            self.rect(bar, 0.0, fade(color, 0.85));
        }
    }
}

/// Type of `size`, never smaller than [`MIN_TEXT`].
fn text_size(size: f32) -> f32 {
    size.max(MIN_TEXT)
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
fn confidence_color(c: f64, p: &Palette) -> Color {
    if c < 0.35 {
        fade(p.error, 0.8)
    } else if c < 0.65 {
        fade(p.warning, 0.8)
    } else {
        p.channel(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::{Channel, Name};

    fn out(values: &[(&str, f64)]) -> Chans {
        let channels =
            values.iter().map(|(n, v)| Channel { name: Name::new(n).unwrap(), samples: vec![*v] }).collect();
        Chans::new(60.0, channels).unwrap()
    }

    struct Rough;

    impl Measure for Rough {
        fn text_size(&self, text: &str, size: f32) -> [f32; 2] {
            [text.chars().count() as f32 * size * 0.55, size * 1.2]
        }
    }

    #[test]
    fn hits_count_once_per_cook_and_peaks_hold_then_fall() {
        let mut m = Meters::default();
        let a = out(&[("level", 0.9), ("kick", 1.0), ("tick", 1.0), ("bpm", 120.0), ("beat", 0.0)]);
        m.update(Some(&a), true, 10.0);
        // A repaint of the same cook adds nothing.
        m.update(Some(&a), false, 10.01);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Kick).count(), 1);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Beat).count(), 1);
        let quiet = out(&[("level", 0.1), ("bpm", 120.0), ("beat", 0.5)]);
        m.update(Some(&quiet), true, 10.5);
        assert_eq!(falling(m.peaks[0], 10.5), 0.9, "held");
        assert!(falling(m.peaks[0], 11.5) < 0.9, "falling after the hold");
        // A missed tick shows through the phase starting over.
        let next = out(&[("level", 0.1), ("bpm", 120.0), ("beat", 0.02)]);
        m.update(Some(&next), true, 10.6);
        assert_eq!(m.hits.iter().filter(|(_, k, _)| *k == Hit::Beat).count(), 2);
        // Old hits leave the strip.
        m.update(Some(&next), true, 20.0);
        assert!(m.hits.is_empty());
        // The clock going back starts over.
        m.update(Some(&a), true, 21.0);
        assert_eq!(m.hit_count(), 2);
        m.update(Some(&quiet), true, 1.0);
        assert_eq!(m.hit_count(), 0);
        assert!(m.highest_peak(1.0) <= 0.1 + 1e-9);
    }

    #[test]
    fn missing_channels_read_zero_and_the_bar_comes_from_the_analysis() {
        let mut m = Meters::default();
        let r = m.update(None, true, 0.0);
        assert_eq!(r, Reading::default());
        let odd = out(&[
            ("level", f64::NAN),
            ("bar_beat", -3.0),
            ("beats_per_bar", 1e9),
            ("low_hz", f64::INFINITY),
        ]);
        let r = m.update(Some(&odd), true, 0.1);
        assert_eq!(r.meters[0], 0.0);
        assert_eq!(r.bar_beat, 0);
        assert_eq!(r.beats_per_bar, 64);
        assert_eq!(r.low_hz, DEFAULT_LOW_HZ);
        let three = out(&[("beats_per_bar", 3.0), ("bpm", 90.0)]);
        let r = m.update(Some(&three), true, 0.2);
        assert_eq!(r.beats_per_bar, 3);
        let shapes = m.shapes(
            Rect::from_min_size([0.0, 0.0], [640.0, 360.0]),
            &r,
            Some(&[0.5; 32]),
            true,
            &Palette::STANDARD,
            &Rough,
            0.2,
        );
        let cells = shapes.iter().filter(|s| matches!(s, Shape::Text { text, .. } if text == "3")).count();
        assert_eq!(cells, 1, "a cell numbered 3");
        assert!(!shapes.iter().any(|s| matches!(s, Shape::Text { text, .. } if text == "4")));
    }

    #[test]
    fn small_rooms_leave_things_out_and_nothing_is_not_finite() {
        let m = Meters::default();
        let r = Reading::default();
        let spectrum = [0.3; 16];
        let wide = m.shapes(
            Rect::from_min_size([0.0, 0.0], [640.0, 360.0]),
            &r,
            Some(&spectrum),
            false,
            &Palette::STANDARD,
            &Rough,
            0.0,
        );
        let compact = m.shapes(
            Rect::from_min_size([0.0, 0.0], [640.0, 120.0]),
            &r,
            Some(&spectrum),
            false,
            &Palette::STANDARD,
            &Rough,
            0.0,
        );
        let labels = |v: &[Shape]| {
            v.iter().filter(|s| matches!(s, Shape::Text { text, .. } if text == "kick")).count()
        };
        assert_eq!(labels(&wide), 1);
        assert_eq!(labels(&compact), 0, "no row names when compact");
        assert!(wide.len() > compact.len(), "the spectrum goes first");
        assert!(wide.iter().any(|s| matches!(s, Shape::Text { text, .. } if text == "no analysis yet")));
        let tiny = m.shapes(
            Rect::from_min_size([0.0, 0.0], [30.0, 30.0]),
            &r,
            None,
            true,
            &Palette::STANDARD,
            &Rough,
            0.0,
        );
        assert!(tiny.is_empty());
    }
}
