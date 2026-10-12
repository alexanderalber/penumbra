//! How one projector maps the surface onto its image, as pure arithmetic: what the output windows
//! draw and what the editor's handles show.
//!
//! The **surface** is the projector's input texture, in texture coordinates (0..1, origin top
//! left). Projectors fed by the same output share a surface. Each projector shows a `region`
//! of it: placed into the projector's image by `fit` (the fitted rectangle), its four corners
//! moved by the corner offsets (a homography from the region onto that quadrilateral), then
//! every point moved by the mesh warp, a smooth (Catmull-Rom) interpolation of offsets given on
//! a grid over the region. All positions on the projector side are in projector sizes: (0, 0)
//! top left of its image (its viewport), (1, 1) bottom right.
//!
//! Where regions of projectors on the same surface overlap, each projector's share of the light
//! falls off towards its region's edge ([`blend_weight`]); the shares add up to one in linear
//! light, which is what the projectors emit when their gamma is the sRGB curve the output
//! encodes with.

use std::sync::atomic::{AtomicU64, Ordering};

use penumbra_calib::Homography;
use penumbra_gpu::MeshVertex;

use crate::out::Fit;
use crate::out::place::place;

/// Most control points across or down a mesh warp.
pub const MAX_MESH: usize = 33;
/// Largest offset a mesh point may have, in projector sizes.
const MAX_OFFSET: f64 = 4.0;
/// Edges of a region closer than this to the surface's border count as on it (no blending
/// there: nothing lies beyond).
const BORDER: f64 = 1e-4;
/// Distance used for a region with no edge inside the surface (it covers all of it).
const FAR: f64 = 1e9;

/// Offsets on a grid over the region, row by row, in projector sizes.
#[derive(Clone, Debug, PartialEq)]
pub struct Mesh {
    cols: usize,
    rows: usize,
    offsets: Vec<[f64; 2]>,
}

impl Mesh {
    /// A flat mesh (all offsets zero); `None` outside 2 to [`MAX_MESH`] points per side.
    pub fn new(cols: usize, rows: usize) -> Option<Mesh> {
        let ok = (2..=MAX_MESH).contains(&cols) && (2..=MAX_MESH).contains(&rows);
        ok.then(|| Mesh { cols, rows, offsets: vec![[0.0, 0.0]; cols * rows] })
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The offset of point (`c`, `r`); zero outside the grid.
    pub fn get(&self, c: usize, r: usize) -> [f64; 2] {
        if c >= self.cols || r >= self.rows {
            return [0.0, 0.0];
        }
        self.offsets.get(r * self.cols + c).copied().unwrap_or([0.0, 0.0])
    }

    /// Sets the offset of point (`c`, `r`), limited to ±[`MAX_OFFSET`]; ignored outside the grid.
    pub fn set(&mut self, c: usize, r: usize, offset: [f64; 2]) {
        if c >= self.cols || r >= self.rows {
            return;
        }
        let i = r * self.cols + c;
        if let Some(o) = self.offsets.get_mut(i) {
            *o = offset.map(|v| if v.is_finite() { v.clamp(-MAX_OFFSET, MAX_OFFSET) } else { 0.0 });
        }
    }

    /// Where point (`c`, `r`) sits in the region (0..1 each way).
    pub fn grid_uv(&self, c: usize, r: usize) -> [f64; 2] {
        [c as f64 / (self.cols - 1).max(1) as f64, r as f64 / (self.rows - 1).max(1) as f64]
    }

    /// Whether every offset is zero (the mesh changes nothing).
    pub fn is_flat(&self) -> bool {
        self.offsets.iter().all(|o| *o == [0.0, 0.0])
    }

    /// The `warp` param's text: one line per grid row, points `dx,dy` separated by spaces.
    /// Empty (or only white space) is no mesh.
    pub fn parse(text: &str) -> Result<Option<Mesh>, String> {
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if lines.is_empty() {
            return Ok(None);
        }
        let mut offsets = Vec::new();
        let mut cols = 0;
        for (r, line) in lines.iter().enumerate() {
            let row: Vec<&str> = line.split_whitespace().collect();
            if r == 0 {
                cols = row.len();
            } else if row.len() != cols {
                return Err(format!("warp: row {} has {} points, the first has {cols}", r + 1, row.len()));
            }
            for point in row {
                let parsed = point.split_once(',').and_then(|(x, y)| {
                    let x: f64 = x.trim().parse().ok()?;
                    let y: f64 = y.trim().parse().ok()?;
                    (x.is_finite() && y.is_finite() && x.abs() <= MAX_OFFSET && y.abs() <= MAX_OFFSET)
                        .then_some([x, y])
                });
                let Some(p) = parsed else {
                    return Err(format!(
                        "warp: `{point}` in row {} is not `dx,dy` (numbers within ±{MAX_OFFSET})",
                        r + 1
                    ));
                };
                offsets.push(p);
            }
        }
        let rows = lines.len();
        if !(2..=MAX_MESH).contains(&cols) || !(2..=MAX_MESH).contains(&rows) {
            return Err(format!("warp: {cols} x {rows} points; 2 to {MAX_MESH} each way"));
        }
        Ok(Some(Mesh { cols, rows, offsets }))
    }

    /// The text [`Mesh::parse`] reads, five decimals (a fifth of a pixel at 16K).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            let row: Vec<String> = (0..self.cols)
                .map(|c| {
                    let [x, y] = self.get(c, r);
                    format!("{},{}", number(x), number(y))
                })
                .collect();
            out.push_str(&row.join(" "));
            out.push('\n');
        }
        out
    }

    /// The offset at `uv` in the region: bicubic Catmull-Rom through the grid points, so the
    /// surface passes through every point and bends smoothly between them. Beyond the outer
    /// points the grid continues linearly.
    pub fn eval(&self, uv: [f64; 2]) -> [f64; 2] {
        let locate = |t: f64, n: usize| {
            let x = (t.clamp(0.0, 1.0)) * (n - 1) as f64;
            let i = (x.floor() as usize).min(n - 2);
            (i as isize, x - i as f64)
        };
        let (i, s) = locate(uv[0], self.cols);
        let (j, t) = locate(uv[1], self.rows);
        let mut column = [[0.0; 2]; 4];
        for (k, out) in column.iter_mut().enumerate() {
            let r = j - 1 + k as isize;
            *out = catmull(
                [self.ghost(i - 1, r), self.ghost(i, r), self.ghost(i + 1, r), self.ghost(i + 2, r)],
                s,
            );
        }
        catmull(column, t)
    }

    /// A copy with `cols` x `rows` points, each the current surface at its place (so changing
    /// the grid size keeps the warp's shape as far as the new grid can hold it).
    pub fn resample(&self, cols: usize, rows: usize) -> Option<Mesh> {
        let mut out = Mesh::new(cols, rows)?;
        for r in 0..rows {
            for c in 0..cols {
                let uv = out.grid_uv(c, r);
                out.set(c, r, self.eval(uv));
            }
        }
        Some(out)
    }

    /// The point at (`c`, `r`), continued linearly one step beyond the grid on each side.
    fn ghost(&self, c: isize, r: isize) -> [f64; 2] {
        // (index, neighbour inward, weight): value = v[index] + weight * (v[index] - v[neighbour]).
        let edge = |i: isize, n: usize| -> (usize, usize, f64) {
            let last = n - 1;
            if i < 0 {
                (0, 1, 1.0)
            } else if i as usize > last {
                (last, last - 1, 1.0)
            } else {
                (i as usize, i as usize, 0.0)
            }
        };
        let (ca, cb, kc) = edge(c, self.cols);
        let (ra, rb, kr) = edge(r, self.rows);
        let along = |row: usize| {
            let a = self.get(ca, row);
            let b = self.get(cb, row);
            [a[0] + kc * (a[0] - b[0]), a[1] + kc * (a[1] - b[1])]
        };
        let a = along(ra);
        let b = along(rb);
        [a[0] + kr * (a[0] - b[0]), a[1] + kr * (a[1] - b[1])]
    }
}

/// A number with at most five decimals and no trailing zeros.
pub(crate) fn number(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".to_string() } else { s.to_string() }
}

/// Catmull-Rom between `p[1]` (t = 0) and `p[2]` (t = 1).
fn catmull(p: [[f64; 2]; 4], t: f64) -> [f64; 2] {
    let [p0, p1, p2, p3] = p;
    let (t2, t3) = (t * t, t * t * t);
    let f = |a: f64, b: f64, c: f64, d: f64| {
        0.5 * (2.0 * b
            + (c - a) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
            + (3.0 * b - a - 3.0 * c + d) * t3)
    };
    [f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])]
}

/// What a projector's params say about its mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub fit: Fit,
    /// x, y, width, height of the surface shown, in surface sizes.
    pub region: [f64; 4],
    /// Offsets of the corners (top left, top right, bottom right, bottom left) from their
    /// fitted place, in projector sizes.
    pub corners: [[f64; 2]; 4],
    pub mesh: Option<Mesh>,
    pub blend: bool,
    /// Exponent on the blend share (1: the shares add up in linear light).
    pub blend_gamma: f64,
    /// Draw the alignment grid instead of the input.
    pub pattern: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            fit: Fit::Fit,
            region: [0.0, 0.0, 1.0, 1.0],
            corners: [[0.0; 2]; 4],
            mesh: None,
            blend: true,
            blend_gamma: 1.0,
            pattern: false,
        }
    }
}

impl Setup {
    /// The region, clipped to the surface; `None` when nothing of it is left.
    pub fn clipped_region(&self) -> Option<[f64; 4]> {
        let [x, y, w, h] = self.region.map(|v| if v.is_finite() { v } else { 0.0 });
        let x0 = x.clamp(0.0, 1.0);
        let y0 = y.clamp(0.0, 1.0);
        let x1 = (x + w).clamp(0.0, 1.0);
        let y1 = (y + h).clamp(0.0, 1.0);
        (x1 - x0 > 1e-6 && y1 - y0 > 1e-6).then_some([x0, y0, x1 - x0, y1 - y0])
    }
}

/// Unique id of every computed [`Mapping`], so the outputs know when to upload new geometry.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One projector's mapping, computed for its image size and its input's size.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub id: u64,
    /// The projector's image in pixels.
    pub raster: (u32, u32),
    /// The part of the surface shown (the region, cropped by `fill`), in surface sizes.
    pub region: [f64; 4],
    /// Where the corners sit before their offsets (the fitted rectangle).
    pub base: [[f64; 2]; 4],
    /// Where the corners sit (base plus offsets).
    pub corners: [[f64; 2]; 4],
    /// From the region (unit square) to the projector, before the mesh.
    pub homography: Homography,
    pub mesh: Option<Mesh>,
    /// Triangles over the region, positions in the projector's clip space.
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
    pub blend: bool,
    pub blend_gamma: f64,
    pub pattern: bool,
}

impl Mapping {
    /// Computes the mapping of `setup` for a `raster`-sized projector image and a `tex`-sized
    /// input. Fails with a message for the node when the corners cross or nothing is visible.
    pub fn new(setup: &Setup, raster: (u32, u32), tex: (u32, u32)) -> Result<Mapping, String> {
        let region = setup.clipped_region().ok_or("the region is empty")?;
        let region_px = (
            ((region[2] * f64::from(tex.0)).round() as u32).max(1),
            ((region[3] * f64::from(tex.1)).round() as u32).max(1),
        );
        let raster = (raster.0.max(1), raster.1.max(1));
        let placed =
            place(setup.fit, [0.0, 0.0, 1.0, 1.0], raster, region_px).ok_or("nothing would be visible")?;
        let (rw, rh) = (f64::from(raster.0), f64::from(raster.1));
        let x0 = f64::from(placed.dest.x) / rw;
        let y0 = f64::from(placed.dest.y) / rh;
        let x1 = f64::from(placed.dest.x + placed.dest.w) / rw;
        let y1 = f64::from(placed.dest.y + placed.dest.h) / rh;
        let base = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let [o0, o1, s0, s1] =
            [placed.uv_offset[0], placed.uv_offset[1], placed.uv_scale[0], placed.uv_scale[1]].map(f64::from);
        let region = [region[0] + o0 * region[2], region[1] + o1 * region[3], region[2] * s0, region[3] * s1];
        let mut corners = base;
        for (c, o) in corners.iter_mut().zip(&setup.corners) {
            let o = o.map(|v| if v.is_finite() { v.clamp(-MAX_OFFSET, MAX_OFFSET) } else { 0.0 });
            *c = [c[0] + o[0], c[1] + o[1]];
        }
        let homography = Homography::square_to_quad(corners).map_err(|e| e.to_string())?;
        let mesh = setup.mesh.clone().filter(|m| !m.is_flat());
        let (vertices, indices) = tessellate(&homography, mesh.as_ref(), region)?;
        Ok(Mapping {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            raster,
            region,
            base,
            corners,
            homography,
            mesh: setup.mesh.clone(),
            vertices,
            indices,
            blend: setup.blend,
            blend_gamma: if setup.blend_gamma.is_finite() { setup.blend_gamma.clamp(0.1, 10.0) } else { 1.0 },
            pattern: setup.pattern,
        })
    }

    /// Where mesh point (`c`, `r`) sits before its offset (on the corner-pinned region).
    pub fn anchor(&self, uv: [f64; 2]) -> Option<[f64; 2]> {
        self.homography.apply(uv)
    }

    /// Where a point of the region lands in the projector image, mesh included.
    pub fn project(&self, uv: [f64; 2]) -> Option<[f64; 2]> {
        let q = self.homography.apply(uv)?;
        let o = self.mesh.as_ref().map_or([0.0, 0.0], |m| m.eval(uv));
        Some([q[0] + o[0], q[1] + o[1]])
    }
}

/// Share of the projected area whose need `res.analyze` meets: the 95th percentile, not the
/// maximum, which a grazing corner would push to absurd values.
pub const DEMAND_PERCENTILE: f64 = 0.95;
/// Cells across and down the region for [`Mapping::surface_demand`].
const DEMAND_SAMPLES: usize = 32;

impl Mapping {
    /// The resolution (across, down, in surface pixels) the surface needs so that one of its
    /// pixels covers one pixel of this mapper's picture, when the picture itself needs `out`
    /// pixels (its own size when it is shown one to one). Measured per cell of a grid over the
    /// region whose centre lands inside the picture, weighted by the area the cell covers there,
    /// at [`DEMAND_PERCENTILE`]; `None` when nothing of it is visible.
    pub fn surface_demand(&self, out: [f64; 2]) -> Option<[f64; 2]> {
        let n = DEMAND_SAMPLES;
        let step = 1.0 / n as f64;
        // Where the corners of an n x n grid of cells land, in needed picture pixels.
        let mut grid = Vec::with_capacity((n + 1) * (n + 1));
        for j in 0..=n {
            for i in 0..=n {
                let p = self.project([i as f64 * step, j as f64 * step]);
                grid.push(p.map(|p| [p[0] * out[0], p[1] * out[1]]));
            }
        }
        let at = |i: usize, j: usize| grid.get(j * (n + 1) + i).copied().flatten();
        let [_, _, rw, rh] = self.region;
        let mut across = Vec::with_capacity(n * n);
        let mut down = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                let (Some(a), Some(b), Some(c), Some(d)) =
                    (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1))
                else {
                    continue;
                };
                let centre = [(a[0] + b[0] + c[0] + d[0]) / 4.0, (a[1] + b[1] + c[1] + d[1]) / 4.0];
                if !(0.0..=out[0]).contains(&centre[0]) || !(0.0..=out[1]).contains(&centre[1]) {
                    continue;
                }
                // Needed picture pixels per region unit, across the cell and down it.
                let ds =
                    [(b[0] + d[0] - a[0] - c[0]) / (2.0 * step), (b[1] + d[1] - a[1] - c[1]) / (2.0 * step)];
                let dt =
                    [(c[0] + d[0] - a[0] - b[0]) / (2.0 * step), (c[1] + d[1] - a[1] - b[1]) / (2.0 * step)];
                let det = ds[0] * dt[1] - dt[0] * ds[1];
                if !det.is_finite() || det.abs() < 1e-12 {
                    continue;
                }
                // The inverse: region units per needed picture pixel, scaled to surface units.
                let du = [dt[1] / det * rw, -dt[0] / det * rw];
                let dv = [-ds[1] / det * rh, ds[0] / det * rh];
                across.push((1.0 / du[0].hypot(du[1]), det.abs()));
                down.push((1.0 / dv[0].hypot(dv[1]), det.abs()));
            }
        }
        Some([percentile(across, DEMAND_PERCENTILE)?, percentile(down, DEMAND_PERCENTILE)?])
    }
}

/// The `q` quantile of equally weighted values (as [`percentile`] with equal weights);
/// `None` without a finite value.
pub(crate) fn quantile(mut v: Vec<f64>, q: f64) -> Option<f64> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    let k = ((q.clamp(0.0, 1.0) * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
    let (_, x, _) = v.select_nth_unstable_by(k, f64::total_cmp);
    Some(*x)
}

/// The weighted `q` quantile of `(value, weight)` pairs; `None` without weight.
fn percentile(mut v: Vec<(f64, f64)>, q: f64) -> Option<f64> {
    v.retain(|(x, w)| x.is_finite() && w.is_finite() && *w > 0.0);
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = v.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return None;
    }
    let mut sum = 0.0;
    for (x, w) in &v {
        sum += w;
        if sum >= q * total {
            return Some(*x);
        }
    }
    v.last().map(|(x, _)| *x)
}

/// Triangles over the region: positions through the homography (in homogeneous form, so the
/// texture is interpolated perspective-correctly across each triangle) plus the mesh offsets.
fn tessellate(
    h: &Homography,
    mesh: Option<&Mesh>,
    region: [f64; 4],
) -> Result<(Vec<MeshVertex>, Vec<u32>), String> {
    // Without a mesh the homography alone is exact on any triangulation; with one, a few
    // triangles per mesh cell follow its curves closely.
    let n = mesh.map_or(16, |m| ((m.cols().max(m.rows()) - 1) * 6).clamp(32, 192));
    let center = h.apply_homogeneous([0.5, 0.5])[2];
    if !center.is_finite() || center.abs() < 1e-12 {
        return Err("the corners make the picture degenerate".to_string());
    }
    let mut vertices = Vec::with_capacity((n + 1) * (n + 1));
    for j in 0..=n {
        for i in 0..=n {
            let uv = [i as f64 / n as f64, j as f64 / n as f64];
            let [x, y, w] = h.apply_homogeneous(uv).map(|v| v / center);
            if w.is_nan() || w <= 1e-9 || !x.is_finite() || !y.is_finite() {
                return Err("the corners make the picture pass through infinity".to_string());
            }
            let o = mesh.map_or([0.0, 0.0], |m| m.eval(uv));
            let p = [x / w + o[0], y / w + o[1]];
            let ndc = [p[0] * 2.0 - 1.0, 1.0 - p[1] * 2.0];
            vertices.push(MeshVertex {
                pos: [(ndc[0] * w) as f32, (ndc[1] * w) as f32, 0.0, w as f32],
                uv: [(region[0] + uv[0] * region[2]) as f32, (region[1] + uv[1] * region[3]) as f32],
            });
        }
    }
    let mut indices = Vec::with_capacity(n * n * 6);
    let stride = (n + 1) as u32;
    for j in 0..n as u32 {
        for i in 0..n as u32 {
            let a = j * stride + i;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }
    Ok((vertices, indices))
}

/// How far `s` lies inside `region`, towards the nearest edge that is not on the surface's
/// border, in `tex` pixels: 0 outside, [`FAR`] when no edge lies inside the surface.
fn inset(region: [f64; 4], s: [f64; 2], tex: (f64, f64)) -> f64 {
    let [x, y, w, h] = region;
    if s[0] < x || s[0] > x + w || s[1] < y || s[1] > y + h {
        return 0.0;
    }
    let mut d = FAR;
    if x > BORDER {
        d = d.min((s[0] - x) * tex.0);
    }
    if x + w < 1.0 - BORDER {
        d = d.min((x + w - s[0]) * tex.0);
    }
    if y > BORDER {
        d = d.min((s[1] - y) * tex.1);
    }
    if y + h < 1.0 - BORDER {
        d = d.min((y + h - s[1]) * tex.1);
    }
    d
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The share of light the projector showing `own` gives surface point `s` when `peers` show
/// the same surface (the reference for `projection.wgsl`, which computes it per pixel). Each
/// projector's raw share is its distance to its region's inner edge over the sum of all; the
/// shares are shaped by smoothstep (so the falloff starts and ends without a kink) and
/// normalised again, then raised to `gamma`.
pub fn blend_weight(own: [f64; 4], peers: &[[f64; 4]], s: [f64; 2], tex: (f64, f64), gamma: f64) -> f64 {
    let mine = inset(own, s, tex).max(1e-9);
    let others: Vec<f64> = peers.iter().map(|p| inset(*p, s, tex)).collect();
    let sum = mine + others.iter().sum::<f64>();
    let shaped = smooth(mine / sum);
    let total = shaped + others.iter().map(|d| smooth(d / sum)).sum::<f64>();
    let w = if total > 0.0 { shaped / total } else { 1.0 };
    w.powf(gamma)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn mesh_text_round_trips() {
        let mut m = Mesh::new(3, 2).unwrap();
        m.set(1, 0, [0.0125, -0.5]);
        m.set(2, 1, [1e-7, 0.33333333]);
        let text = m.to_text();
        assert_eq!(text, "0,0 0.0125,-0.5 0,0\n0,0 0,0 0,0.33333\n");
        let back = Mesh::parse(&text).unwrap().unwrap();
        assert_eq!((back.cols(), back.rows()), (3, 2));
        assert_eq!(back.get(1, 0), [0.0125, -0.5]);
        assert_eq!(Mesh::parse("  \n ").unwrap(), None);
        assert!(Mesh::parse("0,0 0,0\n0,0").is_err());
        assert!(Mesh::parse("0,0 x,0\n0,0 0,0").is_err());
        assert!(Mesh::parse("0,0 9,0\n0,0 0,0").is_err());
        assert!(Mesh::parse("0,0\n0,0").is_err());
        assert!(Mesh::parse("0,0 NaN,0\n0,0 0,0").is_err());
    }

    #[test]
    fn mesh_passes_through_its_points_and_is_smooth() {
        let mut m = Mesh::new(5, 4).unwrap();
        m.set(2, 1, [0.02, -0.01]);
        m.set(4, 3, [-0.03, 0.0]);
        for r in 0..4 {
            for c in 0..5 {
                assert!(close(m.eval(m.grid_uv(c, r)), m.get(c, r)), "{c} {r}");
            }
        }
        // Between points the bump falls off without jumps.
        let mut last = m.eval([0.0, 1.0 / 3.0]);
        for k in 1..=200 {
            let p = m.eval([k as f64 / 200.0, 1.0 / 3.0]);
            assert!((p[0] - last[0]).abs() < 0.002, "{k}");
            last = p;
        }
        // A flat mesh stays flat; a 2 x 2 mesh is bilinear.
        assert!(close(Mesh::new(3, 3).unwrap().eval([0.3, 0.7]), [0.0, 0.0]));
        let mut b = Mesh::new(2, 2).unwrap();
        b.set(1, 1, [0.1, 0.1]);
        assert!(close(b.eval([0.5, 0.5]), [0.025, 0.025]));
    }

    #[test]
    fn resampling_keeps_the_shape() {
        let mut m = Mesh::new(3, 3).unwrap();
        m.set(1, 1, [0.04, 0.0]);
        let fine = m.resample(5, 5).unwrap();
        assert!(close(fine.get(2, 2), [0.04, 0.0]));
        assert!(close(fine.get(1, 1), m.eval([0.25, 0.25])));
        assert!(m.resample(1, 5).is_none());
    }

    #[test]
    fn default_setup_fills_the_projector() {
        let m = Mapping::new(&Setup::default(), (1920, 1080), (1920, 1080)).unwrap();
        assert!(close(m.corners[0], [0.0, 0.0]) && close(m.corners[2], [1.0, 1.0]));
        assert!(close(m.project([0.5, 0.5]).unwrap(), [0.5, 0.5]));
        // Clip space: the top left corner is (-1, 1).
        let v = m.vertices.first().unwrap();
        assert_eq!([v.pos[0] / v.pos[3], v.pos[1] / v.pos[3]], [-1.0, 1.0]);
        assert_eq!(v.uv, [0.0, 0.0]);
        assert_eq!(m.indices.len(), 16 * 16 * 6);
    }

    #[test]
    fn fit_letterboxes_and_fill_crops_the_region() {
        let square = (1000, 1000);
        let fit = Mapping::new(&Setup::default(), (1920, 1080), square).unwrap();
        assert!((fit.base[0][0] - 420.0 / 1920.0).abs() < 1e-6, "{:?}", fit.base);
        let fill = Mapping::new(&Setup { fit: Fit::Fill, ..Setup::default() }, (1920, 1080), square).unwrap();
        assert!(close(fill.base[2], [1.0, 1.0]));
        assert!((fill.region[3] - 0.5625).abs() < 1e-6, "{:?}", fill.region);
        // Half the surface, stretched.
        let half = Setup { fit: Fit::Stretch, region: [0.5, 0.0, 0.5, 1.0], ..Setup::default() };
        let m = Mapping::new(&half, (1920, 1080), (3840, 1080)).unwrap();
        assert_eq!(m.region, [0.5, 0.0, 0.5, 1.0]);
        assert!(
            Mapping::new(&Setup { region: [1.0, 0.0, 0.5, 1.0], ..Setup::default() }, (10, 10), (10, 10))
                .is_err()
        );
    }

    #[test]
    fn corner_offsets_pin_the_corners() {
        let setup = Setup {
            fit: Fit::Stretch,
            corners: [[0.05, 0.02], [-0.1, 0.0], [0.0, -0.03], [0.02, 0.0]],
            ..Setup::default()
        };
        let m = Mapping::new(&setup, (1920, 1080), (1920, 1080)).unwrap();
        assert!(close(m.project([0.0, 0.0]).unwrap(), [0.05, 0.02]));
        assert!(close(m.project([1.0, 0.0]).unwrap(), [0.9, 0.0]));
        assert!(close(m.project([1.0, 1.0]).unwrap(), [1.0, 0.97]));
        // The first vertex is the top left corner, perspective included.
        let v = m.vertices.first().unwrap();
        let ndc = [v.pos[0] / v.pos[3], v.pos[1] / v.pos[3]];
        assert!((ndc[0] - (0.05 * 2.0 - 1.0)).abs() < 1e-5 && (ndc[1] - (1.0 - 0.04)).abs() < 1e-5);
        // A bow tie is refused.
        let crossed = Setup { corners: [[1.0, 0.0], [-1.0, 0.0], [0.0, 0.0], [0.0, 0.0]], ..setup };
        assert!(Mapping::new(&crossed, (1920, 1080), (1920, 1080)).is_err());
    }

    #[test]
    fn surface_demand_follows_the_projected_size() {
        let near =
            |a: [f64; 2], b: [f64; 2], tol: f64| (a[0] - b[0]).abs() < tol && (a[1] - b[1]).abs() < tol;
        // The whole surface onto the whole picture: one surface pixel per picture pixel.
        let m = Mapping::new(&Setup::default(), (1920, 1080), (1920, 1080)).unwrap();
        let d = m.surface_demand([1920.0, 1080.0]).unwrap();
        assert!(near(d, [1920.0, 1080.0], 1e-3), "{d:?}");
        // The picture shown at half its size needs half.
        let d = m.surface_demand([960.0, 540.0]).unwrap();
        assert!(near(d, [960.0, 540.0], 1e-3), "{d:?}");
        // Half the surface stretched over the picture: the whole surface needs twice as wide.
        let half = Setup { fit: Fit::Stretch, region: [0.0, 0.0, 0.5, 1.0], ..Setup::default() };
        let m = Mapping::new(&half, (1920, 1080), (1920, 1080)).unwrap();
        let d = m.surface_demand([1920.0, 1080.0]).unwrap();
        assert!(near(d, [3840.0, 1080.0], 1e-3), "{d:?}");
        // Keystoned: the top edge shrunk to 60 % across. Most of the picture still needs more
        // than the narrow top; the 95th percentile lies near the full width.
        let keystone = Setup {
            fit: Fit::Stretch,
            corners: [[0.2, 0.0], [-0.2, 0.0], [0.0, 0.0], [0.0, 0.0]],
            ..Setup::default()
        };
        let m = Mapping::new(&keystone, (1920, 1080), (1920, 1080)).unwrap();
        let d = m.surface_demand([1920.0, 1080.0]).unwrap();
        assert!(d[0] > 1700.0 && d[0] <= 1921.0, "{d:?}");
        // Nothing visible: no demand.
        let away = Setup { corners: [[2.0, 0.0], [2.0, 0.0], [2.0, 0.0], [2.0, 0.0]], ..Setup::default() };
        let m = Mapping::new(&away, (100, 100), (100, 100)).unwrap();
        assert_eq!(m.surface_demand([100.0, 100.0]), None);
    }

    #[test]
    fn mesh_moves_the_vertices() {
        let mut mesh = Mesh::new(3, 3).unwrap();
        mesh.set(1, 1, [0.01, 0.0]);
        let setup = Setup { fit: Fit::Stretch, mesh: Some(mesh), ..Setup::default() };
        let m = Mapping::new(&setup, (1920, 1080), (1920, 1080)).unwrap();
        assert!(close(m.project([0.5, 0.5]).unwrap(), [0.51, 0.5]));
        assert!(close(m.project([0.0, 0.5]).unwrap(), [0.0, 0.5]));
        assert_eq!(m.indices.len(), 32 * 32 * 6);
        let centre = m.vertices.get(16 * 33 + 16).unwrap();
        assert!((centre.pos[0] / centre.pos[3] - 0.02).abs() < 1e-6);
    }

    #[test]
    fn blend_shares_add_up_to_one() {
        let tex = (3840.0, 1080.0);
        let a = [0.0, 0.0, 0.6, 1.0];
        let b = [0.4, 0.0, 0.6, 1.0];
        for k in 0..=100 {
            let s = [k as f64 / 100.0, 0.3];
            let wa = if s[0] <= 0.6 { blend_weight(a, &[b], s, tex, 1.0) } else { 0.0 };
            let wb = if s[0] >= 0.4 { blend_weight(b, &[a], s, tex, 1.0) } else { 0.0 };
            assert!((wa + wb - 1.0).abs() < 1e-9, "{s:?}: {wa} + {wb}");
        }
        // Outside the overlap each has all of it; in the middle of it, half; near the top
        // border (where neither has an inner edge) the horizontal position still decides.
        assert_eq!(blend_weight(a, &[b], [0.2, 0.5], tex, 1.0), 1.0);
        assert!((blend_weight(a, &[b], [0.5, 0.5], tex, 1.0) - 0.5).abs() < 1e-9);
        assert!(blend_weight(a, &[b], [0.42, 0.001], tex, 1.0) > 0.95);
        // Three in a row.
        let c = [0.8, 0.0, 0.2, 1.0];
        let a2 = [0.0, 0.0, 0.45, 1.0];
        let b2 = [0.35, 0.0, 0.5, 1.0];
        let s = [0.82, 0.5];
        let total = blend_weight(b2, &[a2, c], s, tex, 1.0) + blend_weight(c, &[a2, b2], s, tex, 1.0);
        assert!((total - 1.0).abs() < 1e-9);
    }
}
