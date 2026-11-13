//! Rigid bodies: a [`World`] of copies of one shape that fall, collide and come to rest, on a
//! floor and on static triangle meshes. Space is penumbra's: metres, z up. Rotations cross as
//! degrees about x, then y, then z, the order `geo.transform` and `geo.instance` apply them in.
//!
//! The solver is rapier3d. Everything that reaches it is checked here first: numbers must be
//! finite, triangles must name vertices that exist, counts stay under the limits below.

use rapier3d::prelude::*;

/// Most bodies a world holds.
pub const MAX_BODIES: usize = 4000;

/// Most triangles of all obstacles together.
pub const MAX_TRIANGLES: usize = 1_000_000;

/// Most points a convex hull is made from; a larger cloud is thinned evenly.
pub const MAX_HULL_POINTS: usize = 2048;

/// Most solver steps one call of [`World::step`] makes.
pub const MAX_SUBSTEPS: u32 = 8;

/// The smallest scale of a body along an axis; smaller (and zero) counts as this.
pub const MIN_SCALE: f32 = 1e-3;

/// What may change while the bodies move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Metres per second squared.
    pub gravity: [f32; 3],
    /// The height of a horizontal floor without end, or none.
    pub floor: Option<f32>,
    /// 0 slides like ice, 1 grips like rubber.
    pub friction: f32,
    /// How much of its speed a body keeps when it bounces: 0 none, 1 all.
    pub bounce: f32,
    /// How fast bodies lose speed on their own, per second.
    pub damping: f32,
    /// How fast bodies lose spin on their own, per second.
    pub spin_damping: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gravity: [0.0, 0.0, -9.81],
            floor: Some(0.0),
            friction: 0.5,
            bounce: 0.0,
            damping: 0.0,
            spin_damping: 0.0,
        }
    }
}

impl Settings {
    /// The same settings with every number finite and inside what the solver takes.
    fn checked(self) -> Self {
        let n = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { lo.max(0.0).min(hi) };
        Self {
            gravity: self.gravity.map(|g| n(g, -1e4, 1e4)),
            floor: self.floor.filter(|z| z.is_finite()),
            friction: n(self.friction, 0.0, 10.0),
            bounce: n(self.bounce, 0.0, 1.0),
            damping: n(self.damping, 0.0, 1e3),
            spin_damping: n(self.spin_damping, 0.0, 1e3),
        }
    }
}

/// The shape every body of a world collides as, at scale 1, in the body's own space.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Box {
        center: [f32; 3],
        half: [f32; 3],
    },
    Sphere {
        center: [f32; 3],
        radius: f32,
    },
    /// The convex hull of these points.
    Hull {
        points: Vec<[f32; 3]>,
    },
}

/// Where a body starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    pub translate: [f32; 3],
    pub rotate_degrees: [f32; 3],
    pub scale: [f32; 3],
    /// Metres per second.
    pub velocity: [f32; 3],
    /// Degrees per second about x, y and z.
    pub spin: [f32; 3],
}

impl Default for Start {
    fn default() -> Self {
        Self {
            translate: [0.0; 3],
            rotate_degrees: [0.0; 3],
            scale: [1.0; 3],
            velocity: [0.0; 3],
            spin: [0.0; 3],
        }
    }
}

/// Where a body is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub translate: [f64; 3],
    pub rotate_degrees: [f64; 3],
    /// As the body started; bodies do not change size.
    pub scale: [f32; 3],
}

/// A static triangle mesh in world space.
pub struct Obstacle {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

pub struct World {
    world: PhysicsWorld,
    shape: Shape,
    settings: Settings,
    bodies: Vec<(RigidBodyHandle, ColliderHandle, [f32; 3])>,
    floor: Option<ColliderHandle>,
    obstacles: Vec<ColliderHandle>,
    /// The hull per scale, since a body's scale is part of its collision shape.
    hulls: Vec<([f32; 3], SharedShape)>,
}

impl World {
    /// An empty world whose bodies will collide as `shape`.
    pub fn new(shape: Shape, settings: Settings) -> Result<Self, String> {
        let shape = checked_shape(shape)?;
        let settings = settings.checked();
        let mut world = PhysicsWorld::new();
        world.gravity = vector(settings.gravity);
        let mut out = Self {
            world,
            shape,
            settings,
            bodies: Vec::new(),
            floor: None,
            obstacles: Vec::new(),
            hulls: Vec::new(),
        };
        out.put_floor();
        Ok(out)
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// Adds a body. An error leaves the world as it was.
    pub fn add(&mut self, start: &Start) -> Result<(), String> {
        if self.bodies.len() >= MAX_BODIES {
            return Err(format!("more than {MAX_BODIES} bodies"));
        }
        let all = [start.translate, start.rotate_degrees, start.scale, start.velocity, start.spin];
        if all.iter().flatten().any(|v| !v.is_finite()) {
            return Err("a body's numbers must be finite".into());
        }
        let scale = start.scale.map(|s| s.abs().max(MIN_SCALE));
        let collider =
            self.collider(scale)?.friction(self.settings.friction).restitution(self.settings.bounce);
        let body = RigidBodyBuilder::dynamic()
            .pose(Pose::from_parts(vector(start.translate), rotation(start.rotate_degrees)))
            .linvel(vector(start.velocity))
            .angvel(vector(start.spin.map(f32::to_radians)))
            .linear_damping(self.settings.damping)
            .angular_damping(self.settings.spin_damping);
        let (b, c) = self.world.insert(body, collider);
        self.bodies.push((b, c, scale));
        Ok(())
    }

    /// Replaces every obstacle. An error leaves the world without obstacles.
    pub fn set_obstacles(&mut self, obstacles: Vec<Obstacle>) -> Result<(), String> {
        for handle in self.obstacles.drain(..) {
            self.world.remove_collider(handle);
        }
        self.world.wake_up_all(true);
        let triangles: usize = obstacles.iter().map(|o| o.triangles.len()).sum();
        if triangles > MAX_TRIANGLES {
            return Err(format!("the obstacles have {triangles} triangles, at most {MAX_TRIANGLES} work"));
        }
        for o in obstacles {
            if o.triangles.is_empty() {
                continue;
            }
            if o.vertices.iter().flatten().any(|v| !v.is_finite()) {
                return Err("an obstacle has a vertex that is not a number".into());
            }
            let count = o.vertices.len();
            if o.triangles.iter().flatten().any(|&i| i as usize >= count) {
                return Err("an obstacle has a triangle that names a vertex it does not have".into());
            }
            let vertices: Vec<Vector> = o.vertices.into_iter().map(vector).collect();
            // Fixing inner edges keeps a body that slides over a flat mesh from catching on
            // the edges between its triangles. A mesh it cannot work on collides as it is.
            let fixed = ColliderBuilder::trimesh_with_flags(
                vertices.clone(),
                o.triangles.clone(),
                TriMeshFlags::FIX_INTERNAL_EDGES,
            );
            let collider = match fixed {
                Ok(c) => c,
                Err(_) => ColliderBuilder::trimesh(vertices, o.triangles)
                    .map_err(|e| format!("an obstacle cannot be collided with: {e}"))?,
            };
            let collider = collider.friction(self.settings.friction).restitution(self.settings.bounce);
            self.obstacles.push(self.world.insert_collider(collider, None));
        }
        Ok(())
    }

    /// Takes over `settings` for the bodies that exist and those to come.
    pub fn apply(&mut self, settings: Settings) {
        let settings = settings.checked();
        if settings == self.settings {
            return;
        }
        let old = std::mem::replace(&mut self.settings, settings);
        self.world.gravity = vector(settings.gravity);
        if old.floor != settings.floor {
            self.put_floor();
        }
        let surface = old.friction != settings.friction || old.bounce != settings.bounce;
        let slowing = old.damping != settings.damping || old.spin_damping != settings.spin_damping;
        if surface {
            for (_, collider) in self.world.all_colliders_mut() {
                collider.set_friction(settings.friction);
                collider.set_restitution(settings.bounce);
            }
        }
        if slowing {
            for (_, body) in self.world.rigid_bodies_mut() {
                body.set_linear_damping(settings.damping);
                body.set_angular_damping(settings.spin_damping);
            }
        }
        // Bodies at rest sleep; a new gravity or a floor that left must reach them too.
        self.world.wake_up_all(true);
    }

    /// Moves time on by `seconds`, in `substeps` equal steps (more keeps fast or stacked
    /// bodies steadier). Nothing happens for a time that is not positive and finite.
    pub fn step(&mut self, seconds: f32, substeps: u32) {
        if !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let n = substeps.clamp(1, MAX_SUBSTEPS);
        self.world.integration_parameters.dt = seconds / n as f32;
        for _ in 0..n {
            self.world.step();
        }
    }

    /// Where every body is, in the order they were added.
    pub fn placements(&self) -> Vec<Placement> {
        self.bodies
            .iter()
            .map(|(handle, _, scale)| {
                let found = self.world.bodies.get(*handle).map(|b| (b.translation(), *b.rotation()));
                let (t, r) = found.unwrap_or((Vector::ZERO, Rotation::IDENTITY));
                let q = [r.x, r.y, r.z, r.w].map(f64::from);
                let finite = |v: f32| if v.is_finite() { f64::from(v) } else { 0.0 };
                Placement {
                    translate: [finite(t.x), finite(t.y), finite(t.z)],
                    rotate_degrees: degrees_of(q),
                    scale: *scale,
                }
            })
            .collect()
    }

    /// How many bodies do not move at the moment (the solver lets resting bodies sleep).
    pub fn asleep(&self) -> usize {
        let sleeping = |h: &RigidBodyHandle| self.world.bodies.get(*h).is_some_and(RigidBody::is_sleeping);
        self.bodies.iter().filter(|(h, _, _)| sleeping(h)).count()
    }

    fn put_floor(&mut self) {
        if let Some(handle) = self.floor.take() {
            self.world.remove_collider(handle);
        }
        if let Some(z) = self.settings.floor {
            let collider = ColliderBuilder::new(SharedShape::halfspace(Vector::Z))
                .translation(Vector::new(0.0, 0.0, z))
                .friction(self.settings.friction)
                .restitution(self.settings.bounce);
            self.floor = Some(self.world.insert_collider(collider, None));
        }
    }

    /// The collision shape at `scale`.
    fn collider(&mut self, scale: [f32; 3]) -> Result<ColliderBuilder, String> {
        let [sx, sy, sz] = scale;
        match &self.shape {
            Shape::Box { center, half } => {
                let [hx, hy, hz] = *half;
                Ok(ColliderBuilder::cuboid(hx * sx, hy * sy, hz * sz).translation(scaled(*center, scale)))
            }
            Shape::Sphere { center, radius } => {
                // A ball stays a ball: the widest scale decides.
                Ok(ColliderBuilder::ball(radius * sx.max(sy).max(sz)).translation(scaled(*center, scale)))
            }
            Shape::Hull { points } => {
                if let Some((_, shape)) = self.hulls.iter().find(|(s, _)| *s == scale) {
                    return Ok(ColliderBuilder::new(shape.clone()));
                }
                let scaled: Vec<Vector> = points.iter().map(|p| scaled(*p, scale)).collect();
                let shape = hull(&scaled)?;
                self.hulls.push((scale, shape.clone()));
                Ok(ColliderBuilder::new(shape))
            }
        }
    }
}

fn vector(v: [f32; 3]) -> Vector {
    let [x, y, z] = v;
    Vector::new(x, y, z)
}

fn scaled(p: [f32; 3], scale: [f32; 3]) -> Vector {
    let ([x, y, z], [sx, sy, sz]) = (p, scale);
    Vector::new(x * sx, y * sy, z * sz)
}

/// Degrees about x, then y, then z, as one rotation.
fn rotation(degrees: [f32; 3]) -> Rotation {
    let [x, y, z] = degrees.map(f32::to_radians);
    Rotation::from_rotation_z(z) * Rotation::from_rotation_y(y) * Rotation::from_rotation_x(x)
}

/// The degrees about x, then y, then z that make the rotation of the quaternion `[x, y, z, w]`.
/// Looking straight along the y turn (90 degrees) the x and z turns are one: z is then 0.
pub fn degrees_of(q: [f64; 4]) -> [f64; 3] {
    let len = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !len.is_finite() || len < 1e-12 {
        return [0.0; 3];
    }
    let [x, y, z, w] = q.map(|v| v / len);
    let r20 = 2.0 * (x * z - w * y);
    let ry = (-r20).clamp(-1.0, 1.0).asin();
    let (rx, rz) = if r20.abs() < 1.0 - 1e-9 {
        let r21 = 2.0 * (y * z + w * x);
        let r22 = 1.0 - 2.0 * (x * x + y * y);
        let r10 = 2.0 * (x * y + w * z);
        let r00 = 1.0 - 2.0 * (y * y + z * z);
        (r21.atan2(r22), r10.atan2(r00))
    } else {
        let r12 = 2.0 * (y * z - w * x);
        let r11 = 1.0 - 2.0 * (x * x + z * z);
        ((-r12).atan2(r11), 0.0)
    };
    [rx, ry, rz].map(f64::to_degrees)
}

/// The convex hull of `points`. An error for points that enclose no room (all in one plane, on
/// one line or in one place): such a hull weighs nothing, and a body without weight hangs in
/// the air.
fn hull(points: &[Vector]) -> Result<SharedShape, String> {
    const FLAT: &str = "the shape is flat: no convex hull encloses it";
    let shape = SharedShape::convex_hull(points).ok_or(FLAT)?;
    let (lo, hi) = points.iter().fold((Vector::MAX, Vector::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let reach = (hi - lo).length();
    let room = shape.mass_properties(1.0).mass();
    if room.is_finite() && reach.is_finite() && room > 1e-6 * reach * reach * reach {
        Ok(shape)
    } else {
        Err(FLAT.into())
    }
}

fn checked_shape(shape: Shape) -> Result<Shape, String> {
    let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
    match shape {
        Shape::Box { center, half } => {
            if !finite(&center) || !finite(&half) {
                return Err("the box's numbers must be finite".into());
            }
            Ok(Shape::Box { center, half: half.map(|h| h.abs().max(MIN_SCALE * 0.5)) })
        }
        Shape::Sphere { center, radius } => {
            if !finite(&center) || !radius.is_finite() {
                return Err("the sphere's numbers must be finite".into());
            }
            Ok(Shape::Sphere { center, radius: radius.abs().max(MIN_SCALE * 0.5) })
        }
        Shape::Hull { points } => {
            if points.iter().any(|p| !finite(p)) {
                return Err("the shape has a point that is not a number".into());
            }
            if points.len() < 4 {
                return Err("a convex hull needs at least four points".into());
            }
            let stride = points.len().div_ceil(MAX_HULL_POINTS).max(1);
            let points: Vec<[f32; 3]> = points.into_iter().step_by(stride).collect();
            // Tried once here, so a shape no hull encloses fails before any body is added.
            let cloud: Vec<Vector> = points.iter().copied().map(vector).collect();
            hull(&cloud)?;
            Ok(Shape::Hull { points })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn unit_box() -> Shape {
        Shape::Box { center: [0.0; 3], half: [0.5; 3] }
    }

    fn at(z: f32) -> Start {
        Start { translate: [0.0, 0.0, z], ..Start::default() }
    }

    fn run(world: &mut World, seconds: f32) {
        for _ in 0..(seconds / DT) as usize {
            world.step(DT, 1);
        }
    }

    #[test]
    fn a_box_falls_onto_the_floor_and_rests_on_it() {
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        world.add(&at(3.0)).unwrap();
        world.step(DT, 1);
        let early = world.placements()[0].translate[2];
        assert!(early < 3.0 && early > 2.9, "one step in: {early}");
        run(&mut world, 4.0);
        let p = world.placements()[0];
        assert!((p.translate[2] - 0.5).abs() < 0.02, "rests at {}", p.translate[2]);
        assert_eq!(world.asleep(), 1);
    }

    #[test]
    fn without_a_floor_a_body_keeps_falling() {
        let mut world = World::new(unit_box(), Settings { floor: None, ..Settings::default() }).unwrap();
        world.add(&at(3.0)).unwrap();
        run(&mut world, 2.0);
        // 3 - 9.81 * 2^2 / 2 = -16.6
        let z = world.placements()[0].translate[2];
        assert!((z + 16.6).abs() < 0.5, "{z}");
    }

    #[test]
    fn a_ball_with_bounce_comes_back_up_and_one_without_stays_down() {
        let ball = Shape::Sphere { center: [0.0; 3], radius: 0.5 };
        let top_after_landing = |bounce: f32| {
            let mut world = World::new(ball.clone(), Settings { bounce, ..Settings::default() }).unwrap();
            world.add(&at(3.0)).unwrap();
            let mut landed = false;
            let mut top = 0.0f64;
            for _ in 0..240 {
                world.step(DT, 1);
                let z = world.placements()[0].translate[2];
                if z < 0.6 {
                    landed = true;
                } else if landed {
                    top = top.max(z);
                }
            }
            assert!(landed);
            top
        };
        assert!(top_after_landing(0.8) > 1.5);
        assert_eq!(top_after_landing(0.0), 0.0);
    }

    #[test]
    fn a_body_lands_on_a_mesh_and_falls_on_when_the_mesh_goes() {
        let mut world = World::new(unit_box(), Settings { floor: None, ..Settings::default() }).unwrap();
        let table = || Obstacle {
            vertices: vec![[-2.0, -2.0, 1.0], [2.0, -2.0, 1.0], [2.0, 2.0, 1.0], [-2.0, 2.0, 1.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        world.set_obstacles(vec![table()]).unwrap();
        world.add(&at(3.0)).unwrap();
        run(&mut world, 4.0);
        let z = world.placements()[0].translate[2];
        assert!((z - 1.5).abs() < 0.03, "rests at {z}");
        world.set_obstacles(Vec::new()).unwrap();
        run(&mut world, 1.0);
        assert!(world.placements()[0].translate[2] < 0.0);
    }

    #[test]
    fn a_hull_stands_on_its_lowest_points_and_scale_sizes_it() {
        // A pyramid: base 2 x 2 at z = 0, tip at z = 1.
        let points =
            vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut world = World::new(Shape::Hull { points }, Settings::default()).unwrap();
        world.add(&at(2.0)).unwrap();
        world
            .add(&Start {
                translate: [10.0, 0.0, 2.0],
                rotate_degrees: [180.0, 0.0, 0.0],
                ..Start::default()
            })
            .unwrap();
        run(&mut world, 5.0);
        let p = world.placements();
        assert!(p[0].translate[2].abs() < 0.03, "the base is on the floor: {}", p[0].translate[2]);
        // Dropped tip first it cannot stay: it ends on a side or its base, origin above the floor
        // by less than the pyramid is wide.
        assert!(p[1].translate[2] > -0.03 && p[1].translate[2] < 1.5, "{}", p[1].translate[2]);
        let big = [2.0; 3];
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        world.add(&Start { translate: [0.0, 0.0, 4.0], scale: big, ..Start::default() }).unwrap();
        run(&mut world, 4.0);
        let p = world.placements()[0];
        assert!((p.translate[2] - 1.0).abs() < 0.03, "{}", p.translate[2]);
        assert_eq!(p.scale, big);
    }

    #[test]
    fn settings_reach_bodies_that_rest() {
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        world.add(&at(1.0)).unwrap();
        run(&mut world, 3.0);
        assert_eq!(world.asleep(), 1);
        world.apply(Settings { gravity: [0.0, 0.0, 9.81], ..Settings::default() });
        run(&mut world, 1.0);
        assert!(world.placements()[0].translate[2] > 2.0);
    }

    #[test]
    fn bad_input_is_an_error_and_never_reaches_the_solver() {
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        assert!(world.add(&at(f32::NAN)).is_err());
        assert!(world.add(&Start { velocity: [f32::INFINITY, 0.0, 0.0], ..Start::default() }).is_err());
        assert!(world.is_empty());
        let bad_index = Obstacle { vertices: vec![[0.0; 3]; 3], triangles: vec![[0, 1, 7]] };
        assert!(world.set_obstacles(vec![bad_index]).is_err());
        let bad_vertex = Obstacle { vertices: vec![[f32::NAN; 3]; 3], triangles: vec![[0, 1, 2]] };
        assert!(world.set_obstacles(vec![bad_vertex]).is_err());
        assert!(World::new(Shape::Hull { points: vec![[0.0; 3]; 8] }, Settings::default()).is_err());
        assert!(World::new(Shape::Hull { points: vec![[1.0, 0.0, 0.0]] }, Settings::default()).is_err());
        // Settings that are not numbers fall back instead of failing.
        let odd = Settings { gravity: [f32::NAN; 3], friction: -3.0, bounce: 9.0, ..Settings::default() };
        let mut world = World::new(unit_box(), odd).unwrap();
        world.add(&at(1.0)).unwrap();
        world.step(f32::NAN, 1);
        world.step(-1.0, 1);
        world.step(DT, 100);
        assert!(world.placements()[0].translate.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_world_is_full_at_the_limit() {
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        for k in 0..MAX_BODIES {
            world.add(&at(k as f32 * 2.0)).unwrap();
        }
        assert!(world.add(&at(0.0)).is_err());
        assert_eq!(world.len(), MAX_BODIES);
    }

    /// The matrix of degrees about x, then y, then z.
    fn matrix(degrees: [f64; 3]) -> [[f64; 3]; 3] {
        let [x, y, z] = degrees.map(f64::to_radians);
        let ((sx, cx), (sy, cy), (sz, cz)) = (x.sin_cos(), y.sin_cos(), z.sin_cos());
        [
            [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
            [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
            [-sy, cy * sx, cy * cx],
        ]
    }

    #[test]
    fn degrees_go_in_and_come_out_as_the_same_rotation() {
        let cases = [
            [0.0, 0.0, 0.0],
            [30.0, 0.0, 0.0],
            [0.0, 45.0, 0.0],
            [0.0, 0.0, -120.0],
            [20.0, -35.0, 70.0],
            [170.0, 80.0, -160.0],
            [10.0, 90.0, 25.0],
            [-40.0, -90.0, 5.0],
        ];
        for degrees in cases {
            let r = rotation(degrees.map(|d| d as f32));
            let back = degrees_of([r.x, r.y, r.z, r.w].map(f64::from));
            let (a, b) = (matrix(degrees), matrix(back));
            for (ra, rb) in a.iter().zip(&b) {
                for (va, vb) in ra.iter().zip(rb) {
                    assert!((va - vb).abs() < 2e-3, "{degrees:?} came back as {back:?}");
                }
            }
        }
        assert_eq!(degrees_of([0.0; 4]), [0.0; 3]);
        assert_eq!(degrees_of([f64::NAN, 0.0, 0.0, 1.0]), [0.0; 3]);
    }

    #[test]
    fn a_resting_body_keeps_its_place_to_the_digit() {
        let mut world = World::new(unit_box(), Settings::default()).unwrap();
        world.add(&at(0.6)).unwrap();
        run(&mut world, 3.0);
        let a = world.placements();
        run(&mut world, 1.0);
        assert_eq!(a, world.placements());
    }
}
