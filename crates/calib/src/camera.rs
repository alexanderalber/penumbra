//! The projector as a camera: a pinhole with intrinsics in pixels and a pose in world space.
//!
//! Conventions, the same as OpenCV's so numbers can be compared with other tools:
//!
//! - **Projector frame:** x right, y down, z forward along the optical axis, so a point in
//!   front of the lens has a positive depth.
//! - **Pose:** world to projector, `X_p = R X_w + t`; `R` stored as a rotation vector (axis
//!   times angle in radians, OpenCV's `rvec`). The projector's position in the world is
//!   `-Rᵀ t` ([`Camera::position`]).
//! - **Picture coordinates:** pixels with the origin at the top left *corner* of the picture,
//!   x right, y down; the centre of pixel (i, j) is (i + 0.5, j + 0.5), the picture spans
//!   0..width and 0..height.
//! - **Projection:** `x = X_p.x / X_p.z`, `y = X_p.y / X_p.z`, radial distortion
//!   `d = 1 + k1 r² + k2 r⁴` with `r² = x² + y²`, then `u = fx d x + cx`, `v = fy d y + cy`.
//!   No skew, no tangential distortion.

use nalgebra::{Matrix3, Matrix4, Rotation3, Vector3};
use serde::{Deserialize, Serialize};

/// Largest coordinate (world units or pixels) the calibration accepts; beyond this, squares
/// and products of coordinates lose too much precision to mean anything.
pub const MAX_COORDINATE: f64 = 1e9;

/// What a projector's lens does: focal lengths and principal point in pixels of its picture.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intrinsics {
    /// Picture width and height in pixels (the resolution the other numbers are in).
    pub size: [u32; 2],
    /// `fx`, `fy` in pixels. Equal for square pixels; `fx / width` is the throw ratio.
    pub focal: [f64; 2],
    /// `cx`, `cy` in pixels: where the optical axis meets the picture. Projectors with lens
    /// shift have it far from the centre, often near or below the bottom edge.
    pub principal: [f64; 2],
    /// Radial distortion `k1`, `k2` (0 for most projectors).
    #[serde(default)]
    pub distortion: [f64; 2],
}

impl Intrinsics {
    /// A lens with the given throw ratio (distance / picture width), square pixels, the principal
    /// point at the picture's centre and no distortion.
    pub fn from_throw_ratio(size: [u32; 2], throw_ratio: f64) -> Self {
        let [w, h] = size.map(f64::from);
        Self {
            size,
            focal: [throw_ratio * w, throw_ratio * w],
            principal: [w / 2.0, h / 2.0],
            distortion: [0.0; 2],
        }
    }

    /// Distance / picture width, from `fx`.
    pub fn throw_ratio(&self) -> f64 {
        self.focal[0] / f64::from(self.size[0].max(1))
    }

    /// The 3x3 camera matrix `K`, row by row.
    pub fn matrix(&self) -> [[f64; 3]; 3] {
        let ([fx, fy], [cx, cy]) = (self.focal, self.principal);
        [[fx, 0.0, cx], [0.0, fy, cy], [0.0, 0.0, 1.0]]
    }

    /// Pixel of a point in the projector frame; `None` behind the lens (or on its plane) and
    /// for non-finite results.
    pub(crate) fn project_local(&self, p: &Vector3<f64>) -> Option<[f64; 2]> {
        // Written so that NaN counts as behind.
        let in_front = p.z > 1e-12 * (p.x.abs() + p.y.abs()).max(1e-300);
        if !in_front {
            return None;
        }
        let (x, y) = (p.x / p.z, p.y / p.z);
        let r2 = x * x + y * y;
        let [k1, k2] = self.distortion;
        let d = 1.0 + r2 * (k1 + k2 * r2);
        let q = [self.focal[0] * d * x + self.principal[0], self.focal[1] * d * y + self.principal[1]];
        q.iter().all(|v| v.is_finite()).then_some(q)
    }

    /// The undistorted normalised direction `(x, y)` (at depth 1) of a pixel, or `None` when the
    /// distortion cannot be undone there (the iteration does not settle, or a focal length is 0).
    fn unproject(&self, q: [f64; 2]) -> Option<[f64; 2]> {
        let ([fx, fy], [cx, cy], [k1, k2]) = (self.focal, self.principal, self.distortion);
        if fx == 0.0 || fy == 0.0 {
            return None;
        }
        let (xd, yd) = ((q[0] - cx) / fx, (q[1] - cy) / fy);
        let (mut x, mut y) = (xd, yd);
        if k1 != 0.0 || k2 != 0.0 {
            // Fixed point iteration x = xd / d(x), bounded; checked by projecting back.
            for _ in 0..50 {
                let r2 = x * x + y * y;
                let d = 1.0 + r2 * (k1 + k2 * r2);
                if !d.is_finite() || d.abs() < 1e-9 {
                    return None;
                }
                (x, y) = (xd / d, yd / d);
            }
            let r2 = x * x + y * y;
            let d = 1.0 + r2 * (k1 + k2 * r2);
            let tol = 1e-9 * (1.0 + xd.abs() + yd.abs());
            if (d * x - xd).abs() > tol || (d * y - yd).abs() > tol {
                return None;
            }
        }
        [x, y].iter().all(|v| v.is_finite()).then_some([x, y])
    }
}

/// Where a projector is and where it points: world to projector, `X_p = R X_w + t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    /// `R` as a rotation vector: axis times angle in radians.
    pub rotation: [f64; 3],
    /// `t` in world units.
    pub translation: [f64; 3],
}

impl Pose {
    pub(crate) fn from_parts(r: &Rotation3<f64>, t: &Vector3<f64>) -> Self {
        let v = r.scaled_axis();
        Self { rotation: [v.x, v.y, v.z], translation: [t.x, t.y, t.z] }
    }

    pub(crate) fn rotation3(&self) -> Rotation3<f64> {
        Rotation3::from_scaled_axis(Vector3::from(self.rotation))
    }

    pub(crate) fn t(&self) -> Vector3<f64> {
        Vector3::from(self.translation)
    }

    /// A pose looking from `eye` at `target`, with `up` (world) pointing up in the picture.
    /// `None` when `eye` and `target` coincide or `up` is parallel to the view.
    pub fn look_at(eye: [f64; 3], target: [f64; 3], up: [f64; 3]) -> Option<Self> {
        let (eye, target, up) = (Vector3::from(eye), Vector3::from(target), Vector3::from(up));
        let z = (target - eye).try_normalize(1e-12)?;
        // Picture y points down (-up), so the projector's x is down x forward = forward x up.
        let x = z.cross(&up).try_normalize(1e-12)?;
        let y = z.cross(&x);
        let r = Rotation3::from_matrix_unchecked(Matrix3::from_rows(&[
            x.transpose(),
            y.transpose(),
            z.transpose(),
        ]));
        let t = -(r * eye);
        Some(Self::from_parts(&r, &t))
    }

    /// The same pose turned by `degrees` about its own view axis, in place. Positive turns the
    /// picture's top to the right, as a head tilted to the right (an aircraft's roll), so the
    /// world in the picture turns counter-clockwise.
    pub fn rolled(&self, degrees: f64) -> Self {
        // The rolled axes are x' = cos x + sin y and y' = -sin x + cos y: R' = Rz(-a) R, and t
        // turns alike so the position stays.
        let m = Rotation3::from_axis_angle(&Vector3::z_axis(), -degrees.to_radians());
        Self::from_parts(&(m * self.rotation3()), &(m * self.t()))
    }

    /// The rotation matrix `R`, row by row.
    pub fn rotation_matrix(&self) -> [[f64; 3]; 3] {
        let m = self.rotation3().into_inner();
        [[m.m11, m.m12, m.m13], [m.m21, m.m22, m.m23], [m.m31, m.m32, m.m33]]
    }
}

/// A projector (or camera) as a pinhole: lens and pose.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub intrinsics: Intrinsics,
    pub pose: Pose,
}

impl Camera {
    /// The pixel a world point lands on; `None` when it is behind the projector.
    pub fn project(&self, world: [f64; 3]) -> Option<[f64; 2]> {
        let p = self.pose.rotation3() * Vector3::from(world) + self.pose.t();
        self.intrinsics.project_local(&p)
    }

    /// The point's depth in front of the projector (along its axis), in world units; negative
    /// behind it.
    pub fn depth(&self, world: [f64; 3]) -> f64 {
        (self.pose.rotation3() * Vector3::from(world) + self.pose.t()).z
    }

    /// The projector's position in world space (`-Rᵀ t`), where its rays start.
    pub fn position(&self) -> [f64; 3] {
        let c = -(self.pose.rotation3().inverse() * self.pose.t());
        [c.x, c.y, c.z]
    }

    /// The ray through a pixel: start (the projector's position) and unit direction in world
    /// space. `None` when the lens model cannot be inverted there.
    pub fn ray(&self, pixel: [f64; 2]) -> Option<([f64; 3], [f64; 3])> {
        let [x, y] = self.intrinsics.unproject(pixel)?;
        let d = (self.pose.rotation3().inverse() * Vector3::new(x, y, 1.0)).try_normalize(1e-300)?;
        Some((self.position(), [d.x, d.y, d.z]))
    }

    /// Distance in pixels between each world point's projection and its picture point, in the
    /// order of `pairs`; infinite for points behind the projector.
    pub fn errors(&self, pairs: &[([f64; 3], [f64; 2])]) -> Vec<f64> {
        pairs
            .iter()
            .map(|(w, q)| match self.project(*w) {
                Some(p) => (p[0] - q[0]).hypot(p[1] - q[1]),
                None => f64::INFINITY,
            })
            .collect()
    }

    /// Root mean square of [`Camera::errors`] (0 for no pairs).
    pub fn rms_error(&self, pairs: &[([f64; 3], [f64; 2])]) -> f64 {
        rms(&self.errors(pairs))
    }

    /// The world to clip space matrix for rendering what this projector shows (row by row, for
    /// column vectors: `clip = M (x, y, z, 1)`; transpose it for WGSL's column-major `mat4x4`).
    /// Clip space as wgpu's: x right and y up in -1..1, depth 0 at `near` to 1 at `far`. The
    /// picture's top left corner is (-1, 1). Distortion is not representable in a matrix and is
    /// left out. `None` unless `0 < near < far` (finite) and the picture has a size.
    pub fn clip_from_world(&self, near: f64, far: f64) -> Option<[[f64; 4]; 4]> {
        if !(near > 0.0 && far > near && far.is_finite()) {
            return None;
        }
        let [w, h] = self.intrinsics.size.map(f64::from);
        if w < 1.0 || h < 1.0 {
            return None;
        }
        let ([fx, fy], [cx, cy]) = (self.intrinsics.focal, self.intrinsics.principal);
        let a = far / (far - near);
        #[rustfmt::skip]
        let proj = Matrix4::new(
            2.0 * fx / w, 0.0,           2.0 * cx / w - 1.0, 0.0,
            0.0,          -2.0 * fy / h, 1.0 - 2.0 * cy / h, 0.0,
            0.0,          0.0,           a,                  -a * near,
            0.0,          0.0,           1.0,                0.0,
        );
        let r = self.pose.rotation3().into_inner();
        let t = self.pose.t();
        #[rustfmt::skip]
        let view = Matrix4::new(
            r.m11, r.m12, r.m13, t.x,
            r.m21, r.m22, r.m23, t.y,
            r.m31, r.m32, r.m33, t.z,
            0.0,   0.0,   0.0,   1.0,
        );
        let m = proj * view;
        let rows = [
            [m.m11, m.m12, m.m13, m.m14],
            [m.m21, m.m22, m.m23, m.m24],
            [m.m31, m.m32, m.m33, m.m34],
            [m.m41, m.m42, m.m43, m.m44],
        ];
        rows.iter().flatten().all(|v| v.is_finite()).then_some(rows)
    }
}

pub(crate) fn rms(errors: &[f64]) -> f64 {
    if errors.is_empty() {
        return 0.0;
    }
    (errors.iter().map(|e| e * e).sum::<f64>() / errors.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        let pose = Pose::look_at([0.5, -1.2, -4.0], [0.0, 0.2, 0.0], [0.0, -1.0, 0.0]).unwrap();
        let mut intrinsics = Intrinsics::from_throw_ratio([1920, 1080], 1.4);
        intrinsics.principal = [970.0, 1050.0];
        Camera { intrinsics, pose }
    }

    #[test]
    fn look_at_points_the_axis_at_the_target() {
        let c = camera();
        let p = c.project([0.0, 0.2, 0.0]).unwrap();
        assert!((p[0] - 970.0).abs() < 1e-9 && (p[1] - 1050.0).abs() < 1e-9, "{p:?}");
        let pos = c.position();
        assert!((pos[0] - 0.5).abs() < 1e-12 && (pos[1] + 1.2).abs() < 1e-12 && (pos[2] + 4.0).abs() < 1e-12);
        // World up (-y here) is up in the picture: a point above the target has a smaller v.
        let above = c.project([0.0, -0.3, 0.0]).unwrap();
        assert!(above[1] < p[1]);
        assert!(c.depth([0.0, 0.2, 0.0]) > 0.0);
        assert_eq!(c.project([0.5, -1.2, -10.0]), None);
        assert!(Pose::look_at([0.0; 3], [0.0; 3], [0.0, 1.0, 0.0]).is_none());
        assert!(Pose::look_at([0.0; 3], [0.0, 1.0, 0.0], [0.0, 1.0, 0.0]).is_none());
    }

    #[test]
    fn roll_turns_the_top_of_the_picture_to_the_right() {
        let mut c = camera();
        let centre = c.project([0.0, 0.2, 0.0]).unwrap();
        // Above the target (world up is -y here): straight above the centre, unrolled.
        let above = [0.0, -0.3, 0.0];
        let p = c.project(above).unwrap();
        assert!((p[0] - centre[0]).abs() < 1e-9 && p[1] < centre[1], "{p:?}");
        let pos = c.position();
        c.pose = c.pose.rolled(90.0);
        // Rolled right by a quarter turn: what was above is to the left, at the centre's height.
        let p = c.project(above).unwrap();
        assert!(p[0] < centre[0] - 100.0 && (p[1] - centre[1]).abs() < 1e-6, "{p:?}");
        let q = c.project([0.0, 0.2, 0.0]).unwrap();
        assert!((q[0] - centre[0]).abs() < 1e-9 && (q[1] - centre[1]).abs() < 1e-9, "{q:?}");
        let moved = c.position();
        assert!((0..3).all(|k| (moved[k] - pos[k]).abs() < 1e-12), "{moved:?}");
        // Back again.
        let back = c.pose.rolled(-90.0);
        let p = Camera { pose: back, ..c }.project(above).unwrap();
        assert!((p[0] - centre[0]).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn rays_go_back_through_their_points() {
        let mut c = camera();
        for k in [[0.0, 0.0], [-0.08, 0.01]] {
            c.intrinsics.distortion = k;
            for w in [[0.3, 0.1, 0.4], [-1.0, 0.6, 0.9], [0.0, 0.2, 0.0]] {
                let q = c.project(w).unwrap();
                let (o, d) = c.ray(q).unwrap();
                // The point lies on the ray: (w - o) is parallel to d.
                let v = Vector3::from(w) - Vector3::from(o);
                let cross = v.cross(&Vector3::from(d)).norm() / v.norm();
                assert!(cross < 1e-9, "{k:?} {w:?}: {cross}");
            }
        }
    }

    #[test]
    fn clip_matrix_agrees_with_the_projection() {
        let c = camera();
        let m = Matrix4::from_fn(|i, j| c.clip_from_world(0.1, 100.0).unwrap()[i][j]);
        let [w, h] = [1920.0, 1080.0];
        for p in [[0.3, 0.1, 0.4], [-1.0, 0.6, 0.9], [2.0, -0.5, 3.0]] {
            let q = c.project(p).unwrap();
            let clip = m * nalgebra::Vector4::new(p[0], p[1], p[2], 1.0);
            let (nx, ny, nz) = (clip.x / clip.w, clip.y / clip.w, clip.z / clip.w);
            assert!((nx - (2.0 * q[0] / w - 1.0)).abs() < 1e-12);
            assert!((ny - (1.0 - 2.0 * q[1] / h)).abs() < 1e-12);
            assert!((0.0..=1.0).contains(&nz));
            assert!((clip.w - c.depth(p)).abs() < 1e-12);
        }
        assert!(c.clip_from_world(0.0, 10.0).is_none());
        assert!(c.clip_from_world(1.0, 1.0).is_none());
    }

    #[test]
    fn errors_are_pixel_distances() {
        let c = camera();
        let w = [0.3, 0.1, 0.4];
        let q = c.project(w).unwrap();
        let pairs = [(w, [q[0] + 3.0, q[1] + 4.0]), ([0.5, -1.2, -10.0], q)];
        let e = c.errors(&pairs);
        assert!((e[0] - 5.0).abs() < 1e-9);
        assert!(e[1].is_infinite());
        assert!((c.rms_error(&pairs[..1]) - 5.0).abs() < 1e-9);
    }
}
