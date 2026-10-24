//! `tex.text`: text laid out and rasterised on the CPU with `effectcraft-text` (shaping, bidi,
//! bundled OFL fonts plus the system's) and tiny-skia, off the cook thread, then placed and
//! tinted by a GPU pass. Only the text, font, style, size and alignment rasterise again;
//! position and colour are uniforms, so binding them to channels costs one pass per frame. While a
//! new size rasterises, the last raster is drawn scaled to it, so dragging `size` follows the
//! pointer every frame and sharpens a frame later.

use std::sync::{Arc, Once, OnceLock};

use effectcraft_text::kurbo::{Affine, BezPath, PathEl, Shape};
use effectcraft_text::{Align, ParagraphStyle, TextStyle};
use half::f16;
use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::image::Decoded;
use penumbra_gpu::{Gpu, PassInput, Tex, TexRing};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDef, NodeDesc,
    NodeTypes, Operator,
};

use crate::on_gpu;

/// Longest text rasterised. Layout time grows with the square of a line's length
/// (measured: 16k characters 0.2 s, 200k 62 s).
pub const MAX_CHARS: usize = 4096;
/// Largest font size in pixels.
pub const MAX_SIZE: f64 = 2000.0;

/// The node kind.
pub const TEXT: &str = "tex.text";
/// The `font` param's default, bundled.
pub const DEFAULT_FONT: &str = "Inter";

const SOURCE: &str = include_str!("text.wgsl");

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(TEXT, "Writes text into a picture, in an installed font or a bundled one")
            .param(
                ParamSpec::text("text", "Text")
                    .multiline()
                    .doc("What is written. Line breaks are kept; align sets how the lines line up"),
            )
            .param(
                ParamSpec::float("size", 100.0)
                    .clamp(1.0, MAX_SIZE)
                    .range(4.0, 500.0)
                    .curve(Curve::Log)
                    .unit(Unit::Pixels)
                    .doc("Font size in pixels of the output picture (the font's em, a little more than a capital's height)"),
            )
            .param(
                ParamSpec::color("color", [1.0, 1.0, 1.0, 1.0])
                    .doc("Colour of the text, linear RGBA with straight alpha; the rest is transparent"),
            )
            .param(
                ParamSpec::vec2("position", [0.5, 0.5])
                    .range(0.0, 1.0)
                    .doc("Where the text's anchor sits, in image sizes; 0, 0 is the top left"),
            )
            .param(
                ParamSpec::choice("align", &["center", "left", "right"])
                    .doc("How the lines line up with each other, and which point of the block sits at position")
                    .choice_doc("center", "centred; the block's centre at position")
                    .choice_doc("left", "flush left; the block's left edge at position")
                    .choice_doc("right", "flush right; the block's right edge at position"),
            )
            .param(
                ParamSpec::text("font", DEFAULT_FONT)
                    .doc("Family name; Inter, JetBrains Mono, Noto Serif are bundled"),
            )
            .param(
                ParamSpec::text("style", "Regular")
                    .doc("Style within the family: Regular, Bold, Italic, Light, ... The menu lists the family's styles"),
            )
            .param(super::width_param().more())
            .param(super::height_param().more())
            .param(super::keep_size())
            .output("out", DataKind::Tex),
        || Box::new(Text::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// Font families with their styles (weight order), sorted by family, for the param panel's
/// menus and `text.fonts`. The first call starts the system font scan on its own thread, so a
/// slow disk never stalls the UI; `None` until that scan is done.
pub fn families() -> Option<&'static [(String, Vec<String>)]> {
    static LIST: OnceLock<Vec<(String, Vec<String>)>> = OnceLock::new();
    static START: Once = Once::new();
    if let Some(list) = LIST.get() {
        return Some(list);
    }
    START.call_once(|| {
        let spawned = std::thread::Builder::new().name("font scan".into()).spawn(|| {
            LIST.get_or_init(effectcraft_text::families);
        });
        if let Err(e) = spawned {
            log::warn!("cannot start the font scan: {e}");
        }
    });
    LIST.get().map(Vec::as_slice)
}

/// The params the placing pass declares.
fn pass_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::color("color", [1.0, 1.0, 1.0, 1.0]),
        ParamSpec::vec2("position", [0.5, 0.5]),
        ParamSpec::vec2("anchor", [0.0, 0.0]),
        ParamSpec::float("scale", 1.0),
    ]
}

/// Compiles the placing pass ahead of its first use.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    pass(gpu).map(|_| ())
}

fn pass(gpu: &Gpu) -> Result<Arc<penumbra_gpu::Pass>, Error> {
    gpu.cached_pass(TEXT, SOURCE, &pass_params())
}

/// What a raster depends on.
#[derive(Clone, PartialEq)]
struct Key {
    text: String,
    font: String,
    style: String,
    size: f64,
    align: String,
}

/// Rasterised text: white coverage, premultiplied, where the anchor lies in it (pixels), and the
/// font size it was laid out at.
struct Raster {
    tex: Tex,
    anchor: [f64; 2],
    size: f64,
    missing_font: bool,
}

#[derive(Default)]
struct Text {
    pass: super::PassCell,
    slot: AsyncSlot<Arc<Raster>>,
    key: Option<Key>,
    current: Option<Arc<Raster>>,
    ring: TexRing,
}

impl Operator for Text {
    fn trim(&mut self) {
        self.ring.trim();
    }

    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        if self.slot.poll() {
            self.current = self.slot.latest().cloned();
        }
        let key = Key {
            text: cx.text("text")?.to_string(),
            font: cx.text("font")?.to_string(),
            style: cx.text("style")?.to_string(),
            size: cx.float("size")?,
            align: cx.text("align")?.to_string(),
        };
        // One raster in flight; a newer key waits for it rather than replacing it. Replacing
        // dropped every result while a slider dragged `size`, so nothing showed until the drag
        // stopped. The job's end cooks the node again, which starts the next one.
        if self.key.as_ref() != Some(&key) && !self.slot.busy() {
            self.key = Some(key.clone());
            let gpu = gpu.clone();
            self.slot.start(cx.spawn(move |_job| rasterise(&gpu, &key).map(Arc::new)));
        }
        let Some(raster) = self.current.clone() else {
            return Err(match self.slot.error() {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = self.slot.error() {
            cx.warn(format!("{e} (showing the last text that worked)"));
        }
        if raster.missing_font {
            cx.warn(format!("font `{}` not found; using Inter", cx.text("font")?));
        }
        let (w, h) = (super::side(cx, "width")?, super::side(cx, "height")?);
        let pass = self.pass.get(gpu, pass)?;
        let target = self.ring.next(gpu, w, h, "tex.text")?;
        let anchor = Value::Vec2(raster.anchor);
        let size = cx.float("size")?.clamp(1.0, MAX_SIZE);
        let scale = Value::Float(if raster.size > 0.0 { size / raster.size } else { 1.0 });
        let param = |name: &str| match name {
            "anchor" => Some(anchor.clone()),
            "scale" => Some(scale.clone()),
            _ => cx.param(name).cloned(),
        };
        let inputs = [Some(&raster.tex)];
        pass.render(gpu, &target, &PassInput { inputs: &inputs, seconds: 0.0, frame: 0, param: &param })?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

/// Lays out and fills the text into a texture just large enough to hold it.
fn rasterise(gpu: &Gpu, key: &Key) -> Result<Raster, String> {
    if key.text.chars().count() > MAX_CHARS {
        return Err(format!("the text is longer than {MAX_CHARS} characters"));
    }
    let resolved = effectcraft_text::resolve(&key.font, &key.style);
    let style = TextStyle {
        family: key.font.clone(),
        style: key.style.clone(),
        size: key.size.clamp(1.0, MAX_SIZE) as f32,
        ..TextStyle::default()
    };
    let align = match key.align.as_str() {
        "left" => Align::Left,
        "right" => Align::Right,
        _ => Align::Center,
    };
    let para = ParagraphStyle { align, ..ParagraphStyle::default() };
    let layout = effectcraft_text::layout::layout_uncached(&key.text, &style, &para);

    let mut path = BezPath::new();
    for g in &layout.glyphs {
        let outline =
            Affine::translate((f64::from(g.x), f64::from(g.y))) * effectcraft_text::glyph_outline(g);
        path.extend(outline.elements().iter().copied());
    }
    // The block: line boxes for the vertical extent (stable while the letters change), the
    // lines' content for the horizontal anchor.
    let top = layout.lines.first().map_or(0.0, |l| f64::from(l.baseline - l.ascent));
    let bottom = layout.lines.last().map_or(0.0, |l| f64::from(l.baseline + l.descent));
    let left = layout.lines.iter().map(|l| f64::from(l.x)).fold(f64::INFINITY, f64::min);
    let right = layout.lines.iter().map(|l| f64::from(l.x + l.width)).fold(f64::NEG_INFINITY, f64::max);
    let (left, right) = if left <= right { (left, right) } else { (0.0, 0.0) };
    let ink = if path.elements().is_empty() { None } else { Some(path.bounding_box()) };
    let pad = 2.0;
    let x0 = ink.map_or(left, |r| r.x0.min(left)).floor() - pad;
    let y0 = ink.map_or(top, |r| r.y0.min(top)).floor() - pad;
    let x1 = ink.map_or(right, |r| r.x1.max(right)).ceil() + pad;
    let y1 = ink.map_or(bottom, |r| r.y1.max(bottom)).ceil() + pad;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return Err("the text has no finite extent".into());
    }
    let max = f64::from(gpu.max_texture_size());
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    if w > max || h > max {
        return Err(format!("the text needs {w} x {h} pixels, more than the GPU allows ({max})"));
    }
    // Bounded by the GPU limit just checked.
    let (w, h) = (w as u32, h as u32);
    let mut mask = tiny_skia::Mask::new(w, h).ok_or("cannot allocate the text mask")?;
    if let Some(p) = to_skia(&path) {
        let shift = tiny_skia::Transform::from_translate(-x0 as f32, -y0 as f32);
        mask.fill_path(&p, tiny_skia::FillRule::Winding, true, shift);
    }
    let rgba: Vec<f16> = mask
        .data()
        .iter()
        .flat_map(|&c| {
            let c = f16::from_f32(f32::from(c) / 255.0);
            [c, c, c, c]
        })
        .collect();
    let decoded = Decoded { width: w, height: h, rgba, source: "text".into() };
    let tex = gpu.upload(&decoded, "tex.text raster").map_err(|e| e.0)?;
    let ax = match align {
        Align::Left => left,
        Align::Right => right,
        _ => (left + right) / 2.0,
    };
    let ay = (top + bottom) / 2.0;
    let size = f64::from(style.size);
    Ok(Raster { tex, anchor: [ax - x0, ay - y0], size, missing_font: resolved.missing })
}

pub(super) fn to_skia(path: &BezPath) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    // Non-finite coordinates would poison the fill; such a glyph is dropped instead.
    let f = |x: f64| if x.is_finite() { Some(x as f32) } else { None };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => pb.move_to(f(p.x)?, f(p.y)?),
            PathEl::LineTo(p) => pb.line_to(f(p.x)?, f(p.y)?),
            PathEl::QuadTo(a, p) => pb.quad_to(f(a.x)?, f(a.y)?, f(p.x)?, f(p.y)?),
            PathEl::CurveTo(a, b, p) => pb.cubic_to(f(a.x)?, f(a.y)?, f(b.x)?, f(b.y)?, f(p.x)?, f(p.y)?),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}
