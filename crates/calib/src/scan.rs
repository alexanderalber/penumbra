//! Structured light: the Gray-code stripe patterns a projector shows, and the decoding of what a
//! camera saw of them into a map between camera pixels and projector pixels.
//!
//! **Patterns** ([`Patterns`]), in this order: all white, all black, then one pattern per bit of
//! the column index (most significant first), each followed by its inverse, then the same for the
//! row index. A projector `w` pixels wide needs `ceil(log2 w)` column bits (11 for 1920 and for
//! 1080), so 1920 x 1080 takes 2 + 2 * (11 + 11) = 46 pictures. Column `x` is lit in the pattern
//! of bit `b` when bit `bits - 1 - b` of `x ^ (x >> 1)` is set. Neighbouring columns differ in one
//! bit only, so a camera pixel on a stripe edge that reads that bit either way lands on one of the
//! two columns beside the edge: an ambiguous bit costs at most a pixel.
//!
//! **Decoding** ([`Decoder`]): per camera pixel and bit, "pattern brighter than inverse" instead
//! of a threshold, so dark and bright surfaces and stray light decode alike. A pixel is kept when
//! the projector reaches it (white minus black at least [`Options::min_contrast`]), the white
//! picture is not clipped there (a mirror-like highlight shows another projector pixel than the
//! one lighting the surface, and clips), its bits are clear (the confidence below), the decoded
//! column and row lie on the projector, and at least two of its eight neighbours agree with it.
//! Brightness is the mean of red, green and blue, so a saturated blue surface keeps the contrast
//! a luma weighting would take away.
//!
//! **Confidence:** the margin of a bit is `|pattern - inverse| / (white - black)`, about 1 on a
//! clear stripe and 0 on its edge. Since only one bit per axis sits on an edge at any place, the
//! smallest margin per axis is ignored and the second smallest counts; the pixel's confidence is
//! the lower of the two axes. Bits the camera cannot resolve at all (finer stripes than its
//! pixels, the median margin over the reached pixels below [`RESOLVED_MARGIN`]) are still read,
//! but do not count towards the confidence; their noise widens the neighbour check instead.
//!
//! **The inverse** ([`Scan::inverse`]): every kept camera pixel is scattered to its projector
//! pixel (camera positions and colours averaged where several land on one), then the holes are
//! filled from their neighbours, ring by ring up to a distance: sampling gaps where the camera
//! sees the projector coarser than its pixels, and shadows the projector does not reach. The
//! colour is the white picture's: the scene as the projector sees it, so a mask drawn on it is
//! in projector pixels.

/// Largest projector side, in pixels.
pub const MAX_SIDE: u32 = 16384;
/// Most camera pixels decoded (4096 x 4096).
pub const MAX_CAMERA_PIXELS: usize = 1 << 24;
/// Median margin below which a bit counts as not resolved by the camera.
pub const RESOLVED_MARGIN: f32 = 0.3;
/// Largest hole fill, in projector pixels.
pub const MAX_FILL: u32 = 1024;
/// [`Options::window`]: its default and largest value, camera pixels.
pub const DEFAULT_WINDOW: u32 = 7;
pub const MAX_WINDOW: u32 = 256;
/// A window holding fewer reached pixels than this share of its area judges by the whole picture.
const WINDOW_SHARE: f64 = 0.25;
/// Most bits per axis a window may resolve fewer than the whole picture and still judge alone.
const MAX_DROP: u32 = 2;

/// The bits of `slots` as a mask (slots past 31 left out).
fn mask_of(slots: std::ops::Range<usize>) -> u32 {
    slots.filter(|s| *s < 32).fold(0, |m, s| m | 1 << s)
}

/// Pattern minus inverse is kept as `i16` in units of `1 / DIFF_SCALE`, clamped to about +-4.
const DIFF_SCALE: f32 = 8192.0;
/// Bins of the margin histograms (margins 0 to 1).
const BINS: usize = 64;

/// Bits needed to number `side` pixels (at least 1).
pub fn bits(side: u32) -> u32 {
    if side <= 1 { 1 } else { 32 - (side - 1).leading_zeros() }
}

/// The Gray code of `v`.
pub fn gray(v: u32) -> u32 {
    v ^ (v >> 1)
}

/// The value whose Gray code is `g`.
pub fn from_gray(g: u32) -> u32 {
    let mut v = g;
    let mut s = g >> 1;
    while s != 0 {
        v ^= s;
        s >>= 1;
    }
    v
}

/// One picture of a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    White,
    Black,
    /// Stripes of a column bit; `bit` 0 is the most significant (the coarsest stripes).
    Columns {
        bit: u32,
        inverse: bool,
    },
    /// Stripes of a row bit; `bit` 0 is the most significant.
    Rows {
        bit: u32,
        inverse: bool,
    },
}

impl Pattern {
    /// `white`, `black`, `column 3`, `row 0 inverse`.
    pub fn name(&self) -> String {
        let inv = |i: bool| if i { " inverse" } else { "" };
        match self {
            Pattern::White => "white".into(),
            Pattern::Black => "black".into(),
            Pattern::Columns { bit, inverse } => format!("column {bit}{}", inv(*inverse)),
            Pattern::Rows { bit, inverse } => format!("row {bit}{}", inv(*inverse)),
        }
    }
}

/// The patterns of a scan for a projector of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patterns {
    width: u32,
    height: u32,
}

impl Patterns {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return Err(format!("a projector of {width} x {height}: each side is 1 to {MAX_SIDE} pixels"));
        }
        Ok(Self { width, height })
    }

    /// The projector's size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn column_bits(&self) -> u32 {
        bits(self.width)
    }

    pub fn row_bits(&self) -> u32 {
        bits(self.height)
    }

    /// Pictures in a scan: white, black, and a pattern and its inverse per bit.
    pub fn count(&self) -> usize {
        2 + 2 * (self.column_bits() + self.row_bits()) as usize
    }

    /// The pattern shown as picture `index` (0 is white), `None` past the last.
    pub fn get(&self, index: usize) -> Option<Pattern> {
        let (cb, rb) = (self.column_bits() as usize, self.row_bits() as usize);
        match index {
            0 => Some(Pattern::White),
            1 => Some(Pattern::Black),
            _ => {
                let k = index - 2;
                let (bit, inverse) = (k / 2, k % 2 == 1);
                if bit < cb {
                    Some(Pattern::Columns { bit: bit as u32, inverse })
                } else if bit < cb + rb {
                    Some(Pattern::Rows { bit: (bit - cb) as u32, inverse })
                } else {
                    None
                }
            }
        }
    }

    /// The position of `pattern` in the scan, `None` for a bit the projector does not have.
    pub fn index(&self, pattern: Pattern) -> Option<usize> {
        let (cb, rb) = (self.column_bits(), self.row_bits());
        match pattern {
            Pattern::White => Some(0),
            Pattern::Black => Some(1),
            Pattern::Columns { bit, inverse } if bit < cb => {
                Some(2 + 2 * bit as usize + usize::from(inverse))
            }
            Pattern::Rows { bit, inverse } if bit < rb => {
                Some(2 + 2 * (cb + bit) as usize + usize::from(inverse))
            }
            _ => None,
        }
    }

    /// Whether projector pixel `(x, y)` is lit in `pattern`.
    pub fn lit(&self, pattern: Pattern, x: u32, y: u32) -> bool {
        let stripe = |v: u32, bits: u32, bit: u32| {
            bits.checked_sub(1 + bit).is_some_and(|shift| (gray(v) >> shift) & 1 == 1)
        };
        match pattern {
            Pattern::White => true,
            Pattern::Black => false,
            Pattern::Columns { bit, inverse } => stripe(x, self.column_bits(), bit) != inverse,
            Pattern::Rows { bit, inverse } => stripe(y, self.row_bits(), bit) != inverse,
        }
    }
}

/// The thresholds of [`Decoder::finish`].
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Least white minus black (linear, 0 to 1) at which the projector counts as reaching a pixel.
    pub min_contrast: f32,
    /// White (mean of red, green and blue) from which a pixel counts as clipped.
    pub saturation: f32,
    /// Least confidence (0 to 1) of a kept pixel.
    pub min_confidence: f32,
    /// Projector pixels by which a neighbour may differ beyond the expected step and still agree.
    pub tolerance: f32,
    /// Which bits count towards a pixel's confidence is judged over this many camera pixels
    /// around it (a square of `2 window + 1`); 0 judges over the whole picture.
    pub window: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_contrast: 0.02,
            saturation: 0.98,
            min_confidence: 0.2,
            tolerance: 3.0,
            window: DEFAULT_WINDOW,
        }
    }
}

impl Options {
    /// Whether every threshold is in its range.
    pub fn check(&self) -> Result<(), String> {
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !ok(self.min_contrast, 0.0, 1.0) {
            return Err("`min_contrast` is 0 to 1".into());
        }
        if !ok(self.saturation, 0.0, 1000.0) {
            return Err("`saturation` is 0 or more".into());
        }
        if !ok(self.min_confidence, 0.0, 1.0) {
            return Err("`min_confidence` is 0 to 1".into());
        }
        if !ok(self.tolerance, 0.0, 1e6) {
            return Err("`tolerance` is 0 or more projector pixels".into());
        }
        if self.window > MAX_WINDOW {
            return Err(format!("`window` is 0 to {MAX_WINDOW} camera pixels"));
        }
        Ok(())
    }
}

/// Why a camera pixel has a projector position or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Valid,
    /// White minus black below the contrast: the projector does not reach it (or a dark surface).
    Dark,
    /// The white picture clipped there (a highlight, or exposed too bright).
    Clipped,
    /// A bit too close to call.
    Ambiguous,
    /// A column or row past the projector's edge (the codes run up to a power of two).
    Outside,
    /// Fewer than two neighbours agree with it.
    Inconsistent,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Valid,
        Status::Dark,
        Status::Clipped,
        Status::Ambiguous,
        Status::Outside,
        Status::Inconsistent,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Status::Valid => "valid",
            Status::Dark => "dark",
            Status::Clipped => "clipped",
            Status::Ambiguous => "ambiguous",
            Status::Outside => "outside",
            Status::Inconsistent => "inconsistent",
        }
    }
}

/// Collects the camera pictures of a scan, one at a time and in any order, and decodes them.
///
/// Holds the white picture's colour, the black picture's brightness, and per bit pattern minus
/// inverse (two bytes a pixel); a pattern waits for its inverse as one brightness picture. Adding
/// a pattern again starts its pair over (a bit captured too early is captured again).
pub struct Decoder {
    patterns: Patterns,
    width: u32,
    height: u32,
    white: Option<Vec<[f32; 3]>>,
    black: Option<Vec<f32>>,
    /// Per bit (columns, then rows): the first of pattern and inverse until the other arrives,
    /// with whether it was the inverse.
    pending: Vec<Option<(bool, Vec<f32>)>>,
    /// Per bit: pattern minus inverse.
    diffs: Vec<Option<Vec<i16>>>,
}

impl Decoder {
    /// A decoder for a camera of `width` x `height` pixels watching `patterns`.
    pub fn new(patterns: Patterns, width: u32, height: u32) -> Result<Self, String> {
        let n = (width as usize).saturating_mul(height as usize);
        if n == 0 || n > MAX_CAMERA_PIXELS {
            return Err(format!("a camera of {width} x {height}: 1 to {MAX_CAMERA_PIXELS} pixels"));
        }
        let slots = (patterns.column_bits() + patterns.row_bits()) as usize;
        Ok(Self {
            patterns,
            width,
            height,
            white: None,
            black: None,
            pending: vec![None; slots],
            diffs: vec![None; slots],
        })
    }

    pub fn patterns(&self) -> Patterns {
        self.patterns
    }

    /// The camera's size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Adds the camera picture of pattern `index`: linear RGBA (alpha ignored), rows top to
    /// bottom, the camera's size. Non-finite values count as 0.
    pub fn add(&mut self, index: usize, pixels: &[[f32; 4]]) -> Result<(), String> {
        let n = self.width as usize * self.height as usize;
        if pixels.len() != n {
            return Err(format!(
                "picture {index} has {} pixels, the camera {} x {} has {n}",
                pixels.len(),
                self.width,
                self.height
            ));
        }
        let pattern = self
            .patterns
            .get(index)
            .ok_or_else(|| format!("picture {index}: a scan has {} pictures", self.patterns.count()))?;
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let (slot, inverse) = match pattern {
            Pattern::White => {
                self.white =
                    Some(pixels.iter().map(|&[r, g, b, _]| [finite(r), finite(g), finite(b)]).collect());
                return Ok(());
            }
            Pattern::Black => {
                self.black = Some(pixels.iter().map(brightness).collect());
                return Ok(());
            }
            Pattern::Columns { bit, inverse } => (bit as usize, inverse),
            Pattern::Rows { bit, inverse } => ((self.patterns.column_bits() + bit) as usize, inverse),
        };
        let this: Vec<f32> = pixels.iter().map(brightness).collect();
        let (Some(pending), Some(diff)) = (self.pending.get_mut(slot), self.diffs.get_mut(slot)) else {
            return Err(format!("picture {index}: no such bit"));
        };
        match pending.take() {
            Some((other_inverse, other)) if other_inverse != inverse => {
                let (p, q) = if inverse { (&other, &this) } else { (&this, &other) };
                *diff = Some(
                    p.iter()
                        .zip(q)
                        .map(|(a, b)| ((a - b) * DIFF_SCALE).clamp(-32767.0, 32767.0).round() as i16)
                        .collect(),
                );
            }
            _ => {
                *diff = None;
                *pending = Some((inverse, this));
            }
        }
        Ok(())
    }

    /// The pictures still missing, by index.
    pub fn missing(&self) -> Vec<usize> {
        let mut out = Vec::new();
        if self.white.is_none() {
            out.push(0);
        }
        if self.black.is_none() {
            out.push(1);
        }
        for (slot, diff) in self.diffs.iter().enumerate() {
            if diff.is_some() {
                continue;
            }
            let have = self.pending.get(slot).and_then(|p| p.as_ref()).map(|(inv, _)| *inv);
            for inverse in [false, true] {
                if have == Some(inverse) {
                    continue;
                }
                let cb = self.patterns.column_bits();
                let s = slot as u32;
                let pattern = if s < cb {
                    Pattern::Columns { bit: s, inverse }
                } else {
                    Pattern::Rows { bit: s - cb, inverse }
                };
                out.extend(self.patterns.index(pattern));
            }
        }
        out
    }

    /// Decodes the pictures added so far; every one must be there.
    pub fn finish(&self, options: &Options) -> Result<Scan, String> {
        options.check()?;
        let missing = self.missing();
        if !missing.is_empty() {
            let names: Vec<String> = missing
                .iter()
                .take(8)
                .filter_map(|i| self.patterns.get(*i).map(|p| format!("{i} ({})", p.name())))
                .collect();
            let more = if missing.len() > 8 { ", ..." } else { "" };
            return Err(format!("{} picture(s) missing: {}{more}", missing.len(), names.join(", ")));
        }
        let (Some(white), Some(black)) = (&self.white, &self.black) else {
            return Err("white or black missing".into());
        };
        let diffs: Vec<&Vec<i16>> = self.diffs.iter().flatten().collect();
        let n = white.len();
        let cb = self.patterns.column_bits() as usize;
        let (pw, ph) = self.patterns.size();

        // Reached and not clipped: where the margins are measured.
        let contrast: Vec<f32> =
            white.iter().zip(black).map(|(w, b)| (w[0] + w[1] + w[2]) / 3.0 - b).collect();
        let lit = |i: usize| -> Status {
            let c = contrast.get(i).copied().unwrap_or(0.0);
            let w = white.get(i).map_or(0.0, |w| (w[0] + w[1] + w[2]) / 3.0);
            if c.is_nan() || c < options.min_contrast.max(1e-6) {
                Status::Dark
            } else if w >= options.saturation {
                Status::Clipped
            } else {
                Status::Valid
            }
        };
        let lit_status: Vec<Status> = (0..n).map(lit).collect();

        // Which bits the camera resolves: the median margin over the reached pixels.
        let resolved: Vec<bool> = diffs
            .iter()
            .map(|d| {
                let mut hist = [0u64; BINS];
                let mut total = 0u64;
                for ((v, c), s) in d.iter().zip(&contrast).zip(&lit_status) {
                    if *s != Status::Valid {
                        continue;
                    }
                    let m = (f32::from(*v) / DIFF_SCALE / c).abs().min(1.0);
                    let bin = ((m * BINS as f32) as usize).min(BINS - 1);
                    if let Some(h) = hist.get_mut(bin) {
                        *h += 1;
                    }
                    total += 1;
                }
                if total == 0 {
                    return true;
                }
                let mut seen = 0u64;
                for (b, h) in hist.iter().enumerate() {
                    seen += h;
                    if seen * 2 >= total {
                        return (b as f32 + 0.5) / BINS as f32 >= RESOLVED_MARGIN;
                    }
                }
                true
            })
            .collect();
        let resolved_axis = |range: std::ops::Range<usize>| -> u32 {
            resolved.get(range).map_or(0, |r| r.iter().filter(|x| **x).count() as u32)
        };
        let resolved_count = [resolved_axis(0..cb), resolved_axis(cb..diffs.len())];
        let counts_mask = self.counting_bits(&diffs, &contrast, &lit_status, &resolved, options.window);

        // Per pixel: the code of both axes and the confidence.
        let mut status = lit_status;
        let mut points: Vec<Option<[f32; 2]>> = vec![None; n];
        let mut confidence = vec![0.0f32; n];
        for (i, ((st, pt), conf)) in
            status.iter_mut().zip(points.iter_mut()).zip(confidence.iter_mut()).enumerate()
        {
            if *st != Status::Valid {
                continue;
            }
            let c = contrast.get(i).copied().unwrap_or(1.0);
            let counts = counts_mask.get(i).copied().unwrap_or(0);
            let axis = |slots: std::ops::Range<usize>| -> (u32, f32) {
                let mut g = 0u32;
                let (mut lo1, mut lo2) = (f32::INFINITY, f32::INFINITY);
                let mut counted = 0;
                for s in slots {
                    let d = diffs.get(s).and_then(|d| d.get(i)).copied().unwrap_or(0);
                    g = (g << 1) | u32::from(d > 0);
                    if counts >> s & 1 == 1 {
                        let m = (f32::from(d) / DIFF_SCALE / c).abs();
                        counted += 1;
                        if m < lo1 {
                            lo2 = lo1;
                            lo1 = m;
                        } else if m < lo2 {
                            lo2 = m;
                        }
                    }
                }
                let conf = match counted {
                    0 => 1.0,
                    1 => lo1,
                    _ => lo2,
                };
                (from_gray(g), conf.min(1.0))
            };
            let (x, cx) = axis(0..cb);
            let (y, cy) = axis(cb..diffs.len());
            *conf = cx.min(cy);
            if *conf < options.min_confidence {
                *st = Status::Ambiguous;
            } else if x >= pw || y >= ph {
                *st = Status::Outside;
            } else {
                *pt = Some([x as f32 + 0.5, y as f32 + 0.5]);
            }
        }

        // The expected step between neighbouring camera pixels, in projector pixels.
        let w = self.width as usize;
        let h = self.height as usize;
        let step = {
            let mut hist = vec![0u64; 257];
            let mut total = 0u64;
            for (i, p) in points.iter().enumerate() {
                let (Some(p), true) = (p, (i % w) + 1 < w) else { continue };
                let Some(Some(q)) = points.get(i + 1) else { continue };
                let d = (p[0] - q[0]).abs().max((p[1] - q[1]).abs());
                let bin = ((d * 4.0) as usize).min(256);
                if let Some(b) = hist.get_mut(bin) {
                    *b += 1;
                }
                total += 1;
            }
            let mut seen = 0u64;
            let mut median = 0.0;
            for (b, c) in hist.iter().enumerate() {
                seen += c;
                if total > 0 && seen * 2 >= total {
                    median = b as f32 / 4.0;
                    break;
                }
            }
            median
        };
        let unresolved_span = |bits: usize, count: u32| -> f32 {
            let unresolved = (bits as u32).saturating_sub(count).min(16);
            ((1u32 << unresolved) - 1) as f32
        };
        let all = diffs.len();
        let tol = |i: usize| -> [f32; 2] {
            let m = counts_mask.get(i).copied().unwrap_or(0);
            let counted = |r: std::ops::Range<usize>| r.filter(|s| m >> s & 1 == 1).count() as u32;
            [
                options.tolerance + 2.0 * step + unresolved_span(cb, counted(0..cb)),
                options.tolerance + 2.0 * step + unresolved_span(all - cb, counted(cb..all)),
            ]
        };

        // Neighbours that agree: at least two of eight, or the pixel is an outlier.
        let before = points.clone();
        for (i, (pt, st)) in points.iter_mut().zip(status.iter_mut()).enumerate() {
            let Some(p) = *pt else { continue };
            let t = tol(i);
            let agree = neighbours(i, w, h)
                .filter(|j| {
                    let u = tol(*j);
                    let (tx, ty) = (t[0].max(u[0]), t[1].max(u[1]));
                    before
                        .get(*j)
                        .copied()
                        .flatten()
                        .is_some_and(|q| (p[0] - q[0]).abs() <= tx && (p[1] - q[1]).abs() <= ty)
                })
                .count();
            if agree < 2 {
                *pt = None;
                *st = Status::Inconsistent;
            }
        }
        for (c, p) in confidence.iter_mut().zip(&points) {
            if p.is_none() {
                *c = 0.0;
            }
        }

        let mut counts = [0usize; 6];
        for s in &status {
            if let Some(k) = Status::ALL.iter().position(|a| a == s)
                && let Some(c) = counts.get_mut(k)
            {
                *c += 1;
            }
        }
        Ok(Scan {
            camera: (self.width, self.height),
            projector: (pw, ph),
            points,
            confidence,
            status,
            color: white.clone(),
            stats: Stats {
                counts,
                bits: [self.patterns.column_bits(), self.patterns.row_bits()],
                resolved: resolved_count,
                step,
            },
        })
    }
}

impl Decoder {
    /// Per camera pixel the bits (bit `s` for slot `s`, columns first) that count towards its
    /// confidence. With `window` 0 the bits the camera resolves over the whole picture
    /// (`resolved`). Otherwise per pixel the bits whose mean margin over the reached pixels in
    /// the window is at least [`RESOLVED_MARGIN`]: a surface the camera or the projector sees at
    /// a grazing angle squeezes the stripes, so the finest bits the rest of the picture resolves
    /// are noise there, and light the walls around throw back flattens a coarse one; counting
    /// those would call most of it ambiguous. An axis that resolves more than [`MAX_DROP`] bits
    /// fewer than the whole picture, and a window with too few reached pixels, fall back to
    /// `resolved`: light that comes not from the projector but bounced off another surface (a
    /// shadow lit by the object casting it) carries that surface's coarse bits only, and must
    /// stay ambiguous.
    fn counting_bits(
        &self,
        diffs: &[&Vec<i16>],
        contrast: &[f32],
        lit: &[Status],
        resolved: &[bool],
        window: u32,
    ) -> Vec<u32> {
        let (w, h) = (self.width as usize, self.height as usize);
        let n = w * h;
        let global: u32 =
            resolved.iter().enumerate().filter(|(s, r)| **r && *s < 32).fold(0, |m, (s, _)| m | 1 << s);
        if window == 0 || diffs.len() > 32 {
            return vec![global; n];
        }
        let r = window as usize;
        let reached: Vec<bool> = lit.iter().map(|s| *s == Status::Valid).collect();
        let count = box_sums(w, h, r, |i| f64::from(u8::from(reached.get(i).copied().unwrap_or(false))));
        let side = (2 * r + 1) as f64;
        let enough: Vec<bool> = count.iter().map(|c| *c >= WINDOW_SHARE * side * side).collect();
        let mut local = vec![0u32; n];
        for (s, d) in diffs.iter().enumerate() {
            let sums = box_sums(w, h, r, |i| {
                if !reached.get(i).copied().unwrap_or(false) {
                    return 0.0;
                }
                let c = contrast.get(i).copied().unwrap_or(1.0).max(1e-6);
                f64::from((f32::from(d.get(i).copied().unwrap_or(0)) / DIFF_SCALE / c).abs().min(1.0))
            });
            for (i, m) in local.iter_mut().enumerate() {
                let (Some(sum), Some(c), Some(ok)) = (sums.get(i), count.get(i), enough.get(i)) else {
                    continue;
                };
                let on = if *ok { sum / c >= f64::from(RESOLVED_MARGIN) } else { global >> s & 1 == 1 };
                if on {
                    *m |= 1 << s;
                }
            }
        }
        let cb = self.patterns.column_bits() as usize;
        let all = diffs.len();
        let axes = [mask_of(0..cb), mask_of(cb..all)];
        for m in &mut local {
            let mut keep = 0u32;
            for axis in axes {
                let here = (*m & axis).count_ones();
                keep |=
                    if here + MAX_DROP >= (global & axis).count_ones() { *m & axis } else { global & axis };
            }
            *m = keep;
        }
        local
    }
}

/// Per pixel of a `w` x `h` picture the sum of `value` over the square of radius `r` around it
/// (clipped at the picture's edges), by a summed-area table.
fn box_sums(w: usize, h: usize, r: usize, value: impl Fn(usize) -> f64) -> Vec<f64> {
    let (tw, th) = (w + 1, h + 1);
    let mut table = vec![0.0f64; tw * th];
    for y in 0..h {
        let mut row = 0.0;
        for x in 0..w {
            row += value(y * w + x);
            let above = table.get(y * tw + x + 1).copied().unwrap_or(0.0);
            if let Some(t) = table.get_mut((y + 1) * tw + x + 1) {
                *t = above + row;
            }
        }
    }
    let at = |x: usize, y: usize| table.get(y * tw + x).copied().unwrap_or(0.0);
    let mut out = vec![0.0f64; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            if let Some(o) = out.get_mut(y * w + x) {
                *o = at(x1, y1) - at(x0, y1) - at(x1, y0) + at(x0, y0);
            }
        }
    }
    out
}

/// Brightness of a camera pixel: the mean of red, green and blue.
fn brightness(p: &[f32; 4]) -> f32 {
    let [r, g, b, _] = *p;
    let v = (r + g + b) / 3.0;
    if v.is_finite() { v } else { 0.0 }
}

/// The up to eight neighbours of pixel `i` in a `w` x `h` picture.
fn neighbours(i: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let (x, y) = ((i % w.max(1)) as isize, (i / w.max(1)) as isize);
    const OFFSETS: [(isize, isize); 8] =
        [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
    OFFSETS.into_iter().filter_map(move |(dx, dy)| {
        let (nx, ny) = (x + dx, y + dy);
        (nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h).then(|| ny as usize * w + nx as usize)
    })
}

/// Counts of a decode.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Camera pixels per [`Status`], in the order of [`Status::ALL`].
    pub counts: [usize; 6],
    /// Column and row bits of the scan.
    pub bits: [u32; 2],
    /// Of those, the bits the camera resolves.
    pub resolved: [u32; 2],
    /// Median step between neighbouring camera pixels, in projector pixels.
    pub step: f32,
}

impl Stats {
    pub fn count(&self, status: Status) -> usize {
        Status::ALL.iter().position(|s| *s == status).and_then(|k| self.counts.get(k)).copied().unwrap_or(0)
    }
}

/// A decoded scan, in camera space.
#[derive(Clone, Debug)]
pub struct Scan {
    /// Camera size.
    pub camera: (u32, u32),
    /// Projector size.
    pub projector: (u32, u32),
    /// Per camera pixel, rows top to bottom: the centre of the projector pixel it sees, in
    /// projector pixels; `None` where not valid.
    pub points: Vec<Option<[f32; 2]>>,
    /// Per camera pixel: 0 where not valid, else the confidence (0 to 1).
    pub confidence: Vec<f32>,
    pub status: Vec<Status>,
    /// The white picture: the scene lit by the projector, linear RGB.
    pub color: Vec<[f32; 3]>,
    pub stats: Stats,
}

/// What a projector pixel of an [`Inverse`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    /// Not seen, and too far from anything seen.
    Empty,
    /// From its neighbours.
    Filled,
    /// Seen by the camera.
    Measured,
}

/// A scan in projector space.
#[derive(Clone, Debug)]
pub struct Inverse {
    /// Projector size.
    pub size: (u32, u32),
    /// Camera size.
    pub camera: (u32, u32),
    /// Per projector pixel: where the camera sees it, in camera pixels.
    pub points: Vec<Option<[f32; 2]>>,
    /// Per projector pixel: the camera's colour there (linear RGB, black where empty).
    pub color: Vec<[f32; 3]>,
    pub samples: Vec<Sample>,
}

impl Scan {
    /// A map for `tex.remap` in camera space: red and green the projector position (0 to 1, the
    /// pixel's centre), blue the confidence, alpha 1; -1, -1 where not valid, so an `extend` of
    /// `zero` shows nothing there.
    pub fn camera_map(&self) -> Vec<[f32; 4]> {
        let (pw, ph) = (self.projector.0 as f32, self.projector.1 as f32);
        self.points
            .iter()
            .zip(&self.confidence)
            .map(|(p, c)| match p {
                Some([x, y]) => [x / pw, y / ph, *c, 1.0],
                None => [-1.0, -1.0, 0.0, 1.0],
            })
            .collect()
    }

    /// A picture of the decode for checking it: valid pixels grey by confidence, the projector
    /// not reaching black, clipped magenta, ambiguous blue, outside yellow, outliers red. Linear
    /// RGBA, camera size.
    pub fn quality(&self) -> Vec<[f32; 4]> {
        self.status
            .iter()
            .zip(&self.confidence)
            .map(|(s, c)| match s {
                Status::Valid => {
                    let v = 0.05 + 0.95 * c.clamp(0.0, 1.0);
                    [v, v, v, 1.0]
                }
                Status::Dark => [0.0, 0.0, 0.0, 1.0],
                Status::Clipped => [1.0, 0.0, 1.0, 1.0],
                Status::Ambiguous => [0.0, 0.15, 1.0, 1.0],
                Status::Outside => [1.0, 0.85, 0.0, 1.0],
                Status::Inconsistent => [1.0, 0.0, 0.0, 1.0],
            })
            .collect()
    }

    /// The scan in projector space: valid camera pixels scattered to their projector pixels,
    /// holes filled from their neighbours up to `fill` projector pixels away (at most
    /// [`MAX_FILL`]).
    pub fn inverse(&self, fill: u32) -> Inverse {
        let (pw, ph) = (self.projector.0 as usize, self.projector.1 as usize);
        let n = pw * ph;
        let cw = self.camera.0 as usize;
        let mut sum = vec![[0.0f32; 5]; n];
        let mut count = vec![0u32; n];
        for (i, (p, c)) in self.points.iter().zip(&self.color).enumerate() {
            let Some([x, y]) = p else { continue };
            let (px, py) = (*x as usize, *y as usize);
            if px >= pw || py >= ph {
                continue;
            }
            let j = py * pw + px;
            let cam = [(i % cw.max(1)) as f32 + 0.5, (i / cw.max(1)) as f32 + 0.5];
            if let (Some(s), Some(k)) = (sum.get_mut(j), count.get_mut(j)) {
                for (a, v) in s.iter_mut().zip([cam[0], cam[1], c[0], c[1], c[2]]) {
                    *a += v;
                }
                *k += 1;
            }
        }
        // Distance ring per pixel: 0 measured, u32::MAX not reached yet.
        let mut ring: Vec<u32> = count.iter().map(|k| if *k > 0 { 0 } else { u32::MAX }).collect();
        let mut value: Vec<Option<[f32; 5]>> =
            sum.iter().zip(&count).map(|(s, k)| (*k > 0).then(|| s.map(|v| v / *k as f32))).collect();
        let mut frontier: Vec<usize> = (0..n).filter(|i| ring.get(*i) == Some(&0)).collect();
        for d in 1..=fill.min(MAX_FILL) {
            let mut next = Vec::new();
            for &i in &frontier {
                for j in neighbours(i, pw, ph) {
                    if let Some(r) = ring.get_mut(j)
                        && *r == u32::MAX
                    {
                        *r = d;
                        next.push(j);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            for &j in &next {
                let mut acc = [0.0f32; 5];
                let mut k = 0u32;
                for q in neighbours(j, pw, ph) {
                    if ring.get(q).is_some_and(|r| *r < d)
                        && let Some(Some(v)) = value.get(q)
                    {
                        for (a, b) in acc.iter_mut().zip(v) {
                            *a += b;
                        }
                        k += 1;
                    }
                }
                if k > 0
                    && let Some(slot) = value.get_mut(j)
                {
                    *slot = Some(acc.map(|v| v / k as f32));
                }
            }
            frontier = next;
        }
        let samples = ring
            .iter()
            .map(|r| match *r {
                0 => Sample::Measured,
                u32::MAX => Sample::Empty,
                _ => Sample::Filled,
            })
            .collect();
        Inverse {
            size: self.projector,
            camera: self.camera,
            points: value.iter().map(|v| v.map(|v| [v[0], v[1]])).collect(),
            color: value.iter().map(|v| v.map_or([0.0; 3], |v| [v[2], v[3], v[4]])).collect(),
            samples,
        }
    }
}

impl Inverse {
    /// A map for `tex.remap` in projector space: red and green the camera position (0 to 1),
    /// blue 1 where measured, 0.5 where filled, 0 where empty, alpha 1; -1, -1 where empty.
    /// Remapping the camera's picture through it shows the camera's view as the projector sees it.
    pub fn projector_map(&self) -> Vec<[f32; 4]> {
        let (cw, ch) = (self.camera.0 as f32, self.camera.1 as f32);
        self.points
            .iter()
            .zip(&self.samples)
            .map(|(p, s)| match (p, s) {
                (Some([x, y]), Sample::Measured) => [x / cw, y / ch, 1.0, 1.0],
                (Some([x, y]), Sample::Filled) => [x / cw, y / ch, 0.5, 1.0],
                _ => [-1.0, -1.0, 0.0, 1.0],
            })
            .collect()
    }

    /// The scene as the projector sees it: linear premultiplied RGBA, transparent where empty.
    pub fn scene(&self) -> Vec<[f32; 4]> {
        self.color
            .iter()
            .zip(&self.samples)
            .map(|(c, s)| match s {
                Sample::Empty => [0.0; 4],
                _ => [c[0], c[1], c[2], 1.0],
            })
            .collect()
    }

    /// Projector pixels measured, filled and empty.
    pub fn counts(&self) -> [usize; 3] {
        let mut out = [0usize; 3];
        for s in &self.samples {
            let k = match s {
                Sample::Measured => 0,
                Sample::Filled => 1,
                Sample::Empty => 2,
            };
            if let Some(c) = out.get_mut(k) {
                *c += 1;
            }
        }
        out
    }
}
