//! `geo.text`: text as solid lettering. Shaping and fonts are the text node's (`effectcraft-text`);
//! the outlines go through [`penumbra_mesh::extrude`], which makes the closed mesh and says which
//! of its edges a wireframe draws. Built in a job, off the cook thread: a font scan or a hostile
//! glyph may take a while, and a panic in a font parser ends only the job.

use std::sync::Arc;

use effectcraft_text::kurbo::{Affine, BezPath, PathEl, Point};
use effectcraft_text::{Align, Glyph, ParagraphStyle, TextStyle};
use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::geo::Model;
use penumbra_gpu::{Geo, Gpu};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator,
};
use penumbra_mesh::extrude::{self, Place, Shape, Solid};
use penumbra_mesh::{Limits, Mesh};

use crate::tex::DEFAULT_FONT;

/// The node kind.
pub const GEO_TEXT: &str = "geo.text";

/// Longest text used; the rest is cut off with a warning. As `tex.text`'s limit, for the same
/// reason: layout time grows with the square of a line's length.
pub const MAX_CHARS: usize = 4096;
/// Triangles of the body; glyphs past this are left out with a warning. A line of 500 characters
/// takes about 100 000.
pub const MAX_TRIANGLES: usize = 1 << 20;
/// Largest `size` and `depth`, in metres.
pub const MAX_SIZE: f64 = 1000.0;
/// Smallest `size`, in metres.
pub const MIN_SIZE: f64 = 0.001;
/// Letter spacing allowed, in ems.
pub const SPACING: (f64, f64) = (-1.0, 10.0);

/// Laid out at this size in pixels, whatever `size` is. So `size` scales the lettering without
/// changing its shape.
const LAYOUT_EM: f64 = 1024.0;
/// Grid units per pixel of the layout: 65536 to the em.
const GRID: f64 = 64.0;
/// How far a stepped curve may stray from the font's curve, in pixels of the layout: a thousandth
/// of the em. On a letter 500 pixels high that is half a pixel.
const CURVE_ERROR: f64 = 1.024;
/// Largest turn per step of a curve, in degrees. It has to stay under the wall's crease, or the
/// steps of a tight curve would show as edges.
const CURVE_TURN: f64 = 15.0;
/// Most steps of one curve.
const MAX_CURVE_STEPS: usize = 256;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(GEO_TEXT, "Text as solid 3D letters: front, back and sides, with the holes in letters open. Upright, facing -y, centred on the origin")
            .param(
                ParamSpec::text("text", "Text")
                    .multiline()
                    .doc("What is written. Line breaks are kept. Longer than 4096 characters: the rest is left out, with a warning"),
            )
            .param(
                ParamSpec::text("font", DEFAULT_FONT)
                    .doc("Family name; Inter, JetBrains Mono, Noto Serif are bundled"),
            )
            .param(
                ParamSpec::text("style", "Regular")
                    .doc("Style within the family: Regular, Bold, Italic, Light, ... The menu lists the family's styles"),
            )
            .param(
                ParamSpec::float("size", 1.0)
                    .range(0.05, 20.0)
                    .curve(Curve::Log)
                    .doc("Font size in metres (the font's em, a little more than a capital's height). Kept between 0.001 and 1000, with a warning"),
            )
            .param(
                ParamSpec::float("depth", 0.2)
                    .range(0.0, 5.0)
                    .doc("Thickness along y in metres, centred on y = 0. 0 draws a flat front only. Kept between 0 and 1000, with a warning"),
            )
            .param(
                ParamSpec::float("spacing", 0.0)
                    .range(-0.2, 1.0)
                    .doc("Letter spacing in ems: 0.1 adds a tenth of the font size after each letter. Kept between -1 and 10, with a warning"),
            )
            .param(
                ParamSpec::choice("align", &["center", "left", "right"])
                    .doc("How the lines line up with each other, and which point of the block sits at x = 0. The block's middle sits at z = 0")
                    .choice_doc("center", "centred; the block's centre at x = 0")
                    .choice_doc("left", "flush left; the block's left edge at x = 0")
                    .choice_doc("right", "flush right; the block's right edge at x = 0"),
            )
            .output("out", DataKind::Geo)
            .output("front", DataKind::Geo)
            .describe_output("out", "The closed body. With mat.wireframe and `edges` on polygons, lines show the outlines of front and back and the corners of the sides, without the triangles or the steps of curves")
            .describe_output("front", "The front face alone, at the same place: for a second material on it, a picture or a glow. Texture coordinates span the block of text"),
        || Box::new(GeoText::default()) as Box<dyn Operator>,
    ))
}

/// What a body depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub text: String,
    pub font: String,
    pub style: String,
    pub size: f64,
    pub depth: f64,
    pub spacing: f64,
    pub align: String,
}

/// The lettering on the CPU, and what the node warns about.
#[derive(Clone, Debug, Default)]
pub struct Lettering {
    pub solid: Solid,
    pub warnings: Vec<String>,
}

struct Built {
    body: Geo,
    front: Geo,
    warnings: Vec<String>,
}

/// One glyph's contours on the grid and their box.
struct GlyphShape {
    contours: Vec<Vec<[f64; 2]>>,
    bounds: [f64; 4],
}

/// `path` as closed contours on the grid, z up, curves stepped.
fn contours(path: &BezPath) -> Vec<Vec<[f64; 2]>> {
    let grid = |p: Point| [p.x * GRID, -p.y * GRID];
    let mut out = Vec::new();
    let mut now: Vec<[f64; 2]> = Vec::new();
    let mut last = Point::ZERO;
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                if !now.is_empty() {
                    out.push(std::mem::take(&mut now));
                }
                now.push(grid(p));
                last = p;
            }
            PathEl::LineTo(p) => {
                now.push(grid(p));
                last = p;
            }
            PathEl::QuadTo(a, p) => {
                let n = steps(&[last, a, p]);
                now.extend((1..=n).map(|k| grid(quad(last, a, p, k as f64 / n as f64))));
                last = p;
            }
            PathEl::CurveTo(a, b, p) => {
                let n = steps(&[last, a, b, p]);
                now.extend((1..=n).map(|k| grid(cubic(last, a, b, p, k as f64 / n as f64))));
                last = p;
            }
            PathEl::ClosePath => {
                if !now.is_empty() {
                    out.push(std::mem::take(&mut now));
                }
            }
        }
    }
    if !now.is_empty() {
        out.push(now);
    }
    out
}

fn mix(a: Point, b: Point, t: f64) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn quad(a: Point, b: Point, c: Point, t: f64) -> Point {
    mix(mix(a, b, t), mix(b, c, t), t)
}

fn cubic(a: Point, b: Point, c: Point, d: Point, t: f64) -> Point {
    let (ab, bc, cd) = (mix(a, b, t), mix(b, c, t), mix(c, d, t));
    mix(mix(ab, bc, t), mix(bc, cd, t), t)
}

/// Steps for a curve given by its control points: enough for [`CURVE_ERROR`] (Wang's formula) and
/// for [`CURVE_TURN`] (the turning of the control polygon bounds the curve's).
fn steps(points: &[Point]) -> usize {
    let degree = points.len().saturating_sub(1) as f64;
    let mut bend = 0.0f64;
    let mut turn = 0.0f64;
    for w in points.windows(3) {
        let &[a, b, c] = w else { continue };
        bend = bend.max((a.to_vec2() - 2.0 * b.to_vec2() + c.to_vec2()).hypot());
        let (u, v) = (b - a, c - b);
        turn += v.cross(u).atan2(u.dot(v)).abs();
    }
    let by_error = (degree * (degree - 1.0) / 8.0 * bend / CURVE_ERROR).sqrt();
    let by_turn = turn.to_degrees() / CURVE_TURN;
    let n = by_error.max(by_turn).ceil();
    // Not a number from a broken outline stays a single step, and the contour is judged later.
    if n.is_finite() { (n as usize).clamp(1, MAX_CURVE_STEPS) } else { 1 }
}

fn bounds_of(contours: &[Vec<[f64; 2]>]) -> Option<[f64; 4]> {
    let mut points = contours.iter().flatten().filter(|p| p[0].is_finite() && p[1].is_finite());
    let first = points.next()?;
    Some(points.fold([first[0], first[1], first[0], first[1]], |b, p| {
        [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]
    }))
}

/// The group `k` belongs to, shortening the way up.
fn root(group: &mut [usize], mut k: usize) -> usize {
    while let Some(&up) = group.get(k) {
        if up == k {
            break;
        }
        let over = group.get(up).copied().unwrap_or(up);
        if let Some(slot) = group.get_mut(k) {
            *slot = over;
        }
        k = up;
    }
    k
}

/// Groups of glyphs whose boxes overlap: their outlines have to be joined into one shape, as a
/// letter with its accent or letters pushed into each other by negative spacing.
fn overlapping(glyphs: Vec<GlyphShape>) -> Vec<Vec<Vec<[f64; 2]>>> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    let min_x = |k: &usize| glyphs.get(*k).map_or(0.0, |g| g.bounds[0]);
    order.sort_by(|a, b| min_x(a).total_cmp(&min_x(b)));
    let mut group: Vec<usize> = (0..glyphs.len()).collect();
    for (i, &a) in order.iter().enumerate() {
        let Some(ga) = glyphs.get(a) else { continue };
        for &b in order.iter().skip(i + 1) {
            let Some(gb) = glyphs.get(b) else { continue };
            if gb.bounds[0] > ga.bounds[2] {
                break;
            }
            if gb.bounds[1] <= ga.bounds[3] && gb.bounds[3] >= ga.bounds[1] {
                let (ra, rb) = (root(&mut group, a), root(&mut group, b));
                if let Some(slot) = group.get_mut(ra) {
                    *slot = rb;
                }
            }
        }
    }
    let mut groups: Vec<Vec<Vec<[f64; 2]>>> = vec![Vec::new(); glyphs.len()];
    for (k, g) in glyphs.into_iter().enumerate() {
        let r = root(&mut group, k);
        if let Some(list) = groups.get_mut(r) {
            list.extend(g.contours);
        }
    }
    groups.retain(|g| !g.is_empty());
    groups
}

/// The value `v` kept in `min..=max`, and a warning about `name` if that changed it.
fn kept(v: f64, min: f64, max: f64, fallback: f64, name: &str, warnings: &mut Vec<String>) -> f64 {
    let out = if v.is_finite() { v.clamp(min, max) } else { fallback };
    if out != v {
        warnings.push(format!("`{name}` is {v}; using {out} (it is kept between {min} and {max})"));
    }
    out
}

/// Lays out `request` and makes its meshes. Nothing here fails on what the text or the font holds:
/// parts that cannot be used are left out, with a warning.
pub fn lettering(request: &Request) -> Result<Lettering, Error> {
    let mut warnings = Vec::new();
    let size = kept(request.size, MIN_SIZE, MAX_SIZE, 1.0, "size", &mut warnings);
    let depth = kept(request.depth, 0.0, MAX_SIZE, 0.0, "depth", &mut warnings);
    let spacing = kept(request.spacing, SPACING.0, SPACING.1, 0.0, "spacing", &mut warnings);
    let text: String = request.text.chars().take(MAX_CHARS).collect();
    if text.len() < request.text.len() {
        warnings.push(format!("the text is longer than {MAX_CHARS} characters; the rest is left out"));
    }
    let resolved = effectcraft_text::resolve(&request.font, &request.style);
    if resolved.missing {
        warnings.push(format!("font `{}` not found; using {DEFAULT_FONT}", request.font));
    }
    let style = TextStyle {
        family: request.font.clone(),
        style: request.style.clone(),
        size: LAYOUT_EM as f32,
        tracking: (spacing * 1000.0) as f32,
        ..TextStyle::default()
    };
    let align = match request.align.as_str() {
        "left" => Align::Left,
        "right" => Align::Right,
        _ => Align::Center,
    };
    let para = ParagraphStyle { align, ..ParagraphStyle::default() };
    let layout = effectcraft_text::layout::layout_uncached(&text, &style, &para);

    let top = layout.lines.first().map_or(0.0, |l| f64::from(l.baseline - l.ascent));
    let bottom = layout.lines.last().map_or(0.0, |l| f64::from(l.baseline + l.descent));
    let left = layout.lines.iter().map(|l| f64::from(l.x)).fold(f64::INFINITY, f64::min);
    let right = layout.lines.iter().map(|l| f64::from(l.x + l.width)).fold(f64::NEG_INFINITY, f64::max);
    let (left, right) = if left <= right { (left, right) } else { (0.0, 0.0) };
    let anchor_x = match align {
        Align::Left => left,
        Align::Right => right,
        _ => (left + right) / 2.0,
    };

    let missing = layout.glyphs.iter().filter(|g| is_missing(g, &text)).count();
    if missing > 0 {
        warnings.push(format!(
            "{missing} characters have no glyph in the font; they show as the font's empty box or not at all"
        ));
    }

    let mut glyphs = Vec::new();
    for g in &layout.glyphs {
        let outline =
            Affine::translate((f64::from(g.x), f64::from(g.y))) * effectcraft_text::glyph_outline(g);
        let contours = contours(&outline);
        if let Some(bounds) = bounds_of(&contours) {
            glyphs.push(GlyphShape { contours, bounds });
        }
    }
    let mut shapes: Vec<Shape> = Vec::new();
    let (mut triangles, mut too_many, mut too_complex, mut unusable) = (0usize, false, 0usize, 0usize);
    for group in overlapping(glyphs) {
        match extrude::shape(&group) {
            Ok(s) => {
                unusable += s.unusable;
                if triangles + s.triangle_count() > MAX_TRIANGLES {
                    too_many = true;
                    break;
                }
                triangles += s.triangle_count();
                if !s.is_empty() {
                    shapes.push(s);
                }
            }
            Err(e) => {
                too_complex += 1;
                log::debug!("geo.text left out a glyph: {e}");
            }
        }
    }
    if too_many {
        warnings.push(format!("the text needs more than {MAX_TRIANGLES} triangles; the rest is left out"));
    }
    if too_complex > 0 {
        warnings.push(format!("{too_complex} glyphs have outlines too complex to build; they are left out"));
    }
    if unusable > 0 {
        warnings.push(format!("{unusable} outlines of the font are broken; they are left out"));
    }
    let picture = shapes
        .iter()
        .filter_map(Shape::bounds)
        .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
    let Some(picture) = picture else { return Ok(Lettering { solid: Solid::default(), warnings }) };
    let place = Place {
        scale: size / (LAYOUT_EM * GRID),
        origin: [anchor_x * GRID, -(top + bottom) / 2.0 * GRID],
        depth: depth as f32,
        picture,
    };
    let solid = extrude::extrude(&shapes, &place, &Limits::default())?;
    Ok(Lettering { solid, warnings })
}

/// A glyph the fonts have nothing for: glyph 0, on a character that is not white space.
fn is_missing(g: &Glyph, text: &str) -> bool {
    g.id == 0 && text.get(g.cluster..).and_then(|t| t.chars().next()).is_some_and(|c| !c.is_whitespace())
}

fn upload(gpu: &Gpu, mesh: Mesh, label: &str) -> Result<Geo, Error> {
    if mesh.indices.is_empty() {
        return Ok(Geo::default());
    }
    let (model, _) = Model::upload(gpu, mesh, label)?;
    Ok(Geo::single(Arc::new(model)))
}

fn built(gpu: &Gpu, request: &Request) -> Result<Built, Error> {
    let Lettering { solid, warnings } = lettering(request)?;
    let body = upload(gpu, solid.body, "geo.text")?;
    let front = upload(gpu, solid.front, "geo.text front")?;
    Ok(Built { body, front, warnings })
}

#[derive(Default)]
struct GeoText {
    slot: AsyncSlot<Arc<Built>>,
    request: Option<Request>,
    /// The result last handed out, so a cook without news changes nothing.
    shown: Option<Arc<Built>>,
}

impl Operator for GeoText {
    fn gpu_bytes(&self) -> u64 {
        let bytes = |g: &Geo| g.items.iter().map(|i| i.model.bytes()).sum::<u64>();
        self.slot.latest().map_or(0, |b| bytes(&b.body) + bytes(&b.front))
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.slot.poll();
        let gpu = crate::gpu(cx)?.clone();
        let request = Request {
            text: cx.text("text")?.to_string(),
            font: cx.text("font")?.to_string(),
            style: cx.text("style")?.to_string(),
            size: cx.float("size")?,
            depth: cx.float("depth")?,
            spacing: cx.float("spacing")?,
            align: cx.text("align")?.to_string(),
        };
        // One build at a time; a newer request waits for it, and the job's end cooks the node
        // again. While a slider moves, the lettering follows at the speed it can be built.
        if self.request.as_ref() != Some(&request) && !self.slot.busy() {
            self.request = Some(request.clone());
            self.slot.start(cx.spawn(move |_job| built(&gpu, &request).map(Arc::new).map_err(|e| e.0)));
        }
        let Some(b) = self.slot.latest().cloned() else {
            return Err(match self.slot.error() {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = self.slot.error() {
            cx.warn(format!("{e} (showing the last text that worked)"));
        }
        for w in &b.warnings {
            cx.warn(w.clone());
        }
        if self.shown.as_ref().is_some_and(|s| Arc::ptr_eq(s, &b)) {
            return Ok(Cooked::Unchanged);
        }
        self.shown = Some(b.clone());
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Geo, b.body.clone()),
            Data::new(DataKind::Geo, b.front.clone()),
        ]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn ask(text: &str) -> Request {
        Request {
            text: text.to_string(),
            font: DEFAULT_FONT.to_string(),
            style: "Regular".to_string(),
            size: 1.0,
            depth: 0.2,
            spacing: 0.0,
            align: "center".to_string(),
        }
    }

    fn assert_closed(mesh: &Mesh) {
        let mut net: HashMap<([u32; 3], [u32; 3]), i64> = HashMap::new();
        for t in mesh.indices.as_chunks::<3>().0 {
            let p = t.map(|i| mesh.vertices[i as usize].position.map(f32::to_bits));
            for k in 0..3 {
                let (a, b) = (p[k], p[(k + 1) % 3]);
                if a < b {
                    *net.entry((a, b)).or_default() += 1;
                } else if b < a {
                    *net.entry((b, a)).or_default() -= 1;
                }
            }
        }
        assert!(!net.is_empty());
        assert!(net.values().all(|n| *n == 0), "an edge is not shared by two triangles");
    }

    /// Loops of the front's outline: boundary edges followed round. Outer and holes by their
    /// sign, as seen from the front (x right, z up).
    fn outer_and_holes(front: &Mesh) -> (usize, usize) {
        let key = |i: u32| {
            let p = front.vertices[i as usize].position;
            (p[0].to_bits(), p[2].to_bits())
        };
        let mut count: HashMap<_, i64> = HashMap::new();
        for t in front.indices.as_chunks::<3>().0 {
            for k in 0..3 {
                let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                if a != b {
                    *count.entry((a, b)).or_default() += 1;
                    *count.entry((b, a)).or_default() -= 1;
                }
            }
        }
        let mut next: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
        for ((a, b), n) in &count {
            if *n > 0 {
                next.entry(*a).or_default().push(*b);
            }
        }
        let (mut outer, mut holes) = (0, 0);
        while let Some(start) = next.keys().next().copied() {
            let mut at = start;
            let mut area = 0.0f64;
            loop {
                let list = next.get_mut(&at).unwrap();
                let b = list.pop().unwrap();
                if list.is_empty() {
                    next.remove(&at);
                }
                let f = |k: (u32, u32)| (f64::from(f32::from_bits(k.0)), f64::from(f32::from_bits(k.1)));
                let (p, q) = (f(at), f(b));
                area += p.0 * q.1 - q.0 * p.1;
                at = b;
                if at == start {
                    break;
                }
            }
            if area > 0.0 {
                outer += 1;
            } else {
                holes += 1;
            }
        }
        (outer, holes)
    }

    fn built(text: &str) -> Lettering {
        lettering(&ask(text)).unwrap()
    }

    #[test]
    fn letters_with_holes_have_them_and_are_closed() {
        // (text, outer loops, holes), Inter Regular.
        for (text, outer, holes) in
            [("O", 1, 1), ("B", 1, 2), ("8", 1, 2), ("e", 1, 1), ("A", 1, 1), ("S", 1, 0), ("i", 2, 0)]
        {
            let l = built(text);
            assert_closed(&l.solid.body);
            assert_eq!(outer_and_holes(&l.solid.front), (outer, holes), "{text}");
            assert!(l.warnings.is_empty(), "{text}: {:?}", l.warnings);
        }
    }

    #[test]
    fn every_bundled_font_gives_closed_letters() {
        let text = "Quick Sessions - Beta Mix 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ &@%$#?!";
        for (font, style) in [
            ("Inter", "Regular"),
            ("Inter", "Medium"),
            ("Inter", "SemiBold"),
            ("Inter", "Bold"),
            ("Inter", "Italic"),
            ("JetBrains Mono", "Regular"),
            ("Noto Serif", "Regular"),
        ] {
            let mut r = ask(text);
            r.font = font.to_string();
            r.style = style.to_string();
            let l = lettering(&r).unwrap();
            assert_closed(&l.solid.body);
            assert!(l.solid.body.triangle_count() > 1000, "{font} {style}");
            assert!(l.warnings.is_empty(), "{font} {style}: {:?}", l.warnings);
        }
    }

    #[test]
    fn umlauts_and_diacritics_have_their_marks() {
        // Each with its dots or accent as loops of their own; e with an acute both as one
        // character and as two.
        for (text, outer, holes) in
            [("Ä", 3, 1), ("Ö", 3, 1), ("ü", 3, 0), ("é", 2, 1), ("e\u{301}", 2, 1), ("ß", 1, 0), ("ñ", 2, 0)]
        {
            let l = built(text);
            assert_closed(&l.solid.body);
            assert_eq!(outer_and_holes(&l.solid.front), (outer, holes), "{text}");
        }
    }

    #[test]
    fn empty_text_gives_nothing_and_no_warning() {
        for text in ["", " ", "\n\n", "   \n  "] {
            let l = built(text);
            assert_eq!(l.solid.body.triangle_count(), 0, "{text:?}");
            assert!(l.warnings.is_empty(), "{text:?}: {:?}", l.warnings);
        }
    }

    #[test]
    fn huge_text_is_cut_off_with_a_warning() {
        let l = built(&"a".repeat(MAX_CHARS * 10));
        assert!(l.warnings.iter().any(|w| w.contains("longer than")), "{:?}", l.warnings);
        assert!(l.solid.body.triangle_count() > 0);
        assert!(l.solid.body.triangle_count() <= MAX_TRIANGLES);
        assert_closed(&l.solid.body);
    }

    #[test]
    fn the_triangle_cap_holds_with_the_roundest_glyphs() {
        let l = built(&"@".repeat(MAX_CHARS));
        assert!(l.solid.body.triangle_count() <= MAX_TRIANGLES, "{}", l.solid.body.triangle_count());
        assert_closed(&l.solid.body);
    }

    #[test]
    fn a_character_no_font_has_builds_and_counts_as_missing() {
        // Which fonts are installed decides whether this code point has a glyph; it builds either way.
        assert_closed(&built("A{378}B").solid.body);
        let style = TextStyle { family: DEFAULT_FONT.to_string(), ..TextStyle::default() };
        let text = "A ";
        let layout = effectcraft_text::layout::layout_uncached(text, &style, &ParagraphStyle::default());
        let mut g = layout.glyphs[0];
        assert!(!is_missing(&g, text));
        g.id = 0;
        assert!(is_missing(&g, text));
        g.cluster = 1;
        assert!(!is_missing(&g, text), "white space is not missing");
    }

    #[test]
    fn a_missing_font_warns_and_builds() {
        let mut r = ask("Ag");
        r.font = "No Such Family 12345".to_string();
        let l = lettering(&r).unwrap();
        assert!(l.warnings.iter().any(|w| w.contains("not found")), "{:?}", l.warnings);
        assert_closed(&l.solid.body);
    }

    #[test]
    fn zero_negative_and_huge_sizes_are_kept_in_range_with_a_warning() {
        for size in [0.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e30, -1e30] {
            let mut r = ask("O");
            r.size = size;
            let l = lettering(&r).unwrap();
            assert!(l.warnings.iter().any(|w| w.contains("`size`")), "{size}: {:?}", l.warnings);
            let b = l.solid.body.bounds().unwrap();
            assert!(b.size().iter().all(|s| s.is_finite()), "{size}");
            assert!(b.size()[2] <= MAX_SIZE as f32 * 2.0, "{size}");
            assert!(b.size()[2] > 0.0, "{size}");
        }
    }

    #[test]
    fn zero_negative_and_huge_depths_are_kept_in_range_with_a_warning() {
        for (depth, thickness) in
            [(-1.0, 0.0), (f64::NAN, 0.0), (f64::NEG_INFINITY, 0.0), (f64::INFINITY, 0.0), (1e30, MAX_SIZE)]
        {
            let mut r = ask("O");
            r.depth = depth;
            let l = lettering(&r).unwrap();
            assert!(l.warnings.iter().any(|w| w.contains("`depth`")), "{depth}: {:?}", l.warnings);
            let b = l.solid.body.bounds().unwrap();
            assert!((f64::from(b.size()[1]) - thickness).abs() < 1e-3, "{depth}: {:?}", b.size());
        }
        // Zero is allowed and plain: a flat front.
        let mut r = ask("O");
        r.depth = 0.0;
        let l = lettering(&r).unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.solid.body.triangle_count(), l.solid.front.triangle_count());
    }

    #[test]
    fn hostile_spacing_is_kept_in_range() {
        for spacing in [f64::NAN, f64::INFINITY, -1e9, 1e9] {
            let mut r = ask("Hello");
            r.spacing = spacing;
            let l = lettering(&r).unwrap();
            assert!(l.warnings.iter().any(|w| w.contains("`spacing`")), "{spacing}: {:?}", l.warnings);
            assert_closed(&l.solid.body);
        }
    }

    #[test]
    fn letters_pushed_into_each_other_are_joined() {
        let mut r = ask("HHH");
        r.spacing = -0.3;
        let l = lettering(&r).unwrap();
        assert_closed(&l.solid.body);
        assert_eq!(outer_and_holes(&l.solid.front).0, 1);
    }

    #[test]
    fn spacing_and_alignment_move_the_block() {
        let width = |r: &Request| {
            let b = lettering(r).unwrap().solid.body.bounds().unwrap();
            (b.min[0], b.max[0])
        };
        let plain = ask("HH");
        let mut wide = plain.clone();
        wide.spacing = 0.5;
        assert!(width(&wide).1 - width(&wide).0 > width(&plain).1 - width(&plain).0 + 0.4);
        let mut left = plain.clone();
        left.align = "left".to_string();
        let mut right = plain.clone();
        right.align = "right".to_string();
        assert!(width(&left).0 > -0.2 && width(&left).0 < 0.2);
        assert!(width(&right).1 < 0.2 && width(&right).1 > -0.2);
        let (a, b) = width(&plain);
        assert!((a + b).abs() < 0.1, "{a} {b}");
    }

    #[test]
    fn size_scales_and_faces_minus_y() {
        let mut r = ask("O");
        r.size = 3.0;
        r.depth = 0.5;
        let l = lettering(&r).unwrap();
        let b = l.solid.body.bounds().unwrap();
        let cap = b.size()[2];
        // Inter's O: about 0.75 of the em.
        assert!(cap > 2.0 && cap < 2.6, "{cap}");
        assert!((b.min[1] + 0.25).abs() < 1e-5 && (b.max[1] - 0.25).abs() < 1e-5);
        assert!(l.solid.front.vertices.iter().all(|v| v.position[1] == -0.25 && v.normal[1] == -1.0));
    }

    #[test]
    fn a_curve_draws_no_lines_across_its_side() {
        // O has no corner: every wall triangle has two inner edges.
        let m = built("O").solid.body;
        let wall = |t: &[u32; 3]| {
            let [a, b, c] = t.map(|i| m.vertices[i as usize].position[1]);
            a != b || b != c
        };
        for (t, mask) in m.indices.as_chunks::<3>().0.iter().zip(&m.inner_edges) {
            if wall(t) {
                assert_eq!(mask.count_ones(), 2, "{mask:b}");
            }
        }
        // H is all corners: every wall triangle keeps its line along the depth.
        let m = built("H").solid.body;
        for (t, mask) in m.indices.as_chunks::<3>().0.iter().zip(&m.inner_edges) {
            let [a, b, c] = t.map(|i| m.vertices[i as usize].position[1]);
            if a != b || b != c {
                assert_eq!(mask.count_ones(), 1, "{mask:b}");
            }
        }
    }

    #[test]
    fn the_longest_line_at_the_largest_spacing_stays_on_the_grid() {
        let mut r = ask(&"H".repeat(MAX_CHARS));
        r.spacing = SPACING.1;
        let l = lettering(&r).unwrap();
        assert_closed(&l.solid.body);
    }

    #[test]
    fn a_broken_font_cannot_crash() {
        // Random bytes, a cut file and a file with bytes of its tables scrambled: the fonts
        // database refuses them or they give odd outlines. A panic in the parser ends only the
        // job in the node, as here it ends only the catch_unwind.
        let bytes = effectcraft_text::fonts::INTER_REGULAR;
        let mut cases: Vec<Vec<u8>> = vec![vec![0x5a; 4096], bytes[..bytes.len() / 3].to_vec()];
        let mut scrambled = bytes.to_vec();
        let mut seed = 5u64;
        for _ in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // Past the table directory, into the data.
            let at = 2048 + (seed >> 33) as usize % (scrambled.len() - 2048);
            scrambled[at] ^= (seed >> 25) as u8;
        }
        cases.push(scrambled);
        for case in cases {
            let faces = effectcraft_text::fonts::add_font_data(case);
            for face in faces {
                let family = effectcraft_text::fonts::face(face).info.family.clone();
                let mut r = ask("Broken @&8");
                r.font = family;
                let done = std::panic::catch_unwind(|| lettering(&r));
                if let Ok(Ok(l)) = done
                    && !l.solid.body.indices.is_empty()
                {
                    assert_closed(&l.solid.body);
                }
            }
        }
    }

    /// Contours with the coordinates a broken font could hand over.
    #[test]
    fn broken_outlines_are_left_out_with_their_count() {
        let path = {
            let mut p = BezPath::new();
            p.move_to((f64::NAN, 0.0));
            p.line_to((1.0, 0.0));
            p.quad_to((f64::INFINITY, 1.0), (1.0, 1.0));
            p.close_path();
            p
        };
        let c = contours(&path);
        let s = extrude::shape(&c).unwrap();
        assert_eq!(s.unusable, 1);
        assert!(s.is_empty());
    }

    #[test]
    fn a_curve_is_stepped_under_the_crease() {
        // A quarter circle as one cubic gets at least six steps of 15 degrees, tiny or not.
        let k = 0.5523;
        for r in [1.0, 1000.0] {
            let q = [Point::new(r, 0.0), Point::new(r, k * r), Point::new(k * r, r), Point::new(0.0, r)];
            assert!(steps(&q) >= 6, "{r}: {}", steps(&q));
        }
    }
}
