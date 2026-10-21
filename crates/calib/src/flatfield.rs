//! Flat field and black level of projectors, from a camera's pictures: brightness and colour
//! that vary across one projector's picture (a hot spot in the middle, a colour cast towards an
//! edge), and the brighter black where projectors overlap.
//!
//! **Gain** ([`relative_gain`], [`Surface`]): each projector shows each primary at a few drive
//! levels. Per camera pixel and channel, its readings are compared with a reference (the
//! median reading of all the projector's pixels at each level): the drive at which the pixel
//! reads what the reference reads at a level, interpolated between the levels, gives how much
//! brighter or dimmer the pixel is. Only comparisons of readings are used, so this holds
//! whatever monotone curve the camera applies; what it assumes is the projector's own response,
//! a power of the drive (as the sRGB curve is near enough). The logarithm of that ratio is
//! fitted with a smooth polynomial over the projector's picture (robustly, dropping the worst
//! residuals): a hot spot and an edge cast are smooth across the projector, while the
//! surface's own pattern (a relief, a painting) is not, and should not be undone. Each
//! channel's fit is then divided by its value at the spot where `projector.match` measures, and
//! every projector's map scaled by one common factor so the dimmest usable region (all but a
//! `floor` share) needs no more than full drive ([`common_scale`]): the gain is at most 1, and
//! at the spot it is that same factor for every channel of every projector, so the spot keeps
//! its colour and projectors matched there stay matched.
//!
//! On a relief or objects the surface's shading changes the brightness as much as the projector
//! does, and no fit tells them apart; but shading is the same for every channel, so the colour
//! cast alone ([`colour_only`]) can still be measured there.
//!
//! **Black** ([`cover`], [`blacks`], [`lift`]): with every projector black, each one's black
//! is read where it alone lights the wall (minus the room's light, read where none does). Where
//! some projectors do not reach, the black there lacks theirs; the projectors that do reach
//! lift their drive by that missing light, shared equally, in units of their own white there.
//! The camera reads such dark values poorly, so the result is an estimate; the projector's
//! `lift` param scales it.

use crate::raster;

/// Most levels per primary.
pub const MAX_LEVELS: usize = 16;
/// Highest polynomial degree of a [`Surface`].
pub const MAX_DEGREE: usize = 6;
/// Largest ratio [`relative_gain`] reports (and its inverse the smallest).
pub const MAX_RATIO: f64 = 8.0;
/// Most a projector lifts its black, in its own drive.
pub const MAX_LIFT: f32 = 0.25;
/// Readings below this (camera linear) are taken as no light.
pub const MIN_READING: f64 = 1e-4;
/// A black below this (camera linear, a tenth of a step of an 8-bit camera near black) is
/// taken as not seen.
pub const MIN_BLACK: f64 = 1e-5;
/// A level whose reading rises less than this share of its light's rise over the level below
/// (both in logs) is saturated (the projector or the camera clips), and so are those above it.
pub const SATURATED: f64 = 0.2;

/// The sRGB decoding of an encoded drive: the projector's light, up to a factor, for a
/// projector following the sRGB curve.
fn light(encoded: f64) -> f64 {
    crate::colour::decode(encoded.clamp(0.0, 1.0))
}

/// How much brighter the reference is than a pixel, as a factor on the pixel's linear drive:
/// with it, the pixel reads what the reference reads at the same drive.
///
/// `levels` are encoded drives, `pixel` and `reference` the camera's readings of them (black
/// subtracted, one per level, any monotone camera curve). For the highest level the reference
/// reads something the pixel also reads somewhere between its levels, the drive at which it does
/// is interpolated (straight between neighbouring levels in the logarithms of light and
/// reading), and the factor is the light there over the light at that level. A pixel dimmer
/// than the reference at every level, or brighter at every level, is extrapolated from its two
/// nearest levels. Levels from where the reference stops rising ([`SATURATED`]) on are left out
/// for both: a flat top cannot be inverted. `None` when the readings are too few or too dark to
/// compare. At most [`MAX_RATIO`] either way.
pub fn relative_gain(levels: &[f64], pixel: &[f64], reference: &[f64]) -> Option<f64> {
    let mut rows: Vec<(f64, f64, f64)> = levels
        .iter()
        .zip(pixel)
        .zip(reference)
        .filter(|((d, p), r)| d.is_finite() && **d > 0.0 && **d <= 1.0 && p.is_finite() && r.is_finite())
        .map(|((d, p), r)| (light(*d).ln(), *p, *r))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
    // Up to where the reference saturates.
    let mut usable = rows.len().min(1);
    for k in 1..rows.len() {
        let (Some(a), Some(b)) = (rows.get(k - 1), rows.get(k)) else { break };
        if a.2 < MIN_READING || b.2 < MIN_READING {
            usable = k + 1;
            continue;
        }
        if (b.2.ln() - a.2.ln()) < SATURATED * (b.0 - a.0) {
            break;
        }
        usable = k + 1;
    }
    rows.truncate(usable);
    // The pixel's readings as a rising curve in logs (noise can make a level read less than the
    // one below it; such a level takes the one below's value plus a hair).
    let mut xs = Vec::with_capacity(rows.len());
    let mut ys = Vec::with_capacity(rows.len());
    let mut last = f64::NEG_INFINITY;
    for (x, p, _) in &rows {
        if *p < MIN_READING {
            continue;
        }
        let y = p.ln().max(last + 1e-6);
        xs.push(*x);
        ys.push(y);
        last = y;
    }
    if xs.len() < 2 {
        return None;
    }
    let targets: Vec<(f64, f64)> =
        rows.iter().filter(|(_, _, r)| *r >= MIN_READING).map(|(x, _, r)| (*x, r.ln())).collect();
    let (&lo_x, &hi_x) = (xs.first()?, xs.last()?);
    let (&lo_y, &hi_y) = (ys.first()?, ys.last()?);
    let clamp = |ratio_ln: f64| ratio_ln.exp().clamp(1.0 / MAX_RATIO, MAX_RATIO);
    // Highest reference level first: the best lit, so the least noisy.
    for (x_ref, y_ref) in targets.iter().rev() {
        if *y_ref < lo_y || *y_ref > hi_y {
            continue;
        }
        for k in 1..xs.len() {
            let (x0, x1) = (*xs.get(k - 1)?, *xs.get(k)?);
            let (y0, y1) = (*ys.get(k - 1)?, *ys.get(k)?);
            if *y_ref >= y0 && *y_ref <= y1 {
                let t = if y1 - y0 > 1e-12 { (y_ref - y0) / (y1 - y0) } else { 0.0 };
                return Some(clamp(x0 + t * (x1 - x0) - x_ref));
            }
        }
    }
    // Out of range: extrapolate along the nearest two levels, the slope kept sensible.
    let slope = |a: usize, b: usize| -> Option<f64> {
        let s = (ys.get(b)? - ys.get(a)?) / (xs.get(b)? - xs.get(a)?);
        Some(if s.is_finite() { s.clamp(0.3, 4.0) } else { 1.0 })
    };
    let n = xs.len();
    let (&(lowest_x, lowest_y), &(highest_x, highest_y)) = (targets.first()?, targets.last()?);
    if lowest_y > hi_y {
        // Dimmer than the reference even at its lowest level.
        let s = slope(n - 2, n - 1)?;
        return Some(clamp(hi_x + (lowest_y - hi_y) / s - lowest_x));
    }
    if highest_y < lo_y {
        // Brighter than the reference even at its lowest level.
        let s = slope(0, 1)?;
        return Some(clamp(lo_x + (highest_y - lo_y) / s - highest_x));
    }
    None
}

/// A smooth function of a position in a picture (0..1 across and down): a polynomial of
/// `degree` in both, fitted by least squares.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub degree: usize,
    pub coefficients: Vec<f64>,
    /// The fitted values' range over the samples; [`Surface::value`] stays inside it, so it
    /// does not run away where nothing was measured.
    pub range: [f64; 2],
}

fn terms(degree: usize, u: f64, v: f64, out: &mut Vec<f64>) {
    out.clear();
    // Centred, so the powers stay small.
    let (x, y) = (2.0 * u - 1.0, 2.0 * v - 1.0);
    for total in 0..=degree {
        for i in 0..=total {
            out.push(x.powi((total - i) as i32) * y.powi(i as i32));
        }
    }
}

/// Solves `a x = b` (n x n, row by row) by Gaussian elimination with partial pivoting; `None`
/// when singular.
fn solve(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Option<Vec<f64>> {
    if a.len() != n * n || b.len() != n {
        return None;
    }
    for col in 0..n {
        let pivot = (col..n).max_by(|i, j| {
            let (x, y) =
                (a.get(i * n + col).copied().unwrap_or(0.0), a.get(j * n + col).copied().unwrap_or(0.0));
            x.abs().total_cmp(&y.abs())
        })?;
        let p = a.get(pivot * n + col).copied()?;
        if p.abs().is_nan() || p.abs() <= 1e-12 {
            return None;
        }
        if pivot != col {
            for k in 0..n {
                a.swap(pivot * n + k, col * n + k);
            }
            b.swap(pivot, col);
        }
        for row in col + 1..n {
            let f = a.get(row * n + col).copied()? / p;
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                let v = a.get(col * n + k).copied()?;
                if let Some(x) = a.get_mut(row * n + k) {
                    *x -= f * v;
                }
            }
            let v = b.get(col).copied()?;
            if let Some(x) = b.get_mut(row) {
                *x -= f * v;
            }
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let mut s = b.get(row).copied()?;
        for k in row + 1..n {
            s -= a.get(row * n + k).copied()? * x.get(k).copied()?;
        }
        let d = a.get(row * n + row).copied()?;
        if let Some(v) = x.get_mut(row) {
            *v = s / d;
        }
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

impl Surface {
    /// Fits `samples` (`[u, v, value]`) with a polynomial of `degree`; then, `rounds` times, drops
    /// the `trim` share of samples farthest from the fit and fits again. Fails with too few
    /// samples or a degree beyond [`MAX_DEGREE`].
    pub fn fit(samples: &[[f64; 3]], degree: usize, trim: f64, rounds: usize) -> Result<Surface, String> {
        if degree > MAX_DEGREE {
            return Err(format!("`degree` is 0 to {MAX_DEGREE}"));
        }
        if !(trim.is_finite() && (0.0..0.9).contains(&trim)) {
            return Err("`trim` is 0 to 0.9".into());
        }
        let good: Vec<[f64; 3]> =
            samples.iter().copied().filter(|s| s.iter().all(|v| v.is_finite())).collect();
        let n = (degree + 1) * (degree + 2) / 2;
        if good.len() < 4 * n {
            return Err(format!("{} samples are too few for degree {degree}", good.len()));
        }
        let mut keep = vec![true; good.len()];
        let mut surface = Surface { degree, coefficients: vec![0.0; n], range: [0.0, 0.0] };
        let mut t = Vec::with_capacity(n);
        for round in 0..=rounds {
            let mut ata = vec![0.0; n * n];
            let mut atb = vec![0.0; n];
            for (s, k) in good.iter().zip(&keep) {
                if !*k {
                    continue;
                }
                terms(degree, s[0], s[1], &mut t);
                for (i, ti) in t.iter().enumerate() {
                    if let Some(b) = atb.get_mut(i) {
                        *b += ti * s[2];
                    }
                    for (j, tj) in t.iter().enumerate().skip(i) {
                        if let Some(a) = ata.get_mut(i * n + j) {
                            *a += ti * tj;
                        }
                    }
                }
            }
            for i in 0..n {
                for j in 0..i {
                    let v = ata.get(j * n + i).copied().unwrap_or(0.0);
                    if let Some(a) = ata.get_mut(i * n + j) {
                        *a = v;
                    }
                }
            }
            // A touch of ridge keeps a fit over a narrow strip of the picture solvable.
            let scale = (0..n).map(|i| ata.get(i * n + i).copied().unwrap_or(0.0)).sum::<f64>() / n as f64;
            for i in 0..n {
                if let Some(a) = ata.get_mut(i * n + i) {
                    *a += scale * 1e-9 + 1e-12;
                }
            }
            surface.coefficients = solve(ata, atb, n).ok_or("the fit is singular (samples on a line?)")?;
            if round == rounds || trim == 0.0 {
                break;
            }
            let mut residuals: Vec<f64> =
                good.iter().map(|s| (surface.raw(s[0], s[1]) - s[2]).abs()).collect();
            let mut sorted = residuals.clone();
            sorted.sort_by(f64::total_cmp);
            let cut =
                sorted.get(((sorted.len() as f64) * (1.0 - trim)) as usize).copied().unwrap_or(f64::MAX);
            for (k, r) in keep.iter_mut().zip(residuals.iter_mut()) {
                *k = *r <= cut;
            }
        }
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for s in &good {
            let v = surface.raw(s[0], s[1]);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        surface.range = [lo, hi];
        Ok(surface)
    }

    fn raw(&self, u: f64, v: f64) -> f64 {
        let mut t = Vec::with_capacity(self.coefficients.len());
        terms(self.degree, u, v, &mut t);
        t.iter().zip(&self.coefficients).map(|(a, b)| a * b).sum()
    }

    /// The value at `u, v`, within the range seen over the samples.
    pub fn value(&self, u: f64, v: f64) -> f64 {
        let x = self.raw(u, v);
        if x.is_finite() { x.clamp(self.range[0], self.range[1]) } else { 0.0 }
    }

    /// The root mean square distance of `samples` from the fit, and of the best `1 - trim` share.
    pub fn residual(&self, samples: &[[f64; 3]], trim: f64) -> (f64, f64) {
        let mut r: Vec<f64> =
            samples.iter().map(|s| (self.raw(s[0], s[1]) - s[2]).powi(2)).filter(|v| v.is_finite()).collect();
        if r.is_empty() {
            return (0.0, 0.0);
        }
        let all = (r.iter().sum::<f64>() / r.len() as f64).sqrt();
        r.sort_by(f64::total_cmp);
        let kept = ((r.len() as f64 * (1.0 - trim.clamp(0.0, 0.9))).ceil() as usize).clamp(1, r.len());
        let best = (r.iter().take(kept).sum::<f64>() / kept as f64).sqrt();
        (all, best)
    }
}

/// Log gains `[r, g, b]` reduced to their colour: each minus their mean, so a factor common to
/// all three (the shading of a surface, which is the same for every channel) drops out.
pub fn colour_only(logs: [f64; 3]) -> [f64; 3] {
    let mean = logs.iter().sum::<f64>() / 3.0;
    logs.map(|v| v - mean)
}

/// The value below which a `q` share of `values` lies (`None` when empty).
pub fn quantile(values: &[f64], q: f64) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let i = ((v.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    v.get(i).copied()
}

/// The common scale of all projectors' gains: so that, of each projector's fitted log gains
/// (all channels), no more than a `floor` share needs more than full drive. Each list holds one
/// projector's fitted log gains at its measured samples.
pub fn common_scale(log_gains: &[Vec<f64>], floor: f64) -> Option<f64> {
    let highest = log_gains.iter().filter_map(|g| quantile(g, 1.0 - floor)).fold(f64::MIN, f64::max);
    (highest > f64::MIN).then(|| (-highest).exp())
}

/// The gain map of a `width` x `height` projector: per pixel and channel the channel's `scale`
/// times the exponential of its surface, at most 1.
pub fn gain_map(surfaces: &[Surface; 3], scale: [f64; 3], width: usize, height: usize) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let v = (y as f64 + 0.5) / height.max(1) as f64;
        for x in 0..width {
            let u = (x as f64 + 0.5) / width.max(1) as f64;
            let mut g = [0f32; 3];
            for ((o, s), k) in g.iter_mut().zip(surfaces).zip(scale) {
                *o = (k * s.value(u, v).exp()).clamp(0.0, 1.0) as f32;
            }
            out.push(g);
        }
    }
    out
}

/// Where a projector lights the camera's picture at all: `white` over `black` (same size, camera
/// linear) by more than `share` of its brightest (99th percentile) difference.
pub fn cover(white: &[[f32; 3]], black: &[[f32; 3]], share: f32) -> Vec<bool> {
    if white.len() != black.len() {
        return vec![false; white.len()];
    }
    let diff: Vec<f32> =
        white.iter().zip(black).map(|(w, b)| (w[0] - b[0] + w[1] - b[1] + w[2] - b[2]) / 3.0).collect();
    let mut sorted: Vec<f32> = diff.iter().copied().filter(|v| v.is_finite()).collect();
    sorted.sort_by(f32::total_cmp);
    let top = sorted.get(sorted.len().saturating_sub(1) * 99 / 100).copied().unwrap_or(0.0);
    if top.is_nan() || top <= 0.0 {
        return vec![false; white.len()];
    }
    diff.iter().map(|d| *d > top * share).collect()
}

/// The mean without the lowest and highest tenth, per channel: robust like a median, but finer
/// than the camera's steps where its values are only a step or two (a projector's black).
fn trimmed3(values: impl Iterator<Item = [f32; 3]>) -> Option<[f64; 3]> {
    let all: Vec<[f32; 3]> = values.collect();
    if all.len() < crate::colour::MIN_PIXELS {
        return None;
    }
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut v: Vec<f64> =
            all.iter().filter_map(|p| p.get(c)).map(|v| f64::from(*v)).filter(|v| v.is_finite()).collect();
        v.sort_by(f64::total_cmp);
        let cut = v.len() / 10;
        let kept = v.get(cut..v.len() - cut).filter(|k| !k.is_empty())?;
        *o = kept.iter().sum::<f64>() / kept.len() as f64;
    }
    Some(out)
}

/// `values` (a `w` x `h` picture) smoothed within `mask` by a normalised Gaussian of `sigma`
/// pixels: outside the mask nothing leaks in; pixels outside it keep their value.
pub fn smooth_within(w: usize, h: usize, values: &[[f32; 3]], mask: &[bool], sigma: f32) -> Vec<[f32; 3]> {
    if values.len() != w * h || mask.len() != w * h || sigma.is_nan() || sigma <= 0.0 {
        return values.to_vec();
    }
    let weight: Vec<f32> = mask.iter().map(|m| f32::from(u8::from(*m))).collect();
    let den = raster::gaussian(w, h, &weight, sigma);
    let mut out = values.to_vec();
    for c in 0..3 {
        let plane: Vec<f32> = values
            .iter()
            .zip(mask)
            .map(|(v, m)| if *m { v.get(c).copied().unwrap_or(0.0) } else { 0.0 })
            .collect();
        let num = raster::gaussian(w, h, &plane, sigma);
        for (i, o) in out.iter_mut().enumerate() {
            let (Some(n), Some(d)) = (num.get(i), den.get(i)) else { continue };
            if mask.get(i).copied().unwrap_or(false)
                && *d > 1e-3
                && let Some(s) = o.get_mut(c)
            {
                *s = n / d;
            }
        }
    }
    out
}

/// The black levels in the all-black picture `black` (camera linear): per projector the
/// (trimmed) mean where it alone covers the picture (`covers`, one mask each), minus the room's
/// light (the same where none covers, else 0), and that room light. `None` for a projector that covers
/// nothing alone.
pub fn blacks(black: &[[f32; 3]], covers: &[Vec<bool>]) -> (Vec<Option<[f64; 3]>>, [f64; 3]) {
    let count = |i: usize| covers.iter().filter(|c| c.get(i).copied().unwrap_or(false)).count();
    let room = trimmed3(black.iter().enumerate().filter(|(i, _)| count(*i) == 0).map(|(_, p)| *p))
        .unwrap_or([0.0; 3]);
    let per = covers
        .iter()
        .map(|c| {
            let m = trimmed3(
                black
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| c.get(*i).copied().unwrap_or(false) && count(*i) == 1)
                    .map(|(_, p)| *p),
            )?;
            let mut out = [0.0; 3];
            for ((o, a), b) in out.iter_mut().zip(m).zip(room) {
                *o = (a - b).max(0.0);
            }
            Some(out)
        })
        .collect();
    (per, room)
}

/// Per projector and camera pixel the lift of its black (in its own drive, per channel), where
/// it covers the pixel: the black of the projectors that do not cover it (`blacks`), shared
/// equally among those that do, over this one's white there (`whites`: camera linear, black
/// subtracted). `None` where it does not cover the pixel or its white is too dark to tell.
pub fn lift(
    covers: &[Vec<bool>],
    whites: &[Vec<[f32; 3]>],
    blacks: &[[f64; 3]],
) -> Vec<Vec<Option<[f32; 3]>>> {
    let n = covers.first().map_or(0, Vec::len);
    let mut out = vec![vec![None; n]; covers.len()];
    for i in 0..n {
        let covering: Vec<usize> = (0..covers.len())
            .filter(|p| covers.get(*p).and_then(|c| c.get(i)).copied().unwrap_or(false))
            .collect();
        if covering.is_empty() {
            continue;
        }
        let mut missing = [0.0f64; 3];
        for (q, b) in blacks.iter().enumerate() {
            if !covering.contains(&q) {
                for (m, v) in missing.iter_mut().zip(b) {
                    *m += v.max(0.0);
                }
            }
        }
        let share = covering.len() as f64;
        for p in covering {
            let Some(w) = whites.get(p).and_then(|w| w.get(i)) else { continue };
            if w.iter().any(|v| v.is_nan() || f64::from(*v) <= MIN_READING * 10.0) {
                continue;
            }
            let mut l = [0f32; 3];
            for ((o, m), wc) in l.iter_mut().zip(missing).zip(w) {
                *o = ((m / share / f64::from(*wc)) as f32).clamp(0.0, MAX_LIFT);
            }
            if let Some(slot) = out.get_mut(p).and_then(|o| o.get_mut(i)) {
                *slot = Some(l);
            }
        }
    }
    out
}

/// Projector pixels at most this far from one with a value take its value; farther ones (parts
/// of the picture the camera does not see) the median of all values.
pub const NEAR: f32 = 8.0;

/// Camera-space values brought into a projector's pixels: per projector pixel the value at its
/// camera position (`to_camera`, a `proj_to_cam` map) in the `cw` x `ch` picture; projector pixels
/// without one, or whose camera pixel has none, take the nearest projector pixel's that has one
/// within [`NEAR`], else the median of the values; then a Gaussian blur of `sigma` projector
/// pixels. `fallback` where nothing is known.
pub fn to_projector(
    values: &[Option<[f32; 3]>],
    cw: usize,
    ch: usize,
    to_camera: &crate::objects::Map,
    sigma: f32,
    fallback: [f32; 3],
) -> Vec<[f32; 3]> {
    let (pw, ph) = (to_camera.width, to_camera.height);
    let n = to_camera.len();
    let mut known = vec![false; n];
    let mut out = vec![fallback; n];
    if values.len() == cw * ch {
        for i in 0..n {
            if let Some(v) = to_camera.pixel_in(i, cw, ch).and_then(|c| values.get(c).copied().flatten())
                && let (Some(o), Some(k)) = (out.get_mut(i), known.get_mut(i))
            {
                *o = v;
                *k = true;
            }
        }
    }
    if known.iter().any(|k| *k) && known.iter().any(|k| !*k) {
        let (distance, nearest) = raster::distance(pw, ph, &known);
        let mut median = fallback;
        for (c, m) in median.iter_mut().enumerate() {
            let v: Vec<f64> = out
                .iter()
                .zip(&known)
                .filter(|(_, k)| **k)
                .filter_map(|(o, _)| o.get(c))
                .map(|x| f64::from(*x))
                .collect();
            if let Some(q) = quantile(&v, 0.5) {
                *m = q as f32;
            }
        }
        let filled: Vec<[f32; 3]> = (0..n)
            .map(|i| {
                if known.get(i).copied().unwrap_or(false) {
                    return out.get(i).copied().unwrap_or(fallback);
                }
                if distance.get(i).is_none_or(|d| *d > NEAR) {
                    return median;
                }
                nearest.get(i).copied().flatten().and_then(|s| out.get(s as usize).copied()).unwrap_or(median)
            })
            .collect();
        out = filled;
    }
    if sigma > 0.0 {
        let planes: Vec<Vec<f32>> = (0..3)
            .map(|c| {
                let plane: Vec<f32> = out.iter().map(|v| v.get(c).copied().unwrap_or(0.0)).collect();
                raster::gaussian(pw, ph, &plane, sigma)
            })
            .collect();
        for (i, o) in out.iter_mut().enumerate() {
            for (c, plane) in planes.iter().enumerate() {
                if let (Some(s), Some(v)) = (o.get_mut(c), plane.get(i)) {
                    *s = *v;
                }
            }
        }
    }
    out
}
