//! Skins: geometry whose control points follow bones.
//!
//! A `Deformer` object of sub-class `Skin` is connected into a `Geometry`. Its clusters
//! (`Deformer` objects of sub-class `Cluster`, connected into the skin) each belong to one bone,
//! a `Model` connected into the cluster. A cluster lists control points (`Indexes`) with a
//! weight each (`Weights`), and two matrices from the moment the skin was bound:
//! `TransformLink`, the bone's world transform, and `Transform`, which takes the mesh model's
//! space into the bone's. When `TransformLink` is missing, the bone's matrix from a `BindPose`
//! stands in; when `Transform` is missing, it follows from `TransformLink` and the mesh model's
//! world transform as the file stores it.
//!
//! At any time, a cluster takes the geometry to the world by
//! `bone world * Transform * geometric transform of the mesh model` ([`Skin::cluster_matrices`]).
//! A control point moves by the weighted sum of its clusters' matrices, the weights divided by
//! their sum (linear blend skinning, [`Skin::point_matrix`]). A point without any weight stays
//! with its mesh model.
//!
//! Dual quaternion skinning (`SkinningType` `DualQuaternion`, or `Blend` with nonzero
//! `BlendWeights`) is read as linear blend skinning, with a warning: joints that twist then thin
//! out more than their author saw. Blend shapes and geometry caches are not read.
//!
//! Ported from ufbx's cluster reading and `ufbx_get_skin_vertex_matrix` (Unlicense).

use crate::document::{Document, Object};
use crate::transform::{Hierarchy, IDENTITY, Mat4, NodeWorld, mul};
use crate::tree::{Array, Node};
use crate::{Error, Limits};

/// How a skin's author wanted its points blended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkinningMethod {
    /// `Linear` or no `SkinningType`.
    #[default]
    Linear,
    /// `Rigid`: blended linearly as well.
    Rigid,
    /// `DualQuaternion`: blended linearly here.
    DualQuaternion,
    /// `Blend`: per point between linear and dual quaternion by `BlendWeights`.
    Blended,
}

/// One bone's share of a skin.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Cluster {
    /// The `Cluster` deformer's object id.
    pub id: i64,
    /// The cluster's name.
    pub name: String,
    /// The bone model's index in [`Hierarchy::nodes`].
    pub bone: usize,
    /// Geometry space to the bone's space at bind time: `Transform` times the mesh model's
    /// geometric transform.
    pub geometry_to_bone: Mat4,
    /// The bone's world transform at bind time (`TransformLink`, else the bind pose).
    pub bind_to_world: Mat4,
}

/// One weight of a control point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Influence {
    /// Index into [`Skin::clusters`].
    pub cluster: u32,
    /// The weight as stored; [`Skin::point_matrix`] divides by the sum of a point's weights.
    pub weight: f64,
}

/// A skin read from a file: its clusters, and per control point of the geometry the clusters
/// that move it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Skin {
    /// The `Skin` deformer's object id.
    pub id: i64,
    /// The skin's name.
    pub name: String,
    /// The blending the file asks for. Every method is blended linearly.
    pub method: SkinningMethod,
    /// Whether the file asks for dual quaternion blending anywhere (applied as linear).
    pub dual_quaternion: bool,
    /// Only the clusters with a bone, in connection order.
    pub clusters: Vec<Cluster>,
    /// Per control point, where its influences start in `influences`; one more entry at the end.
    first: Vec<u32>,
    influences: Vec<Influence>,
    /// What was read in a doubtful way or left out, in words.
    pub warnings: Vec<String>,
}

/// All twelve entries of the affine part are zero: the file left the matrix out.
fn all_zero(m: &Mat4) -> bool {
    m.iter().take(3).flatten().all(|x| *x == 0.0)
}

/// A matrix from 16 numbers stored column after column; `None` when fewer are there.
fn matrix(a: &Array) -> Option<Mat4> {
    if a.len() < 16 {
        return None;
    }
    let at = |i: usize| a.f64_at(i).unwrap_or(0.0);
    Some([
        [at(0), at(4), at(8), at(12)],
        [at(1), at(5), at(9), at(13)],
        [at(2), at(6), at(10), at(14)],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// The inverse of an affine matrix, all zero when it is singular (as ufbx does).
pub(crate) fn invert_affine(m: &Mat4) -> Mat4 {
    let [[a, b, c, tx], [d, e, f, ty], [g, h, i, tz], _] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() <= f64::MIN_POSITIVE {
        let mut z = [[0.0; 4]; 4];
        z[3][3] = 1.0;
        return z;
    }
    let r = 1.0 / det;
    let r0 = [(e * i - f * h) * r, (c * h - b * i) * r, (b * f - c * e) * r];
    let r1 = [(f * g - d * i) * r, (a * i - c * g) * r, (c * d - a * f) * r];
    let r2 = [(d * h - e * g) * r, (b * g - a * h) * r, (a * e - b * d) * r];
    let back = |row: [f64; 3]| -(row[0] * tx + row[1] * ty + row[2] * tz);
    [
        [r0[0], r0[1], r0[2], back(r0)],
        [r1[0], r1[1], r1[2], back(r1)],
        [r2[0], r2[1], r2[2], back(r2)],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// The skins connected into geometry `geometry` (usually one).
pub(crate) fn skins_of(doc: &Document, geometry: i64) -> impl Iterator<Item = &Object> + '_ {
    doc.sources(geometry, "Deformer").filter(|d| d.sub_class == "Skin")
}

/// Per model id: its matrix in the first bind pose that lists it.
fn bind_pose(doc: &Document, model: i64) -> Option<Mat4> {
    doc.objects.iter().filter(|o| o.class == "Pose" && o.sub_class == "BindPose").find_map(|p| {
        doc.node(p)?.children_named("PoseNode").find_map(|n| {
            let id = n.child("Node").and_then(|c| c.value(0)).and_then(|v| v.as_i64())?;
            if id != model {
                return None;
            }
            n.child_array("Matrix").and_then(matrix)
        })
    })
}

impl Skin {
    /// Reads skin `skin` of a geometry with `points` control points, placed by hierarchy node
    /// `mesh_node` (whose world transform `worlds` holds as the file stores it).
    pub(crate) fn read(
        doc: &Document,
        hierarchy: &Hierarchy,
        worlds: &[NodeWorld],
        skin: &Object,
        mesh_node: usize,
        points: usize,
        limits: &Limits,
    ) -> Result<Skin, Error> {
        let mut warnings = Vec::new();
        let record = doc.node(skin);
        let method = match record.and_then(|r| r.child_str("SkinningType")) {
            Some("Rigid") => SkinningMethod::Rigid,
            Some("DualQuaternion") => SkinningMethod::DualQuaternion,
            Some("Blend") => SkinningMethod::Blended,
            _ => SkinningMethod::Linear,
        };
        let dual_quaternion = match method {
            SkinningMethod::DualQuaternion => true,
            SkinningMethod::Blended => record.is_some_and(|r| blend_weights_used(r, points)),
            _ => false,
        };
        if dual_quaternion {
            warnings.push(format!(
                "skin `{}` asks for dual quaternion skinning; it is blended linearly",
                skin.name
            ));
        }
        let (mesh_world, geometric) = worlds.get(mesh_node).map_or((IDENTITY, IDENTITY), |w| {
            (w.world, hierarchy.nodes.get(mesh_node).map_or(IDENTITY, |n| n.props.geometric()))
        });
        let mut clusters = Vec::new();
        let mut lists: Vec<(&Array, &Array)> = Vec::new();
        let mut total = 0usize;
        for c in doc.sources(skin.id, "Deformer").filter(|d| d.sub_class == "Cluster") {
            // A cluster without a bone moves nothing (ufbx drops it as well).
            let Some(bone) = doc.sources(c.id, "Model").find_map(|m| hierarchy.index_of(m.id)) else {
                continue;
            };
            let Some(node) = doc.node(c) else { continue };
            let (indexes, weights) = (node.child_array("Indexes"), node.child_array("Weights"));
            let mut both = None;
            match (indexes, weights) {
                (Some(i), Some(w)) if i.len() == w.len() => both = Some((i, w)),
                (Some(_), Some(_)) => warnings.push(format!(
                    "cluster `{}` has not as many weights as indexes; its weights are left out",
                    c.name
                )),
                _ => {}
            }
            if let Some((i, _)) = both {
                total = total.saturating_add(i.len());
                if total > limits.max_skin_weights {
                    return Err(Error::new(format!(
                        "skin `{}` has more than {} weights",
                        skin.name, limits.max_skin_weights
                    )));
                }
            }
            let (mut transform, mut link) = ([[0.0; 4]; 4], [[0.0; 4]; 4]);
            if let (Some(t), Some(l)) = (
                node.child_array("Transform").and_then(matrix),
                node.child_array("TransformLink").and_then(matrix),
            ) {
                (transform, link) = (t, l);
            }
            let bone_id = hierarchy.nodes.get(bone).map_or(0, |n| n.id);
            if all_zero(&link)
                && let Some(m) = bind_pose(doc, bone_id)
            {
                link = m;
            }
            if all_zero(&transform) {
                transform = mul(&invert_affine(&link), &mesh_world);
            }
            clusters.push(Cluster {
                id: c.id,
                name: c.name.clone(),
                bone,
                geometry_to_bone: mul(&transform, &geometric),
                bind_to_world: link,
            });
            lists.push(both.unwrap_or((&EMPTY, &EMPTY)));
        }
        if points > limits.max_vertices {
            return Err(Error::new(format!("more than {} control points", limits.max_vertices)));
        }
        // Count per point, then place (a prefix sum), as ufbx does; weights of points beyond the
        // geometry are dropped.
        let mut first = vec![0u32; points + 1];
        for (indexes, _) in &lists {
            for k in 0..indexes.len() {
                if let Some(p) = point_index(indexes, k, points)
                    && let Some(n) = first.get_mut(p + 1)
                {
                    *n += 1;
                }
            }
        }
        for p in 0..points {
            let before = first.get(p).copied().unwrap_or(0);
            if let Some(n) = first.get_mut(p + 1) {
                *n = n.saturating_add(before);
            }
        }
        let used = first.last().copied().unwrap_or(0) as usize;
        let mut influences = vec![Influence { cluster: 0, weight: 0.0 }; used];
        let mut fill: Vec<u32> = first.clone();
        for (c, (indexes, weights)) in lists.iter().enumerate() {
            for k in 0..indexes.len() {
                let Some(p) = point_index(indexes, k, points) else { continue };
                let Some(slot) = fill.get_mut(p) else { continue };
                if let Some(i) = influences.get_mut(*slot as usize) {
                    *i = Influence { cluster: c as u32, weight: weights.f64_at(k).unwrap_or(0.0) };
                }
                *slot += 1;
            }
        }
        // Heaviest first (stable, as ufbx).
        for p in 0..points {
            let (a, b) = (
                first.get(p).copied().unwrap_or(0) as usize,
                first.get(p + 1).copied().unwrap_or(0) as usize,
            );
            if let Some(s) = influences.get_mut(a..b) {
                s.sort_by(|x, y| y.weight.total_cmp(&x.weight));
            }
        }
        Ok(Skin {
            id: skin.id,
            name: skin.name.clone(),
            method,
            dual_quaternion,
            clusters,
            first,
            influences,
            warnings,
        })
    }

    /// Control points the skin covers (the geometry's).
    pub fn points(&self) -> usize {
        self.first.len().saturating_sub(1)
    }

    /// The weights of control point `point`, heaviest first, as stored (not divided by their
    /// sum, zero weights included).
    pub fn influences(&self, point: usize) -> &[Influence] {
        let a = self.first.get(point).copied().unwrap_or(0) as usize;
        let b = self.first.get(point + 1).copied().unwrap_or(0) as usize;
        self.influences.get(a..b).unwrap_or(&[])
    }

    /// The most weights any control point has.
    pub fn max_influences(&self) -> usize {
        self.first
            .iter()
            .zip(self.first.iter().skip(1))
            .map(|(a, b)| b.saturating_sub(*a) as usize)
            .max()
            .unwrap_or(0)
    }

    /// Per cluster, geometry space to the world for the models placed at `worlds` (one per
    /// node of the hierarchy, as [`crate::Animation::worlds`] gives them):
    /// `bone world * geometry_to_bone`.
    pub fn cluster_matrices(&self, worlds: &[NodeWorld]) -> Vec<Mat4> {
        self.clusters
            .iter()
            .map(|c| worlds.get(c.bone).map_or(IDENTITY, |w| mul(&w.world, &c.geometry_to_bone)))
            .collect()
    }

    /// Geometry space to the world for control point `point`: its clusters' matrices
    /// (`cluster_matrices`) weighted, the weights divided by their sum. `None` when the point
    /// has no weight or its weights add up to zero or less: it then stays with its mesh model.
    pub fn point_matrix(&self, point: usize, cluster_matrices: &[Mat4]) -> Option<Mat4> {
        let list = self.influences(point);
        let total: f64 = list.iter().map(|i| i.weight).sum();
        if total.is_nan() || total <= 0.0 {
            return None;
        }
        let mut m = [[0.0; 4]; 4];
        for i in list {
            let Some(c) = cluster_matrices.get(i.cluster as usize) else { continue };
            let w = i.weight / total;
            for (row, src) in m.iter_mut().zip(c).take(3) {
                for (x, s) in row.iter_mut().zip(src) {
                    *x += w * s;
                }
            }
        }
        m[3] = [0.0, 0.0, 0.0, 1.0];
        Some(m)
    }
}

static EMPTY: Array = Array::I32(Vec::new());

/// Entry `k` of `indexes` as a control point below `points`.
fn point_index(indexes: &Array, k: usize, points: usize) -> Option<usize> {
    let p = indexes.i64_at(k)?;
    usize::try_from(p).ok().filter(|p| *p < points)
}

/// Whether a `Blend` skin gives any of its points a dual quaternion share.
fn blend_weights_used(record: &Node, points: usize) -> bool {
    let (Some(i), Some(w)) = (record.child_array("Indexes"), record.child_array("BlendWeights")) else {
        return false;
    };
    (0..i.len().min(w.len()))
        .any(|k| point_index(i, k, points).is_some() && w.f64_at(k).is_some_and(|x| x > 0.0))
}
