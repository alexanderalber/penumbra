//! A ray-cast room for tests and demonstrations of [`crate::scene`]: a floor, a wall, a box
//! standing on the floor with a ball on its top, a camera above looking down at it and
//! projectors beside the camera. Its maps are what `scan.decode` writes for such a room.
//!
//! [`Room::pumpkin`] is a landscape of crates with a ribbed pumpkin on one of them instead of
//! the ball, with decode noise like a real scan's.
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
    Pumpkin,
}

/// A pumpkin: an oblate body whose horizontal radius dips into a furrow between each two ribs,
/// its top sunk a little round the stalk (the stalk itself is left out: a scan does not see
/// it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pumpkin {
    pub centre: [f64; 3],
    /// Horizontal radius over a rib's crest.
    pub radius: f64,
    /// Half the height.
    pub half_height: f64,
    pub ribs: u32,
    /// How deep a furrow cuts in, as a share of the radius.
    pub furrow: f64,
    /// How far the top sinks round the stalk, as a share of the half height.
    pub dimple: f64,
    /// Turn about the vertical axis, radians.
    pub turn: f64,
}

impl Pumpkin {
    /// A pumpkin 29 cm across and 21 cm high with ten ribs and furrows 1 cm deep, standing on
    /// the plane `y = floor` at `x`, `z`.
    pub fn on(x: f64, floor: f64, z: f64) -> Pumpkin {
        let half_height = 1.05;
        Pumpkin {
            centre: [x, floor + half_height * 0.97, z],
            radius: 1.45,
            half_height,
            ribs: 10,
            furrow: 0.07,
            dimple: 0.18,
            turn: 0.2,
        }
    }

    /// The horizontal radius at azimuth `phi` as a share of [`Pumpkin::radius`]: 1 over a rib,
    /// `1 - furrow` at the bottom of a furrow; a raised cosine to the fourth power, so the
    /// furrows are narrow and the ribs broad.
    pub fn rib(&self, phi: f64) -> f64 {
        let c = 0.5 + 0.5 * (f64::from(self.ribs) * (phi - self.turn)).cos();
        1.0 - self.furrow * c.powi(4)
    }

    /// Negative inside, zero on the skin, positive outside (not a distance).
    pub fn field(&self, p: V) -> f64 {
        let q = sub(p, self.centre);
        let rho = q[0].hypot(q[2]);
        let r = (self.radius * self.rib(q[2].atan2(q[0]))).max(1e-9);
        let mut b = self.half_height;
        if q[1] > 0.0 {
            b *= 1.0 - self.dimple * (-(rho / (0.35 * self.radius)).powi(2)).exp();
        }
        (rho / r).powi(2) + (q[1] / b.max(1e-9)).powi(2) - 1.0
    }

    /// The outward normal at (or near) the skin.
    pub fn normal(&self, p: V) -> V {
        let e = 1e-4;
        let g = |k: usize| {
            let (mut a, mut b) = (p, p);
            if let (Some(x), Some(y)) = (a.get_mut(k), b.get_mut(k)) {
                *x += e;
                *y -= e;
            }
            self.field(a) - self.field(b)
        };
        unit([g(0), g(1), g(2)])
    }

    /// Where the ray from `o` along `d` first meets the skin (in units of `d`): marched in
    /// steps of a hundredth of the bounding sphere's radius, then bisected.
    pub fn enter(&self, o: V, d: V) -> Option<f64> {
        let bound = self.radius.max(self.half_height) * 1.02;
        let oc = sub(o, self.centre);
        let (a, b, c) = (dot(d, d), 2.0 * dot(oc, d), dot(oc, oc) - bound * bound);
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 || a <= 0.0 {
            return None;
        }
        let (t0, t1) = (((-b - disc.sqrt()) / (2.0 * a)).max(0.0), (-b + disc.sqrt()) / (2.0 * a));
        if t1 <= t0 {
            return None;
        }
        let step = bound / 100.0 / a.sqrt();
        let inside = |t: f64| self.field(add(o, mul(d, t))) < 0.0;
        let (mut prev, mut t) = (t0, t0);
        while t < t1 {
            t = (t + step).min(t1);
            if inside(t) {
                let (mut lo, mut hi) = (prev, t);
                for _ in 0..40 {
                    let mid = 0.5 * (lo + hi);
                    if inside(mid) {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                return Some(hi);
            }
            prev = t;
        }
        None
    }
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
    /// Centre and radius; no ball with a radius of 0.
    pub ball: [f64; 4],
    pub pumpkin: Option<Pumpkin>,
    /// Uniform noise on the camera positions, up to this many camera pixels either way.
    pub noise: f64,
    /// Gaussian noise on the camera positions (standard deviation per axis, camera pixels) and
    /// on the projector positions of the camera map (projector pixels).
    pub sigma: f64,
    /// The share of measured pixels that land far off (up to 8 camera pixels either way), as
    /// a wrongly decoded bit leaves them.
    pub outliers: f64,
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
            pumpkin: None,
            noise: 0.25,
            sigma: 0.0,
            outliers: 0.0,
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

    /// A landscape of five Euro crates (two side by side, one stacked on the left one, a low
    /// one in front, one standing on its end at the right) with a ribbed pumpkin on the right
    /// crate, seen by a 16:9 camera and two 16:9 projectors of `scale` times 480 x 270 pixels,
    /// with decode noise like a real scan's.
    pub fn pumpkin(scale: f64) -> Room {
        let scale = if scale.is_finite() { scale.clamp(0.25, 8.0) } else { 1.0 };
        let size =
            |w: f64, h: f64| ((w * scale).round().max(8.0) as usize, (h * scale).round().max(8.0) as usize);
        Room {
            camera: size(480.0, 270.0),
            focal: 420.0 * scale,
            projector: size(480.0, 270.0),
            projector_focal: 400.0 * scale,
            eye: [0.0, 11.0, -1.0],
            target: [0.5, 2.6, 13.0],
            offsets: vec![-4.0, 4.0],
            boxes: vec![
                // Left and right on the floor with a gap: their tops and fronts coplanar.
                [[-6.4, -0.4], [0.0, 3.1], [11.0, 15.0]],
                [[0.0, 6.0], [0.0, 3.1], [11.0, 15.0]],
                // Stacked on the left one's back.
                [[-6.4, -0.4], [3.1, 6.2], [12.6, 16.6]],
                // A low one (12 cm) in front of the gap.
                [[-3.4, 2.6], [0.0, 1.2], [6.8, 10.8]],
                // On its end right of the right one: 31 wide, 40 high, 60 deep.
                [[6.4, 9.5], [0.0, 4.0], [10.0, 16.0]],
            ],
            ball: [0.0; 4],
            pumpkin: Some(Pumpkin::on(3.2, 3.1, 13.3)),
            noise: 0.0,
            sigma: 0.15 * scale,
            outliers: 0.002,
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
        if r > 0.0 && disc >= 0.0 && a > 0.0 {
            take((-b - disc.sqrt()) / (2.0 * a), Surface::Ball);
        }
        if let Some(t) = self.pumpkin.and_then(|p| p.enter(o, d)) {
            take(t, Surface::Pumpkin);
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
        let pixels = rows(ph, |v| {
            (0..pw)
                .map(|u| {
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
                            let [dx, dy] = self.error((v * pw + u) as u64);
                            let measured = if (u + v) % 3 == 0 { 0.5 } else { 1.0 };
                            let (x, y) = ((c[0] + dx) as f32, (c[1] + dy) as f32);
                            let px = [(x / cw).clamp(0.0, 1.0), (y / ch).clamp(0.0, 1.0), measured, 1.0];
                            (px, Some((s, point)))
                        }
                        None => ([-1.0, -1.0, 0.0, 1.0], None),
                    }
                })
                .collect()
        });
        let (rgba, truth) = pixels.into_iter().unzip();
        (Map { width: pw, height: ph, rgba }, truth)
    }

    /// The decode error of pixel `i`: uniform up to `noise`, Gaussian of `sigma`, and for a
    /// share `outliers` of the pixels a jump of up to 8 pixels.
    fn error(&self, i: u64) -> [f64; 2] {
        // The default room's uniform noise draws from `hash(2 i)` and `hash(2 i + 1)` as it
        // always did, so its tests see the same numbers.
        let uniform = |j: u64| (hash(i.wrapping_mul(2) + j) * 2.0 - 1.0) * self.noise;
        let k = i.wrapping_mul(8) ^ 0x5555_0000_0000_0000;
        let gauss = |j: u64| {
            if self.sigma <= 0.0 {
                return 0.0;
            }
            let (a, b) = (hash(k + 2 * j).max(1e-12), hash(k + 2 * j + 1));
            (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos() * self.sigma
        };
        let jump = |j: u64| {
            if self.outliers > 0.0 && hash(k + 4) < self.outliers {
                (hash(k + 5 + j) * 2.0 - 1.0) * 8.0
            } else {
                0.0
            }
        };
        [uniform(0) + gauss(0) + jump(0), uniform(1) + gauss(1) + jump(1)]
    }

    /// Projector `p`'s `cam_to_proj` map (with the Gaussian part of the noise, in projector
    /// pixels).
    pub fn from_camera(&self, p: usize) -> Map {
        let (proj, cam) = (self.projector_pinhole(p), self.camera_pinhole());
        let (cw, ch) = self.camera;
        let (pw, ph) = (self.projector.0 as f32, self.projector.1 as f32);
        let quiet = Room { noise: 0.0, outliers: 0.0, ..self.clone() };
        let salt = (p as u64 + 1) << 40;
        let rgba = rows(ch, |y| {
            (0..cw)
                .map(|x| {
                    let d = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                    let lit = self.hit(cam.at, d).and_then(|(t, _)| {
                        let point = add(cam.at, mul(d, t));
                        if !self.visible(proj.at, point) {
                            return None;
                        }
                        proj.project(point)
                    });
                    match lit {
                        Some(q) => {
                            let [dx, dy] = quiet.error(salt + (y * cw + x) as u64);
                            let u = ((q[0] + dx) as f32 / pw).clamp(0.0, 1.0);
                            let v = ((q[1] + dy) as f32 / ph).clamp(0.0, 1.0);
                            [u, v, 1.0, 1.0]
                        }
                        None => [-1.0, -1.0, 0.0, 1.0],
                    }
                })
                .collect()
        });
        Map { width: cw, height: ch, rgba }
    }

    /// Per camera pixel what it sees.
    pub fn camera_truth(&self) -> Vec<Option<(Surface, V)>> {
        let cam = self.camera_pinhole();
        let (cw, ch) = self.camera;
        rows(ch, |y| {
            (0..cw)
                .map(|x| {
                    let d = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                    self.hit(cam.at, d).map(|(t, s)| (s, add(cam.at, mul(d, t))))
                })
                .collect()
        })
    }

    /// The normal of surface `s` at `p`, on the side facing `from`.
    fn normal(&self, s: Surface, p: V, from: V) -> V {
        let n = match s {
            Surface::Floor | Surface::Top => [0.0, 1.0, 0.0],
            Surface::Wall | Surface::Front => [0.0, 0.0, -1.0],
            Surface::Side => [1.0, 0.0, 0.0],
            Surface::Ball => unit(sub(p, [self.ball[0], self.ball[1], self.ball[2]])),
            Surface::Pumpkin => self.pumpkin.map_or([0.0, 1.0, 0.0], |k| k.normal(p)),
        };
        if dot(n, sub(from, p)) < 0.0 { mul(n, -1.0) } else { n }
    }

    /// The camera's picture of the room lit white by projector `p` alone (linear RGBA, camera
    /// size): light wood crates, a grey floor and wall, an orange pumpkin; diffuse, falling off
    /// with the square of the distance (1 at the target), black where the projector does not
    /// reach.
    pub fn photo(&self, p: usize) -> Vec<[f32; 4]> {
        let (proj, cam) = (self.projector_pinhole(p), self.camera_pinhole());
        let (cw, ch) = self.camera;
        let reach = dot(sub(self.target, proj.at), sub(self.target, proj.at));
        rows(ch, |y| {
            (0..cw)
                .map(|x| {
                    let d = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                    let Some((t, s)) = self.hit(cam.at, d) else { return [0.0, 0.0, 0.0, 1.0] };
                    let point = add(cam.at, mul(d, t));
                    if !self.visible(proj.at, point) || proj.project(point).is_none() {
                        return [0.0, 0.0, 0.0, 1.0];
                    }
                    let to = sub(proj.at, point);
                    let lambert = dot(self.normal(s, point, cam.at), unit(to)).max(0.0);
                    let light = lambert * reach / dot(to, to).max(1e-9);
                    let a = albedo(s);
                    [(a[0] * light) as f32, (a[1] * light) as f32, (a[2] * light) as f32, 1.0]
                })
                .collect()
        })
    }

    /// Where the camera sees a point (camera pixels), if in its picture.
    pub fn camera_point(&self, p: V) -> Option<[f64; 2]> {
        self.camera_pinhole().project(p)
    }

    /// Where projector `p` stands.
    pub fn projector_at(&self, p: usize) -> V {
        self.projector_pinhole(p).at
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

/// A surface's colour (linear).
fn albedo(s: Surface) -> [f64; 3] {
    match s {
        Surface::Floor => [0.25, 0.25, 0.26],
        Surface::Wall => [0.55, 0.56, 0.6],
        Surface::Top | Surface::Front | Surface::Side => [0.7, 0.52, 0.32],
        Surface::Ball => [0.8, 0.8, 0.8],
        Surface::Pumpkin => [0.85, 0.36, 0.06],
    }
}

/// `row(y)` for every `y` below `count`, on as many threads as there are cores, concatenated.
fn rows<T: Send>(count: usize, row: impl Fn(usize) -> Vec<T> + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, count.max(1));
    let chunk = count.div_ceil(threads).max(1);
    let run = |start: usize| (start..(start + chunk).min(count)).flat_map(&row).collect::<Vec<T>>();
    std::thread::scope(|scope| {
        let handles: Vec<_> =
            (0..count).step_by(chunk).map(|start| (start, scope.spawn(move || run(start)))).collect();
        // A thread that failed has its rows done again here, so the picture keeps its size.
        handles.into_iter().flat_map(|(start, h)| h.join().unwrap_or_else(|_| run(start))).collect()
    })
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
