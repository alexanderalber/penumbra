//! Automation ids: every interactive element registers a stable id and its screen rect each
//! frame, so the control channel can find and operate it (`ui.elements`, `ui.click {id}`,
//! `ui.drag {from, to}`), the way photocraft and effectcraft do.
//!
//! Ids are dotted and stable across runs: `node./noise1`, `port.in./level1.0`,
//! `port.out./noise1.0`, `param./noise1.scale`, `button.undo`, `history.3`, `add.tex.noise`.

use egui::Rect;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    /// x, y, width, height in points.
    pub rect: [f32; 4],
}

impl Element {
    pub fn rect(&self) -> Rect {
        let [x, y, w, h] = self.rect;
        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }
}

#[derive(Default)]
pub struct Registry {
    current: Vec<Element>,
    /// The last complete frame's elements; lookups between frames use these.
    previous: Vec<Element>,
}

impl Registry {
    pub fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    pub fn add(&mut self, id: impl Into<String>, rect: Rect, label: impl Into<String>) {
        self.current.push(Element {
            id: id.into(),
            label: label.into(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
        });
    }

    /// The element with this id, from this frame or the last one.
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.current.iter().rev().chain(self.previous.iter().rev()).find(|e| e.id == id)
    }

    /// The last complete frame's elements whose id starts with `prefix`.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.previous.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}
