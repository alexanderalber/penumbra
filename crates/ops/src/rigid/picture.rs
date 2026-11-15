//! The bodies of `physics.rigid_bodies` as pictures, for a simulation in a picture such as
//! `tex.liquid`: where they are (`mask`) and how fast each place of them moves (`velocity`),
//! seen through a camera. Each body is drawn as the outline of what it collides as: its box,
//! ball or hull, flattened by the camera. Velocities come from the solver, not from comparing
//! frames: a place of a body moves at the body's speed plus what its spin adds there.
//!
//! The velocity is a picture's own unit: red in picture widths per second, green in picture
//! heights per second, down positive, as `tex.liquid` reads `obstacle_velocity`. It is exact for
//! an orthographic camera that looks along the axis the bodies turn about (the solver's `plane`).
//! Otherwise a body's spin is taken in the plane through its centre that faces the camera.

use penumbra_gpu::geo::Mat4;
use penumbra_physics::{Motion, Placement, Shape};

use super::group::Bodies;
use crate::geo::trs;

/// Points on a ball's outline.
const ROUND: usize = 64;

type V3 = [f64; 3];

fn dot([a, b, c]: V3, [x, y, z]: V3) -> f64 {
    a * x + b * y + c * z
}

fn cross([a, b, c]: V3, [x, y, z]: V3) -> V3 {
    [b * z - c * y, c * x - a * z, a * y - b * x]
}

fn add([a, b, c]: V3, [x, y, z]: V3) -> V3 {
    [a + x, b + y, c + z]
}

fn scaled([a, b, c]: V3, k: f64) -> V3 {
    [a * k, b * k, c * k]
}

/// One body as the pictures draw it: its outline in pixels, and the velocity at each pixel
/// centre (x, y) as two rows `[a, b, c]` of `a + b x + c y`, red and green.
pub struct Drawn {
    pub outline: Vec<[f32; 2]>,
    pub motion: [[f32; 3]; 2],
}

/// How a camera puts the world into a picture of `size` pixels.
pub struct Seen {
    clip: Mat4,
    size: [f64; 2],
}

impl Seen {
    pub fn new(clip: Mat4, size: (u32, u32)) -> Self {
        Self { clip, size: [f64::from(size.0.max(1)), f64::from(size.1.max(1))] }
    }

    /// The clip space position of `p`.
    fn clip_of(&self, [x, y, z]: V3) -> [f64; 4] {
        self.clip.map(|[a, b, c, d]| a * x + b * y + c * z + d)
    }

    /// Where `p` lands in the picture, in pixels from the top left; none behind the camera.
    pub fn pixel(&self, p: V3) -> Option<[f64; 2]> {
        let [x, y, _, w] = self.clip_of(p);
        // Also not a number: behind.
        if w.partial_cmp(&1e-9) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        let [sw, sh] = self.size;
        let px = [(x / w + 1.0) * 0.5 * sw, (1.0 - y / w) * 0.5 * sh];
        px.iter().all(|v| v.is_finite()).then_some(px)
    }

    /// How the pixel of `p` moves when `p` moves: pixels per metre along x, y and z.
    fn jacobian(&self, p: V3) -> Option<[V3; 2]> {
        let [x, y, _, w] = self.clip_of(p);
        if w.partial_cmp(&1e-9) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        let [c0, c1, _, c3] = self.clip;
        let [sw, sh] = self.size;
        let row = |[a, b, c, _]: [f64; 4], v: f64, s: f64| {
            let [d, e, f, _] = c3;
            [a * w - v * d, b * w - v * e, c * w - v * f].map(|t| s * t / (w * w))
        };
        Some([row(c0, x, 0.5 * sw), row(c1, y, -0.5 * sh)])
    }

    /// The velocity in the picture of a body moving as `m`, as two rows of `a + b x + c y`
    /// over pixel centres: red in picture widths, green in picture heights per second. Still
    /// where the camera cannot say.
    pub fn motion(&self, m: &Motion) -> [[f32; 3]; 2] {
        let still = [[0.0; 3]; 2];
        let (Some([j0, j1]), Some([sx, sy])) = (self.jacobian(m.center), self.pixel(m.center)) else {
            return still;
        };
        // A pixel offset back into the world, in the plane through the centre that faces the
        // camera: the least way that lands there (J^T (J J^T)^-1).
        let (a, b, d) = (dot(j0, j0), dot(j0, j1), dot(j1, j1));
        let det = a * d - b * b;
        if !(det.abs() > 1e-12 * (a * d) && det.is_finite()) {
            return still;
        }
        let back_x = add(scaled(j0, d / det), scaled(j1, -b / det));
        let back_y = add(scaled(j0, -b / det), scaled(j1, a / det));
        let w = m.spin.map(f64::from);
        // The turn's share: how a pixel offset from the centre changes the velocity there.
        let (tx, ty) = (cross(w, back_x), cross(w, back_y));
        let v = m.velocity.map(f64::from);
        let [sw, sh] = self.size;
        let row = |j: V3, unit: f64| {
            let (gx, gy) = (dot(j, tx), dot(j, ty));
            [dot(j, v) - gx * sx - gy * sy, gx, gy].map(|t| (t / unit) as f32)
        };
        let rows = [row(j0, sw), row(j1, sh)];
        if rows.iter().flatten().all(|x| x.is_finite()) { rows } else { still }
    }

    /// The axis the camera looks along, in the world: what moves no pixel.
    fn along(&self, p: V3) -> Option<V3> {
        let [a, b] = self.jacobian(p)?;
        let n = cross(a, b);
        let len = dot(n, n).sqrt();
        (len > 0.0 && len.is_finite()).then(|| scaled(n, 1.0 / len))
    }
}

/// `p` placed by `m`.
fn moved(m: &Mat4, [x, y, z]: V3) -> V3 {
    let [r0, r1, r2, _] = m;
    [r0, r1, r2].map(|[a, b, c, d]| a * x + b * y + c * z + d)
}

/// The points of a body's collision shape in the world, whose hull in the picture is its
/// outline there. A ball gives points around it in the plane that faces the camera.
fn points(shape: &Shape, place: &Placement, seen: &Seen) -> Vec<V3> {
    let m = trs(place.translate, place.rotate_degrees, place.scale.map(f64::from));
    let f = |p: [f32; 3]| p.map(f64::from);
    match shape {
        Shape::Box { center, half } => {
            let ([cx, cy, cz], [hx, hy, hz]) = (f(*center), f(*half));
            let mut corners = Vec::with_capacity(8);
            for sx in [-hx, hx] {
                for sy in [-hy, hy] {
                    for sz in [-hz, hz] {
                        corners.push(moved(&m, [cx + sx, cy + sy, cz + sz]));
                    }
                }
            }
            corners
        }
        Shape::Hull { points } => points.iter().map(|p| moved(&m, f(*p))).collect(),
        Shape::Sphere { center, radius } => {
            // As the solver has it: a ball stays a ball, the widest scale decides.
            let widest = place.scale.iter().fold(0.0f32, |w, s| w.max(s.abs()));
            let r = f64::from(radius * widest);
            let c = moved(&m, f(*center));
            let Some(n) = seen.along(c) else { return Vec::new() };
            // Two directions square to the view and to each other.
            let pick = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let e1 = cross(n, pick);
            let e1 = scaled(e1, 1.0 / dot(e1, e1).sqrt());
            let e2 = cross(n, e1);
            (0..ROUND)
                .map(|k| {
                    let (s, co) = (k as f64 * std::f64::consts::TAU / ROUND as f64).sin_cos();
                    add(c, add(scaled(e1, r * co), scaled(e2, r * s)))
                })
                .collect()
        }
    }
}

/// The convex hull of `points` (Andrew's monotone chain), without repeats; empty for fewer
/// than three points that are not on one line.
pub fn hull(mut points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    points.retain(|p| p.iter().all(|v| v.is_finite()));
    points.sort_by(|[ax, ay], [bx, by]| ax.total_cmp(bx).then(ay.total_cmp(by)));
    points.dedup();
    if points.len() < 3 {
        return Vec::new();
    }
    let turn = |[ox, oy]: [f64; 2], [ax, ay]: [f64; 2], [bx, by]: [f64; 2]| {
        (ax - ox) * (by - oy) - (ay - oy) * (bx - ox)
    };
    // The lower chain left to right, then the upper one back.
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(points.len() + 1);
    let chain = |run: &mut dyn Iterator<Item = &[f64; 2]>, out: &mut Vec<[f64; 2]>| {
        let start = out.len();
        for &p in run {
            while let [.., a, b] = out.as_slice()
                && out.len() >= start + 2
                && turn(*a, *b, p) <= 0.0
            {
                out.pop();
            }
            out.push(p);
        }
        out.pop();
    };
    chain(&mut points.iter(), &mut out);
    chain(&mut points.iter().rev(), &mut out);
    if out.len() < 3 { Vec::new() } else { out }
}

/// Every body of `groups` as the pictures draw it, group after group. A body behind the
/// camera or flattened to a line is left out.
pub fn drawn(groups: &[(&Bodies, Vec<Placement>, Vec<Motion>)], seen: &Seen) -> Vec<Drawn> {
    let mut out = Vec::new();
    for (bodies, places, motions) in groups {
        for (k, place) in places.iter().enumerate() {
            let pixels: Option<Vec<[f64; 2]>> =
                points(&bodies.shape, place, seen).into_iter().map(|p| seen.pixel(p)).collect();
            let outline = hull(pixels.unwrap_or_default());
            if outline.is_empty() {
                continue;
            }
            let motion = motions.get(k).map_or([[0.0; 3]; 2], |m| seen.motion(m));
            out.push(Drawn { outline: outline.iter().map(|p| p.map(|v| v as f32)).collect(), motion });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An orthographic camera at y = -10 looking along +y, 3 m high, for a picture of 160 x 90:
    /// x right, z up, 30 pixels per metre, the origin in the middle.
    fn front() -> Seen {
        let (h, aspect) = (3.0, 160.0 / 90.0);
        let (sx, sy) = (2.0 / (h * aspect), 2.0 / h);
        let clip = [[sx, 0.0, 0.0, 0.0], [0.0, 0.0, sy, 0.0], [0.0, 0.01, 0.0, 0.1], [0.0, 0.0, 0.0, 1.0]];
        Seen::new(clip, (160, 90))
    }

    fn at(m: &[[f32; 3]; 2], p: [f64; 2]) -> [f64; 2] {
        let row = |r: [f32; 3]| f64::from(r[0]) + f64::from(r[1]) * p[0] + f64::from(r[2]) * p[1];
        [row(m[0]), row(m[1])]
    }

    #[test]
    fn a_body_that_slides_and_one_that_turns_move_as_the_solver_says() {
        let seen = front();
        let p = seen.pixel([0.0, 3.0, 1.0]).unwrap();
        assert!((p[0] - 80.0).abs() < 1e-9 && (p[1] - 15.0).abs() < 1e-9, "{p:?}");
        // Sliding right at 1.6 m/s and falling at 0.9: 48 / 160 widths, 27 / 90 heights down.
        let slide = Motion { velocity: [1.6, 0.0, -0.9], spin: [0.0; 3], center: [1.0, 0.0, 1.0] };
        let v = at(&seen.motion(&slide), [5.0, 7.0]);
        assert!((v[0] - 0.3).abs() < 1e-6 && (v[1] - 0.3).abs() < 1e-6, "{v:?}");
        // Turning about y at 2 rad/s through (1, 0, 0): a point 0.5 m above moves by 2 x 0.5
        // along +x (y cross z), and one 0.5 m to the right 1 m/s along -z, down on screen.
        let turn = Motion { velocity: [0.0; 3], spin: [0.0, 2.0, 0.0], center: [1.0, 0.0, 0.0] };
        let c = seen.pixel([1.0, 0.0, 0.0]).unwrap();
        let above = at(&seen.motion(&turn), [c[0], c[1] - 15.0]);
        assert!((above[0] - 30.0 / 160.0).abs() < 1e-5 && above[1].abs() < 1e-5, "{above:?}");
        let right = at(&seen.motion(&turn), [c[0] + 15.0, c[1]]);
        assert!(right[0].abs() < 1e-5 && (right[1] - 30.0 / 90.0).abs() < 1e-5, "{right:?}");
        // The same as Motion::at says for the point in the world.
        let p = turn.at([1.5, 0.0, 0.0]);
        assert!((p[2] + 1.0).abs() < 1e-9, "{p:?}");
        // A camera that sees nothing: no motion.
        let blind = Seen::new([[0.0; 4]; 4], (160, 90));
        assert_eq!(blind.motion(&slide), [[0.0; 3]; 2]);
        let huge = Motion { velocity: [f32::MAX, 0.0, 0.0], ..slide };
        assert!(seen.motion(&huge).iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn outlines_are_the_shapes_seen_flat() {
        let seen = front();
        let place = Placement { translate: [0.0; 3], rotate_degrees: [0.0, 45.0, 0.0], scale: [1.0; 3] };
        let cube = Shape::Box { center: [0.0; 3], half: [0.5; 3] };
        let outline =
            hull(points(&cube, &place, &seen).into_iter().map(|p| seen.pixel(p).unwrap()).collect());
        // A cube turned 45 degrees about the view: a diamond reaching 0.707 m from its middle.
        assert_eq!(outline.len(), 4, "{outline:?}");
        let reach = outline.iter().map(|p| (p[0] - 80.0).abs().max((p[1] - 45.0).abs())).fold(0.0, f64::max);
        assert!((reach - 30.0 * 0.5f64.sqrt()).abs() < 1e-3, "{reach}");
        let ball = Shape::Sphere { center: [0.0; 3], radius: 0.5 };
        let place = Placement { scale: [1.0, 2.0, 1.0], ..place };
        let outline =
            hull(points(&ball, &place, &seen).into_iter().map(|p| seen.pixel(p).unwrap()).collect());
        assert_eq!(outline.len(), ROUND);
        assert!(outline.iter().all(|p| ((p[0] - 80.0).hypot(p[1] - 45.0) - 30.0).abs() < 1e-6));
    }

    #[test]
    fn a_hull_of_points_on_a_line_or_not_numbers_is_empty() {
        assert!(hull(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]).is_empty());
        assert!(hull(vec![[0.0, 0.0], [f64::NAN, 1.0], [2.0, 0.0]]).is_empty());
        let square = hull(vec![[0.0, 0.0], [1.0, 0.0], [0.5, 0.5], [1.0, 1.0], [0.0, 1.0], [1.0, 1.0]]);
        assert_eq!(square.len(), 4, "{square:?}");
    }
}
