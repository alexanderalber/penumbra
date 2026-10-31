//! Model transforms. A `Model`'s place relative to its parent is a function of its property
//! values only ([`TransformProps::local`]), so the same code serves a static file and every
//! frame of an animation once animated values are put into a [`TransformProps`].
//!
//! FBX composes a local transform as
//! `T * Roff * Rp * Rpre * R * Rpost^-1 * Rp^-1 * Soff * Sp * S * Sp^-1`: translation, rotation
//! offset and pivot, pre-rotation, the rotation in its Euler order, the inverted
//! post-rotation, then the scaling offset and pivot around the scale. Pre- and post-rotation
//! and the rotation order count only while `RotationActive` is set. `GeometricTranslation`,
//! `GeometricRotation` and `GeometricScaling` move the model's geometry but not its children.
//! `InheritType` decides how a parent's scale reaches the child: fully (`RSrs`, the usual),
//! per axis without shear (`RrSs`), or not at all (`Rrs`).
//!
//! Ported from ufbx's `ufbxi_get_transform` and `ufbxi_update_node` (Unlicense).
//!
//! Matrices are row-major for column vectors: `p' = M * p`, translation in the last column.

use std::collections::HashMap;

use crate::document::{Document, Properties};
use crate::{Error, Limits};

pub type Mat4 = [[f64; 4]; 4];
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat4 =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
const IDENTITY3: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The order Euler angles apply in: `Xyz` turns about x first, then y, then z.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RotationOrder {
    #[default]
    Xyz,
    Xzy,
    Yzx,
    Yxz,
    Zxy,
    Zyx,
    /// Stored by some writers; treated as `Xyz`.
    Spheric,
}

impl RotationOrder {
    pub fn from_index(i: i64) -> RotationOrder {
        match i {
            1 => RotationOrder::Xzy,
            2 => RotationOrder::Yzx,
            3 => RotationOrder::Yxz,
            4 => RotationOrder::Zxy,
            5 => RotationOrder::Zyx,
            6 => RotationOrder::Spheric,
            _ => RotationOrder::Xyz,
        }
    }

    /// The axes in the order they apply.
    pub fn axes(self) -> [usize; 3] {
        match self {
            RotationOrder::Xyz | RotationOrder::Spheric => [0, 1, 2],
            RotationOrder::Xzy => [0, 2, 1],
            RotationOrder::Yzx => [1, 2, 0],
            RotationOrder::Yxz => [1, 0, 2],
            RotationOrder::Zxy => [2, 0, 1],
            RotationOrder::Zyx => [2, 1, 0],
        }
    }
}

/// How a model takes its parent's scale (`InheritType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Inherit {
    /// `RSrs` (1): the parent's whole matrix, shear included.
    #[default]
    Normal,
    /// `RrSs` (0): the parent's scale per axis, without its rotation shearing it.
    ComponentwiseScale,
    /// `Rrs` (2): not the parent's scale.
    IgnoreParentScale,
}

/// The property values a model's transform depends on. Angles in degrees.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformProps {
    pub translation: [f64; 3],
    pub rotation: [f64; 3],
    pub scaling: [f64; 3],
    pub pre_rotation: [f64; 3],
    pub post_rotation: [f64; 3],
    pub rotation_offset: [f64; 3],
    pub rotation_pivot: [f64; 3],
    pub scaling_offset: [f64; 3],
    pub scaling_pivot: [f64; 3],
    pub rotation_order: RotationOrder,
    /// `RotationActive` and not `RotationSpaceForLimitOnly`: pre- and post-rotation and the
    /// rotation order apply.
    pub rotation_space: bool,
    pub inherit: Inherit,
    pub geometric_translation: [f64; 3],
    pub geometric_rotation: [f64; 3],
    pub geometric_scaling: [f64; 3],
}

impl Default for TransformProps {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scaling: [1.0; 3],
            pre_rotation: [0.0; 3],
            post_rotation: [0.0; 3],
            rotation_offset: [0.0; 3],
            rotation_pivot: [0.0; 3],
            scaling_offset: [0.0; 3],
            scaling_pivot: [0.0; 3],
            rotation_order: RotationOrder::Xyz,
            rotation_space: true,
            inherit: Inherit::Normal,
            geometric_translation: [0.0; 3],
            geometric_rotation: [0.0; 3],
            geometric_scaling: [1.0; 3],
        }
    }
}

/// A translation, a rotation and a per-axis scale applied first: `T * R * S`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub translation: [f64; 3],
    pub rotation: Mat3,
    pub scale: [f64; 3],
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        let ([r0, r1, r2], [sx, sy, sz], [tx, ty, tz]) = (self.rotation, self.scale, self.translation);
        [
            [r0[0] * sx, r0[1] * sy, r0[2] * sz, tx],
            [r1[0] * sx, r1[1] * sy, r1[2] * sz, ty],
            [r2[0] * sx, r2[1] * sy, r2[2] * sz, tz],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// The matrix without the scale.
    pub fn unscaled_matrix(&self) -> Mat4 {
        Trs { scale: [1.0; 3], ..*self }.matrix()
    }
}

impl TransformProps {
    /// The values of `props`, template defaults included, constants where both are missing.
    pub fn from_props(props: &Properties) -> TransformProps {
        let d = TransformProps::default();
        let v = |name: &str, fallback: [f64; 3]| props.vec3(name).unwrap_or(fallback);
        let rotation_active = props.int("RotationActive").unwrap_or(1) != 0;
        let limit_only = props.int("RotationSpaceForLimitOnly").unwrap_or(0) != 0;
        TransformProps {
            translation: v("Lcl Translation", d.translation),
            rotation: v("Lcl Rotation", d.rotation),
            scaling: v("Lcl Scaling", d.scaling),
            pre_rotation: v("PreRotation", d.pre_rotation),
            post_rotation: v("PostRotation", d.post_rotation),
            rotation_offset: v("RotationOffset", d.rotation_offset),
            rotation_pivot: v("RotationPivot", d.rotation_pivot),
            scaling_offset: v("ScalingOffset", d.scaling_offset),
            scaling_pivot: v("ScalingPivot", d.scaling_pivot),
            rotation_order: RotationOrder::from_index(props.int("RotationOrder").unwrap_or(0)),
            rotation_space: rotation_active && !limit_only,
            inherit: match props.int("InheritType") {
                Some(0) => Inherit::ComponentwiseScale,
                Some(2) => Inherit::IgnoreParentScale,
                _ => Inherit::Normal,
            },
            geometric_translation: v("GeometricTranslation", d.geometric_translation),
            geometric_rotation: v("GeometricRotation", d.geometric_rotation),
            geometric_scaling: v("GeometricScaling", d.geometric_scaling),
        }
    }

    /// The model relative to its parent.
    pub fn local(&self) -> Trs {
        let r = if self.rotation_space {
            let pre = euler(self.pre_rotation, RotationOrder::Xyz);
            let post = transpose3(&euler(self.post_rotation, RotationOrder::Xyz));
            mul3(&mul3(&pre, &euler(self.rotation, self.rotation_order)), &post)
        } else {
            euler(self.rotation, RotationOrder::Xyz)
        };
        let s = self.scaling;
        let (sp, rp) = (self.scaling_pivot, self.rotation_pivot);
        let scaled_pivot = each(s, sp, |a, b| a * b);
        let inner = each(
            each(self.scaling_offset, sp, |a, b| a + b),
            each(rp, scaled_pivot, |a, b| a + b),
            |a, b| a - b,
        );
        let turned = apply3(&r, inner);
        let translation = each(
            each(self.translation, self.rotation_offset, |a, b| a + b),
            each(rp, turned, |a, b| a + b),
            |a, b| a + b,
        );
        Trs { translation, rotation: r, scale: s }
    }

    /// The geometry relative to the model: `T * R * S` of the geometric properties (rotation
    /// order `Xyz`).
    pub fn geometric(&self) -> Mat4 {
        Trs {
            translation: self.geometric_translation,
            rotation: euler(self.geometric_rotation, RotationOrder::Xyz),
            scale: self.geometric_scaling,
        }
        .matrix()
    }
}

/// The rotation matrix of Euler angles in degrees.
pub fn euler(degrees: [f64; 3], order: RotationOrder) -> Mat3 {
    let mut m = IDENTITY3;
    for axis in order.axes() {
        let a = degrees.get(axis).copied().unwrap_or(0.0).to_radians();
        let (s, c) = a.sin_cos();
        let r = match axis {
            0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
            1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
            _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
        };
        m = mul3(&r, &m);
    }
    m
}

/// `f` per component.
pub fn each(a: [f64; 3], b: [f64; 3], f: impl Fn(f64, f64) -> f64) -> [f64; 3] {
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

fn dot<const N: usize>(a: &[f64; N], b: &[f64; N]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn mul3(a: &Mat3, b: &Mat3) -> Mat3 {
    let bt = transpose3(b);
    a.map(|row| bt.map(|col| dot(&row, &col)))
}

fn transpose3(m: &Mat3) -> Mat3 {
    [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]]
}

fn apply3(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    m.map(|row| dot(&row, &v))
}

fn transpose(m: &Mat4) -> Mat4 {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m0, n, o, p]] = *m;
    [[a, e, i, m0], [b, f, j, n], [c, g, k, o], [d, h, l, p]]
}

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let bt = transpose(b);
    a.map(|row| bt.map(|col| dot(&row, &col)))
}

/// `m * p` for a point.
pub fn transform_point(m: &Mat4, p: [f64; 3]) -> [f64; 3] {
    let row = |r: &[f64; 4]| r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3];
    [row(&m[0]), row(&m[1]), row(&m[2])]
}

/// One model in the hierarchy.
#[derive(Clone, Debug, PartialEq)]
pub struct HierarchyNode {
    pub id: i64,
    pub name: String,
    /// Index of the parent model in [`Hierarchy::nodes`]; `None` under the scene root.
    pub parent: Option<usize>,
    /// The values read from the file.
    pub props: TransformProps,
}

/// The models of a document, depth first: each parent before its children, roots and siblings
/// in file order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hierarchy {
    pub nodes: Vec<HierarchyNode>,
    index: HashMap<i64, usize>,
}

/// Where a model ended up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeWorld {
    /// Model to world.
    pub world: Mat4,
    /// Model to world without the model's own scale (for children that do not inherit it).
    pub unscaled: Mat4,
    /// The scale a child with `RrSs` multiplies its own by.
    pub inherit_scale: [f64; 3],
    /// Geometry to world: `world` times the geometric transform.
    pub geometry: Mat4,
}

impl Hierarchy {
    /// The models of `doc` and their parents. A model's parent is the first model it is
    /// connected into. A cycle of parents is broken at the model where the walk returns, which
    /// then hangs under the root (with a warning); nesting deeper than `limits.max_depth` is an
    /// error.
    pub fn from_document(
        doc: &Document,
        limits: &Limits,
        warnings: &mut Vec<String>,
    ) -> Result<Hierarchy, Error> {
        let models: Vec<&crate::Object> = doc.objects.iter().filter(|o| o.class == "Model").collect();
        let local: HashMap<i64, usize> = models.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
        let mut parent: Vec<Option<usize>> = models
            .iter()
            .map(|m| doc.destinations(m.id, "Model").find_map(|p| local.get(&p.id).copied()))
            .collect();
        // Depth of each model, breaking cycles: walk up from each model until a model of known
        // depth, the root, or a model already on the walk (a cycle, cut there).
        let mut depth: Vec<Option<usize>> = vec![None; models.len()];
        let mut on_path = vec![false; models.len()];
        for start in 0..models.len() {
            let mut path = Vec::new();
            let mut at = start;
            loop {
                if depth.get(at).copied().flatten().is_some() {
                    break;
                }
                if on_path.get(at).copied().unwrap_or(false) {
                    if let Some(p) = parent.get_mut(at) {
                        *p = None;
                    }
                    if let Some(d) = depth.get_mut(at) {
                        *d = Some(0);
                    }
                    warnings.push(format!(
                        "model `{}` is its own ancestor; it is placed under the root",
                        models.get(at).map_or("", |m| m.name.as_str())
                    ));
                    break;
                }
                if let Some(o) = on_path.get_mut(at) {
                    *o = true;
                }
                path.push(at);
                match parent.get(at).copied().flatten() {
                    Some(p) => at = p,
                    None => break,
                }
            }
            // Parents are resolved before their children: the walk went upwards.
            while let Some(m) = path.pop() {
                if let Some(o) = on_path.get_mut(m) {
                    *o = false;
                }
                if depth.get(m).copied().flatten().is_some() {
                    continue;
                }
                let d = match parent.get(m).copied().flatten() {
                    Some(p) => depth.get(p).copied().flatten().unwrap_or(0) + 1,
                    None => 0,
                };
                if d > limits.max_depth {
                    return Err(Error::new(format!("models nested deeper than {}", limits.max_depth)));
                }
                if let Some(slot) = depth.get_mut(m) {
                    *slot = Some(d);
                }
            }
        }
        // Depth first from the roots, children in file order: parents come before children.
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); models.len()];
        let mut roots = Vec::new();
        for (i, p) in parent.iter().enumerate() {
            match p.and_then(|p| kids.get_mut(p)) {
                Some(k) => k.push(i),
                None => roots.push(i),
            }
        }
        let mut order = Vec::with_capacity(models.len());
        let mut stack: Vec<usize> = roots.into_iter().rev().collect();
        while let Some(i) = stack.pop() {
            order.push(i);
            if let Some(k) = kids.get(i) {
                stack.extend(k.iter().rev());
            }
        }
        let mut position = vec![0usize; models.len()];
        for (pos, i) in order.iter().enumerate() {
            if let Some(slot) = position.get_mut(*i) {
                *slot = pos;
            }
        }
        let mut nodes = Vec::with_capacity(models.len());
        let mut index = HashMap::new();
        for i in order {
            let Some(m) = models.get(i) else { continue };
            index.insert(m.id, nodes.len());
            nodes.push(HierarchyNode {
                id: m.id,
                name: m.name.clone(),
                parent: parent.get(i).copied().flatten().and_then(|p| position.get(p).copied()),
                props: TransformProps::from_props(&m.props),
            });
        }
        Ok(Hierarchy { nodes, index })
    }

    /// The index of model `id` in [`Hierarchy::nodes`].
    pub fn index_of(&self, id: i64) -> Option<usize> {
        self.index.get(&id).copied()
    }

    /// The world transforms from the values read from the file.
    pub fn evaluate_static(&self) -> Vec<NodeWorld> {
        let props: Vec<TransformProps> = self.nodes.iter().map(|n| n.props.clone()).collect();
        self.evaluate(&props)
    }

    /// The world transforms for `props`, one per node in [`Hierarchy::nodes`] order (missing
    /// entries take the file's values).
    pub fn evaluate(&self, props: &[TransformProps]) -> Vec<NodeWorld> {
        let mut out: Vec<NodeWorld> = Vec::with_capacity(self.nodes.len());
        // For `RrSs` and `Rrs`: whose scale a node inherits.
        let mut scale_node: Vec<Option<usize>> = Vec::with_capacity(self.nodes.len());
        for (i, n) in self.nodes.iter().enumerate() {
            let p = props.get(i).unwrap_or(&n.props);
            let local = p.local();
            let parent = n.parent.filter(|q| *q < i).and_then(|q| out.get(q).map(|w| (q, *w)));
            let (world, unscaled, inherit_scale, scaler) = match parent {
                None => (local.matrix(), local.unscaled_matrix(), local.scale, None),
                Some((_, pw)) if p.inherit == Inherit::Normal => (
                    mul(&pw.world, &local.matrix()),
                    mul(&pw.world, &local.unscaled_matrix()),
                    local.scale,
                    None,
                ),
                Some((q, pw)) => {
                    let scaler = match p.inherit {
                        Inherit::ComponentwiseScale => Some(q),
                        _ => scale_node.get(q).copied().flatten(),
                    };
                    let ps = scaler.and_then(|k| out.get(k)).map_or([1.0; 3], |w| w.inherit_scale);
                    let t = Trs {
                        translation: each(local.translation, pw.inherit_scale, |a, b| a * b),
                        rotation: local.rotation,
                        scale: each(local.scale, ps, |a, b| a * b),
                    };
                    (mul(&pw.unscaled, &t.matrix()), mul(&pw.unscaled, &t.unscaled_matrix()), t.scale, scaler)
                }
            };
            scale_node.push(scaler);
            out.push(NodeWorld { world, unscaled, inherit_scale, geometry: mul(&world, &p.geometric()) });
        }
        out
    }
}
