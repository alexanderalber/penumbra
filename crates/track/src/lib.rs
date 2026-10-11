//! Blob tracking on the CPU: pure functions and one stateful [`Tracker`], tested against
//! synthetic pictures.
//!
//! A frame arrives as a small [`Image`] (the GPU shrinks the camera picture before it is read
//! back, so everything here runs on a few hundred thousand pixels at most). The tracker
//!
//! 1. marks the pixels that differ from a learned background, or that are bright
//!    ([`Detect`]), as a [`Mask`];
//! 2. lets the background follow slow changes in the room ([`Settings::adapt`]);
//! 3. removes specks and fills holes ([`Mask::open`], [`Mask::close`]);
//! 4. finds the connected regions ([`components`]) and keeps those of a sensible size;
//! 5. matches them to the blobs of the frames before, so a blob keeps its id while it moves
//!    and through a short gap ([`Settings::reach`], [`Settings::hold`]).
//!
//! Coordinates are fractions of the picture, origin top left, as surface coordinates are.

mod blobs;
mod mask;

pub use blobs::{Blob, Detect, Settings, Tracker};
pub use mask::{Mask, Region, components};

/// Straight (not premultiplied) linear RGB, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub data: Vec<[f32; 3]>,
}

impl Image {
    /// `None` when `data` does not hold `width * height` pixels.
    pub fn new(width: usize, height: usize, data: Vec<[f32; 3]>) -> Option<Self> {
        (width.checked_mul(height) == Some(data.len())).then_some(Image { width, height, data })
    }

    /// A picture of one colour.
    pub fn filled(width: usize, height: usize, color: [f32; 3]) -> Self {
        Image { width, height, data: vec![color; width.saturating_mul(height)] }
    }

    pub fn get(&self, x: usize, y: usize) -> Option<[f32; 3]> {
        if x >= self.width {
            return None;
        }
        self.data.get(y.checked_mul(self.width)?.checked_add(x)?).copied()
    }

    pub fn set(&mut self, x: usize, y: usize, color: [f32; 3]) {
        if x >= self.width {
            return;
        }
        if let Some(p) =
            y.checked_mul(self.width).and_then(|i| i.checked_add(x)).and_then(|i| self.data.get_mut(i))
        {
            *p = color;
        }
    }
}

/// Rec. 709 luminance of linear RGB.
pub fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

#[cfg(test)]
mod tests;
