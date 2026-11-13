//! Rigid bodies: a [`World`] of groups of bodies that fall, collide and come to rest, on a floor
//! and on static triangle meshes. A group is copies of one [`Shape`] with one [`Material`];
//! bodies of every group collide with each other. Space is penumbra's: metres, z up. Rotations
//! cross as degrees about x, then y, then z, the order `geo.transform` and `geo.instance` apply
//! them in.
//!
//! The solver is rapier3d. Everything that reaches it is checked here first: numbers must be
//! finite, triangles must name vertices that exist, counts stay under the limits below.

use rapier3d::prelude::*;

/// Most bodies a world holds, over all its groups.
pub const MAX_BODIES: usize = 4000;

/// Most groups a world holds; a group is named by a number below this.
pub const MAX_GROUPS: usize = 64;

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
    /// Of the floor and the obstacles: 0 slides like ice, 1 grips like rubber. Where a body
    /// touches them, the mean of this and the body's own friction counts.
    pub friction: f32,
    /// Of the floor and the obstacles: how much of its speed a body keeps when it bounces off
    /// them, 0 none, 1 all. The larger of this and the body's own bounce counts.
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

/// What the bodies of a group are made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// 0 slides like ice, 1 grips like rubber. Where two surfaces touch, the mean of both counts.
    pub friction: f32,
    /// How much of its speed a body keeps when it bounces: 0 none, 1 all. Where two surfaces
    /// touch, the larger of both counts.
    pub bounce: f32,
    /// Weight per volume. Only the ratio between groups shows: the heavier body pushes the
    /// lighter one aside.
    pub density: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self { friction: 0.5, bounce: 0.0, density: 1.0 }
    }
}

/// The lightest and the heaviest a material may be.
pub const DENSITY_RANGE: [f32; 2] = [1e-3, 1e4];

impl Material {
    /// The same material with every number finite and inside what the solver takes.
    fn checked(self) -> Self {
        let n = |v: f32, lo: f32, hi: f32, or: f32| if v.is_finite() { v.clamp(lo, hi) } else { or };
        let [light, heavy] = DENSITY_RANGE;
        Self {
            friction: n(self.friction, 0.0, 10.0, 0.0),
            bounce: n(self.bounce, 0.0, 1.0, 0.0),
            density: n(self.density, light, heavy, 1.0),
        }
    }
}

/// The shape every body of a group collides as, at scale 1, in the body's own space.
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

/// One body of a group.
struct Body {
    body: RigidBodyHandle,
    collider: ColliderHandle,
    scale: [f32; 3],
}

/// Copies of one shape with one material.
struct Group {
    shape: Shape,
    material: Material,
    bodies: Vec<Body>,
    /// The hull per scale, since a body's scale is part of its collision shape.
    hulls: Vec<([f32; 3], SharedShape)>,
}

pub struct World {
    world: PhysicsWorld,
    settings: Settings,
    /// By the group's number; a number without a group holds none.
    groups: Vec<Option<Group>>,
    /// Bodies of all groups together.
    count: usize,
    floor: Option<ColliderHandle>,
    obstacles: Vec<ColliderHandle>,
}

impl World {
    /// A world without bodies.
    pub fn new(settings: Settings) -> Self {
        let settings = settings.checked();
        let mut world = PhysicsWorld::new();
        world.gravity = vector(settings.gravity);
        let mut out =
            Self { world, settings, groups: Vec::new(), count: 0, floor: None, obstacles: Vec::new() };
        out.put_floor();
        out
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Bodies of all groups together.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn has_group(&self, group: usize) -> bool {
        self.group(group).is_some()
    }

    /// Bodies of one group; 0 for a group that does not exist.
    pub fn group_len(&self, group: usize) -> usize {
        self.group(group).map_or(0, |g| g.bodies.len())
    }

    /// Makes `group` an empty group of bodies that collide as `shape` and are made of
    /// `material`. Bodies the group held leave the world. An error leaves the world as it was.
    pub fn set_group(&mut self, group: usize, shape: Shape, material: Material) -> Result<(), String> {
        if group >= MAX_GROUPS {
            return Err(format!("a world holds {MAX_GROUPS} groups at most"));
        }
        let shape = check(shape)?;
        self.remove_group(group);
        if self.groups.len() <= group {
            self.groups.resize_with(group + 1, || None);
        }
        if let Some(slot) = self.groups.get_mut(group) {
            *slot =
                Some(Group { shape, material: material.checked(), bodies: Vec::new(), hulls: Vec::new() });
        }
        Ok(())
    }

    /// Takes `group` and its bodies out of the world. Bodies that rested on them fall on.
    pub fn remove_group(&mut self, group: usize) {
        let Some(old) = self.groups.get_mut(group).and_then(Option::take) else { return };
        self.count = self.count.saturating_sub(old.bodies.len());
        for body in &old.bodies {
            self.world.remove_body(body.body);
        }
        if !old.bodies.is_empty() {
            self.world.wake_up_all(true);
        }
    }

    /// Takes over `material` for the bodies of `group` that exist and those to come.
    pub fn set_material(&mut self, group: usize, material: Material) {
        let material = material.checked();
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else { return };
        if g.material == material {
            return;
        }
        g.material = material;
        for body in &g.bodies {
            if let Some(collider) = self.world.colliders.get_mut(body.collider) {
                collider.set_friction(material.friction);
                collider.set_restitution(material.bounce);
                collider.set_density(material.density);
                refresh(collider);
            }
        }
        // A body at rest on a slope must slide when it loses its grip.
        self.world.wake_up_all(true);
    }

    /// Adds a body to `group`. An error leaves the world as it was.
    pub fn add(&mut self, group: usize, start: &Start) -> Result<(), String> {
        if self.count >= MAX_BODIES {
            return Err(format!("more than {MAX_BODIES} bodies"));
        }
        let all = [start.translate, start.rotate_degrees, start.scale, start.velocity, start.spin];
        if all.iter().flatten().any(|v| !v.is_finite()) {
            return Err("a body's numbers must be finite".into());
        }
        let Some(g) = self.groups.get_mut(group).and_then(Option::as_mut) else {
            return Err(format!("there is no group {group}"));
        };
        let scale = start.scale.map(|s| s.abs().max(MIN_SCALE));
        let collider = g
            .collider(scale)?
            .friction(g.material.friction)
            .restitution(g.material.bounce)
            .restitution_combine_rule(CoefficientCombineRule::Max)
            .density(g.material.density);
        let body = RigidBodyBuilder::dynamic()
            .pose(Pose::from_parts(vector(start.translate), rotation(start.rotate_degrees)))
            .linvel(vector(start.velocity))
            .angvel(vector(start.spin.map(f32::to_radians)))
            .linear_damping(self.settings.damping)
            .angular_damping(self.settings.spin_damping);
        let (body, collider) = self.world.insert(body, collider);
        g.bodies.push(Body { body, collider, scale });
        self.count += 1;
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
            let collider = self.ground(collider);
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
        if old.friction != settings.friction || old.bounce != settings.bounce {
            for handle in self.floor.iter().chain(&self.obstacles) {
                if let Some(collider) = self.world.colliders.get_mut(*handle) {
                    collider.set_friction(settings.friction);
                    collider.set_restitution(settings.bounce);
                    refresh(collider);
                }
            }
        }
        if old.damping != settings.damping || old.spin_damping != settings.spin_damping {
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

    /// Where every body of `group` is, in the order they were added.
    pub fn placements(&self, group: usize) -> Vec<Placement> {
        let Some(g) = self.group(group) else { return Vec::new() };
        g.bodies
            .iter()
            .map(|b| {
                let found = self.world.bodies.get(b.body).map(|r| (r.translation(), *r.rotation()));
                let (t, r) = found.unwrap_or((Vector::ZERO, Rotation::IDENTITY));
                let q = [r.x, r.y, r.z, r.w].map(f64::from);
                let finite = |v: f32| if v.is_finite() { f64::from(v) } else { 0.0 };
                Placement {
                    translate: [finite(t.x), finite(t.y), finite(t.z)],
                    rotate_degrees: degrees_of(q),
                    scale: b.scale,
                }
            })
            .collect()
    }

    /// How many bodies do not move at the moment (the solver lets resting bodies sleep).
    pub fn asleep(&self) -> usize {
        let sleeping = |b: &&Body| self.world.bodies.get(b.body).is_some_and(RigidBody::is_sleeping);
        self.groups.iter().flatten().flat_map(|g| &g.bodies).filter(sleeping).count()
    }

    fn group(&self, group: usize) -> Option<&Group> {
        self.groups.get(group).and_then(Option::as_ref)
    }

    /// `collider` with the surface of the floor and the obstacles.
    fn ground(&self, collider: ColliderBuilder) -> ColliderBuilder {
        collider
            .friction(self.settings.friction)
            .restitution(self.settings.bounce)
            .restitution_combine_rule(CoefficientCombineRule::Max)
    }

    fn put_floor(&mut self) {
        if let Some(handle) = self.floor.take() {
            self.world.remove_collider(handle);
        }
        if let Some(z) = self.settings.floor {
            let collider = self
                .ground(ColliderBuilder::new(SharedShape::halfspace(Vector::Z)))
                .translation(Vector::new(0.0, 0.0, z));
            self.floor = Some(self.world.insert_collider(collider, None));
        }
    }
}

impl Group {
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

/// Makes the solver work out the contacts of `collider` anew. While two bodies barely move
/// against each other it keeps their contact as it was, friction and bounce included, so a new
/// surface would not reach a body at rest.
fn refresh(collider: &mut Collider) {
    let shape = collider.shared_shape().clone();
    collider.set_shape(shape);
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

/// `shape` as a world takes it: sizes away from zero, a hull thinned to [`MAX_HULL_POINTS`].
/// An error for numbers that are not finite and for a hull that encloses no room.
pub fn check(shape: Shape) -> Result<Shape, String> {
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

    fn ball() -> Shape {
        Shape::Sphere { center: [0.0; 3], radius: 0.5 }
    }

    /// A world whose group 0 collides as `shape`, of the default material.
    fn world_of(shape: Shape, settings: Settings) -> World {
        let mut world = World::new(settings);
        world.set_group(0, shape, Material::default()).unwrap();
        world
    }

    #[test]
    fn a_ball_and_a_box_of_two_groups_collide() {
        let mut world = World::new(Settings::default());
        world.set_group(0, unit_box(), Material::default()).unwrap();
        world.set_group(3, ball(), Material::default()).unwrap();
        assert!(world.has_group(0) && world.has_group(3) && !world.has_group(1));
        world.add(0, &at(0.5)).unwrap();
        world.add(3, &at(4.0)).unwrap();
        // The ball lands on the box, a little off its middle, and may roll off: it never
        // passes through. On the box its centre is at 1.5, on the floor beside it at 0.5.
        let mut lowest = f64::MAX;
        for _ in 0..360 {
            world.step(DT, 1);
            let (b, s) = (world.placements(0)[0].translate, world.placements(3)[0].translate);
            let apart = (0..3).map(|k| (b[k] - s[k]).powi(2)).sum::<f64>().sqrt();
            lowest = lowest.min(apart);
        }
        assert!(lowest > 0.95, "the ball came as near as {lowest} to the middle of the box");
        assert!((world.placements(3)[0].translate[2] - 1.5).abs() < 0.03, "it rests on the box");
        assert_eq!((world.len(), world.group_len(0), world.group_len(3), world.group_len(9)), (2, 1, 1, 0));
        // A group that leaves takes its bodies along: the ball falls on to the floor.
        world.remove_group(0);
        assert_eq!((world.len(), world.placements(0).len()), (1, 0));
        run(&mut world, 3.0);
        assert!((world.placements(3)[0].translate[2] - 0.5).abs() < 0.03);
        // A group set again starts empty.
        world.add(3, &at(9.0)).unwrap();
        world.set_group(3, unit_box(), Material::default()).unwrap();
        assert!(world.is_empty());
    }

    /// How far a box slides in two seconds down a slab tilted by 30 degrees.
    fn slide(material: Material, change: Option<Material>) -> f64 {
        let mut world = World::new(Settings { floor: None, ..Settings::default() });
        let (s, c) = 30f32.to_radians().sin_cos();
        // A slab through the origin, falling towards +x.
        let corner = |x: f32, y: f32| [x * c, y, -x * s];
        world
            .set_obstacles(vec![Obstacle {
                vertices: vec![corner(-5.0, -5.0), corner(40.0, -5.0), corner(40.0, 5.0), corner(-5.0, 5.0)],
                triangles: vec![[0, 1, 2], [0, 2, 3]],
            }])
            .unwrap();
        world.set_group(0, unit_box(), material).unwrap();
        let start = Start {
            translate: [0.5 * s, 0.0, 0.5 * c + 0.01],
            rotate_degrees: [0.0, 30.0, 0.0],
            ..Start::default()
        };
        world.add(0, &start).unwrap();
        run(&mut world, 1.0);
        if let Some(m) = change {
            world.set_material(0, m);
        }
        run(&mut world, 1.0);
        world.placements(0)[0].translate[0] - f64::from(start.translate[0])
    }

    #[test]
    fn each_group_has_its_own_grip() {
        // The slab has friction 0.5; with the body's own the mean is 0.25 for ice and 1.25 for
        // rubber. tan(30 degrees) is 0.58: ice slides, rubber holds.
        let ice = Material { friction: 0.0, ..Material::default() };
        let rubber = Material { friction: 2.0, ..Material::default() };
        let (slid, held) = (slide(ice, None), slide(rubber, None));
        assert!(slid > 2.0, "ice slid {slid} m");
        assert!(held.abs() < 0.05, "rubber slid {held} m");
        // A material set while the body rests reaches it.
        let late = slide(rubber, Some(ice));
        assert!(late > 0.5 && late < slid, "rubber turned ice slid {late} m");
    }

    #[test]
    fn a_new_surface_of_the_ground_reaches_a_body_that_rests() {
        let mut world = world_of(unit_box(), Settings { gravity: [3.0, 0.0, -9.81], ..Settings::default() });
        world.add(0, &at(0.5)).unwrap();
        // Pulled sideways by 3 against a grip of 0.5 times 9.81: it holds.
        run(&mut world, 2.0);
        assert!(world.placements(0)[0].translate[0].abs() < 0.02);
        // The mean of the body's 0.5 and an icy floor is 0.25: now it slides.
        world.apply(Settings { gravity: [3.0, 0.0, -9.81], friction: 0.0, ..Settings::default() });
        run(&mut world, 2.0);
        assert!(world.placements(0)[0].translate[0] > 0.5, "{:?}", world.placements(0)[0].translate);
    }

    #[test]
    fn the_bouncier_of_two_surfaces_decides() {
        let top = |ball_bounce: f32, floor_bounce: f32| {
            let mut world = World::new(Settings { bounce: floor_bounce, ..Settings::default() });
            world.set_group(0, ball(), Material { bounce: ball_bounce, ..Material::default() }).unwrap();
            world.add(0, &at(3.0)).unwrap();
            let (mut landed, mut top) = (false, 0.0f64);
            for _ in 0..240 {
                world.step(DT, 1);
                let z = world.placements(0)[0].translate[2];
                if z < 0.6 {
                    landed = true;
                } else if landed {
                    top = top.max(z);
                }
            }
            top
        };
        assert!(top(0.8, 0.0) > 1.5, "a rubber ball bounces on a dead floor");
        assert!(top(0.0, 0.8) > 1.5, "a dead ball bounces on a rubber floor");
        assert_eq!(top(0.0, 0.0), 0.0);
    }

    #[test]
    fn the_heavier_body_pushes_the_lighter_one_aside() {
        // Two balls meet head on at the same speed, without gravity. Where they are afterwards
        // tells which one won.
        let after = |left: f32, right: f32| {
            let mut world = World::new(Settings { gravity: [0.0; 3], floor: None, ..Settings::default() });
            world.set_group(0, ball(), Material { density: left, ..Material::default() }).unwrap();
            world.set_group(1, ball(), Material { density: right, ..Material::default() }).unwrap();
            let thrown = |x: f32, v: f32| Start {
                translate: [x, 0.0, 0.0],
                velocity: [v, 0.0, 0.0],
                ..Start::default()
            };
            world.add(0, &thrown(-2.0, 2.0)).unwrap();
            world.add(1, &thrown(2.0, -2.0)).unwrap();
            run(&mut world, 2.0);
            (world.placements(0)[0].translate[0], world.placements(1)[0].translate[0])
        };
        let (a, b) = after(1.0, 1.0);
        assert!((a + b).abs() < 0.05, "equal balls end mirrored: {a} {b}");
        let (a, b) = after(20.0, 1.0);
        assert!(a > 0.0 && b > a, "the heavy ball went on to the right: {a} {b}");
        // Density set while the bodies move changes what they weigh.
        let mut world = World::new(Settings { gravity: [0.0; 3], floor: None, ..Settings::default() });
        world.set_group(0, ball(), Material::default()).unwrap();
        world.set_group(1, ball(), Material::default()).unwrap();
        world
            .add(0, &Start { translate: [-2.0, 0.0, 0.0], velocity: [2.0, 0.0, 0.0], ..Start::default() })
            .unwrap();
        world
            .add(1, &Start { translate: [2.0, 0.0, 0.0], velocity: [-2.0, 0.0, 0.0], ..Start::default() })
            .unwrap();
        world.step(DT, 1);
        world.set_material(0, Material { density: 20.0, ..Material::default() });
        run(&mut world, 2.0);
        assert!(world.placements(0)[0].translate[0] > 0.0, "{:?}", world.placements(0)[0].translate);
    }

    #[test]
    fn a_box_falls_onto_the_floor_and_rests_on_it() {
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(3.0)).unwrap();
        world.step(DT, 1);
        let early = world.placements(0)[0].translate[2];
        assert!(early < 3.0 && early > 2.9, "one step in: {early}");
        run(&mut world, 4.0);
        let p = world.placements(0)[0];
        assert!((p.translate[2] - 0.5).abs() < 0.02, "rests at {}", p.translate[2]);
        assert_eq!(world.asleep(), 1);
    }

    #[test]
    fn without_a_floor_a_body_keeps_falling() {
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        world.add(0, &at(3.0)).unwrap();
        run(&mut world, 2.0);
        // 3 - 9.81 * 2^2 / 2 = -16.6
        let z = world.placements(0)[0].translate[2];
        assert!((z + 16.6).abs() < 0.5, "{z}");
    }

    #[test]
    fn a_ball_with_bounce_comes_back_up_and_one_without_stays_down() {
        let ball = Shape::Sphere { center: [0.0; 3], radius: 0.5 };
        let top_after_landing = |bounce: f32| {
            let mut world = world_of(ball.clone(), Settings { bounce, ..Settings::default() });
            world.add(0, &at(3.0)).unwrap();
            let mut landed = false;
            let mut top = 0.0f64;
            for _ in 0..240 {
                world.step(DT, 1);
                let z = world.placements(0)[0].translate[2];
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
        let mut world = world_of(unit_box(), Settings { floor: None, ..Settings::default() });
        let table = || Obstacle {
            vertices: vec![[-2.0, -2.0, 1.0], [2.0, -2.0, 1.0], [2.0, 2.0, 1.0], [-2.0, 2.0, 1.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        world.set_obstacles(vec![table()]).unwrap();
        world.add(0, &at(3.0)).unwrap();
        run(&mut world, 4.0);
        let z = world.placements(0)[0].translate[2];
        assert!((z - 1.5).abs() < 0.03, "rests at {z}");
        world.set_obstacles(Vec::new()).unwrap();
        run(&mut world, 1.0);
        assert!(world.placements(0)[0].translate[2] < 0.0);
    }

    #[test]
    fn a_hull_stands_on_its_lowest_points_and_scale_sizes_it() {
        // A pyramid: base 2 x 2 at z = 0, tip at z = 1.
        let points =
            vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut world = world_of(Shape::Hull { points }, Settings::default());
        world.add(0, &at(2.0)).unwrap();
        world
            .add(
                0,
                &Start { translate: [10.0, 0.0, 2.0], rotate_degrees: [180.0, 0.0, 0.0], ..Start::default() },
            )
            .unwrap();
        run(&mut world, 5.0);
        let p = world.placements(0);
        assert!(p[0].translate[2].abs() < 0.03, "the base is on the floor: {}", p[0].translate[2]);
        // Dropped tip first it cannot stay: it ends on a side or its base, origin above the floor
        // by less than the pyramid is wide.
        assert!(p[1].translate[2] > -0.03 && p[1].translate[2] < 1.5, "{}", p[1].translate[2]);
        let big = [2.0; 3];
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &Start { translate: [0.0, 0.0, 4.0], scale: big, ..Start::default() }).unwrap();
        run(&mut world, 4.0);
        let p = world.placements(0)[0];
        assert!((p.translate[2] - 1.0).abs() < 0.03, "{}", p.translate[2]);
        assert_eq!(p.scale, big);
    }

    #[test]
    fn settings_reach_bodies_that_rest() {
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(1.0)).unwrap();
        run(&mut world, 3.0);
        assert_eq!(world.asleep(), 1);
        world.apply(Settings { gravity: [0.0, 0.0, 9.81], ..Settings::default() });
        run(&mut world, 1.0);
        assert!(world.placements(0)[0].translate[2] > 2.0);
    }

    #[test]
    fn bad_input_is_an_error_and_never_reaches_the_solver() {
        let mut world = world_of(unit_box(), Settings::default());
        assert!(world.add(0, &at(f32::NAN)).is_err());
        assert!(world.add(0, &Start { velocity: [f32::INFINITY, 0.0, 0.0], ..Start::default() }).is_err());
        assert!(world.is_empty());
        let bad_index = Obstacle { vertices: vec![[0.0; 3]; 3], triangles: vec![[0, 1, 7]] };
        assert!(world.set_obstacles(vec![bad_index]).is_err());
        let bad_vertex = Obstacle { vertices: vec![[f32::NAN; 3]; 3], triangles: vec![[0, 1, 2]] };
        assert!(world.set_obstacles(vec![bad_vertex]).is_err());
        assert!(check(Shape::Hull { points: vec![[0.0; 3]; 8] }).is_err());
        assert!(check(Shape::Hull { points: vec![[1.0, 0.0, 0.0]] }).is_err());
        assert!(check(Shape::Sphere { center: [0.0; 3], radius: f32::NAN }).is_err());
        // A shape that fails leaves the group as it was; so does a group beyond the last.
        world.add(0, &at(1.0)).unwrap();
        assert!(world.set_group(0, Shape::Hull { points: vec![[0.0; 3]; 8] }, Material::default()).is_err());
        assert_eq!(world.group_len(0), 1);
        assert!(world.set_group(MAX_GROUPS, unit_box(), Material::default()).is_err());
        assert!(world.add(MAX_GROUPS, &at(1.0)).is_err());
        assert!(world.add(7, &at(1.0)).is_err(), "no such group");
        world.set_material(7, Material::default());
        world.remove_group(usize::MAX);
        assert_eq!((world.len(), world.placements(7).len()), (1, 0));
        // Settings and materials that are not numbers fall back instead of failing.
        let odd = Settings { gravity: [f32::NAN; 3], friction: -3.0, bounce: 9.0, ..Settings::default() };
        let mut world = World::new(odd);
        let strange = Material { friction: f32::NAN, bounce: -1.0, density: 0.0 };
        world.set_group(0, unit_box(), strange).unwrap();
        world.add(0, &at(1.0)).unwrap();
        world.set_material(0, Material { density: f32::INFINITY, ..strange });
        world.step(f32::NAN, 1);
        world.step(-1.0, 1);
        world.step(DT, 100);
        assert!(world.placements(0)[0].translate.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_world_is_full_at_the_limit() {
        // The limit counts the bodies of every group together.
        let mut world = world_of(unit_box(), Settings::default());
        world.set_group(1, ball(), Material::default()).unwrap();
        for k in 0..MAX_BODIES {
            world.add(k % 2, &at(k as f32 * 2.0)).unwrap();
        }
        assert!(world.add(0, &at(0.0)).is_err());
        assert!(world.add(1, &at(0.0)).is_err());
        assert_eq!(world.len(), MAX_BODIES);
        // A group that leaves makes room again.
        world.remove_group(1);
        assert_eq!(world.len(), MAX_BODIES / 2);
        world.add(0, &at(0.0)).unwrap();
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
        let mut world = world_of(unit_box(), Settings::default());
        world.add(0, &at(0.6)).unwrap();
        run(&mut world, 3.0);
        let a = world.placements(0);
        run(&mut world, 1.0);
        assert_eq!(a, world.placements(0));
    }
}
