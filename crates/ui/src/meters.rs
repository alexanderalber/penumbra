//! The viewer's view of a `chan.audio_analysis`: the meters [`penumbra_ops::meters`] lays out
//! (levels with peaks, tempo, beat light, bar, recent hits, spectrum), painted with egui. The
//! `tex.audio_meters` node draws the same shapes into a texture, so both look alike.
//!
//! The meters' short memory (peaks, recent hits, when the last beat came) is kept here, per
//! viewed node, not in the op.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2,
};
use penumbra_core::{Chans, NodeId};
use penumbra_graph::Data;
use penumbra_ops::meters::{self, Anchor, Measure, Meters, Shape};

use crate::theme;

/// The analysis node the viewer shows meters for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioNode {
    pub id: NodeId,
}

impl AudioNode {
    /// The analysis node `id` of `net`, if it is one.
    pub fn of(net: &penumbra_graph::Network, id: NodeId) -> Option<AudioNode> {
        let node = net.node(id)?;
        (node.kind == penumbra_ops::audio::AUDIO_ANALYSIS).then_some(AudioNode { id })
    }
}

/// The view's memory of one node.
#[derive(Default)]
pub struct AudioMeters {
    node: Option<NodeId>,
    /// The `out` output seen last, to tell a new cook from a repaint of the same one; held, so
    /// a new output cannot be allocated where it was.
    last: Option<Data>,
    meters: Meters,
}

impl AudioMeters {
    /// Draws the view of `node` into `rect`: `out` and `spectrum` are its two outputs (either
    /// may be missing or not cooked yet), `now` in seconds on a steady clock.
    pub fn draw(
        &mut self,
        painter: &Painter,
        rect: Rect,
        node: &AudioNode,
        out: Option<&Data>,
        spectrum: Option<&Data>,
        now: f64,
    ) {
        if self.node != Some(node.id) {
            *self = AudioMeters { node: Some(node.id), ..AudioMeters::default() };
        }
        let fresh = match (out, &self.last) {
            (Some(o), Some(last)) => !o.same(last),
            (Some(_), None) => true,
            (None, _) => false,
        };
        if fresh {
            self.last = out.cloned();
        }
        let chans = out.and_then(|d| d.get::<Chans>());
        let reading = self.meters.update(chans, fresh, now);
        let spectrum = spectrum
            .and_then(|d| d.get::<Chans>())
            .and_then(|c| c.channels().first())
            .map(|c| c.samples.as_slice());
        let room = meters::Rect::from_min_max([rect.min.x, rect.min.y], [rect.max.x, rect.max.y]);
        let analysed = chans.is_some_and(|c| !c.is_empty());
        let shapes = self.meters.shapes(
            room,
            &reading,
            spectrum,
            analysed,
            &theme::meters_palette(),
            &EguiMeasure(painter),
            now,
        );
        for shape in shapes {
            paint(painter, shape);
        }
    }
}

/// Text measured as egui sets it.
struct EguiMeasure<'a>(&'a Painter);

impl Measure for EguiMeasure<'_> {
    fn text_size(&self, text: &str, size: f32) -> [f32; 2] {
        let galley = self.0.layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::WHITE);
        [galley.size().x, galley.size().y]
    }
}

fn color(c: meters::Color) -> Color32 {
    let [r, g, b, a] = c;
    Color32::from_rgba_premultiplied(r, g, b, a)
}

fn rect(r: meters::Rect) -> Rect {
    Rect::from_min_max(pos2(r.min[0], r.min[1]), pos2(r.max[0], r.max[1]))
}

fn corner(radius: f32) -> CornerRadius {
    // `as`: a small radius in points, clamped first.
    CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)
}

fn align(fraction: f32) -> Align {
    if fraction < 0.25 {
        Align::Min
    } else if fraction > 0.75 {
        Align::Max
    } else {
        Align::Center
    }
}

fn paint(painter: &Painter, shape: Shape) {
    match shape {
        Shape::Rect { rect: r, radius, color: c } => {
            painter.rect_filled(rect(r), corner(radius), color(c));
        }
        Shape::Frame { rect: r, radius, width, color: c } => {
            painter.rect_stroke(rect(r), corner(radius), Stroke::new(width, color(c)), StrokeKind::Inside);
        }
        Shape::Circle { center, radius, color: c } => {
            painter.circle_filled(pos2(center[0], center[1]), radius, color(c));
        }
        Shape::Arc { center, radius, width, fraction, color: c } => {
            arc(painter, pos2(center[0], center[1]), radius, fraction, Stroke::new(width, color(c)));
        }
        Shape::Line { from, to, width, color: c } => {
            painter.line_segment([pos2(from[0], from[1]), pos2(to[0], to[1])], Stroke::new(width, color(c)));
        }
        Shape::Text { at, anchor: Anchor([x, y]), size, text, color: c } => {
            painter.text(
                pos2(at[0], at[1]),
                Align2([align(x), align(y)]),
                text,
                FontId::proportional(size),
                color(c),
            );
        }
    }
}

/// A ring from the top, clockwise, for `fraction` of a turn.
fn arc(painter: &Painter, center: Pos2, radius: f32, fraction: f32, stroke: Stroke) {
    let fraction = fraction.clamp(0.0, 1.0);
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
