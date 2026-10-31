//! Whether a camera's exposure suits a measurement: from its pictures of one projector alone at
//! white (a few consecutive frames) and of all projectors at black, read within where that
//! projector lights the picture.
//!
//! What a measurement needs from the camera:
//! - Headroom: no pixel of the white at the top of the camera's range. A clipped white reads
//!   too dim, and every colour or brightness derived from it is wrong.
//! - Enough signal: the brightest pixels of the white well above the black, so the dim steps
//!   of a ramp still differ from noise.
//! - A steady picture: the same light from frame to frame and from row to row. A shutter
//!   shorter than the projector's frame catches part of a frame and shows bands or flicker.
//!
//! Values are read as the camera encoded them (0..1 of its range, sRGB-like), since clipping
//! and "brightest at about 200 of 255" are statements about codes, not light.

use crate::colour::{Picture, count, decode, encode, luminance};

/// Encoded values at or above this count as clipped (about 251 of 255).
pub const CLIPPED: f64 = 0.985;
/// Share of the spot that may clip before the exposure is refused.
pub const MAX_CLIPPED: f64 = 0.002;
/// The brightest pixels (99th percentile, encoded) should lie about here: high enough for a
/// clean signal, low enough for headroom when the projector or the room gets brighter.
pub const TARGET: f64 = 200.0 / 255.0;
/// Below this the white is too dim for a clean measurement.
pub const DARK: f64 = 110.0 / 255.0;
/// Frame-to-frame or row-to-row variation (relative) above this warns.
pub const UNSTEADY: f64 = 0.02;

/// What the camera reads of one projector's white.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Pixels the reading covers.
    pub pixels: usize,
    /// The brightest pixels of the white: the 99th percentile of each pixel's largest channel,
    /// encoded (0..1).
    pub peak: f64,
    /// The median of each pixel's largest channel, encoded.
    pub median: f64,
    /// The median of the black there, encoded.
    pub black: f64,
    /// Share of the pixels where some channel clips.
    pub clipped: f64,
    /// Channels that clip somewhere (red, green, blue).
    pub clipped_channels: [bool; 3],
    /// Spread of the mean brightness over the frames, relative to it ((max - min) / mean).
    pub flicker: f64,
    /// How much the brightness of the rows changes between frames, relative (RMS): bands
    /// that move, from a shutter shorter than the projector's frame.
    pub banding: f64,
}

/// The verdict on a [`Reading`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Some of the white clips: a measurement would be wrong.
    Clipped,
    /// The white is dim: a measurement works but is noisy.
    Dark,
    /// The light changes between frames or rows: shutter shorter than the projector's frame.
    Unsteady,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Clipped => "clipped",
            Verdict::Dark => "dark",
            Verdict::Unsteady => "unsteady",
        }
    }
}

impl Reading {
    pub fn verdict(&self) -> Verdict {
        if self.clipped > MAX_CLIPPED {
            Verdict::Clipped
        } else if self.flicker > UNSTEADY || self.banding > UNSTEADY {
            Verdict::Unsteady
        } else if self.peak < DARK {
            Verdict::Dark
        } else {
            Verdict::Ok
        }
    }

    /// Stops to change the exposure by (negative: darker) to bring the peak to [`TARGET`],
    /// taking the camera's encoding as sRGB. A clipped white says only that it must be darker:
    /// then at least one stop.
    pub fn stops(&self) -> f64 {
        let got = decode(self.peak);
        let want = decode(TARGET);
        let s = if got > 0.0 { (want / got).log2() } else { 0.0 };
        if self.verdict() == Verdict::Clipped { s.min(-1.0) } else { s }
    }

    /// What to do, in a sentence; `None` when nothing.
    pub fn advice(&self) -> Option<String> {
        let stops = self.stops();
        match self.verdict() {
            Verdict::Ok if stops.abs() < 0.75 => None,
            Verdict::Ok | Verdict::Dark => Some(format!(
                "{} the exposure by about {:.1} stop(s) for a brightest pixel near 200 of 255",
                if stops < 0.0 { "lower" } else { "raise" },
                stops.abs()
            )),
            Verdict::Clipped => Some(format!(
                "the camera clips {:.1} % of this white; lower the exposure by at least {:.0} stop(s) \
                 (shutter, aperture or ISO; not the projector)",
                self.clipped * 100.0,
                (-stops).ceil().max(1.0)
            )),
            Verdict::Unsteady => Some(format!(
                "the light changes {} ({:.1} %); set the shutter to the projector's frame time or a \
                 multiple (1/60 s at 60 Hz) or longer",
                if self.flicker > self.banding { "from frame to frame" } else { "from row to row" },
                self.flicker.max(self.banding) * 100.0
            )),
        }
    }
}

/// Reads `whites` (consecutive camera frames of one projector alone at white) against `black`
/// within `mask`. `None` when the pictures differ in size or the mask is empty.
pub fn read(whites: &[Picture], black: &Picture, mask: &[bool]) -> Option<Reading> {
    let first = whites.first()?;
    let (w, h) = (first.width, first.height);
    let n = w * h;
    let fits = |p: &Picture| p.width == w && p.height == h && p.rgb.len() == n;
    if !fits(black) || !whites.iter().all(fits) || mask.len() != n || count(mask) == 0 {
        return None;
    }
    let enc = |v: f32| encode(f64::from(v));
    let top = |p: &[f32; 3]| enc(p[0].max(p[1]).max(p[2]));

    let mut tops = Vec::new();
    let mut blacks = Vec::new();
    let mut clipped = 0usize;
    let mut channels = [false; 3];
    for (i, m) in mask.iter().enumerate() {
        if !*m {
            continue;
        }
        let (Some(p), Some(b)) = (first.rgb.get(i), black.rgb.get(i)) else { continue };
        tops.push(top(p));
        blacks.push(top(b));
        // A pixel clips when any frame clips it.
        let mut any = false;
        for f in whites {
            if let Some(q) = f.rgb.get(i) {
                for (c, ch) in channels.iter_mut().enumerate() {
                    if q.get(c).is_some_and(|v| enc(*v) >= CLIPPED) {
                        *ch = true;
                        any = true;
                    }
                }
            }
        }
        clipped += usize::from(any);
    }
    let pixels = tops.len();
    let peak = percentile(&mut tops, 0.99);
    let median = percentile(&mut tops, 0.5);
    let black_median = percentile(&mut blacks, 0.5);

    let bright = |p: &[f32; 3]| luminance([f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]);
    let means: Vec<f64> = whites
        .iter()
        .map(|f| {
            let (s, k) = f
                .rgb
                .iter()
                .zip(mask)
                .filter(|(_, m)| **m)
                .fold((0.0, 0usize), |(s, k), (p, _)| (s + bright(p), k + 1));
            if k > 0 { s / k as f64 } else { 0.0 }
        })
        .collect();
    let mean = means.iter().sum::<f64>() / means.len().max(1) as f64;
    let (lo, hi) = means.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    let flicker = if mean > 0.0 && means.len() > 1 { (hi - lo) / mean } else { 0.0 };

    let banding = bands(whites, mask, w, h, bright);

    Some(Reading {
        pixels,
        peak,
        median,
        black: black_median,
        clipped: clipped as f64 / pixels.max(1) as f64,
        clipped_channels: channels,
        flicker,
        banding,
    })
}

/// The mean brightness of each row of `f` within `mask` (`None` for rows with too little of it).
fn row_means(
    f: &Picture,
    mask: &[bool],
    w: usize,
    h: usize,
    bright: &impl Fn(&[f32; 3]) -> f64,
) -> Vec<Option<f64>> {
    (0..h)
        .map(|y| {
            let (mut s, mut k) = (0.0, 0usize);
            for x in 0..w {
                let i = y * w + x;
                if mask.get(i).copied().unwrap_or(false)
                    && let Some(p) = f.rgb.get(i)
                {
                    s += bright(p);
                    k += 1;
                }
            }
            (k >= 8).then(|| s / k as f64)
        })
        .collect()
}

/// How far each frame's row profile lies from the frames' mean profile: RMS relative to the
/// mean brightness. Bands a short shutter leaves move from frame to frame; the projector's own
/// falloff and the wall's shading stay, so they do not count.
fn bands(frames: &[Picture], mask: &[bool], w: usize, h: usize, bright: impl Fn(&[f32; 3]) -> f64) -> f64 {
    if frames.len() < 2 {
        return 0.0;
    }
    let profiles: Vec<Vec<Option<f64>>> = frames.iter().map(|f| row_means(f, mask, w, h, &bright)).collect();
    let (mut sq, mut k, mut total, mut rows) = (0.0, 0usize, 0.0, 0usize);
    for y in 0..h {
        let values: Vec<f64> = profiles.iter().filter_map(|p| p.get(y).copied().flatten()).collect();
        if values.len() != frames.len() {
            continue;
        }
        let avg = values.iter().sum::<f64>() / values.len() as f64;
        total += avg;
        rows += 1;
        for v in &values {
            sq += (v - avg) * (v - avg);
            k += 1;
        }
    }
    let mean = if rows > 0 { total / rows as f64 } else { 0.0 };
    if k == 0 || mean <= 0.0 { 0.0 } else { (sq / k as f64).sqrt() / mean }
}

/// The `q` quantile of `v` (sorted in place); 0 when empty.
fn percentile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    v.get(i).copied().unwrap_or(0.0)
}

#[cfg(test)]
#[path = "exposure_tests.rs"]
mod tests;
