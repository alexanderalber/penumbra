//! Height maps of a mostly flat object (a relief panel) from structured light scans, without a
//! scan of the empty scene.
//!
//! **Per projector** ([`analyse`]): its `proj_to_cam` map pairs each measured projector pixel
//! with a camera position. A homography projector -> camera fitted to those pairs robustly
//! ([`Homography::fit_cauchy`]) is the panel's plane as seen by both; whatever stands out of
//! that plane lands beside the position the homography predicts, by its parallax, along the
//! direction between camera and projector. That residual along its main direction is the
//! height, up to scale and sign, in camera pixels of parallax.
//!
//! Before the height, the panel is separated from the wall or floor around it, which would tilt
//! the plane: the residual, filled from the nearest measured pixel and median filtered, jumps
//! where the panel ends (its gradient above [`ReliefOptions::jump`] camera pixels per
//! projector pixel); cut there, the largest piece with its holes filled is the panel, and the
//! plane is fitted again on it alone, twice. Unmeasured pixels (the camera coarser than the
//! projector, self-shadows) get their height by a normalised convolution at a few scales in
//! the projector's own pixels ([`fill`]). The light mask is the piece's measured pixels closed
//! and their enclosed holes filled (self-shadows inside the panel stay lit, while unmeasured
//! regions open to the outside, such as wall the panel hides from the camera, stay dark),
//! without the pixels whose light the plane predicts outside the camera's picture (it was never
//! seen, so it is not known to land on the panel), its largest part, shrunk by
//! [`ReliefOptions::erode`].
//!
//! **Sign:** the data does not tell near from far (mirroring the depth and the camera's side of
//! the projector gives the same residuals). [`analyse`] orients the height by a heuristic: when
//! enough measured pixels lie well outside the panel, they are taken as the wall behind it, so
//! the panel's side of them is up; otherwise the heights are taken as skewed upwards (narrow
//! peaks over a broad base).
//!
//! **Several projectors** ([`overlap`], [`scale_fit`]): each one's heights are in its own units
//! (its parallax per height). They are brought to the first one's over the camera pixels both
//! light inside their masks, by a symmetric line fit (the standard deviations matched, so
//! neither side's noise pulls the slope towards zero, as least squares does), outliers
//! trimmed. The units still vary somewhat across the field (perspective), so one scale per
//! projector is an approximation.

use crate::homography::Homography;
use crate::objects::Map;
use crate::raster;

/// Rounds of cutting the panel out and fitting the plane again on it.
const SEGMENT_ROUNDS: usize = 2;
/// Cauchy reweighting rounds per plane fit.
const IRLS_ROUNDS: usize = 10;
/// Median window for the residual before its gradient.
const MEDIAN: usize = 5;
/// How far a cut at a jump is widened, projector pixels.
const JUMP_WIDEN: u32 = 2;
/// Closing of the panel before the light mask, projector pixels.
const MASK_CLOSE: u32 = 6;
/// Closing of the piece's measured pixels for the light mask, projector pixels: the mask
/// reaches at most about this far past them, and notches narrower than twice it close.
const MEASURED_CLOSE: u32 = 24;
/// Light predicted closer than this to the camera picture's edge counts as outside, camera px.
const SEEN_MARGIN: f64 = 3.0;
/// Scales of the hole fill, projector pixels, and the weight density each must reach to count.
const FILL_SIGMAS: [f32; 5] = [2.0, 5.0, 12.0, 30.0, 80.0];
const FILL_DENSITY: f32 = 0.15;
/// Measured pixels this far from the panel (projector pixels) count as the wall for the sign.
const OUTSIDE_GAP: u32 = 10;
/// The wall decides the sign when it has this many measured pixels and this share of all.
const OUTSIDE_MIN: usize = 200;
const OUTSIDE_SHARE: f64 = 0.01;
/// Fewest measured pixels a projector must have.
pub const MIN_MEASURED: usize = 100;
/// Fewest camera pixels two projectors must share for a scale between them.
pub const MIN_OVERLAP: usize = 200;
/// Trimming rounds of the scale fit, and the cut in robust standard deviations.
const SCALE_ROUNDS: usize = 3;
const SCALE_CUT: f64 = 3.0;

/// How [`analyse`] finds the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReliefOptions {
    /// Residual jump at the panel's edge, camera pixels per projector pixel.
    pub jump: f32,
    /// The light mask shrinks by this, projector pixels.
    pub erode: u32,
}

impl Default for ReliefOptions {
    fn default() -> Self {
        Self { jump: 4.0, erode: 4 }
    }
}

impl ReliefOptions {
    pub fn check(&self) -> Result<(), String> {
        if !self.jump.is_finite() || !(0.01..=1e5).contains(&self.jump) {
            return Err("`jump` is 0.01 to 100000 camera pixels per projector pixel".into());
        }
        if self.erode > 64 {
            return Err("`erode` is 0 to 64 projector pixels".into());
        }
        Ok(())
    }
}

/// What decided a projector's sign in [`analyse`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Heuristic {
    /// The measured pixels outside the panel (count, their median height before orienting).
    Wall { pixels: usize, median: f32 },
    /// The skewness of the heights on the panel before orienting.
    Skew { skew: f32 },
}

impl Heuristic {
    pub fn name(&self) -> &'static str {
        match self {
            Heuristic::Wall { .. } => "wall behind",
            Heuristic::Skew { .. } => "skew",
        }
    }
}

/// One projector's relief.
#[derive(Clone, Debug)]
pub struct Relief {
    pub width: usize,
    pub height: usize,
    /// The panel's plane, projector pixels (centres at +0.5) to camera pixels.
    pub plane: Homography,
    /// Unit vector in the camera along which a pixel moves as its height grows.
    pub direction: [f64; 2],
    /// Per projector pixel the height, camera pixels of parallax, filled where not measured.
    pub heights: Vec<f32>,
    /// The panel as cut out (before closing and erosion).
    pub piece: Vec<bool>,
    /// The light mask.
    pub mask: Vec<bool>,
    pub measured: usize,
    /// Measured pixels on the piece.
    pub piece_measured: usize,
    /// Median and 90th percentile of the plane's error on the piece, camera pixels (the height
    /// is most of it).
    pub error_px: [f64; 2],
    /// How the sign was chosen, and whether the heights were flipped since ([`Relief::flip`]).
    pub heuristic: Heuristic,
    pub flipped: bool,
}

impl Relief {
    /// Turns the heights upside down.
    pub fn flip(&mut self) {
        self.heights.iter_mut().for_each(|h| *h = -*h);
        self.direction = [-self.direction[0], -self.direction[1]];
        self.flipped = !self.flipped;
    }
}

/// The relief seen by one projector: `map` is its `proj_to_cam` map, `camera` the camera's size.
pub fn analyse(map: &Map, camera: (usize, usize), o: &ReliefOptions) -> Result<Relief, String> {
    o.check()?;
    let (w, h) = (map.width, map.height);
    let n = map.len();
    if camera.0 == 0 || camera.1 == 0 {
        return Err("the camera picture is empty".into());
    }
    let (cw, ch) = (camera.0 as f64, camera.1 as f64);
    let measured: Vec<bool> = (0..n).map(|i| map.measured(i)).collect();
    let idx: Vec<usize> = (0..n).filter(|i| measured.get(*i) == Some(&true)).collect();
    if idx.len() < MIN_MEASURED {
        return Err(format!("only {} measured projector pixels (at least {MIN_MEASURED})", idx.len()));
    }
    let from: Vec<[f64; 2]> = idx.iter().map(|i| [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]).collect();
    let to: Vec<[f64; 2]> = idx
        .iter()
        .map(|i| map.uv(*i).map_or([f64::NAN; 2], |[u, v]| [f64::from(u) * cw, f64::from(v) * ch]))
        .collect();
    let fit = |sel: &[bool]| -> Result<Homography, String> {
        let (f, t): (Vec<[f64; 2]>, Vec<[f64; 2]>) =
            from.iter().zip(&to).zip(sel).filter(|(_, s)| **s).map(|((f, t), _)| (*f, *t)).unzip();
        if f.len() < 4 {
            return Err("the panel holds fewer than 4 measured pixels".into());
        }
        Homography::fit_cauchy(&f, &t, IRLS_ROUNDS).map_err(|e| e.to_string())
    };
    let residuals = |plane: &Homography| -> Vec<[f64; 2]> {
        from.iter()
            .zip(&to)
            .map(|(p, q)| match plane.apply(*p) {
                Some(r) => [q[0] - r[0], q[1] - r[1]],
                None => [f64::NAN; 2],
            })
            .collect()
    };

    let mut plane = fit(&vec![true; idx.len()])?;
    let (_, nearest) = raster::distance(w, h, &measured);
    let mut piece = vec![false; n];
    for _ in 0..SEGMENT_ROUNDS {
        piece = cut(w, h, &idx, &residuals(&plane), &nearest, o.jump);
        let on: Vec<bool> = idx.iter().map(|i| piece.get(*i) == Some(&true)).collect();
        plane = fit(&on)?;
    }
    let res = residuals(&plane);
    let on: Vec<bool> = idx.iter().map(|i| piece.get(*i) == Some(&true)).collect();
    let core: Vec<[f64; 2]> = res
        .iter()
        .zip(&on)
        .filter(|(r, s)| **s && r[0].is_finite() && r[1].is_finite())
        .map(|(r, _)| *r)
        .collect();
    if core.len() < 4 {
        return Err("the panel holds fewer than 4 measured pixels".into());
    }
    let mut direction = main_direction(&core);
    let mut errors: Vec<f64> = core.iter().map(|r| r[0].hypot(r[1])).collect();
    errors.sort_by(f64::total_cmp);
    let error_px = [quantile(&errors, 0.5), quantile(&errors, 0.9)];

    // Heights before orienting, then the sign by the heuristic.
    let along = |r: &[f64; 2], d: [f64; 2]| (r[0] * d[0] + r[1] * d[1]) as f32;
    let far = raster::city_distance(w, h, &piece);
    let outside: Vec<f32> = idx
        .iter()
        .zip(&res)
        .filter(|(i, r)| far.get(**i).is_some_and(|d| *d > OUTSIDE_GAP) && r[0].is_finite())
        .map(|(_, r)| along(r, direction))
        .collect();
    let on_piece: Vec<f32> = core.iter().map(|r| along(r, direction)).collect();
    let wall = outside.len() >= OUTSIDE_MIN && outside.len() as f64 >= OUTSIDE_SHARE * idx.len() as f64;
    let heuristic = if wall {
        let mut s = outside;
        s.sort_by(f32::total_cmp);
        Heuristic::Wall { pixels: s.len(), median: s.get(s.len() / 2).copied().unwrap_or(0.0) }
    } else {
        Heuristic::Skew { skew: skewness(&on_piece) }
    };
    let up = match heuristic {
        Heuristic::Wall { median, .. } => median <= 0.0,
        Heuristic::Skew { skew } => skew >= 0.0,
    };
    if !up {
        direction = [-direction[0], -direction[1]];
    }

    let mut values = vec![0f32; n];
    let mut weights = vec![0f32; n];
    for ((i, r), s) in idx.iter().zip(&res).zip(&on) {
        if *s
            && r[0].is_finite()
            && let (Some(v), Some(wt)) = (values.get_mut(*i), weights.get_mut(*i))
        {
            *v = along(r, direction);
            *wt = 1.0;
        }
    }
    let heights = fill(w, h, &values, &weights);

    // The light mask: the piece closed and filled, without light the camera never saw.
    let seen: Vec<bool> = (0..n)
        .map(|i| {
            plane.apply([(i % w) as f64 + 0.5, (i / w) as f64 + 0.5]).is_some_and(|q| {
                q[0] > SEEN_MARGIN && q[0] < cw - SEEN_MARGIN && q[1] > SEEN_MARGIN && q[1] < ch - SEEN_MARGIN
            })
        })
        .collect();
    // Only the measured pixels of the piece say where light lands on the panel: unmeasured ones
    // the piece took in from its nearest measured neighbour may be wall the panel hides from the
    // camera. So the measured pixels are closed (bridging the gaps a coarser camera leaves and
    // narrow notches at the edge) and their enclosed holes filled (self-shadows inside the
    // panel); unmeasured regions open to the outside stay dark.
    let measured_piece: Vec<bool> = measured.iter().zip(&piece).map(|(a, b)| *a && *b).collect();
    let lit = raster::fill_holes(w, h, &raster::close(w, h, &measured_piece, MEASURED_CLOSE));
    let closed = raster::fill_holes(w, h, &raster::close(w, h, &piece, MASK_CLOSE));
    let m: Vec<bool> = closed.iter().zip(&lit).zip(&seen).map(|((a, l), b)| *a && *l && *b).collect();
    let m = raster::fill_holes(w, h, &largest(w, h, &m));
    let mask = raster::erode(w, h, &m, o.erode);

    Ok(Relief {
        width: w,
        height: h,
        plane,
        direction,
        heights,
        piece,
        mask,
        measured: idx.len(),
        piece_measured: core.len(),
        error_px,
        heuristic,
        flipped: false,
    })
}

/// The panel: per projector pixel the residual of its nearest measured pixel, median filtered,
/// cut where it jumps; the largest piece, holes filled.
fn cut(w: usize, h: usize, idx: &[usize], res: &[[f64; 2]], nearest: &[Option<u32>], jump: f32) -> Vec<bool> {
    let n = w * h;
    let mut at = vec![[0f32; 2]; n];
    for (i, r) in idx.iter().zip(res) {
        if let Some(a) = at.get_mut(*i)
            && r[0].is_finite()
            && r[1].is_finite()
        {
            *a = [r[0] as f32, r[1] as f32];
        }
    }
    let mut jumps = vec![false; n];
    for c in 0..2 {
        let filled: Vec<f32> = nearest
            .iter()
            .map(|j| j.and_then(|j| at.get(j as usize)).map_or(0.0, |a| a.get(c).copied().unwrap_or(0.0)))
            .collect();
        let g = raster::gradient(w, h, &raster::median(w, h, &filled, MEDIAN));
        for (j, g) in jumps.iter_mut().zip(&g) {
            *j |= *g > jump;
        }
    }
    let jumps = raster::dilate(w, h, &jumps, JUMP_WIDEN);
    let free: Vec<bool> = jumps.iter().map(|j| !j).collect();
    raster::fill_holes(w, h, &largest(w, h, &free))
}

/// The largest connected piece of a mask (4 neighbours); empty when the mask is.
fn largest(w: usize, h: usize, mask: &[bool]) -> Vec<bool> {
    let (comp, sizes) = raster::components(w, h, mask);
    let best = sizes.iter().enumerate().max_by_key(|(k, s)| (**s, std::cmp::Reverse(*k))).map(|(k, _)| k + 1);
    match best {
        Some(b) => comp.iter().map(|c| *c as usize == b).collect(),
        None => vec![false; mask.len()],
    }
}

/// The unit main axis of 2-D points around their mean.
fn main_direction(points: &[[f64; 2]]) -> [f64; 2] {
    let n = points.len().max(1) as f64;
    let mx = points.iter().map(|p| p[0]).sum::<f64>() / n;
    let my = points.iter().map(|p| p[1]).sum::<f64>() / n;
    let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
    for p in points {
        let (x, y) = (p[0] - mx, p[1] - my);
        a += x * x;
        b += x * y;
        c += y * y;
    }
    let t = 0.5 * (2.0 * b).atan2(a - c);
    [t.cos(), t.sin()]
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    let at = ((sorted.len().max(1) as f64 - 1.0) * q).round() as usize;
    sorted.get(at).copied().unwrap_or(f64::NAN)
}

fn skewness(v: &[f32]) -> f32 {
    let n = v.len().max(1) as f64;
    let m = v.iter().map(|x| f64::from(*x)).sum::<f64>() / n;
    let (mut m2, mut m3) = (0.0, 0.0);
    for x in v {
        let d = f64::from(*x) - m;
        m2 += d * d;
        m3 += d * d * d;
    }
    let (m2, m3) = (m2 / n, m3 / n);
    if m2 <= 0.0 { 0.0 } else { (m3 / m2.powf(1.5)) as f32 }
}

/// `values` where `weights` is above 0, filled in elsewhere: normalised Gaussian convolution at
/// growing scales, each pixel taking the finest at which enough weight lies near it (blended
/// over the step), so holes fill smoothly from their edges and measured pixels keep about
/// their value (smoothed by the finest scale).
pub fn fill(w: usize, h: usize, values: &[f32], weights: &[f32]) -> Vec<f32> {
    let n = w * h;
    if values.len() != n || weights.len() != n {
        return vec![0.0; values.len()];
    }
    let weighted: Vec<f32> = values.iter().zip(weights).map(|(v, w)| v * w).collect();
    let mut acc = vec![0f32; n];
    let mut got = vec![0f32; n];
    for sigma in FILL_SIGMAS {
        if got.iter().all(|g| *g >= 1.0) {
            break;
        }
        let num = raster::gaussian(w, h, &weighted, sigma);
        let den = raster::gaussian(w, h, weights, sigma);
        for (((a, g), nu), de) in acc.iter_mut().zip(got.iter_mut()).zip(&num).zip(&den) {
            let take = (de / FILL_DENSITY).clamp(0.0, 1.0) * (1.0 - *g).max(0.0);
            if take > 0.0 {
                *a += take * nu / de.max(1e-6);
                *g += take;
            }
        }
    }
    acc.iter().zip(&got).map(|(a, g)| if *g > 1e-6 { a / g } else { 0.0 }).collect()
}

/// One projector as seen through the camera: its `cam_to_proj` map, heights and light mask.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub from_camera: &'a Map,
    pub heights: &'a [f32],
    pub mask: &'a [bool],
    pub width: usize,
    pub height: usize,
}

impl View<'_> {
    /// The height a camera pixel sees through this projector, inside its mask.
    fn at(&self, c: usize) -> Option<f32> {
        let j = self.from_camera.pixel_in(c, self.width, self.height)?;
        if self.mask.get(j) == Some(&true) { self.heights.get(j).copied() } else { None }
    }
}

/// Pairs over the camera pixels where `this` and any of `others` light the mask: this one's
/// height, and the mean of the others'.
pub fn overlap(this: &View<'_>, others: &[View<'_>]) -> (Vec<f32>, Vec<f32>) {
    let (mut x, mut y) = (Vec::new(), Vec::new());
    for c in 0..this.from_camera.len() {
        let Some(h) = this.at(c) else { continue };
        let (mut s, mut k) = (0f32, 0usize);
        for o in others {
            if let Some(v) = o.at(c) {
                s += v;
                k += 1;
            }
        }
        if k > 0 {
            x.push(h);
            y.push(s / k as f32);
        }
    }
    (x, y)
}

/// Per camera pixel the mean height of the projectors lighting it inside their masks.
pub fn camera_heights(views: &[View<'_>], camera: (usize, usize)) -> Vec<Option<f32>> {
    (0..camera.0 * camera.1)
        .map(|c| {
            let (mut s, mut k) = (0f32, 0usize);
            for v in views {
                if let Some(h) = v.at(c) {
                    s += h;
                    k += 1;
                }
            }
            (k > 0).then(|| s / k as f32)
        })
        .collect()
}

/// The line `y = scale x + offset` through pairs of heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScaleFit {
    /// The slope used (symmetric or least squares, as asked).
    pub scale: f64,
    pub offset: f64,
    /// Both slopes over the same pairs, for comparison.
    pub symmetric: f64,
    pub least_squares: f64,
    pub correlation: f64,
    /// Pairs offered, and those kept after trimming.
    pub pairs: usize,
    pub kept: usize,
}

/// Fits `y ~ scale x + offset`: `symmetric` with the standard deviations matched (the slope's
/// sign from the covariance; neither variable's noise biases it), else least squares of `y` on
/// `x`. Pairs off the line by more than three robust standard deviations are dropped and the
/// line fitted again, a few times. `None` with fewer than [`MIN_OVERLAP`] pairs or no spread.
pub fn scale_fit(x: &[f32], y: &[f32], symmetric: bool) -> Option<ScaleFit> {
    let pairs: Vec<(f64, f64)> = x
        .iter()
        .zip(y)
        .map(|(a, b)| (f64::from(*a), f64::from(*b)))
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .collect();
    if pairs.len() < MIN_OVERLAP {
        return None;
    }
    let mut keep = vec![true; pairs.len()];
    let mut out = None;
    for round in 0..=SCALE_ROUNDS {
        let kept: Vec<(f64, f64)> = pairs.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| *p).collect();
        if kept.len() < MIN_OVERLAP {
            break;
        }
        let n = kept.len() as f64;
        let mx = kept.iter().map(|p| p.0).sum::<f64>() / n;
        let my = kept.iter().map(|p| p.1).sum::<f64>() / n;
        let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
        for (a, b) in &kept {
            sxx += (a - mx) * (a - mx);
            syy += (b - my) * (b - my);
            sxy += (a - mx) * (b - my);
        }
        if sxx <= 0.0 || syy <= 0.0 || sxy == 0.0 {
            break;
        }
        let ls = sxy / sxx;
        let sym = sxy.signum() * (syy / sxx).sqrt();
        let scale = if symmetric { sym } else { ls };
        let offset = my - scale * mx;
        out = Some(ScaleFit {
            scale,
            offset,
            symmetric: sym,
            least_squares: ls,
            correlation: sxy / (sxx * syy).sqrt(),
            pairs: pairs.len(),
            kept: kept.len(),
        });
        if round == SCALE_ROUNDS {
            break;
        }
        let resid: Vec<f64> = pairs.iter().map(|(a, b)| b - (scale * a + offset)).collect();
        let mut kr: Vec<f64> = resid.iter().zip(&keep).filter(|(_, k)| **k).map(|(r, _)| *r).collect();
        kr.sort_by(f64::total_cmp);
        let med = quantile(&kr, 0.5);
        let mut dev: Vec<f64> = kr.iter().map(|r| (r - med).abs()).collect();
        dev.sort_by(f64::total_cmp);
        let sigma = 1.4826 * quantile(&dev, 0.5);
        if !sigma.is_finite() || sigma <= 0.0 {
            break;
        }
        keep = resid.iter().map(|r| (r - med).abs() <= SCALE_CUT * sigma).collect();
    }
    out
}

/// The `lo` and `hi` quantiles (0 to 1) of the finite values.
pub fn range(values: &[f32], lo: f64, hi: f64) -> Option<[f32; 2]> {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let at = |q: f64| v.get(((v.len() as f64 - 1.0) * q.clamp(0.0, 1.0)).round() as usize).copied();
    Some([at(lo)?, at(hi)?])
}
