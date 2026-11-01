//! Meshes that move: which vertices follow which part of a file's animation, and their
//! positions and normals at a time.
//!
//! A [`Motion`] belongs to the [`Mesh`](crate::Mesh) it was read with. The mesh holds the
//! vertices as the file stores them (its rest pose). Every vertex belongs to one piece: one
//! placed geometry of the file. At a time, each piece gets a matrix that takes its vertices
//! from the rest pose to where the animation puts them; [`Motion::deform`] applies it to
//! positions and normals. The matrices live in the mesh's own space, so whatever was done to
//! the mesh after reading (turning it to z up, smoothing its normals) carries over.
//!
//! A piece with a skin moves each vertex by several matrices instead, one per bone that weighs
//! on the vertex's control point, blended by the weights (linear blend skinning). Normals take
//! the blend of the bones' normal matrices and are made unit length again.

use std::sync::Arc;

use fbx_reader::{Animation, Hierarchy, NodeWorld, Skin};

use crate::{Up, Vertex, normal_matrix, normalize};

type Mat = [[f64; 4]; 4];

const IDENTITY: Mat =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// A take of the file: an animation with a name and a span.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub name: String,
    /// Seconds on the file's clock.
    pub start: f64,
    pub stop: f64,
}

impl Take {
    pub fn duration(&self) -> f64 {
        (self.stop - self.start).max(0.0)
    }
}

/// A skin as the motion needs it: per bone its hierarchy node and geometry-to-bone matrix, and
/// per control point the bones that weigh on it with their weights, which add up to one.
#[derive(Clone, Debug, PartialEq)]
pub struct Bind {
    clusters: Vec<(usize, Mat)>,
    /// Per control point, where its influences start; one more entry at the end.
    first: Vec<u32>,
    influences: Vec<(u32, f32)>,
}

impl Bind {
    /// The clusters and weights of `skin`. A control point whose weights add up to zero or less
    /// (or to no number) gets none: it moves with its piece's model.
    pub fn from_skin(skin: &Skin) -> Bind {
        let clusters = skin.clusters.iter().map(|c| (c.bone, c.geometry_to_bone)).collect();
        let mut first = Vec::with_capacity(skin.points() + 1);
        let mut influences = Vec::new();
        first.push(0);
        for p in 0..skin.points() {
            let list = skin.influences(p);
            let total: f64 = list.iter().map(|i| i.weight).sum();
            if total.is_finite() && total > 0.0 {
                influences.extend(list.iter().map(|i| (i.cluster, (i.weight / total) as f32)));
            }
            first.push(influences.len() as u32);
        }
        Bind { clusters, first, influences }
    }

    fn influences(&self, point: u32) -> &[(u32, f32)] {
        let p = point as usize;
        let a = self.first.get(p).copied().unwrap_or(0) as usize;
        let b = self.first.get(p + 1).copied().unwrap_or(0) as usize;
        self.influences.get(a..b).unwrap_or(&[])
    }
}

/// One placed geometry: the model that carries it, its geometry-to-file matrix at rest, and
/// its skin if it has one.
#[derive(Clone, Debug, PartialEq)]
struct Piece {
    node: usize,
    rest: Mat,
    skin: Option<Arc<Bind>>,
    /// Mesh space at rest to the piece's geometry space; `None` when the rest matrix cannot
    /// be inverted (a zero scale): the piece's vertices are then posed from [`Motion::local`].
    unbind: Option<Mat>,
}

/// A piece's posing matrix as `f32` rows, its normal matrix, and whether it applies to
/// [`Motion::local`].
type Turn = ([[f32; 4]; 3], [[f32; 3]; 3], bool);

/// A bone's posing matrix and normal matrix as `f32` rows.
type Blend = ([[f32; 4]; 3], [[f32; 3]; 3]);

/// The animation of a mesh read from a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pub takes: Vec<Take>,
    /// Frames per second the file was made at.
    pub frame_rate: f64,
    /// Per vertex of the mesh: the piece it moves with.
    pub piece: Vec<u32>,
    /// Per vertex: the control point of its piece's geometry it was made from.
    pub point: Vec<u32>,
    /// Per vertex: its position and normal in its piece's geometry space, as the file stores
    /// them. Used for pieces whose rest pose collapses them (a zero scale), where the mesh's
    /// vertices no longer tell.
    pub local: Vec<Vertex>,
    pieces: Vec<Piece>,
    /// Per hierarchy node: a piece's model or one of its ancestors, so its animation moves
    /// vertices.
    carries: Vec<bool>,
    hierarchy: Hierarchy,
    animation: Animation,
    /// File space to mesh space.
    root: Mat,
}

impl Motion {
    /// `pieces` are (hierarchy node, geometry to file space at rest, skin) per piece; `root`
    /// takes file space to the mesh's. `None` when no take moves a model that carries a piece
    /// or a bone of a piece's skin.
    pub(crate) fn new(
        hierarchy: Hierarchy,
        animation: Animation,
        pieces: Vec<(usize, Mat, Option<Arc<Bind>>)>,
        root: Mat,
        piece: Vec<u32>,
        point: Vec<u32>,
        local: Vec<Vertex>,
    ) -> Option<Motion> {
        let mut carries = vec![false; hierarchy.nodes.len()];
        let bones = pieces.iter().flat_map(|(_, _, s)| s.iter().flat_map(|b| b.clusters.iter().map(|c| c.0)));
        let starts: Vec<usize> = pieces.iter().map(|p| p.0).chain(bones).collect();
        for node in starts {
            let mut at = Some(node);
            while let Some(i) = at {
                match carries.get_mut(i) {
                    Some(c) if !*c => *c = true,
                    _ => break,
                }
                at = hierarchy.nodes.get(i).and_then(|n| n.parent).filter(|p| *p < i);
            }
        }
        let moves =
            |t: usize| animation.moves_any(&hierarchy, t, |i| carries.get(i).copied().unwrap_or(false));
        if !(0..animation.takes.len()).any(moves) {
            return None;
        }
        let takes = animation
            .takes
            .iter()
            .map(|t| Take { name: t.name.clone(), start: t.start, stop: t.stop })
            .collect();
        let pieces =
            pieces.into_iter().map(|(node, rest, skin)| Piece { node, rest, skin, unbind: None }).collect();
        let mut m = Motion {
            takes,
            frame_rate: animation.frame_rate,
            piece,
            point,
            local,
            pieces,
            carries,
            hierarchy,
            animation,
            root,
        };
        m.rebind();
        Some(m)
    }

    fn rebind(&mut self) {
        for p in &mut self.pieces {
            p.unbind = inverse(&mul(&self.root, &p.rest));
        }
    }

    /// Whether take `take` moves any vertex.
    pub fn moves(&self, take: usize) -> bool {
        self.animation.moves_any(&self.hierarchy, take, |i| self.carries.get(i).copied().unwrap_or(false))
    }

    /// Follows [`Mesh::to_z_up`](crate::Mesh::to_z_up) on the mesh.
    pub fn to_z_up(&mut self, from: Up) {
        if from == Up::Z {
            return;
        }
        let turn = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        self.root = mul(&turn, &self.root);
        self.rebind();
    }

    /// Follows [`Mesh::smooth_normals`](crate::Mesh::smooth_normals): `origins` are the
    /// vertices the new ones at the end were copied from.
    pub fn split(&mut self, origins: &[u32]) {
        for o in origins {
            let o = *o as usize;
            self.piece.push(self.piece.get(o).copied().unwrap_or(0));
            self.point.push(self.point.get(o).copied().unwrap_or(0));
            self.local.push(self.local.get(o).copied().unwrap_or_default());
        }
    }

    /// Per piece, the matrix to its pose `seconds` into take `take` (the take's start is 0),
    /// in the mesh's space: from the rest pose (`false`) or, for a piece collapsed at rest,
    /// from its geometry space (`true`, applied to [`Motion::local`]). A skinned piece's
    /// vertices follow their bones instead; its matrix moves only those without weights.
    pub fn matrices(&self, take: usize, seconds: f64) -> Vec<(Mat, bool)> {
        let worlds = self.worlds(take, seconds);
        self.pieces.iter().map(|p| self.rigid(p, &worlds)).collect()
    }

    fn worlds(&self, take: usize, seconds: f64) -> Vec<NodeWorld> {
        let start = self.takes.get(take).map_or(0.0, |t| t.start);
        self.animation.worlds(&self.hierarchy, take, start + seconds)
    }

    /// Geometry to file space at a time, as a posing matrix of `p` (see [`Motion::matrices`]).
    fn posing(&self, p: &Piece, geometry_to_file: &Mat) -> (Mat, bool) {
        let posed = mul(&self.root, geometry_to_file);
        let (m, local) = match &p.unbind {
            Some(unbind) => (mul(&posed, unbind), false),
            None => (posed, true),
        };
        if m.iter().flatten().all(|x| x.is_finite()) {
            (m, local)
        } else if local {
            // Where the rest pose has the vertices.
            (mul(&self.root, &p.rest), true)
        } else {
            (IDENTITY, false)
        }
    }

    fn rigid(&self, p: &Piece, worlds: &[NodeWorld]) -> (Mat, bool) {
        match worlds.get(p.node) {
            Some(w) => self.posing(p, &w.geometry),
            None => (IDENTITY, false),
        }
    }

    /// The vertices of `rest` (the mesh's, at rest) `seconds` into take `take`, written into
    /// `out`. Texture coordinates stay; positions and normals move with their piece, or with
    /// their bones in a skinned piece.
    pub fn deform(&self, rest: &[Vertex], take: usize, seconds: f64, out: &mut Vec<Vertex>) {
        let worlds = self.worlds(take, seconds);
        let rows = |m: &Mat| [0, 1, 2].map(|r| m.get(r).map_or([0.0; 4], |row| row.map(|x| x as f32)));
        let turns: Vec<Turn> = self
            .pieces
            .iter()
            .map(|p| {
                let (m, local) = self.rigid(p, &worlds);
                (rows(&m), normal_matrix(&m).map(|row| row.map(|x| x as f32)), local)
            })
            .collect();
        // Per skinned piece, per bone: the bone's posing matrix at this time.
        let blends: Vec<Vec<Blend>> = self
            .pieces
            .iter()
            .map(|p| {
                let Some(bind) = &p.skin else { return Vec::new() };
                bind.clusters
                    .iter()
                    .map(|(bone, to_bone)| {
                        let world = worlds.get(*bone).map_or(IDENTITY, |w| w.world);
                        let (m, _) = self.posing(p, &mul(&world, to_bone));
                        (rows(&m), normals_of(&m).map(|row| row.map(|x| x as f32)))
                    })
                    .collect()
            })
            .collect();
        out.clear();
        out.reserve(rest.len());
        for (i, v) in rest.iter().enumerate() {
            let k = self.piece.get(i).copied().unwrap_or(0) as usize;
            let Some((m, n, local)) = turns.get(k) else {
                out.push(*v);
                continue;
            };
            let from = if *local { self.local.get(i).unwrap_or(v) } else { v };
            let list = match self.pieces.get(k).and_then(|p| p.skin.as_deref()) {
                Some(bind) => bind.influences(self.point.get(i).copied().unwrap_or(u32::MAX)),
                None => &[],
            };
            let (m, n) = match blends.get(k) {
                Some(bones) if !list.is_empty() => blend(list, bones),
                _ => (*m, *n),
            };
            let [x, y, z] = from.position;
            let p = m.map(|r| r[0] * x + r[1] * y + r[2] * z + r[3]);
            let [a, b, c] = from.normal;
            let q = normalize(n.map(|r| r[0] * a + r[1] * b + r[2] * c));
            let position = if p.iter().all(|c| c.is_finite()) { p } else { v.position };
            out.push(Vertex { position, normal: q, uv: v.uv });
        }
    }
}

/// The bones' posing and normal matrices summed by the weights in `list`.
fn blend(list: &[(u32, f32)], bones: &[Blend]) -> Blend {
    let mut m = [[0.0f32; 4]; 3];
    let mut n = [[0.0f32; 3]; 3];
    for (c, w) in list {
        let Some((bm, bn)) = bones.get(*c as usize) else { continue };
        for (row, src) in m.iter_mut().zip(bm) {
            for (x, s) in row.iter_mut().zip(src) {
                *x += w * s;
            }
        }
        for (row, src) in n.iter_mut().zip(bn) {
            for (x, s) in row.iter_mut().zip(src) {
                *x += w * s;
            }
        }
    }
    (m, n)
}

/// The normal matrix of `m` at its true scale (the inverse transpose of its 3x3), so that
/// several can be blended by weights; [`normal_matrix`] when `m` is singular.
fn normals_of(m: &Mat) -> [[f64; 3]; 3] {
    let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() <= 1e-30 {
        return normal_matrix(m);
    }
    let n = [
        [e * i - f * h, f * g - d * i, d * h - e * g],
        [c * h - b * i, a * i - c * g, b * g - a * h],
        [b * f - c * e, c * d - a * f, a * e - b * d],
    ];
    n.map(|row| row.map(|x| x / det))
}

use fbx_reader::transform::mul;

/// The inverse of an affine matrix (last row 0 0 0 1); `None` when it is singular.
fn inverse(m: &Mat) -> Option<Mat> {
    let [[a, b, c, tx], [d, e, f, ty], [g, h, i, tz], _] = *m;
    let co = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    let det = a * (e * i - f * h) + b * (f * g - d * i) + c * (d * h - e * g);
    let scale = [a, b, c, d, e, f, g, h, i].iter().fold(0.0f64, |s, x| s.max(x.abs()));
    if !det.is_finite() || det.abs() <= 1e-12 * scale * scale * scale || scale == 0.0 {
        return None;
    }
    let [r0, r1, r2] = co.map(|row| row.map(|x| x / det));
    let back = |r: [f64; 3]| -(r[0] * tx + r[1] * ty + r[2] * tz);
    Some([
        [r0[0], r0[1], r0[2], back(r0)],
        [r1[0], r1[1], r1[2], back(r1)],
        [r2[0], r2[1], r2[2], back(r2)],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inverse_undoes_an_affine_matrix() {
        let m = [[0.0, -2.0, 0.0, 1.0], [2.0, 0.0, 0.0, 2.0], [0.0, 0.0, 0.5, 3.0], [0.0, 0.0, 0.0, 1.0]];
        let p = mul(&inverse(&m).unwrap(), &m);
        for (r, row) in p.iter().enumerate() {
            for (c, x) in row.iter().enumerate() {
                assert!((x - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
        let flat = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        assert!(inverse(&flat).is_none());
    }
}
