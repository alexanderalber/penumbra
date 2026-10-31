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
//! **Gray code plus phase** ([`Coding::GrayPhase`]): the Gray code stops at stripes half a
//! `period` wide, and `steps` sines of `period` projector pixels follow per axis (columns, then
//! rows), each shifted by `1 / steps` of a period. The Gray code says which quarter period a
//! camera pixel sees; the sines' phase says where in its period, to a fraction of a pixel. A
//! camera that sees the projector coarser than its pixels loses the finest Gray stripes, but it
//! still sees sines of 16 pixels clearly. The phase is unwrapped to the period nearest the Gray
//! code's quarter, so a Gray bit read wrong on its own stripe edge costs nothing. The sines are
//! light: the projector's response bends them (its gamma), which [`Patterns::light`] leaves to
//! whoever turns light into the signal. 1920 x 1080 with periods of 16 and 8 steps takes
//! 2 + 2 * (9 + 9) + 2 * 8 = 54 pictures.
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
//!
//! **Levels** ([`fuse`]): no single brightness suits a scene with white and dark surfaces, since
//! the camera clips on the one and loses the other in its noise. Scans at two or more levels of
//! the projector's light merge per camera pixel, the surest decode winning.

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
/// Default of [`Options::epipolar`], pixels. Measured on real scans of crates and a pumpkin: 99 %
/// of the decoded pixels lie within 2.1 to 2.7 px of the decode's own geometry, 0.1 to 1.1 %
/// beyond 4 px; pixels decoded from light thrown back by the walls (not from the projector)
/// lie a median 50 to 200 px off.
pub const DEFAULT_EPIPOLAR: f32 = 4.0;
/// Fewest decoded pixels the geometry check runs on, and the least share of them its fit must
/// hold to be trusted (else nothing is dropped).
const GEOMETRY_MIN: usize = 2000;
const GEOMETRY_TRUST: f64 = 0.2;
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

/// The periods of [`Coding::GrayPhase`]'s sines (projector pixels): powers of two, so the Gray
/// code's cells fit them.
pub const PHASE_PERIODS: [u32; 4] = [8, 16, 32, 64];
pub const DEFAULT_PERIOD: u32 = 16;
/// The fewest and most sines per axis of [`Coding::GrayPhase`].
pub const MIN_STEPS: u32 = 3;
pub const MAX_STEPS: u32 = 16;
pub const DEFAULT_STEPS: u32 = 8;

/// How a scan tells the projector's pixels apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Coding {
    /// Gray code down to stripes two projector pixels wide.
    #[default]
    Gray,
    /// Gray code down to stripes half `period` wide, then `steps` shifted sines of `period`
    /// projector pixels per axis.
    GrayPhase { period: u32, steps: u32 },
}

impl Coding {
    /// Gray code plus 8 sines of 16 pixels per axis.
    pub fn gray_phase() -> Coding {
        Coding::GrayPhase { period: DEFAULT_PERIOD, steps: DEFAULT_STEPS }
    }

    /// `gray` or `gray_phase`.
    pub fn name(&self) -> &'static str {
        match self {
            Coding::Gray => "gray",
            Coding::GrayPhase { .. } => "gray_phase",
        }
    }

    /// Whether the period and the steps are in their ranges.
    pub fn check(&self) -> Result<(), String> {
        match *self {
            Coding::Gray => Ok(()),
            Coding::GrayPhase { period, steps } => {
                if !PHASE_PERIODS.contains(&period) {
                    return Err(format!("the sines' `period` is one of {PHASE_PERIODS:?} projector pixels"));
                }
                if !(MIN_STEPS..=MAX_STEPS).contains(&steps) {
                    return Err(format!("`steps` is {MIN_STEPS} to {MAX_STEPS} sines per axis"));
                }
                Ok(())
            }
        }
    }

    /// The finest Gray bits per axis the sines stand in for.
    pub fn dropped_bits(&self) -> u32 {
        match self {
            Coding::Gray => 0,
            Coding::GrayPhase { period, .. } => (period / 4).max(1).trailing_zeros(),
        }
    }

    /// Sines per axis.
    pub fn steps(&self) -> u32 {
        match self {
            Coding::Gray => 0,
            Coding::GrayPhase { steps, .. } => *steps,
        }
    }
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
    /// Sine `step` across the columns ([`Coding::GrayPhase`]).
    ColumnPhase {
        step: u32,
    },
    /// Sine `step` down the rows.
    RowPhase {
        step: u32,
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
            Pattern::ColumnPhase { step } => format!("column sine {step}"),
            Pattern::RowPhase { step } => format!("row sine {step}"),
        }
    }
}

/// The patterns of a scan for a projector of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patterns {
    width: u32,
    height: u32,
    coding: Coding,
}

impl Patterns {
    /// Plain Gray code.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        Self::with_coding(width, height, Coding::Gray)
    }

    pub fn with_coding(width: u32, height: u32, coding: Coding) -> Result<Self, String> {
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return Err(format!("a projector of {width} x {height}: each side is 1 to {MAX_SIDE} pixels"));
        }
        coding.check()?;
        let dropped = coding.dropped_bits();
        if bits(width) <= dropped || bits(height) <= dropped {
            return Err(format!(
                "a projector of {width} x {height} is too small for sines of {} pixels",
                4u32 << dropped
            ));
        }
        Ok(Self { width, height, coding })
    }

    /// The projector's size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn coding(&self) -> Coding {
        self.coding
    }

    /// Gray bits shown across the columns.
    pub fn column_bits(&self) -> u32 {
        bits(self.width).saturating_sub(self.coding.dropped_bits())
    }

    pub fn row_bits(&self) -> u32 {
        bits(self.height).saturating_sub(self.coding.dropped_bits())
    }

    /// Projector pixels per cell of the Gray code shown: 1 for plain Gray code, a quarter of the
    /// sines' period with them.
    pub fn cell(&self) -> u32 {
        1 << self.coding.dropped_bits()
    }

    /// Pictures in a scan: white, black, a pattern and its inverse per Gray bit, the sines.
    pub fn count(&self) -> usize {
        2 + 2 * (self.column_bits() + self.row_bits()) as usize + 2 * self.coding.steps() as usize
    }

    /// The pattern shown as picture `index` (0 is white), `None` past the last.
    pub fn get(&self, index: usize) -> Option<Pattern> {
        let (cb, rb) = (self.column_bits() as usize, self.row_bits() as usize);
        let steps = self.coding.steps() as usize;
        match index {
            0 => Some(Pattern::White),
            1 => Some(Pattern::Black),
            _ => {
                let k = index - 2;
                let pairs = 2 * (cb + rb);
                if k < pairs {
                    let (bit, inverse) = (k / 2, k % 2 == 1);
                    if bit < cb {
                        Some(Pattern::Columns { bit: bit as u32, inverse })
                    } else {
                        Some(Pattern::Rows { bit: (bit - cb) as u32, inverse })
                    }
                } else if k - pairs < steps {
                    Some(Pattern::ColumnPhase { step: (k - pairs) as u32 })
                } else if k - pairs < 2 * steps {
                    Some(Pattern::RowPhase { step: (k - pairs - steps) as u32 })
                } else {
                    None
                }
            }
        }
    }

    /// The position of `pattern` in the scan, `None` for a bit or sine the scan does not have.
    pub fn index(&self, pattern: Pattern) -> Option<usize> {
        let (cb, rb) = (self.column_bits(), self.row_bits());
        let steps = self.coding.steps();
        let sines = 2 + 2 * (cb + rb) as usize;
        match pattern {
            Pattern::White => Some(0),
            Pattern::Black => Some(1),
            Pattern::Columns { bit, inverse } if bit < cb => {
                Some(2 + 2 * bit as usize + usize::from(inverse))
            }
            Pattern::Rows { bit, inverse } if bit < rb => {
                Some(2 + 2 * (cb + bit) as usize + usize::from(inverse))
            }
            Pattern::ColumnPhase { step } if step < steps => Some(sines + step as usize),
            Pattern::RowPhase { step } if step < steps => Some(sines + (steps + step) as usize),
            _ => None,
        }
    }

    /// Whether projector pixel `(x, y)` is lit in `pattern`; a sine counts as lit where it is
    /// above half.
    pub fn lit(&self, pattern: Pattern, x: u32, y: u32) -> bool {
        let stripe = |v: u32, side: u32, shown: u32, bit: u32| {
            bit < shown && bits(side).checked_sub(1 + bit).is_some_and(|shift| (gray(v) >> shift) & 1 == 1)
        };
        match pattern {
            Pattern::White => true,
            Pattern::Black => false,
            Pattern::Columns { bit, inverse } => stripe(x, self.width, self.column_bits(), bit) != inverse,
            Pattern::Rows { bit, inverse } => stripe(y, self.height, self.row_bits(), bit) != inverse,
            Pattern::ColumnPhase { .. } | Pattern::RowPhase { .. } => self.light(pattern, x, y) >= 0.5,
        }
    }

    /// The light of projector pixel `(x, y)` in `pattern`, 0 to 1: 0 or 1 for the stripes,
    /// `(1 + cos(2 pi ((v + 0.5) / period - step / steps))) / 2` for a sine along `v`. This is
    /// light: a projector whose response to its signal is not linear needs it bent first (its
    /// gamma).
    pub fn light(&self, pattern: Pattern, x: u32, y: u32) -> f32 {
        let wave = |v: u32, step: u32| -> f32 {
            let Coding::GrayPhase { period, steps } = self.coding else { return 0.0 };
            if step >= steps {
                return 0.0;
            }
            let turn = (f64::from(v) + 0.5) / f64::from(period) - f64::from(step) / f64::from(steps);
            (0.5 + 0.5 * (std::f64::consts::TAU * turn).cos()) as f32
        };
        match pattern {
            Pattern::ColumnPhase { step } => wave(x, step),
            Pattern::RowPhase { step } => wave(y, step),
            _ => {
                if self.lit(pattern, x, y) {
                    1.0
                } else {
                    0.0
                }
            }
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
    /// A decoded pixel further than this (pixels) off the epipolar geometry that the decode as a
    /// whole obeys is dropped as inconsistent ([`crate::epipolar`]); 0 keeps every pixel.
    pub epipolar: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_contrast: 0.02,
            saturation: 0.98,
            min_confidence: 0.2,
            tolerance: 3.0,
            window: DEFAULT_WINDOW,
            epipolar: DEFAULT_EPIPOLAR,
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
        if !ok(self.epipolar, 0.0, 1e6) {
            return Err("`epipolar` is 0 (off) or more pixels".into());
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
    /// Fewer than two neighbours agree with it, or its sines' phase and its Gray code disagree.
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
/// a pattern again starts its pair over (a bit captured too early is captured again). Each sine
/// is kept as its brightness (two bytes a pixel); adding one again replaces it.
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
    /// Per sine (columns, then rows): its brightness in units of `1 / WAVE_SCALE`.
    waves: Vec<Option<Vec<u16>>>,
}

/// A sine's brightness is kept as `u16` in units of `1 / WAVE_SCALE`, so up to 2.
const WAVE_SCALE: f32 = 32767.0;

/// The sines' sums per axis and camera pixel: `sum(I_k sin d_k)` and `sum(I_k cos d_k)` over the
/// steps `k` with `d_k = 2 pi k / steps`, whose angle is the phase.
struct Phase {
    sums: [Vec<[f32; 2]>; 2],
    period: f32,
    steps: f32,
    /// Projector pixels per Gray code cell.
    cell: f32,
}

impl Phase {
    /// The position along `axis` of camera pixel `i` whose Gray code says cell `code`; how
    /// clearly the camera sees the sines there (their amplitude over that of a full sine at its
    /// `contrast`, 0 to 1); and how far the phase lies from the cell's centre, in cells.
    fn position(&self, axis: usize, i: usize, code: u32, contrast: f32) -> (f32, f32, f32) {
        let [s, c] = self.sums.get(axis).and_then(|v| v.get(i)).copied().unwrap_or([0.0; 2]);
        let modulation = 4.0 * (s * s + c * c).sqrt() / (self.steps * contrast.max(1e-6));
        let turn = s.atan2(c) / std::f32::consts::TAU;
        let within = turn.rem_euclid(1.0) * self.period;
        let centre = (code as f32 + 0.5) * self.cell;
        let at = within + ((centre - within) / self.period).round() * self.period;
        (at, modulation.min(1.0), (at - centre).abs() / self.cell)
    }
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
            waves: vec![None; 2 * patterns.coding().steps() as usize],
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
            Pattern::ColumnPhase { step } | Pattern::RowPhase { step } => {
                let row = usize::from(matches!(pattern, Pattern::RowPhase { .. }));
                let slot = row * self.patterns.coding().steps() as usize + step as usize;
                let Some(wave) = self.waves.get_mut(slot) else {
                    return Err(format!("picture {index}: no such sine"));
                };
                *wave = Some(
                    pixels
                        .iter()
                        .map(|p| (brightness(p) * WAVE_SCALE).clamp(0.0, f32::from(u16::MAX)).round() as u16)
                        .collect(),
                );
                return Ok(());
            }
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
        let steps = self.patterns.coding().steps() as usize;
        for (slot, wave) in self.waves.iter().enumerate() {
            if wave.is_some() {
                continue;
            }
            let step = (slot % steps.max(1)) as u32;
            let pattern =
                if slot < steps { Pattern::ColumnPhase { step } } else { Pattern::RowPhase { step } };
            out.extend(self.patterns.index(pattern));
        }
        out
    }

    /// The sines' sums per axis, `None` for plain Gray code.
    fn phase(&self) -> Option<Phase> {
        let Coding::GrayPhase { period, steps } = self.patterns.coding() else { return None };
        let n = self.width as usize * self.height as usize;
        let mut sums = [vec![[0.0f32; 2]; n], vec![[0.0f32; 2]; n]];
        for (slot, wave) in self.waves.iter().enumerate() {
            let Some(wave) = wave else { continue };
            let (axis, step) = (slot / steps as usize, slot % steps as usize);
            let (sin, cos) = (std::f32::consts::TAU * step as f32 / steps as f32).sin_cos();
            let Some(sum) = sums.get_mut(axis) else { continue };
            for (acc, v) in sum.iter_mut().zip(wave) {
                let v = f32::from(*v) / WAVE_SCALE;
                acc[0] += v * sin;
                acc[1] += v * cos;
            }
        }
        Some(Phase { sums, period: period as f32, steps: steps as f32, cell: self.patterns.cell() as f32 })
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
        let phase = self.phase();
        // How clearly the camera sees the sines: per axis the median over the reached pixels.
        let modulation = phase.as_ref().map_or([0.0; 2], |ph| {
            [0, 1].map(|axis| {
                let mut hist = [0u64; BINS];
                let mut total = 0u64;
                for (i, s) in lit_status.iter().enumerate() {
                    if *s != Status::Valid {
                        continue;
                    }
                    let (_, m, _) = ph.position(axis, i, 0, contrast.get(i).copied().unwrap_or(1.0));
                    if let Some(h) = hist.get_mut(((m * BINS as f32) as usize).min(BINS - 1)) {
                        *h += 1;
                    }
                    total += 1;
                }
                let mut seen = 0u64;
                for (b, h) in hist.iter().enumerate() {
                    seen += h;
                    if total > 0 && seen * 2 >= total {
                        return (b as f32 + 0.5) / BINS as f32;
                    }
                }
                0.0
            })
        });

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
            // With sines: the position from the phase, the confidence also how clearly the
            // sines show, and a phase further than a cell from the Gray code's cell is a
            // disagreement (a sine or a bit read wrong).
            let (at, sure, apart) = match &phase {
                None => ([x as f32 + 0.5, y as f32 + 0.5], 1.0, 0.0),
                Some(ph) => {
                    let (u, mu, du) = ph.position(0, i, x, c);
                    let (v, mv, dv) = ph.position(1, i, y, c);
                    ([u, v], mu.min(mv), du.max(dv))
                }
            };
            *conf = cx.min(cy).min(sure);
            if *conf < options.min_confidence {
                *st = Status::Ambiguous;
            } else if apart > 1.0 {
                *st = Status::Inconsistent;
            } else if at[0] < 0.0 || at[1] < 0.0 || at[0] >= pw as f32 || at[1] >= ph as f32 {
                *st = Status::Outside;
            } else {
                *pt = Some(at);
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
        let cell = self.patterns.cell() as f32;
        let unresolved_span = |bits: usize, count: u32| -> f32 {
            let unresolved = (bits as u32).saturating_sub(count).min(16);
            ((1u32 << unresolved) - 1) as f32 * cell
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
        let off_geometry = drop_off_geometry(&mut points, &mut status, &contrast, w, options.epipolar);
        for (c, p) in confidence.iter_mut().zip(&points) {
            if p.is_none() {
                *c = 0.0;
            }
        }

        let counts = status_counts(&status);
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
                off_geometry,
                coding: self.patterns.coding(),
                modulation,
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

/// Drops the decoded pixels (`points`, per camera pixel of a picture `w` wide) that lie further
/// than `threshold` off the epipolar geometry most of them obey, marking them inconsistent;
/// returns how many. Light the projector throws onto a wall that throws it on to another surface
/// carries the stripes of where it came from, coarse ones mostly: the camera decodes them as if
/// the projector lit that surface directly, often with a fair margin, at a projector pixel that
/// lies nowhere near the surface. A bright pass in a niche of light surfaces decodes much of the
/// picture so. The fit's hypotheses come from the brighter half of the decoded pixels (`contrast`,
/// white minus black), which the projector lights directly; nothing is dropped when no fit holds
/// [`GEOMETRY_TRUST`] of them.
pub(crate) fn drop_off_geometry(
    points: &mut [Option<[f32; 2]>],
    status: &mut [Status],
    contrast: &[f32],
    w: usize,
    threshold: f32,
) -> usize {
    if threshold.is_nan() || threshold <= 0.0 {
        return 0;
    }
    let idx: Vec<usize> = points.iter().enumerate().filter(|(_, p)| p.is_some()).map(|(i, _)| i).collect();
    if idx.len() < GEOMETRY_MIN {
        return 0;
    }
    let w = w.max(1);
    let from: Vec<[f64; 2]> = idx.iter().map(|i| [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]).collect();
    let to: Vec<[f64; 2]> = idx
        .iter()
        .map(|i| {
            points.get(*i).copied().flatten().map_or([f64::NAN; 2], |p| [f64::from(p[0]), f64::from(p[1])])
        })
        .collect();
    let brightness: Vec<f32> = idx.iter().map(|i| contrast.get(*i).copied().unwrap_or(0.0)).collect();
    let mut sorted = brightness.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let seeds: Vec<usize> =
        brightness.iter().enumerate().filter(|(_, c)| **c >= median).map(|(k, _)| k).collect();
    let Some(fit) = crate::epipolar::fit(&from, &to, &seeds, f64::from(threshold)) else { return 0 };
    if fit.share < GEOMETRY_TRUST {
        return 0;
    }
    let mut dropped = 0usize;
    for (k, i) in idx.iter().enumerate() {
        let (Some(p), Some(q)) = (from.get(k), to.get(k)) else { continue };
        if fit.fundamental.distance(*p, *q) <= f64::from(threshold) {
            continue;
        }
        if let Some(slot) = points.get_mut(*i) {
            *slot = None;
        }
        if let Some(s) = status.get_mut(*i) {
            *s = Status::Inconsistent;
        }
        dropped += 1;
    }
    dropped
}

/// Camera pixels per [`Status`], in the order of [`Status::ALL`].
fn status_counts(status: &[Status]) -> [usize; 6] {
    let mut counts = [0usize; 6];
    for s in status {
        if let Some(k) = Status::ALL.iter().position(|a| a == s)
            && let Some(c) = counts.get_mut(k)
        {
            *c += 1;
        }
    }
    counts
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
    /// Decoded pixels dropped (as inconsistent) for lying off the decode's epipolar geometry.
    pub off_geometry: usize,
    pub coding: Coding,
    /// How clearly the camera sees the sines across the columns and down the rows: the median
    /// over the reached pixels of their amplitude over a full sine's, 0 to 1 (0 without sines).
    pub modulation: [f32; 2],
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

/// Scans of one projector at several levels of light, merged ([`fuse`]).
#[derive(Clone, Debug)]
pub struct Fusion {
    pub scan: Scan,
    /// Per scan given: the camera pixels decoded in it, those taken from it, and those decoded
    /// in it alone.
    pub valid: Vec<usize>,
    pub taken: Vec<usize>,
    pub only: Vec<usize>,
    /// Where two or more scans decoded a pixel: how far each other one lies from the one taken,
    /// projector pixels (the larger of the two axes), median and 90th percentile; `None` where
    /// none overlap.
    pub apart: Option<[f32; 2]>,
}

/// Merges scans of the same projector seen by the same camera, taken at different levels of light
/// (a bright one clips on light surfaces and in highlights, a dim one loses dark and grazing ones):
/// per camera pixel the decode of the surest scan, ties to the earlier one. Where none decoded,
/// the pixel's status is that of the first scan not clipped there (else clipped). The scene colour
/// and the counts of resolved bits and the step are the first scan's, so its exposure sets the
/// look of the scene picture; resolved bits and the sines' modulation are the most any scan
/// reached.
pub fn fuse(scans: Vec<Scan>) -> Result<Fusion, String> {
    let mut it = scans.into_iter();
    let Some(mut out) = it.next() else { return Err("no scans to merge".into()) };
    let rest: Vec<Scan> = it.collect();
    if let Some(s) = rest.iter().find(|s| s.camera != out.camera || s.projector != out.projector) {
        return Err(format!(
            "scans of a {} x {} camera and {} x {} projector cannot merge with one of {} x {} and {} x {}",
            out.camera.0,
            out.camera.1,
            out.projector.0,
            out.projector.1,
            s.camera.0,
            s.camera.1,
            s.projector.0,
            s.projector.1
        ));
    }
    let n = out.points.len();
    let count = 1 + rest.len();
    let valid_of = |s: &Scan| s.points.iter().filter(|p| p.is_some()).count();
    let mut valid = vec![valid_of(&out)];
    valid.extend(rest.iter().map(valid_of));
    let mut taken = vec![0usize; count];
    let mut only = vec![0usize; count];
    let mut apart: Vec<f32> = Vec::new();
    for i in 0..n {
        // The surest decode: (scan, point, confidence).
        let first = out
            .points
            .get(i)
            .copied()
            .flatten()
            .map(|p| (0, p, out.confidence.get(i).copied().unwrap_or(0.0)));
        let mut best = first;
        let mut decoded = usize::from(first.is_some());
        let mut alone = first.map(|_| 0);
        for (k, s) in rest.iter().enumerate() {
            let Some(p) = s.points.get(i).copied().flatten() else { continue };
            let c = s.confidence.get(i).copied().unwrap_or(0.0);
            decoded += 1;
            alone = Some(k + 1);
            if best.is_none_or(|(_, _, b)| c > b) {
                best = Some((k + 1, p, c));
            }
        }
        if decoded == 1
            && let Some(o) = alone.and_then(|k| only.get_mut(k))
        {
            *o += 1;
        }
        let Some((k, p, c)) = best else {
            // Nothing decoded: the first scan's reason, unless it clipped and another had a say.
            let first_status = out.status.get(i).copied().unwrap_or(Status::Dark);
            if first_status == Status::Clipped
                && let Some(st) =
                    rest.iter().filter_map(|s| s.status.get(i).copied()).find(|s| *s != Status::Clipped)
                && let Some(slot) = out.status.get_mut(i)
            {
                *slot = st;
            }
            continue;
        };
        if let Some(t) = taken.get_mut(k) {
            *t += 1;
        }
        if decoded > 1 {
            let others = std::iter::once(&out.points).chain(rest.iter().map(|s| &s.points));
            for (j, points) in others.enumerate() {
                if j != k
                    && let Some(Some(q)) = points.get(i)
                {
                    apart.push((p[0] - q[0]).abs().max((p[1] - q[1]).abs()));
                }
            }
        }
        if k > 0 {
            if let Some(slot) = out.points.get_mut(i) {
                *slot = Some(p);
            }
            if let Some(slot) = out.confidence.get_mut(i) {
                *slot = c;
            }
            if let Some(slot) = out.status.get_mut(i) {
                *slot = Status::Valid;
            }
        }
    }
    out.stats.counts = status_counts(&out.status);
    for s in &rest {
        for (a, b) in out.stats.resolved.iter_mut().zip(s.stats.resolved) {
            *a = (*a).max(b);
        }
        out.stats.off_geometry += s.stats.off_geometry;
        for (a, b) in out.stats.modulation.iter_mut().zip(s.stats.modulation) {
            *a = a.max(b);
        }
    }
    let apart = (!apart.is_empty()).then(|| {
        apart.sort_by(f32::total_cmp);
        let at = |q: f32| apart.get(((apart.len() - 1) as f32 * q).round() as usize).copied().unwrap_or(0.0);
        [at(0.5), at(0.9)]
    });
    Ok(Fusion { scan: out, valid, taken, only, apart })
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
