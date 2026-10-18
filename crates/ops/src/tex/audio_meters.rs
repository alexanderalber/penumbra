//! `tex.audio_meters`: the viewer's meters of a `chan.audio_analysis` as a picture in the
//! network, to project or composite. The look comes from [`crate::meters`], the same description
//! the viewer paints with egui; here it is drawn by [`Canvas`] on the GPU.
//!
//! Text is set from glyphs cached in the canvas' atlas: each character is measured and
//! rasterised once per size, off the cook thread, so a tempo or a percentage changing costs
//! nothing but quads. Until a glyph is ready its character is left out (a frame or two after
//! the node starts or the size changes).
//!
//! The picture is laid out as the viewer would draw it at about 640 x 360 points and enlarged
//! to the output's size, so type and lines keep their proportions at any resolution.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use effectcraft_text::kurbo::{Affine, BezPath, Shape as _};
use effectcraft_text::{ParagraphStyle, TextStyle};
use penumbra_core::{Chans, Error, ParamSpec};
use penumbra_gpu::canvas::ATLAS;
use penumbra_gpu::{Canvas, Paint, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Job, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::{gpu, side};
use crate::meters::{Anchor, Color, Measure, Meters, Palette, Rect, Shape};

/// The node kind.
pub const AUDIO_METERS: &str = "tex.audio_meters";

/// The layout's room at scale 1; larger pictures are this enlarged.
const LAYOUT: [f32; 2] = [640.0, 360.0];
/// The font of every text.
const FONT: &str = "Inter";
const STYLE: &str = "Regular";
/// Size characters are measured at, in pixels (outlines are kept per em).
const MEASURE_SIZE: f32 = 64.0;
/// Largest glyph rasterised, in pixels; larger type is drawn enlarged from this.
const MAX_GLYPH: u32 = 384;
/// Most characters measured or glyphs rasterised by one job.
const MAX_BATCH: usize = 256;
/// Most distinct characters kept (the meters use a few dozen).
const MAX_CHARS: usize = 512;
/// The analysis channels of which `in` must have at least one.
const SHAPE: [&str; 6] = ["level", "low", "high", "beat", "bpm", "confidence"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            AUDIO_METERS,
            "The meters of a chan.audio_analysis as a picture: levels, tempo, beat, bar, hits, spectrum",
        )
        .time_dependent()
        .stateful()
        .param(super::width_param())
        .param(super::height_param())
        .param(super::keep_size())
        .param(ParamSpec::color("background", [0.0, 0.0, 0.0, 0.0]).doc(
            "Colour behind the meters, linear RGBA with straight alpha; the default (alpha 0) leaves only the meters, to lay over a picture",
        ))
        .input("in", DataKind::Chan)
        .input("spectrum", DataKind::Chan)
        .describe_input("in", "The out output of a chan.audio_analysis")
        .describe_input("spectrum", "Its spectrum output; unconnected, the spectrum is left out")
        .output("out", DataKind::Tex)
        .describe_output(
            "out",
            "The meters as in the viewer: level and band bars with peaks, tempo, beat light, bar, hits, spectrum",
        ),
        || Box::new(AudioMeters::default()) as Box<dyn Operator>,
    ))
}

/// Compiles the canvas pipelines ahead of the first node.
pub fn warm_up(gpu: &penumbra_gpu::Gpu) -> Result<(), Error> {
    gpu.canvas_pipelines().map(|_| ())
}

/// A character's measure: its advance and outline per em, the pen at the baseline.
struct CharMetrics {
    advance: f32,
    outline: Arc<BezPath>,
}

/// The font's line box per em and the characters measured so far.
#[derive(Default)]
struct Font {
    ascent: f32,
    descent: f32,
    chars: HashMap<char, CharMetrics>,
}

impl Measure for Font {
    fn text_size(&self, text: &str, size: f32) -> [f32; 2] {
        // Unmeasured characters as an average one, so the first frames lay out about right.
        let width: f32 = text.chars().map(|c| self.chars.get(&c).map_or(0.55, |m| m.advance)).sum();
        let line = if self.ascent + self.descent > 0.0 { self.ascent + self.descent } else { 1.2 };
        [width * size, line * size]
    }
}

/// A glyph in the atlas: its texels and where they sit from the pen (pixels at its raster
/// size, y down from the baseline).
#[derive(Clone, Copy)]
struct Glyph {
    at: [u32; 2],
    size: [u32; 2],
    offset: [f32; 2],
}

/// A rasterised glyph on its way to the atlas.
struct Bitmap {
    key: (char, u32),
    width: u32,
    height: u32,
    offset: [f32; 2],
    coverage: Vec<u8>,
}

/// What one job brings: measured characters (and the line box), rasterised glyphs.
struct Batch {
    line: Option<(f32, f32)>,
    measured: Vec<(char, CharMetrics)>,
    bitmaps: Vec<Bitmap>,
}

/// Rows of glyphs filling the atlas from the top.
#[derive(Default)]
struct Shelf {
    x: u32,
    y: u32,
    row: u32,
}

impl Shelf {
    /// A place for a `w` x `h` glyph with a texel between neighbours, if the atlas has room.
    fn place(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        if w + 1 > ATLAS || h + 1 > ATLAS {
            return None;
        }
        if self.x + w + 1 > ATLAS {
            self.y += self.row;
            self.x = 0;
            self.row = 0;
        }
        if self.y + h + 1 > ATLAS {
            return None;
        }
        let at = [self.x, self.y];
        self.x += w + 1;
        self.row = self.row.max(h + 1);
        Some(at)
    }
}

#[derive(Default)]
struct AudioMeters {
    meters: Meters,
    canvas: Canvas,
    ring: TexRing,
    font: Font,
    glyphs: HashMap<(char, u32), Glyph>,
    shelf: Shelf,
    /// The glyph job in flight, if any.
    job: Option<Job<Batch>>,
}

impl Operator for AudioMeters {
    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        if cx.def().inputs.first().is_none_or(Option::is_none) {
            return Err(CookError::failed("connect the out output of a chan.audio_analysis to `in`"));
        }
        let Some(out) = cx.input_as::<Chans>(0) else { return Err(CookError::Pending) };
        if !out.is_empty() && !SHAPE.iter().any(|n| out.get(n).is_some()) {
            return Err(CookError::failed(format!(
                "`in` has none of an analysis' channels ({}): connect the out output of a chan.audio_analysis",
                SHAPE.join(", ")
            )));
        }
        let spectrum =
            cx.input_as::<Chans>(1).and_then(|c| c.channels().first()).map(|c| c.samples.as_slice());
        if let Some(done) = self.job.as_ref().and_then(Job::take) {
            self.job = None;
            match done {
                Ok(batch) => self.take(gpu, batch)?,
                Err(e) => cx.warn(format!("cannot set the meters' text: {e}")),
            }
        }
        let (w, h) = (side(cx, "width")?, side(cx, "height")?);
        let now = cx.time().seconds();
        let fresh = cx.inputs_changed() || cx.first_cook();
        let reading = self.meters.update(Some(out), fresh, now);
        // As the viewer at about 640 x 360 points, enlarged.
        let k = (w as f32 / LAYOUT[0]).min(h as f32 / LAYOUT[1]).max(1.0);
        let room = Rect::from_min_size([0.0, 0.0], [w as f32 / k, h as f32 / k]);
        let shapes = self.meters.shapes(
            room,
            &reading,
            spectrum,
            !out.is_empty(),
            &Palette::STANDARD,
            &self.font,
            now,
        );
        let mut paints = Vec::with_capacity(shapes.len() + 64);
        let mut wanted = HashSet::new();
        for shape in shapes {
            self.paint(shape.scaled(k), &mut paints, &mut wanted);
        }
        if !wanted.is_empty() && self.job.is_none() {
            self.request(cx, wanted);
        }
        let background = match cx.param("background").and_then(penumbra_core::Value::components) {
            // `as`: colour components to f32.
            Some([r, g, b, a]) => [*r as f32, *g as f32, *b as f32, *a as f32],
            _ => [0.0; 4],
        };
        let target = self.ring.next(gpu, w, h, AUDIO_METERS)?;
        self.canvas.render(gpu, &target, background, &paints)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }
}

impl AudioMeters {
    /// Adds the paints of one shape (in pixels); characters without a glyph yet go to `wanted`.
    fn paint(&self, shape: Shape, paints: &mut Vec<Paint>, wanted: &mut HashSet<(char, u32)>) {
        let rgba = |c: Color| c.map(|v| f32::from(v) / 255.0);
        match shape {
            Shape::Rect { rect, radius, color } => {
                paints.push(Paint::Rect { min: rect.min, max: rect.max, radius, color: rgba(color) });
            }
            Shape::Frame { rect, radius, width, color } => {
                paints.push(Paint::Frame { min: rect.min, max: rect.max, radius, width, color: rgba(color) });
            }
            Shape::Circle { center, radius, color } => paints.push(Paint::Rect {
                min: [center[0] - radius, center[1] - radius],
                max: [center[0] + radius, center[1] + radius],
                radius,
                color: rgba(color),
            }),
            Shape::Arc { center, radius, width, fraction, color } => {
                paints.push(Paint::Arc { center, radius, width, fraction, color: rgba(color) });
            }
            Shape::Line { from, to, width, color } => {
                paints.push(Paint::Line { from, to, width, color: rgba(color) })
            }
            Shape::Text { at, anchor, size, text, color } => {
                self.text(at, anchor, size, &text, rgba(color), paints, wanted);
            }
        }
    }

    /// One line of text from cached glyphs.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        at: [f32; 2],
        anchor: Anchor,
        size: f32,
        text: &str,
        color: [f32; 4],
        paints: &mut Vec<Paint>,
        wanted: &mut HashSet<(char, u32)>,
    ) {
        if !(size.is_finite() && size > 0.5) {
            return;
        }
        let [bw, bh] = self.font.text_size(text, size);
        let left = at[0] - anchor.0[0] * bw;
        let top = at[1] - anchor.0[1] * bh;
        let baseline = top + self.font.ascent * size;
        let raster = raster_size(size);
        let k = size / raster as f32;
        let mut pen = left;
        for c in text.chars() {
            let Some(m) = self.font.chars.get(&c) else {
                wanted.insert((c, raster));
                pen += 0.55 * size;
                continue;
            };
            match self.glyphs.get(&(c, raster)) {
                Some(g) if g.size[0] > 0 && g.size[1] > 0 => {
                    let min = [pen + g.offset[0] * k, baseline + g.offset[1] * k];
                    let max = [min[0] + g.size[0] as f32 * k, min[1] + g.size[1] as f32 * k];
                    let uv_min = [g.at[0] as f32, g.at[1] as f32];
                    let uv_max = [uv_min[0] + g.size[0] as f32, uv_min[1] + g.size[1] as f32];
                    paints.push(Paint::Glyph { min, max, uv_min, uv_max, color });
                }
                Some(_) => {}
                None => {
                    wanted.insert((c, raster));
                }
            }
            pen += m.advance * size;
        }
    }

    /// Starts a job measuring and rasterising what `wanted` lists.
    fn request(&mut self, cx: &CookContext<'_>, wanted: HashSet<(char, u32)>) {
        let mut keys: Vec<(char, u32)> = wanted.into_iter().collect();
        keys.sort_unstable();
        keys.truncate(MAX_BATCH);
        let known: HashMap<char, Arc<BezPath>> =
            self.font.chars.iter().map(|(c, m)| (*c, m.outline.clone())).collect();
        let measure_line = self.font.ascent + self.font.descent <= 0.0;
        let room = MAX_CHARS.saturating_sub(self.font.chars.len());
        self.job = Some(cx.spawn(move |_job| Ok(rasterise(&keys, &known, measure_line, room))));
    }

    /// Takes in a finished batch: measured characters, glyphs into the atlas. A full atlas
    /// starts over empty; what is missing then is asked for again.
    fn take(&mut self, gpu: &penumbra_gpu::Gpu, batch: Batch) -> Result<(), CookError> {
        if let Some((ascent, descent)) = batch.line {
            self.font.ascent = ascent;
            self.font.descent = descent;
        }
        for (c, m) in batch.measured {
            if self.font.chars.len() < MAX_CHARS {
                self.font.chars.insert(c, m);
            }
        }
        for b in batch.bitmaps {
            if b.width == 0 || b.height == 0 {
                self.glyphs.insert(b.key, Glyph { at: [0, 0], size: [0, 0], offset: b.offset });
                continue;
            }
            let at = match self.shelf.place(b.width, b.height) {
                Some(at) => at,
                None => {
                    self.glyphs.clear();
                    self.shelf = Shelf::default();
                    let Some(at) = self.shelf.place(b.width, b.height) else { continue };
                    at
                }
            };
            self.canvas.put(gpu, at[0], at[1], b.width, b.height, &b.coverage)?;
            self.glyphs.insert(b.key, Glyph { at, size: [b.width, b.height], offset: b.offset });
        }
        Ok(())
    }
}

/// The size a glyph for type of `size` pixels is rasterised at: whole pixels up to 32, then
/// steps of an eighth of an octave (drawn at most 5 % off), at most [`MAX_GLYPH`].
fn raster_size(size: f32) -> u32 {
    let size = size.clamp(1.0, MAX_GLYPH as f32);
    if size <= 32.0 {
        // `as`: 1 to 32.
        return size.round() as u32;
    }
    let steps = (size.log2() * 8.0).round();
    // `as`: 33 to MAX_GLYPH.
    (2f32.powf(steps / 8.0).round() as u32).clamp(1, MAX_GLYPH)
}

/// Measures the characters of `keys` not in `known` (at most `room` new ones) and rasterises
/// every key; runs off the cook thread.
fn rasterise(
    keys: &[(char, u32)],
    known: &HashMap<char, Arc<BezPath>>,
    measure_line: bool,
    room: usize,
) -> Batch {
    let style = TextStyle {
        family: FONT.to_string(),
        style: STYLE.to_string(),
        size: MEASURE_SIZE,
        ..TextStyle::default()
    };
    let para = ParagraphStyle::default();
    let mut batch = Batch { line: None, measured: Vec::new(), bitmaps: Vec::new() };
    if measure_line {
        let layout = effectcraft_text::layout::layout_uncached("Hg", &style, &para);
        if let Some(l) = layout.lines.first() {
            batch.line = Some((l.ascent / MEASURE_SIZE, l.descent / MEASURE_SIZE));
        }
    }
    let mut outlines: HashMap<char, Arc<BezPath>> = HashMap::new();
    for &(c, _) in keys {
        if known.contains_key(&c) || outlines.contains_key(&c) || outlines.len() >= room {
            continue;
        }
        let Some(m) = measure(c, &style, &para) else { continue };
        outlines.insert(c, m.outline.clone());
        batch.measured.push((c, m));
    }
    for &(c, size) in keys {
        let Some(outline) = known.get(&c).or_else(|| outlines.get(&c)) else { continue };
        batch.bitmaps.push(glyph(c, size, outline));
    }
    batch
}

/// One character laid out alone: its advance and outline per em, from the pen at the baseline.
fn measure(c: char, style: &TextStyle, para: &ParagraphStyle) -> Option<CharMetrics> {
    let layout = effectcraft_text::layout::layout_uncached(&c.to_string(), style, para);
    let line = layout.lines.first()?;
    let mut path = BezPath::new();
    for g in &layout.glyphs {
        let outline = Affine::translate((f64::from(g.x - line.x), f64::from(g.y - line.baseline)))
            * effectcraft_text::glyph_outline(g);
        path.extend(outline.elements().iter().copied());
    }
    let per_em = Affine::scale(1.0 / f64::from(MEASURE_SIZE));
    // A line's width leaves out trailing white space: a space is measured between two bars.
    let width = if c.is_whitespace() {
        let width = |text: &str| {
            effectcraft_text::layout::layout_uncached(text, style, para)
                .lines
                .first()
                .map_or(0.0, |l| l.width)
        };
        width(&format!("|{c}|")) - width("||")
    } else {
        line.width
    };
    let advance = width.max(0.0) / MEASURE_SIZE;
    advance.is_finite().then(|| CharMetrics { advance, outline: Arc::new(per_em * path) })
}

/// The coverage of `outline` (per em) at `size` pixels, with a texel of room around it.
fn glyph(c: char, size: u32, outline: &BezPath) -> Bitmap {
    let empty = Bitmap { key: (c, size), width: 0, height: 0, offset: [0.0; 2], coverage: Vec::new() };
    if outline.elements().is_empty() {
        return empty;
    }
    let scaled = Affine::scale(f64::from(size)) * outline.clone();
    let b = scaled.bounding_box();
    let (x0, y0) = (b.x0.floor() - 1.0, b.y0.floor() - 1.0);
    let (x1, y1) = (b.x1.ceil() + 1.0, b.y1.ceil() + 1.0);
    let limit = f64::from(MAX_GLYPH) * 4.0;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite())
        || x1 - x0 > limit
        || y1 - y0 > limit
    {
        return empty;
    }
    // `as`: bounded just above.
    let (w, h) = ((x1 - x0).max(1.0) as u32, (y1 - y0).max(1.0) as u32);
    let Some(mut mask) = tiny_skia::Mask::new(w, h) else { return empty };
    if let Some(p) = super::text::to_skia(&scaled) {
        mask.fill_path(
            &p,
            tiny_skia::FillRule::Winding,
            true,
            tiny_skia::Transform::from_translate(-x0 as f32, -y0 as f32),
        );
    }
    Bitmap {
        key: (c, size),
        width: w,
        height: h,
        offset: [x0 as f32, y0 as f32],
        coverage: mask.data().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_sizes_are_whole_pixels_then_steps() {
        assert_eq!(raster_size(9.4), 9);
        assert_eq!(raster_size(0.0), 1);
        assert_eq!(raster_size(f32::NAN.max(1.0)), 1);
        let big = raster_size(300.0);
        assert!((f64::from(big) / 300.0 - 1.0).abs() < 0.05, "{big}");
        assert_eq!(raster_size(1e9), MAX_GLYPH);
    }

    #[test]
    fn the_shelf_fills_rows_and_says_when_full() {
        let mut s = Shelf::default();
        assert_eq!(s.place(10, 20), Some([0, 0]));
        assert_eq!(s.place(10, 5), Some([11, 0]));
        assert_eq!(s.place(ATLAS - 5, 5), Some([0, 21]));
        assert_eq!(s.place(ATLAS, 1), None);
        let mut n = 0;
        while s.place(300, 300).is_some() {
            n += 1;
        }
        assert!(n > 10 && n < 100, "{n}");
    }

    #[test]
    fn characters_measure_and_rasterise() {
        let style = TextStyle {
            family: FONT.into(),
            style: STYLE.into(),
            size: MEASURE_SIZE,
            ..TextStyle::default()
        };
        let m = measure('8', &style, &ParagraphStyle::default()).unwrap();
        assert!(m.advance > 0.3 && m.advance < 1.0, "{}", m.advance);
        let g = glyph('8', 40, &m.outline);
        assert!(g.width > 10 && g.height > 20, "{} x {}", g.width, g.height);
        assert!(g.offset[1] < -20.0, "above the baseline: {:?}", g.offset);
        assert!(g.coverage.contains(&255));
        let space = measure(' ', &style, &ParagraphStyle::default()).unwrap();
        assert_eq!(glyph(' ', 40, &space.outline).width, 0);
        assert!(space.advance > 0.15 && space.advance < 0.5, "a space has width: {}", space.advance);
    }
}
