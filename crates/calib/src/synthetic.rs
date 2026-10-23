//! A ray-cast room for tests and demonstrations of [`crate::scene`]: a floor, a wall, a box
//! standing on the floor with a ball on its top, a camera above looking down at it and
//! projectors beside the camera. Its maps are what `scan.decode` writes for such a room.
//!
//! World units are decimetres: the box is a 60 x 40 x 31 cm crate (x across, z away from the
//! camera, y up), the ball 32 cm across, the wall 1.9 m behind the camera.

use crate::objects::Map;

/// The default room's box: x, y and z ranges.
pub const BOX: [[f64; 2]; 3] = [[-3.0, 3.0], [0.0, 3.1], [11.0, 15.0]];
/// A box: x, y and z ranges.
pub type Cuboid = [[f64; 2]; 3];
pub const WALL_Z: f64 = 19.0;

/// What a ray hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    Floor,
    Wall,
    Top,
    Front,
    Side,
    Ball,
}

/// The room's cameras and the ball.
#[derive(Clone, Debug)]
pub struct Room {
    pub camera: (usize, usize),
    pub focal: f64,
    pub projector: (usize, usize),
    pub projector_focal: f64,
    pub eye: [f64; 3],
    pub target: [f64; 3],
    /// Each projector's offset from the camera along the camera's right; each looks at the
    /// target.
    pub offsets: Vec<f64>,
    /// Boxes standing in the room (on the floor or on each other).
    pub boxes: Vec<Cuboid>,
    /// Centre and radius.
    pub ball: [f64; 4],
    /// Uniform noise on the camera positions, up to this many camera pixels either way.
    pub noise: f64,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            camera: (480, 360),
            focal: 400.0,
            projector: (400, 300),
            projector_focal: 340.0,
            eye: [0.0, 6.5, 0.0],
            target: [0.0, 2.5, 13.0],
            offsets: vec![-4.0, 4.0],
            boxes: vec![BOX],
            ball: [0.3, 4.6, 13.2, 1.6],
            noise: 0.25,
        }
    }
}

type V = [f64; 3];

fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V, s: f64) -> V {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V) -> V {
    let l = dot(a, a).sqrt().max(1e-12);
    mul(a, 1.0 / l)
}

/// A pinhole: position, forward, right and down.
#[derive(Clone, Copy)]
struct Pinhole {
    at: V,
    f: V,
    r: V,
    d: V,
    focal: f64,
    size: (usize, usize),
}

impl Pinhole {
    fn ray(&self, x: f64, y: f64) -> V {
        let (w, h) = (self.size.0 as f64, self.size.1 as f64);
        add(add(self.f, mul(self.r, (x - w / 2.0) / self.focal)), mul(self.d, (y - h / 2.0) / self.focal))
    }

    fn project(&self, p: V) -> Option<[f64; 2]> {
        let q = sub(p, self.at);
        let z = dot(q, self.f);
        if z <= 1e-9 {
            return None;
        }
        let (w, h) = (self.size.0 as f64, self.size.1 as f64);
        let x = self.focal * dot(q, self.r) / z + w / 2.0;
        let y = self.focal * dot(q, self.d) / z + h / 2.0;
        ((0.0..w).contains(&x) && (0.0..h).contains(&y)).then_some([x, y])
    }
}

impl Room {
    /// A landscape: two crates side by side with a gap (their tops and fronts coplanar), a third
    /// one stacked on the left one's back, the ball on the right one.
    pub fn landscape() -> Room {
        Room {
            boxes: vec![
                [[-6.0, 0.0], [0.0, 3.1], [11.0, 15.0]],
                [[0.4, 6.4], [0.0, 3.1], [11.0, 15.0]],
                [[-6.0, 0.0], [3.1, 6.2], [12.6, 16.6]],
            ],
            ball: [3.4, 4.6, 13.4, 1.6],
            eye: [0.0, 10.0, 0.0],
            target: [0.0, 2.5, 13.5],
            ..Room::default()
        }
    }

    fn basis(&self) -> (V, V, V) {
        let f = unit(sub(self.target, self.eye));
        let r = unit(cross([0.0, 1.0, 0.0], f));
        let d = cross(r, f);
        (f, r, d)
    }

    fn camera_pinhole(&self) -> Pinhole {
        let (f, r, d) = self.basis();
        Pinhole { at: self.eye, f, r, d, focal: self.focal, size: self.camera }
    }

    /// Projector `p`: beside the camera along its right, turned towards the target.
    fn projector_pinhole(&self, p: usize) -> Pinhole {
        let (_, right, _) = self.basis();
        let at = add(self.eye, mul(right, self.offsets.get(p).copied().unwrap_or(0.0)));
        let f = unit(sub(self.target, at));
        let r = unit(cross([0.0, 1.0, 0.0], f));
        Pinhole { at, f, r, d: cross(r, f), focal: self.projector_focal, size: self.projector }
    }

    /// The first surface along the ray from `o` in direction `d`: its distance (in units of
    /// `d`) and kind.
    pub fn hit(&self, o: V, d: V) -> Option<(f64, Surface)> {
        let mut best: Option<(f64, Surface)> = None;
        let mut take = |t: f64, s: Surface| {
            if t > 1e-9 && best.is_none_or(|(b, _)| t < b) {
                best = Some((t, s));
            }
        };
        if d[1] < 0.0 {
            take(-o[1] / d[1], Surface::Floor);
        }
        if d[2] > 0.0 {
            take((WALL_Z - o[2]) / d[2], Surface::Wall);
        }
        for b in &self.boxes {
            if let Some((t, s)) = enter(b, o, d) {
                take(t, s);
            }
        }
        // The ball.
        let [cx, cy, cz, r] = self.ball;
        let oc = sub(o, [cx, cy, cz]);
        let (a, b, c) = (dot(d, d), 2.0 * dot(oc, d), dot(oc, oc) - r * r);
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 && a > 0.0 {
            take((-b - disc.sqrt()) / (2.0 * a), Surface::Ball);
        }
        best
    }

    /// Whether `p` is the first thing seen from `from`.
    fn visible(&self, from: V, p: V) -> bool {
        let d = sub(p, from);
        self.hit(from, d).is_some_and(|(t, _)| (t - 1.0).abs() < 1e-6)
    }

    /// Projector `p`'s `proj_to_cam` map (every third pixel marked filled, as a coarser camera
    /// leaves them), and per projector pixel what it lights where the camera sees it, and the
    /// point.
    pub fn to_camera(&self, p: usize) -> (Map, Vec<Option<(Surface, V)>>) {
        let (proj, cam) = (self.projector_pinhole(p), self.camera_pinhole());
        let (pw, ph) = self.projector;
        let (cw, ch) = (self.camera.0 as f32, self.camera.1 as f32);
        let mut rgba = Vec::with_capacity(pw * ph);
        let mut truth = Vec::with_capacity(pw * ph);
        for v in 0..ph {
            for u in 0..pw {
                let d = proj.ray(u as f64 + 0.5, v as f64 + 0.5);
                let seen = self.hit(proj.at, d).and_then(|(t, s)| {
                    let point = add(proj.at, mul(d, t));
                    if !self.visible(cam.at, point) {
                        return None;
                    }
                    cam.project(point).map(|c| (c, s, point))
                });
                match seen {
                    Some((c, s, point)) => {
                        let i = (v * pw + u) as u64;
                        let jitter = |k: u64| (hash(i * 2 + k) * 2.0 - 1.0) * self.noise;
                        let measured = if (u + v) % 3 == 0 { 0.5 } else { 1.0 };
                        let (x, y) = ((c[0] + jitter(0)) as f32, (c[1] + jitter(1)) as f32);
                        rgba.push([(x / cw).clamp(0.0, 1.0), (y / ch).clamp(0.0, 1.0), measured, 1.0]);
                        truth.push(Some((s, point)));
                    }
                    None => {
                        rgba.push([-1.0, -1.0, 0.0, 1.0]);
                        truth.push(None);
                    }
                }
            }
        }
        (Map { width: pw, height: ph, rgba }, truth)
    }

    /// Projector `p`'s `cam_to_proj` map.
    pub fn from_camera(&self, p: usize) -> Map {
        let (proj, cam) = (self.projector_pinhole(p), self.camera_pinhole());
        let (cw, ch) = self.camera;
        let (pw, ph) = (self.projector.0 as f32, self.projector.1 as f32);
        let mut rgba = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            for x in 0..cw {
                let d = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                let lit = self.hit(cam.at, d).and_then(|(t, _)| {
                    let point = add(cam.at, mul(d, t));
                    if !self.visible(proj.at, point) {
                        return None;
                    }
                    proj.project(point)
                });
                rgba.push(match lit {
                    Some(q) => [q[0] as f32 / pw, q[1] as f32 / ph, 1.0, 1.0],
                    None => [-1.0, -1.0, 0.0, 1.0],
                });
            }
        }
        Map { width: cw, height: ch, rgba }
    }

    /// Per camera pixel what it sees.
    pub fn camera_truth(&self) -> Vec<Option<(Surface, V)>> {
        let cam = self.camera_pinhole();
        let (cw, ch) = self.camera;
        let mut out = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            for x in 0..cw {
                let d = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                out.push(self.hit(cam.at, d).map(|(t, s)| (s, add(cam.at, mul(d, t)))));
            }
        }
        out
    }

    /// Where the camera sees a point (camera pixels), if in its picture.
    pub fn camera_point(&self, p: V) -> Option<[f64; 2]> {
        self.camera_pinhole().project(p)
    }

    /// The point of box `b`'s top at `(u, v)` (0 to 1; u across from the left, v from the far
    /// edge towards the camera).
    pub fn top_point(&self, b: usize, u: f64, v: f64) -> V {
        let [x, y, z] = self.boxes.get(b).copied().unwrap_or(BOX);
        [x[0] + u * (x[1] - x[0]), y[1], z[1] - v * (z[1] - z[0])]
    }

    /// The point of box `b`'s front at `(u, v)` (u across from the left, v down from the top).
    pub fn front_point(&self, b: usize, u: f64, v: f64) -> V {
        let [x, y, z] = self.boxes.get(b).copied().unwrap_or(BOX);
        [x[0] + u * (x[1] - x[0]), y[1] - v * (y[1] - y[0]), z[0]]
    }

    /// The box whose surface `p` lies on.
    pub fn box_of(&self, p: V) -> Option<usize> {
        self.boxes.iter().position(|b| b.iter().zip(&p).all(|(r, v)| *v >= r[0] - 1e-6 && *v <= r[1] + 1e-6))
    }
}

/// A number in 0 to 1 from an integer, the same on every run.
fn hash(i: u64) -> f64 {
    let mut x = i.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x6a09_e667_f3bc_c909);
    x ^= x >> 31;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Where the ray from `o` along `d` enters box `b`, by slabs (the entry face is the slab entered
/// last).
fn enter(b: &Cuboid, o: V, d: V) -> Option<(f64, Surface)> {
    let (mut t0, mut t1, mut face) = (f64::NEG_INFINITY, f64::INFINITY, 0usize);
    for (axis, range) in b.iter().enumerate() {
        let (o, d) = (o.get(axis).copied().unwrap_or(0.0), d.get(axis).copied().unwrap_or(0.0));
        if d.abs() < 1e-12 {
            if o < range[0] || o > range[1] {
                return None;
            }
            continue;
        }
        let (a, c) = ((range[0] - o) / d, (range[1] - o) / d);
        let (a, c) = if a < c { (a, c) } else { (c, a) };
        if a > t0 {
            t0 = a;
            face = axis;
        }
        t1 = t1.min(c);
    }
    let s = match face {
        0 => Surface::Side,
        1 => Surface::Top,
        _ => Surface::Front,
    };
    (t0 <= t1 && t0.is_finite()).then_some((t0, s))
}
