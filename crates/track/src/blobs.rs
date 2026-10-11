//! The stateful part: background, detection and ids that stay with a blob.

use crate::mask::{Mask, Region, components};
use crate::{Image, luma};

/// What makes a pixel part of a blob.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detect {
    /// It differs from the learned background by at least the threshold (in any channel).
    #[default]
    Background,
    /// Its luminance is at least the threshold; no background (spots of light, a lit person
    /// against a dark room).
    Bright,
}

/// How the tracker works; may change between frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub detect: Detect,
    /// Difference to the background, or luminance, from which a pixel counts (0..1, linear).
    pub threshold: f32,
    /// Seconds in which a lasting change of the room becomes background (63 % of the way);
    /// pixels inside a blob follow ten times slower, so someone standing still fades out
    /// slowly. 0: the background stays as learned.
    pub adapt: f32,
    /// Radius in pixels of the opening that removes specks and thin lines.
    pub open: usize,
    /// Radius in pixels of the closing that fills holes and joins near parts.
    pub close: usize,
    /// Smallest and largest blob as a fraction of the picture's area.
    pub min_area: f32,
    pub max_area: f32,
    /// Most blobs reported (the largest).
    pub max_blobs: usize,
    /// How far a blob may move from one frame to the next and keep its id, as a fraction of
    /// the picture's width (beyond where its speed predicted it).
    pub reach: f32,
    /// Seconds a blob may go missing and come back with its id.
    pub hold: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            detect: Detect::Background,
            threshold: 0.1,
            adapt: 30.0,
            open: 1,
            close: 2,
            min_area: 0.001,
            max_area: 0.5,
            max_blobs: 16,
            reach: 0.15,
            hold: 0.5,
        }
    }
}

/// A tracked region, in fractions of the picture (origin top left).
#[derive(Clone, Debug, PartialEq)]
pub struct Blob {
    /// Stays the same while the blob is tracked; never reused within a [`Tracker`]'s life.
    pub id: u64,
    /// Centre of its pixels.
    pub x: f32,
    pub y: f32,
    /// Bounding box.
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// Fraction of the picture's area.
    pub area: f32,
    /// Mean colour of its pixels in the picture (straight linear RGB).
    pub color: [f32; 3],
    /// Seconds since it appeared.
    pub age: f32,
    /// Speed in picture widths and heights per second, smoothed.
    pub vx: f32,
    pub vy: f32,
}

struct Track {
    blob: Blob,
    /// Seconds since it was last seen; 0 when seen in this frame.
    missing: f32,
}

/// Background, the blobs of the frames before and the next id.
pub struct Tracker {
    background: Option<Image>,
    learn: bool,
    tracks: Vec<Track>,
    next_id: u64,
    mask: Mask,
    /// The value thresholded per pixel in the last frame (difference or luminance).
    value: Vec<f32>,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Tracker {
            background: None,
            learn: false,
            tracks: Vec::new(),
            next_id: 1,
            mask: Mask::default(),
            value: Vec::new(),
        }
    }

    /// The next frame becomes the background.
    pub fn learn(&mut self) {
        self.learn = true;
    }

    pub fn background(&self) -> Option<&Image> {
        self.background.as_ref()
    }

    /// Takes a background kept from before (a restart); ignored when its size differs from the
    /// frames.
    pub fn set_background(&mut self, background: Image) {
        self.background = Some(background);
        self.learn = false;
    }

    /// The pixels counted in the last frame, after opening and closing.
    pub fn mask(&self) -> &Mask {
        &self.mask
    }

    /// Per pixel of the last frame: its difference to the background, or its luminance.
    pub fn value(&self) -> &[f32] {
        &self.value
    }

    /// Takes a frame `dt` seconds after the one before and returns the blobs seen in it, by id.
    pub fn update(&mut self, frame: &Image, dt: f32, s: &Settings) -> Vec<Blob> {
        let dt = if dt.is_finite() { dt.clamp(0.0, 1.0) } else { 0.0 };
        let (w, h) = (frame.width, frame.height);
        if frame.data.len() != w.saturating_mul(h) {
            return self.age(dt, s);
        }
        let mut mask = Mask::empty(w, h);
        self.value.clear();
        let threshold = if s.threshold.is_finite() { s.threshold } else { 0.1 };
        match s.detect {
            Detect::Bright => {
                for (m, c) in mask.data.iter_mut().zip(&frame.data) {
                    let v = luma(*c);
                    self.value.push(v);
                    *m = u8::from(v >= threshold);
                }
            }
            Detect::Background => {
                let fits = self.background.as_ref().is_some_and(|b| b.width == w && b.height == h);
                if self.learn || !fits {
                    self.background = Some(frame.clone());
                    self.learn = false;
                }
                let Some(bg) = self.background.as_mut() else { return self.age(dt, s) };
                let rate =
                    if s.adapt > 0.0 && s.adapt.is_finite() { 1.0 - (-dt / s.adapt).exp() } else { 0.0 };
                for ((m, c), b) in mask.data.iter_mut().zip(&frame.data).zip(bg.data.iter_mut()) {
                    let v = (c[0] - b[0]).abs().max((c[1] - b[1]).abs()).max((c[2] - b[2]).abs());
                    let v = if v.is_finite() { v } else { 0.0 };
                    self.value.push(v);
                    let set = v >= threshold;
                    *m = u8::from(set);
                    let r = if set { rate * 0.1 } else { rate };
                    if r > 0.0 {
                        for (bc, cc) in b.iter_mut().zip(c) {
                            if cc.is_finite() {
                                *bc += (cc - *bc) * r;
                            }
                        }
                    }
                }
            }
        }
        mask.open(s.open);
        mask.close(s.close);
        let regions = components(&mask, &frame.data);
        self.mask = mask;
        let pixels = (w * h).max(1) as f32;
        let found: Vec<Region> = regions
            .into_iter()
            .filter(|r| {
                let a = r.area as f32 / pixels;
                a >= s.min_area && a <= s.max_area
            })
            .take(s.max_blobs)
            .collect();
        self.follow(&found, w, h, dt, s)
    }

    /// No picture this frame: blobs go missing.
    fn age(&mut self, dt: f32, s: &Settings) -> Vec<Blob> {
        for t in &mut self.tracks {
            t.missing += dt;
        }
        self.tracks.retain(|t| t.missing <= s.hold);
        Vec::new()
    }

    /// Matches `found` to the tracks: nearest pairs first, each within `reach` of where the
    /// track's speed puts it now.
    fn follow(&mut self, found: &[Region], w: usize, h: usize, dt: f32, s: &Settings) -> Vec<Blob> {
        let (fw, fh) = (w.max(1) as f32, h.max(1) as f32);
        let aspect = fh / fw;
        let blobs: Vec<Blob> = found
            .iter()
            .map(|r| {
                let n = r.area.max(1) as f64;
                Blob {
                    id: 0,
                    x: r.centre[0] as f32 / fw,
                    y: r.centre[1] as f32 / fh,
                    left: r.min[0] as f32 / fw,
                    top: r.min[1] as f32 / fh,
                    width: (r.max[0] + 1 - r.min[0]) as f32 / fw,
                    height: (r.max[1] + 1 - r.min[1]) as f32 / fh,
                    area: r.area as f32 / (fw * fh),
                    color: r.color_sum.map(|c| (c / n) as f32),
                    age: 0.0,
                    vx: 0.0,
                    vy: 0.0,
                }
            })
            .collect();
        let reach = if s.reach.is_finite() { s.reach.max(0.0) } else { 0.0 };
        let mut pairs = Vec::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            let ahead = t.missing + dt;
            let (px, py) = (t.blob.x + t.blob.vx * ahead, t.blob.y + t.blob.vy * ahead);
            for (bi, b) in blobs.iter().enumerate() {
                // Distances in picture widths in both directions.
                let d = ((b.x - px).powi(2) + ((b.y - py) * aspect).powi(2)).sqrt();
                if d <= reach {
                    pairs.push((d, ti, bi));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut track_of = vec![None; blobs.len()];
        let mut taken = vec![false; self.tracks.len()];
        for (_, ti, bi) in pairs {
            let (Some(slot), Some(t)) = (track_of.get_mut(bi), taken.get_mut(ti)) else { continue };
            if slot.is_none() && !*t {
                *slot = Some(ti);
                *t = true;
            }
        }
        for t in &mut self.tracks {
            t.missing += dt;
        }
        for (b, ti) in blobs.into_iter().zip(track_of) {
            match ti.and_then(|i| self.tracks.get_mut(i)) {
                Some(t) => {
                    let gap = t.missing;
                    let old = &t.blob;
                    let (vx, vy) =
                        if gap > 0.0 { ((b.x - old.x) / gap, (b.y - old.y) / gap) } else { (old.vx, old.vy) };
                    // Half the new measurement: a camera's centroids jitter by a pixel or two.
                    let vx = 0.5 * (old.vx + vx);
                    let vy = 0.5 * (old.vy + vy);
                    t.blob = Blob { id: old.id, age: old.age + gap, vx, vy, ..b };
                    t.missing = 0.0;
                }
                None => {
                    let id = self.next_id;
                    self.next_id = self.next_id.saturating_add(1);
                    self.tracks.push(Track { blob: Blob { id, ..b }, missing: 0.0 });
                }
            }
        }
        self.tracks.retain(|t| t.missing <= s.hold);
        let mut seen: Vec<Blob> =
            self.tracks.iter().filter(|t| t.missing == 0.0).map(|t| t.blob.clone()).collect();
        seen.sort_by_key(|b| b.id);
        seen
    }
}
