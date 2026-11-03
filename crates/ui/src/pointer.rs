//! A drawn mouse pointer for recordings and screenshots: synthetic input (`ui.click`,
//! `ui.drag`, ...) moves no real cursor, so a headless picture shows none. With `ui.pointer
//! {show}` the app paints an arrow where egui sees the pointer and a ring where a button goes
//! down; with `glide` the input commands move the pointer to their target over that many
//! frames instead of jumping, so a viewer can follow it.

use egui::{Color32, Event, LayerId, Order, Pos2, Shape, Stroke, vec2};

/// Frames a click's ring grows and fades.
const RING_FRAMES: u32 = 14;
/// Most frames a glide may take.
pub const MAX_GLIDE: u32 = 600;

/// The pointer's look and where synthetic input left it.
pub struct Pointer {
    pub show: bool,
    /// Frames a move to the next target takes; 0 jumps.
    pub glide: u32,
    /// The arrow's size relative to a system cursor.
    pub size: f32,
    /// Where the last queued synthetic move put the pointer.
    pub at: Option<Pos2>,
    /// Rings of clicks still showing: where, and frames since.
    rings: Vec<(Pos2, u32)>,
}

impl Default for Pointer {
    fn default() -> Self {
        Self { show: false, glide: 0, size: 1.4, at: None, rings: Vec::new() }
    }
}

/// `t` from 0 to 1, slow at both ends.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Pointer {
    /// The moves that bring the pointer from where it is to `to`, one per frame, ending at
    /// `to`; just `to` when gliding is off or the pointer's place is unknown.
    pub fn moves_to(&mut self, to: Pos2) -> Vec<Event> {
        let from = self.at.replace(to);
        let steps = match from {
            Some(f) if self.glide > 0 && f.distance(to) > 0.5 => self.glide.min(MAX_GLIDE),
            _ => 1,
        };
        let from = from.unwrap_or(to);
        (1..=steps).map(|i| Event::PointerMoved(from.lerp(to, ease(i as f32 / steps as f32)))).collect()
    }

    /// Notes a synthetic move that did not come from [`Pointer::moves_to`] (the steps of a drag).
    pub fn moved(&mut self, to: Pos2) {
        self.at = Some(to);
    }

    /// Paints the pointer and the rings of this frame's presses, over everything.
    pub fn paint(&mut self, ctx: &egui::Context) {
        if !self.show {
            self.rings.clear();
            return;
        }
        let (pos, pressed) = ctx.input(|i| {
            let pressed: Vec<Pos2> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::PointerButton { pos, pressed: true, .. } => Some(*pos),
                    _ => None,
                })
                .collect();
            (i.pointer.latest_pos(), pressed)
        });
        self.rings.extend(pressed.into_iter().map(|p| (p, 0)));
        let painter = ctx.layer_painter(LayerId::new(Order::Debug, egui::Id::new("penumbra pointer")));
        for (at, age) in &mut self.rings {
            let t = *age as f32 / RING_FRAMES as f32;
            let alpha = ((1.0 - t) * 220.0) as u8;
            painter.circle_stroke(
                *at,
                (6.0 + 18.0 * ease(t)) * self.size,
                Stroke::new(2.5 * self.size, Color32::from_rgba_unmultiplied(255, 214, 90, alpha)),
            );
            *age += 1;
        }
        self.rings.retain(|(_, age)| *age < RING_FRAMES);
        let Some(pos) = pos.or(self.at) else { return };
        // The classic arrow, its tip at the pointer, 20 points tall at size 1.
        let s = self.size;
        let corners =
            [(0.0, 0.0), (0.0, 17.0), (4.2, 13.2), (7.0, 19.6), (9.6, 18.5), (6.9, 12.3), (12.2, 12.3)];
        let [tip, left, notch, heel, toe, inner, right] = corners.map(|(x, y)| pos + vec2(x, y) * s);
        // Not convex at the notch: filled as two convex parts, the head and the tail.
        let parts = |off: egui::Vec2| {
            [vec![tip, left, notch, inner, right], vec![notch, heel, toe, inner]]
                .map(|part| part.into_iter().map(|p| p + off).collect::<Vec<Pos2>>())
        };
        for part in parts(vec2(1.2, 1.6) * s) {
            painter.add(Shape::convex_polygon(part, Color32::from_black_alpha(90), Stroke::NONE));
        }
        for part in parts(egui::Vec2::ZERO) {
            painter.add(Shape::convex_polygon(part, Color32::WHITE, Stroke::NONE));
        }
        let outline = vec![tip, left, notch, heel, toe, inner, right];
        painter.add(Shape::closed_line(outline, Stroke::new(1.2 * s, Color32::BLACK)));
    }
}

#[cfg(test)]
mod tests {
    use egui::pos2;

    use super::*;

    #[test]
    fn a_glide_eases_to_the_target_and_jumps_without_one() {
        let mut p = Pointer::default();
        // The first move has nowhere to start from.
        assert_eq!(p.moves_to(pos2(10.0, 10.0)).len(), 1);
        p.glide = 4;
        let moves = p.moves_to(pos2(50.0, 10.0));
        let xs: Vec<f32> = moves
            .iter()
            .map(|e| match e {
                Event::PointerMoved(at) => at.x,
                _ => f32::NAN,
            })
            .collect();
        assert_eq!(xs.len(), 4);
        assert_eq!(xs[3], 50.0);
        assert!(xs.windows(2).all(|w| w[1] > w[0]), "{xs:?}");
        // Slow at the start: the first step is shorter than the second.
        assert!(xs[0] - 10.0 < xs[1] - xs[0], "{xs:?}");
        // Already there: one move.
        assert_eq!(p.moves_to(pos2(50.0, 10.0)).len(), 1);
        p.glide = 0;
        assert_eq!(p.moves_to(pos2(0.0, 0.0)).len(), 1);
    }
}
