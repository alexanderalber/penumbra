//! Calibrating a projector as a camera from pairs of world points and the picture points they
//! were placed on.
//!
//! Starting points: the normalised DLT of the 3x4 projection (six or more points not on one
//! plane), a homography when the points lie on (or near) a plane and the principal point is
//! fixed, or a camera given by the caller (e.g. the last calibration). From each,
//! Levenberg-Marquardt minimises the reprojection error in pixels over the pose and the free
//! intrinsics; the best result wins.
//!
//! Inside, the world points are moved to their centroid and scaled to a mean distance of √3,
//! so millimetres and metres, far from the origin or not, behave the same.

use nalgebra::{DMatrix, DVector, Matrix3, Matrix3x4, Rotation3, Vector3, Vector4};
use penumbra_core::Error;
use serde::{Deserialize, Serialize};

use crate::camera::{Camera, Intrinsics, MAX_COORDINATE, Pose, rms};
use crate::homography::{Homography, MAX_PAIRS, normalising};

/// Most Levenberg-Marquardt iterations [`Options::max_iterations`] may ask for.
pub const MAX_ITERATIONS: u32 = 1000;

/// Points flatter than this (smallest over largest spread) count as one plane.
const PLANAR: f64 = 1e-6;
/// Points flatter than this also get the plane's starting point (the DLT gets poor there).
const THIN: f64 = 0.1;

/// Which parameters the calibration may change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Keep the focal length of the initial camera (needs one).
    pub fix_focal: bool,
    /// `fx = fy` (projectors have square pixels). Default on.
    pub square_pixels: bool,
    /// Keep the principal point: the initial camera's, or the picture's centre without one.
    /// Needed when the points lie on one plane.
    pub fix_principal_point: bool,
    /// Solve radial distortion `k1`, `k2` too (otherwise the initial camera's, or none).
    pub distortion: bool,
    /// Levenberg-Marquardt iterations per starting point, 1 to [`MAX_ITERATIONS`].
    pub max_iterations: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fix_focal: false,
            square_pixels: true,
            fix_principal_point: false,
            distortion: false,
            max_iterations: 100,
        }
    }
}

/// Where the winning fit started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// The normalised DLT of the 3x4 projection.
    Dlt,
    /// A homography of points on (or near) a plane.
    Plane,
    /// The camera the caller gave.
    Initial,
}

/// How well the points pin the intrinsics down: one standard deviation in pixels, from the
/// residuals and the fit's Jacobian (0 for fixed parameters).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spread {
    pub focal: [f64; 2],
    pub principal: [f64; 2],
}

/// A calibrated projector and how well it fits its points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Solution {
    pub camera: Camera,
    /// Root mean square reprojection error in pixels.
    pub rms: f64,
    /// Largest reprojection error in pixels.
    pub max_error: f64,
    /// Reprojection error of every pair in pixels, in the order given.
    pub errors: Vec<f64>,
    /// Levenberg-Marquardt iterations of the winning fit.
    pub iterations: u32,
    /// False when the iteration limit stopped the fit before it settled.
    pub converged: bool,
    pub start: Start,
    /// `None` without redundancy (as many residuals as free parameters).
    pub spread: Option<Spread>,
}

/// Calibrates a projector (or camera) whose picture is `size` pixels from pairs of a world
/// point and the picture point it appears at (pixels, origin at the picture's top left corner).
///
/// Needs four or more pairs; six or more not on one plane unless the points lie on a plane
/// (then with a fixed principal point) or `initial` is given. `initial` is a further starting
/// point and supplies the values of fixed parameters. Fails, never panics, on too few or
/// non-finite or huge coordinates, points all in one place or on one line, points behind the
/// projector, and configurations that do not determine the free parameters.
pub fn calibrate(
    pairs: &[([f64; 3], [f64; 2])],
    size: [u32; 2],
    initial: Option<&Camera>,
    options: &Options,
) -> Result<Solution, Error> {
    check_input(pairs, size, initial, options)?;
    let frame = Frame::of(pairs)?;
    let pts: Vec<(Vector3<f64>, [f64; 2])> = pairs.iter().map(|(w, q)| (frame.to_local(w), *q)).collect();
    let shape = Shape::of(&pts)?;

    let [w, h] = size.map(f64::from);
    let fixed_focal = match (options.fix_focal, initial) {
        (true, Some(c)) => Some(c.intrinsics.focal),
        (true, None) => return Err(Error::new("a fixed focal length needs an initial camera")),
        (false, _) => None,
    };
    let principal = initial.map_or([w / 2.0, h / 2.0], |c| c.intrinsics.principal);
    let distortion = initial.map_or([0.0; 2], |c| c.intrinsics.distortion);
    if shape.thickness < PLANAR && !options.fix_principal_point {
        return Err(Error::new(
            "points on one plane cannot tell the principal point: fix the principal point, or add points off the plane",
        ));
    }

    let free = free_params(options);
    if 2 * pts.len() < free.len() {
        return Err(Error::new(format!(
            "{} pairs give {} equations, fewer than the {} free parameters",
            pts.len(),
            2 * pts.len(),
            free.len()
        )));
    }

    // Starting points, each with fixed parameters set to their fixed values.
    let settle = |mut s: State| {
        if let Some(f) = fixed_focal {
            s.k.focal = f;
        }
        if options.fix_principal_point {
            s.k.principal = principal;
        }
        if options.square_pixels {
            let f = (s.k.focal[0] + s.k.focal[1]) / 2.0;
            s.k.focal = [f, f];
        }
        if !options.distortion {
            s.k.distortion = distortion;
        }
        s
    };
    let mut starts: Vec<(Start, State)> = Vec::new();
    let mut why: Vec<String> = Vec::new();
    if let Some(c) = initial {
        starts.push((Start::Initial, settle(frame.state_to_local(c))));
    }
    if shape.thickness >= PLANAR {
        if pts.len() >= 6 {
            let known = fixed_focal.filter(|_| options.fix_principal_point);
            match dlt_start(&pts, size, known.map(|f| (f, principal))) {
                Ok(mut s) => {
                    s.k.distortion = distortion;
                    starts.push((Start::Dlt, settle(s)));
                }
                Err(e) => why.push(format!("DLT: {e}")),
            }
        } else {
            why.push("points not on one plane need six pairs (or an initial camera)".into());
        }
    }
    if shape.thickness < THIN {
        // An estimated focal length first; the initial camera's when the plane cannot tell.
        let mut plane = plane_start(&pts, &shape, size, principal, fixed_focal);
        if plane.is_err()
            && fixed_focal.is_none()
            && let Some(c) = initial
        {
            plane = plane_start(&pts, &shape, size, principal, Some(c.intrinsics.focal));
        }
        match plane {
            Ok(mut s) => {
                s.k.distortion = distortion;
                starts.push((Start::Plane, settle(s)));
            }
            Err(e) => why.push(format!("plane: {e}")),
        }
    }
    if starts.is_empty() {
        return Err(Error::new(format!("no starting point for the fit ({})", why.join("; "))));
    }

    let iterations = options.max_iterations.clamp(1, MAX_ITERATIONS);
    let best = starts
        .into_iter()
        .filter_map(|(start, s)| levenberg_marquardt(s, &free, &pts, iterations).map(|fit| (start, fit)))
        .min_by(|a, b| a.1.cost.total_cmp(&b.1.cost));
    let Some((start, fit)) = best else {
        return Err(Error::new(
            "every starting point puts points behind the projector (is the picture mirrored, e.g. a rear-projection mode?)",
        ));
    };

    let spread = spread(&fit, &free, &pts)?;
    let camera = frame.camera_to_world(&fit.state);
    let errors = camera.errors(pairs);
    let max_error = errors.iter().copied().fold(0.0, f64::max);
    Ok(Solution {
        camera,
        rms: rms(&errors),
        max_error,
        errors,
        iterations: fit.iterations,
        converged: fit.converged,
        start,
        spread,
    })
}

fn check_input(
    pairs: &[([f64; 3], [f64; 2])],
    size: [u32; 2],
    initial: Option<&Camera>,
    options: &Options,
) -> Result<(), Error> {
    if pairs.len() < 4 {
        return Err(Error::new(format!("a calibration needs at least 4 point pairs, got {}", pairs.len())));
    }
    if pairs.len() > MAX_PAIRS {
        return Err(Error::new(format!("at most {MAX_PAIRS} point pairs, got {}", pairs.len())));
    }
    let sane = |v: &f64| v.is_finite() && v.abs() <= MAX_COORDINATE;
    if !pairs.iter().all(|(w, q)| w.iter().chain(q).all(sane)) {
        return Err(Error::new(format!(
            "point coordinates must be finite numbers within ±{MAX_COORDINATE:e}"
        )));
    }
    if size.iter().any(|s| *s == 0 || *s > 65_536) {
        return Err(Error::new(format!("the picture size must be 1 to 65536 pixels per side, got {size:?}")));
    }
    if let Some(c) = initial {
        let k = &c.intrinsics;
        if k.size != size {
            return Err(Error::new(format!(
                "the initial camera is for a {:?} picture; this picture is {size:?}",
                k.size
            )));
        }
        let numbers = k.focal.iter().chain(&k.principal).chain(&k.distortion);
        let pose = c.pose.rotation.iter().chain(&c.pose.translation);
        if !numbers.chain(pose).all(sane) || k.focal.iter().any(|f| *f <= 0.0) {
            return Err(Error::new("the initial camera needs finite numbers and positive focal lengths"));
        }
    }
    if options.max_iterations == 0 {
        return Err(Error::new("max_iterations must be at least 1"));
    }
    Ok(())
}

/// The similarity between world coordinates and the solver's: `local = (world - centre) / scale`.
struct Frame {
    centre: Vector3<f64>,
    scale: f64,
}

impl Frame {
    fn of(pairs: &[([f64; 3], [f64; 2])]) -> Result<Self, Error> {
        let n = pairs.len() as f64;
        let centre = pairs.iter().fold(Vector3::zeros(), |acc, (w, _)| acc + Vector3::from(*w)) / n;
        let mean = pairs.iter().map(|(w, _)| (Vector3::from(*w) - centre).norm()).sum::<f64>() / n;
        let reach = centre.amax().max(1.0);
        if !mean.is_finite() || mean <= 1e-9 * reach {
            return Err(Error::new("the world points are all in one place"));
        }
        Ok(Self { centre, scale: mean / 3f64.sqrt() })
    }

    fn to_local(&self, w: &[f64; 3]) -> Vector3<f64> {
        (Vector3::from(*w) - self.centre) / self.scale
    }

    /// With `X_w = s X_l + c`: `R X_w + t = s (R X_l + (R c + t) / s)`, and the scale does not
    /// change the projection.
    fn state_to_local(&self, c: &Camera) -> State {
        let r = c.pose.rotation3();
        State { r, t: (r * self.centre + c.pose.t()) / self.scale, k: c.intrinsics }
    }

    fn camera_to_world(&self, s: &State) -> Camera {
        let t = s.t * self.scale - s.r * self.centre;
        Camera { intrinsics: s.k, pose: Pose::from_parts(&s.r, &t) }
    }
}

/// The spread of the (local) world points: principal axes and how flat they are.
struct Shape {
    /// Unit axes of the largest and second largest spread.
    major: Vector3<f64>,
    minor: Vector3<f64>,
    /// Smallest over largest spread (standard deviations along the principal axes).
    thickness: f64,
}

impl Shape {
    fn of(pts: &[(Vector3<f64>, [f64; 2])]) -> Result<Self, Error> {
        let n = pts.len() as f64;
        let cov = pts.iter().fold(Matrix3::zeros(), |acc, (x, _)| acc + x * x.transpose()) / n;
        let eig = cov
            .try_symmetric_eigen(f64::EPSILON, 1000)
            .ok_or_else(|| Error::new("the world points could not be analysed"))?;
        let mut order: Vec<(f64, Vector3<f64>)> = eig
            .eigenvalues
            .iter()
            .copied()
            .zip(eig.eigenvectors.column_iter().map(|c| c.into_owned()))
            .collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        let [(l0, major), (l1, minor), (l2, _)] = order.as_slice() else {
            return Err(Error::new("the world points could not be analysed"));
        };
        if !positive(*l0) || (l1.max(0.0) / l0).sqrt() < PLANAR {
            return Err(Error::new("the world points lie on one line"));
        }
        Ok(Self { major: *major, minor: *minor, thickness: (l2.max(0.0) / l0).sqrt() })
    }
}

/// The solver's state: pose in the local frame and the lens.
#[derive(Clone, Copy, Debug)]
struct State {
    r: Rotation3<f64>,
    t: Vector3<f64>,
    k: Intrinsics,
}

/// A free parameter. The rotation is updated multiplicatively, `R <- exp(ω) R`, so the
/// parameters are small angles around the current estimate and never meet the rotation
/// vector's singularity at π.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Param {
    Rot(usize),
    Trans(usize),
    /// `fx` and `fy` together (square pixels).
    Focal,
    Fx,
    Fy,
    Cx,
    Cy,
    K1,
    K2,
}

fn free_params(o: &Options) -> Vec<Param> {
    let mut v =
        vec![Param::Rot(0), Param::Rot(1), Param::Rot(2), Param::Trans(0), Param::Trans(1), Param::Trans(2)];
    if !o.fix_focal {
        if o.square_pixels {
            v.push(Param::Focal);
        } else {
            v.extend([Param::Fx, Param::Fy]);
        }
    }
    if !o.fix_principal_point {
        v.extend([Param::Cx, Param::Cy]);
    }
    if o.distortion {
        v.extend([Param::K1, Param::K2]);
    }
    v
}

impl State {
    /// The state moved by `delta`, one entry per free parameter.
    fn moved(&self, free: &[Param], delta: &[f64]) -> State {
        let mut s = *self;
        let mut omega = Vector3::zeros();
        for (p, d) in free.iter().zip(delta) {
            let d = *d;
            match p {
                Param::Rot(i) => add(&mut omega, *i, d),
                Param::Trans(i) => add(&mut s.t, *i, d),
                Param::Focal => s.k.focal = [s.k.focal[0] + d, s.k.focal[1] + d],
                Param::Fx => s.k.focal[0] += d,
                Param::Fy => s.k.focal[1] += d,
                Param::Cx => s.k.principal[0] += d,
                Param::Cy => s.k.principal[1] += d,
                Param::K1 => s.k.distortion[0] += d,
                Param::K2 => s.k.distortion[1] += d,
            }
        }
        if omega != Vector3::zeros() {
            s.r = Rotation3::from_scaled_axis(omega) * s.r;
        }
        s
    }

    /// The finite-difference step for a parameter.
    fn step(&self, p: Param) -> f64 {
        let h = 1e-6;
        match p {
            Param::Rot(_) | Param::K1 | Param::K2 => h,
            Param::Trans(i) => h * (1.0 + self.t.get(i).copied().unwrap_or(0.0).abs()),
            Param::Focal | Param::Fx | Param::Fy | Param::Cx | Param::Cy => {
                h * (1.0 + self.k.focal[0].abs().max(self.k.focal[1].abs()))
            }
        }
    }

    /// Reprojection residuals (du, dv per pair) into `out` and their sum of squares; `None`
    /// when a point is behind the projector or a number is not finite.
    fn residuals(&self, pts: &[(Vector3<f64>, [f64; 2])], out: &mut Vec<f64>) -> Option<f64> {
        out.clear();
        if self.k.focal.iter().any(|f| !positive(*f)) {
            return None;
        }
        let mut cost = 0.0;
        for (x, q) in pts {
            let p = self.k.project_local(&(self.r * x + self.t))?;
            let (du, dv) = (p[0] - q[0], p[1] - q[1]);
            cost += du * du + dv * dv;
            out.extend([du, dv]);
        }
        cost.is_finite().then_some(cost)
    }

    /// The Jacobian of the residuals by central differences, `2n x free.len()`.
    fn jacobian(&self, free: &[Param], pts: &[(Vector3<f64>, [f64; 2])]) -> Option<DMatrix<f64>> {
        let rows = 2 * pts.len();
        let mut data = Vec::with_capacity(rows * free.len());
        let (mut plus, mut minus) = (Vec::new(), Vec::new());
        let mut delta = vec![0.0; free.len()];
        for (j, p) in free.iter().enumerate() {
            let h = self.step(*p);
            set(&mut delta, j, h);
            self.moved(free, &delta).residuals(pts, &mut plus)?;
            set(&mut delta, j, -h);
            self.moved(free, &delta).residuals(pts, &mut minus)?;
            set(&mut delta, j, 0.0);
            data.extend(plus.iter().zip(&minus).map(|(a, b)| (a - b) / (2.0 * h)));
        }
        (data.len() == rows * free.len()).then(|| DMatrix::from_vec(rows, free.len(), data))
    }
}

/// `x > 0`, false for NaN (so `!positive(x)` refuses NaN too).
fn positive(x: f64) -> bool {
    x > 0.0
}

fn add(v: &mut Vector3<f64>, i: usize, d: f64) {
    if let Some(e) = v.get_mut(i) {
        *e += d;
    }
}

fn set(v: &mut [f64], i: usize, x: f64) {
    if let Some(e) = v.get_mut(i) {
        *e = x;
    }
}

struct Fit {
    state: State,
    cost: f64,
    iterations: u32,
    converged: bool,
}

/// Levenberg-Marquardt with Marquardt's diagonal scaling. Stops when a step no longer lowers
/// the cost noticeably, when no damping finds a lower cost (a minimum to working precision),
/// or after `max_iterations`. `None` when the start itself puts points behind the projector.
fn levenberg_marquardt(
    start: State,
    free: &[Param],
    pts: &[(Vector3<f64>, [f64; 2])],
    max_iterations: u32,
) -> Option<Fit> {
    let mut r = Vec::new();
    let mut trial = Vec::new();
    let mut cost = start.residuals(pts, &mut r)?;
    let mut s = start;
    let mut lambda = 1e-3;
    let mut iterations = 0;
    let mut converged = false;
    while iterations < max_iterations && !converged {
        iterations += 1;
        if cost <= 1e-24 * pts.len() as f64 {
            converged = true;
            break;
        }
        let Some(j) = s.jacobian(free, pts) else { break };
        let jt = j.transpose();
        let jtj = &jt * &j;
        let g = &jt * DVector::from_column_slice(&r);
        let floor = 1e-12 * jtj.diagonal().amax().max(1e-300);
        let mut improved = false;
        while lambda <= 1e16 {
            let mut a = jtj.clone();
            for i in 0..free.len() {
                if let Some(e) = a.get_mut((i, i)) {
                    *e += lambda * (*e).max(floor);
                }
            }
            let Some(chol) = a.cholesky() else {
                lambda *= 10.0;
                continue;
            };
            let delta = -chol.solve(&g);
            let cand = s.moved(free, delta.as_slice());
            match cand.residuals(pts, &mut trial) {
                Some(c) if c < cost => {
                    converged = cost - c <= 1e-12 * cost;
                    s = cand;
                    cost = c;
                    std::mem::swap(&mut r, &mut trial);
                    lambda = (lambda / 10.0).max(1e-12);
                    improved = true;
                    break;
                }
                _ => lambda *= 10.0,
            }
        }
        if !improved {
            // No damping lowers the cost: a minimum to working precision.
            converged = true;
        }
    }
    Some(Fit { state: s, cost, iterations, converged })
}

/// The intrinsics' standard deviations at the fit; an error when the free parameters are not
/// determined by the points (the normal matrix is singular).
fn spread(fit: &Fit, free: &[Param], pts: &[(Vector3<f64>, [f64; 2])]) -> Result<Option<Spread>, Error> {
    let undetermined =
        || Error::new("these points do not determine the free parameters (fix some, or add points)");
    let j = fit.state.jacobian(free, pts).ok_or_else(undetermined)?;
    let norms: Vec<f64> = j.column_iter().map(|c| c.norm()).collect();
    if norms.iter().any(|n| !positive(*n) || !n.is_finite()) {
        return Err(undetermined());
    }
    let scale = DMatrix::from_diagonal(&DVector::from_iterator(norms.len(), norms.iter().map(|n| 1.0 / n)));
    let jn = &j * &scale;
    let normal = jn.transpose() * &jn;
    let eig = normal.clone().try_symmetric_eigen(f64::EPSILON, 1000).ok_or_else(undetermined)?;
    let (lo, hi) =
        eig.eigenvalues.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), e| (lo.min(*e), hi.max(*e)));
    if !positive(lo - 1e-12 * hi) {
        return Err(undetermined());
    }
    let dof = 2 * pts.len() - free.len().min(2 * pts.len());
    if dof == 0 {
        return Ok(None);
    }
    let Some(inv) = normal.try_inverse() else { return Err(undetermined()) };
    let sigma2 = fit.cost / dof as f64;
    let sd = |p: Param| {
        free.iter()
            .position(|q| *q == p)
            .and_then(|i| Some((i, inv.get((i, i))?, norms.get(i)?)))
            .map_or(0.0, |(_, v, n)| (sigma2 * v.max(0.0)).sqrt() / n)
    };
    let focal =
        if free.contains(&Param::Focal) { [sd(Param::Focal); 2] } else { [sd(Param::Fx), sd(Param::Fy)] };
    Ok(Some(Spread { focal, principal: [sd(Param::Cx), sd(Param::Cy)] }))
}

/// The 3x4 projection by the normalised DLT (Hartley and Zisserman, algorithm 7.1), from local
/// world points (already centred and scaled) to `img`, signed so that `det(M) > 0`; an error
/// unless every point ends up in front.
fn dlt(pts: &[(Vector3<f64>, [f64; 2])], img: &[[f64; 2]]) -> Result<Matrix3x4<f64>, Error> {
    let t = normalising(img)?;
    let rows = (2 * pts.len()).max(12);
    let mut a = DMatrix::<f64>::zeros(rows, 12);
    for (k, ((x, _), q)) in pts.iter().zip(img).enumerate() {
        let q = t * Vector3::new(q[0], q[1], 1.0);
        let (u, v) = (q.x / q.z, q.y / q.z);
        let xh = [x.x, x.y, x.z, 1.0];
        let mut first = [0.0; 12];
        let mut second = [0.0; 12];
        for (c, xc) in xh.iter().enumerate() {
            set(&mut first, c, *xc);
            set(&mut first, 8 + c, -u * xc);
            set(&mut second, 4 + c, *xc);
            set(&mut second, 8 + c, -v * xc);
        }
        for (c, (f, s)) in first.iter().zip(&second).enumerate() {
            if let Some(e) = a.get_mut((2 * k, c)) {
                *e = *f;
            }
            if let Some(e) = a.get_mut((2 * k + 1, c)) {
                *e = *s;
            }
        }
    }
    let svd =
        a.try_svd(false, true, f64::EPSILON, 1000).ok_or_else(|| Error::new("the decomposition failed"))?;
    let v_t = svd.v_t.as_ref().ok_or_else(|| Error::new("the decomposition failed"))?;
    let mut order: Vec<(usize, f64)> = svd.singular_values.iter().copied().enumerate().collect();
    order.sort_by(|x, y| x.1.total_cmp(&y.1));
    let (smallest, second, largest) = match order.as_slice() {
        [s, t, .., l] => (s.0, t.1, l.1),
        _ => return Err(Error::new("the decomposition failed")),
    };
    if !positive(largest) || second <= largest * 1e-9 {
        return Err(Error::new("the points do not determine a projection (degenerate configuration)"));
    }
    let p: Vec<f64> = v_t.row(smallest).iter().copied().collect();
    if p.len() != 12 {
        return Err(Error::new("the decomposition failed"));
    }
    let back = t.try_inverse().ok_or_else(|| Error::new("the picture points are degenerate"))?;
    let mut p = back * Matrix3x4::from_row_slice(&p);
    if p.fixed_view::<3, 3>(0, 0).determinant() < 0.0 {
        p = -p;
    }
    let depths = pts.iter().map(|(x, _)| p.row(2).dot(&Vector4::new(x.x, x.y, x.z, 1.0).transpose()));
    let front = depths.filter(|d| *d > 0.0).count();
    if front == 0 {
        return Err(Error::new("every point is behind the projector (is the picture mirrored?)"));
    }
    if front < pts.len() {
        return Err(Error::new("the pairs disagree: some points would be behind the projector"));
    }
    Ok(p)
}

/// The DLT's starting point. With `known` intrinsics (focal, principal point) the picture
/// points are normalised first and only the pose is taken from the projection.
fn dlt_start(
    pts: &[(Vector3<f64>, [f64; 2])],
    size: [u32; 2],
    known: Option<([f64; 2], [f64; 2])>,
) -> Result<State, Error> {
    if let Some(([fx, fy], [cx, cy])) = known {
        let img: Vec<[f64; 2]> = pts.iter().map(|(_, q)| [(q[0] - cx) / fx, (q[1] - cy) / fy]).collect();
        let p = dlt(pts, &img)?;
        let m = p.fixed_view::<3, 3>(0, 0).into_owned();
        let svd = m
            .try_svd(true, true, f64::EPSILON, 1000)
            .ok_or_else(|| Error::new("the decomposition failed"))?;
        let (Some(u), Some(v_t)) = (svd.u, svd.v_t) else {
            return Err(Error::new("the decomposition failed"));
        };
        let scale = svd.singular_values.sum() / 3.0;
        let r = u * v_t;
        if !positive(scale) || r.determinant() < 0.0 {
            return Err(Error::new("the projection has no proper rotation"));
        }
        let t = p.column(3).into_owned() / scale;
        let k = Intrinsics { size, focal: [fx, fy], principal: [cx, cy], distortion: [0.0; 2] };
        return Ok(State { r: Rotation3::from_matrix_unchecked(r), t, k });
    }
    let img: Vec<[f64; 2]> = pts.iter().map(|(_, q)| *q).collect();
    let p = dlt(pts, &img)?;
    let (k, r) = rq(&p.fixed_view::<3, 3>(0, 0).into_owned())
        .ok_or_else(|| Error::new("the projection does not split into lens and rotation"))?;
    let t = k.try_inverse().ok_or_else(|| Error::new("the lens matrix is singular"))? * p.column(3);
    let k = k / k.m33;
    let (fx, fy) = (k.m11, k.m22);
    if !(fx > 0.0 && fy > 0.0) || ![fx, fy, k.m13, k.m23].iter().all(|v| v.is_finite()) {
        return Err(Error::new("the projection gives no usable lens"));
    }
    let k = Intrinsics { size, focal: [fx, fy], principal: [k.m13, k.m23], distortion: [0.0; 2] };
    Ok(State { r: Rotation3::from_matrix_unchecked(r), t, k })
}

/// `M = K R` with `K` upper triangular with a positive diagonal and `R` a rotation (for
/// `det(M) > 0`), through the QR decomposition of the flipped transpose.
fn rq(m: &Matrix3<f64>) -> Option<(Matrix3<f64>, Matrix3<f64>)> {
    let flip = Matrix3::new(0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0);
    let qr = (flip * m).transpose().qr();
    let k = flip * qr.r().transpose() * flip;
    let r = flip * qr.q().transpose();
    let signs = Vector3::new(k.m11.signum(), k.m22.signum(), k.m33.signum());
    if k.m11 == 0.0 || k.m22 == 0.0 || k.m33 == 0.0 {
        return None;
    }
    let d = Matrix3::from_diagonal(&signs);
    let (k, r) = (k * d, d * r);
    (r.determinant() > 0.0 && k.iter().chain(r.iter()).all(|v| v.is_finite())).then_some((k, r))
}

/// The starting point for points on (or near) a plane: the homography from plane coordinates
/// to the picture, with the principal point given and the focal length given or estimated from
/// the homography (both picture axes of the plane must stay perpendicular and equally long,
/// Zhang's constraints with one view and square pixels).
fn plane_start(
    pts: &[(Vector3<f64>, [f64; 2])],
    shape: &Shape,
    size: [u32; 2],
    principal: [f64; 2],
    focal: Option<[f64; 2]>,
) -> Result<State, Error> {
    let (e1, e2) = (shape.major, shape.minor);
    let pairs: Vec<([f64; 2], [f64; 2])> = pts.iter().map(|(x, q)| ([x.dot(&e1), x.dot(&e2)], *q)).collect();
    let h = Homography::from_pairs(&pairs)?.rows();
    let [cx, cy] = principal;
    let to_centre = Matrix3::new(1.0, 0.0, -cx, 0.0, 1.0, -cy, 0.0, 0.0, 1.0);
    let [[a0, a1, a2], [b0, b1, b2], [c0, c1, c2]] = h;
    let hc = to_centre * Matrix3::new(a0, a1, a2, b0, b1, b2, c0, c1, c2);
    let [fx, fy] = match focal {
        Some(f) => f,
        None => {
            // Normalised so the sizes of the terms compare; w = 1 / f².
            let hc = hc / hc.norm();
            let (h11, h12, h21, h22, h31, h32) = (hc.m11, hc.m12, hc.m21, hc.m22, hc.m31, hc.m32);
            let (p1, q1) = (h11 * h12 + h21 * h22, h31 * h32);
            let (p2, q2) = (h11 * h11 + h21 * h21 - h12 * h12 - h22 * h22, h31 * h31 - h32 * h32);
            let w = -(p1 * q1 + p2 * q2) / (p1 * p1 + p2 * p2);
            let f = 1.0 / w.sqrt();
            let width = f64::from(size[0]);
            if !positive(w) || !f.is_finite() || f < 0.05 * width || f > 50.0 * width {
                return Err(Error::new(
                    "the plane is seen too squarely to tell the focal length: fix the focal length, or give an initial camera",
                ));
            }
            [f, f]
        }
    };
    let b = Matrix3::new(1.0 / fx, 0.0, 0.0, 0.0, 1.0 / fy, 0.0, 0.0, 0.0, 1.0) * hc;
    let (b1, b2, b3) = (b.column(0).into_owned(), b.column(1).into_owned(), b.column(2).into_owned());
    let mut lambda = 2.0 / (b1.norm() + b2.norm());
    if b3.z * lambda < 0.0 {
        lambda = -lambda;
    }
    let (r1, r2) = (b1 * lambda, b2 * lambda);
    let rp = Matrix3::from_columns(&[r1, r2, r1.cross(&r2)]);
    let svd =
        rp.try_svd(true, true, f64::EPSILON, 1000).ok_or_else(|| Error::new("the decomposition failed"))?;
    let (Some(u), Some(v_t)) = (svd.u, svd.v_t) else { return Err(Error::new("the decomposition failed")) };
    let rp = u * v_t;
    if !lambda.is_finite() || rp.determinant() < 0.0 {
        return Err(Error::new("the plane's homography gives no pose"));
    }
    let axes = Matrix3::from_columns(&[e1, e2, e1.cross(&e2)]);
    let r = rp * axes.transpose();
    let k = Intrinsics { size, focal: [fx, fy], principal, distortion: [0.0; 2] };
    Ok(State { r: Rotation3::from_matrix_unchecked(r), t: b3 * lambda, k })
}
